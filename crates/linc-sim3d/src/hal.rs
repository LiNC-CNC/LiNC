//! HAL component pins: the simulated machine's hardware interface.
//!
//! Owner: wave-2 task C.
//!
//! The simulator is a HAL component named [COMPONENT_NAME]. It reads the
//! machine position (and the active tool) from its input pins, evaluates the
//! model, and publishes probe contact, collision and the modelled tip position
//! on its output pins. It never drives motion.
//!
//! The pieces here are the seam between HAL and the model:
//!
//! - [PINS] is the published pin table. It is the single source of truth for
//!   the pin names, so a rename cannot drift away from `hal/linc-sim3d.hal`
//!   (a test reads that file and compares).
//! - [HalBridge] creates the component and its pins and reads/writes them.
//! - [HalSignal] implements [Sim3dSignal] on top of the bridge.
//! - [HalPositionSource] implements [PositionSource] by reading the bridge's
//!   own input pins.
//!
//! [run_hal] is the headless mode `hal/linc-sim3d.hal` starts: it is [start]
//! plus `SimModel::from_config` plus `engine::run_engine`, and it never opens
//! a window.

use std::os::raw::c_int;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use linc_hal::{HalComponent, HalPin};

use crate::config::Sim3dConfig;
use crate::engine::{run_engine, PositionSource, Positions, DEFAULT_SAMPLE_MS};
use crate::model::{SharedState, SimModel};
use crate::sink::Sim3dSignal;
use crate::types::{Bounds, P3};

/// The HAL component name; every pin is `linc-sim3d.<pin>`.
pub const COMPONENT_NAME: &str = "linc-sim3d";

/// Direction of a HAL pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDir {
    /// HAL writes it; the component reads it.
    In,
    /// The component writes it; HAL reads it.
    Out,
}

/// HAL data type of a pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    Bit,
    Float,
    S32,
}

/// One published pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinSpec {
    /// Pin name without the component prefix.
    pub name: &'static str,
    pub dir: PinDir,
    pub kind: PinKind,
}

impl PinSpec {
    /// The fully qualified HAL name, `linc-sim3d.<name>`.
    pub fn full_name(&self) -> String {
        format!("{COMPONENT_NAME}.{}", self.name)
    }
}

/// The pin table, in declaration order.
///
/// `pos-*` and `tool-*` are the machine state the simulator samples;
/// `probe-out`, `collision` and `tip-*` are what it reports.
pub const PINS: [PinSpec; 10] = [
    PinSpec {
        name: "pos-x",
        dir: PinDir::In,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "pos-y",
        dir: PinDir::In,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "pos-z",
        dir: PinDir::In,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "tool-offset",
        dir: PinDir::In,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "tool-number",
        dir: PinDir::In,
        kind: PinKind::S32,
    },
    PinSpec {
        name: "probe-out",
        dir: PinDir::Out,
        kind: PinKind::Bit,
    },
    PinSpec {
        name: "collision",
        dir: PinDir::Out,
        kind: PinKind::Bit,
    },
    PinSpec {
        name: "tip-x",
        dir: PinDir::Out,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "tip-y",
        dir: PinDir::Out,
        kind: PinKind::Float,
    },
    PinSpec {
        name: "tip-z",
        dir: PinDir::Out,
        kind: PinKind::Float,
    },
];

/// Number of published pins.
pub const PIN_COUNT: usize = PINS.len();

/// The fully qualified names of every published pin, in declaration order.
pub fn full_pin_names() -> Vec<String> {
    PINS.iter().map(PinSpec::full_name).collect()
}

/// A pin the simulator writes, so [HalSignal] can be exercised without HAL.
///
/// [HalBridge] is the production implementation; tests use a recording double.
pub trait HalOutputPins: Send + Sync {
    fn set_probe(&self, contact: bool);
    fn set_collision(&self, hit: bool);
    fn set_tip(&self, tip: P3);
}

