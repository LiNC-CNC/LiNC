//! Optional Loobric tool-table sync.
//!
//! Loobric publishes no Rust binding. Its LinuxCNC client is a single
//! standard-library Python file that owns the three-way push/pull state, the
//! conflict rules, the timestamped table backup and the surgical line rewrite.
//! LiNC therefore runs that client as a subprocess, the same way the Baiermill
//! Probe Basic integration does, instead of reimplementing the Loobric schema
//! and its merge rules.
//!
//! The wrapper is deliberately outside both machine backends: it observes the
//! tool-table revision in each snapshot, runs the client in a worker thread (so
//! neither the UI nor the NML loop waits on the network), and sends the
//! internal [`MachineCommand::ReloadToolTable`] follow-up when a pull changed
//! the file. A reload is never attempted under a running interpreter.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::MachineBackend;
use crate::command::{MachineCommand, MachineError};
use crate::config::Profile;
use crate::model::{now_ms, InterpState, LoobricStatus, MachineSnapshot};
use crate::tooltable::revision_for;

/// How often the wrapper checks a running client, and how often the client
/// process itself is polled for exit.
const CLIENT_POLL: Duration = Duration::from_millis(25);

/// Resolved settings for one Loobric client run.
#[derive(Clone, Debug)]
pub struct LoobricSettings {
    pub client: String,
    pub interpreter: Option<String>,
    pub config: Option<PathBuf>,
    pub tool_table: PathBuf,
    pub timeout: Duration,
    pub sync_on_startup: bool,
    pub sync_after_tool_write: bool,
}

impl LoobricSettings {
    /// `None` when the profile leaves Loobric off, which is the default.
    pub fn from_profile(profile: &Profile) -> Option<Self> {
        let config = &profile.config.loobric;
        if !config.enabled {
            return None;
        }
        Some(Self {
            client: config.client.clone(),
            interpreter: config.interpreter.clone().filter(|value| !value.is_empty()),
            config: config.config.as_ref().map(PathBuf::from),
            tool_table: profile.machine_ini.tool_table.clone(),
            timeout: Duration::from_secs(config.timeout_secs),
            sync_on_startup: config.sync_on_startup,
            sync_after_tool_write: config.sync_after_tool_write,
        })
    }
}

/// A finished client run, before it is folded into the shared status.
struct ClientRun {
    ok: bool,
    message: String,
}

/// Runs the official client on a worker thread and publishes its last result.
struct LoobricRunner {
    status: Arc<Mutex<LoobricStatus>>,
    requests: Sender<()>,
}

