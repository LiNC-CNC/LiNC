//! The LiNC ladder PLC service.
//!
//! A userspace PLC: it loads one ladder project JSON, compiles it with
//! `linc-plc-core`, owns its own `linc_plc.*` HAL component and scans the
//! compiled program in the daemon's main thread. A Unix socket serves the UI
//! (validate, download, start/stop, monitor, force), so the machine's ladder
//! logic lives outside the HMI and survives an HMI restart.
//!
//! # Threading
//!
//! The main thread owns the HAL component, every pin and the scan loop; no
//! other thread touches HAL. One listener thread accepts connections and
//! handles them sequentially — one UI client is enough — and forwards state
//! changes to the scan loop over an `mpsc` channel with a reply channel. The
//! daemon compiles and validates projects on the socket thread, because that
//! work needs no HAL, and only hands the compiled program to the loop.
//!
//! # State machine
//!
//! `Idle` before a project is loaded, `Loaded` after a successful load,
//! `Running` after Start, `Fault` after a HAL failure or an internal error.
//! Start is only accepted from `Loaded`; Stop returns to `Loaded` and drives
//! every ladder output false. A fault keeps the socket alive so the UI can
//! show it, and Load or Stop clears it once the underlying error is gone.
//!
//! # The HAL pin map is fixed for the process lifetime
//!
//! Pins are created from the project named on the command line, before
//! `hal_ready()`. A Load whose compiled input and output binding names differ
//! from that map is refused with the error code `io_map_locked`; restart the
//! daemon to bind a different map. A missing or unparsable startup project
//! leaves the map empty, which the same rule reports on the first Load.
//!
//! # Failure behaviour
//!
//! The `linc_plc.alive` heartbeat means "the ladder is executing": it toggles
//! only while the state is `Running` and is held false in `Idle`, `Loaded` and
//! `Fault`, on Stop and on the way out. A watchdog must therefore require both
//! `linc_plc.alive` edges and `NOT linc_plc.fault`. The snapshot's `alive`
//! field is the matching health flag (`Running` and not faulted), not the
//! pin's parity, so a UI sampling the snapshot at its own rate never reads a
//! stale edge.
//!
//! A HAL read or write error sets `Fault`, drives every output false, raises
//! `linc_plc.fault` and leaves the socket serving. On any exit from
//! [`run`] — Stop, Shutdown, a signal, or an error — the outputs are driven
//! false, the fault pin is raised and the component is unregistered through
//! [`linc_hal::HalComponent`]'s `Drop` (`hal_exit`).
//!
//! Panics in the scan loop are deliberately not caught: an unwind runs the
//! drop glue and unregisters the component, and a release build aborts the
//! process without running it. Either way HAL keeps the last output values and
//! the component disappears, so the machine must watch the `linc_plc.alive`
//! heartbeat with an external watchdog that drops the outputs when the
//! heartbeat stops. This daemon is not a safety function.

mod daemon;
mod hal;
mod server;

use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use linc_plc_core::protocol::{RunState, StateSnapshot};
use linc_plc_core::{Compiled, Diagnostic, Project, Severity};
use thiserror::Error;

/// Everything the daemon needs to start.
#[derive(Debug, Clone)]
pub struct Config {
    /// Ladder project JSON. Read at startup to create the HAL pin map and
    /// rewritten in place by every successful Load.
    pub project: PathBuf,
    /// Unix socket to serve. Created with mode 0600 and removed on exit.
    pub socket: PathBuf,
    /// Skip the HAL component and its pins; everything else behaves the same.
    pub no_hal: bool,
    /// Target scan period in microseconds.
    pub scan_us: u64,
    /// Accept Force requests. Commissioning and tests only.
    pub allow_force: bool,
}

/// A fatal startup or runtime error. Per-request failures travel back to the
/// UI as [`linc_plc_core::protocol::Response::Error`] instead.
#[derive(Debug, Error)]
pub enum Error {
    #[error("HAL: {0}")]
    Hal(#[from] linc_hal::HalError),
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("socket protocol: {0}")]
    Protocol(#[from] linc_plc_core::ProtocolError),
    #[error("project: {0}")]
    Project(String),
    #[error("socket path {0} exists and is not a socket")]
    SocketPath(String),
    #[error("HAL pin {0} has no data slot")]
    Unlinked(String),
    #[error("output layout mismatch: {expected} HAL pin(s), {got} ladder value(s)")]
    Layout { expected: usize, got: usize },
}

/// A state change handed from the socket thread to the scan loop. Each command
/// carries its own reply channel so one connection cannot wait on another.
pub(crate) enum Command {
    Load {
        compiled: Box<Compiled>,
        warnings: Vec<Diagnostic>,
        reply: mpsc::Sender<linc_plc_core::protocol::Response>,
    },
    Start {
        reply: mpsc::Sender<linc_plc_core::protocol::Response>,
    },
    Stop {
        reply: mpsc::Sender<linc_plc_core::protocol::Response>,
    },
    Force {
        name: String,
        value: bool,
        ttl_ms: u32,
        reply: mpsc::Sender<linc_plc_core::protocol::Response>,
    },
    ClearForces {
        reply: mpsc::Sender<linc_plc_core::protocol::Response>,
    },
}

/// The compiled input and output binding names the pins were created for.
/// Frozen at startup for the process lifetime.
#[derive(Clone, Default)]
pub(crate) struct IoMap {
    pub(crate) inputs: Vec<String>,
    pub(crate) outputs: Vec<String>,
}

impl IoMap {
    fn from_compiled(compiled: &Compiled) -> Self {
        Self {
            inputs: compiled
                .inputs
                .iter()
                .map(|binding| binding.name.clone())
                .collect(),
            outputs: compiled
                .outputs
                .iter()
                .map(|binding| binding.name.clone())
                .collect(),
        }
    }

