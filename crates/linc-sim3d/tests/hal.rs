//! Acceptance tests for the linc-sim3d HAL layer.
//!
//! Two kinds of test live here: behaviour of the position sources and the
//! signal seam, and the wiring contract between the Rust code and the HAL
//! files, the INI and the wrapper script. The wiring tests read the repository
//! files so a renamed pin or a second probe reader breaks a test instead of
//! failing silently on a machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use linc_sim3d::engine::{FilePositionSource, MockPositionSource, PositionSource, Positions};
use linc_sim3d::hal::{
    full_pin_names, HalOutputPins, HalSignal, PinDir, PinKind, COMPONENT_NAME, PINS, PIN_COUNT,
};
use linc_sim3d::sink::{MockSignal, Sim3dSignal};
use linc_sim3d::types::P3;

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn read_repo_file(relative: &str) -> String {
    let path = repo_file(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// A path inside the repository, relative to its root, for test messages.
fn display_path(path: &Path) -> String {
    let root = fs::canonicalize(repo_file("")).unwrap_or_else(|_| repo_file(""));
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match canonical.strip_prefix(&root) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => canonical.display().to_string(),
    }
}

// ------------------------------------------------------- profile HAL chains
//
// A HAL pin takes exactly one signal, but which files are loaded depends on
// the profile: configs/sim.ini loads the simulator chain, and
// configs/baiermill-pb.ini loads the BaierMill chain. A reader in one profile
// is not a reader in the other, so the rule is checked per profile: following
// only the files an INI lists, no pin may be read by two signals.

/// One profile's HAL chain: the HALFILE and POSTGUI_HALFILE entries of its INI,
/// in load order.
struct HalChain {
    /// The INI the chain came from, for messages.
    ini: String,
    /// The listed files that exist, as (repository-relative path, contents).
    files: Vec<(String, String)>,
    /// Listed but absent. Only a `../../` sibling checkout may be missing.
    missing: Vec<String>,
}

impl HalChain {
    /// Read a profile INI and the HAL files it lists.
    fn load(ini: &str) -> Self {
        let ini_path = repo_file(ini);
        let text = fs::read_to_string(&ini_path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", ini_path.display()));
        let base = ini_path
            .parent()
            .expect("the INI has a directory")
            .to_path_buf();

        let mut files = Vec::new();
        let mut missing = Vec::new();
        let mut in_hal = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_hal = line.eq_ignore_ascii_case("[HAL]");
                continue;
            }
            if !in_hal {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !(key.eq_ignore_ascii_case("HALFILE") || key.eq_ignore_ascii_case("POSTGUI_HALFILE"))
            {
                continue;
            }
            let listed = value.trim();
            let path = base.join(listed);
            match fs::read_to_string(&path) {
                Ok(contents) => files.push((display_path(&path), contents)),
                Err(_) => missing.push(listed.to_string()),
            }
        }
        for absent in &missing {
            assert!(
                absent.starts_with("../../"),
                "{ini} lists {absent}, which is neither in the repository nor a sibling checkout"
            );
        }
        assert!(!files.is_empty(), "{ini} lists no HAL file that exists");
        Self {
            ini: ini.to_string(),
            files,
            missing,
        }
    }

    /// A synthetic chain, for the negative control.
    fn from_files(files: Vec<(String, String)>) -> Self {
        Self {
            ini: "synthetic".to_string(),
            files,
            missing: Vec::new(),
        }
    }

    fn assert_loads(&self, file: &str) {
        assert!(
            self.files.iter().any(|(name, _)| name == file),
            "{} must load {file}, loads {:?}",
            self.ini,
            self.names()
        );
    }

    fn assert_does_not_load(&self, file: &str) {
        assert!(
            !self.files.iter().any(|(name, _)| name == file),
            "{} must not load {file}",
            self.ini
        );
    }

    fn names(&self) -> Vec<&str> {
        self.files.iter().map(|(name, _)| name.as_str()).collect()
    }
}

/// The reader pins a HAL `net` line drives: the pins after a `=>` marker, so a
/// multi-arrow line reports all of them. The signal name is the first argument
/// and may itself contain dots, so it is skipped rather than parsed.
fn reader_pins(line: &str) -> Vec<String> {
    let body = line.split('#').next().unwrap_or("").trim();
    let mut tokens = body.split_whitespace();
    if tokens.next() != Some("net") {
        return Vec::new();
    }
    let _signal = tokens.next();
    let mut readers = Vec::new();
    let mut sinking = false;
    for token in tokens {
        match token {
            "<=" => sinking = false,
            "=>" => sinking = true,
            pin if sinking => readers.push(pin.to_string()),
            _ => {}
        }
    }
    readers
}

/// Every line of the chain that makes `pin` a reader, as "file:line: text".
fn reader_lines(chain: &HalChain, pin: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (file, text) in &chain.files {
        for (number, line) in text.lines().enumerate() {
            if reader_pins(line).iter().any(|candidate| candidate == pin) {
                hits.push(format!("{file}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    hits
}

/// Pins this chain reads from more than one signal. A HAL pin takes exactly one
/// signal, so inside one profile this must always be empty.
fn pins_read_twice(chain: &HalChain) -> Vec<String> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut doubled = Vec::new();
    for (file, text) in &chain.files {
        for (number, line) in text.lines().enumerate() {
            let location = format!("{file}:{}", number + 1);
            for pin in reader_pins(line) {
                match seen.iter().find(|(name, _)| name == &pin) {
                    Some((_, first)) => {
                        doubled.push(format!("{pin} is read by {first} and by {location}"))
                    }
                    None => seen.push((pin, location.clone())),
                }
            }
        }
    }
    doubled
}

/// The output pins a test can watch, without a running HAL.
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

#[test]
fn file_source_yields_samples_in_order_and_stops_at_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("positions.txt");
    fs::write(
        &path,
        "# probe approach, one sample per line\n-5 0 0 3 -1.0\n0 0 0 3 -1.0\n5 0 0 3 -1.0\n",
    )
    .unwrap();

    let mut source = FilePositionSource::from_path(&path).unwrap();
    let expected = [
        Positions {
            x: -5.0,
            y: 0.0,
            z: 0.0,
            tool_offset_z: -1.0,
            tool_number: 3,
        },
        Positions {
            x: 0.0,
            ..expected_first()
        },
        Positions {
            x: 5.0,
            ..expected_first()
        },
    ];
    for sample in expected {
        assert_eq!(source.sample(), Some(sample));
    }
    assert_eq!(source.sample(), None);
    assert_eq!(source.sample(), None, "EOF stays EOF");
}

fn expected_first() -> Positions {
    Positions {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        tool_offset_z: -1.0,
        tool_number: 3,
    }
}

#[test]
fn file_source_is_send_and_boxed_as_the_engine_wants_it() {
    let source: Box<dyn PositionSource> =
        Box::new(FilePositionSource::from_reader(&b"1 2 3\n"[..]));
    let mut source = source;
    assert_eq!(source.sample(), Some(Positions::new(1.0, 2.0, 3.0)));
    assert_eq!(source.sample(), None);
}

#[test]
fn mock_source_is_send_and_last_write_wins_across_threads() {
    let handle = MockPositionSource::new();
    let mut engine_side = handle.clone();
    let source_for_engine: Box<dyn PositionSource> = Box::new(handle.clone());
    drop(source_for_engine);

    let writer = std::thread::spawn(move || {
        for step in 0..500 {
            handle.set(Positions::new(f64::from(step) * 0.01, 0.0, 0.0));
        }
    });
    writer.join().unwrap();

    let last = engine_side.sample().expect("a mock source never ends");
    assert!((last.x - 4.99).abs() < 1e-9, "last write wins: {last:?}");
    assert_eq!(engine_side.sample(), Some(last));
}

#[test]
fn probe_round_trips_and_a_fault_is_recorded_once() {
    let mock = MockSignal::new();
    let signal: &dyn Sim3dSignal = &mock;

    signal.probe(true);
    assert!(mock.probe());
    signal.probe(false);
    assert!(!mock.probe());

    signal.fault("probe mesh missing");
    assert_eq!(mock.take_faults(), vec!["probe mesh missing".to_string()]);
    assert!(mock.take_faults().is_empty());
}

#[test]
fn hal_signal_writes_pins_and_latches_a_fault() {
    let pins = Arc::new(RecordingPins::default());
    let signal = HalSignal::new(Arc::clone(&pins));
    let seam: &dyn Sim3dSignal = &signal;

    seam.probe(true);
    assert!(pins.probe.load(Ordering::Relaxed));
    seam.tip([1.0, 2.0, 3.0]);
    assert_eq!(*pins.tip.lock().unwrap(), [1.0, 2.0, 3.0]);
    seam.probe(false);
    assert!(!pins.probe.load(Ordering::Relaxed));

    seam.fault("bvH lost a triangle");
    assert!(signal.faulted());
    assert!(pins.collision.load(Ordering::Relaxed));
    // The engine's next tick must not hide the modelling error.
    seam.collision(false);
    assert!(pins.collision.load(Ordering::Relaxed));
}

#[test]
fn run_hal_without_a_hal_names_the_missing_library() {
    use std::io::Write;

    use linc_sim3d::hal::{run_hal, HalBridge};

    // A component name of this test's own keeps it clear of the other tests
    // and of any running session: two components cannot share a name.
    let component = format!("linc-sim3d-runhal-{}", std::process::id());
    if HalBridge::open_named(&component).is_ok() {
        // A working HAL would start the sampling loop, which never returns;
        // the unavailable path is the one this test can assert.
        eprintln!(
            "skipping: this environment initialises the component, so run_hal would \
             sample forever instead of returning"
        );
        return;
    }

    let ini = repo_file("configs/sim.ini");
    let dir = tempfile::tempdir().expect("temporary directory");
    let config = dir.path().join("sim3d.toml");
    let mut file = fs::File::create(&config).expect("temporary config");
    writeln!(file, "[machine]").unwrap();
    writeln!(file, "ini = \"{}\"", ini.display()).unwrap();
    writeln!(file, "[hal]").unwrap();
    writeln!(file, "component = \"{component}\"").unwrap();
    drop(file);

    let error = run_hal(&config).expect_err("HAL is unavailable in this environment");
    let text = format!("{error:#}");
    assert!(
        text.contains("liblinuxcnchal"),
        "a missing HAL must name the library: {text}"
    );
}

#[test]
fn the_published_pin_table_is_the_contract() {
    let expected: [(&str, PinDir, PinKind); PIN_COUNT] = [
        ("pos-x", PinDir::In, PinKind::Float),
        ("pos-y", PinDir::In, PinKind::Float),
        ("pos-z", PinDir::In, PinKind::Float),
        ("tool-offset", PinDir::In, PinKind::Float),
        ("tool-number", PinDir::In, PinKind::S32),
        ("probe-out", PinDir::Out, PinKind::Bit),
        ("collision", PinDir::Out, PinKind::Bit),
        ("tip-x", PinDir::Out, PinKind::Float),
        ("tip-y", PinDir::Out, PinKind::Float),
        ("tip-z", PinDir::Out, PinKind::Float),
    ];
    assert_eq!(COMPONENT_NAME, "linc-sim3d");
    assert_eq!(PINS.len(), expected.len());
    for (spec, (name, dir, kind)) in PINS.iter().zip(expected) {
        assert_eq!(spec.name, name, "pin order and names");
        assert_eq!(spec.dir, dir, "{name} direction");
        assert_eq!(spec.kind, kind, "{name} type");
        assert_eq!(spec.full_name(), format!("linc-sim3d.{name}"));
    }

    let names = full_pin_names();
    assert_eq!(
        names,
        expected.map(|(name, _, _)| format!("linc-sim3d.{name}"))
    );

    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "pin names must be unique");
}

#[test]
fn the_hal_file_nets_every_input_pin_and_uses_the_table() {
    let hal = read_repo_file("hal/linc-sim3d.hal");
    let known: Vec<&str> = PINS.iter().map(|spec| spec.name).collect();

    // Every linc-sim3d.pin named in the wiring exists in the table.
    let wiring = hal
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for token in wiring.split(|c: char| c.is_whitespace() || c == ';') {
        if let Some(name) = token.strip_prefix("linc-sim3d.") {
            assert!(
                known.contains(&name),
                "hal/linc-sim3d.hal uses unknown pin linc-sim3d.{name}"
            );
        }
    }

    // Each machine-state input pin is netted exactly once.
    let net_lines: Vec<&str> = hal
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("net "))
        .collect();
    for pin in ["pos-x", "pos-y", "pos-z", "tool-number"] {
        let needle = format!("linc-sim3d.{pin}");
        let count = net_lines
            .iter()
            .filter(|line| line.split_whitespace().any(|token| token == needle))
            .count();
        assert_eq!(count, 1, "linc-sim3d.{pin} must appear in exactly one net");
    }

    // The advisory live offset has no HAL source; it is pinned to its default.
    assert!(
        hal.lines()
            .any(|line| line.trim() == "setp linc-sim3d.tool-offset 0"),
        "tool-offset must be pinned to its documented default"
    );
    assert!(
        !net_lines
            .iter()
            .any(|line| line.contains("linc-sim3d.tool-offset")),
        "no HAL pin drives tool-offset, so it cannot be netted"
    );

    // The component is started through the wrapper script and the OR is added
    // to the servo thread.
    assert!(hal.contains("loadusr -Wn linc-sim3d ../scripts/sim3d-hal.sh"));
    assert!(hal.contains("addf or2.probe_sim3d servo-thread"));
    assert!(hal.contains("net sig_rio_probe_bit => or2.probe_sim3d.in0"));
    assert!(hal.contains("net sim3d-probe-out linc-sim3d.probe-out => or2.probe_sim3d.in1"));
    assert!(hal.contains("net probe-input or2.probe_sim3d.out => motion.probe-input"));
}

#[test]
fn the_baier_chain_only_misses_the_sibling_checkout() {
    // configs/baiermill-pb.ini lists ../../baiermill_pb/hallib/*.hal, which
    // lives outside this repository. Every other listed file must exist, or the
    // chain checks above would silently skip files.
    let chain = HalChain::load("configs/baiermill-pb.ini");
    assert!(
        chain
            .missing
            .iter()
            .all(|listed| listed.starts_with("../../")),
        "only the sibling checkout may be absent: {:?}",
        chain.missing
    );
}

#[test]
fn no_pin_is_read_by_two_signals_inside_one_profile() {
    // HAL pins take exactly one signal. The two profiles load different HAL
    // chains, so the rule is per chain: sim.ini's simulator chain and
    // baiermill-pb.ini's machine chain are each checked against the files that
    // profile actually lists - a reader in one is not a reader in the other.
    for ini in [
        "configs/sim.ini",
        "configs/sim-no3d.ini",
        "configs/baiermill-pb.ini",
    ] {
        let chain = HalChain::load(ini);
        let doubled = pins_read_twice(&chain);
        assert!(
            doubled.is_empty(),
            "{} drives a pin from two signals: {doubled:#?}",
            chain.ini
        );
    }
}

#[test]
fn the_sim_profile_reads_the_probe_through_the_simulated_or() {
    let chain = HalChain::load("configs/sim.ini");
    chain.assert_loads("hal/linc-sim3d.hal");
    chain.assert_does_not_load("hal/baiermill-pb-postgui.hal");
    // The two simulation profiles are mutually exclusive: only this one loads
    // the 3D simulator, and only the other carries the standalone reader.
    chain.assert_does_not_load("hal/linc-sim-no3d.hal");

    let readers = reader_lines(&chain, "motion.probe-input");
    assert_eq!(
        readers.len(),
        1,
        "motion.probe-input takes exactly one signal inside the sim profile, found: {readers:#?}"
    );
    assert!(
        readers[0].starts_with("hal/linc-sim3d.hal:") && readers[0].contains("or2.probe_sim3d.out"),
        "the sim reader is the simulated probe OR in linc-sim3d.hal: {}",
        readers[0]
    );
    assert!(
        !read_repo_file("hal/linc.hal").contains("=> motion.probe-input"),
        "linc.hal must not drive the probe input; hal/linc-sim3d.hal owns it in the sim profile"
    );
}

#[test]
fn the_no_simulator_profile_reads_the_probe_through_the_panel_key() {
    let chain = HalChain::load("configs/sim-no3d.ini");
    chain.assert_loads("hal/linc-sim-no3d.hal");
    chain.assert_does_not_load("hal/linc-sim3d.hal");

    let readers = reader_lines(&chain, "motion.probe-input");
    assert_eq!(
        readers.len(),
        1,
        "motion.probe-input takes exactly one signal inside the no-simulator profile, found: {readers:#?}"
    );
    assert!(
        readers[0].starts_with("hal/linc-sim-no3d.hal:")
            && readers[0].contains("sig_rio_probe_bit"),
        "without the 3D simulator the panel PROBE key is the only probe source: {}",
        readers[0]
    );
    assert!(
        !readers[0].contains("or2.probe_sim3d"),
        "the simulator OR is not part of the no-simulator profile: {}",
        readers[0]
    );
}

#[test]
fn the_baier_profile_reads_the_probe_through_the_machine_signal() {
    let chain = HalChain::load("configs/baiermill-pb.ini");
    chain.assert_loads("hal/baiermill-pb-postgui.hal");
    chain.assert_does_not_load("hal/linc-sim3d.hal");
    chain.assert_does_not_load("hal/linc-sim.hal");

    let readers = reader_lines(&chain, "motion.probe-input");
    assert_eq!(
        readers.len(),
        1,
        "motion.probe-input takes exactly one signal inside the BaierMill profile, found: {readers:#?}"
    );
    assert!(
        readers[0].starts_with("hal/baiermill-pb-postgui.hal:")
            && readers[0].contains("sig_rio_probe_bit"),
        "the machine's own probe signal must drive it: {}",
        readers[0]
    );
    assert!(
        !readers[0].contains("or2.probe_sim3d"),
        "the simulator OR is not part of the machine profile: {}",
        readers[0]
    );
}

#[test]
fn the_single_reader_check_catches_a_double_drive() {
    // Negative control: two files in one chain net the same pin. The generic
    // rule must catch it, and the probe-specific lookup must see both readers,
    // or the two tests above would pass on a real double drive.
    let chain = HalChain::from_files(vec![
        (
            "hal/first.hal".to_string(),
            "net a or2.probe_sim3d.out => motion.probe-input\n".to_string(),
        ),
        (
            "hal/second.hal".to_string(),
            "# a comment that only mentions motion.probe-input\n\
             net b rio.probe.bit => motion.probe-input\n"
                .to_string(),
        ),
    ]);

    assert_eq!(
        reader_lines(&chain, "motion.probe-input").len(),
        2,
        "both readers must be found, comments must not count"
    );
    let doubled = pins_read_twice(&chain);
    assert_eq!(doubled.len(), 1, "{doubled:#?}");
    assert!(
        doubled[0].contains("motion.probe-input")
            && doubled[0].contains("hal/first.hal:1")
            && doubled[0].contains("hal/second.hal:2"),
        "the failure must name the pin and both files: {}",
        doubled[0]
    );
}

#[test]
fn or2_is_loaded_once_in_linc_hal() {
    let hal = read_repo_file("hal/linc.hal");
    let loads: Vec<&str> = hal
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("loadrt or2"))
        .collect();
    assert_eq!(
        loads.len(),
        1,
        "a second loadrt or2 fails with 'already exists'"
    );
    assert!(
        loads[0].contains("or2.probe_sim3d"),
        "the probe OR instance belongs on the single loadrt or2 line: {}",
        loads[0]
    );
    assert!(
        !hal.lines().any(|line| {
            let line = line.trim();
            !line.starts_with('#') && line.starts_with("net ") && line.contains(" linc.")
        }),
        "linc.* pins only exist after the GUI starts; no pre-GUI linc.* net"
    );
    assert!(
        !hal.lines()
            .any(|line| line.trim() == "addf or2.probe_sim3d servo-thread"),
        "or2.probe_sim3d is simulation-only and is added in linc-sim3d.hal"
    );
}

#[test]
fn wrapper_script_and_ini_agree_with_the_component() {
    let script = read_repo_file("scripts/sim3d-hal.sh");
    assert_eq!(COMPONENT_NAME, "linc-sim3d");
    assert!(script.contains("target/$profile/linc-sim3d"));
    assert!(script.contains("exec \"$binary\" --hal --config configs/sim3d.toml"));
    for profile in ["release", "debug"] {
        assert!(script.contains(profile));
    }

    let test_script = read_repo_file("scripts/sim3d-hal-test.sh");
    assert!(test_script.contains("--hal --config"));
    assert!(test_script.contains("halrun"));
    assert!(test_script.contains("probe-out"));
    assert!(test_script.contains("collision"));

    // Both scripts are executable.
    for relative in ["scripts/sim3d-hal.sh", "scripts/sim3d-hal-test.sh"] {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(repo_file(relative))
            .unwrap()
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "{relative} must be executable (mode {mode:o})"
        );
    }

    // The INI loads the simulator after linc-sim.hal and before the GUI.
    let ini = read_repo_file("configs/sim.ini");
    let sim_index = ini
        .lines()
        .position(|line| line.trim() == "HALFILE = ../hal/linc-sim.hal")
        .expect("configs/sim.ini keeps the linc-sim.hal HALFILE");
    let sim3d_index = ini
        .lines()
        .position(|line| line.trim() == "HALFILE = ../hal/linc-sim3d.hal")
        .expect("configs/sim.ini loads hal/linc-sim3d.hal");
    let postgui_index = ini
        .lines()
        .position(|line| line.trim().starts_with("POSTGUI_HALFILE"))
        .expect("configs/sim.ini keeps its POSTGUI_HALFILE");
    assert!(
        sim_index < sim3d_index,
        "linc-sim3d.hal comes after linc-sim.hal"
    );
    assert!(
        sim3d_index < postgui_index,
        "linc-sim3d.hal loads before the GUI"
    );
}