impl LoobricRunner {
    fn new(settings: LoobricSettings) -> Self {
        let status = Arc::new(Mutex::new(LoobricStatus {
            enabled: true,
            message: "not synced yet".into(),
            ..LoobricStatus::default()
        }));
        let (requests, receiver) = mpsc::channel();
        let worker_status = Arc::clone(&status);
        thread::Builder::new()
            .name("linc-loobric".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    loop {
                        run_once(&settings, &worker_status);
                        // A request that arrived during the run may describe a
                        // table the run never saw (an operator edit landed
                        // after it read the file). Run once more; several
                        // queued requests collapse into that one pass.
                        let mut again = false;
                        while receiver.try_recv().is_ok() {
                            again = true;
                        }
                        if !again {
                            break;
                        }
                    }
                }
            })
            .expect("spawn Loobric worker");
        Self { status, requests }
    }

    fn request(&self) {
        let _ = self.requests.send(());
    }

    fn status(&self) -> LoobricStatus {
        self.status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

fn run_once(settings: &LoobricSettings, status: &Arc<Mutex<LoobricStatus>>) {
    let before = file_revision(&settings.tool_table);
    update(status, |status| {
        status.running = true;
        status.message = "syncing".into();
        status.last_attempt_ms = Some(now_ms());
        status.file_changed = false;
        status.file_revision = before;
    });

    let result = run_client(settings);
    let after = file_revision(&settings.tool_table);
    update(status, |status| {
        status.running = false;
        status.completed_runs = status.completed_runs.saturating_add(1);
        status.last_ok = Some(result.ok);
        status.message = result.message;
        status.file_changed = before != after;
        status.file_revision = after;
        if result.ok {
            status.last_success_ms = Some(now_ms());
        }
    });
}

fn update(status: &Arc<Mutex<LoobricStatus>>, change: impl FnOnce(&mut LoobricStatus)) {
    let mut status = status.lock().unwrap_or_else(|poison| poison.into_inner());
    change(&mut status);
}

/// Run the client once. `sync` is deliberate: it is the client's full cycle
/// (push local changes, pull bound server changes, report conflicts), and the
/// official client's state file keeps the three-way comparison honest.
fn run_client(settings: &LoobricSettings) -> ClientRun {
    let mut command = match settings.interpreter.as_deref() {
        Some(interpreter) => {
            let mut command = Command::new(interpreter);
            command.arg(&settings.client);
            command
        }
        None => Command::new(&settings.client),
    };
    if let Some(config) = &settings.config {
        // argparse's global option belongs before the subcommand.
        command.arg("--config").arg(config);
    }
    command.arg("sync");
    // The LinuxCNC INI is authoritative for the table path. Passing it here
    // also covers a client config that names a different INI or no table.
    command.env("TOOL_TABLE", &settings.tool_table);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ClientRun {
                ok: false,
                message: format!("cannot run {}: {error}", client_label(settings)),
            }
        }
    };
    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let outcome = wait_for_exit(&mut child, settings.timeout);
    let stdout = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();

    let lines: Vec<String> = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    for line in &lines {
        log::info!("loobric: {line}");
    }

    match outcome {
        Ok(Some(exit)) => {
            let ok = exit.success();
            let message = lines
                .last()
                .cloned()
                .unwrap_or_else(|| format!("client exited with {exit}"));
            ClientRun {
                ok,
                message: shorten(&message),
            }
        }
        Ok(None) => ClientRun {
            ok: false,
            message: format!("timed out after {}s", settings.timeout.as_secs().max(1)),
        },
        Err(error) => ClientRun {
            ok: false,
            message: format!("cannot wait for {}: {error}", client_label(settings)),
        },
    }
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    })
}

/// `Ok(Some(status))` when the client exited, `Ok(None)` on timeout.
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
        thread::sleep(CLIENT_POLL);
    }
}

fn client_label(settings: &LoobricSettings) -> String {
    match settings.interpreter.as_deref() {
        Some(interpreter) => format!("{interpreter} {}", settings.client),
        None => settings.client.clone(),
    }
}

fn file_revision(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| revision_for(&text))
}

fn shorten(message: &str) -> String {
    const MAX: usize = 240;
    if message.chars().count() <= MAX {
        return message.to_string();
    }
    let mut shortened: String = message.chars().take(MAX - 3).collect();
    shortened.push_str("...");
    shortened
}

fn idle(snapshot: &MachineSnapshot) -> bool {
    snapshot.task.interp == InterpState::Idle && !snapshot.task.running
}

/// Wrap the selected backend when `[loobric] enabled` is true. A disabled
/// profile returns the backend untouched, so no worker, status or UI exists.
pub fn wrap_backend(
    backend: Box<dyn MachineBackend>,
    profile: &Profile,
) -> Box<dyn MachineBackend> {
    let Some(settings) = LoobricSettings::from_profile(profile) else {
        return backend;
    };
    Box::new(LoobricBackend::new(backend, settings))
}

