//! A coil names the variable it writes.
//!
//! IEC 61131-3's coil is an assignment: `--( )--` with nothing above it writes
//! nowhere and draws as a nameless `( )`. The converters used to emit one for a
//! branch row (a row that is a parallel continuation has no coil at all) and for
//! any block that feeds no coil, so converted projects carried 17 of them.

use std::path::{Path, PathBuf};

use linc_plc_core::model::{Coil, CoilKind, Rung};
use linc_plc_core::{compile, validate, Logic, Project, Severity};

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

/// Every rung that carries a coil must carry one that names its target.
fn nameless(project: &Project) -> Vec<String> {
    let mut out = Vec::new();
    for pou in project.all_pous() {
        for (section, list) in pou.sections.iter().enumerate() {
            for (rung, row) in list.rungs.iter().enumerate() {
                if let Some(coil) = row.coil.as_ref() {
                    if !coil.names_a_target() {
                        out.push(format!("{}/s{section}/r{rung}", pou.name));
                    }
                }
            }
        }
    }
    out
}

/// The import path the converters take — XML in, project out, then the editor's
/// normalisation — produces no nameless coil for any corpus program.
#[test]
fn the_importer_emits_no_nameless_coil() {
    let dir = corpus_dir();
    for name in CORPUS {
        let xml = std::fs::read_to_string(dir.join(format!("{name}.xml"))).expect("the xml reads");
        let mut project = linc_plc_core::plcopen::import(&xml).expect("the xml imports");
        let pous = project.all_pous();
        project.set_pous(pous);
        project.normalise();
        assert!(
            nameless(&project).is_empty(),
            "{name} imported a nameless coil at {:?}",
            nameless(&project)
        );
        let errors: Vec<String> = validate(&project)
            .into_iter()
            .filter(|item| item.severity == Severity::Error)
            .map(|item| format!("{} @ {}", item.message, item.path))
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        assert!(compile(&project).is_ok(), "{name} compiles");
    }
}

/// The committed projects carry none either, so a load never shows a nameless
/// `( )` and never has to be refused for one.
#[test]
fn the_committed_projects_carry_no_nameless_coil() {
    let dir = corpus_dir().join("projects");
    for name in CORPUS {
        let text = std::fs::read_to_string(dir.join(format!("{name}.json"))).expect("reads");
        let project: Project = serde_json::from_str(&text).expect("parses");
        assert!(
            nameless(&project).is_empty(),
            "{name}.json carries a nameless coil at {:?}",
            nameless(&project)
        );
    }
}

/// Normalising a project clears a blank coil slot, so a file written by an older
/// build — or saved in the middle of naming a coil — never keeps one.
#[test]
fn normalising_clears_a_blank_coil() {
    let mut project = Project::new("blank");
    project.io = vec![linc_plc_core::IoPoint::new(
        "run",
        linc_plc_core::IoDirection::Out,
    )];
    project.sections = vec![linc_plc_core::model::Section {
        name: "main".to_string(),
        rungs: vec![Rung {
            coil: Some(Coil::new("", CoilKind::Normal)),
            ..Rung::new(
                Logic::Contact {
                    var: "start".to_string(),
                    negated: false,
                },
                Coil::new("run", CoilKind::Normal),
            )
        }],
    }];
    project.set_pous(project.all_pous());

    assert_eq!(nameless(&project).len(), 1, "the fixture carries one");
    assert!(validate(&project)
        .iter()
        .any(|item| item.code == "coil_operand"));

    project.normalise();
    assert!(
        nameless(&project).is_empty(),
        "normalise clears the blank slot"
    );
    assert!(!validate(&project)
        .iter()
        .any(|item| item.code == "coil_operand"));
}
