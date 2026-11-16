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

use linc_plc_core::model::{IoDirection, IoKind, Project};
use linc_plc_core::{compile, plcopen, validate, Severity, Vm};

/// The files that run and match their trace today: §III.8 rule 4's count. A
/// constant, so raising it is a visible diff and a file that starts passing is
/// noticed rather than quietly counted.
const CONVERTIBLE: [&str; 12] = [
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
    /// Analogue inputs: a word point's raw value from a scan onward.
    #[serde(default)]
    words: BTreeMap<String, Vec<(u32, i32)>>,
}

#[derive(serde::Deserialize)]
struct Manifest {
    files: Vec<ManifestFile>,
}

#[derive(serde::Deserialize)]
struct ManifestFile {
    name: String,
    /// Numeric outputs this file compares within a tolerance instead of exactly. A
    /// name that is not listed is exact, and a bit is always exact.
    #[serde(default)]
    tolerance: BTreeMap<String, f64>,
    /// The converter produced `<name>.xml`.
    converted: bool,
    /// LiNC imports it and matches its trace after every scan.
    runs: bool,
    blocker: Option<String>,
}

// ---------------------------------------------------------------------------
// Replay. Every output starts at `"0"` and takes the last change at or before
// the scan under test; a one-scan offset is a failure.