/// Backend wrapper that owns the Loobric client and its status.
pub struct LoobricBackend<B> {
    inner: B,
    runner: LoobricRunner,
    last_revision: Option<u64>,
    /// The file revision the controller is known to hold. Startup seeds it
    /// from the loaded table; a successful reload updates it. It deliberately
    /// does not follow every observed revision: the NML snapshot builder also
    /// re-reads the file, and that does not load the IO process's table.
    controller_revision: Option<u64>,
    last_completed_runs: u64,
    suppress_revision: Option<u64>,
    startup_pending: bool,
    sync_after_tool_write: bool,
    /// A tool-table write owed a sync while the interpreter was busy. The write
    /// itself already happened; this only holds the client run back so it does
    /// not rewrite the table under a running program.
    sync_pending: bool,
    pending_reload: bool,
}

impl<B: MachineBackend> LoobricBackend<B> {
    pub fn new(inner: B, settings: LoobricSettings) -> Self {
        let startup_pending = settings.sync_on_startup;
        let sync_after_tool_write = settings.sync_after_tool_write;
        Self {
            inner,
            runner: LoobricRunner::new(settings),
            last_revision: None,
            controller_revision: None,
            last_completed_runs: 0,
            suppress_revision: None,
            startup_pending,
            sync_after_tool_write,
            sync_pending: false,
            pending_reload: false,
        }
    }

    /// Fold a finished client run into the wrapper. A run whose file revision
    /// differs from the one the controller holds asks for a reload, even if
    /// the client later reported an error: the controller must be looking at
    /// whatever the file now holds.
    fn collect_result(&mut self) {
        let status = self.runner.status();
        if status.completed_runs <= self.last_completed_runs {
            return;
        }
        self.last_completed_runs = status.completed_runs;
        let needs_reload = match status.file_revision {
            Some(revision) => self.controller_revision != Some(revision),
            // The file could not be read after the run. If it also changed,
            // try the reload; the reload itself reports an unreadable table.
            None => status.file_changed,
        };
        if needs_reload {
            self.pending_reload = true;
            self.suppress_revision = status.file_revision;
            log::info!("loobric: client changed the tool table");
        }
        if status.last_ok == Some(false) {
            log::warn!("loobric: sync failed: {}", status.message);
        }
    }

    fn reload_if_idle(&mut self, snapshot: &mut MachineSnapshot) {
        if !self.pending_reload || !idle(snapshot) {
            return;
        }
        match self.inner.send(MachineCommand::ReloadToolTable) {
            Ok(()) => {
                self.pending_reload = false;
                // The reload changed the backend's own table; take a fresh
                // snapshot so the revision check below sees the new revision
                // and does not mistake it for an operator edit.
                *snapshot = self.inner.snapshot();
                self.controller_revision = Some(snapshot.tool_table.revision);
                log::info!("loobric: reloaded the tool table after a pull");
            }
            Err(MachineError::NotIdle) => {}
            Err(error) => {
                self.pending_reload = false;
                self.suppress_revision = None;
                log::warn!("loobric: cannot reload the tool table: {error}");
            }
        }
    }

    fn request(&self) {
        self.runner.request();
    }
}

