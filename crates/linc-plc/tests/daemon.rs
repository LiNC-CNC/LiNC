//! Integration tests: run the daemon in-process with `--no-hal` and drive it
//! with the blocking client, exactly as the UI does.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use linc_plc::Config;
use linc_plc_client::Client;
use linc_plc_core::protocol::{Request, Response, StateSnapshot, PROTOCOL_VERSION};
use linc_plc_core::{Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Project, Rung, Section};

/// A daemon running on a temp socket with a temp project file.
struct Harness {
    directory: tempfile::TempDir,
    socket: PathBuf,
    project: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), linc_plc::Error>>>,
}

impl Harness {
    fn start(project: &Project, allow_force: bool) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
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
        };
        let flag = Arc::clone(&shutdown);
        let thread = thread::spawn(move || linc_plc::run(config, flag));

        let mut harness = Self {
            directory,
            socket,
            project: project_path,
            shutdown,
            thread: Some(thread),
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
    });
    let mut section = Section::new("io map lock");
    section.rungs.push(Rung::new(
        Logic::Const { value: true },
        Coil {
            var: name,
            kind: CoilKind::Normal,
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
