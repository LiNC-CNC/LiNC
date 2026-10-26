//! The scan loop: command application, ladder execution, state machine and the
//! shared snapshot.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use linc_plc_core::protocol::{Response, RunState, StateSnapshot};
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

pub(crate) struct Daemon {
    project: PathBuf,
    phase: Phase,
    fault: Option<String>,
    vm: Option<Vm>,
    hash: String,
    forces: Vec<Force>,
    scan_count: u64,
    scan_time_us: u32,
    overruns: u64,
    alive: bool,
}

impl Daemon {
    pub(crate) fn new(project: PathBuf) -> Self {
        Self {
            project,
            phase: Phase::Idle,
            fault: None,
            vm: None,
            hash: String::new(),
            forces: Vec::new(),
            scan_count: 0,
            scan_time_us: 0,
            overruns: 0,
            alive: false,
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
        while !shared.shutdown.load(Ordering::Relaxed) {
            let started = Instant::now();
            while let Ok(command) = commands.try_recv() {
                self.apply(command, hal, shared);
            }

            if self.phase == Phase::Running {
                if let Err(message) = self.execute(hal) {
                    self.fail(message, hal, shared);
                }
            } else if let Err(error) = hal.drive_outputs_false() {
                self.fail(
                    format!("cannot drive the outputs false: {error}"),
                    hal,
                    shared,
                );
            }

            self.scan_count = self.scan_count.wrapping_add(1);
            self.scan_time_us = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);

            // The snapshot is a convenience for the UI, not the scan itself:
            // publishing every scan would contend on the mutex, and a load,
            // stop, force or fault publishes immediately anyway.
            if self.scan_count % SNAPSHOT_EVERY == 0 {
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

            let elapsed = started.elapsed();
            if elapsed >= period {
                self.overruns = self.overruns.wrapping_add(1);
            } else {
                std::thread::sleep(period - elapsed);
            }
        }
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
                self.hash = compiled.hash.clone();
                self.forces.clear();
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
                self.phase = Phase::Loaded;
                self.fault = None;
                self.alive = false;
                let _ = hal.ctl().set_alive(false);
                let _ = hal.ctl().set_fault(false);
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

            Command::ClearForces { reply } => {
                if let Some(vm) = self.vm.as_mut() {
                    vm.clear_forces();
                }
                self.forces.clear();
                self.publish(shared);
                answer(reply, Response::State(self.snapshot()));
            }
        }
    }

    /// One ladder scan: read the bound inputs, run the program, write the
    /// bound outputs.
    fn execute(&mut self, hal: &Hal) -> Result<(), String> {
        self.expire_forces();
        let Some(vm) = self.vm.as_mut() else {
            return Err("no program is loaded".to_string());
        };
        let inputs = hal.read_inputs().map_err(|error| error.to_string())?;
        vm.scan(&inputs);
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
        if self.forces.len() == before {
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

    fn snapshot(&self) -> StateSnapshot {
        let (vars, forced) = match &self.vm {
            Some(vm) => (vm.snapshot(), vm.forced()),
            None => (Vec::new(), Vec::new()),
        };
        StateSnapshot {
            state: self.phase.run_state(),
            project: self.project.display().to_string(),
            hash: self.hash.clone(),
            scan_count: self.scan_count,
            scan_time_us: self.scan_time_us,
            overruns: self.overruns,
            // Health, not the pin's parity: the HAL heartbeat toggles once per
            // scan, so a snapshot sampled at a fixed cadence would otherwise
            // report whichever edge it happened to catch. A UI reads this field
            // as "the ladder is running and not faulted"; the pin remains the
            // edge signal for a watchdog.
            alive: self.phase == Phase::Running && self.fault.is_none(),
            fault: self.fault.clone(),
            vars,
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
