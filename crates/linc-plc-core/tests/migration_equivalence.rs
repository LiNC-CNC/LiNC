//! The editor normalises a project when it opens it: `Project::normalise` lays
//! every tree rung out as grid cells, so the canvas has one model. That migration
//! is a representation change and must not change what the project means.
//!
//! It did: the cell form of a function block demanded a literal preset while the
//! tree form accepts a declared word, an array element or one field of an array of
//! structs, so eight of the twelve corpus projects were refused the moment the
//! editor opened them (`traffic-lights-routines` timed on `Durations[Phase]`,
//! `recipes-data-type-ld` on `Recipes[Sel].FillTime`).

use std::path::{Path, PathBuf};

use linc_plc_core::{compile, validate, Diagnostic, Project, Severity};

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

fn errors(project: &Project) -> Vec<Diagnostic> {
    validate(project)
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect()
}

fn describe(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{} @ {}", diagnostic.message, diagnostic.path))
        .collect()
}

/// Validation before and after the migration: the same errors, and none at all
/// for a project the corpus ships as valid.
#[test]
fn migration_does_not_invalidate_a_valid_project() {
    for name in CORPUS {
        let path = corpus_dir().join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).expect("the project reads");
        let project: Project = serde_json::from_str(&text).expect("the project parses");

        let before = errors(&project);
        assert!(
            before.is_empty(),
            "{name} is invalid before migration: {:?}",
            describe(&before)
        );

        let mut migrated = project.clone();
        migrated.normalise();
        let after = errors(&migrated);
        assert_eq!(
            after.len(),
            before.len(),
            "{name} gained errors when the editor laid it out as cells: {:?}",
            describe(&after)
        );
    }
}

/// The migration is a representation change: the compiled program is the same op
/// for op, so a project cannot behave differently after being opened.
#[test]
fn migration_does_not_change_the_compiled_program() {
    for name in CORPUS {
        let path = corpus_dir().join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).expect("the project reads");
        let project: Project = serde_json::from_str(&text).expect("the project parses");

        let mut migrated = project.clone();
        migrated.normalise();

        let before = compile(&project).expect("the project compiles");
        let after = compile(&migrated).expect("the migrated project compiles");
        assert_eq!(
            after.program.ops, before.program.ops,
            "{name} compiles to a different program after migration"
        );
        assert_eq!(
            after.program.ops.len(),
            before.program.ops.len(),
            "{name} changed length"
        );
    }
}
