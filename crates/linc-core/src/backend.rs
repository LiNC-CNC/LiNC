//! Backend boundary and the worker thread that owns it.
//!
//! The UI never speaks NML, HAL, serial or Python. It sends typed commands to
//! a worker, reads snapshots, and drains asynchronous events.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::command::{
    BackendEvent, CommandResult, HalValue, LincOutput, MachineCommand, MachineError,
};
use crate::model::{now_ms, Alarm, AlarmSeverity, AlarmSource, MachineSnapshot, StatusSource};

/// A backend owns machine state and executes commands. It must never block the
/// UI thread: it is driven by [`BackendHandle`]'s worker.
pub trait MachineBackend: Send + 'static {
    fn snapshot(&mut self) -> MachineSnapshot;
    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError>;
}

/// Typed HAL writer. Only [`LincOutput`] values can reach it.
pub trait HalWriter: Send + 'static {
    fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError>;
}

pub struct NoopHal;

impl HalWriter for NoopHal {
    fn write(&mut self, _output: LincOutput, _value: HalValue) -> Result<(), MachineError> {
        Ok(())
    }
}

struct Shared {
    snapshot: Mutex<MachineSnapshot>,
    events: Mutex<VecDeque<BackendEvent>>,
    generation: Mutex<u64>,
    condvar: Condvar,
    shutdown: AtomicBool,
    next_id: AtomicU64,
}

/// Cloneable handle for the UI.
pub struct BackendHandle {
    shared: Arc<Shared>,
    tx: std::sync::mpsc::Sender<Request>,
    join: Option<JoinHandle<()>>,
}

enum Request {
    Command { id: u64, command: MachineCommand },
    Shutdown,
}

impl BackendHandle {
    pub fn spawn(
        mut backend: Box<dyn MachineBackend>,
        mut hal: Option<Box<dyn HalWriter>>,
        status_hz: u32,
        status_source: StatusSource,
    ) -> Self {
        let initial = backend.snapshot();
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(initial),
            events: Mutex::new(VecDeque::new()),
            generation: Mutex::new(0),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_shared = shared.clone();
        let period = Duration::from_secs_f64(1.0 / status_hz.max(1) as f64);
        let join = thread::Builder::new()
            .name("linc-backend".into())
            .spawn(move || {
                run_worker(
                    &mut backend,
                    &mut hal,
                    &worker_shared,
                    rx,
                    period,
                    status_source,
                );
            })
            .expect("spawn backend worker");

        Self {
            shared,
            tx,
            join: Some(join),
        }
    }

    pub fn snapshot(&self) -> MachineSnapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn send(&self, command: MachineCommand) -> u64 {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Request::Command { id, command });
        id
    }

    pub fn drain_events(&self) -> Vec<BackendEvent> {
        let mut events = self
            .shared
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        events.drain(..).collect()
    }

    pub fn has_events(&self) -> bool {
        !self
            .shared
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
    }

    /// Block until a snapshot newer than `generation` is available, or timeout.
    pub fn wait_for_change(&self, generation: u64, timeout: Duration) -> u64 {
        let mut current = self
            .shared
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let deadline = Instant::now() + timeout;
        while *current <= generation {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, _) = self
                .shared
                .condvar
                .wait_timeout(current, deadline - now)
                .unwrap_or_else(|poison| poison.into_inner());
            current = guard;
        }
        *current
    }

    pub fn shutdown(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Request::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for BackendHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.shutdown();
        }
    }
}

