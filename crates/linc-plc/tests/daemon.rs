//! Integration tests: run the daemon in-process on a [`TestDevice`] and drive it
//! with the blocking client, exactly as the UI does.
//!
//! M5's acceptance lives here: the whole daemon path — socket, state machine,
//! scan loop, publishing — runs on a scripted device, so no HAL component is
//! created and `hal_init` is never called. The field itself is `HalDevice`; the
//! test only needs what the seam exposes.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use linc_plc::{Config, TestDevice};
use linc_plc_client::Client;
use linc_plc_core::protocol::{Request, Response, StateSnapshot, PROTOCOL_VERSION};
use linc_plc_core::{
    Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Project, Rung, Section, VarDecl,
};

/// A daemon running on a temp socket with a temp project file.
struct Harness {
    directory: tempfile::TempDir,
    socket: PathBuf,
    project: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), linc_plc::Error>>>,
    /// The scripted field: inputs the tests set, outputs the ladder drives.
    device: TestDevice,
}

impl Harness {
    fn start(project: &Project, allow_force: bool) -> Self {
        Self::start_with(project, allow_force, None)
    }

    fn start_with(project: &Project, allow_force: bool, retain: Option<PathBuf>) -> Self {
        Self::start_in(
            project,
            allow_force,
            retain,
            tempfile::tempdir().expect("temp dir"),
        )
    }

    fn start_in(
        project: &Project,
        allow_force: bool,
        retain: Option<PathBuf>,
        directory: tempfile::TempDir,
    ) -> Self {
        let project_path = directory.path().join("ladder.json");
        let json = serde_json::to_vec_pretty(project).expect("serialize the project");
        fs::write(&project_path, json).expect("write the project");

        let socket = directory.path().join("plc.sock");
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = Config {
            project: project_path.clone(),
            socket: socket.clone(),
            no_hal: true,
            scan_us: 1000,
            allow_force,
            retain_file: retain,
            retain_flush_s: 1,
        };
        let flag = Arc::clone(&shutdown);
        let device = TestDevice::new();
        let field = device.clone();
        let thread = thread::spawn(move || linc_plc::run_with_device(config, flag, field));

        let mut harness = Self {
            directory,
            socket,
            project: project_path,
            shutdown,
            thread: Some(thread),
            device,
        };
        harness.wait_for_socket();
        harness
    }

    fn wait_for_socket(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.socket.exists() {
            if let Some(thread) = self.thread.as_ref() {
                if thread.is_finished() {
                    let result = self.thread.take().expect("thread").join();
                    panic!("the daemon exited before binding its socket: {result:?}");
                }
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never bound {}",
                self.socket.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket).expect("connect to the daemon")
    }

    /// Wait for the daemon thread and require a clean exit.
    fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => panic!("the daemon returned an error: {error}"),
                Err(_) => panic!("the daemon thread panicked"),
            }
        }
    }

    fn project_path(&self) -> &Path {
        &self.project
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Keep the temp directory alive until the daemon has stopped.
        let _ = &self.directory;
    }
}

fn describe(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| "unserializable response".to_string())
}

fn state_name(snapshot: &StateSnapshot) -> String {
    match serde_json::to_value(&snapshot.state) {
        Ok(serde_json::Value::String(name)) => name,
        other => panic!("the state did not serialize as a string: {other:?}"),
    }
}

fn expect_state(response: Response) -> StateSnapshot {
    match response {
        Response::State(snapshot) => snapshot,
        other => panic!("expected a state, got {}", describe(&other)),
    }
}

fn expect_error(response: Response) -> (String, String) {
    match response {
        Response::Error { code, message } => (code, message),
        other => panic!("expected an error, got {}", describe(&other)),
    }
}

fn output_names(project: &Project) -> Vec<String> {
    project
        .io
        .iter()
        .filter(|point| matches!(point.direction, IoDirection::Out))
        .map(|point| point.name.clone())
        .collect()
}

fn var_value(snapshot: &StateSnapshot, name: &str) -> (bool, bool) {
    let var = snapshot
        .vars
        .iter()
        .find(|var| var.name == name)
        .unwrap_or_else(|| panic!("the snapshot has no variable named {name}"));
    (var.value, var.forced)
}

