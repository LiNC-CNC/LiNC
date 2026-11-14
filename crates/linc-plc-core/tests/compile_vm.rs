use linc_plc_core::{
    compile, project_hash, Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Op, PresetSource,
    Project, Rung, Section, SymbolKind, VarDecl, Vm,
};
use linc_plc_core::validate::validate;

fn codes(diagnostics: &[linc_plc_core::Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

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

/// Compute rows execute, and with sequential rungs the second sees the first's
/// result: `MW2 := MW1+5` then `MW2 := MW2+1` gives 6 from MW1 = 0 in one scan.
#[test]
fn compute_rows_run_and_chain_their_words() {
    use linc_plc_core::grid::Cell;
    let compute = |target: &str, left: &str, right: &str| Rung {
        label: String::new(),
        cells: vec![Cell::Compute {
            op: "ADD".to_string(),
            target: target.to_string(),
            left: left.to_string(),
            right: right.to_string(),
        }],
        comment: String::new(),
        logic: Logic::Const { value: true },
        coil: Coil::new("", CoilKind::Normal),
    };
    let project = project_with(
        vec![IoPoint::new("lamp", IoDirection::Out)],
        Vec::new(),
        vec![section(
            "main",
            vec![compute("MW2", "MW1", "5"), compute("MW2", "MW2", "1")],
        )],
    );
    let compiled = compile(&project).expect("compute rows compile");
    let mut vm = Vm::new(&compiled);
    // Sequential rungs: the second compute sees the first's write, so one scan
    // lands on MW1+5+1 = 6, which is what the daemon reports.
    vm.scan(&[]);
    assert_eq!(vm.word(2), 6, "MW1+5 then +1 in the same scan");
    vm.scan(&[]);
    assert_eq!(vm.word(2), 6, "and it settles there");
}

/// A compute rung's coil slot is unused, so a shared coil name there is not a
/// duplicate write; the same name on two ordinary rows still is.
#[test]
fn a_compute_rung_does_not_count_as_a_coil_write() {
    use linc_plc_core::grid::Cell;
    let mut project = project_with(
        vec![
            IoPoint::new("start", IoDirection::In),
            IoPoint::new("run", IoDirection::Out),
        ],
        Vec::new(),
        vec![section(
            "main",
            vec![Rung::new(
                Logic::contact("start", false),
                Coil::new("run", CoilKind::Normal),
            )],
        )],
    );
    let mut compute_row = Rung::new(Logic::Const { value: true }, Coil::new("run", CoilKind::Normal));
    compute_row.cells = vec![Cell::Compute {
        op: "ADD".to_string(),
        target: "MW2".to_string(),
        left: "MW1".to_string(),
        right: "1".to_string(),
    }];
    project.sections[0].rungs.push(compute_row);
    assert!(
        validate(&project).is_empty(),
        "a compute row may reuse the section's coil name: {:?}",
        validate(&project)
    );
    // Two ordinary writes of the same kind are still ambiguous.
    let mut second = Rung::new(Logic::contact("start", false), Coil::new("run", CoilKind::Normal));
    second.cells = Vec::new();
    project.sections[0].rungs.push(second);
    assert!(
        codes(&validate(&project)).iter().any(|code| code == "duplicate_coil"),
        "ordinary duplicate writes are still caught"
    );
}

/// Each block publishes its `Q`, so the editor colours a block from the value the
/// program produced rather than from the rung's power.
#[test]
fn block_outputs_are_published_with_the_snapshot() {
    use linc_plc_core::grid::Cell;
    let mut rung = Rung::new(Logic::Const { value: true }, Coil::new("lamp", CoilKind::Normal));
    rung.cells = vec![
        Cell::Contact {
            var: "start".into(),
            negated: false,
        },
        Cell::Block {
            block: "TON".into(),
            instance: "TON0".into(),
            preset: "T#0ms".into(),
        },
    ];
    let project = project_with(
        vec![
            IoPoint::new("start", IoDirection::In),
            IoPoint::new("lamp", IoDirection::Out),
        ],
        Vec::new(),
        vec![section("main", vec![rung])],
    );
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    for _ in 0..20 {
        vm.scan(&[true]);
    }
    let q = vm
        .snapshot()
        .into_iter()
        .find(|var| var.name == "TON0.Q")
        .expect("the snapshot carries the block output");
    assert!(q.value, "the timer's Q is true once its preset elapses");
}

/// An unconditional jump skips the lines under it; execution resumes at the
/// label.
#[test]
fn a_jump_skips_to_its_label() {
    let mut project = project_with(
        vec![
            IoPoint::new("start", IoDirection::In),
            IoPoint::new("stop", IoDirection::In),
            IoPoint::new("skipped", IoDirection::Out),
            IoPoint::new("out", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec![
        "LD start".into(),
        "JMP SKIP".into(),
        "ST skipped".into(),
        "SKIP:".into(),
        "LD stop".into(),
        "ST out".into(),
    ];
    project.pous = vec![pou];
    let mut vm = Vm::new(&compile(&project).expect("jumps compile"));
    vm.scan(&[true, true]);
    assert_eq!(vm.outputs(), &[false, true], "skipped stays false, out follows stop");
}

/// JMPC jumps on a true result and JMPCN on a false one; the lines under the
/// jump are the path that was not taken.
#[test]
fn conditional_jumps_test_the_result_bit() {
    let program = |jump: &str| {
        let mut project = project_with(
            vec![
                IoPoint::new("test", IoDirection::In),
                IoPoint::new("value", IoDirection::In),
                IoPoint::new("out", IoDirection::Out),
            ],
            Vec::new(),
            Vec::new(),
        );
        let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
        pou.il = vec![
            "LD test".into(),
            format!("{jump} TAKEN"),
            "JMP END".into(),
            "TAKEN:".into(),
            "LD value".into(),
            "ST out".into(),
            "END:".into(),
        ];
        project.pous = vec![pou];
        project
    };
    for (jump, jumps_on_true) in [("JMPC", true), ("JMPCN", false)] {
        for test in [true, false] {
            let mut vm = Vm::new(&compile(&program(jump)).expect("conditional jump compiles"));
            vm.scan(&[test, true]);
            let jumped = test == jumps_on_true;
            assert_eq!(
                vm.outputs(),
                &[jumped],
                "{jump} with test {test} must {} the jump",
                if jumped { "take" } else { "skip" }
            );
        }
    }
}

/// RETC returns from an inlined POU when the result bit is true, RETCN when it
/// is false; the lines under the return do not run.
#[test]
fn conditional_returns_end_a_subroutine_early() {
    let program = |ret: &str| {
        let mut project = project_with(
            vec![
                IoPoint::new("gate", IoDirection::In),
                IoPoint::new("body", IoDirection::In),
                IoPoint::new("out", IoDirection::Out),
            ],
            Vec::new(),
            Vec::new(),
        );
        let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
        main.il = vec!["CAL SUB".into(), "RET".into()];
        let mut sub = linc_plc_core::Pou::new("SUB", linc_plc_core::PouLanguage::Il);
        sub.il = vec![
            "LD gate".into(),
            ret.into(),
            "LD body".into(),
            "ST out".into(),
            "RET".into(),
        ];
        project.pous = vec![main, sub];
        project
    };
    for (ret, returns_on_true) in [("RETC", true), ("RETCN", false)] {
        for gate in [true, false] {
            let mut vm = Vm::new(&compile(&program(ret)).expect("conditional return compiles"));
            vm.scan(&[gate, true]);
            let returned = gate == returns_on_true;
            assert_eq!(
                vm.outputs(),
                &[!returned],
                "{ret} with gate {gate} must {} the body",
                if returned { "skip" } else { "run" }
            );
        }
    }
}

/// CALC runs a POU only when the result bit is true; CALCN only when it is
/// false. The callee's coils are the ones it owns when called.
#[test]
fn conditional_calls_run_the_pou_only_on_their_bit() {
    let program = |call: &str| {
        let mut project = project_with(
            vec![
                IoPoint::new("call_it", IoDirection::In),
                IoPoint::new("body", IoDirection::In),
                IoPoint::new("out", IoDirection::Out),
            ],
            Vec::new(),
            Vec::new(),
        );
        let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
        main.il = vec!["LD call_it".into(), format!("{call} SUB"), "RET".into()];
        let mut sub = linc_plc_core::Pou::new("SUB", linc_plc_core::PouLanguage::Il);
        sub.il = vec!["LD body".into(), "ST out".into(), "RET".into()];
        project.pous = vec![main, sub];
        project
    };
    for (call, runs_on_true) in [("CALC", true), ("CALCN", false)] {
        for call_it in [true, false] {
            let mut vm = Vm::new(&compile(&program(call)).expect("conditional call compiles"));
            vm.scan(&[call_it, true]);
            let ran = call_it == runs_on_true;
            assert_eq!(
                vm.outputs(),
                &[ran],
                "{call} with call_it {call_it} must {} the callee",
                if ran { "run" } else { "skip" }
            );
        }
    }
}

/// A label that no line declares is a validation error, and so is declaring one
/// twice. Jumps may point forward or back within their own POU.
#[test]
fn jump_labels_are_checked() {
    let il_project = |lines: Vec<&str>| {
        let mut project = project_with(Vec::new(), Vec::new(), Vec::new());
        let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
        pou.il = lines.into_iter().map(str::to_string).collect();
        project.pous = vec![pou];
        project
    };
    let missing = il_project(vec!["JMP NOWHERE"]);
    assert!(
        codes(&validate(&missing))
            .iter()
            .any(|code| code == "il_label_missing"),
        "{:?}",
        validate(&missing)
    );
    let duplicate = il_project(vec!["LOOP:", "LOOP:"]);
    assert!(
        validate(&duplicate)
            .iter()
            .any(|item| item.code == "il_label" && item.message.contains("twice")),
        "{:?}",
        validate(&duplicate)
    );
    let backwards = il_project(vec!["LOOP:", "LD start", "JMP LOOP"]);
    assert!(
        !validate(&backwards)
            .iter()
            .any(|item| item.code == "il_label_missing"),
        "{:?}",
        validate(&backwards)
    );
}

/// A jump loop that never ends stops at the op budget instead of freezing the
/// scan.
#[test]
fn an_endless_jump_loop_stops_at_the_budget() {
    let mut project = project_with(Vec::new(), Vec::new(), Vec::new());
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec!["LOOP:".into(), "JMP LOOP".into()];
    project.pous = vec![pou];
    let mut vm = Vm::new(&compile(&project).expect("the loop compiles"));
    let stats = vm.scan(&[]);
    assert_eq!(stats.scan_count, 1, "the scan returned");
}

// --- item 1: the 32-bit word bank and word/float HAL I/O ---------------------

fn point(name: &str, direction: IoDirection, kind: IoKind) -> IoPoint {
    IoPoint {
        name: name.to_string(),
        direction,
        kind,
        comment: String::new(),
    }
}

fn il_project(io: Vec<IoPoint>, il: Vec<&str>) -> Project {
    let mut project = project_with(io, Vec::new(), Vec::new());
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = il.into_iter().map(str::to_string).collect();
    project.pous = vec![pou];
    project
}

/// `T#5m` runs for five minutes. The 16-bit bank used to clamp the preset to
/// 32,767 ms with no diagnostic, so a dwell silently ran for 32.7 s; this is the
/// bug item 1 exists to fix and the assertion is at the exact boundary.
#[test]
fn a_five_minute_timer_is_not_clamped_to_thirty_two_seconds() {
    let project = simple_project(
        Logic::series(vec![
            Logic::contact("a", false),
            Logic::Block {
                block: "TON".to_string(),
                instance: "TON0".to_string(),
                preset: "T#5m".to_string(),
            },
        ]),
        Coil::new("y", CoilKind::Normal),
    );
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    // TON is block kind 0, so TON0 is instance 0.
    let ton = 0;
    for _ in 0..299 {
        vm.scan_with_dt(&[true], 1000);
    }
    assert_eq!(vm.block_value(ton), 299_000, "299 s elapsed");
    assert!(!vm.block_q(ton), "not due at 299 s");
    vm.scan_with_dt(&[true], 1000);
    assert_eq!(vm.block_value(ton), 300_000, "300 s elapsed");
    assert!(vm.block_q(ton), "due at 300 s");
    assert_eq!(vm.outputs(), &[true], "the coil follows Q");
    assert_eq!(vm.block_preset(ton), 300_000, "TON0.PT reports 300,000");
}

#[test]
fn a_forty_second_preset_is_not_clamped() {
    use linc_plc_core::compile::parse_preset;
    assert_eq!(parse_preset("T#40s", true), Some(40_000));
    assert_eq!(parse_preset("T#5m", true), Some(300_000));
    assert_eq!(parse_preset("T#1s500ms", true), Some(1_500));
    // A preset the bank cannot hold is refused, not cut down to fit.
    assert_eq!(parse_preset("T#99000m", true), None);
}

#[test]
fn a_word_holds_values_beyond_the_16_bit_range() {
    let project = il_project(
        Vec::new(),
        vec!["LD 100000", "ST MW0", "LD MW0", "ADD 1", "ST MW1"],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[]);
    assert_eq!(vm.word(0), 100_000);
    assert_eq!(vm.word(1), 100_001, "no wrap at 100,001");
}

#[test]
fn an_overflowing_word_wraps_at_32_bits() {
    let project = il_project(Vec::new(), vec!["LD 2147483647", "ADD 1", "ST MW0"]);
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[]);
    assert_eq!(vm.word(0), i32::MIN, "i32::MAX + 1 wraps");
}

/// A word input is live for a rung on the same scan and a word output reflects
/// the rung's write after it. The slots are part of the ABI: `MW0..MW15`, then
/// word inputs, then word outputs.
#[test]
fn a_word_input_is_live_and_a_word_output_follows_the_scan() {
    let project = il_project(
        vec![
            point("iw0", IoDirection::In, IoKind::Word),
            point("qw0", IoDirection::Out, IoKind::Word),
        ],
        vec!["LD iw0", "ADD 1", "ST qw0"],
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(compiled.word_inputs[0].index, 16, "first word slot after MW");
    assert_eq!(compiled.word_outputs[0].index, 17);
    assert_eq!(compiled.program.words.total(), 18);
    assert_eq!(compiled.program.var_count, 0, "no bit image for word points");

    let mut vm = Vm::new(&compiled);
    let frame = |words: &'static [i32]| linc_plc_core::vm::IoFrame {
        bits: &[],
        words,
        floats: &[],
    };
    vm.scan_with_io(frame(&[41]), 20);
    assert_eq!(vm.word(17), 42, "the rung read the live field value");
    assert_eq!(vm.word_outputs(), &[42]);

    // A second scan with a new field value: the rung reads this scan's input,
    // not the previous scan's.
    vm.scan_with_io(frame(&[9]), 20);
    assert_eq!(vm.word_outputs(), &[10]);

    // The snapshot names the slots the way the project does, so a watch entry
    // for a word point resolves like any other operand.
    let named = vm
        .word_snapshot()
        .into_iter()
        .filter(|word| word.name == "iw0" || word.name == "qw0")
        .map(|word| (word.name, word.value))
        .collect::<Vec<_>>();
    assert_eq!(
        named,
        vec![("iw0".to_string(), 9), ("qw0".to_string(), 10)],
        "word points are named, not called MW16"
    );
}

/// A float pin is an integer view of its slot: the daemon truncates 2.7 to 2
/// before the scan and writes 2 back as 2.0 after it (`hal.rs`), so the VM only
/// carries the integer through the bank.
#[test]
fn a_float_pin_carries_its_integer_view_through_the_bank() {
    let project = il_project(
        vec![
            point("if0", IoDirection::In, IoKind::Float),
            point("qf0", IoDirection::Out, IoKind::Float),
        ],
        vec!["LD if0", "ST qf0"],
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(compiled.float_inputs[0].index, 16);
    assert_eq!(compiled.float_outputs[0].index, 17);
    let mut vm = Vm::new(&compiled);
    vm.scan_with_io(
        linc_plc_core::vm::IoFrame {
            bits: &[],
            words: &[],
            floats: &[2],
        },
        20,
    );
    assert_eq!(vm.float_outputs(), &[2]);
}

#[test]
fn a_word_point_used_as_a_contact_is_refused() {
    let mut project = simple_project(
        Logic::contact("iw0", false),
        Coil::new("y", CoilKind::Normal),
    );
    project
        .io
        .push(point("iw0", IoDirection::In, IoKind::Word));
    let codes = codes(&validate(&project));
    assert!(codes.contains(&"io_kind".to_string()), "{codes:?}");
}

#[test]
fn a_bit_point_used_as_a_word_value_is_refused() {
    let project = simple_project(
        Logic::Compare {
            operator: "GT".to_string(),
            left: "a".to_string(),
            right: "5".to_string(),
        },
        Coil::new("y", CoilKind::Normal),
    );
    let codes = codes(&validate(&project));
    assert!(codes.contains(&"io_kind".to_string()), "{codes:?}");
}

#[test]
fn a_word_bank_over_256_slots_is_refused() {
    let mut project = Project::new("wide");
    project.io = (0..241)
        .map(|index| point(&format!("w{index}"), IoDirection::In, IoKind::Word))
        .collect();
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec!["LD 1".to_string()];
    project.pous = vec![pou];
    // 16 marker words + 241 points = 257 slots.
    assert!(codes(&validate(&project)).contains(&"too_large".to_string()));

    project.io.truncate(240);
    assert!(
        !codes(&validate(&project)).contains(&"too_large".to_string()),
        "16 + 240 = 256 is the last accepted bank"
    );
}

// --- item 3: presets from a word, and preset writes -------------------------

/// A compute (MOVE) coil on a rung of its own. Its coil slot is unused.
fn compute_rung(target: &str, op: &str, left: &str, right: &str) -> Rung {
    Rung {
        label: String::new(),
        cells: vec![linc_plc_core::grid::Cell::Compute {
            op: op.to_string(),
            target: target.to_string(),
            left: left.to_string(),
            right: right.to_string(),
        }],
        comment: String::new(),
        logic: Logic::Const { value: true },
        coil: Coil::new("", CoilKind::Normal),
    }
}

/// `PT:=MW3` makes the preset a live word: the timer follows the bank, and a
/// changed recipe word lands on the next scan instead of a baked literal.
#[test]
fn a_timer_preset_can_come_from_a_word() {
    let project = il_project(
        vec![IoPoint::new("start", IoDirection::In)],
        vec!["CAL TON0(IN:=start, PT:=MW3)"],
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(
        compiled.program.ops[1],
        Op::Fb {
            kind: linc_plc_core::compile::FbKind::Ton,
            instance: 0,
            preset: PresetSource::Word(3),
        },
        "PT:=MW3 is a live source, not a baked default"
    );

    let mut vm = Vm::new(&compiled);
    vm.set_word(3, 5_000);
    for _ in 0..4 {
        vm.scan_with_dt(&[true], 1000);
    }
    assert_eq!(vm.block_value(0), 4_000, "4 s elapsed");
    assert!(!vm.block_q(0), "not due at 4 s");
    vm.scan_with_dt(&[true], 1000);
    assert_eq!(vm.block_value(0), 5_000);
    assert!(vm.block_q(0), "due at the word's 5 s");

    // Off resets the elapsed time; a 3 s recipe then fires at 3 s.
    vm.scan_with_dt(&[false], 1000);
    assert_eq!(vm.block_value(0), 0, "input off resets ET");
    vm.set_word(3, 3_000);
    for _ in 0..2 {
        vm.scan_with_dt(&[true], 1000);
    }
    assert_eq!(vm.block_preset(0), 3_000, "the preset follows the word");
    assert!(!vm.block_q(0), "2 s is not 3 s");
    vm.scan_with_dt(&[true], 1000);
    assert!(vm.block_q(0), "due at the new 3 s");
    assert_eq!(vm.block_value(0), 3_000);
}

/// `TON0.PT := 10_000` from a compute coil. The compare rung reads the preset
/// back through `FbPreset`, so this is the ladder's own read path.
#[test]
fn a_compute_coil_writes_a_block_preset() {
    let write = compute_rung("TON0.PT", "MOVE", "10000", "0");
    let read = Rung::new(
        Logic::Compare {
            operator: "EQ".to_string(),
            left: "TON0.PT".to_string(),
            right: "10000".to_string(),
        },
        Coil::new("y", CoilKind::Normal),
    );
    let project = project_with(
        vec![IoPoint::new("y", IoDirection::Out)],
        Vec::new(),
        vec![section("main", vec![write, read])],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan(&[]);
    assert_eq!(vm.block_preset(0), 10_000);
    assert_eq!(vm.outputs(), &[true], "TON0.PT reads 10,000");
}

/// The widened target is a `WordOperand`, so a compute coil can also drive a
/// declared word point and therefore a word output pin.
#[test]
fn a_compute_coil_can_write_a_word_point() {
    let project = project_with(
        vec![point("qw0", IoDirection::Out, IoKind::Word)],
        Vec::new(),
        vec![section("main", vec![compute_rung("qw0", "MOVE", "MW0", "0")])],
    );
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    vm.set_word(0, 42);
    vm.scan(&[]);
    assert_eq!(vm.word(16), 42, "qw0 is the first word slot after MW");
    assert_eq!(vm.word_outputs(), &[42]);
}

/// A literal preset still bakes in, and a call with no `PT` keeps the block's
/// default.
#[test]
fn a_literal_preset_still_works_and_reports_its_value() {
    let project = il_project(
        vec![IoPoint::new("start", IoDirection::In)],
        vec!["CAL TON0(IN:=start, PT:=T#4s)"],
    );
    let compiled = compile(&project).expect("compile");
    assert_eq!(
        compiled.program.ops[1],
        Op::Fb {
            kind: linc_plc_core::compile::FbKind::Ton,
            instance: 0,
            preset: PresetSource::Const(4_000),
        }
    );
    let mut vm = Vm::new(&compiled);
    for _ in 0..3 {
        vm.scan_with_dt(&[true], 1000);
    }
    assert!(!vm.block_q(0), "not due at 3 s");
    vm.scan_with_dt(&[true], 1000);
    assert!(vm.block_q(0), "due at 4 s");
    assert_eq!(vm.block_preset(0), 4_000);

    let default = il_project(
        vec![IoPoint::new("start", IoDirection::In)],
        vec!["CAL TON0(IN:=start)"],
    );
    let compiled = compile(&default).expect("compile");
    assert_eq!(
        compiled.program.ops[1],
        Op::Fb {
            kind: linc_plc_core::compile::FbKind::Ton,
            instance: 0,
            preset: PresetSource::Const(1_000),
        },
        "no PT keeps the block's one-second default"
    );
}

/// Anything that is not a preset or a declared word is refused by name. A block
/// member is a readable value but not a preset source, and `MW99` is outside
/// the bank.
#[test]
fn a_bad_preset_is_still_refused_by_name() {
    for preset in ["BOGUS", "MW99", "TON1.ET"] {
        let line = format!("CAL TON0(IN:=start, PT:={preset})");
        let project = il_project(
            vec![IoPoint::new("start", IoDirection::In)],
            vec![line.as_str()],
        );
        let diagnostics = validate(&project);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "cal_arg" && item.message.contains(preset)),
            "{preset}: {diagnostics:?}"
        );
    }
}

/// A literal is a value, not a place: a compute coil whose target is a number
/// is refused with its own code, and the operator check does not fire.
#[test]
fn a_compute_coil_refuses_a_literal_target() {
    let project = project_with(
        Vec::new(),
        Vec::new(),
        vec![section("main", vec![compute_rung("5", "MOVE", "MW0", "0")])],
    );
    let codes = codes(&validate(&project));
    assert!(codes.contains(&"compute_target".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"cell_compute".to_string()),
        "the operator is valid: {codes:?}"
    );
}

/// `Op::Fb` stores its own source into the block's preset every execution — the
/// same as before item 3 — so a preset written by a compute coil holds only
/// until that block runs again. A preset that must stay authoritative is the
/// one sourced from a word (`PT:=MW3`), not a one-off write.
#[test]
fn a_block_reasserts_its_own_preset_after_a_write() {
    let timer = Rung::new(
        Logic::series(vec![
            Logic::contact("start", false),
            Logic::Block {
                block: "TON".to_string(),
                instance: "TON0".to_string(),
                preset: "T#4s".to_string(),
            },
        ]),
        Coil::new("y", CoilKind::Normal),
    );
    // The write comes first, so the timer's own op runs after it in this scan.
    let write = compute_rung("TON0.PT", "MOVE", "10000", "0");
    let project = project_with(
        vec![
            IoPoint::new("start", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        vec![section("main", vec![write, timer])],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.scan_with_dt(&[true], 1000);
    assert_eq!(
        vm.block_preset(0),
        4_000,
        "the timer re-asserted its literal after the write"
    );
    assert_eq!(vm.block_value(0), 1_000, "and it counts toward its own 4 s");
}

// --- item 4: jump and call coils --------------------------------------------

/// A rung that stores on every scan, so a skipped one is visible in the image.
fn tap(var: &str) -> Rung {
    Rung::new(Logic::Const { value: true }, Coil::new(var, CoilKind::Normal))
}

fn coil_project(outputs: &[&str], rungs: Vec<Rung>) -> Project {
    project_with(
        outputs
            .iter()
            .map(|name| IoPoint::new(*name, IoDirection::Out))
            .collect(),
        Vec::new(),
        vec![section("main", rungs)],
    )
}

/// A jump over a rung: the rows between the coil and its label never run, so the
/// skipped coil cannot store. Rungs after the label run as usual.
#[test]
fn a_forward_jump_coil_skips_the_rows_it_names() {
    let mut landing = tap("y2");
    landing.label = "END".to_string();
    let project = coil_project(
        &["y0", "y1", "y2"],
        vec![
            tap("y0"),
            Rung::new(
                Logic::Const { value: true },
                Coil::new("END", CoilKind::Jump),
            ),
            tap("y1"),
            landing,
        ],
    );
    let compiled = compile(&project).expect("the jump compiles");
    let mut vm = Vm::new(&compiled);
    vm.scan(&[]);
    assert_eq!(
        vm.outputs(),
        &[true, false, true],
        "y0 stored before the jump, y1 was skipped, y2 is the landing"
    );
}

/// A backward jump loops. A counter in the rung that jumps stops it, and a jump
/// that never stops still returns: the op budget ends the scan instead of
/// hanging the scheduler.
#[test]
fn a_backward_jump_coil_is_bounded_by_the_op_budget() {
    use linc_plc_core::grid::Cell;
    // LOOP: MW0 := MW0 + 1, then jump back while MW0 < 3.
    let mut count = compute_rung("MW0", "ADD", "MW0", "1");
    count.label = "LOOP".to_string();
    let test = Rung {
        label: String::new(),
        cells: vec![Cell::Compare {
            op: "LT".to_string(),
            left: "MW0".to_string(),
            right: "3".to_string(),
        }],
        comment: String::new(),
        logic: Logic::Const { value: true },
        coil: Coil::new("LOOP", CoilKind::Jump),
    };
    let project = project_with(
        Vec::new(),
        Vec::new(),
        vec![section("main", vec![count, test])],
    );
    let mut vm = Vm::new(&compile(&project).expect("the loop compiles"));
    let stats = vm.scan(&[]);
    assert_eq!(vm.word(0), 3, "three passes, then the compare fell through");
    assert_eq!(stats.scan_count, 1);

    // An always-powered backward jump is the runaway case.
    let mut forever = compute_rung("MW1", "ADD", "MW1", "1");
    forever.label = "TOP".to_string();
    let jump = Rung::new(
        Logic::Const { value: true },
        Coil::new("TOP", CoilKind::Jump),
    );
    let project = project_with(
        Vec::new(),
        Vec::new(),
        vec![section("main", vec![forever, jump])],
    );
    let mut vm = Vm::new(&compile(&project).expect("the runaway compiles"));
    let stats = vm.scan(&[]);
    assert_eq!(stats.scan_count, 1, "the scan returned instead of hanging");
}

/// Every refusal a jump or call coil can produce, with the operand named.
#[test]
fn a_jump_or_call_with_a_bad_target_is_refused() {
    let missing = coil_project(
        &["y"],
        vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("NOWHERE", CoilKind::Jump),
        )],
    );
    let diagnostics = validate(&missing);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "jump_target_missing" && item.message.contains("NOWHERE")),
        "{diagnostics:?}"
    );

    let empty = coil_project(
        &["y"],
        vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("", CoilKind::Jump),
        )],
    );
    assert!(
        codes(&validate(&empty)).contains(&"rung_label_missing".to_string()),
        "{:?}",
        validate(&empty)
    );

    let mut first = tap("y");
    first.label = "LOOP".to_string();
    let mut second = tap("y2");
    second.label = "loop".to_string();
    let duplicate = coil_project(&["y", "y2"], vec![first, second]);
    assert!(
        codes(&validate(&duplicate)).contains(&"rung_label_duplicate".to_string()),
        "{:?}",
        validate(&duplicate)
    );

    let bad_call = coil_project(
        &["y"],
        vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("NOPE", CoilKind::Call),
        )],
    );
    let diagnostics = validate(&bad_call);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "call_target_missing" && item.message.contains("NOPE")),
        "{diagnostics:?}"
    );
}