/// The live HAL component and its pins.
///
/// Creation is the only fallible step: [HalBridge::open] registers the
/// component, creates every pin in [PINS] and marks it ready. Without a
/// running HAL it returns the library error instead of panicking.
pub struct HalBridge {
    component: HalComponent,
    name: String,
    pins: [HalPin; PIN_COUNT],
}

// SAFETY: the pins point into HAL shared memory, which HAL's own threads write
// regardless of what Rust believes. The bridge only dereferences the slots it
// created; the sampling thread is the single writer of the output pins and the
// single reader of the input pins.
unsafe impl Send for HalBridge {}
unsafe impl Sync for HalBridge {}

impl HalBridge {
    /// Create the component, publish the pins, and mark the component ready.
    ///
    /// This is the step that fails on a machine without HAL: the error says
    /// which library or pin call failed, and nothing is left registered.
    pub fn open() -> anyhow::Result<Self> {
        Self::open_named(COMPONENT_NAME)
    }

    /// [HalBridge::open] under another component name.
    ///
    /// `config.hal.component` defaults to [COMPONENT_NAME] and the HAL file
    /// nets `linc-sim3d.*`, so a different name only works together with a HAL
    /// file that uses it.
    pub fn open_named(name: &str) -> anyhow::Result<Self> {
        let component = HalComponent::new(name)
            .with_context(|| format!("creating the {name} HAL component"))?;
        let mut created = Vec::with_capacity(PIN_COUNT);
        for spec in PINS.iter() {
            let pin = match (spec.dir, spec.kind) {
                (PinDir::In, PinKind::Bit) => component.bit_in(spec.name),
                (PinDir::Out, PinKind::Bit) => component.bit_out(spec.name),
                (PinDir::In, PinKind::Float) => component.float_in(spec.name),
                (PinDir::Out, PinKind::Float) => component.float_out(spec.name),
                (PinDir::In, PinKind::S32) => component.s32_in(spec.name),
                (PinDir::Out, PinKind::S32) => component.s32_out(spec.name),
            }
            .with_context(|| format!("creating HAL pin {name}.{}", spec.name))?;
            created.push(pin);
        }
        let pins: [HalPin; PIN_COUNT] = created
            .try_into()
            .map_err(|_| anyhow!("the pin table does not match {PIN_COUNT} pins"))?;
        let bridge = Self {
            component,
            name: name.to_string(),
            pins,
        };
        bridge.publish_initial_state();
        bridge.component.ready().with_context(|| {
            format!(
                "marking the {} HAL component ready ({PIN_COUNT} pins)",
                bridge.name
            )
        })?;
        log::debug!(
            "linc-sim3d: HAL component {} ready: {PIN_COUNT} pins ({} in, {} out)",
            bridge.name,
            PINS.iter().filter(|p| p.dir == PinDir::In).count(),
            PINS.iter().filter(|p| p.dir == PinDir::Out).count()
        );
        Ok(bridge)
    }

    /// The component name this bridge created.
    pub fn component_name(&self) -> &str {
        &self.name
    }

    /// Whether `hal_ready` succeeded.
    pub fn is_ready(&self) -> bool {
        self.component.is_ready()
    }

    /// The fully qualified names of the pins this bridge created.
    pub fn pin_names(&self) -> Vec<String> {
        PINS.iter()
            .map(|spec| format!("{}.{}", self.name, spec.name))
            .collect()
    }

    /// The machine state the engine evaluates, read from the input pins.
    ///
    /// A negative `tool-number` (iocontrol uses -1 before a tool is known)
    /// reads as tool 0.
    pub fn positions(&self) -> Positions {
        Positions {
            x: self.read_float("pos-x"),
            y: self.read_float("pos-y"),
            z: self.read_float("pos-z"),
            tool_offset_z: self.read_float("tool-offset"),
            tool_number: self.read_s32("tool-number").clamp(0, i32::from(u16::MAX)) as u16,
        }
    }

