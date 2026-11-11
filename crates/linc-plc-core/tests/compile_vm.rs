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
fn a_later_rung_sees_a_coil_write_in_the_same_scan() {
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
    // The first rung sets `m`; the second reads it in the same scan. Only the
    // external input image stays latched for the whole scan.
    assert_eq!(vm.outputs(), &[true]);
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

    // Start held: the seal picks up and the run output follows in the same scan,
    // because a coil write is visible to the rungs after it.
    assert_eq!(
        drive(&mut vm, true, false),
        (vec![true], Some(true), Some(false))
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
    // Stop breaks the seal immediately; the run output drops in the same scan and
    // the falling edge of run latches the alarm in that scan.
    assert_eq!(
        drive(&mut vm, false, true),
        (vec![false], Some(false), Some(true))
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("y2", CoilKind::Normal),
                ),
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("y", CoilKind::Normal),
                ),
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
    assert_eq!(
        vm.outputs(),
        &[true, false],
        "ET is 1200 ms, Q is still low"
    );
}

#[test]
fn a_compute_coil_writes_a_word_while_it_has_power() {
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    let mut project = simple_project(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
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
    assert_eq!(
        vm.outputs(),
        &[false, true],
        "ET passed 800 ms, Q still low"
    );
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
    main.il = vec!["LD a".into(), "ST y".into(), "CAL COUNT".into()];
    let mut counter = linc_plc_core::Pou::new("COUNT", linc_plc_core::PouLanguage::Il);
    counter.il = vec!["LD MW0".into(), "ADD 1".into(), "ST MW0".into()];
    project.pous = vec![main, counter];

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "MAIN drives y");
    assert_eq!(
        vm.word(0),
        1,
        "the routine ran once in the call, not as a root"
    );
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
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
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

#[test]
fn only_main_runs_and_an_uncalled_pou_is_reported() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
            IoPoint::new("z", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    main.il = vec!["LD a".into(), "ST y".into()];
    let mut spare = linc_plc_core::Pou::new("SPARE", linc_plc_core::PouLanguage::Il);
    spare.il = vec!["LD a".into(), "ST z".into()];
    project.pous = vec![main, spare];

    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "pou_unreachable"),
        "an uncalled POU is a warning: {diagnostics:?}"
    );

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true, false], "only MAIN ran");
}

#[test]
fn an_sr_latch_holds_and_an_rt_block_pulses() {
    let mut project = project_with(
        vec![
            IoPoint::new("set", IoDirection::In),
            IoPoint::new("reset", IoDirection::In),
            IoPoint::new("clk", IoDirection::In),
            IoPoint::new("held", IoDirection::Out),
            IoPoint::new("pulse", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    pou.fbd = vec![
        linc_plc_core::model::FbdLine {
            output: "held".into(),
            block: "SR0".into(),
            inputs: vec!["set".into(), "reset".into()],
            preset: String::new(),
            comment: String::new(),
        },
        linc_plc_core::model::FbdLine {
            output: "pulse".into(),
            block: "RT0".into(),
            inputs: vec!["clk".into()],
            preset: String::new(),
            comment: String::new(),
        },
    ];
    project.pous = vec![pou];
    let mut vm = Vm::new(&compile(&project).expect("compile"));

    // held = SR(set, reset); pulse = rising edge of clk.
    vm.scan(&[true, false, false]);
    assert_eq!(vm.outputs(), &[true, false], "S1 sets the latch");
    vm.scan(&[false, false, true]);
    assert_eq!(vm.outputs(), &[true, true], "the latch holds, clk rises");
    vm.scan(&[false, false, true]);
    assert_eq!(
        vm.outputs(),
        &[true, false],
        "a held clk is not another edge"
    );
    vm.scan(&[false, true, true]);
    assert_eq!(vm.outputs(), &[false, false], "R resets the latch");
}

#[test]
fn a_counter_resets_from_a_coil_and_its_pv_can_be_read() {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("r", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
            IoPoint::new("below".to_string(), IoDirection::Out),
        ],
        Vec::new(),
        vec![section(
            "main",
            vec![
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("y", CoilKind::Normal),
                ),
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("CTU0.R", CoilKind::Normal),
                ),
                Rung::new(
                    Logic::Const { value: true },
                    Coil::new("below", CoilKind::Normal),
                ),
            ],
        )],
    );
    project.sections[0].rungs[0].cells = vec![
        linc_plc_core::Cell::Contact {
            var: "a".into(),
            negated: false,
        },
        linc_plc_core::Cell::Block {
            block: "CTU".into(),
            instance: "CTU0".into(),
            preset: "5".into(),
        },
    ];
    project.sections[0].rungs[1].cells = vec![linc_plc_core::Cell::Contact {
        var: "r".into(),
        negated: false,
    }];
    project.sections[0].rungs[2].cells = vec![linc_plc_core::Cell::Compare {
        op: "LT".into(),
        left: "CTU0.CV".into(),
        right: "CTU0.PV".into(),
    }];
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics
            .iter()
            .all(|item| item.severity != linc_plc_core::Severity::Error),
        "{diagnostics:?}"
    );

    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true, false]);
    vm.scan(&[false, false]);
    vm.scan(&[true, false]);
    assert_eq!(vm.block_value(12), 2, "two rising edges");
    assert_eq!(vm.outputs(), &[false, true], "below PV");
    vm.scan(&[false, true]);
    assert_eq!(vm.block_value(12), 0, "the reset coil cleared CV");
}