fn expected_at(trace: &Trace, scan: u32) -> BTreeMap<&str, f64> {
    let mut now = trace
        .outputs
        .iter()
        .map(|tag| (tag.as_str(), 0.0))
        .collect::<BTreeMap<_, _>>();
    for change in &trace.changes {
        if change.scan > scan {
            break;
        }
        for (tag, value) in &change.v {
            // A bit's level is `1`; a real is its value as written. Both are numbers
            // here, so the replay holds one table.
            now.insert(tag.as_str(), value.parse::<f64>().unwrap_or(0.0));
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
fn run(
    xml: &str,
    trace: &Trace,
    stimulus: &Stimulus,
    tolerance: &BTreeMap<String, f64>,
) -> Result<(), String> {
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
        .filter(|point| point.direction == IoDirection::Out && point.kind == IoKind::Bit)
        .map(|point| point.name.clone())
        .collect::<Vec<_>>();
    let word_outputs = project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::Out && point.kind != IoKind::Bit)
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
    let traced_set: std::collections::BTreeSet<&str> = traced.iter().map(String::as_str).collect();
    // Every bit output is traced, and a trace may add a declared word variable: an
    // analogue value is observable even though it is not a bit output.
    let words = compiled
        .symbols
        .iter()
        .filter(|symbol| symbol.bank == linc_plc_core::Bank::Word)
        .map(|symbol| symbol.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let declared_words = word_outputs
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let missing = declared
        .iter()
        .chain(declared_words.iter())
        .filter(|name| !traced_set.contains(**name))
        .collect::<Vec<_>>();
    let unknown = traced_set
        .iter()
        .filter(|name| {
            !declared.contains(**name)
                && !declared_words.contains(**name)
                && !words.contains(**name)
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() || !unknown.is_empty() {
        return Err(format!(
            "the trace's outputs {traced_set:?} do not cover the program's {declared:?} \
             and name {unknown:?}, which are not declared words"
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
        let words = compiled
            .word_inputs
            .iter()
            .map(|binding| {
                // The stimulus names the point as the source does; the binding names
                // it as the model does.
                let tag = stimulus
                    .words
                    .keys()
                    .find(|tag| plcopen::operand_name(tag) == binding.name)
                    .cloned()
                    .unwrap_or_else(|| binding.name.clone());
                level_word(&stimulus, &tag, tick)
            })
            .collect::<Vec<_>>();
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
        vm.scan_with_io(
            linc_plc_core::IoFrame {
                bits: &inputs,
                words: &words,
                floats: &[],
            },
            stimulus.scan_ms,
        );
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
            let want = want_of(name) != 0.0;
            if vm.outputs()[index] != want {
                return Err(format!(
                    "{name} at scan {scan} is {}, the trace says {want}",
                    vm.outputs()[index]
                ));
            }
        }
        // A traced word variable is compared with its declared tolerance; a missing
        // tolerance means exact.
        for tag in &trace.outputs {
            let name = plcopen::operand_name(tag);
            if declared.contains(name.as_str()) || declared_words.contains(name.as_str()) {
                continue;
            }
            let want = want_of(&name);
            let Some(slot) = vm.word_slot(&name) else {
                return Err(format!("{name} is traced but the program has no such word"));
            };
            let raw = vm.word(slot);
            let real = compiled
                .word_types
                .iter()
                .find(|(at, _)| *at == slot)
                .map(|(_, ty)| *ty == linc_plc_core::profile::layout::WordType::REAL)
                .unwrap_or(false);
            let got = if real {
                f64::from(f32::from_bits(raw as u32))
            } else {
                f64::from(raw)
            };
            // The manifest names the output as the source does.
            let tolerance = tolerance
                .iter()
                .find(|(key, _)| plcopen::operand_name(key) == name)
                .map(|(_, value)| *value)
                .unwrap_or(0.0);
            if (got - want).abs() > tolerance {
                return Err(format!(
                    "{name} at scan {scan} is {got}, the trace says {want} \
                     (tolerance {tolerance})"
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
                let outcome = run(&read(&xml_path), &trace, &stimulus, &file.tolerance);
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
        match run(&read(&xml_path), &trace, &stimulus, &file.tolerance) {
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
    assert_eq!(expected_at(&trace, 2)["%Q0.0"], 0.0);
    assert_eq!(expected_at(&trace, 3)["%Q0.0"], 1.0);
    assert_eq!(
        expected_at(&trace, 19)["%Q0.0"],
        1.0,
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
    assert_eq!(
        at(1)["%Q0.0"],
        0.0,
        "the change starts at scan 2, not before"
    );
    assert_eq!(at(2)["%Q0.0"], 1.0);
    assert_eq!(at(4)["%Q0.0"], 1.0);
    assert_eq!(at(5)["%Q0.0"], 0.0);
    assert_eq!(at(5)["%Q0.1"], 1.0);
    assert_eq!(at(9)["%Q0.1"], at(5)["%Q0.1"]);
}

/// A word input's raw value at a tick: the last write at or before it.
fn level_word(stimulus: &Stimulus, tag: &str, tick: u32) -> i32 {
    let mut value = 0;
    for (at, level) in stimulus.words.get(tag).map(Vec::as_slice).unwrap_or(&[]) {
        if *at <= tick {
            value = *level;
        }
    }
    value
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

/// The converted projects are the same program as their XML.
///
/// `projects/*.json` is what the PLC page's Open reads, so a corpus tested only
/// as XML would leave the open path unproven: the JSON could rot and no test
/// would say. Each pair is compiled and the two programs compared — the hash,
/// the word layout and the ops. A missing project is a failure, not a skip.
#[test]
fn every_project_json_is_the_same_program_as_its_xml() {
    let dir = corpus();
    let projects = dir.join("projects");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("the corpus directory reads") {
        let xml_path = entry.expect("an entry").path();
        if xml_path.extension().and_then(|ext| ext.to_str()) != Some("xml") {
            continue;
        }
        let stem = xml_path.file_stem().expect("a stem");
        let json_path = projects.join(stem).with_extension("json");
        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|error| panic!("{}: {error}", json_path.display()));
        let parsed: linc_plc_core::model::Project = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("{}: {error}", json_path.display()));

        // The page validates before it offers START, and so must the file.
        let diagnostics = linc_plc_core::validate(&parsed);
        let errors = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert!(
            errors.is_empty(),
            "{}: {:?}",
            json_path.display(),
            errors
                .iter()
                .map(|diagnostic| format!("{} @ {}", diagnostic.message, diagnostic.path))
                .collect::<Vec<_>>()
        );

        let xml = std::fs::read_to_string(&xml_path).expect("the xml reads");
        let imported = linc_plc_core::plcopen::import(&xml).expect("the xml imports");
        let from_xml = linc_plc_core::compile(&imported).expect("the xml compiles");
        let from_json = linc_plc_core::compile(&parsed).expect("the json compiles");
        // The comparison is the compiled program, not the project's spelling:
        // the JSON is a canonicalised form, so the two files differ in
        // representation and must agree in what they compile to.
        assert_eq!(
            from_json.program.ops,
            from_xml.program.ops,
            "{} does not compile to the program {} does",
            json_path.display(),
            xml_path.display()
        );
        assert_eq!(
            from_json.program.words.total(),
            from_xml.program.words.total()
        );
        assert_eq!(
            from_json
                .program
                .words
                .regions()
                .iter()
                .map(|region| (region.name, region.base, region.len))
                .collect::<Vec<_>>(),
            from_xml
                .program
                .words
                .regions()
                .iter()
                .map(|region| (region.name, region.base, region.len))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            from_json.program.output_count,
            from_xml.program.output_count
        );
        checked += 1;
    }
    assert_eq!(checked, 12, "every corpus file has a converted project");
}