/// Poll until a variable holds the expected (value, forced) pair. A force is
/// recorded immediately but reaches the scan image on the next scan.
fn wait_for_var(client: &mut Client, name: &str, expected: (bool, bool)) -> StateSnapshot {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if var_value(&snapshot, name) == expected {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the variable {name} never became {expected:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Poll until the published heartbeat reads true while running.
fn wait_for_alive(client: &mut Client) -> StateSnapshot {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if snapshot.alive {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the heartbeat never read true while the ladder ran"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_scans(client: &mut Client) -> StateSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if snapshot.scan_count > 0 {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the scan counter never advanced past zero"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn hello_validate_load_start_stop_shutdown() {
    let mut harness = Harness::start(&Project::example(), false);
    let mut client = harness.client();

    // Idle before a load, and no heartbeat yet.
    let idle = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
    assert_eq!(state_name(&idle), "idle");
    assert!(!idle.alive, "the heartbeat is held false before a start");
    assert!(idle.hash.is_empty(), "no project is loaded yet");

    match client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello")
    {
        Response::Hello {
            protocol,
            schema,
            daemon,
            hal,
        } => {
            assert_eq!(protocol, PROTOCOL_VERSION);
            assert!(schema >= 1, "schema {schema} must be positive");
            assert!(!daemon.is_empty(), "the daemon version must be reported");
            assert!(!hal, "--no-hal must report hal=false");
        }
        other => panic!("expected hello, got {}", describe(&other)),
    }

    match client
        .call(&Request::Validate {
            project: Project::example(),
        })
        .expect("validate")
    {
        Response::Validated { ok, diagnostics } => {
            assert!(
                ok,
                "the example project must validate: {}",
                serde_json::to_string(&diagnostics).unwrap_or_default()
            );
        }
        other => panic!("expected validated, got {}", describe(&other)),
    }

    let hash = match client
        .call(&Request::Load {
            project: Project::example(),
        })
        .expect("load")
    {
        Response::Loaded { hash, diagnostics } => {
            assert!(!hash.is_empty(), "a load must report the project hash");
            assert!(
                diagnostics.iter().all(|diagnostic| !matches!(
                    diagnostic.severity,
                    linc_plc_core::Severity::Error
                )),
                "a successful load carries no error diagnostics"
            );
            hash
        }
        other => panic!("expected loaded, got {}", describe(&other)),
    };

    let started = expect_state(client.call(&Request::Start).expect("start"));
    assert_eq!(state_name(&started), "running");

    let snapshot = wait_for_scans(&mut client);
    assert_eq!(state_name(&snapshot), "running");
    assert_eq!(snapshot.hash, hash);
    // The published heartbeat alternates while the ladder runs: a watchdog
    // consuming the snapshot (or the pin) must see movement, not a level.
    let beat = wait_for_alive(&mut client);
    assert_eq!(state_name(&beat), "running");
    assert_eq!(
        snapshot.project,
        harness.project_path().display().to_string(),
        "the snapshot reports the daemon's project path"
    );

    let stopped = expect_state(client.call(&Request::Stop).expect("stop"));
    assert_eq!(state_name(&stopped), "loaded");
    assert!(
        !stopped.alive,
        "the heartbeat stops with the ladder, so a watchdog sees it"
    );
    assert!(stopped.forced.is_empty(), "stop clears forces");

    match client.call(&Request::Shutdown).expect("shutdown") {
        Response::Bye => {}
        other => panic!("expected bye, got {}", describe(&other)),
    }
    harness.finish();
    assert!(
        !harness.socket.exists(),
        "run() removes the socket file on exit"
    );
}

#[test]
fn load_rejects_a_changed_io_map() {
    let mut harness = Harness::start(&Project::example(), false);
    let mut client = harness.client();

    let mut changed = Project::example();
    let name = "io-lock-probe".to_string();
    changed.io.push(IoPoint {
        name: name.clone(),
        direction: IoDirection::Out,
        kind: IoKind::Bit,
        comment: String::new(),
    });
    let mut section = Section::new("io map lock");
    section.rungs.push(Rung::new(
        Logic::Const { value: true },
        Coil {
            var: name,
            kind: CoilKind::Normal,
            expr: None,
            args: Vec::new(),
        },
    ));
    changed.sections.push(section);

    let (code, message) = expect_error(
        client
            .call(&Request::Load { project: changed })
            .expect("load"),
    );
    assert_eq!(code, "io_map_locked");
    assert!(
        message.to_lowercase().contains("restart"),
        "the message must tell the user to restart: {message}"
    );

    let _ = client.call(&Request::Shutdown);
    harness.finish();
}

#[test]
fn force_is_rejected_without_allow_force() {
    let mut harness = Harness::start(&Project::example(), false);
    let mut client = harness.client();

    let (code, _message) = expect_error(
        client
            .call(&Request::Force {
                name: "anything".to_string(),
                value: true,
                ttl_ms: 0,
            })
            .expect("force"),
    );
    assert_eq!(code, "force_disabled");

    let _ = client.call(&Request::Shutdown);
    harness.finish();
}

#[test]
fn force_is_accepted_until_stop() {
    let project = Project::example();
    let outputs = output_names(&project);
    assert!(!outputs.is_empty(), "the example project has an output");
    let target = outputs[0].clone();

    let mut harness = Harness::start(&project, true);
    let mut client = harness.client();

    match client
        .call(&Request::Load {
            project: Project::example(),
        })
        .expect("load")
    {
        Response::Loaded { .. } => {}
        other => panic!("expected loaded, got {}", describe(&other)),
    }
    let started = expect_state(client.call(&Request::Start).expect("start"));
    assert_eq!(state_name(&started), "running");
    let snapshot = wait_for_scans(&mut client);

    let forced = expect_state(
        client
            .call(&Request::Force {
                name: target.clone(),
                value: true,
                ttl_ms: 0,
            })
            .expect("force"),
    );
    assert!(
        forced.forced.iter().any(|name| *name == target),
        "the snapshot lists the forced symbol"
    );
    let applied = wait_for_var(&mut client, &target, (true, true));
    assert!(
        applied.forced.iter().any(|name| *name == target),
        "the force is still listed after it reached the scan image"
    );

    // A ttl expires on its own inside the scan loop.
    let _ = expect_state(
        client
            .call(&Request::Force {
                name: target.clone(),
                value: true,
                ttl_ms: 50,
            })
            .expect("force with ttl"),
    );
    thread::sleep(Duration::from_millis(250));
    let expired = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
    assert!(
        !expired.forced.iter().any(|name| *name == target),
        "a force with a ttl is dropped after it elapses"
    );

    // Force again, then Stop: outputs false, forces gone.
    let _ = expect_state(
        client
            .call(&Request::Force {
                name: target.clone(),
                value: true,
                ttl_ms: 0,
            })
            .expect("force"),
    );
    let _ = wait_for_var(&mut client, &target, (true, true));

    let stopped = expect_state(client.call(&Request::Stop).expect("stop"));
    assert_eq!(state_name(&stopped), "loaded");
    assert!(stopped.forced.is_empty(), "stop clears every force");
    for name in &outputs {
        assert_eq!(
            var_value(&stopped, name).0,
            false,
            "stop drives output {name} false"
        );
    }
    assert!(
        snapshot.scan_count > 0,
        "the daemon was scanning before the stop"
    );

    let _ = client.call(&Request::Shutdown);
    harness.finish();
}

#[test]
fn socket_is_private_and_removed_on_shutdown() {
    let mut harness = Harness::start(&Project::example(), false);
    let mut client = harness.client();
    let _ = client.hello().expect("hello");

    let mode = fs::metadata(&harness.socket)
        .expect("the socket exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the socket is created 0600");

    match client.call(&Request::Shutdown).expect("shutdown") {
        Response::Bye => {}
        other => panic!("expected bye, got {}", describe(&other)),
    }
    drop(client);
    harness.finish();
    assert!(
        !harness.socket.exists(),
        "the socket is removed on shutdown"
    );
}

#[test]
fn the_trend_ring_samples_the_watch_list_every_scan() {
    let mut project = Project::example();
    project.watch = vec!["run".to_string(), "MW0".to_string()];
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();

    match client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load")
    {
        Response::Loaded { .. } => {}
        other => panic!("expected loaded, got {}", describe(&other)),
    }
    client.call(&Request::Start).expect("start");

    let mut data = None;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(25));
        if let Response::Trend(trend) = client
            .call(&Request::Trend { window_ms: 20_000 })
            .expect("trend")
        {
            if trend.samples.len() >= 4 {
                data = Some(trend);
                break;
            }
        }
    }
    let trend = data.expect("the daemon must fill the trend ring once it scans");
    assert_eq!(trend.names, vec!["run".to_string(), "MW0".to_string()]);
    assert_eq!(trend.words, vec![false, true], "a word is marked as such");
    assert!(
        trend.samples.iter().all(|row| row.len() == 2),
        "every sample carries one value per watch entry"
    );
}

#[test]
fn a_new_trend_window_restarts_history_and_stretches_the_stride() {
    let mut project = Project::example();
    project.watch = vec!["run".to_string()];
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();
    match client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load")
    {
        Response::Loaded { .. } => {}
        other => panic!("expected loaded, got {}", describe(&other)),
    }
    client.call(&Request::Start).expect("start");

    // Fill the default window first.
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(20));
        if let Response::Trend(data) = client
            .call(&Request::Trend { window_ms: 20_000 })
            .expect("trend")
        {
            if data.samples.len() >= 4 {
                break;
            }
        }
    }

    // Asking for a longer window drops what was there and samples sparsely.
    let reset = match client
        .call(&Request::Trend { window_ms: 100_000 })
        .expect("trend")
    {
        Response::Trend(data) => data,
        other => panic!("expected trend, got {}", describe(&other)),
    };
    assert!(
        reset.samples.len() <= 1,
        "the history restarts on a window change, got {} samples",
        reset.samples.len()
    );

    let mut stretched = reset;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(20));
        if let Response::Trend(data) = client
            .call(&Request::Trend { window_ms: 100_000 })
            .expect("trend")
        {
            stretched = data;
            if stretched.stride > 1 && !stretched.samples.is_empty() {
                break;
            }
        }
    }
    assert!(stretched.stride > 1, "a long window samples sparsely");
    assert_eq!(stretched.window_ms, 100_000);
}