#[test]
fn a_comment_row_compiles_to_nothing() {
    let mut project = simple_project(Logic::contact("a", false), Coil::new("y", CoilKind::Normal));
    let mut comment = Rung::new(
        Logic::Const { value: true },
        Coil::new(String::new(), CoilKind::Normal),
    );
    comment.cells = vec![linc_plc_core::Cell::Wire; 9];
    comment.comment = "// starts the pump".into();
    project.sections[0].rungs.insert(0, comment);

    assert!(project.sections[0].rungs[0].is_comment_row());
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics
            .iter()
            .all(|item| item.severity != linc_plc_core::Severity::Error),
        "{diagnostics:?}"
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "the comment row changed nothing");
}

fn fbd_project(lines: Vec<linc_plc_core::model::FbdLine>) -> Project {
    let mut project = project_with(
        vec![
            IoPoint::new("a", IoDirection::In),
            IoPoint::new("b", IoDirection::In),
            IoPoint::new("clk", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
            IoPoint::new("z", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Fbd);
    pou.fbd = lines;
    project.pous = vec![pou];
    project
}

fn line(output: &str, block: &str, inputs: &[&str], preset: &str) -> linc_plc_core::model::FbdLine {
    linc_plc_core::model::FbdLine {
        output: output.into(),
        block: block.into(),
        inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
        preset: preset.into(),
        comment: String::new(),
    }
}

#[test]
fn every_fbd_line_project_migrates_to_the_grid_with_the_same_ops() {
    let cases = vec![
        // Nested gates through a named output.
        vec![
            line("y", "AND", &["a", "b"], ""),
            line("z", "NOT", &["y"], ""),
        ],
        // A timer feeding a compare, plus a negated input.
        vec![
            line("y", "TON0", &["a"], "T#1s"),
            line("z", "GT", &["TON0.ET", "T#500ms"], ""),
        ],
        // A latch and an edge block.
        vec![
            line("y", "SR0", &["a", "b"], ""),
            line("z", "RT0", &["clk"], ""),
        ],
        // A pin reference instead of a named intermediate.
        vec![
            line("y", "NOT", &["a"], ""),
            line("z", "AND", &["@1", "b"], ""),
        ],
    ];

    for lines in cases {
        let mut from_lines = fbd_project(lines.clone());
        let lines_ops = compile(&from_lines).expect("line compile").program.ops;

        let grid = linc_plc_core::model::FbdGrid::from_lines(&lines);
        from_lines.pous[0].fbd_grid = Some(grid);
        let grid_ops = compile(&from_lines).expect("grid compile").program.ops;

        assert_eq!(
            grid_ops, lines_ops,
            "the grid must compile to exactly the ops the lines did for {lines:?}"
        );
    }
}

#[test]
fn a_grid_block_chains_through_the_cell_to_its_left() {
    use linc_plc_core::model::{FbdBlock, FbdCell, FbdGrid};
    let mut grid = FbdGrid {
        columns: 6,
        rows: 2,
        cells: Vec::new(),
        blocks: Vec::new(),
    };
    // Row 0: a AND b, its output on the wire; row 1: NOT of that wire.
    grid.set_cell(
        0,
        1,
        FbdCell::In {
            operand: "a".into(),
        },
    );
    grid.set_cell(
        1,
        1,
        FbdCell::In {
            operand: "b".into(),
        },
    );
    grid.set_cell(
        0,
        3,
        FbdCell::Out {
            operand: "y".into(),
        },
    );
    grid.blocks.push(FbdBlock {
        row: 0,
        col: 2,
        ins: 2,
        kind: "AND".into(),
        instance: String::new(),
        preset: String::new(),
    });
    // Second network: a NOT whose input cell is empty next to the first
    // block's output row, which is the chaining case.
    grid.set_cell(
        0,
        5,
        FbdCell::Out {
            operand: "z".into(),
        },
    );
    grid.blocks.push(FbdBlock {
        row: 0,
        col: 4,
        ins: 1,
        kind: "NOT".into(),
        instance: String::new(),
        preset: String::new(),
    });

    let mut project = fbd_project(Vec::new());
    project.pous[0].fbd_grid = Some(grid);
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics
            .iter()
            .all(|item| item.severity != linc_plc_core::Severity::Error),
        "{diagnostics:?}"
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[true, false, false]);
    assert_eq!(vm.outputs(), &[false, true], "z is not (a and b)");
    vm.scan(&[true, true, false]);
    assert_eq!(vm.outputs(), &[true, false], "z is not (a and b)");
}

#[test]
fn a_tree_rung_can_hold_a_block_an_edge_and_a_compare() {
    let logic = Logic::series(vec![
        Logic::contact("a", false),
        Logic::Block {
            block: "TON".into(),
            instance: "TON0".into(),
            preset: "T#1s".into(),
        },
        Logic::Edge {
            var: "b".into(),
            rising: true,
        },
    ]);
    let mut project = simple_project(logic, Coil::new("y", CoilKind::Normal));
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan_with_dt(&[true, false], 400);
    assert_eq!(vm.outputs(), &[false], "the timer is still counting");
    vm.scan_with_dt(&[true, false], 700);
    assert_eq!(vm.outputs(), &[false], "Q is on but b has not risen");
    vm.scan_with_dt(&[true, true], 20);
    assert_eq!(vm.outputs(), &[true], "b rose");

    // A compare in a series behaves like a contact.
    project.sections[0].rungs[0].logic = Logic::series(vec![Logic::Compare {
        operator: "GE".into(),
        left: "MW0".into(),
        right: "5".into(),
    }]);
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 5);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true]);
}