    fn pin(&self, name: &str) -> &HalPin {
        let index = PINS
            .iter()
            .position(|spec| spec.name == name)
            .expect("every pin name here is in the table");
        &self.pins[index]
    }

    fn read_float(&self, name: &str) -> f64 {
        let data = self.pin(name).float();
        if data.is_null() {
            0.0
        } else {
            // SAFETY: the slot belongs to this component and HAL keeps it
            // valid for the component's lifetime.
            unsafe { *data }
        }
    }

    fn read_s32(&self, name: &str) -> i32 {
        let data = self.pin(name).s32();
        if data.is_null() {
            0
        } else {
            // SAFETY: as above.
            unsafe { *data }
        }
    }

    fn write_bit(&self, name: &str, value: bool) {
        let data = self.pin(name).bit();
        if !data.is_null() {
            // SAFETY: as above; the sampling thread is the only writer.
            unsafe { *data = u8::from(value) };
        }
    }

    fn write_float(&self, name: &str, value: f64) {
        let data = self.pin(name).float();
        if !data.is_null() {
            // SAFETY: as above.
            unsafe { *data = value };
        }
    }

    /// Start from a known state: no contact, no collision, tip at the origin.
    fn publish_initial_state(&self) {
        self.set_probe(false);
        self.set_collision(false);
        self.set_tip([0.0; 3]);
    }
}

impl HalOutputPins for HalBridge {
    fn set_probe(&self, contact: bool) {
        self.write_bit("probe-out", contact);
    }

    fn set_collision(&self, hit: bool) {
        self.write_bit("collision", hit);
    }

    fn set_tip(&self, tip: P3) {
        self.write_float("tip-x", tip[0]);
        self.write_float("tip-y", tip[1]);
        self.write_float("tip-z", tip[2]);
    }
}

/// [Sim3dSignal] over HAL output pins.
///
/// A fault sets `collision` high and logs once. The collision output stays
/// latched high after the first fault, so a modelling error is visible in
/// `halshow` instead of flickering for one tick and being cleared by the next
/// `collision(false)`.
pub struct HalSignal<P: ?Sized + HalOutputPins = HalBridge> {
    pins: Arc<P>,
    faulted: AtomicBool,
}

impl<P: ?Sized + HalOutputPins> HalSignal<P> {
    /// A signal writing to `pins`.
    pub fn new(pins: Arc<P>) -> Self {
        Self {
            pins,
            faulted: AtomicBool::new(false),
        }
    }

    /// Whether a fault has been reported on this signal.
    pub fn faulted(&self) -> bool {
        self.faulted.load(Ordering::Relaxed)
    }
}

impl<P: ?Sized + HalOutputPins> Sim3dSignal for HalSignal<P> {
    fn probe(&self, contact: bool) {
        self.pins.set_probe(contact);
    }

    fn collision(&self, hit: bool) {
        self.pins.set_collision(hit || self.faulted());
    }

    fn tip(&self, point: P3) {
        self.pins.set_tip(point);
    }

    fn fault(&self, message: &str) {
        self.pins.set_collision(true);
        if !self.faulted.swap(true, Ordering::Relaxed) {
            log::error!("linc-sim3d: modelling fault, collision output held high: {message}");
        }
    }
}

/// [PositionSource] reading the machine position from this component's in pins.
///
/// The source samples the same bridge the signal writes through, so HAL has
/// exactly one `linc-sim3d` component. Without HAL there is nothing to read:
/// [HalPositionSource::from_component] returns an error rather than a source
/// that reports the origin forever.
pub struct HalPositionSource {
    bridge: Arc<HalBridge>,
}

impl HalPositionSource {
    /// A source reading `bridge`'s input pins.
    pub fn new(bridge: Arc<HalBridge>) -> Self {
        Self { bridge }
    }

    /// Create the component and read from it. Equivalent to
    /// [HalBridge::open] followed by [HalPositionSource::new].
    pub fn from_component() -> anyhow::Result<Self> {
        Ok(Self::new(Arc::new(HalBridge::open()?)))
    }
}