#[test]
fn retained_values_survive_a_daemon_restart() {
    // A retained variable that a set coil drives on the first scan.
    let mut project = Project::new("retain-restart");
    let mut var = VarDecl::new("kept");
    var.retain = true;
    project.vars = vec![var];
    project.sections = vec![Section {
        name: "main".into(),
        rungs: vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("kept", CoilKind::Set),
        )],
    }];

    let directory = tempfile::tempdir().expect("temp dir");
    let retain_path = directory.path().join("retain.bin");

    // First run: let the scan set the coil, then stop so the store is written.
    {
        let mut harness = Harness::start_in(
            &project,
            false,
            Some(retain_path.clone()),
            tempfile::tempdir().expect("first temp dir"),
        );
        let mut client = harness.client();
        match client
            .call(&Request::Load {
                project: project.clone(),
            })
            .expect("load")
        {
            Response::Loaded { .. } => {}
            other => panic!("expected loaded, got {}", describe(&other)),
        }
        client.call(&Request::Start).expect("start");
        let mut seen = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(25));
            if let Response::State(snapshot) = client.call(&Request::Snapshot).expect("snapshot") {
                if var_value(&snapshot, "kept").0 {
                    seen = true;
                    break;
                }
            }
        }
        assert!(seen, "the set coil must drive the retained variable");
        client.call(&Request::Stop).expect("stop");
        client.call(&Request::Shutdown).expect("shutdown");
        drop(harness);
    }
    assert!(retain_path.exists(), "Stop must publish the store");

    // Second run: the value comes back on Load, before any scan.
    {
        let mut harness = Harness::start_in(
            &project,
            false,
            Some(retain_path.clone()),
            tempfile::tempdir().expect("second temp dir"),
        );
        let mut client = harness.client();
        match client
            .call(&Request::Load {
                project: project.clone(),
            })
            .expect("load")
        {
            Response::Loaded { .. } => {}
            other => panic!("expected loaded, got {}", describe(&other)),
        }
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        assert_eq!(
            var_value(&snapshot, "kept").0,
            true,
            "the retained value came back without a scan"
        );
        assert!(!snapshot.forced.iter().any(|name| name == "kept"));
        client.call(&Request::Shutdown).expect("shutdown");
    }
}