    /// True when the candidate program drives exactly the bound map, in order.
    fn accepts(&self, compiled: &Compiled) -> bool {
        let candidate = Self::from_compiled(compiled);
        candidate.inputs == self.inputs && candidate.outputs == self.outputs
    }
}

/// State shared by the socket thread and the scan loop.
pub(crate) struct Shared {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) snapshot: Arc<Mutex<StateSnapshot>>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) io_map: IoMap,
    pub(crate) project: PathBuf,
    pub(crate) no_hal: bool,
    pub(crate) allow_force: bool,
}

/// Run the daemon until `shutdown` is set. See the crate documentation for the
/// threading model, the state machine and the failure behaviour.
pub fn run(config: Config, shutdown: Arc<AtomicBool>) -> Result<(), Error> {
    log::info!(
        "linc-plc: starting, project {}, socket {}",
        config.project.display(),
        config.socket.display()
    );

    let startup = startup_compiled(&config.project);
    let io_map = match &startup {
        Some(compiled) => IoMap::from_compiled(compiled),
        None => {
            log::warn!(
                "linc-plc: no usable ladder project at startup; no HAL I/O pins are bound, and a \
                 Load with I/O bindings will be refused until the daemon is restarted with one"
            );
            IoMap::default()
        }
    };

    let listener = server::bind(&config.socket)?;
    let socket_file = SocketFile {
        path: config.socket.clone(),
    };
    let machine = hal::Hal::new(&config, &io_map)?;

    let (commands, receiver) = mpsc::channel();
    let shared = Arc::new(Shared {
        commands,
        snapshot: Arc::new(Mutex::new(StateSnapshot {
            state: RunState::Idle,
            project: config.project.display().to_string(),
            hash: String::new(),
            scan_count: 0,
            scan_time_us: 0,
            overruns: 0,
            alive: false,
            fault: None,
            vars: Vec::new(),
            forced: Vec::new(),
        })),
        shutdown: Arc::clone(&shutdown),
        io_map,
        project: config.project.clone(),
        no_hal: config.no_hal,
        allow_force: config.allow_force,
    });

    let server_shared = Arc::clone(&shared);
    let server = thread::Builder::new()
        .name("linc-plc-socket".to_string())
        .spawn(move || server::serve(listener, server_shared))?;

    let mut daemon = daemon::Daemon::new(config.project.clone());
    daemon.run(
        &machine,
        &shared,
        &receiver,
        Duration::from_micros(config.scan_us.max(1)),
    );

    // Leave the plant safe before the listener goes away, then stop the socket
    // thread and unregister the component.
    machine.force_safe();
    daemon.mark_stopped(&shared);
    shutdown.store(true, Ordering::SeqCst);
    if let Err(error) = server.join() {
        log::warn!("linc-plc: socket thread ended abnormally: {error:?}");
    }
    drop(machine);
    drop(socket_file);
    log::info!("linc-plc: stopped");
    Ok(())
}

/// Compile the project named on the command line. Its only job is to define
/// the pin map, so a failure is a warning, not a startup error.
fn startup_compiled(path: &Path) -> Option<Compiled> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("linc-plc: cannot read {}: {error}", path.display());
            return None;
        }
    };
    let project: Project = match serde_json::from_str(&text) {
        Ok(project) => project,
        Err(error) => {
            log::warn!(
                "linc-plc: {} is not a ladder project: {error}",
                path.display()
            );
            return None;
        }
    };
    match linc_plc_core::compile(&project) {
        Ok(compiled) => Some(compiled),
        Err(diagnostics) => {
            log::warn!(
                "linc-plc: {} does not compile: {}",
                path.display(),
                summarize(&diagnostics)
            );
            None
        }
    }
}

/// Remove the socket file on the way out, but only while it is still a socket.
struct SocketFile {
    path: PathBuf,
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                if let Err(error) = fs::remove_file(&self.path) {
                    log::warn!("linc-plc: cannot remove {}: {error}", self.path.display());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("linc-plc: cannot inspect {}: {error}", self.path.display()),
        }
    }
}

/// Validate and compile one project. Returns whether it is free of errors, the
/// merged diagnostics, and the compiled program when compilation succeeded.
pub(crate) fn evaluate(project: &Project) -> (bool, Vec<Diagnostic>, Option<Compiled>) {
    let mut diagnostics = linc_plc_core::validate(project);
    match linc_plc_core::compile(project) {
        Ok(compiled) => {
            for warning in &compiled.warnings {
                push_unique(&mut diagnostics, warning);
            }
            let ok = !diagnostics
                .iter()
                .any(|diagnostic| matches!(diagnostic.severity, Severity::Error));
            (ok, diagnostics, Some(compiled))
        }
        Err(errors) => {
            for error in &errors {
                push_unique(&mut diagnostics, error);
            }
            (false, diagnostics, None)
        }
    }
}

fn push_unique(diagnostics: &mut Vec<Diagnostic>, diagnostic: &Diagnostic) {
    let duplicate = diagnostics.iter().any(|existing| {
        existing.code == diagnostic.code
            && existing.path == diagnostic.path
            && existing.message == diagnostic.message
    });
    if !duplicate {
        diagnostics.push(diagnostic.clone());
    }
}

/// One line naming the first error, for a [`linc_plc_core::protocol::Response::Error`].
pub(crate) fn summarize(diagnostics: &[Diagnostic]) -> String {
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.severity, Severity::Error))
        .count();
    match diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.severity, Severity::Error))
    {
        Some(first) if errors > 1 => format!("{} ({} errors)", first.message, errors),
        Some(first) => first.message.clone(),
        None => "the project did not compile".to_string(),
    }
}