fn run_worker(
    backend: &mut Box<dyn MachineBackend>,
    hal: &mut Option<Box<dyn HalWriter>>,
    shared: &Arc<Shared>,
    rx: std::sync::mpsc::Receiver<Request>,
    period: Duration,
    status_source: StatusSource,
) {
    let mut last_publish = Instant::now() - period;
    let mut last_heartbeat = Instant::now() - Duration::from_secs(1);
    let mut heartbeat = false;
    let mut last_status: Option<i32> = None;
    let mut last_status_write = Instant::now() - Duration::from_secs(1);
    let mut last_confirm_level = false;
    // NaN so the first pass always publishes the initial selection.
    let mut last_jog_increment = f64::NAN;
    let mut last_jog_speed = f64::NAN;
    let mut last_machine_off = false;

    while !shared.shutdown.load(Ordering::Relaxed) {
        let request = rx.recv_timeout(Duration::from_millis(10));
        match request {
            Ok(Request::Command { id, command }) => {
                let label = command.label().to_string();
                let result = backend.send(command);
                if let Err(error) = &result {
                    log::warn!("{label} rejected: {error}");
                }
                let event = BackendEvent::Command(CommandResult {
                    id,
                    command: label,
                    result,
                    timestamp_ms: now_ms(),
                });
                let mut events = shared
                    .events
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                events.push_back(event);
                while events.len() > 512 {
                    events.pop_front();
                }
            }
            Ok(Request::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }

        if last_publish.elapsed() >= period {
            last_publish = Instant::now();
            let mut snapshot = backend.snapshot();
            snapshot.meta.sequence += 1;
            snapshot.meta.captured_at_ms = now_ms();
            // `meta.stale` belongs to the backend: the LinuxCNC bridge raises
            // it when the status link stops updating. Clearing it here erased
            // that flag before the two-digit status display was derived, so a
            // dead link kept showing the last work offset instead of `--`.

            // HAL heartbeat at 2 Hz.
            if last_heartbeat.elapsed() >= Duration::from_millis(500) {
                last_heartbeat = Instant::now();
                heartbeat = !heartbeat;
                snapshot.hal.hmi_alive = heartbeat;
                snapshot.hal.heartbeat_seq += 1;
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) = writer.write(LincOutput::HmiAlive, HalValue::Bit(heartbeat))
                    {
                        log::warn!("HAL hmi-alive write failed: {error}");
                    }
                }
            }

            // Status display on change or at 5 Hz.
            let value = snapshot.status_display_value(status_source);
            snapshot.hal.status_display = value.or(Some(-1));
            if value != last_status || last_status_write.elapsed() >= Duration::from_millis(200) {
                last_status = value;
                last_status_write = Instant::now();
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) = writer.write(
                        LincOutput::StatusDisplay,
                        HalValue::S32(value.unwrap_or(-1)),
                    ) {
                        log::warn!("HAL status-display write failed: {error}");
                    }
                }
            }

            // Selected jog increment for `halui.axis.L.increment`. 0.0 means
            // continuous jog; the machine HAL routes the jog keys by mode.
            let increment = snapshot.hal.jog_increment.unwrap_or(0.0);
            if increment != last_jog_increment {
                last_jog_increment = increment;
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) =
                        writer.write(LincOutput::JogIncrement, HalValue::Float(increment))
                    {
                        log::warn!("HAL jog-increment write failed: {error}");
                    }
                }
            }

            // Jog speed for `halui.axis.jog-speed`, already scaled by the
            // operator's percentage in the HMI.
            let speed = snapshot.hal.jog_speed.unwrap_or(0.0);
            if speed != last_jog_speed {
                last_jog_speed = speed;
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) = writer.write(LincOutput::JogSpeed, HalValue::Float(speed)) {
                        log::warn!("HAL jog-speed write failed: {error}");
                    }
                }
            }

            // Manual tool-change confirmation is a level, not a pulse: it is
            // held for as long as the operator's confirmation stands, so
            // `iocontrol` cannot miss it in a single 100 ms cycle. The backend
            // drops it when `iocontrol.0.tool-change` does.
            if snapshot.hal.tool_change_confirm != last_confirm_level {
                last_confirm_level = snapshot.hal.tool_change_confirm;
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) = writer.write(
                        LincOutput::ToolChangeConfirm,
                        HalValue::Bit(last_confirm_level),
                    ) {
                        log::warn!("HAL tool-change-confirm write failed: {error}");
                    }
                }
            }

            // A latched probe trip switches the machine off. halui accepts a
            // jog only while the task state is ON, and the panel jogs straight
            // into halui, so this is the one response the HMI can enforce.
            // The output is a level: high while the trip stands, low once it is
            // reset. halui acts on the rising edge, so the held level never
            // switches the machine off a second time after the operator powers
            // back on; nothing here retries it.
            //
            // Except when the program was paused at the trip: the operator
            // provokes that on purpose, with a finger on the probe, to prove the
            // input is wired, and nothing was aborted that had to be stopped.
            // The answer rides on the fault, recorded when the message arrived;
            // it cannot be re-derived here, because the trip itself ends the
            // pause.
            let machine_off = snapshot.probe_trip_switched_machine_off();
            snapshot.hal.machine_off = machine_off;
            if machine_off != last_machine_off {
                last_machine_off = machine_off;
                if let Some(writer) = hal.as_mut() {
                    if let Err(error) =
                        writer.write(LincOutput::MachineOff, HalValue::Bit(machine_off))
                    {
                        log::warn!("HAL machine-off write failed: {error}");
                    }
                }
            }

            *shared
                .snapshot
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = snapshot;
            let mut generation = shared
                .generation
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *generation += 1;
            shared.condvar.notify_all();
        }
    }

    if let Some(writer) = hal.as_mut() {
        let _ = writer.write(LincOutput::ToolChangeConfirm, HalValue::Bit(false));
        let _ = writer.write(LincOutput::HmiAlive, HalValue::Bit(false));
        // Leaving the machine off behind a dead HMI would strand a powered
        // machine with no UI left to reset the trip from.
        let _ = writer.write(LincOutput::MachineOff, HalValue::Bit(false));
    }
}