/// A call coil inlines the callee at the coil position and is gated by the
/// rung's power, so an unpowered call runs nothing.
#[test]
fn a_call_coil_inlines_the_pou_and_follows_the_rung() {
    let mut project = project_with(
        vec![
            IoPoint::new("go", IoDirection::In),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        Vec::new(),
    );
    let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Ld);
    main.sections = vec![section(
        "main",
        vec![Rung::new(
            Logic::contact("go", false),
            Coil::new("SUB", CoilKind::Call),
        )],
    )];
    let mut sub = linc_plc_core::Pou::new("SUB", linc_plc_core::PouLanguage::Ld);
    sub.sections = vec![section("body", vec![tap("y")])];
    project.pous = vec![main, sub];

    let compiled = compile(&project).expect("the call compiles");
    assert_eq!(
        compiled
            .program
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Store(_)))
            .count(),
        1,
        "SUB is inlined once and not also run on its own"
    );
    let mut vm = Vm::new(&compiled);
    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false], "an unpowered call runs nothing");
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "the inlined coil stored this scan");
}

/// A ladder recursion is a call cycle like an IL one: refused, not inlined until
/// the depth guard stops it.
#[test]
fn a_recursive_call_coil_is_refused() {
    let mut project = project_with(Vec::new(), Vec::new(), Vec::new());
    let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Ld);
    main.sections = vec![section(
        "main",
        vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("SUB", CoilKind::Call),
        )],
    )];
    let mut sub = linc_plc_core::Pou::new("SUB", linc_plc_core::PouLanguage::Ld);
    sub.sections = vec![section(
        "body",
        vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("MAIN", CoilKind::Call),
        )],
    )];
    project.pous = vec![main, sub];

    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "cal_cycle" && item.message.contains("MAIN")),
        "{diagnostics:?}"
    );
    assert!(compile(&project).is_err(), "a cycle must not compile");
}