impl PositionSource for HalPositionSource {
    fn sample(&mut self) -> Option<Positions> {
        // The pins always exist while the component does, so the source never
        // runs out of samples.
        Some(self.bridge.positions())
    }
}

/// Create the component and hand back the two halves that share it: the signal
/// the engine writes and the position source it samples.
pub fn start() -> anyhow::Result<(HalSignal<HalBridge>, HalPositionSource)> {
    start_named(COMPONENT_NAME)
}

/// [start] under another component name, for `config.hal.component`.
pub fn start_named(name: &str) -> anyhow::Result<(HalSignal<HalBridge>, HalPositionSource)> {
    let bridge = Arc::new(HalBridge::open_named(name)?);
    Ok((
        HalSignal::new(Arc::clone(&bridge)),
        HalPositionSource::new(bridge),
    ))
}

/// The component name the config asks for: [HalConfig::component], or
/// [COMPONENT_NAME] when the config names nothing.
pub fn configured_name(config: &Sim3dConfig) -> &str {
    let name = config.hal.component.trim();
    if name.is_empty() {
        COMPONENT_NAME
    } else {
        name
    }
}

/// The one line that says what was published, for the HAL file's log and for a
/// human at the terminal.
///
/// env_logger may not be initialised in this mode, so it goes to stderr.
fn announce(name: &str, workpieces: usize, travel: &Bounds, interval: Duration) {
    eprintln!(
        "linc-sim3d: HAL component {name}: {PIN_COUNT} pins, {workpieces} workpiece(s), \
         travel [{:.1}, {:.1}, {:.1}]..[{:.1}, {:.1}, {:.1}], sampling every {:.1} ms",
        travel.min[0],
        travel.min[1],
        travel.min[2],
        travel.max[0],
        travel.max[1],
        travel.max[2],
        interval.as_secs_f64() * 1000.0
    );
}

/// How often [Rig::wait_for_stop] looks at the flag.
const STOP_POLL: Duration = Duration::from_millis(20);

/// A running HAL rig: the published component, the model it publishes and the
/// sampling thread.
///
/// This is the headless half of every --hal mode, and the single owner of the
/// component. [run_hal] starts one and waits for a signal; the command line's
/// window mode starts one, draws the state and keeps sampling after the window
/// closes, so one process owns the component and the view and closing the
/// window never drops probe-out.
pub struct Rig {
    name: String,
    config: Sim3dConfig,
    shared: Arc<SharedState>,
    interval: Duration,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Rig {
    /// Load the config, build the model, publish the component, then sample.
    ///
    /// The model is built before the component is published, so loadusr -Wn in
    /// the HAL files only returns once the simulator can actually sample; a
    /// model error therefore leaves no half-published component behind.
    pub fn start(config_path: &Path) -> anyhow::Result<Self> {
        let config = Sim3dConfig::load(config_path)
            .with_context(|| format!("loading the simulator config {}", config_path.display()))?;
        let interval = sample_interval(config.hal.sample_ms);
        let name = configured_name(&config).to_string();
        let model = SimModel::from_config(&config).with_context(|| {
            format!(
                "building the model from {}",
                config.machine_ini_path().display()
            )
        })?;
        let workpieces = model.workpieces().len();
        let travel = model.machine().travel;

        let (signal, source) = start_named(&name).map_err(|error| hal_unavailable(&name, error))?;
        let shared = Arc::new(SharedState::new(model));
        announce(&name, workpieces, &travel, interval);

        let stop = Arc::new(AtomicBool::new(false));
        signals::install(&stop);

        let engine_shared = Arc::clone(&shared);
        let engine_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("linc-sim3d-engine".to_string())
            .spawn(move || {
                run_engine(
                    engine_shared,
                    Box::new(source),
                    Box::new(signal),
                    interval,
                    engine_stop,
                );
            })
            .context("starting the sampling thread")?;

        Ok(Self {
            name,
            config,
            shared,
            interval,
            stop,
            thread: Some(thread),
        })
    }

    /// The component this rig published.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The config the rig was started from, for the window's panel.
    pub fn config(&self) -> &Sim3dConfig {
        &self.config
    }

    /// The state the window draws and the engine publishes into.
    pub fn shared(&self) -> &Arc<SharedState> {
        &self.shared
    }

    /// The sampling period.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// True once SIGINT or SIGTERM asked this rig to stop.
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The flag SIGINT/SIGTERM sets, for a window that has to close when it
    /// fires.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Block until SIGINT or SIGTERM arrives.
    ///
    /// The handler may only store into an atomic, so the flag is polled rather
    /// than waited on. The engine thread is already sampling; this is the main
    /// thread keeping the process alive.
    pub fn wait_for_stop(&self) {
        while !self.stop_requested() {
            std::thread::sleep(STOP_POLL);
        }
    }

    /// Stop sampling and join the thread. Idempotent; [Drop] calls it too.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // The process may outlive this call; do not leave a handler pointing at
        // a flag that is about to be dropped.
        signals::detach();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for Rig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Rig")
            .field("name", &self.name)
            .field("interval", &self.interval)
            .field("sampling", &self.thread.is_some())
            .finish()
    }
}

