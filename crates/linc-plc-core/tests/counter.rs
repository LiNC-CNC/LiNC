//! The second semantics, tested the way §II.6 says: a naive reference model of
//! the construct, the same input sequence through the real compiler and VM, and
//! the observable pins compared per scan.
//!
//! The reference is `CalcTypeCounter` as this tree records it: `CU`/`CD` step on
//! a rising edge, then `L` loads the preset and `R` resets to zero at level, so
//! a level operand overrides the scan's count and the reset wins over the load.
//! The value wraps at 0..=9999, `done` is equality, and `E`/`F` are `CV == 0` and
//! `CV == 9999`.
//!
//! The order is not a guess. The PLCYard oracle (`/home/jack/CNC/tmp/plcyard`),
//! driven headlessly, was given a count edge and its level operand on the same
//! scan: `CTU` stayed at 0 with `R` true, and `CTD` stayed at its preset with
//! `LD` true — so the level operand is applied after the step and wins. The
//! oracle has one such operand per counter kind (`R` for `CTU`, `LD` for `CTD`),
//! so it cannot rank `R` against `L`; that pair keeps the order the `.clp`
//! importer already documented ("preset load, then the reset: applied in the
//! order `CalcTypeCounter` applies them, so the reset wins"), which is the
//! in-tree authority until ClassicLadder's `calc.c` is in hand. The rung order
//! below is therefore `CU`, `CD`, `L`, `R`, then the block that publishes `Q`.

use linc_plc_core::grid::Cell;
use linc_plc_core::{
    compile, Coil, CoilKind, Dialect, IoDirection, IoPoint, Logic, Pou, PouLanguage, Project, Rung,
    Section, Vm,
};

/// ClassicLadder's counter, as naively as it can be written: no reuse of the
/// compiler, the VM or `profile`.
struct Reference {
    value: i32,
    preset: i32,
    up: bool,
    down: bool,
}

impl Reference {
    fn new(preset: i32) -> Self {
        Self {
            value: 0,
            preset,
            up: false,
            down: false,
        }
    }

    /// One scan. Returns `(done, empty, full)`. The steps run first, then the
    /// level operands, which is the order the oracle settled.
    fn step(&mut self, cu: bool, cd: bool, r: bool, l: bool) -> (bool, bool, bool) {
        if cu && !self.up {
            self.value = if self.value >= 9_999 {
                0
            } else {
                self.value + 1
            };
        }
        if cd && !self.down {
            self.value = if self.value <= 0 {
                9_999
            } else {
                self.value - 1
            };
        }
        self.up = cu;
        self.down = cd;
        if l {
            self.value = self.preset;
        }
        if r {
            self.value = 0;
        }
        (
            self.value == self.preset,
            self.value == 0,
            self.value == 9_999,
        )
    }
}

/// A classicladder project: four input bits drive the counter's member coils,
/// and three output bits read its done, empty and full outputs.
fn classic_project(preset: i32) -> Project {
    let coil = |var: &str, coil: &str| {
        Rung::new(
            Logic::Contact {
                var: var.to_string(),
                negated: false,
            },
            Coil::new(coil, CoilKind::Normal),
        )
    };
    let mut done_rung = Rung::new(
        Logic::Const { value: true },
        Coil::new("done", CoilKind::Normal),
    );
    done_rung.cells = vec![Cell::Block {
        block: "CTU".to_string(),
        instance: "CTU0".to_string(),
        preset: preset.to_string(),
    }];

    let mut project = Project::new("classicladder-counter");
    project.profile = Dialect::ClassicLadder;
    project.io = vec![
        IoPoint::new("cu", IoDirection::In),
        IoPoint::new("cd", IoDirection::In),
        IoPoint::new("r", IoDirection::In),
        IoPoint::new("l", IoDirection::In),
        IoPoint::new("done", IoDirection::Out),
        IoPoint::new("empty", IoDirection::Out),
        IoPoint::new("full", IoDirection::Out),
    ];
    let mut pou = Pou::new("MAIN", PouLanguage::Ld);
    pou.sections = vec![Section {
        name: "main".into(),
        rungs: vec![
            coil("cu", "CTU0.CU"),
            coil("cd", "CTU0.CD"),
            coil("l", "CTU0.L"),
            coil("r", "CTU0.R"),
            done_rung,
            coil("CTU0.E", "empty"),
            coil("CTU0.F", "full"),
        ],
    }];
    project.pous = vec![pou];
    project
}

/// The ABI slot of `CTU0`: the kind's code times the instances per kind. Derived
/// rather than written down, so a table change cannot silently retarget a test.
fn ctu0() -> u16 {
    linc_plc_core::compile::parse_block("CTU", "CTU0", "")
        .expect("CTU0 is an instance")
        .1
}