/// A compute cell consumes the rung's power and pushes nothing, so a jump or
/// call coil on that row has nothing to test; it is refused rather than dropped.
#[test]
fn a_jump_coil_on_a_compute_row_is_refused() {
    let mut rung = compute_rung("MW0", "ADD", "MW0", "1");
    rung.coil = Coil::new("NOWHERE", CoilKind::Jump);
    let project = project_with(Vec::new(), Vec::new(), vec![section("main", vec![rung])]);
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "cell_compute" && item.message.contains("jump")),
        "{diagnostics:?}"
    );
}

// --- item 5: expressions in compare and compute -----------------------------

fn expr_rung(text: &str, coil: Coil) -> Rung {
    Rung {
        label: String::new(),
        cells: vec![linc_plc_core::grid::Cell::Expr {
            text: text.to_string(),
        }],
        comment: String::new(),
        logic: Logic::Const { value: true },
        coil,
    }
}

fn expr_project(rungs: Vec<Rung>, outputs: &[&str]) -> Project {
    project_with(
        outputs
            .iter()
            .map(|name| IoPoint::new(*name, IoDirection::Out))
            .collect(),
        Vec::new(),
        vec![section("main", rungs)],
    )
}

/// `MW1<2*MW2`: a condition over arithmetic, with `*` above `<`.
#[test]
fn an_expression_compare_is_arithmetic_with_precedence() {
    let project = expr_project(
        vec![expr_rung("MW1<2*MW2", Coil::new("y", CoilKind::Normal))],
        &["y"],
    );
    let compiled = compile(&project).expect("the expression compiles");
    assert!(
        compiled.program.ops.contains(&Op::Mul),
        "`*` lowered: {:?}",
        compiled.program.ops
    );
    let mut vm = Vm::new(&compiled);
    vm.set_word(1, 3);
    vm.set_word(2, 2);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true], "3 < 2*2");
    vm.set_word(2, 1);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[false], "3 < 2 is false");
}