/// The strict headless mode: publish the component and sample until SIGINT or
/// SIGTERM, with no window at all.
///
/// The command line's --hal mode uses [Rig] directly, because that process may
/// also show the window; this stays the entry point for anything that wants
/// exactly the headless behaviour and nothing else.
pub fn run_hal(config_path: &Path) -> anyhow::Result<()> {
    let rig = Rig::start(config_path)?;
    rig.wait_for_stop();
    // Dropping the rig stops the engine thread and detaches the handlers.
    Ok(())
}

/// The message a missing HAL produces: what failed, and what has to be
/// installed for it to work.
fn hal_unavailable(component: &str, error: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "cannot publish the {component} HAL component; this needs liblinuxcnchal and a \
         running LinuxCNC HAL: {error:#}"
    )
}

/// The sampling period, in seconds; an unusable `sample_ms` falls back to
/// [DEFAULT_SAMPLE_MS] instead of spinning or dividing by zero.
fn sample_interval(sample_ms: f64) -> Duration {
    let millis = if sample_ms.is_finite() && sample_ms >= 0.1 {
        sample_ms
    } else {
        DEFAULT_SAMPLE_MS
    };
    Duration::from_secs_f64(millis / 1000.0)
}

/// SIGINT/SIGTERM end the sampling loop.
///
/// A HAL process is stopped with SIGTERM (`halrun` does that when its script
/// ends), so the handler only flips the flag `run_engine` polls; that is
/// async-signal-safe, and `run_hal` keeps the `Arc` alive until it detaches.
mod signals {
    use super::*;

    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;

    /// The flag [super::Rig::stop] polls; null while no handler is installed.
    static STOP_FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn on_stop_signal(_signal: c_int) {
        let flag = STOP_FLAG.load(Ordering::SeqCst);
        if flag.is_null() {
            return;
        }
        // SAFETY: run_hal keeps the Arc this points at alive until detach().
        unsafe { (*flag).store(true, Ordering::SeqCst) };
    }

    unsafe extern "C" {
        fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> *mut std::ffi::c_void;
    }

    /// Route SIGINT and SIGTERM to `stop`. Returns whether the handlers took.
    pub fn install(stop: &Arc<AtomicBool>) -> bool {
        STOP_FLAG.store(Arc::as_ptr(stop).cast_mut(), Ordering::SeqCst);
        let mut installed = true;
        for number in [SIGINT, SIGTERM] {
            // SAFETY: libc's signal(); the handler only stores into an atomic.
            let previous = unsafe { signal(number, on_stop_signal) };
            installed &= previous as usize != usize::MAX;
        }
        if !installed {
            log::warn!("linc-sim3d: could not install the SIGINT/SIGTERM handlers");
        }
        installed
    }

