//! The acceptance corpus in `examples/plc-acceptance/`, checked rule by rule
//! against §III.8.
//!
//! The corpus is GPL-3.0-or-later and is aggregated with LiNC, never linked into
//! it, so rule 5 is a build rule: nothing here may be embedded with
//! `include_str!`, no build script may name the directory, and the test binary
//! must hold none of it. Every file below is read from disk while the test runs.
//!
//! What `manifest.json` says about each file, and what is asserted about it:
//!
//! * `converted` — the converter produced `<name>.xml`;
//! * `runs` — LiNC imports it, compiles it, and matches its trace after **every**
//!   scan. That is the "N of 12 convertible" count, and the set is asserted;
//! * otherwise — the file names its cause, and a converted file that does not run
//!   must be shown *not* to pass, so the count cannot drift green.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use linc_plc_core::model::{IoDirection, Project};
use linc_plc_core::{compile, plcopen, validate, Severity, Vm};

/// The files that run and match their trace today: §III.8 rule 4's count. A
/// constant, so raising it is a visible diff and a file that starts passing is
/// noticed rather than quietly counted.
const CONVERTIBLE: [&str; 6] = [
    "garage-door",
    "motor-star-delta",
    "silo",
    "start-stop-delay",
    "timer-counter",
    "traffic-lights",
];

fn corpus() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plc-acceptance");
    root.canonicalize().unwrap_or(root)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// The corpus files, as serde sees them.

#[derive(serde::Deserialize)]
struct Trace {
    scan_ms: u16,
    scans: u32,
    outputs: Vec<String>,
    fault: Option<String>,
    changes: Vec<Change>,
}