/// `ABS(MW0)=1` for `MW0 = -1`, and not for 2.
#[test]
fn an_expression_abs_is_the_magnitude() {
    let project = expr_project(
        vec![expr_rung("ABS(MW0)=1", Coil::new("y", CoilKind::Normal))],
        &["y"],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, -1);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true], "ABS(-1) = 1");
    vm.set_word(0, 1);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true]);
    vm.set_word(0, 2);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[false], "ABS(2) is not 1");
}

/// `MINI` clamps at the upper bound and `MAXI` at the lower one.
#[test]
fn mini_and_maxi_clamp() {
    let project = expr_project(
        vec![
            Rung {
                label: String::new(),
                cells: Vec::new(),
                comment: String::new(),
                logic: Logic::Const { value: true },
                coil: Coil::compute("MW1=MINI(MW0,5)"),
            },
            Rung {
                label: String::new(),
                cells: Vec::new(),
                comment: String::new(),
                logic: Logic::Const { value: true },
                coil: Coil::compute("MW2=MAXI(MW0,0)"),
            },
        ],
        &[],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 7);
    vm.scan(&[]);
    assert_eq!(vm.word(1), 5, "MINI(7,5) = 5");
    assert_eq!(vm.word(2), 7);
    vm.set_word(0, -3);
    vm.scan(&[]);
    assert_eq!(vm.word(1), -3);
    assert_eq!(vm.word(2), 0, "MAXI(-3,0) = 0");
}