/// Every one of the sixteen input combinations, in order, each one scan.
fn all_combinations() -> impl Iterator<Item = (bool, bool, bool, bool)> {
    (0u8..16).map(|bits| {
        (
            bits & 0b0001 != 0,
            bits & 0b0010 != 0,
            bits & 0b0100 != 0,
            bits & 0b1000 != 0,
        )
    })
}

/// §II.6's matrix: sixteen combinations, simultaneous edges, and the pins
/// compared per scan against the reference.
#[test]
fn every_input_combination_agrees_with_the_reference() {
    let project = classic_project(5);
    let compiled = compile(&project).expect("a classicladder counter compiles");
    let mut vm = Vm::new(&compiled);
    let mut reference = Reference::new(5);

    for (cu, cd, r, l) in all_combinations() {
        vm.scan(&[cu, cd, r, l]);
        let expected = reference.step(cu, cd, r, l);
        let outputs = vm.outputs();
        assert_eq!(
            (outputs[0], outputs[1], outputs[2]),
            expected,
            "inputs cu={cu} cd={cd} r={r} l={l}: value {} vs {}",
            vm.block_value(ctu0()),
            reference.value
        );
        assert_eq!(
            vm.block_value(ctu0()),
            reference.value,
            "the value itself must agree, not only the outputs"
        );
    }
}

/// What the oracle settled: a count edge and its level operand on the same scan
/// leave the level operand's value, because the level is applied after the step.
/// Without this, an imported program that resets a counter while counting would
/// run one count hot — a one-scan difference the trace comparison would catch.
#[test]
fn a_level_operand_wins_over_the_same_scans_count() {
    let compiled = compile(&classic_project(5)).expect("compile");
    let mut vm = Vm::new(&compiled);
    // Prime the edge memories, then raise the count input and the level operand
    // together: the step must be overwritten.
    vm.scan(&[false, false, false, false]);
    vm.scan(&[true, false, true, false]); // CU edge + R
    assert_eq!(vm.block_value(ctu0()), 0, "the reset wins over the count");

    vm.scan(&[false, false, false, false]);
    vm.scan(&[false, true, false, true]); // CD edge + L
    assert_eq!(
        vm.block_value(ctu0()),
        5,
        "the preset load wins over the count"
    );
}

/// A simultaneous up and down edge is two steps, and both apply.
#[test]
fn simultaneous_edges_are_both_applied() {
    let compiled = compile(&classic_project(5)).expect("compile");
    let mut vm = Vm::new(&compiled);

    // Load the preset, then raise both count inputs on the same scan. One step
    // up and one step down cancel, so the value stays where it was.
    vm.scan(&[false, false, false, true]);
    vm.scan(&[false, false, false, false]);
    assert_eq!(vm.block_value(ctu0()), 5, "the preset load took effect");
    vm.scan(&[true, true, false, false]);
    assert_eq!(
        vm.block_value(ctu0()),
        5,
        "one step up and one step down cancel"
    );
    assert!(
        !vm.outputs()[1],
        "not empty: the value is still the preset, not zero"
    );
}

/// The value wraps at both ends: 9999 + 1 is 0, and 0 - 1 is 9999.
#[test]
fn the_value_wraps_at_both_ends() {
    let compiled = compile(&classic_project(9_999)).expect("compile");
    let mut vm = Vm::new(&compiled);

    // Load the preset, then one up edge: 9999 wraps to 0.
    vm.scan(&[false, false, false, true]);
    assert_eq!(vm.block_value(ctu0()), 9_999);
    vm.scan(&[false, false, false, false]);
    vm.scan(&[true, false, false, false]);
    assert_eq!(vm.block_value(ctu0()), 0, "9999 + 1 wraps to 0");

    // One down edge from 0 wraps to 9999.
    vm.scan(&[false, false, false, false]);
    vm.scan(&[false, true, false, false]);
    assert_eq!(vm.block_value(ctu0()), 9_999, "0 - 1 wraps to 9999");
}

/// `done` is equality here, not `>=`. The iec counter in the same tree says
/// otherwise, and the difference is the whole point of the second semantics.
#[test]
fn done_is_equality_not_at_least() {
    let compiled = compile(&classic_project(2)).expect("compile");
    let mut vm = Vm::new(&compiled);

    // Two up edges: CV = 2 = PV, done.
    for _ in 0..2 {
        vm.scan(&[false, false, false, false]);
        vm.scan(&[true, false, false, false]);
    }
    assert_eq!(vm.block_value(ctu0()), 2);
    assert!(vm.outputs()[0], "done at CV == PV");

    // One more step: CV = 3 > PV. An `iec` CTU would still say done; this
    // dialect says no, and the compiler baked that comparison in.
    vm.scan(&[false, false, false, false]);
    vm.scan(&[true, false, false, false]);
    assert_eq!(vm.block_value(ctu0()), 3);
    assert!(!vm.outputs()[0], "equality, not `>=`");
}

