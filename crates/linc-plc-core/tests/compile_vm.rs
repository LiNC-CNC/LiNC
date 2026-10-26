use linc_plc_core::{
    compile, project_hash, Coil, CoilKind, IoDirection, IoPoint, Logic, Op, Project, Rung, Section,
    SymbolKind, VarDecl, Vm,
};

fn project_with(io: Vec<IoPoint>, vars: Vec<VarDecl>, sections: Vec<Section>) -> Project {
    let mut project = Project::new("vm-test");
    project.io = io;
    project.vars = vars;
    project.sections = sections;
    project
}

fn section(name: &str, rungs: Vec<Rung>) -> Section {
    Section {
        name: name.to_string(),
        rungs,
    }
}

fn simple_project(logic: Logic, coil: Coil) -> Project {
    project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("b", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        vec![section("main", vec![Rung::new(logic, coil)])],
    )
}

#[test]
fn series_lowering_is_left_associated() {
    let project = simple_project(
        Logic::series(vec![Logic::contact("a", false), Logic::contact("b", true)]),
        Coil::new("y", CoilKind::Normal),
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(
        compiled.program.ops,
        vec![Op::Push(0), Op::Push(1), Op::Not, Op::And, Op::Store(2)]
    );
    assert_eq!(compiled.program.input_count, 2);
    assert_eq!(compiled.program.output_count, 1);
    assert_eq!(compiled.program.var_count, 3);
    assert_eq!(compiled.inputs[0].name, "a");
    assert_eq!(compiled.outputs[0].index, 2);
    assert!(compiled.warnings.is_empty());
}

#[test]
fn parallel_lowering_and_set_coil() {
    let project = simple_project(
        Logic::parallel(vec![
            Logic::contact("a", false),
            Logic::contact("b", false),
            Logic::contact("a", true),
        ]),
        Coil::new("y", CoilKind::Set),
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(
        compiled.program.ops,
        vec![
            Op::Push(0),
            Op::Push(1),
            Op::Or,
            Op::Push(0),
            Op::Not,
            Op::Or,
            Op::Set(2),
        ]
    );
}

#[test]
fn symbol_order_is_inputs_then_outputs_then_vars() {
    let mut project = Project::new("order");
    project.io = vec![
        IoPoint::new("y", IoDirection::Out),
        IoPoint::new("a", IoDirection::In),
        IoPoint::new("z", IoDirection::Out),
        IoPoint::new("b", IoDirection::In),
    ];
    project.vars = vec![VarDecl::new("m"), VarDecl::new("n")];
    project.sections = vec![section(
        "main",
        vec![Rung::new(
            Logic::contact("a", false),
            Coil::new("y", CoilKind::Normal),
        )],
    )];
    let compiled = compile(&project).expect("compile");
    let names: Vec<&str> = compiled
        .symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect();
    assert_eq!(names, vec!["a", "b", "y", "z", "m", "n"]);
    assert_eq!(compiled.symbols[0].kind, SymbolKind::Input);
    assert_eq!(compiled.symbols[2].kind, SymbolKind::Output);
    assert_eq!(compiled.symbols[4].kind, SymbolKind::Var);
    let inputs: Vec<&str> = compiled
        .inputs
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();
    assert_eq!(inputs, vec!["a", "b"]);
    let outputs: Vec<&str> = compiled
        .outputs
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();
    assert_eq!(outputs, vec!["y", "z"]);
}

#[test]
fn empty_series_and_parallel_compile_to_constants() {
    let series = simple_project(
        Logic::Series { items: Vec::new() },
        Coil::new("y", CoilKind::Normal),
    );
    let compiled = compile(&series).expect("compile");
    assert_eq!(
        compiled.program.ops,
        vec![Op::PushConst(true), Op::Store(2)]
    );
    assert_eq!(compiled.warnings.len(), 1);
    assert_eq!(compiled.warnings[0].code, "empty_series");

    let parallel = simple_project(
        Logic::Parallel { items: Vec::new() },
        Coil::new("y", CoilKind::Normal),
    );
    let compiled = compile(&parallel).expect("compile");
    assert_eq!(
        compiled.program.ops,
        vec![Op::PushConst(false), Op::Store(2)]
    );
}

#[test]
fn compile_rejects_a_project_with_errors() {
    let project = simple_project(
        Logic::contact("ghost", false),
        Coil::new("y", CoilKind::Normal),
    );
    let diagnostics = compile(&project).expect_err("must reject");
    assert!(diagnostics.iter().any(|item| item.code == "unknown_symbol"));
}

#[test]
fn rung_sees_the_previous_scan_image() {
    let project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        vec![VarDecl::new("m")],
        vec![section(
            "main",
            vec![
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("m", CoilKind::Normal),
                ),
                Rung::new(Logic::contact("m", false), Coil::new("y", CoilKind::Normal)),
            ],
        )],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false]);
    assert_eq!(vm.get("m"), Some(true));

    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[true]);
}

