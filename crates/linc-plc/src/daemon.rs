//! The scan loop: command application, ladder execution, state machine and the
//! shared snapshot.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use linc_plc_core::protocol::{Response, RunState, StateSnapshot};
use linc_plc_core::timing::ScanTiming;
use linc_plc_core::Vm;

use crate::hal::Hal;
use crate::{Command, Shared};

/// The daemon's own state machine. Kept separate from
/// [`linc_plc_core::protocol::RunState`] so the wire enum is only ever
/// constructed on the way out to the UI.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Loaded,
    Running,
    Fault,
}

impl Phase {
    fn run_state(self) -> RunState {
        match self {
            Phase::Idle => RunState::Idle,
            Phase::Loaded => RunState::Loaded,
            Phase::Running => RunState::Running,
            Phase::Fault => RunState::Fault,
        }
    }
}

/// How many scans may pass between two published snapshots.
const SNAPSHOT_EVERY: u64 = 50;

/// A force held by the daemon. `ttl_ms` 0 means "until cleared".
struct Force {
    name: String,
    value: bool,
    expires: Option<Instant>,
}

/// A marker word held at a value. Block members are not words: they own no
/// bank slot, so they cannot be forced.
struct WordForce {
    name: String,
    value: i16,
    expires: Option<Instant>,
}

pub(crate) struct Daemon {
    project: PathBuf,
    phase: Phase,
    fault: Option<String>,
    vm: Option<Vm>,
    hash: String,
    forces: Vec<Force>,
    /// Forced marker words, applied before and after every scan.
    word_forces: Vec<WordForce>,
    scan_count: u64,
    scan_time_us: u32,
    overruns: u64,
    /// Anchored schedule and the scan, cycle and lateness metrics.
    timing: ScanTiming,
    alive: bool,
    /// Watch list of the loaded project, sampled per scan.
    watch: Vec<String>,
    /// Configured scan period, used to turn a window in milliseconds into a
    /// stride in scans.
    period_us: u64,
    /// Retained values: where they go, how often, and the last packed blob.
    retain_path: Option<PathBuf>,
    retain_flush: Duration,
    retain_dirty: Option<Vec<u8>>,
    retain_last: Instant,
    /// The window and stride the ring is currently using.
    trend_window: u32,
    trend_stride: u32,
    trend_tick: u32,
    /// When the previous scan ran; timers advance by the real delta.
    last_scan: Option<Instant>,
}

impl Daemon {
    pub(crate) fn new(
        project: PathBuf,
        retain_path: Option<PathBuf>,
        retain_flush: Duration,
    ) -> Self {
        Self {
            project,
            phase: Phase::Idle,
            fault: None,
            vm: None,
            hash: String::new(),
            forces: Vec::new(),
            word_forces: Vec::new(),
            scan_count: 0,
            scan_time_us: 0,
            overruns: 0,
            timing: ScanTiming::new(Duration::from_millis(2), Instant::now()),
            alive: false,
            watch: Vec::new(),
            period_us: 2000,
            retain_path,
            retain_flush,
            retain_dirty: None,
            retain_last: Instant::now(),
            trend_window: 20_000,
            trend_stride: 1,
            trend_tick: 0,
            last_scan: None,
        }
    }