/// Simple synchronous backend handle for tests and headless mode. The worker
/// still runs, but commands are applied by the backend immediately.
pub fn default_alarm(
    severity: AlarmSeverity,
    source: AlarmSource,
    code: &str,
    message: impl Into<String>,
) -> Alarm {
    Alarm::new(1, severity, source, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineVariant, PanelMode, PanelModeValidity};

    struct Fake {
        snapshot: MachineSnapshot,
    }

    impl MachineBackend for Fake {
        fn snapshot(&mut self) -> MachineSnapshot {
            self.snapshot.clone()
        }

        fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
            match command {
                MachineCommand::IssueMdi { .. } => Ok(()),
                _ => Err(MachineError::Unsupported("test".into())),
            }
        }
    }

    #[test]
    fn worker_publishes_snapshots_and_events() {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Auto);
        let handle = BackendHandle::spawn(Box::new(Fake { snapshot }), None, 50, StatusSource::Wcs);
        let first = handle.snapshot().meta.sequence;
        let id = handle.send(MachineCommand::IssueMdi {
            block: "G0 X0".into(),
            origin: crate::command::MdiOrigin::Mdi,
        });
        let mut saw_event = false;
        for _ in 0..100 {
            for event in handle.drain_events() {
                if let BackendEvent::Command(result) = event {
                    assert_eq!(result.id, id);
                    assert!(result.result.is_ok());
                    saw_event = true;
                }
            }
            if saw_event && handle.snapshot().meta.sequence > first {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_event);
        assert!(handle.snapshot().meta.sequence > first);
    }

    #[test]
    fn jog_speed_and_increment_are_published_from_the_snapshot() {
        use std::sync::{Arc, Mutex};
        struct RecordingHal(Arc<Mutex<Vec<(LincOutput, HalValue)>>>);
        impl HalWriter for RecordingHal {
            fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
                self.0.lock().unwrap().push((output, value));
                Ok(())
            }
        }
        struct Fixed {
            snapshot: MachineSnapshot,
        }
        impl MachineBackend for Fixed {
            fn snapshot(&mut self) -> MachineSnapshot {
                self.snapshot.clone()
            }
            fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
                Ok(())
            }
        }
        let recording = Arc::new(Mutex::new(Vec::new()));
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.offsets.active_index = Some(1);
        // The HMI-owned selections, as the bridge would carry them.
        snapshot.hal.jog_speed = Some(42.0);
        snapshot.hal.jog_increment = Some(0.5);
        let _handle = BackendHandle::spawn(
            Box::new(Fixed { snapshot }),
            Some(Box::new(RecordingHal(recording.clone()))),
            50,
            StatusSource::Wcs,
        );
        thread::sleep(Duration::from_millis(200));
        let writes = recording.lock().unwrap().clone();
        assert!(
            writes
                .iter()
                .any(|(output, value)| *output == LincOutput::JogSpeed
                    && *value == HalValue::Float(42.0)),
            "jog speed must reach the HAL: {writes:?}"
        );
        assert!(
            writes
                .iter()
                .any(|(output, value)| *output == LincOutput::JogIncrement
                    && *value == HalValue::Float(0.5)),
            "jog increment must reach the HAL: {writes:?}"
        );
    }

    #[test]
    fn tool_change_confirm_is_held_as_a_level() {
        use std::sync::{Arc, Mutex};
        struct RecordingHal(Arc<Mutex<Vec<(LincOutput, HalValue)>>>);
        impl HalWriter for RecordingHal {
            fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
                self.0.lock().unwrap().push((output, value));
                Ok(())
            }
        }
        /// A backend whose confirmation level the test raises and lowers.
        struct Levelled {
            snapshot: Arc<Mutex<MachineSnapshot>>,
        }
        impl MachineBackend for Levelled {
            fn snapshot(&mut self) -> MachineSnapshot {
                self.snapshot.lock().unwrap().clone()
            }
            fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
                Ok(())
            }
        }
        fn confirms(recording: &Arc<Mutex<Vec<(LincOutput, HalValue)>>>) -> Vec<bool> {
            recording
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(output, value)| match (output, value) {
                    (LincOutput::ToolChangeConfirm, HalValue::Bit(value)) => Some(*value),
                    _ => None,
                })
                .collect()
        }

        let recording = Arc::new(Mutex::new(Vec::new()));
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.offsets.active_index = Some(1);
        let shared = Arc::new(Mutex::new(snapshot));
        let handle = BackendHandle::spawn(
            Box::new(Levelled {
                snapshot: shared.clone(),
            }),
            Some(Box::new(RecordingHal(recording.clone()))),
            50,
            StatusSource::Wcs,
        );

        shared.lock().unwrap().hal.tool_change_confirm = true;
        thread::sleep(Duration::from_millis(250));
        // Held: one write, with no automatic drop after the old 100 ms pulse.
        assert_eq!(
            confirms(&recording),
            vec![true],
            "confirmation must be a level, not a pulse"
        );

        shared.lock().unwrap().hal.tool_change_confirm = false;
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            confirms(&recording),
            vec![true, false],
            "confirmation must drop when the request does"
        );
        drop(handle);
    }

    /// A latched probe trip switches the machine off through HAL: a level, so
    /// one write on the rising edge, none while it holds, and one on the falling
    /// edge when the fault is reset.
    #[test]
    fn a_probe_trip_is_published_as_a_machine_off_level() {
        use crate::faults::{ProbeTripFault, ProbeTripKind};
        use std::sync::{Arc, Mutex};
        struct RecordingHal(Arc<Mutex<Vec<(LincOutput, HalValue)>>>);
        impl HalWriter for RecordingHal {
            fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
                self.0.lock().unwrap().push((output, value));
                Ok(())
            }
        }
        /// A backend whose latched trip the test raises and clears.
        struct Trippable {
            snapshot: Arc<Mutex<MachineSnapshot>>,
        }
        impl MachineBackend for Trippable {
            fn snapshot(&mut self) -> MachineSnapshot {
                self.snapshot.lock().unwrap().clone()
            }
            fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
                Ok(())
            }
        }
        fn levels(recording: &Arc<Mutex<Vec<(LincOutput, HalValue)>>>) -> Vec<bool> {
            recording
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(output, value)| match (output, value) {
                    (LincOutput::MachineOff, HalValue::Bit(value)) => Some(*value),
                    _ => None,
                })
                .collect()
        }
        fn trip() -> ProbeTripFault {
            ProbeTripFault::new(
                ProbeTripKind::JointJog,
                ProbeTripKind::JointJog.message(),
                1,
            )
        }

        let recording = Arc::new(Mutex::new(Vec::new()));
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.task.power = crate::model::MachinePower::On;
        let shared = Arc::new(Mutex::new(snapshot));
        let handle = BackendHandle::spawn(
            Box::new(Trippable {
                snapshot: shared.clone(),
            }),
            Some(Box::new(RecordingHal(recording.clone()))),
            50,
            StatusSource::Wcs,
        );

        thread::sleep(Duration::from_millis(120));
        assert!(
            levels(&recording).is_empty(),
            "no trip, no machine-off write: {:?}",
            levels(&recording)
        );

        shared.lock().unwrap().probe_trip = Some(trip());
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            levels(&recording),
            vec![true],
            "the trip's rising edge must switch the machine off"
        );
        assert!(
            handle.snapshot().hal.machine_off,
            "the published snapshot carries the level for the Diagnostics page"
        );

        // Held for as long as the trip stands: no repeat, so powering back on
        // while it is latched is not switched off again.
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            levels(&recording),
            vec![true],
            "a held level is not re-asserted every poll"
        );

        shared.lock().unwrap().probe_trip = None;
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            levels(&recording),
            vec![true, false],
            "the reset must drop the level"
        );
        drop(handle);
    }

    /// A trip raised with the program paused and nothing moving leaves the
    /// machine on: that is the case the operator provokes on purpose, to prove
    /// the probe input is wired, and nothing was aborted that had to be stopped.
    /// Every other combination on the same backend still switches the machine
    /// off, including a paused program being jogged into the probe.
    #[test]
    fn a_paused_probe_trip_leaves_the_machine_on() {
        use crate::faults::{ProbeTripFault, ProbeTripKind};
        use std::sync::{Arc, Mutex};
        struct RecordingHal(Arc<Mutex<Vec<(LincOutput, HalValue)>>>);
        impl HalWriter for RecordingHal {
            fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
                self.0.lock().unwrap().push((output, value));
                Ok(())
            }
        }
        struct Trippable {
            snapshot: Arc<Mutex<MachineSnapshot>>,
        }
        impl MachineBackend for Trippable {
            fn snapshot(&mut self) -> MachineSnapshot {
                self.snapshot.lock().unwrap().clone()
            }
            fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
                Ok(())
            }
        }
        fn levels(recording: &Arc<Mutex<Vec<(LincOutput, HalValue)>>>) -> Vec<bool> {
            recording
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(output, value)| match (output, value) {
                    (LincOutput::MachineOff, HalValue::Bit(value)) => Some(*value),
                    _ => None,
                })
                .collect()
        }
        fn trip(paused: bool, moving: bool) -> ProbeTripFault {
            ProbeTripFault::new_with_state(
                ProbeTripKind::NonProbeMove,
                ProbeTripKind::NonProbeMove.message(),
                1,
                paused,
                moving,
            )
        }

        let recording = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(Mutex::new(MachineSnapshot::empty(MachineVariant::Mill)));
        let handle = BackendHandle::spawn(
            Box::new(Trippable {
                snapshot: shared.clone(),
            }),
            Some(Box::new(RecordingHal(recording.clone()))),
            50,
            StatusSource::Wcs,
        );

        // Paused and still: the finger test that proves the input is wired.
        shared.lock().unwrap().probe_trip = Some(trip(true, false));
        thread::sleep(Duration::from_millis(250));
        assert!(
            levels(&recording).is_empty(),
            "a trip raised with the program paused and nothing moving must not switch the machine off: {:?}",
            levels(&recording)
        );
        assert!(
            !handle.snapshot().hal.machine_off,
            "the published level stays low for a paused trip"
        );

        // Clearing a paused trip writes nothing either: the level never rose.
        shared.lock().unwrap().probe_trip = None;
        thread::sleep(Duration::from_millis(150));
        assert!(
            levels(&recording).is_empty(),
            "no level means no falling edge: {:?}",
            levels(&recording)
        );

        // A paused program being jogged into the probe is not that case: the
        // controller reports the jog as a non-probe move and the pause is still
        // set, so the machine has to go off.
        shared.lock().unwrap().probe_trip = Some(trip(true, true));
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            levels(&recording),
            vec![true],
            "a jog from a paused program switches the machine off"
        );

        // And a trip raised while the program ran, after a reset.
        shared.lock().unwrap().probe_trip = None;
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            levels(&recording),
            vec![true, false],
            "the reset drops the level"
        );
        shared.lock().unwrap().probe_trip = Some(trip(false, false));
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            levels(&recording),
            vec![true, false, true],
            "a trip raised while the program ran switches the machine off"
        );
        drop(handle);
    }
}