#[test]
fn contacts_see_current_inputs_in_the_same_scan() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[true, false]);
    assert_eq!(vm.outputs(), &[true]);
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[false]);
    vm.scan(&[false, true]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn forced_input_changes_the_logic_in_the_same_scan() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[false]);

    vm.set_force("a", true).expect("force a");
    vm.scan(&[false, false]);
    assert_eq!(
        vm.outputs(),
        &[true],
        "a forced input must be seen this scan"
    );
    assert_eq!(vm.get("a"), Some(true));

    vm.clear_forces();
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn forced_output_is_not_overwritten_by_a_false_coil() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.set_force("y", true).expect("force y");
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[true]);
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[true]);

    let snapshot = vm.snapshot();
    let y = snapshot.iter().find(|item| item.name == "y").expect("y");
    assert!(y.value);
    assert!(y.forced);
}

#[test]
fn set_and_reset_hold_across_scans() {
    let project = project_with(
        vec![
            IoPoint::new("on", IoDirection::In),
            IoPoint::new("off", IoDirection::In),
        ],
        vec![VarDecl::new("m")],
        vec![
            section(
                "set",
                vec![Rung::new(
                    Logic::contact("on", false),
                    Coil::new("m", CoilKind::Set),
                )],
            ),
            section(
                "reset",
                vec![Rung::new(
                    Logic::contact("off", false),
                    Coil::new("m", CoilKind::Reset),
                )],
            ),
        ],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[true, false]);
    assert_eq!(vm.get("m"), Some(true));
    vm.scan(&[false, false]);
    assert_eq!(
        vm.get("m"),
        Some(true),
        "Set must hold while its logic is false"
    );
    vm.scan(&[false, true]);
    assert_eq!(vm.get("m"), Some(false));
    vm.scan(&[false, false]);
    assert_eq!(
        vm.get("m"),
        Some(false),
        "Reset must hold while its logic is false"
    );
}

#[test]
fn set_does_not_clear_a_true_slot() {
    let project = project_with(
        vec![IoPoint::new("a", IoDirection::In)],
        vec![VarDecl::new("m")],
        vec![
            section(
                "set",
                vec![Rung::new(
                    Logic::Const { value: true },
                    Coil::new("m", CoilKind::Set),
                )],
            ),
            section(
                "reset",
                vec![Rung::new(
                    Logic::contact("a", false),
                    Coil::new("m", CoilKind::Reset),
                )],
            ),
        ],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[false]);
    assert_eq!(vm.get("m"), Some(true));
    vm.scan(&[false]);
    assert_eq!(vm.get("m"), Some(true));
}

#[test]
fn forces_override_the_scan_image_and_are_reported() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);

    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[false]);

    vm.set_force("y", true).expect("force y");
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[true]);
    assert_eq!(vm.get("y"), Some(true));
    assert_eq!(vm.forced(), vec!["y".to_string()]);

    let snapshot = vm.snapshot();
    let y = snapshot.iter().find(|item| item.name == "y").expect("y");
    assert!(y.forced);
    assert!(y.value);

    vm.clear_forces();
    assert!(vm.forced().is_empty());
    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn forcing_an_unknown_symbol_fails() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    assert!(vm.set_force("ghost", true).is_err());
}

#[test]
fn reset_clears_images_and_forces_but_not_the_counter() {
    let project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    let stats = vm.scan(&[false, false]);
    assert_eq!(stats.scan_count, 1);
    assert_eq!(vm.outputs(), &[true]);
    vm.set_force("y", false).expect("force y");

    vm.reset();
    assert_eq!(vm.get("y"), Some(false));
    assert!(vm.forced().is_empty());

    let stats = vm.scan(&[false, false]);
    assert_eq!(stats.scan_count, 2);
    assert_eq!(vm.outputs(), &[true]);
}

#[test]
fn scan_stats_count_scans() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    let first = vm.scan(&[true, false]);
    assert_eq!(first.scan_count, 1);
    let second = vm.scan(&[true, false]);
    assert_eq!(second.scan_count, 2);
    assert!(second.scan_us <= u32::MAX);
}

#[test]
fn example_program_latches_and_reports() {
    let compiled = compile(&Project::example()).expect("example compiles");
    let mut vm = Vm::new(&compiled);

    let drive = |vm: &mut Vm, start: bool, stop: bool| {
        vm.scan(&[start, stop]);
        (vm.outputs().to_vec(), vm.get("seal"), vm.get("alarm"))
    };

    // Start held: the seal picks up and the run output follows next scan.
    assert_eq!(
        drive(&mut vm, true, false),
        (vec![false], Some(true), Some(false))
    );
    assert_eq!(
        drive(&mut vm, true, false),
        (vec![true], Some(true), Some(false))
    );
    // Released: the seal holds the run output.
    assert_eq!(
        drive(&mut vm, false, false),
        (vec![true], Some(true), Some(false))
    );
    // Stop breaks the seal immediately; run drops a scan later, alarm latches.
    assert_eq!(
        drive(&mut vm, false, true),
        (vec![true], Some(false), Some(true))
    );
    assert_eq!(
        drive(&mut vm, false, false),
        (vec![false], Some(false), Some(true))
    );
}

#[test]
fn project_hash_is_stable_and_sixteen_hex_digits() {
    let project = Project::example();
    let hash = project_hash(&project);
    assert_eq!(hash.len(), 16);
    assert!(hash
        .chars()
        .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    assert_eq!(hash, project_hash(&Project::example()));

    let mut other = Project::example();
    other.name = "other".to_string();
    assert_ne!(hash, project_hash(&other));
    assert_eq!(
        compile(&project).expect("compile").hash,
        project_hash(&project)
    );
}
