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

#[test]
fn a_grid_rung_compiles_like_its_tree() {
    let mut project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Contact {
            var: "b".into(),
            negated: true,
        },
    ];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[true, false]);
    assert_eq!(vm.outputs(), &[true], "a and not b");
    vm.scan(&[true, true]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn an_invalid_block_instance_is_named_by_validation() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![linc_plc_core::Cell::Block {
        block: "TON".into(),
        instance: "TON9".into(),
        preset: "T#1s".into(),
    }];
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "cell_block"),
        "{diagnostics:?}"
    );
}

#[test]
fn il_words_add_and_compare() {
    let mut project = project_with(
        vec![IoPoint::new("y", IoDirection::Out)],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec![
        "LD MW0".into(),
        "ADD 5".into(),
        "ST MW1".into(),
        "LD MW1".into(),
        "GT 9".into(),
        "ST y".into(),
    ];
    project.pous = vec![pou];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 3);
    vm.scan(&[]);
    assert_eq!(vm.word(1), 8, "3 + 5");
    assert_eq!(vm.outputs(), &[false], "8 is not greater than 9");

    vm.set_word(0, 10);
    vm.scan(&[]);
    assert_eq!(vm.word(1), 15);
    assert_eq!(vm.outputs(), &[true], "15 is greater than 9");
}

#[test]
fn a_timer_box_passes_power_after_its_preset() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Block {
            block: "TON".into(),
            instance: "TON0".into(),
            preset: "T#2s".into(),
        },
    ];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan_with_dt(&[true], 1000);
    assert_eq!(vm.outputs(), &[false], "one second is not two");
    vm.scan_with_dt(&[true], 1000);
    assert_eq!(vm.outputs(), &[true], "two seconds elapsed");
    assert_eq!(vm.block_value(0), 2000, "ET stops at PT");

    vm.scan_with_dt(&[false], 1000);
    assert_eq!(vm.outputs(), &[false], "IN low resets the timer");
    assert_eq!(vm.block_value(0), 0);
}

#[test]
fn a_counter_counts_rising_edges() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Block {
            block: "CTU".into(),
            instance: "CTU1".into(),
            preset: "2".into(),
        },
    ];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[true]);
    assert_eq!(vm.block_value(13), 1, "one rising edge");
    assert_eq!(vm.outputs(), &[false]);
    vm.scan(&[true]);
    assert_eq!(vm.block_value(13), 1, "a held input is not another edge");
    vm.scan(&[false]);
    vm.scan(&[true]);
    assert_eq!(vm.block_value(13), 2);
    assert_eq!(vm.outputs(), &[true], "CV reached PV");
}

#[test]
fn a_compare_contact_passes_power_when_true() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![linc_plc_core::Cell::Compare {
        op: "GE".into(),
        left: "MW0".into(),
        right: "5".into(),
    }];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.set_word(0, 4);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[false]);
    vm.set_word(0, 5);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true], "5 is greater than or equal to 5");
}

#[test]
fn a_compare_can_read_a_timer_elapsed_time() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
            IoPoint::new("y2", IoDirection::Out),
        ],
        Vec::new(),
        vec![section(
            "main",
            vec![
                Rung::new(Logic::Const { value: true }, Coil::new("y2", CoilKind::Normal)),
                Rung::new(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal)),
            ],
        )],
    );
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Block {
            block: "TON".into(),
            instance: "TON0".into(),
            preset: "T#10s".into(),
        },
    ];
    project.sections[0].rungs[1].cells = vec![linc_plc_core::Cell::Compare {
        op: "GT".into(),
        left: "TON0.ET".into(),
        right: "T#1s".into(),
    }];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan_with_dt(&[true], 500);
    assert_eq!(vm.outputs(), &[false, false], "ET is under a second");
    vm.scan_with_dt(&[true], 700);
    assert_eq!(vm.outputs(), &[true, false], "ET is 1200 ms, Q is still low");
}

#[test]
fn a_compute_coil_writes_a_word_while_it_has_power() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Compute {
            op: "ADD".into(),
            target: "MW1".into(),
            left: "MW0".into(),
            right: "1".into(),
        },
    ];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.set_word(0, 41);
    vm.scan(&[false]);
    assert_eq!(vm.word(1), 0, "power is low, nothing is written");
    vm.scan(&[true]);
    assert_eq!(vm.word(1), 42);
}

#[test]
fn an_edge_contact_fires_on_one_scan_only() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![linc_plc_core::Cell::Edge {
        var: "a".into(),
        rising: true,
    }];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false], "no edge yet");
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "rising edge");
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[false], "held input is not a new edge");
    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false], "falling edge is not a rising one");
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "the next rising edge fires");
}

#[test]
fn a_falling_edge_contact_is_the_mirror() {
    let mut project = simple_project(Logic::Const { value: true }, Coil::new("y", CoilKind::Normal));
    project.sections[0].rungs[0].cells = vec![linc_plc_core::Cell::Edge {
        var: "a".into(),
        rising: false,
    }];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[false]);
    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[true], "falling edge");
    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn pinning_a_watch_operand_does_not_change_the_program_hash() {
    let project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let before = linc_plc_core::project_hash(&project);
    let mut pinned = project.clone();
    pinned.watch = vec!["a".into(), "MW0".into()];
    assert_eq!(linc_plc_core::project_hash(&pinned), before);
}