/// The pump latch as the grid draws it: a `Link` at position 1 puts the row below
/// in parallel with position 0 only, so `/estop` stays in series with the whole
/// branch and must drop the run output.
#[test]
fn a_linked_branch_parallels_one_cell_and_not_the_estop() {
    use linc_plc_core::grid::Cell;
    let contact = |name: &str, negated: bool| Cell::Contact {
        var: name.to_string(),
        negated,
    };
    let mut latch = Rung::new(
        Logic::contact("pump", false),
        Coil::new("run", CoilKind::Normal),
    );
    latch.cells = vec![contact("pump", false), Cell::Link, contact("estop", true)];
    let mut branch = Rung::new(
        Logic::contact("run", false),
        Coil::new("", CoilKind::Normal),
    );
    branch.cells = vec![contact("run", false)];
    let project = project_with(
        vec![
            IoPoint::new("pump", IoDirection::In),
            IoPoint::new("estop", IoDirection::In),
            IoPoint::new("run", IoDirection::Out),
        ],
        Vec::new(),
        vec![section("main", vec![latch, branch])],
    );
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);

    // inputs are slots 0 (pump) and 1 (estop); the coil is slot 2.
    vm.scan(&[true, false]);
    assert_eq!(vm.outputs(), &[true], "pump seals the run on");

    vm.scan(&[false, false]);
    assert_eq!(vm.outputs(), &[true], "the branch holds the run on");

    vm.scan(&[false, true]);
    assert_eq!(vm.outputs(), &[false], "the estop breaks the branch");
}

/// Two cross-coupled `TP` blocks make the classic blinker: exactly one output is
/// on at a time and they swap about once per preset, never both steady.
#[test]
fn cross_coupled_tp_blocks_alternate() {
    use linc_plc_core::grid::Cell;
    let contact = |name: &str, negated: bool| Cell::Contact {
        var: name.to_string(),
        negated,
    };
    let block = |instance: &str| Cell::Block {
        block: "TP".to_string(),
        instance: instance.to_string(),
        preset: "T#1s".to_string(),
    };
    let mut first = Rung::new(
        Logic::Const { value: true },
        Coil::new("blink_a", CoilKind::Normal),
    );
    first.cells = vec![
        contact("alarm", false),
        contact("blink_b", true),
        block("TP0"),
    ];
    let mut second = Rung::new(
        Logic::Const { value: true },
        Coil::new("blink_b", CoilKind::Normal),
    );
    second.cells = vec![
        contact("alarm", false),
        contact("blink_a", true),
        block("TP1"),
    ];
    let project = project_with(
        vec![
            IoPoint::new("alarm", IoDirection::In),
            IoPoint::new("blink_a", IoDirection::Out),
            IoPoint::new("blink_b", IoDirection::Out),
        ],
        Vec::new(),
        vec![section("main", vec![first, second])],
    );
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);

    vm.scan_with_dt(&[false], 1);
    let idle = vm.outputs().to_vec();
    assert_eq!(idle, vec![false, false], "no alarm, no blinkers: {idle:?}");

    // alarm on: TP0 rises, so blink_a starts and blink_b must stay off.
    vm.scan(&[true]);
    let first_scan = vm.outputs().to_vec();
    assert_eq!(
        first_scan,
        vec![true, false],
        "exactly one blinker starts: {first_scan:?}"
    );

    // Half a second later the pulse is still running: no swap yet.
    vm.scan_with_dt(&[true], 500);
    let mid = vm.outputs().to_vec();
    assert_eq!(mid, vec![true, false], "still the first half: {mid:?}");

    // Past the preset TP0 falls and TP1 takes over.
    vm.scan_with_dt(&[true], 700);
    let swapped = vm.outputs().to_vec();
    assert_eq!(
        swapped,
        vec![false, true],
        "past T#1s the blinkers swap: {swapped:?}"
    );
}