/// One Step is exactly one scan, and it pauses a running ladder first so the
/// scheduler cannot also scan on that tick.
#[test]
fn step_is_exactly_one_scan_and_pauses() {
    let mut harness = Harness::start(&Project::example(), false);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: Project::example(),
        })
        .expect("load");
    let running = expect_state(client.call(&Request::Start).expect("start"));
    assert_eq!(state_name(&running), "running");

    // The first step pauses a running ladder, so the exactness is measured from
    // the paused state onwards, where the scheduler cannot add scans.
    let paused = expect_state(client.call(&Request::Step).expect("step"));
    assert_eq!(
        state_name(&paused),
        "loaded",
        "a step leaves the ladder paused"
    );
    assert!(!paused.alive, "a paused ladder has no heartbeat");

    let first = expect_state(client.call(&Request::Step).expect("step"));
    assert_eq!(
        first.scan_count,
        paused.scan_count + 1,
        "one press is one scan, no more"
    );
    let second = expect_state(client.call(&Request::Step).expect("step"));
    assert_eq!(
        second.scan_count,
        first.scan_count + 1,
        "and the next is too"
    );

    // The pause holds: scans stop climbing on their own.
    thread::sleep(Duration::from_millis(120));
    let after = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
    assert_eq!(
        after.scan_count, second.scan_count,
        "the scheduler stayed idle"
    );
    assert_eq!(state_name(&after), "loaded");
}