/// `,` is ClassicLadder's AND between comparison groups.
#[test]
fn a_comma_is_an_and_between_comparisons() {
    let project = expr_project(
        vec![expr_rung("MW0>1,MW1<3", Coil::new("y", CoilKind::Normal))],
        &["y"],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    for (a, b, wanted) in [(2, 2, true), (1, 2, false), (2, 3, false)] {
        vm.set_word(0, a);
        vm.set_word(1, b);
        vm.scan(&[]);
        assert_eq!(vm.outputs(), &[wanted], "MW0={a}, MW1={b}");
    }
}

/// `MW3=MW0*2+5` from a compute coil, which writes only while its rung passes.
#[test]
fn a_compute_coil_writes_an_expression_and_follows_its_rung() {
    let project = project_with(
        vec![IoPoint::new("go", IoDirection::In)],
        Vec::new(),
        vec![section(
            "main",
            vec![Rung {
                label: String::new(),
                cells: vec![linc_plc_core::grid::Cell::Contact {
                    var: "go".to_string(),
                    negated: false,
                }],
                comment: String::new(),
                logic: Logic::Const { value: true },
                coil: Coil::compute("MW3=MW0*2+5"),
            }],
        )],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 4);
    vm.scan(&[false]);
    assert_eq!(vm.word(3), 0, "an unpowered compute coil writes nothing");
    vm.scan(&[true]);
    assert_eq!(vm.word(3), 13, "4*2+5");
}

/// The parser bounds its own recursion, because it walks the text before any
/// diagnostic could be produced.
#[test]
fn a_deeply_nested_expression_is_refused() {
    let nested = format!("{}MW0{}", "(".repeat(40), ")".repeat(40));
    let project = expr_project(
        vec![expr_rung(&format!("{nested}=1"), Coil::new("y", CoilKind::Normal))],
        &["y"],
    );
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "expr_depth" && item.message.contains("32")),
        "{diagnostics:?}"
    );
}

