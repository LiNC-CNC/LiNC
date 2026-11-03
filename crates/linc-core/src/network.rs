//! Optional supervision of the machine's internet link.
//!
//! The link this module manages is the way the machine reaches the internet:
//! a WiFi radio, or a second NIC (PCIe or USB). It is **never** the machine's
//! own fieldbus and I/O network, which is internal and must keep running. That
//! is why every default here acts on one named device and why validation
//! rejects anything that reaches for system-wide networking
//! (`crate::config::validate_network`).
//!
//! # Why measure first
//!
//! WiFi traffic costs LinuxCNC latency: beacons, scans and the driver's own
//! interrupt work land in the same machine as the realtime thread. Dropping the
//! link before motion is the intended fix, but the numbers that make it work —
//! how long `nmcli` takes, how long the radio keeps working after it returns,
//! and how much interrupt load actually stops — are properties of the driver
//! and the hardware, not of the profile.
//!
//! So this pass measures instead of gating. `[network] enabled = true` runs one
//! down/verify/settle/up cycle on an idle machine and publishes the phases plus
//! the device's interrupt and packet rates in [`NetworkStatus`]. Nothing here
//! refuses a command or blocks motion: the operator-facing promise of this
//! pass is "it tells you what a toggle costs", and a later pass can arm the
//! gate with `settle_ms` chosen from evidence.
//!
//! The cycle is a worker thread, like the Loobric client, so neither the UI nor
//! the NML loop waits on `nmcli`.

use std::fs;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::MachineBackend;
use crate::command::{MachineCommand, MachineError};
use crate::config::{NetworkConfig, Profile};
use crate::model::{now_ms, InterpState, MachineSnapshot, NetworkStatus, TaskMode};

/// How often the worker re-reads the observation before it starts measuring.
const TICK: Duration = Duration::from_millis(100);

/// Deadline for one `nmcli` invocation. A hung command must not hold the
/// measurement open forever; the cycle still restores the link afterwards.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long each interrupt/packet sample counts for. Test overrides shrink it.
const DEFAULT_SAMPLE: Duration = Duration::from_millis(1000);

/// The two kinds of external link the section knows. The kind only selects the
/// default commands and the state parser; policy is identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Wifi,
    Ethernet,
}

impl LinkKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "wifi" => Some(Self::Wifi),
            "ethernet" => Some(Self::Ethernet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
        }
    }
}

/// Resolved settings for one measurement. `from_profile` returns `None` for a
/// disabled profile, so a machine that does not opt in starts no worker and
/// publishes no status.
#[derive(Clone, Debug)]
pub struct NetworkSettings {
    pub kind: LinkKind,
    pub interface: String,
    /// Each step is an argv vector; steps run in order. No shell is involved,
    /// so an override cannot chain a second command.
    pub down: Vec<Vec<String>>,
    pub up: Vec<Vec<String>>,
    pub state: Vec<String>,
    pub settle: Duration,
    pub verify_timeout: Duration,
    pub measure_on_startup: bool,
    pub keep_up: bool,
    pub irq_match: String,
    pub sample: Duration,
    /// The profile expects a Loobric run at startup, so the measurement waits
    /// for it: pulling the link down under a running client is the one failure
    /// worth avoiding even in a measurement pass.
    pub expect_loobric_startup: bool,
}

impl NetworkSettings {
    pub fn from_profile(profile: &Profile) -> Option<Self> {
        let config = &profile.config.network;
        if !config.enabled {
            return None;
        }
        Some(Self::from_config(
            config,
            profile.config.loobric.enabled && profile.config.loobric.sync_on_startup,
        ))
    }

    fn from_config(config: &NetworkConfig, expect_loobric_startup: bool) -> Self {
        // Validation already rejected an unknown kind; falling back keeps a
        // hand-built config from panicking.
        let kind = LinkKind::parse(&config.kind).unwrap_or(LinkKind::Wifi);
        let interface = config.interface.trim().to_string();
        let down = match &config.down_cmd {
            Some(command) => vec![split_command(command)],
            None => default_down(kind, &interface),
        };
        let up = match &config.up_cmd {
            Some(command) => vec![split_command(command)],
            None => default_up(kind, &interface, config.connection.as_deref()),
        };
        let state = match &config.state_cmd {
            Some(command) => split_command(command),
            None => default_state(kind, &interface),
        };
        Self {
            kind,
            interface: interface.clone(),
            down,
            up,
            state,
            settle: Duration::from_millis(config.settle_ms),
            verify_timeout: Duration::from_millis(config.verify_timeout_ms),
            measure_on_startup: config.measure_on_startup,
            keep_up: config.keep_up,
            irq_match: config
                .irq_match
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&interface)
                .to_string(),
            sample: DEFAULT_SAMPLE,
            expect_loobric_startup,
        }
    }
}

/// `nmcli radio wifi off` acts on the radio, not on system networking, so the
/// machine's fieldbus link keeps running.
fn default_down(kind: LinkKind, interface: &str) -> Vec<Vec<String>> {
    match kind {
        LinkKind::Wifi => vec![argv(&["nmcli", "radio", "wifi", "off"])],
        LinkKind::Ethernet => vec![argv(&["nmcli", "device", "disconnect", interface])],
    }
}

