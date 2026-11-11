use linc_plc_core::{
    validate, Coil, CoilKind, Diagnostic, IoDirection, IoPoint, Logic, Project, Rung, Section,
    Severity, VarDecl,
};

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|item| item.code.as_str()).collect()
}

fn base_project() -> Project {
    let mut project = Project::new("demo");
    project.io = vec![
        IoPoint::new("start", IoDirection::In),
        IoPoint::new("run", IoDirection::Out),
    ];
    project.vars = vec![VarDecl::new("seal")];
    project.sections = vec![Section {
        name: "main".to_string(),
        rungs: vec![Rung::new(
            Logic::contact("start", false),
            Coil::new("run", CoilKind::Normal),
        )],
    }];
    project
}

#[test]
fn example_validates_without_errors() {
    let diagnostics = validate(&Project::example());
    assert!(
        diagnostics
            .iter()
            .all(|item| item.severity != Severity::Error),
        "{diagnostics:?}"
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn duplicate_symbol_between_io_and_vars_is_caught() {
    let mut project = base_project();
    project.vars.push(VarDecl::new("start"));
    let diagnostics = validate(&project);
    assert!(
        codes(&diagnostics).contains(&"duplicate_symbol"),
        "{diagnostics:?}"
    );
}

#[test]
fn input_cannot_drive_a_coil() {
    let mut project = base_project();
    project.sections[0].rungs[0].coil = Coil::new("start", CoilKind::Normal);
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["coil_target"]);
    assert_eq!(diagnostics[0].path, "sections[0].rungs[0].coil.var");
}

#[test]
fn duplicate_coil_target_in_one_section_is_caught() {
    let mut project = base_project();
    project.sections[0].rungs.push(Rung::new(
        Logic::Const { value: true },
        Coil::new("run", CoilKind::Set),
    ));
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["duplicate_coil"]);
    assert_eq!(diagnostics[0].path, "sections[0].rungs[1].coil.var");
}

#[test]
fn same_coil_target_in_two_sections_is_allowed() {
    let mut project = base_project();
    project.sections.push(Section {
        name: "second".to_string(),
        rungs: vec![Rung::new(
            Logic::contact("seal", false),
            Coil::new("run", CoilKind::Set),
        )],
    });
    let diagnostics = validate(&project);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn unknown_contact_symbol_reports_the_nested_path() {
    let mut project = base_project();
    project.sections[0].rungs[0].logic = Logic::series(vec![
        Logic::contact("start", false),
        Logic::parallel(vec![Logic::contact("ghost", true)]),
    ]);
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["unknown_symbol"]);
    assert_eq!(
        diagnostics[0].path,
        "sections[0].rungs[0].logic.items[1].items[0].var"
    );
}

#[test]
fn unknown_coil_symbol_is_caught() {
    let mut project = base_project();
    project.sections[0].rungs[0].coil = Coil::new("ghost", CoilKind::Normal);
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["unknown_symbol"]);
}

#[test]
fn bad_and_reserved_names_are_caught() {
    let mut project = base_project();
    project.io[0].name = "Start".to_string();
    project.vars[0].name = "fault".to_string();
    project.sections[0].rungs[0].logic = Logic::Const { value: true };
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["bad_name", "reserved_name"]);
    assert_eq!(diagnostics[0].path, "io[0].name");
    assert_eq!(diagnostics[1].path, "vars[0].name");
}

#[test]
fn schema_name_and_sections_are_checked() {
    let mut project = base_project();
    project.schema = 2;
    project.name = String::new();
    project.sections.clear();
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["schema", "name", "no_sections"]);
}

#[test]
fn empty_series_is_a_warning() {
    let mut project = base_project();
    project.sections[0].rungs[0].logic = Logic::Series { items: Vec::new() };
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["empty_series"]);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(diagnostics[0].path, "sections[0].rungs[0].logic");
}

#[test]
fn logic_deeper_than_sixty_four_levels_is_an_error() {
    let mut project = base_project();
    let mut logic = Logic::contact("start", false);
    for _ in 0..64 {
        logic = Logic::series(vec![logic]);
    }
    project.sections[0].rungs[0].logic = logic;
    let diagnostics = validate(&project);
    assert_eq!(codes(&diagnostics), vec!["too_deep"]);
    assert_eq!(diagnostics[0].path, "sections[0].rungs[0].logic");
}

#[test]
fn project_serde_round_trip_keeps_tagged_logic() {
    let project = Project::example();
    let text = serde_json::to_string(&project).expect("serialise");
    assert!(text.contains("\"op\":\"series\""), "{text}");
    assert!(text.contains("\"op\":\"parallel\""), "{text}");
    assert!(text.contains("\"kind\":\"set\""), "{text}");
    let parsed: Project = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(parsed, project);
}

#[test]
fn rung_and_description_defaults_deserialise() {
    let text = r#"{"schema":1,"name":"demo","io":[],"vars":[],"sections":[{"name":"main","rungs":[{"logic":{"op":"const","value":true},"coil":{"var":"x","kind":"normal"}}]}]}"#;
    let project: Project = serde_json::from_str(text).expect("deserialise");
    assert_eq!(project.description, "");
    assert_eq!(project.sections[0].rungs[0].comment, "");
}

#[test]
fn symbol_comments_round_trip_and_default_to_empty() {
    let mut project = Project::new("comments");
    project.io.push(IoPoint::new("start", IoDirection::In));
    project.io[0].comment = "seal bar closed".to_string();
    project.vars.push(VarDecl::new("lamp"));
    project.vars[0].comment = "panel light".to_string();

    let json = serde_json::to_string(&project).expect("json");
    let back: Project = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.io[0].comment, "seal bar closed");
    assert_eq!(back.vars[0].comment, "panel light");

    // A file written before comments existed still decodes.
    let old = r#"{"schema":2,"name":"old","io":[{"name":"a","direction":"in","kind":"bit"}],
        "vars":[{"name":"v","kind":"bit","retain":false}],"sections":[],"pous":[]}"#;
    let decoded: Project = serde_json::from_str(old).expect("old file");
    assert_eq!(decoded.io[0].comment, "");
    assert_eq!(decoded.vars[0].comment, "");
}

/// A row directly under a `Link` is that link's parallel branch: it carries no
/// coil of its own, and the ladder must still validate and compile.
#[test]
fn a_linked_continuation_row_may_have_a_blank_coil() {
    use linc_plc_core::grid::Cell;
    let mut project = base_project();
    project.sections[0].rungs[0].cells = vec![
        Cell::Contact {
            var: "start".into(),
            negated: false,
        },
        Cell::Link,
    ];
    let mut branch = Rung::new(Logic::contact("seal", false), Coil::new("", CoilKind::Normal));
    branch.cells = vec![Cell::Contact {
        var: "seal".into(),
        negated: false,
    }];
    project.sections[0].rungs.push(branch);
    let diagnostics = validate(&project);
    assert!(
        !codes(&diagnostics).contains(&"unknown_symbol"),
        "a branch row's blank coil is not an undeclared symbol: {diagnostics:?}"
    );
    assert!(
        linc_plc_core::compile::compile(&project).is_ok(),
        "the ladder compiles"
    );
}