#[derive(serde::Deserialize)]
struct Change {
    scan: u32,
    v: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct Stimulus {
    scan_ms: u16,
    scans: u32,
    #[serde(default)]
    held: BTreeMap<String, bool>,
    #[serde(default)]
    inputs: BTreeMap<String, Vec<(u32, bool)>>,
}

#[derive(serde::Deserialize)]
struct Manifest {
    files: Vec<ManifestFile>,
}

#[derive(serde::Deserialize)]
struct ManifestFile {
    name: String,
    /// The converter produced `<name>.xml`.
    converted: bool,
    /// LiNC imports it and matches its trace after every scan.
    runs: bool,
    blocker: Option<String>,
}

// ---------------------------------------------------------------------------
// Replay. Every output starts at `"0"` and takes the last change at or before
// the scan under test; a one-scan offset is a failure.

fn expected_at(trace: &Trace, scan: u32) -> BTreeMap<&str, bool> {
    let mut now = trace
        .outputs
        .iter()
        .map(|tag| (tag.as_str(), false))
        .collect::<BTreeMap<_, _>>();
    for change in &trace.changes {
        if change.scan > scan {
            break;
        }
        for (tag, value) in &change.v {
            now.insert(tag.as_str(), value == "1");
        }
    }
    now
}

/// The reference records a change at the scan index *after* the tick that caused
/// it, so tick `k` is compared with scan `k + 1`.
fn level_at(stimulus: &Stimulus, tag: &str, tick: u32) -> bool {
    let mut value = stimulus.held.get(tag).copied().unwrap_or(false);
    for (at, level) in stimulus.inputs.get(tag).map(Vec::as_slice).unwrap_or(&[]) {
        if *at <= tick {
            value = *level;
        }
    }
    value
}

// ---------------------------------------------------------------------------

/// Run the pipeline on one converted file. `Err` is the reason it does not match
/// its trace: a refusal with its code, or the first scan that differs.
fn run(xml: &str, trace: &Trace, stimulus: &Stimulus) -> Result<(), String> {
    let mut project: Project = match plcopen::import(xml) {
        Ok(project) => project,
        Err(diagnostics) => {
            let first = &diagnostics[0];
            return Err(format!(
                "import refused: {} — {}",
                first.code, first.message
            ));
        }
    };
    let errors = validate(&project)
        .into_iter()
        .filter(|item| item.severity == Severity::Error)
        .collect::<Vec<_>>();
    if let Some(first) = errors.first() {
        return Err(format!("validate: {} — {}", first.code, first.message));
    }
    project.io.retain(|point| point.name.starts_with("n_"));
    let compiled = match compile(&project) {
        Ok(compiled) => compiled,
        Err(diagnostics) => {
            let first = &diagnostics[0];
            return Err(format!("compile: {} — {}", first.code, first.message));
        }
    };

    let input_names = project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::In)
        .map(|point| point.name.clone())
        .collect::<Vec<_>>();
    let output_names = project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::Out)
        .map(|point| point.name.clone())
        .collect::<Vec<_>>();

    let declared = output_names
        .iter()
        .map(|name| name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let traced = trace
        .outputs
        .iter()
        .map(|tag| plcopen::operand_name(tag))
        .collect::<Vec<_>>();
    let traced_set = traced.iter().map(String::as_str).collect();
    if declared != traced_set {
        return Err(format!(
            "the program's outputs {declared:?} are not the trace's {traced_set:?}"
        ));
    }

    // The stimulus says how the program is driven and for how long; the trace says
    // what came out. Together they describe one run, so a disagreement is a corpus
    // bug and must not be resolved by quietly preferring one of them.
    if stimulus.scan_ms != trace.scan_ms {
        return Err(format!(
            "the stimulus runs at {} ms per scan, the trace was recorded at {}",
            stimulus.scan_ms, trace.scan_ms
        ));
    }
    if stimulus.scans != trace.scans {
        return Err(format!(
            "the stimulus covers {} scans, the trace covers {}",
            stimulus.scans, trace.scans
        ));
    }

    let mut vm = Vm::new(&compiled);
    for tick in 0..stimulus.scans {
        let scan = tick + 1;
        if trace.fault.is_some() && scan >= stimulus.scans {
            break;
        }
        let inputs = input_names
            .iter()
            .map(|name| {
                trace
                    .outputs
                    .iter()
                    .find(|tag| plcopen::operand_name(tag) == *name)
                    .map(|tag| level_at(stimulus, tag, tick))
                    .unwrap_or_else(|| {
                        stimulus
                            .inputs
                            .keys()
                            .chain(stimulus.held.keys())
                            .find(|tag| plcopen::operand_name(tag) == *name)
                            .map(|tag| level_at(stimulus, tag, tick))
                            .unwrap_or(false)
                    })
            })
            .collect::<Vec<_>>();
        vm.scan_with_dt(&inputs, stimulus.scan_ms);
        let expected = expected_at(trace, scan);
        let want_of = |name: &str| {
            trace
                .outputs
                .iter()
                .find(|tag| plcopen::operand_name(tag) == name)
                .map(|tag| *expected.get(tag.as_str()).expect("replay covers it"))
                .unwrap_or_else(|| panic!("{name} is not in the trace"))
        };
        for (index, name) in output_names.iter().enumerate() {
            let want = want_of(name);
            if vm.outputs()[index] != want {
                return Err(format!(
                    "{name} at scan {scan} is {}, the trace says {want}",
                    vm.outputs()[index]
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------

#[test]
fn the_corpus_matches_its_traces_or_names_its_cause() {
    let root = corpus();
    let manifest: Manifest = serde_json::from_str(&read(&root.join("manifest.json")))
        .expect("manifest.json is readable JSON");

    let mut convertible = Vec::new();
    let mut blocked = Vec::new();
    for file in &manifest.files {
        let stimulus: Stimulus = serde_json::from_str(&read(
            &root.join("stimuli").join(format!("{}.json", file.name)),
        ))
        .unwrap_or_else(|error| panic!("{}: stimulus unreadable: {error}", file.name));
        let trace: Trace = serde_json::from_str(&read(
            &root.join("traces").join(format!("{}.json", file.name)),
        ))
        .unwrap_or_else(|error| panic!("{}: trace unreadable: {error}", file.name));
        let xml_path = root.join(format!("{}.xml", file.name));

        if !file.runs {
            let blocker = file
                .blocker
                .as_deref()
                .unwrap_or_else(|| panic!("{}: does not run and names no cause", file.name));
            assert!(!blocker.is_empty(), "{}: empty cause", file.name);
            if file.converted {
                assert!(xml_path.exists(), "{}: no XML to convert", file.name);
                // Converted but not passing: prove it does not pass, so a file
                // that starts matching is noticed instead of staying "blocked".
                let outcome = run(&read(&xml_path), &trace, &stimulus);
                assert!(
                    outcome.is_err(),
                    "{name} is listed as blocked ({blocker}) but it matches its trace; \
                     set runs to true",
                    name = file.name
                );
            }
            blocked.push((file.name.clone(), blocker.to_string()));
            continue;
        }

        assert!(
            file.converted && xml_path.exists(),
            "{}: runs, but no conversion exists",
            file.name
        );
        match run(&read(&xml_path), &trace, &stimulus) {
            Ok(()) => convertible.push(file.name.clone()),
            Err(cause) => panic!("{}: does not run: {cause}", file.name),
        }
    }

    println!(
        "{} of {} convertible, {} blocked",
        convertible.len(),
        manifest.files.len(),
        blocked.len()
    );
    for (name, blocker) in &blocked {
        println!("  blocked: {name} — {blocker}");
    }

    assert_eq!(
        convertible, CONVERTIBLE,
        "the set that converts changed; update CONVERTIBLE with the reason"
    );
    assert_eq!(
        convertible.len() + blocked.len(),
        manifest.files.len(),
        "every corpus file either runs or is blocked by name"
    );
}

/// Rule 3, on a trace this corpus does not have: a non-null `fault` ends the run.
/// The twelve shipped traces are all fault-free, so the rule is checked here
/// rather than left to hope.
#[test]
fn a_fault_is_carried_by_its_trace() {
    let trace: Trace = serde_json::from_str(
        r#"{"scan_ms": 10, "scans": 40, "outputs": ["%Q0.0"], "fault": "watchdog",
            "changes": [{"scan": 3, "v": {"%Q0.0": "1"}},
                        {"scan": 20, "v": {"%Q0.0": "0"}}]}"#,
    )
    .expect("the synthetic trace parses");
    assert_eq!(trace.fault.as_deref(), Some("watchdog"));
    assert!(!expected_at(&trace, 2)["%Q0.0"]);
    assert!(expected_at(&trace, 3)["%Q0.0"]);
    assert!(
        expected_at(&trace, 19)["%Q0.0"],
        "a change after the fault is not replayed before it"
    );
}

/// Rule 2, on a trace this corpus does not have: the value at a scan is the last
/// change at or before it, and a one-scan difference is a difference.
#[test]
fn a_trace_is_replayed_not_searched() {
    let trace: Trace = serde_json::from_str(
        r#"{"scan_ms": 10, "scans": 10, "outputs": ["%Q0.0", "%Q0.1"], "fault": null,
            "changes": [{"scan": 2, "v": {"%Q0.0": "1"}},
                        {"scan": 5, "v": {"%Q0.0": "0", "%Q0.1": "1"}}]}"#,
    )
    .expect("the synthetic trace parses");
    let at = |scan| {
        expected_at(&trace, scan)
            .into_iter()
            .map(|(tag, value)| (tag.to_string(), value))
            .collect::<BTreeMap<_, _>>()
    };
    assert!(!at(1)["%Q0.0"], "the change starts at scan 2, not before");
    assert!(at(2)["%Q0.0"]);
    assert!(at(4)["%Q0.0"]);
    assert!(!at(5)["%Q0.0"]);
    assert!(at(5)["%Q0.1"]);
    assert_eq!(at(9)["%Q0.1"], at(5)["%Q0.1"]);
}

/// The stimulus rule: a change applies from its scan onwards, and an input the
/// stimulus never drives is false, not an error — `garage-door` has a beam the
/// program reads and the run leaves untouched.
#[test]
fn a_stimulus_holds_and_releases() {
    let stimulus: Stimulus = serde_json::from_str(
        r#"{"scan_ms": 10, "scans": 5, "held": {"%I0.1": true},
            "inputs": {"%I0.0": [[0, true], [2, false]]}}"#,
    )
    .expect("the synthetic stimulus parses");
    assert!(level_at(&stimulus, "%I0.0", 0));
    assert!(level_at(&stimulus, "%I0.0", 1));
    assert!(!level_at(&stimulus, "%I0.0", 2));
    assert!(level_at(&stimulus, "%I0.1", 0), "a held input stays held");
    assert!(
        !level_at(&stimulus, "%I0.9", 0),
        "an undriven input is false"
    );
}