#[test]
fn an_fbd_line_nests_blocks_by_naming_their_output() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("b", IoDirection::In),
            IoPoint::new("run", IoDirection::Out),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    pou.fbd = vec![
        linc_plc_core::model::FbdLine {
            output: "run".into(),
            block: "AND".into(),
            inputs: vec!["a".into(), "b".into()],
            preset: String::new(),
            comment: String::new(),
        },
        linc_plc_core::model::FbdLine {
            output: "y".into(),
            block: "NOT".into(),
            inputs: vec!["run".into()],
            preset: String::new(),
            comment: String::new(),
        },
    ];
    project.pous = vec![pou];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true, true]);
    assert_eq!(vm.outputs(), &[true, false], "run = a and b, y = not run");
    vm.scan(&[true, false]);
    assert_eq!(vm.outputs(), &[false, true]);
}

#[test]
fn an_fbd_timer_and_compare_share_the_ladder_machinery() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("slow", IoDirection::Out),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    pou.fbd = vec![
        linc_plc_core::model::FbdLine {
            output: "slow".into(),
            block: "TON0".into(),
            inputs: vec!["a".into()],
            preset: "T#1s".into(),
            comment: String::new(),
        },
        linc_plc_core::model::FbdLine {
            output: "y".into(),
            block: "GT".into(),
            inputs: vec!["TON0.ET".into(), "T#500ms".into()],
            preset: String::new(),
            comment: String::new(),
        },
    ];
    project.pous = vec![pou];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan_with_dt(&[true], 400);
    assert_eq!(vm.outputs(), &[false, false], "under half a second");
    vm.scan_with_dt(&[true], 400);
    assert_eq!(vm.outputs(), &[false, true], "ET passed 800 ms, Q still low");
    vm.scan_with_dt(&[true], 400);
    assert_eq!(vm.outputs(), &[true, true], "ET reached PT");
}

#[test]
fn a_called_pou_runs_where_it_is_called_and_not_on_its_own() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    main.il = vec![
        "LD a".into(),
        "ST y".into(),
        "CAL COUNT".into(),
    ];
    let mut counter = linc_plc_core::Pou::new("COUNT", linc_plc_core::PouLanguage::Il);
    counter.il = vec![
        "LD MW0".into(),
        "ADD 1".into(),
        "ST MW0".into(),
    ];
    project.pous = vec![main, counter];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "MAIN drives y");
    assert_eq!(vm.word(0), 1, "the routine ran once in the call, not as a root");
    vm.scan(&[true]);
    assert_eq!(vm.word(0), 2);
}

#[test]
fn a_call_cycle_is_reported() {
    let mut project = project_with(
        vec![IoPoint::new("y", IoDirection::Out)],
        Vec::new(),
        Vec::new(),
    );
    let mut a = linc_plc_core::Pou::new("A", linc_plc_core::PouLanguage::Il);
    a.il = vec!["CAL B".into(), "LD y".into(), "ST y".into()];
    let mut b = linc_plc_core::Pou::new("B", linc_plc_core::PouLanguage::Il);
    b.il = vec!["CAL A".into(), "LD y".into(), "ST y".into()];
    project.pous = vec![a, b];

    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "cal_cycle"),
        "{diagnostics:?}"
    );
}

#[test]
fn an_il_fb_call_runs_a_timer_with_its_arguments() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec![
        "CAL TON0(IN:=a, PT:=T#1s)".into(),
        "ST y".into(),
        "RET".into(),
    ];
    project.pous = vec![pou];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan_with_dt(&[true], 400);
    assert_eq!(vm.outputs(), &[false], "the timer is still counting");
    vm.scan_with_dt(&[true], 700);
    assert_eq!(vm.outputs(), &[true], "ET passed the preset");
    assert_eq!(vm.block_value(0), 1000);
}

#[test]
fn an_fbd_input_can_read_a_pin_above_it() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("b", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    // Line 1 computes (!a), line 2 is (line 1) AND b: the AND reads a pin
    // rather than a named intermediate.
    pou.fbd = vec![
        linc_plc_core::model::FbdLine {
            output: "y".into(),
            block: "NOT".into(),
            inputs: vec!["a".into()],
            preset: String::new(),
            comment: String::new(),
        },
        linc_plc_core::model::FbdLine {
            output: "y".into(),
            block: "AND".into(),
            inputs: vec!["@1".into(), "b".into()],
            preset: String::new(),
            comment: String::new(),
        },
    ];
    project.pous = vec![pou];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[false, true]);
    assert_eq!(vm.outputs(), &[true], "not a and b");
    vm.scan(&[true, true]);
    assert_eq!(vm.outputs(), &[false], "a is high, so not a is low");
}

#[test]
fn a_forward_pin_reference_is_reported() {
    let mut project = project_with(
        vec![IoPoint::new("a", IoDirection::In), IoPoint::new("y", IoDirection::Out)],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    pou.fbd = vec![linc_plc_core::model::FbdLine {
        output: "y".into(),
        block: "NOT".into(),
        inputs: vec!["@2".into()],
        preset: String::new(),
        comment: String::new(),
    }];
    project.pous = vec![pou];
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "fbd_ref"),
        "{diagnostics:?}"
    );
}