impl<B: MachineBackend> MachineBackend for LoobricBackend<B> {
    fn snapshot(&mut self) -> MachineSnapshot {
        let mut snapshot = self.inner.snapshot();
        if self.controller_revision.is_none() {
            // The backend loaded the file at startup, so this is the revision
            // the controller already holds.
            self.controller_revision = Some(snapshot.tool_table.revision);
        }

        self.collect_result();
        self.reload_if_idle(&mut snapshot);

        if let Some(revision) = self.last_revision {
            if revision != snapshot.tool_table.revision {
                if self.suppress_revision == Some(snapshot.tool_table.revision) {
                    self.suppress_revision = None;
                } else {
                    self.suppress_revision = None;
                    if self.sync_after_tool_write {
                        // Owed, not started: the idle gate below decides when.
                        self.sync_pending = true;
                    }
                }
            }
        }
        self.last_revision = Some(snapshot.tool_table.revision);

        // A startup sync and a tool-table write both wait for an idle
        // interpreter. A client run rewrites `tool.tbl`, and a table that
        // changes under a running program changes the offsets the next tool
        // change reads. One run covers both, so two owed syncs collapse into a
        // single request instead of queueing two client runs back to back.
        if (self.startup_pending || self.sync_pending) && idle(&snapshot) {
            if self.sync_pending && !self.startup_pending {
                log::info!("loobric: tool table changed, syncing");
            }
            self.startup_pending = false;
            self.sync_pending = false;
            self.request();
        }

        let mut status = self.runner.status();
        status.reload_pending = self.pending_reload;
        status.sync_pending = self.sync_pending || self.startup_pending;
        snapshot.loobric = Some(status);
        snapshot
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        if matches!(command, MachineCommand::SyncToolTable) {
            // A sync may write the file, so it is an idle-only action. The
            // button and this check share the same rule; never trust the UI
            // alone, because a program can start between frame and command.
            let snapshot = self.inner.snapshot();
            if !idle(&snapshot) {
                return Err(MachineError::NotIdle);
            }
            self.request();
            return Ok(());
        }
        self.inner.send(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineVariant, TaskState};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn table(dir: &Path) -> PathBuf {
        let path = dir.join("tool.tbl");
        std::fs::write(&path, "; tool table\nT1 P0 Z0 D6 ; end mill\n").unwrap();
        path
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("loobric_linuxcnc.py");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn settings(client: PathBuf, table: PathBuf) -> LoobricSettings {
        LoobricSettings {
            client: client.display().to_string(),
            interpreter: None,
            config: None,
            tool_table: table,
            timeout: Duration::from_secs(5),
            sync_on_startup: false,
            sync_after_tool_write: false,
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition did not become true");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn runner_keeps_the_last_output_line_and_success() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let client = script(dir.path(), "echo pushed; echo 'In sync - nothing to do'");
        let runner = LoobricRunner::new(settings(client, table));
        runner.request();
        wait_for(|| runner.status().completed_runs == 1);
        let status = runner.status();
        assert_eq!(status.last_ok, Some(true));
        assert!(status.message.contains("nothing to do"), "{status:?}");
        assert!(!status.file_changed);
        assert!(status.last_success_ms.is_some());
    }

    #[test]
    fn runner_reports_a_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let client = script(dir.path(), "echo 'cannot read config' >&2; exit 2");
        let runner = LoobricRunner::new(settings(client, table));
        runner.request();
        wait_for(|| runner.status().completed_runs == 1);
        let status = runner.status();
        assert_eq!(status.last_ok, Some(false));
        assert!(status.message.contains("cannot read config"), "{status:?}");
    }

    #[test]
    fn runner_times_out_and_kills_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let client = script(dir.path(), "exec sleep 5");
        let mut settings = settings(client, table);
        settings.timeout = Duration::from_millis(150);
        let runner = LoobricRunner::new(settings);
        runner.request();
        wait_for(|| runner.status().completed_runs == 1);
        let status = runner.status();
        assert_eq!(status.last_ok, Some(false));
        assert!(status.message.contains("timed out"), "{status:?}");
    }

    #[test]
    fn runner_passes_the_config_path_and_linuxcnc_table() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let args = dir.path().join("args.txt");
        let client = script(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > {}\nprintf '%s\\n' \"$TOOL_TABLE\" >> {}\necho ok",
                args.display(),
                args.display()
            ),
        );
        let mut settings = settings(client, table.clone());
        let config = dir.path().join("linuxcnc.conf");
        std::fs::write(&config, "MACHINE_NAME=\"test\"\n").unwrap();
        settings.config = Some(config.clone());
        let runner = LoobricRunner::new(settings);
        runner.request();
        wait_for(|| runner.status().completed_runs == 1);
        let text = std::fs::read_to_string(&args).unwrap();
        assert!(text.contains("--config"), "{text}");
        assert!(text.contains(&config.display().to_string()), "{text}");
        assert!(text.contains("sync"), "{text}");
        assert!(text.contains(&table.display().to_string()), "{text}");
    }

    #[test]
    fn a_request_queued_during_a_run_triggers_one_more_run() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let counter = dir.path().join("runs.txt");
        let client = script(
            dir.path(),
            &format!("echo run >> {}; echo ok", counter.display()),
        );
        let runner = LoobricRunner::new(settings(client, table));
        runner.request();
        runner.request();
        wait_for(|| runner.status().completed_runs >= 2);
        let text = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    #[test]
    fn runner_marks_a_pull_that_changed_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let table = table(dir.path());
        let client = script(
            dir.path(),
            "printf 'T2 P0 Z1 D6 ; pulled\\n' >> \"$TOOL_TABLE\"; echo pulled",
        );
        let runner = LoobricRunner::new(settings(client, table));
        runner.request();
        wait_for(|| runner.status().completed_runs == 1);
        let status = runner.status();
        assert_eq!(status.last_ok, Some(true));
        assert!(status.file_changed, "{status:?}");
        assert!(status.file_revision.is_some(), "{status:?}");
    }

    struct FakeBackend {
        snapshot: MachineSnapshot,
        reloads: Arc<AtomicU64>,
        reload_revision: u64,
    }

    impl MachineBackend for FakeBackend {
        fn snapshot(&mut self) -> MachineSnapshot {
            self.snapshot.clone()
        }

        fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
            match command {
                MachineCommand::ReloadToolTable => {
                    self.reloads.fetch_add(1, Ordering::SeqCst);
                    self.snapshot.tool_table.revision = self.reload_revision;
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }

    fn fake(dir: &Path, body: &str) -> (FakeBackend, Arc<AtomicU64>, LoobricSettings) {
        let table = table(dir);
        let client = script(dir, body);
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.task = TaskState::default();
        snapshot.task.interp = InterpState::Idle;
        snapshot.tool_table.revision = revision_for("seed");
        let reloads = Arc::new(AtomicU64::new(0));
        let client_settings = settings(client, table.clone());
        let backend = FakeBackend {
            snapshot,
            reloads: Arc::clone(&reloads),
            reload_revision: revision_for("reloaded"),
        };
        (backend, reloads, client_settings)
    }

    #[test]
    fn startup_sync_reloads_only_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, reloads, mut settings) = fake(
            dir.path(),
            "sleep 0.3; printf 'T2 P0 Z1 D6\\n' >> \"$TOOL_TABLE\"; echo pulled",
        );
        settings.sync_on_startup = true;
        let mut backend = LoobricBackend::new(backend, settings);

        // The first snapshot starts the run while the interpreter is idle.
        // Then keep it busy so the collected pull has to wait for a reload.
        let _ = backend.snapshot();
        backend.inner.snapshot.task.running = true;
        wait_for(|| backend.runner.status().completed_runs == 1);
        let snapshot = backend.snapshot();
        assert_eq!(reloads.load(Ordering::SeqCst), 0);
        assert!(snapshot.loobric.as_ref().unwrap().reload_pending);

        backend.inner.snapshot.task.running = false;
        let snapshot = backend.snapshot();
        assert_eq!(reloads.load(Ordering::SeqCst), 1);
        assert!(!snapshot.loobric.as_ref().unwrap().reload_pending);
    }

    #[test]
    fn a_tool_table_revision_change_triggers_a_sync() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs.txt");
        let (backend, _reloads, mut settings) = fake(
            dir.path(),
            &format!("echo run >> {}; echo synced", counter.display()),
        );
        settings.sync_on_startup = false;
        settings.sync_after_tool_write = true;
        let mut backend = LoobricBackend::new(backend, settings);
        let _ = backend.snapshot();
        backend.inner.snapshot.tool_table.revision = revision_for("operator edit");
        let _ = backend.snapshot();
        wait_for(|| backend.runner.status().completed_runs == 1);
        let text = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn manual_sync_is_idle_only() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs.txt");
        let (backend, _reloads, settings) = fake(
            dir.path(),
            &format!("echo run >> {}; echo synced", counter.display()),
        );
        let mut backend = LoobricBackend::new(backend, settings);
        backend.inner.snapshot.task.running = true;
        let error = backend.send(MachineCommand::SyncToolTable).unwrap_err();
        assert_eq!(error, MachineError::NotIdle);

        backend.inner.snapshot.task.running = false;
        backend.send(MachineCommand::SyncToolTable).unwrap();
        wait_for(|| backend.runner.status().completed_runs == 1);
        let text = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn a_tool_write_while_running_waits_for_idle() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs.txt");
        let (backend, _reloads, mut settings) = fake(
            dir.path(),
            &format!("echo run >> {}; echo synced", counter.display()),
        );
        settings.sync_on_startup = false;
        settings.sync_after_tool_write = true;
        let mut backend = LoobricBackend::new(backend, settings);
        // Seed the observed revision; nothing is owed yet.
        let _ = backend.snapshot();
        assert!(!counter.exists(), "no client run before the table changes");

        // The write lands while a program runs. The client must not rewrite the
        // table under it, so the sync is owed, not started.
        backend.inner.snapshot.task.running = true;
        backend.inner.snapshot.tool_table.revision = revision_for("operator edit");
        let snapshot = backend.snapshot();
        assert!(
            snapshot.loobric.as_ref().unwrap().sync_pending,
            "the page must be able to say it is waiting for idle"
        );
        thread::sleep(Duration::from_millis(150));
        assert!(!counter.exists(), "the client ran under a running program");

        // The program ends: the owed sync runs once.
        backend.inner.snapshot.task.running = false;
        let snapshot = backend.snapshot();
        assert!(!snapshot.loobric.as_ref().unwrap().sync_pending);
        wait_for(|| backend.runner.status().completed_runs == 1);
        let text = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn a_tool_write_is_ignored_when_the_sync_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs.txt");
        let (backend, _reloads, mut settings) = fake(
            dir.path(),
            &format!("echo run >> {}; echo synced", counter.display()),
        );
        settings.sync_on_startup = false;
        settings.sync_after_tool_write = false;
        let mut backend = LoobricBackend::new(backend, settings);
        let _ = backend.snapshot();
        backend.inner.snapshot.tool_table.revision = revision_for("operator edit");
        let snapshot = backend.snapshot();
        assert!(!snapshot.loobric.as_ref().unwrap().sync_pending);
        thread::sleep(Duration::from_millis(150));
        assert!(!counter.exists(), "no sync was asked for");
        assert_eq!(backend.runner.status().completed_runs, 0);
    }

    #[test]
    fn a_startup_sync_and_a_tool_write_collapse_into_one_run() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs.txt");
        let (backend, _reloads, mut settings) = fake(
            dir.path(),
            &format!("sleep 0.2; echo run >> {}; echo synced", counter.display()),
        );
        settings.sync_on_startup = true;
        settings.sync_after_tool_write = true;
        let mut backend = LoobricBackend::new(backend, settings);
        // The startup sync is owed, and a write lands before the machine is
        // idle: both are still owed when idle arrives.
        backend.inner.snapshot.task.running = true;
        backend.inner.snapshot.tool_table.revision = revision_for("operator edit");
        let snapshot = backend.snapshot();
        assert!(snapshot.loobric.as_ref().unwrap().sync_pending);

        backend.inner.snapshot.task.running = false;
        let _ = backend.snapshot();
        wait_for(|| backend.runner.status().completed_runs == 1);
        // A second request would have queued a second client run; one run
        // covers both debts.
        thread::sleep(Duration::from_millis(300));
        let text = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
    }
}