/// Reset clears the VM's data and the forces and leaves a defined paused state.
#[test]
fn reset_clears_data_and_forces() {
    let mut harness = Harness::start(&Project::example(), true);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: Project::example(),
        })
        .expect("load");
    client.call(&Request::Start).expect("start");
    wait_for_scans(&mut client);
    client
        .call(&Request::Force {
            name: "start".to_string(),
            value: true,
            ttl_ms: 0,
        })
        .expect("force");

    let reset = expect_state(client.call(&Request::ResetData).expect("reset"));
    assert_eq!(
        state_name(&reset),
        "loaded",
        "reset leaves the ladder paused"
    );
    assert!(reset.forced.is_empty(), "the forces are cleared");
    assert!(
        reset.vars.iter().all(|var| !var.value),
        "every value is back to false after a reset"
    );
    assert!(!reset.alive, "a reset ladder does not keep a heartbeat");
}

/// A forced marker word holds even when the program writes the slot every
/// scan, is listed as forced, and returns to the program's value on release.
/// Block members refuse: they own no bank slot.
#[test]
fn a_forced_word_holds_against_the_program_and_releases() {
    let mut project = Project::new("word-force");
    project.io = vec![IoPoint::new("out", IoDirection::Out)];
    project.sections = vec![Section {
        name: "main".to_string(),
        rungs: vec![Rung {
            label: String::new(),
            comment: String::new(),
            logic: Logic::Const { value: true },
            coil: Some(Coil::new("out", CoilKind::Normal)),
            cells: vec![linc_plc_core::Cell::Compute {
                op: "MOVE".to_string(),
                target: "MW2".to_string(),
                left: "7".to_string(),
                right: "0".to_string(),
            }],
        }],
    }];
    let mut harness = Harness::start(&project, true);
    let mut client = harness.client();
    match client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load")
    {
        Response::Loaded { .. } => {}
        other => panic!("expected loaded, got {}", describe(&other)),
    }
    expect_state(client.call(&Request::Start).expect("start"));
    let running = wait_for_scans(&mut client);
    let word = |snapshot: &StateSnapshot, name: &str| {
        snapshot
            .words
            .iter()
            .find(|word| word.name == name)
            .map(|word| word.value)
    };
    assert_eq!(
        word(&running, "MW2"),
        Some(7),
        "the program writes the slot"
    );

    let forced = expect_state(
        client
            .call(&Request::ForceWord {
                name: "MW2".to_string(),
                value: 123,
                ttl_ms: 0,
            })
            .expect("force word"),
    );
    assert_eq!(word(&forced, "MW2"), Some(123));
    assert!(
        forced.forced.iter().any(|name| name == "MW2"),
        "the snapshot lists the forced word"
    );

    // The program keeps writing 7 in every scan; the force must still win.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        assert_eq!(
            word(&snapshot, "MW2"),
            Some(123),
            "the forced word holds against the program"
        );
        if snapshot.scan_count > forced.scan_count + 3 {
            break;
        }
        assert!(Instant::now() < deadline, "the scan never advanced");
        thread::sleep(Duration::from_millis(20));
    }

    let released = expect_state(
        client
            .call(&Request::UnforceWord {
                name: "MW2".to_string(),
            })
            .expect("unforce word"),
    );
    assert!(
        !released.forced.iter().any(|name| name == "MW2"),
        "the word is no longer listed as forced"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if word(&snapshot, "MW2") == Some(7) {
            break;
        }
        assert!(Instant::now() < deadline, "the program never wrote again");
        thread::sleep(Duration::from_millis(20));
    }

    let (code, message) = expect_error(
        client
            .call(&Request::ForceWord {
                name: "TON0.ET".to_string(),
                value: 5,
                ttl_ms: 0,
            })
            .expect("force block value"),
    );
    assert_eq!(code, "invalid_force");
    assert!(message.contains("block value"), "{message:?}");

    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