    /// Stop touching the flag. The handlers stay installed and do nothing, so
    /// a late signal cannot reach a dropped `Arc`.
    pub fn detach() {
        STOP_FLAG.store(std::ptr::null_mut(), Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingPins {
        probe: AtomicBool,
        collision: AtomicBool,
        tip: Mutex<P3>,
    }

    impl HalOutputPins for RecordingPins {
        fn set_probe(&self, contact: bool) {
            self.probe.store(contact, Ordering::Relaxed);
        }

        fn set_collision(&self, hit: bool) {
            self.collision.store(hit, Ordering::Relaxed);
        }

        fn set_tip(&self, tip: P3) {
            *self.tip.lock().unwrap() = tip;
        }
    }

    fn recording_signal() -> (Arc<RecordingPins>, HalSignal<RecordingPins>) {
        let pins = Arc::new(RecordingPins::default());
        let signal = HalSignal::new(Arc::clone(&pins));
        (pins, signal)
    }

    #[test]
    fn signal_writes_every_output_pin() {
        let (pins, signal) = recording_signal();
        signal.probe(true);
        signal.tip([1.5, -2.5, 3.5]);
        assert!(pins.probe.load(Ordering::Relaxed));
        assert_eq!(*pins.tip.lock().unwrap(), [1.5, -2.5, 3.5]);

        signal.probe(false);
        assert!(!pins.probe.load(Ordering::Relaxed));

        signal.collision(true);
        assert!(pins.collision.load(Ordering::Relaxed));
        signal.collision(false);
        assert!(!pins.collision.load(Ordering::Relaxed));
    }

    #[test]
    fn fault_sets_collision_and_holds_it() {
        let (pins, signal) = recording_signal();
        assert!(!signal.faulted());
        signal.fault("probe mesh missing");
        assert!(signal.faulted());
        assert!(pins.collision.load(Ordering::Relaxed));

        // The engine's next collision(false) must not hide a modelling error.
        signal.collision(false);
        assert!(
            pins.collision.load(Ordering::Relaxed),
            "a reported fault keeps collision latched"
        );

        // A second fault is reported, not logged twice: faulted stays set.
        signal.fault("same fault again");
        assert!(pins.collision.load(Ordering::Relaxed));
    }

    #[test]
    fn an_unavailable_hal_error_names_the_library_and_the_cause() {
        let error = hal_unavailable(COMPONENT_NAME, anyhow!("hal_init failed with -1"));
        let text = format!("{error:#}");
        assert!(text.contains("liblinuxcnchal"), "{text}");
        assert!(text.contains(COMPONENT_NAME), "{text}");
        assert!(text.contains("hal_init failed with -1"), "{text}");
    }

    #[test]
    fn the_sample_interval_falls_back_to_the_documented_default() {
        assert_eq!(sample_interval(2.0), Duration::from_millis(2));
        assert_eq!(sample_interval(0.5), Duration::from_micros(500));
        for unusable in [0.0, -4.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                sample_interval(unusable),
                Duration::from_secs_f64(DEFAULT_SAMPLE_MS / 1000.0),
                "sample_ms = {unusable}"
            );
        }
    }

    #[test]
    fn hal_is_unavailable_without_panicking() {
        // On a developer box this is an error; inside LinuxCNC it succeeds.
        // Neither outcome may panic, and a bridge that exists must be ready
        // with exactly the published pins.
        match HalBridge::open() {
            Ok(bridge) => {
                assert!(bridge.is_ready());
                assert_eq!(bridge.pin_names().len(), PIN_COUNT);
                let positions = bridge.positions();
                assert_eq!(positions.tool_number, 0);
            }
            Err(error) => {
                assert!(
                    !error.to_string().is_empty(),
                    "a missing HAL must say why: {error:?}"
                );
            }
        }
        if let Ok(mut source) = HalPositionSource::from_component() {
            assert!(
                source.sample().is_some(),
                "a live component always has a position"
            );
        }
    }
}
