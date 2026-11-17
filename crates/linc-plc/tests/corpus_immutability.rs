//! Running a program must not rewrite its file.
//!
//! The fixtures under `examples/plc-acceptance/projects` are evidence: a run that
//! normalises them (a tree rung rewritten as grid cells) destroys the thing the
//! acceptance tests compare against. `Load` used to persist the project it had
//! just compiled, so every capture rewrote the file it opened.
//!
//! Each case copies a corpus project to a temp directory and drives the daemon
//! exactly as the UI does — `Hello`, `Load`, `Start`, a snapshot, `Shutdown` — and
//! then compares the bytes. The copies are used rather than the fixtures so a
//! regression fails the test instead of damaging the corpus; the fixtures' own
//! hashes are checked too, so a run that reached them would be caught.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use linc_plc::{Config, TestDevice};
use linc_plc_client::Client;
use linc_plc_core::protocol::{Request, Response};

/// Every project the acceptance corpus ships.
const CORPUS: &[&str] = &[
    "garage-door",
    "motor-star-delta",
    "motor-starters-function-block",
    "recipes-data-type-ld",
    "silo-function-block",
    "silo",
    "start-stop-delay",
    "start-stop",
    "timer-counter",
    "traffic-lights-routines",
    "traffic-lights",
    "two-tanks-cascade",
];

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plc-acceptance/projects")
        .canonicalize()
        .expect("the corpus is in the repository")
}

/// A daemon on a temp socket whose project is the file at `project`.
struct Daemon {
    directory: tempfile::TempDir,
    socket: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), linc_plc::Error>>>,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let socket = directory.path().join("plc.sock");
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = Config {
            project: project.to_path_buf(),
            socket: socket.clone(),
            no_hal: true,
            scan_us: 1000,
            allow_force: false,
            retain_file: None,
            retain_flush_s: 1,
        };
        let flag = Arc::clone(&shutdown);
        let field = TestDevice::new();
        let thread = thread::spawn(move || linc_plc::run_with_device(config, flag, field));
        let daemon = Self {
            directory,
            socket,
            shutdown,
            thread: Some(thread),
        };
        daemon.wait_for_socket();
        daemon
    }

    fn wait_for_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.socket.exists() {
            assert!(
                Instant::now() < deadline,
                "the daemon never bound its socket"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = &self.directory;
    }
}

/// Load and run one project the way the UI does, then hand back the file's bytes
/// and whether the daemon accepted the program.
///
/// A refusal is not a failure of this test: it writes nothing either way. What the
/// test is about is that an *accepted* load leaves the file alone.
fn load_and_run(project: &Path) -> (bool, Vec<u8>) {
    let daemon = Daemon::start(project);
    let mut client = Client::connect(&daemon.socket).expect("connect");
    let hello = client.hello().expect("hello");
    assert!(
        matches!(hello, Response::Hello { .. }),
        "unexpected hello: {hello:?}"
    );

    let text = fs::read_to_string(project).expect("the project is readable");
    let mut loaded: linc_plc_core::Project =
        serde_json::from_str(&text).expect("the project parses");
    // Exactly what the editor does on open: lay every tree rung out as cells
    // before sending it. This is the shape whose write-back changed the file on
    // disk, so the test must send it, not the bytes as they are on disk.
    loaded.normalise();
    let response = client
        .call(&Request::Load { project: loaded })
        .expect("load");
    let accepted = matches!(response, Response::Loaded { .. });
    if !accepted {
        eprintln!("{}: load refused: {response:?}", project.display());
    } else {
        let response = client.call(&Request::Start).expect("start");
        assert!(
            matches!(response, Response::State(_)),
            "start was refused: {response:?}"
        );
    }
    // A scan, so anything the loop would normalise on the way has happened.
    thread::sleep(Duration::from_millis(50));
    let response = client.call(&Request::Snapshot).expect("snapshot");
    assert!(
        matches!(response, Response::State(_)),
        "snapshot was refused: {response:?}"
    );

    (
        accepted,
        fs::read(project).expect("the project is still readable"),
    )
}

/// A load, a start and a scan leave every corpus project byte for byte as it was.
#[test]
fn running_a_corpus_project_does_not_rewrite_it() {
    let corpus = corpus_dir();
    let mut accepted = 0;
    for name in CORPUS {
        let source = corpus.join(format!("{name}.json"));
        let before = fs::read(&source).unwrap_or_else(|error| panic!("{name}: {error}"));

        let directory = tempfile::tempdir().expect("temp dir");
        let copy = directory.path().join(format!("{name}.json"));
        fs::write(&copy, &before).expect("write the copy");

        let (loaded, after) = load_and_run(&copy);
        accepted += usize::from(loaded);
        assert_eq!(
            after, before,
            "{name}.json was rewritten by loading and running it"
        );
        assert_eq!(
            fs::read(&source).expect("read the fixture"),
            before,
            "{name}.json in the corpus changed"
        );
    }
    assert!(
        accepted > 0,
        "no project loaded, so the write path was never exercised"
    );
}

/// The fixtures themselves, untouched: the hash of every corpus file is what the
/// acceptance tests compare against, so this is the same guard seen from outside.
#[test]
fn the_corpus_files_are_stable_across_a_run() {
    let corpus = corpus_dir();
    let hashes = |dir: &Path| -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = fs::read_dir(dir)
            .expect("the corpus reads")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .map(|entry| {
                let bytes = fs::read(entry.path()).expect("read a corpus file");
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fnv(&bytes),
                )
            })
            .collect();
        out.sort();
        out
    };

    let before = hashes(&corpus);
    assert_eq!(before.len(), CORPUS.len(), "every project is listed");
    for name in CORPUS {
        let copy_dir = tempfile::tempdir().expect("temp dir");
        let copy = copy_dir.path().join(format!("{name}.json"));
        fs::copy(corpus.join(format!("{name}.json")), &copy).expect("copy");
        load_and_run(&copy);
    }
    assert_eq!(hashes(&corpus), before, "a run changed the corpus");
}

/// FNV-1a, so the test needs no hashing crate for a "did the bytes change" check.
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