/// The same program under `iec` is the other semantics: `Q = CV >= PV`, stepped
/// by the block's own input edge. Two projects, one document shape.
#[test]
fn the_iec_counter_still_reports_done_at_or_above_the_preset() {
    let mut project = Project::new("iec-counter");
    project.io = vec![
        IoPoint::new("start", IoDirection::In),
        IoPoint::new("done", IoDirection::Out),
    ];
    let mut rung = Rung::new(
        Logic::Const { value: true },
        Coil::new("done", CoilKind::Normal),
    );
    rung.cells = vec![
        Cell::Contact {
            var: "start".to_string(),
            negated: false,
        },
        Cell::Block {
            block: "CTU".to_string(),
            instance: "CTU0".to_string(),
            preset: "2".to_string(),
        },
    ];
    let mut pou = Pou::new("MAIN", PouLanguage::Ld);
    pou.sections = vec![Section {
        name: "main".into(),
        rungs: vec![rung],
    }];
    project.pous = vec![pou];

    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    // The block's input is the contact, so each rising edge counts once.
    for _ in 0..3 {
        vm.scan(&[false]);
        vm.scan(&[true]);
    }
    assert_eq!(vm.block_value(ctu0()), 3);
    assert!(
        vm.outputs()[0],
        "iec stays done while CV >= PV, unlike classicladder"
    );
}

/// Presets at the edges of the range: zero is done and empty at once, one is a
/// single step, and the top is full.
#[test]
fn presets_zero_one_and_the_top_behave_as_the_reference_says() {
    for preset in [0, 1, 9_999] {
        let compiled = compile(&classic_project(preset)).expect("compile");
        let mut vm = Vm::new(&compiled);
        let mut reference = Reference::new(preset);
        for (cu, cd, r, l) in all_combinations().take(8) {
            vm.scan(&[cu, cd, r, l]);
            let expected = reference.step(cu, cd, r, l);
            let outputs = vm.outputs();
            assert_eq!(
                (outputs[0], outputs[1], outputs[2]),
                expected,
                "preset {preset}, cu={cu} cd={cd} r={r} l={l}"
            );
        }
    }
}

/// A member one dialect does not have is refused, not silently addressed.
#[test]
fn the_other_dialects_members_are_not_addressable() {
    let mut project = classic_project(5);
    // `LD` is iec's load spelling; this dialect's is `L`.
    project.pous[0].sections[0].rungs[0].coil_mut().var = "CTU0.LD".to_string();
    let diagnostics = linc_plc_core::validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "unknown_symbol"),
        "`CTU0.LD` is not a classicladder member: {diagnostics:?}"
    );

    // And an iec project cannot address the count coils at all.
    let mut iec = project.clone();
    iec.profile = Dialect::Iec;
    iec.pous[0].sections[0].rungs[2].coil_mut().var = "CTU0.CU".to_string();
    let diagnostics = linc_plc_core::validate(&iec);
    assert!(
        diagnostics.iter().any(|item| item.code == "unknown_symbol"),
        "`CTU0.CU` is not an iec member: {diagnostics:?}"
    );
}

/// A `.LD` loads the preset its block declares, even when the declaration is an
/// IL call in another POU and the load runs before that block ever executes.
#[test]
fn a_load_uses_the_preset_a_block_or_an_il_call_declares() {
    let mut project = Project::new("il-preset");
    project.io = vec![
        IoPoint::new("load", IoDirection::In),
        IoPoint::new("go", IoDirection::In),
    ];
    // MAIN calls nothing, but the importer-style block cell still declares the
    // instance's preset, and the load rung runs before the block.
    let mut main = Pou::new("MAIN", PouLanguage::Ld);
    let mut load_rung = Rung::new(
        Logic::Contact {
            var: "load".to_string(),
            negated: false,
        },
        Coil::new("CTD0.LD", CoilKind::Normal),
    );
    load_rung.cells = vec![Cell::Contact {
        var: "load".to_string(),
        negated: false,
    }];
    main.sections = vec![Section {
        name: "main".into(),
        rungs: vec![load_rung],
    }];
    // A never-called IL POU that declares the same instance's preset: the load
    // must find it, so the collection cannot be limited to executed blocks.
    let mut decl = Pou::new("DECL", PouLanguage::Il);
    decl.il = vec!["CAL CTD0(CD:=go, PV:=7)".to_string()];
    project.pous = vec![main, decl];

    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    let slot = linc_plc_core::compile::parse_block("CTD", "CTD0", "")
        .expect("CTD0")
        .1;
    vm.scan(&[true, false]);
    assert_eq!(
        vm.block_value(slot),
        7,
        "the load took the preset the IL call declares"
    );
}