    /// Apply commands at the top of each scan, then scan, then publish. Runs
    /// until the shared shutdown flag is set.
    pub(crate) fn run(
        &mut self,
        hal: &Hal,
        shared: &Shared,
        commands: &Receiver<Command>,
        period: Duration,
    ) {
        log::info!("linc-plc: scanning every {} us", period.as_micros());
        self.period_us = period.as_micros().max(1) as u64;
        // The schedule is anchored here once. A late wake-up is measured against
        // this expectation and never re-anchors it, so a persistently late run
        // keeps reporting lateness instead of quietly moving the clock.
        self.timing = ScanTiming::new(period.max(Duration::from_micros(1)), Instant::now());
        while !shared.shutdown.load(Ordering::Relaxed) {
            let started = Instant::now();
            self.timing.wake(started);
            while let Ok(command) = commands.try_recv() {
                self.apply(command, hal, shared);
            }

            self.flush_retained(false);
            if self.phase == Phase::Running {
                if let Err(message) = self.execute(hal, shared) {
                    self.fail(message, hal, shared);
                }
            } else if let Err(error) = hal.drive_outputs_false() {
                self.fail(
                    format!("cannot drive the outputs false: {error}"),
                    hal,
                    shared,
                );
            }

            self.scan_time_us = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);

            // The snapshot is a convenience for the UI, not the scan itself:
            // publishing every scan would contend on the mutex, and a load,
            // stop, force or fault publishes immediately anyway.
            if self.scan_count.is_multiple_of(SNAPSHOT_EVERY) {
                self.publish(shared);
            }

            // The heartbeat means "the ladder is executing", not "the process
            // is alive": a watchdog that only sees edges must not stay happy
            // through Idle, Loaded or Fault.
            self.alive = if self.phase == Phase::Running {
                !self.alive
            } else {
                false
            };
            let _ = hal.ctl().set_alive(self.alive);
            let _ = hal.ctl().set_scan_count(self.scan_count as i32);
            let _ = hal.ctl().set_scan_time_us(self.scan_time_us as i32);
            let _ = hal.ctl().set_overruns(self.overruns as i32);

            // Scan time is the body; the schedule moves one interval, and the
            // sleep aims at the anchored expectation rather than at "now".
            self.timing.finish(Instant::now());
            self.timing.schedule_next();
            self.overruns = self.timing.snapshot().overruns;
            let due = self.timing.expected();
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
        }
        // Leaving: publish whatever the last run produced.
        self.flush_retained(true);
    }

    /// The daemon is leaving: make the last published state say so.
    pub(crate) fn mark_stopped(&mut self, shared: &Shared) {
        self.phase = Phase::Fault;
        self.fault = Some("linc-plc stopped".to_string());
        self.alive = false;
        self.publish(shared);
    }

    fn apply(&mut self, command: Command, hal: &Hal, shared: &Shared) {
        match command {
            Command::Load {
                compiled,
                warnings,
                watch,
                reply,
            } => {
                if self.phase == Phase::Running {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "stop the ladder before loading a project".to_string(),
                        },
                    );
                    return;
                }
                // The program is swapped at the top of a scan, never inside
                // one, so an input decodes against exactly one program.
                self.vm = Some(Vm::new(&compiled));
                self.set_watch(watch, shared);
                self.hash = compiled.hash.clone();
                self.restore_retained();
                self.forces.clear();
                self.word_forces.clear();
                self.phase = Phase::Loaded;
                self.fault = None;
                self.alive = false;
                let _ = hal.ctl().set_alive(false);
                let _ = hal.ctl().set_fault(false);
                let _ = hal.drive_outputs_false();
                self.publish(shared);
                answer(
                    reply,
                    Response::Loaded {
                        hash: self.hash.clone(),
                        diagnostics: warnings,
                    },
                );
            }

            Command::Start { reply } => {
                if self.phase != Phase::Loaded {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "start needs a loaded project (Load first)".to_string(),
                        },
                    );
                    return;
                }
                self.phase = Phase::Running;
                self.fault = None;
                let _ = hal.ctl().set_fault(false);
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }

            Command::Step { reply } => {
                // Pause before scanning: the scheduler must not also scan this
                // tick, or one press becomes two scans and the timers run fast.
                if self.vm.is_none() {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "step needs a loaded project (Load first)".to_string(),
                        },
                    );
                    return;
                }
                self.phase = Phase::Loaded;
                let failed = self.execute(hal, shared).err();
                self.alive = false;
                let _ = hal.ctl().set_alive(false);
                if let Some(message) = failed {
                    self.fail(message, hal, shared);
                }
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
            Command::ResetData { reply } => {
                if let Some(vm) = self.vm.as_mut() {
                    vm.reset();
                    vm.clear_forces();
                }
                self.forces.clear();
                self.word_forces.clear();
                if self.phase == Phase::Running {
                    self.phase = Phase::Loaded;
                }
                let _ = hal.drive_outputs_false();
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
            Command::Stop { reply } => {
                if self.vm.is_none() {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "no project is loaded".to_string(),
                        },
                    );
                    return;
                }
                if let Err(error) = hal.drive_outputs_false() {
                    let message = format!("cannot drive the outputs false: {error}");
                    self.fail(message.clone(), hal, shared);
                    answer(
                        reply,
                        Response::Error {
                            code: "hal".to_string(),
                            message,
                        },
                    );
                    return;
                }
                if let Some(vm) = self.vm.as_mut() {
                    vm.reset();
                    vm.clear_forces();
                }
                self.forces.clear();
                self.word_forces.clear();
                self.phase = Phase::Loaded;
                self.fault = None;
                self.alive = false;
                let _ = hal.ctl().set_alive(false);
                let _ = hal.ctl().set_fault(false);
                // A stop is a safe point: the values a run produced are worth
                // keeping even if the process goes away next.
                self.flush_retained(true);
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }

            Command::Force {
                name,
                value,
                ttl_ms,
                reply,
            } => {
                let Some(vm) = self.vm.as_mut() else {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "force needs a loaded project".to_string(),
                        },
                    );
                    return;
                };
                if matches!(self.phase, Phase::Idle | Phase::Fault) {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "force needs a loaded project".to_string(),
                        },
                    );
                    return;
                }
                if let Err(message) = vm.set_force(&name, value) {
                    answer(
                        reply,
                        Response::Error {
                            code: "invalid_force".to_string(),
                            message,
                        },
                    );
                    return;
                }
                let expires = if ttl_ms == 0 {
                    None
                } else {
                    Some(Instant::now() + Duration::from_millis(u64::from(ttl_ms)))
                };
                match self.forces.iter_mut().find(|force| force.name == name) {
                    Some(existing) => {
                        existing.value = value;
                        existing.expires = expires;
                    }
                    None => self.forces.push(Force {
                        name,
                        value,
                        expires,
                    }),
                }
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }

            Command::ForceWord {
                name,
                value,
                ttl_ms,
                reply,
            } => {
                let Some(vm) = self.vm.as_mut() else {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "force needs a loaded project".to_string(),
                        },
                    );
                    return;
                };
                if matches!(self.phase, Phase::Idle | Phase::Fault) {
                    answer(
                        reply,
                        Response::Error {
                            code: "state".to_string(),
                            message: "force needs a loaded project".to_string(),
                        },
                    );
                    return;
                }
                let Some(index) = linc_plc_core::il::word_operand(&name) else {
                    let message = if name.contains('.') {
                        format!("{name} is a block value and cannot be forced")
                    } else {
                        format!("{name} is not a marker word: MW0 to MW15")
                    };
                    answer(
                        reply,
                        Response::Error {
                            code: "invalid_force".to_string(),
                            message,
                        },
                    );
                    return;
                };
                vm.set_word(index, value);
                let expires = if ttl_ms == 0 {
                    None
                } else {
                    Some(Instant::now() + Duration::from_millis(u64::from(ttl_ms)))
                };
                match self.word_forces.iter_mut().find(|force| force.name == name) {
                    Some(existing) => {
                        existing.value = value;
                        existing.expires = expires;
                    }
                    None => self.word_forces.push(WordForce {
                        name,
                        value,
                        expires,
                    }),
                }
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }

            Command::Unforce { name, reply } => {
                if let Some(vm) = self.vm.as_mut() {
                    vm.clear_force(&name);
                }
                self.forces.retain(|force| force.name != name);
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
            Command::UnforceWord { name, reply } => {
                self.word_forces.retain(|force| force.name != name);
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
            Command::ClearForces { reply } => {
                if let Some(vm) = self.vm.as_mut() {
                    vm.clear_forces();
                }
                self.forces.clear();
                self.word_forces.clear();
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
        }
    }

    /// One ladder scan: read the bound inputs, run the program, write the
    /// bound outputs.
    fn execute(&mut self, hal: &Hal, shared: &Shared) -> Result<(), String> {
        self.expire_forces();
        let retain_hash = self.hash.clone();
        let retain_wanted = self.retain_path.is_some();
        let Some(vm) = self.vm.as_mut() else {
            return Err("no program is loaded".to_string());
        };
        let inputs = hal.read_inputs().map_err(|error| error.to_string())?;
        // Timers add the real scan delta, not a fixed tick. The cap keeps a
        // stalled thread from jumping a timer past its preset in one scan.
        let dt_ms = self
            .last_scan
            .map(|last| last.elapsed().as_millis().min(1000) as u16)
            .unwrap_or(20);
        self.last_scan = Some(Instant::now());
        // A forced word is what the program reads, and it holds against the
        // program's own writes: the loop runs before and after the scan.
        for force in &self.word_forces {
            if let Some(index) = linc_plc_core::il::word_operand(&force.name) {
                vm.set_word(index, force.value);
            }
        }
        vm.scan_with_dt(&inputs, dt_ms.max(1));
        for force in &self.word_forces {
            if let Some(index) = linc_plc_core::il::word_operand(&force.name) {
                vm.set_word(index, force.value);
            }
        }
        // The counter counts scans, not loop ticks: a paused ladder must not climb.
        self.scan_count = self.scan_count.wrapping_add(1);
        let packed = if retain_wanted {
            Some(linc_plc_core::retain::pack(
                &retain_hash,
                vm.retain_layout_hash(),
                &vm.retain_pack(),
            ))
        } else {
            None
        };
        // The watch list is read from the VM after the scan, so a sample is the
        // state a reader saw at the end of that scan.
        if !self.watch.is_empty() {
            if let Ok(mut trend) = shared.trend.lock() {
                if trend.window_ms != self.trend_window {
                    // A new window restarts the chart: the samples on screen
                    // cover a different span, so mixing them would lie.
                    self.trend_window = trend.window_ms.max(1);
                    let window_us = u64::from(self.trend_window) * 1000;
                    let span_us = self.period_us.saturating_mul(512);
                    self.trend_stride = (window_us / span_us.max(1)).max(1) as u32;
                    self.trend_tick = 0;
                    trend.stride = self.trend_stride;
                    trend.samples.clear();
                }
                self.trend_tick = self.trend_tick.wrapping_add(1);
                if self.trend_tick.is_multiple_of(self.trend_stride.max(1)) {
                    let row = self
                        .watch
                        .iter()
                        .map(|name| super::trend_value(name, vm).unwrap_or(0))
                        .collect::<Vec<i32>>();
                    trend.samples.push(row);
                    if trend.samples.len() > 512 {
                        trend.samples.remove(0);
                    }
                }
            }
        }
        if let Some(blob) = packed {
            // A blob over the cap turns retention off with one error rather
            // than writing something the next start cannot read.
            if blob.len() > linc_plc_core::retain::MAX_BLOB {
                log::error!(
                    "linc-plc: the retain blob would be {} bytes, over the {} limit; retention is off",
                    blob.len(),
                    linc_plc_core::retain::MAX_BLOB
                );
                self.retain_path = None;
                self.retain_dirty = None;
            } else {
                self.retain_dirty = Some(blob);
            }
        }
        hal.write_outputs(vm.outputs())
            .map_err(|error| error.to_string())
    }

    /// Drop forces whose ttl ran out. [`linc_plc_core::Vm`] clears all forces
    /// at once, so the survivors are re-applied.
    fn expire_forces(&mut self) {
        let now = Instant::now();
        let before = self.forces.len();
        self.forces
            .retain(|force| force.expires.is_none_or(|deadline| deadline > now));
        let words_before = self.word_forces.len();
        self.word_forces
            .retain(|force| force.expires.is_none_or(|deadline| deadline > now));
        if self.forces.len() == before && self.word_forces.len() == words_before {
            return;
        }
        if let Some(vm) = self.vm.as_mut() {
            vm.clear_forces();
            for force in &self.forces {
                if let Err(message) = vm.set_force(&force.name, force.value) {
                    log::warn!(
                        "linc-plc: cannot re-apply the force {}: {message}",
                        force.name
                    );
                }
            }
        }
    }

    /// A HAL failure or an internal error: fault, drop the outputs, keep the
    /// socket serving so the UI can show what happened.
    fn fail(&mut self, message: String, hal: &Hal, shared: &Shared) {
        if self.phase != Phase::Fault {
            log::error!("linc-plc: fault: {message}");
        }
        self.phase = Phase::Fault;
        self.fault = Some(message);
        self.alive = false;
        let _ = hal.ctl().set_alive(false);
        let _ = hal.ctl().set_fault(true);
        let _ = hal.drive_outputs_false();
        self.publish(shared);
    }

    /// Point the trend ring at the loaded project's watch list and clear it, so
    /// a sample never mixes two programs.
    fn set_watch(&mut self, watch: Vec<String>, shared: &Shared) {
        self.watch = watch;
        if let Ok(mut trend) = shared.trend.lock() {
            trend.names = self.watch.clone();
            trend.window_ms = self.trend_window;
            trend.stride = self.trend_stride;
            trend.words = self
                .watch
                .iter()
                .map(|name| !super::is_bit_operand(name))
                .collect();
            trend.samples.clear();
        }
    }

    /// Put the saved values back after a load, gated on both hashes.
    fn restore_retained(&mut self) {
        let Some(path) = self.retain_path.clone() else {
            return;
        };
        let Some(vm) = self.vm.as_mut() else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                log::info!(
                    "linc-plc: no retain store at {}; starting with fresh values",
                    path.display()
                );
                return;
            }
            Err(error) => {
                log::error!(
                    "linc-plc: cannot read the retain store {}: {error}; continuing without it",
                    path.display()
                );
                return;
            }
        };
        match linc_plc_core::retain::unpack(&bytes, &self.hash, vm.retain_layout_hash()) {
            Ok(retained) => {
                vm.retain_unpack(&retained);
                log::info!(
                    "linc-plc: restored {} retained bit(s) and {} word(s) from {}",
                    retained.bits.len(),
                    retained.words.len(),
                    path.display()
                );
            }
            Err(refusal) => log::error!(
                "linc-plc: {}; continuing with fresh values",
                refusal.message()
            ),
        }
    }

    /// Publish the last packed blob: tmp file, fsync, rename, fsync the folder.
    fn flush_retained(&mut self, force: bool) {
        if !force && self.retain_last.elapsed() < self.retain_flush {
            return;
        }
        let Some(path) = self.retain_path.clone() else {
            return;
        };
        let Some(bytes) = self.retain_dirty.take() else {
            return;
        };
        if let Err(error) = publish_atomic(&path, &bytes) {
            log::error!(
                "linc-plc: cannot publish {}: {error}; values stay in memory",
                path.display()
            );
            self.retain_dirty = Some(bytes);
            return;
        }
        self.retain_last = Instant::now();
    }

    fn snapshot(&self) -> StateSnapshot {
        let (vars, words, mut forced) = match &self.vm {
            Some(vm) => (vm.snapshot(), vm.word_snapshot(), vm.forced()),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        forced.extend(self.word_forces.iter().map(|force| force.name.clone()));
        StateSnapshot {
            state: self.phase.run_state(),
            project: self.project.display().to_string(),
            hash: self.hash.clone(),
            scan_count: self.scan_count,
            scan_time_us: self.scan_time_us,
            overruns: self.overruns,
            timing: self.timing.snapshot(),
            // Health, not the pin's parity: the HAL heartbeat toggles once per
            // scan, so a snapshot sampled at a fixed cadence would otherwise
            // report whichever edge it happened to catch. A UI reads this field
            // as "the ladder is running and not faulted"; the pin remains the
            // edge signal for a watchdog.
            alive: self.phase == Phase::Running && self.fault.is_none(),
            fault: self.fault.clone(),
            vars,
            words,
            forced,
        }
    }

    fn publish(&self, shared: &Shared) {
        let snapshot = self.snapshot();
        match shared.snapshot.lock() {
            Ok(mut guard) => *guard = snapshot,
            Err(_) => log::warn!("linc-plc: the snapshot lock is poisoned"),
        }
    }
}

fn answer(reply: Sender<Response>, response: Response) {
    if reply.send(response).is_err() {
        log::warn!("linc-plc: the client went away before the reply");
    }
}

/// Write a file so a reader sees either the old bytes or the new ones.
fn publish_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "retain".to_string());
    let temporary = directory.join(format!(".{name}.tmp"));
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    if let Ok(folder) = std::fs::File::open(directory) {
        let _ = folder.sync_all();
    }
    Ok(())
}