// --- item 6: reload without dropping outputs --------------------------------

/// A one-rung ladder whose output is high on every scan, so a reload that
/// zeroed the field would be visible.
fn always_on(name: &str) -> Project {
    let mut project = Project::new(name);
    project.io = vec![IoPoint::new("run", IoDirection::Out)];
    project.sections = vec![Section {
        name: "main".to_string(),
        rungs: vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("run", CoilKind::Normal),
        )],
    }];
    project
}

fn word_value(snapshot: &StateSnapshot, name: &str) -> i32 {
    snapshot
        .words
        .iter()
        .find(|word| word.name == name)
        .unwrap_or_else(|| panic!("the snapshot has no word named {name}"))
        .value
}

/// The value of one retained word in a store blob, so a test can wait for the
/// flush that carries it.
fn retained_word(bytes: &[u8], slot: u16) -> Option<i32> {
    if bytes.len() < 4 + 2 + 8 + 1 + 2 + 2 + 4 || bytes[0..4] != *b"LPRT" {
        return None;
    }
    let mut cursor = 14usize;
    let hash_len = *bytes.get(cursor)? as usize;
    cursor += 1 + hash_len;
    let bits = u16::from_le_bytes([*bytes.get(cursor)?, *bytes.get(cursor + 1)?]) as usize;
    cursor += 2 + bits * 3;
    let words = u16::from_le_bytes([*bytes.get(cursor)?, *bytes.get(cursor + 1)?]) as usize;
    cursor += 2;
    for _ in 0..words {
        let at = u16::from_le_bytes([*bytes.get(cursor)?, *bytes.get(cursor + 1)?]);
        let value = i32::from_le_bytes(bytes.get(cursor + 2..cursor + 6)?.try_into().ok()?);
        cursor += 6;
        if at == slot {
            return Some(value);
        }
    }
    None
}