/// An unbalanced parenthesis is a syntax error with the byte offset of the `(`.
#[test]
fn an_unbalanced_parenthesis_is_refused_with_an_offset() {
    let project = expr_project(
        vec![expr_rung("(MW0>1", Coil::new("y", CoilKind::Normal))],
        &["y"],
    );
    let diagnostics = validate(&project);
    let error = diagnostics
        .iter()
        .find(|item| item.code == "expr_syntax")
        .unwrap_or_else(|| panic!("{diagnostics:?}"));
    assert!(error.message.contains("never closed"), "{}", error.message);
    assert!(error.message.contains("byte"), "{}", error.message);
}

/// `/0` and `%0` yield 0, like the IL operators they lower to.
#[test]
fn division_and_modulo_by_zero_yield_zero() {
    for (index, text) in ["MW1=MW0/0", "MW2=MW0%0"].into_iter().enumerate() {
        let project = expr_project(
            vec![Rung {
                label: String::new(),
                cells: Vec::new(),
                comment: String::new(),
                logic: Logic::Const { value: true },
                coil: Coil::compute(text),
            }],
            &[],
        );
        let mut vm = Vm::new(&compile(&project).expect("compile"));
        vm.set_word(0, 7);
        vm.scan(&[]);
        let slot = index as u16 + 1;
        assert_eq!(vm.word(slot), 0, "{text}");
    }
}

