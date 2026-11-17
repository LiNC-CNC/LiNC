//! Aliases are the operand; the address is the binding.
//!
//! A PLCYard source names an I/O point and comments it with a symbol
//! (`"%I0.0": "Enable"`). The converted project reads the alias — `enable` — and
//! carries `%I0.0` as the point's `address`, so a trace or a HAL map that speaks
//! addresses still finds it. Only a point with no alias keeps the importer's own
//! spelling.

use std::path::{Path, PathBuf};

use linc_plc_core::Project;

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
        .join("../../examples/plc-acceptance")
        .canonicalize()
        .expect("the corpus is in the repository")
}

fn project(name: &str) -> Project {
    let text = std::fs::read_to_string(corpus_dir().join("projects").join(format!("{name}.json")))
        .expect("reads");
    serde_json::from_str(&text).expect("parses")
}

/// Every point carries the address it stands for, so a trace's `%Q0.0` or a HAL
/// pin map finds it whatever the operand is called.
#[test]
fn every_io_point_carries_its_address() {
    for name in CORPUS {
        let project = project(name);
        for point in &project.io {
            assert!(
                point.address.starts_with('%'),
                "{name}: {} has no address binding",
                point.name
            );
            assert_ne!(
                point.address, point.name,
                "{name}: {} is named by its address",
                point.name
            );
        }
    }
}

/// A trace names its outputs by address; every one of them must resolve.
#[test]
fn every_traced_output_resolves_by_address() {
    for name in CORPUS {
        let project = project(name);
        let text =
            std::fs::read_to_string(corpus_dir().join("traces").join(format!("{name}.json")))
                .expect("the trace reads");
        let trace: serde_json::Value = serde_json::from_str(&text).expect("the trace parses");
        let outputs: Vec<&str> = trace["outputs"]
            .as_array()
            .expect("outputs")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        for tag in outputs {
            if !tag.starts_with('%') {
                // A declared word or a member value, not an address.
                continue;
            }
            assert!(
                project
                    .io
                    .iter()
                    .any(|point| point.address.eq_ignore_ascii_case(tag)),
                "{name}: the trace's {tag} matches no point's address"
            );
        }
    }
}

/// timer-counter is the worked example: `%I0.0` is `Enable`, so the ladder reads
/// `enable` and the mangled spelling is gone.
#[test]
fn an_aliased_point_reads_by_its_alias() {
    let project = project("timer-counter");
    let names: Vec<(&str, &str)> = project
        .io
        .iter()
        .map(|point| (point.name.as_str(), point.address.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("enable", "%I0.0"),
            ("reset_count", "%I0.1"),
            ("lamp", "%Q0.0"),
            ("five_cycles", "%Q0.1"),
        ]
    );
    let text =
        std::fs::read_to_string(corpus_dir().join("projects/timer-counter.json")).expect("reads");
    assert!(
        !text.contains("n__i0_0") && !text.contains("n__q0_0"),
        "the mangled spelling survives where an alias exists"
    );
    let first = &project.pous[0].sections[0].rungs[0];
    let cell = first.cells.first().expect("a cell");
    assert_eq!(
        cell.operand(),
        Some("enable"),
        "the rung reads the alias: {cell:?}"
    );
}