/// A reload while running keeps the phase and the field: no stop, no
/// outputs-false, no start. The snapshot's output image carries the values the
/// pins still hold until the first scan writes the new program's own.
#[test]
fn a_reload_while_running_keeps_the_outputs_and_the_phase() {
    let project = always_on("reload");
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load");
    let running = expect_state(client.call(&Request::Start).expect("start"));
    assert_eq!(state_name(&running), "running");
    let running = wait_for_var(&mut client, "run", (true, false));
    assert_eq!(running.output_image, vec![true]);

    let reply = client
        .call(&Request::Reload {
            project: project.clone(),
        })
        .expect("reload");
    assert!(
        matches!(reply, Response::Loaded { .. }),
        "{}",
        describe(&reply)
    );

    // It keeps scanning: the phase never leaves Running and the output image
    // never reads false.
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut samples = 0;
    while Instant::now() < deadline {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        assert_eq!(
            state_name(&snapshot),
            "running",
            "the reload did not stop the ladder"
        );
        assert_eq!(
            snapshot.output_image,
            vec![true],
            "the output never dropped"
        );
        samples += 1;
        thread::sleep(Duration::from_millis(5));
    }
    assert!(samples > 10, "the poll ran");
    // The reload's own snapshot precedes the new program's first scan, so the
    // bit image is fresh (false) while the pins hold (true). One scan later the
    // program has written the output again.
    let after = wait_for_var(&mut client, "run", (true, false));
    assert_eq!(after.output_image, vec![true]);
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// A reload while loaded leaves the ladder loaded: it does not start it.
#[test]
fn a_reload_while_loaded_stays_loaded() {
    let project = always_on("reload-loaded");
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load");
    let loaded = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
    assert_eq!(state_name(&loaded), "loaded");

    let reply = client.call(&Request::Reload { project }).expect("reload");
    assert!(
        matches!(reply, Response::Loaded { .. }),
        "{}",
        describe(&reply)
    );
    let after = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
    assert_eq!(
        state_name(&after),
        "loaded",
        "a reload does not start the ladder"
    );
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// The pin map is frozen, so a reload may not change the I/O bindings, even
/// though it may change everything else.
#[test]
fn a_reload_with_different_bindings_is_refused() {
    let project = always_on("reload-map");
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load");
    client.call(&Request::Start).expect("start");

    let mut other = project;
    other.io.push(IoPoint::new("lamp", IoDirection::Out));
    let (code, message) = expect_error(
        client
            .call(&Request::Reload { project: other })
            .expect("reload"),
    );
    assert_eq!(code, "io_map_locked", "{message}");
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// A force follows its name, not its slot: one on a surviving symbol stays, one
/// on a symbol the new program does not declare is dropped from both the force
/// list and the scan image.
#[test]
fn a_force_follows_its_name_across_a_reload() {
    let mut before = always_on("reload-forces");
    before.vars = vec![VarDecl::new("seal"), VarDecl::new("spare")];
    let mut harness = Harness::start(&before, true);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: before.clone(),
        })
        .expect("load");
    client.call(&Request::Start).expect("start");
    for name in ["seal", "spare"] {
        client
            .call(&Request::Force {
                name: name.to_string(),
                value: true,
                ttl_ms: 0,
            })
            .expect("force");
    }
    wait_for_var(&mut client, "seal", (true, true));
    wait_for_var(&mut client, "spare", (true, true));

    let mut after = always_on("reload-forces");
    after.vars = vec![VarDecl::new("seal")];
    let reply = client
        .call(&Request::Reload { project: after })
        .expect("reload");
    assert!(
        matches!(reply, Response::Loaded { .. }),
        "{}",
        describe(&reply)
    );

    let snapshot = wait_for_var(&mut client, "seal", (true, true));
    assert!(
        !snapshot.forced.iter().any(|name| name == "spare"),
        "a dropped force must not be listed: {:?}",
        snapshot.forced
    );
    assert!(
        snapshot.vars.iter().all(|var| var.name != "spare"),
        "and its symbol is gone from the image"
    );
    let (code, _) = expect_error(
        client
            .call(&Request::Force {
                name: "spare".to_string(),
                value: true,
                ttl_ms: 0,
            })
            .expect("force"),
    );
    assert_eq!(code, "invalid_force", "the VM no longer knows the name");
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// Reload is refused before a program is loaded. `Fault` shares the arm; no HAL
/// error can produce a fault under `--no-hal`, so this is the reachable test of
/// the branch.
#[test]
fn a_reload_before_a_load_answers_state() {
    let project = always_on("reload-idle");
    let mut harness = Harness::start(&project, false);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    let (code, message) = expect_error(client.call(&Request::Reload { project }).expect("reload"));
    assert_eq!(code, "state", "{message}");
    assert!(message.contains("Load"), "{message}");
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// A retained value comes back through the store the normal load path reads.
/// The program never writes MW2, so after the reload it can only be the store.
#[test]
fn a_retained_value_survives_a_reload() {
    let project = always_on("reload-retain");
    let directory = tempfile::tempdir().expect("temp dir");
    let retain = directory.path().join("retain.bin");
    let mut harness = Harness::start_in(&project, true, Some(retain.clone()), directory);
    let mut client = harness.client();
    client
        .call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
        .expect("hello");
    client
        .call(&Request::Load {
            project: project.clone(),
        })
        .expect("load");
    client.call(&Request::Start).expect("start");
    client
        .call(&Request::ForceWord {
            name: "MW2".to_string(),
            value: 7,
            ttl_ms: 0,
        })
        .expect("force the marker word");

    // Wait for the store itself to carry the value: the daemon publishes it
    // once a second.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(bytes) = fs::read(&retain) {
            if retained_word(&bytes, 2) == Some(7) {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the retain store never carried MW2 = 7"
        );
        thread::sleep(Duration::from_millis(20));
    }

    // Release the force after the store has been written, then reload: the new
    // VM starts empty, so 7 can only come from the restore.
    client
        .call(&Request::UnforceWord {
            name: "MW2".to_string(),
        })
        .expect("unforce");
    let reply = client
        .call(&Request::Reload {
            project: project.clone(),
        })
        .expect("reload");
    assert!(
        matches!(reply, Response::Loaded { .. }),
        "{}",
        describe(&reply)
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if word_value(&snapshot, "MW2") == 7 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "MW2 = {} after the reload, not the store's 7",
            word_value(&snapshot, "MW2")
        );
        thread::sleep(Duration::from_millis(20));
    }
    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}

/// One input bit straight to one output bit: `in -> lamp`.
fn pass_through_project() -> Project {
    let mut project = Project::new("test-device");
    project.io = vec![
        IoPoint::new("in", IoDirection::In),
        IoPoint::new("lamp", IoDirection::Out),
    ];
    project.pous = vec![linc_plc_core::Pou {
        sections: vec![Section {
            name: "main".into(),
            rungs: vec![Rung::new(
                Logic::Contact {
                    var: "in".to_string(),
                    negated: false,
                },
                Coil::new("lamp", CoilKind::Normal),
            )],
        }],
        ..linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Ld)
    }];
    project
}