/// Every expression refusal has its own code, and names what it refused.
#[test]
fn a_bad_expression_is_refused_with_its_own_code() {
    for (text, code, named) in [
        ("NOPE>1", "expr_operand", Some("NOPE")),
        ("MW0&1", "expr_operand", Some("&")),
        ("FROB(MW0)>1", "expr_function", Some("FROB")),
        ("ABS(1,2)>1", "expr_function", Some("ABS")),
        ("MW0+1", "expr_operand", None),
    ] {
        let project = expr_project(
            vec![expr_rung(text, Coil::new("y", CoilKind::Normal))],
            &["y"],
        );
        let diagnostics = validate(&project);
        let error = diagnostics
            .iter()
            .find(|item| item.code == code)
            .unwrap_or_else(|| panic!("{text}: {diagnostics:?}"));
        if let Some(named) = named {
            assert!(error.message.contains(named), "{text}: {}", error.message);
        }
        assert!(error.message.contains("byte"), "{text}: {}", error.message);
    }
}

/// `expr` belongs to a compute coil: an empty one, a stray one, and an
/// assignment to a bit are all refused rather than ignored.
#[test]
fn a_compute_coil_refuses_a_missing_or_misplaced_expression() {
    let mut missing = expr_rung("", Coil::new("", CoilKind::Compute));
    missing.cells = Vec::new();
    let missing = expr_project(vec![missing], &[]);
    assert!(
        codes(&validate(&missing)).contains(&"coil_expr".to_string()),
        "{:?}",
        validate(&missing)
    );

    let mut stray = Coil::new("y", CoilKind::Normal);
    stray.expr = Some("MW0=1".to_string());
    let stray = expr_project(vec![expr_rung("MW0>1", stray)], &["y"]);
    assert!(
        codes(&validate(&stray)).contains(&"coil_expr".to_string()),
        "{:?}",
        validate(&stray)
    );

    let project = project_with(
        vec![
            IoPoint::new("q3", IoDirection::Out),
            IoPoint::new("y", IoDirection::Out),
        ],
        Vec::new(),
        vec![section(
            "main",
            vec![Rung {
                label: String::new(),
                cells: Vec::new(),
                comment: String::new(),
                logic: Logic::Const { value: true },
                coil: Coil::compute("q3=1"),
            }],
        )],
    );
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "expr_operand" && item.message.contains("q3")),
        "{diagnostics:?}"
    );
}