fn default_up(kind: LinkKind, interface: &str, connection: Option<&str>) -> Vec<Vec<String>> {
    match kind {
        LinkKind::Wifi => {
            let mut steps = vec![argv(&["nmcli", "radio", "wifi", "on"])];
            if let Some(connection) = connection.map(str::trim).filter(|value| !value.is_empty()) {
                steps.push(argv(&["nmcli", "connection", "up", connection]));
            }
            steps
        }
        LinkKind::Ethernet => vec![argv(&["nmcli", "device", "connect", interface])],
    }
}

fn default_state(kind: LinkKind, interface: &str) -> Vec<String> {
    match kind {
        // `enabled` / `disabled`: the radio's own state, which is what the
        // latency question is about.
        LinkKind::Wifi => argv(&["nmcli", "--terse", "--fields", "WIFI", "radio"]),
        // Carrier state. Route removal would leave the PHY and the driver's
        // interrupt work running.
        LinkKind::Ethernet => argv(&["cat", &format!("/sys/class/net/{interface}/operstate")]),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

fn split_command(command: &str) -> Vec<String> {
    command.split_whitespace().map(str::to_string).collect()
}

/// One completed measurement cycle.
#[derive(Clone, Debug, Default)]
struct Measurement {
    down: Duration,
    /// `None` when the link never read down before the deadline.
    verify: Option<Duration>,
    settle: Duration,
    up: Duration,
    up_restored: bool,
    link_down: bool,
    irq_up: Option<f64>,
    irq_down: Option<f64>,
    rx_up: Option<f64>,
    rx_down: Option<f64>,
    notes: Vec<String>,
}

impl Measurement {
    fn summary(&self) -> String {
        if !self.link_down {
            return format!("link never read down after {}ms", self.down.as_millis());
        }
        let mut text = format!(
            "down {}ms, verify {}ms, settle {}ms, up {}ms",
            self.down.as_millis(),
            self.verify
                .map(|value| value.as_millis().to_string())
                .unwrap_or_else(|| ">deadline".into()),
            self.settle.as_millis(),
            self.up.as_millis(),
        );
        match (self.irq_up, self.irq_down) {
            (Some(up), Some(down)) => {
                text.push_str(&format!(", irq {up:.0}/s -> {down:.0}/s"));
            }
            _ => text.push_str(", irq not sampled"),
        }
        if !self.up_restored {
            text.push_str("; link did not come back");
        }
        text
    }
}

/// Record a note once. The verify polls repeat the same failure every 25 ms.
fn note(result: &mut Measurement, message: String) {
    if !result.notes.iter().any(|existing| existing == &message) {
        result.notes.push(message);
    }
}

/// `Ok(state)` from the profile's state command, or `Err` describing why the
/// state could not be read. `None` means the output was not a state word.
fn probe_link(settings: &NetworkSettings) -> Result<bool, String> {
    let output = run_capture(&settings.state, COMMAND_TIMEOUT)?;
    parse_state(&output).ok_or_else(|| format!("unrecognised link state: {:?}", shorten(&output)))
}

fn parse_state(output: &str) -> Option<bool> {
    let value = output
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?
        .to_ascii_lowercase();
    // Drop an `nmcli` field prefix such as `WIFI:` when the terse format is not
    // honoured by an older version.
    let value = value
        .rsplit(':')
        .next()
        .unwrap_or(&value)
        .trim()
        .to_string();
    match value.as_str() {
        "enabled" | "on" | "up" | "1" | "true" => Some(true),
        "disabled" | "off" | "down" | "lowerlayerdown" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// Run one down/verify/settle/up cycle. Always tries to leave the link up: the
/// measurement is diagnostic, so the machine must not be left disconnected
/// because a phase misbehaved.
fn measure(settings: &NetworkSettings) -> Measurement {
    let mut result = Measurement::default();

    let (irq_up, rx_up) = sample_rates(settings);
    result.irq_up = irq_up;
    result.rx_up = rx_up;
    if irq_up.is_none() {
        log::warn!(
            "network: no /proc/interrupts line matched {:?}; set network.irq_match \
             to the driver name (for example iwlwifi)",
            settings.irq_match
        );
    }

    match run_steps(&settings.down) {
        Ok(elapsed) => result.down = elapsed,
        Err(error) => note(&mut result, format!("down command failed: {error}")),
    }

    let started = Instant::now();
    let deadline = started + settings.verify_timeout;
    let mut link_down = false;
    loop {
        match probe_link(settings) {
            Ok(false) => {
                link_down = true;
                break;
            }
            Ok(true) => {}
            // The poll runs every 25 ms for up to the verify deadline. One
            // note per distinct failure, not one per poll.
            Err(error) => note(&mut result, error),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    result.link_down = link_down;
    if link_down {
        result.verify = Some(started.elapsed());
    }

    // The radio's own teardown outlives `nmcli` returning; sample quiet only
    // after the configured window.
    thread::sleep(settings.settle);
    result.settle = settings.settle;
    let (irq_down, rx_down) = sample_rates(settings);
    result.irq_down = irq_down;
    result.rx_down = rx_down;

    match run_steps(&settings.up) {
        Ok(elapsed) => result.up = elapsed,
        Err(error) => note(&mut result, format!("up command failed: {error}")),
    }

    let started = Instant::now();
    let deadline = started + settings.verify_timeout;
    loop {
        match probe_link(settings) {
            Ok(true) => {
                result.up_restored = true;
                break;
            }
            Ok(false) => {}
            Err(error) => note(&mut result, error),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !result.up_restored {
        // Do not sleep on the failure: it is the operator's link.
        log::error!(
            "network: {} {} did not come back within {}ms",
            settings.kind.as_str(),
            settings.interface,
            settings.verify_timeout.as_millis()
        );
    }
    result
}

/// A pair of monotonically increasing counters: interrupts, received packets.
type Counters = (Option<u64>, Option<u64>);

/// Sample the device for one window and return `(interrupts/s, packets/s)`.
/// Both are `None` when the counter cannot be read: a missing sample is not
/// zero, and reporting it as zero would fake the result this pass exists to
/// produce.
fn sample_rates(settings: &NetworkSettings) -> (Option<f64>, Option<f64>) {
    let before = counters(settings);
    thread::sleep(settings.sample);
    let after = counters(settings);
    (
        rate(before.0, after.0, settings.sample),
        rate(before.1, after.1, settings.sample),
    )
}

fn rate(before: Option<u64>, after: Option<u64>, window: Duration) -> Option<f64> {
    let (before, after) = (before?, after?);
    let seconds = window.as_secs_f64();
    if seconds <= 0.0 || after < before {
        return None;
    }
    Some((after - before) as f64 / seconds)
}

fn counters(settings: &NetworkSettings) -> Counters {
    (
        interrupts(&settings.irq_match),
        rx_packets(&settings.interface),
    )
}

/// Sum the counters on every `/proc/interrupts` line that mentions the match.
/// WiFi IRQ lines usually carry the driver name (`iwlwifi`), not the interface,
/// which is why `irq_match` exists as its own key.
fn interrupts(match_text: &str) -> Option<u64> {
    if match_text.trim().is_empty() {
        return None;
    }
    let text = fs::read_to_string("/proc/interrupts").ok()?;
    let mut total = 0u64;
    let mut matched = false;
    for line in text.lines() {
        if !line.contains(match_text) {
            continue;
        }
        matched = true;
        // The leading token is the IRQ number with a colon; every other numeric
        // token is a per-CPU count.
        for token in line.split_whitespace().skip(1) {
            if let Ok(count) = token.parse::<u64>() {
                total = total.saturating_add(count);
            }
        }
    }
    matched.then_some(total)
}

fn rx_packets(interface: &str) -> Option<u64> {
    let text =
        fs::read_to_string(format!("/sys/class/net/{interface}/statistics/rx_packets")).ok()?;
    text.trim().parse().ok()
}

/// The device that carries the default route: the internet path the profile is
/// claiming to control. A mismatch means taking the configured interface down
/// would not stop internet traffic, so the measurement would understate the
/// cost.
pub fn default_route_device() -> Option<String> {
    let text = fs::read_to_string("/proc/net/route").ok()?;
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let device = fields.next()?;
        let destination = fields.next()?;
        if destination == "00000000" {
            return Some(device.to_string());
        }
    }
    None
}

fn run_steps(steps: &[Vec<String>]) -> Result<Duration, String> {
    let started = Instant::now();
    for step in steps {
        wait_for_command(step, COMMAND_TIMEOUT)?;
    }
    Ok(started.elapsed())
}

/// Run one argv to completion, discarding its output. A non-zero exit is an
/// error: on a kiosk box `nmcli` fails when polkit denies the request, and that
/// must be visible rather than silently unmeasured.
fn wait_for_command(argv: &[String], timeout: Duration) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    match wait_for_exit(&mut child, timeout).map_err(|error| error.to_string())? {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(format!("{} exited with {status}", label(argv))),
        None => Err(format!("{} timed out", label(argv))),
    }
}

/// Run one argv and return its combined output. Used for the state command,
/// whose answer is the whole point.
fn run_capture(argv: &[String], timeout: Duration) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let status = wait_for_exit(&mut child, timeout).map_err(|error| error.to_string())?;
    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        }),
        Some(status) => Err(format!(
            "{} exited with {status}: {}",
            label(argv),
            shorten(stderr.trim())
        )),
        None => Err(format!("{} timed out", label(argv))),
    }
}

fn read_pipe(mut pipe: impl std::io::Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    })
}