/// Wait until the device has seen a scan whose output equals `wanted`.
fn wait_for_output(harness: &Harness, client: &mut Client, wanted: bool) -> Duration {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = expect_state(client.call(&Request::Snapshot).expect("snapshot"));
        if harness.device.outputs() == vec![wanted] {
            return harness.device.clock();
        }
        assert!(
            Instant::now() < deadline,
            "the device never saw lamp = {wanted}; outputs are {:?} after {} writes",
            harness.device.outputs(),
            harness.device.writes()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// M5's acceptance: the daemon runs on a scripted device with no HAL, and the
/// route from a field input to a driven output is exercised end to end.
#[test]
fn the_daemon_runs_on_a_test_device_with_no_hal() {
    let mut harness = Harness::start(&pass_through_project(), false);
    let mut client = harness.client();

    // Load, then start: the ladder needs a program before it scans.
    let reply = client
        .call(&Request::Load {
            project: pass_through_project(),
        })
        .expect("load");
    assert!(
        matches!(reply, Response::Loaded { .. }),
        "{}",
        describe(&reply)
    );
    let reply = client.call(&Request::Start {}).expect("start");
    assert!(matches!(reply, Response::State(_)), "{}", describe(&reply));

    // The device starts with nothing scripted: every input reads false.
    wait_for_output(&harness, &mut client, false);
    assert!(
        harness.device.reads() > 0 && harness.device.writes() > 0,
        "the daemon pulled the image {} time(s) and wrote it {} time(s)",
        harness.device.reads(),
        harness.device.writes()
    );

    // The field presents a true input, and the ladder drives the output.
    harness.device.script_bits(&[true]);
    wait_for_output(&harness, &mut client, true);

    // Drop it again: the output follows on a later scan.
    harness.device.script_bits(&[false]);
    wait_for_output(&harness, &mut client, false);

    // The daemon stepped the device's clock, so timers saw real deltas.
    let steps = harness.device.steps();
    assert!(
        steps.len() > 1 && steps.iter().all(|dt| *dt >= Duration::from_millis(1)),
        "the device was stepped {steps:?}"
    );

    // Stop drives every output safe, on the device, with no HAL to ask.
    let before = harness.device.safes();
    client.call(&Request::Stop {}).expect("stop");
    let deadline = Instant::now() + Duration::from_secs(2);
    while harness.device.safes() == before {
        assert!(
            Instant::now() < deadline,
            "stop never drove the outputs safe"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(harness.device.outputs(), vec![false]);
    assert!(
        !harness.device.status().alive,
        "the heartbeat is held false on Stop"
    );

    client.call(&Request::Shutdown).expect("shutdown");
    harness.finish();
}