/// A compare as a rung's first cell leaves exactly one bit: the comparison is
/// the power, and a seed `true` would sit on the stack for the rest of the scan.
#[test]
fn a_compare_first_rung_leaves_no_spare_bit() {
    let project = expr_project(
        vec![Rung {
            label: String::new(),
            cells: vec![linc_plc_core::grid::Cell::Compare {
                op: "EQ".to_string(),
                left: "MW0".to_string(),
                right: "0".to_string(),
            }],
            comment: String::new(),
            logic: Logic::Const { value: true },
            coil: Coil::new("y", CoilKind::Normal),
        }],
        &["y"],
    );
    let compiled = compile(&project).expect("compile");
    assert!(
        !compiled.program.ops.contains(&Op::PushConst(true)),
        "unexpected seed: {:?}",
        compiled.program.ops
    );
    let mut vm = Vm::new(&compiled);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true]);
}

/// The tree form of an expression lowers through the same path as a grid cell.
#[test]
fn a_tree_expression_lowers_like_a_grid_cell() {
    let project = project_with(
        vec![IoPoint::new("y", IoDirection::Out)],
        Vec::new(),
        vec![section(
            "main",
            vec![Rung::new(
                Logic::Expr {
                    text: "MW0<2*MW1".to_string(),
                },
                Coil::new("y", CoilKind::Normal),
            )],
        )],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 3);
    vm.set_word(1, 2);
    vm.scan(&[]);
    assert_eq!(vm.outputs(), &[true], "3 < 4");
}

/// The operators the acceptance list does not exercise: `^` (right-associative,
/// above unary minus), `MOY`/`AVG`, and the boolean operators.
#[test]
fn the_remaining_grammar_lowers() {
    let compute = |target: &str, text: &str| Rung {
        label: String::new(),
        cells: Vec::new(),
        comment: String::new(),
        logic: Logic::Const { value: true },
        coil: Coil::compute(format!("{target}={text}")),
    };
    let project = expr_project(
        vec![
            compute("MW1", "MOY(MW0,10)"),
            compute("MW2", "AVG(MW0,10)"),
            compute("MW3", "2^3*5"),
            compute("MW4", "-MW0^2"),
            compute("MW5", "2^3^2"),
        ],
        &[],
    );
    let mut vm = Vm::new(&compile(&project).expect("compile"));
    vm.set_word(0, 4);
    vm.scan(&[]);
    assert_eq!(vm.word(1), 7, "(4+10)/2");
    assert_eq!(vm.word(2), 7, "AVG is MOY");
    assert_eq!(vm.word(3), 40, "2^3*5");
    assert_eq!(vm.word(4), -16, "-(4^2): ^ binds above unary minus");
    assert_eq!(vm.word(5), 512, "2^(3^2): ^ is right-associative");

    for (text, first, second, wanted) in [
        ("MW0>1|MW1>1", 0, 2, true),
        ("MW0>1|MW1>1", 0, 0, false),
        ("MW0>1&MW1>1", 2, 2, true),
        ("MW0>1&MW1>1", 2, 0, false),
        ("NOT (MW0>1)", 1, 0, true),
        ("NOT (MW0>1)", 2, 0, false),
        ("MW0<>1", 0, 0, true),
        ("MW0<=1", 1, 0, true),
        ("MW0>=1", 0, 0, false),
    ] {
        let project = expr_project(
            vec![expr_rung(text, Coil::new("y", CoilKind::Normal))],
            &["y"],
        );
        let mut vm = Vm::new(&compile(&project).expect("compile"));
        vm.set_word(0, first);
        vm.set_word(1, second);
        vm.scan(&[]);
        assert_eq!(vm.outputs(), &[wanted], "{text} at MW0={first}, MW1={second}");
    }
}

/// The op limit is separate from the nesting limit: a flat chain of terms is
/// shallow but lowers to more operations than the stack budget allows.
#[test]
fn an_expression_with_too_many_operations_is_refused() {
    let terms = vec!["MW0"; 300].join("+");
    let project = expr_project(
        vec![Rung {
            label: String::new(),
            cells: Vec::new(),
            comment: String::new(),
            logic: Logic::Const { value: true },
            coil: Coil::compute(format!("MW1={terms}")),
        }],
        &[],
    );
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "expr_depth" && item.message.contains("512")),
        "{diagnostics:?}"
    );
}