/// `Ok(Some(status))` when the command exited, `Ok(None)` on timeout.
fn wait_for_exit(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn label(argv: &[String]) -> String {
    argv.join(" ")
}

fn shorten(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(MAX - 3).collect();
    shortened.push_str("...");
    shortened
}

/// What the backend wrapper tells the worker each frame: the two facts that
/// decide whether the link may come up.
#[derive(Clone, Copy, Debug, Default)]
struct Observation {
    /// A program is loaded, or MDI is selected: the machine can be asked to
    /// move without the operator doing anything else. This is what arms the
    /// interlock, instead of "idle", so the link can stay up while nobody is
    /// cutting and remote access survives.
    armed: bool,
    /// Motion or a program is executing. The link never comes up here, whatever
    /// else is true.
    busy: bool,
}

/// How long a client waits for the link to come up before it runs anyway: the
/// client reports its own network error, so a stuck link must not become a stuck
/// machine.
const LEASE_TIMEOUT: Duration = Duration::from_secs(20);

/// A claim on the link, or the release of one.
enum GateRequest {
    Up {
        reason: String,
        done: mpsc::Sender<bool>,
    },
    Down,
}

/// The half of the supervisor a client sees. The Loobric wrapper holds a lease
/// around each client run, which is the only reason the link is ever up once the
/// interlock is armed.
pub trait NetworkLease: Send + Sync {
    /// Bring the link up and hold it until [`NetworkLease::release`]. Returns
    /// false when it could not be verified up before [`LEASE_TIMEOUT`].
    fn acquire(&self, reason: &str) -> bool;
    /// Drop this reason's claim. The link goes down when none is left.
    fn release(&self, reason: &str);
}

pub struct SupervisorHandle {
    status: Arc<Mutex<NetworkStatus>>,
    observation: Arc<Mutex<Observation>>,
    safe: Arc<AtomicBool>,
    requests: mpsc::Sender<GateRequest>,
}

impl SupervisorHandle {
    fn start(settings: NetworkSettings) -> Arc<Self> {
        let status = Arc::new(Mutex::new(NetworkStatus {
            enabled: true,
            kind: settings.kind.as_str().to_string(),
            interface: settings.interface.clone(),
            message: "not measured yet".into(),
            ..NetworkStatus::default()
        }));
        let observation = Arc::new(Mutex::new(Observation::default()));
        let safe = Arc::new(AtomicBool::new(false));
        let (requests, receiver) = mpsc::channel();
        let worker_status = Arc::clone(&status);
        let worker_observation = Arc::clone(&observation);
        let worker_safe = Arc::clone(&safe);
        thread::Builder::new()
            .name("linc-network".into())
            .spawn(move || {
                run_worker(
                    settings,
                    &worker_status,
                    &worker_observation,
                    &worker_safe,
                    receiver,
                );
            })
            .expect("spawn network worker");
        Arc::new(Self {
            status,
            observation,
            safe,
            requests,
        })
    }

    fn observe(&self, snapshot: &MachineSnapshot) {
        // `armed`: a loaded program means the panel can start a cut without LiNC
        // seeing the press, and MDI mode means the next command can move the
        // axes. Both need the link down in advance; nothing else does.
        let armed = snapshot.program.path.is_some() || matches!(snapshot.task.mode, TaskMode::Mdi);
        let busy = snapshot.task.running || snapshot.task.interp != InterpState::Idle;
        *lock(&self.observation) = Observation { armed, busy };
    }

    fn status(&self) -> NetworkStatus {
        lock(&self.status).clone()
    }

    /// The interlock reading: motion may happen while this is true.
    pub fn safe(&self) -> bool {
        self.safe.load(Ordering::SeqCst)
    }
}

impl NetworkLease for SupervisorHandle {
    fn acquire(&self, reason: &str) -> bool {
        let (done, wait) = mpsc::channel();
        if self
            .requests
            .send(GateRequest::Up {
                reason: reason.to_string(),
                done,
            })
            .is_err()
        {
            return false;
        }
        wait.recv_timeout(LEASE_TIMEOUT).unwrap_or(false)
    }

    fn release(&self, _reason: &str) {
        let _ = self.requests.send(GateRequest::Down);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn run_worker(
    settings: NetworkSettings,
    status: &Arc<Mutex<NetworkStatus>>,
    observation: &Arc<Mutex<Observation>>,
    safe: &Arc<AtomicBool>,
    requests: Receiver<GateRequest>,
) {
    let route = default_route_device();
    let mismatch = route
        .as_deref()
        .is_some_and(|device| device != settings.interface);
    {
        let mut status = lock(status);
        status.route_mismatch = mismatch;
    }
    if mismatch {
        log::warn!(
            "network: interface {} is not the default-route device {}; \
             taking it down will not stop internet traffic",
            settings.interface,
            route.unwrap_or_default()
        );
    }

    match probe_link(&settings) {
        Ok(up) => {
            log::info!(
                "network: {} {} link is {}",
                settings.kind.as_str(),
                settings.interface,
                if up { "up" } else { "down" }
            );
            lock(status).link_up = Some(up);
        }
        Err(error) => {
            log::warn!("network: cannot read the link state: {error}");
            lock(status).message = error;
        }
    }

    if settings.keep_up {
        lock(status).message = "kept up (network.keep_up)".into();
        lock(status).safe = true;
        safe.store(true, Ordering::SeqCst);
        log::info!("network: keep_up set, leaving the link alone");
        return;
    }

    // The link follows two facts, not the clock. `armed` (a program is loaded
    // or MDI is selected) means it must be down before the panel can start
    // anything; `!armed && !busy` means it can come back up, which is what keeps
    // SSH and log reading working between jobs. The first down cycle is also the
    // measurement, so the numbers still come from a real toggle on this machine.
    let mut sampled = false;
    let mut leases = 0usize;
    loop {
        let observation = *lock(observation);
        if leases == 0 && !observation.busy {
            if observation.armed && !safe.load(Ordering::SeqCst) {
                let sample = settings.measure_on_startup && !sampled;
                if sample {
                    lock(status).measuring = true;
                    lock(status).last_measure_ms = Some(now_ms());
                    let (irq, rx) = sample_rates(&settings);
                    let mut status = lock(status);
                    status.irq_per_s_up = irq;
                    status.rx_per_s_up = rx;
                }
                down_cycle(&settings, status, safe, sample);
                sampled = true;
            } else if !observation.armed && safe.load(Ordering::SeqCst) {
                up_cycle(&settings, status, safe, "idle");
            }
        }

        match requests.recv_timeout(TICK) {
            Ok(GateRequest::Up { reason, done }) => {
                leases += 1;
                let verified = up_cycle(&settings, status, safe, &reason);
                let _ = done.send(verified);
            }
            Ok(GateRequest::Down) => {
                leases = leases.saturating_sub(1);
                if leases == 0 {
                    down_cycle(&settings, status, safe, false);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Take the link down, verify it, wait out the settle window, and (optionally)
/// sample the device. Sets `safe` only when the down state was verified: an
/// unverified down is exactly the state that must block motion.
fn down_cycle(
    settings: &NetworkSettings,
    status: &Arc<Mutex<NetworkStatus>>,
    safe: &Arc<AtomicBool>,
    sample: bool,
) -> bool {
    let mut notes = Vec::new();
    let down = match run_steps(&settings.down) {
        Ok(elapsed) => elapsed,
        Err(error) => {
            notes.push(format!("down command failed: {error}"));
            Duration::ZERO
        }
    };
    let (link_down, verify) = verify_until(settings, false, &mut notes);
    // The radio's own teardown outlives `nmcli` returning, and the measurement
    // on the BaierMill showed a burst after the state reads down; nothing may
    // move until the window has passed.
    thread::sleep(settings.settle);
    let measured = sample.then(|| sample_rates(settings));
    if link_down {
        safe.store(true, Ordering::SeqCst);
    }
    {
        let mut status = lock(status);
        status.link_up = Some(!link_down);
        status.safe = link_down;
        status.settle_ms = Some(settings.settle.as_millis() as u64);
        status.down_ms = Some(down.as_millis() as u64);
        status.verify_ms = verify.map(|value| value.as_millis() as u64);
        status.measuring = false;
        status.lease = None;
        if let Some((irq, rx)) = measured {
            status.irq_per_s_down = irq;
            status.rx_per_s_down = rx;
        }
        status.message = if link_down {
            "link down, motion allowed".into()
        } else {
            "link did not read down, motion blocked".into()
        };
    }
    for note in &notes {
        log::warn!("network: {note}");
    }
    log::info!(
        "network: {} {} down in {}ms, verify {}, settle {}",
        settings.kind.as_str(),
        settings.interface,
        down.as_millis(),
        verify
            .map(|value| format!("{}ms", value.as_millis()))
            .unwrap_or_else(|| "not reached".into()),
        settings.settle.as_millis()
    );
    link_down
}

/// Bring the link up for a lease, verify it and give the connection the settle
/// window: radio on is not association, and the client runs the moment this
/// returns.
fn up_cycle(
    settings: &NetworkSettings,
    status: &Arc<Mutex<NetworkStatus>>,
    safe: &Arc<AtomicBool>,
    reason: &str,
) -> bool {
    // The link is up from here on, whatever the command does: motion must stay
    // gated until a down cycle has verified again.
    safe.store(false, Ordering::SeqCst);
    let mut notes = Vec::new();
    let up = match run_steps(&settings.up) {
        Ok(elapsed) => elapsed,
        Err(error) => {
            notes.push(format!("up command failed: {error}"));
            Duration::ZERO
        }
    };
    let (restored, verify) = verify_until(settings, true, &mut notes);
    thread::sleep(settings.settle);
    {
        let mut status = lock(status);
        status.link_up = Some(restored);
        status.safe = false;
        status.up_ms = Some(up.as_millis() as u64);
        status.verify_ms = verify.map(|value| value.as_millis() as u64);
        status.lease = if reason == "idle" {
            None
        } else {
            Some(reason.to_string())
        };
        status.message = if reason == "idle" {
            "link up, nothing loaded".into()
        } else {
            format!("link up for {reason}")
        };
    }
    for note in &notes {
        log::warn!("network: {note}");
    }
    log::info!(
        "network: {} {} up for {reason} in {}ms (verify {})",
        settings.kind.as_str(),
        settings.interface,
        up.as_millis(),
        if restored { "ok" } else { "not reached" }
    );
    restored
}

/// Poll the state command until it reads `want`, or the deadline passes.
fn verify_until(
    settings: &NetworkSettings,
    want: bool,
    notes: &mut Vec<String>,
) -> (bool, Option<Duration>) {
    let started = Instant::now();
    let deadline = started + settings.verify_timeout;
    loop {
        match probe_link(settings) {
            Ok(state) if state == want => return (true, Some(started.elapsed())),
            Ok(_) => {}
            Err(error) => note_in(notes, error),
        }
        if Instant::now() >= deadline {
            return (false, None);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn note_in(notes: &mut Vec<String>, message: String) {
    if !notes.iter().any(|existing| existing == &message) {
        notes.push(message);
    }
}

/// One measurement without a supervisor, for commissioning:
/// `linc --network-measure`. It runs the same cycle the supervisor would run
/// once the machine is idle, and returns the status that cycle would publish, so
/// the numbers can be collected before a profile enables the section at all.
///
/// The caller is responsible for the machine being idle: that is the whole point
/// of the check the supervisor performs, and a commissioning run is an explicit
/// operator action.
pub fn measure_now(settings: &NetworkSettings) -> NetworkStatus {
    let measurement = measure(settings);
    for note in &measurement.notes {
        log::warn!("network: {note}");
    }
    log::info!(
        "network: measured {} {}: {}",
        settings.kind.as_str(),
        settings.interface,
        measurement.summary()
    );
    status_from(settings, &measurement)
}

fn status_from(settings: &NetworkSettings, measurement: &Measurement) -> NetworkStatus {
    NetworkStatus {
        enabled: true,
        kind: settings.kind.as_str().to_string(),
        interface: settings.interface.clone(),
        // The cycle ends by bringing the link back, so that is the state it
        // leaves behind; a failed restore is recorded in the summary.
        link_up: Some(measurement.up_restored),
        // The commissioning cycle ends with the link back up, so it is not a
        // safe state: this status is not the interlock.
        safe: false,
        lease: None,
        measuring: false,
        last_measure_ms: Some(now_ms()),
        down_ms: Some(measurement.down.as_millis() as u64),
        verify_ms: measurement.verify.map(|value| value.as_millis() as u64),
        settle_ms: Some(measurement.settle.as_millis() as u64),
        up_ms: Some(measurement.up.as_millis() as u64),
        irq_per_s_up: measurement.irq_up,
        irq_per_s_down: measurement.irq_down,
        rx_per_s_up: measurement.rx_up,
        rx_per_s_down: measurement.rx_down,
        message: measurement.summary(),
        route_mismatch: false,
    }
}

/// Start the supervisor for an enabled profile. `None` when `[network]` is off,
/// so a machine without it starts no worker and publishes no status - and its
/// `linc.net-safe` output stays true, keeping motion ungated.
pub fn supervisor_for(profile: &Profile) -> Option<Arc<SupervisorHandle>> {
    NetworkSettings::from_profile(profile).map(SupervisorHandle::start)
}

/// Wrap the backend when a supervisor exists. A profile without one gets the
/// backend untouched.
pub fn wrap_backend(
    backend: Box<dyn MachineBackend>,
    supervisor: Option<Arc<SupervisorHandle>>,
) -> Box<dyn MachineBackend> {
    match supervisor {
        Some(supervisor) => Box::new(NetworkBackend::new(backend, supervisor)),
        None => backend,
    }
}

/// The commands that start motion: MDI (which is also the probe and the tool
/// change), run-from-line, and a conversational tool change. Jog, cycle start
/// and homing never reach here - the panel drives those straight into halui,
/// which is why the HAL interlock exists as well as this check.
fn starts_motion(command: &MachineCommand) -> bool {
    matches!(
        command,
        MachineCommand::IssueMdi { .. }
            | MachineCommand::RunFromLine { .. }
            | MachineCommand::IssueToolChange { .. }
    )
}

/// Backend wrapper that publishes [`NetworkStatus`] and refuses the commands
/// that start motion while the link is up.
pub struct NetworkBackend<B> {
    inner: B,
    supervisor: Arc<SupervisorHandle>,
}

impl<B: MachineBackend> NetworkBackend<B> {
    pub fn new(inner: B, supervisor: Arc<SupervisorHandle>) -> Self {
        Self { inner, supervisor }
    }
}

impl<B: MachineBackend> MachineBackend for NetworkBackend<B> {
    fn snapshot(&mut self) -> MachineSnapshot {
        let mut snapshot = self.inner.snapshot();
        self.supervisor.observe(&snapshot);
        snapshot.network = Some(self.supervisor.status());
        snapshot
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        // The interlock, in the one place LiNC can see: a command that starts
        // motion is refused while the link is up for a client run. The panel's
        // own paths are gated in HAL instead, because they never come through
        // here.
        if starts_motion(&command) && !self.supervisor.safe() {
            return Err(MachineError::NetworkBusy);
        }
        self.inner.send(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crate::model::{MachineVariant, TaskState};

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path.display().to_string()
    }

    /// A fake link whose state lives in a file: `state.sh` prints it,
    /// `down.sh`/`up.sh` write it. The defaults under test are replaced, so the
    /// suite never touches the real network.
    fn fixture(dir: &Path) -> (NetworkSettings, std::path::PathBuf) {
        let state_file = dir.join("state.txt");
        fs::write(&state_file, "up\n").unwrap();
        let state = script(dir, "state.sh", &format!("cat {}", state_file.display()));
        let down = script(
            dir,
            "down.sh",
            &format!("printf 'down\\n' > {}", state_file.display()),
        );
        let up = script(
            dir,
            "up.sh",
            &format!("printf 'up\\n' > {}", state_file.display()),
        );
        let settings = NetworkSettings {
            kind: LinkKind::Ethernet,
            interface: "test0".into(),
            down: vec![vec![down]],
            up: vec![vec![up]],
            state: vec![state],
            settle: Duration::from_millis(10),
            verify_timeout: Duration::from_millis(500),
            measure_on_startup: true,
            keep_up: false,
            irq_match: "test0".into(),
            sample: Duration::from_millis(10),
            expect_loobric_startup: false,
        };
        (settings, state_file)
    }

    #[test]
    fn defaults_act_on_one_device_per_kind() {
        let wifi = NetworkSettings::from_config(
            &NetworkConfig {
                enabled: true,
                kind: "wifi".into(),
                interface: "wlp0s20f3".into(),
                connection: Some("OpenWrt".into()),
                ..NetworkConfig::default()
            },
            false,
        );
        assert_eq!(
            wifi.down,
            vec![argv(&["nmcli", "radio", "wifi", "off"])],
            "wifi must switch the radio, never system networking"
        );
        assert_eq!(
            wifi.up,
            vec![
                argv(&["nmcli", "radio", "wifi", "on"]),
                argv(&["nmcli", "connection", "up", "OpenWrt"]),
            ]
        );

        let ethernet = NetworkSettings::from_config(
            &NetworkConfig {
                enabled: true,
                kind: "ethernet".into(),
                interface: "usb0".into(),
                ..NetworkConfig::default()
            },
            false,
        );
        assert_eq!(
            ethernet.down,
            vec![argv(&["nmcli", "device", "disconnect", "usb0"])]
        );
        assert_eq!(
            ethernet.state,
            argv(&["cat", "/sys/class/net/usb0/operstate"]),
            "carrier state, not a route deletion"
        );
    }

    #[test]
    fn measurement_records_every_phase_and_leaves_the_link_up() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, state_file) = fixture(dir.path());
        let measurement = measure(&settings);
        assert!(measurement.link_down, "{measurement:?}");
        assert!(measurement.verify.is_some(), "{measurement:?}");
        assert_eq!(measurement.settle, Duration::from_millis(10));
        assert!(measurement.up_restored, "{measurement:?}");
        assert!(measurement.notes.is_empty(), "{measurement:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
        // The fake interrupts match nothing, so the rates are honestly absent
        // rather than zero.
        assert!(measurement.irq_up.is_none(), "{measurement:?}");
    }

    #[test]
    fn a_link_that_never_drops_is_reported_and_still_restored() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, state_file) = fixture(dir.path());
        // A down command that does nothing: the state never changes.
        let noop = script(dir.path(), "noop.sh", "exit 0");
        settings.down = vec![vec![noop]];
        settings.verify_timeout = Duration::from_millis(100);
        let measurement = measure(&settings);
        assert!(!measurement.link_down, "{measurement:?}");
        assert!(measurement.verify.is_none(), "{measurement:?}");
        assert!(measurement.up_restored, "{measurement:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
        assert!(measurement.summary().contains("never read down"));
    }

    #[test]
    fn a_failing_down_command_is_a_note_not_a_silent_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, _) = fixture(dir.path());
        let failing = script(dir.path(), "fail.sh", "exit 1");
        settings.down = vec![vec![failing]];
        let measurement = measure(&settings);
        assert!(
            measurement
                .notes
                .iter()
                .any(|note| note.contains("down command failed")),
            "{measurement:?}"
        );
    }

    #[test]
    fn measure_now_publishes_the_cycle_without_a_supervisor() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, state_file) = fixture(dir.path());
        let status = measure_now(&settings);
        assert!(status.enabled, "{status:?}");
        assert_eq!(status.interface, "test0");
        assert_eq!(status.link_up, Some(true), "the cycle restores the link");
        assert!(status.down_ms.is_some() && status.up_ms.is_some());
        assert!(status.verify_ms.is_some(), "{status:?}");
        assert!(status.message.starts_with("down "), "{}", status.message);
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
    }

    #[test]
    fn keep_up_starts_no_measurement() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, state_file) = fixture(dir.path());
        settings.keep_up = true;
        let supervisor = SupervisorHandle::start(settings);
        // The worker probes the state and returns without toggling.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if supervisor.status().message.contains("keep_up") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let status = supervisor.status();
        assert!(status.message.contains("keep_up"), "{status:?}");
        assert_eq!(status.link_up, Some(true), "{status:?}");
        assert!(status.last_measure_ms.is_none(), "{status:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
    }

    #[test]
    fn the_link_follows_a_loaded_program() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, state_file) = fixture(dir.path());
        let supervisor = SupervisorHandle::start(settings);
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.task.interp = InterpState::Idle;
        supervisor.observe(&snapshot);

        // Nothing loaded, no MDI: the link stays up, so the machine is reachable
        // and motion is not claimed safe. Nothing can be started anyway.
        thread::sleep(Duration::from_millis(300));
        assert!(!supervisor.safe());
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");

        // A program is loaded: armed, so the down cycle runs before the panel can
        // start anything, and the first one is also the measurement.
        snapshot.program.path = Some(std::path::PathBuf::from("/nc_files/x.ngc"));
        supervisor.observe(&snapshot);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !supervisor.safe() {
            thread::sleep(Duration::from_millis(10));
        }
        let status = supervisor.status();
        assert!(supervisor.safe(), "{status:?}");
        assert!(status.down_ms.is_some(), "{status:?}");
        assert!(
            status.last_measure_ms.is_some(),
            "the first down is sampled"
        );
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "down");

        // Unloaded: the operator is done cutting, so the link comes back and
        // remote access returns.
        snapshot.program.path = None;
        supervisor.observe(&snapshot);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && supervisor.safe() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!supervisor.safe(), "unloading releases the link");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
    }

    struct FakeBackend {
        sends: Arc<Mutex<u64>>,
        snapshot: MachineSnapshot,
    }

    impl MachineBackend for FakeBackend {
        fn snapshot(&mut self) -> MachineSnapshot {
            self.snapshot.clone()
        }

        fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
            *lock(&self.sends) += 1;
            Ok(())
        }
    }

    #[test]
    fn the_wrapper_publishes_status_and_refuses_motion_until_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, _) = fixture(dir.path());
        settings.keep_up = true;
        let sends = Arc::new(Mutex::new(0));
        let backend = FakeBackend {
            sends: Arc::clone(&sends),
            snapshot: MachineSnapshot::empty(MachineVariant::Mill),
        };
        let supervisor = SupervisorHandle::start(settings);
        let mut backend = NetworkBackend::new(backend, Arc::clone(&supervisor));
        let snapshot = backend.snapshot();
        assert!(snapshot.network.is_some(), "status must be published");

        // `keep_up` leaves the link alone, which also leaves motion ungated: an
        // operator who opted out of supervision did not ask for a locked machine.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !supervisor.safe() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(supervisor.safe(), "kept-up link counts as safe");

        backend.send(MachineCommand::ReloadToolTable).unwrap();
        assert_eq!(*lock(&sends), 1, "a non-motion command passes through");
        backend
            .send(MachineCommand::IssueMdi {
                block: "G0 X0".into(),
                origin: crate::command::MdiOrigin::Mdi,
            })
            .unwrap();
        assert_eq!(*lock(&sends), 2, "safe means MDI is allowed");
    }

    #[test]
    fn motion_is_refused_while_the_link_is_up_for_a_lease() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, _) = fixture(dir.path());
        let sends = Arc::new(Mutex::new(0));
        let backend = FakeBackend {
            sends: Arc::clone(&sends),
            snapshot: MachineSnapshot::empty(MachineVariant::Mill),
        };
        let supervisor = SupervisorHandle::start(settings);
        let mut backend = NetworkBackend::new(backend, Arc::clone(&supervisor));

        // Armed: a program is loaded, so the link is down and motion is allowed.
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.task.interp = InterpState::Idle;
        snapshot.program.path = Some(std::path::PathBuf::from("/nc_files/x.ngc"));
        supervisor.observe(&snapshot);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !supervisor.safe() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(supervisor.safe(), "armed means the link is down");

        // A client takes the link for a run: motion is refused while it is up.
        assert!(supervisor.acquire("test"), "the fake link comes up");
        let error = backend
            .send(MachineCommand::RunFromLine { line: 1 })
            .unwrap_err();
        assert_eq!(error, MachineError::NetworkBusy);
        assert_eq!(
            *lock(&sends),
            0,
            "the refused command never reached the machine"
        );

        // Release: the down cycle verifies again, and motion returns.
        supervisor.release("test");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !supervisor.safe() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(supervisor.safe(), "the release takes the link back down");
        backend
            .send(MachineCommand::RunFromLine { line: 1 })
            .unwrap();
        assert_eq!(*lock(&sends), 1);
    }

    #[test]
    fn the_state_parser_reads_nmcli_and_operstate_words() {
        assert_eq!(parse_state("enabled\n"), Some(true));
        assert_eq!(parse_state("disabled\n"), Some(false));
        assert_eq!(parse_state("up\n"), Some(true));
        assert_eq!(parse_state("lowerlayerdown\n"), Some(false));
        assert_eq!(parse_state("dormant\n"), None);
        assert_eq!(parse_state(""), None);
    }
}
