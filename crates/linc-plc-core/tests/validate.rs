//! Every refusal keeps its code, its path and its sentence.
//!
//! M4 moved the instruction rules out of `validate.rs` into `profile/instr.rs`.
//! The validator used to hold no tests of its own — the diagnostics were pinned
//! only where a test happened to need one — so this suite pins the families the
//! move touched: IL lines, grid cells, coils, ladder nodes, and both FBD shapes.
//! A rule that changes its code, its path or its words fails here.

use linc_plc_core::grid::Cell;
use linc_plc_core::model::{FbdCell, FbdGrid, FbdLine, Pou, PouLanguage};
use linc_plc_core::validate::validate;
use linc_plc_core::{
    Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Project, Rung, Section, VarDecl,
};

/// `(code, path)` for every diagnostic, in the order the validator reports them.
fn found(project: &Project) -> Vec<(String, String)> {
    validate(project)
        .into_iter()
        .map(|item| (item.code, item.path))
        .collect()
}

fn codes(project: &Project) -> Vec<String> {
    validate(project)
        .into_iter()
        .map(|item| item.code)
        .collect()
}

fn messages(project: &Project) -> Vec<String> {
    validate(project)
        .into_iter()
        .map(|item| item.message)
        .collect()
}

fn ld_project() -> Project {
    let mut project = Project::new("ld");
    project.io = vec![IoPoint::new("y", IoDirection::Out)];
    project.vars = vec![VarDecl::new("run")];
    let mut pou = Pou::new("MAIN", PouLanguage::Ld);
    pou.sections = vec![Section {
        name: "main".into(),
        rungs: vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("y", CoilKind::Normal),
        )],
    }];
    project.pous = vec![pou];
    project
}

fn il_project(lines: &[&str]) -> Project {
    let mut project = Project::new("il");
    project.vars = vec![VarDecl::new("run")];
    let mut pou = Pou::new("MAIN", PouLanguage::Il);
    pou.il = lines.iter().map(|line| (*line).to_string()).collect();
    project.pous = vec![pou];
    project
}

fn fbd_project(lines: Vec<FbdLine>) -> Project {
    let mut project = Project::new("fbd");
    project.io = vec![IoPoint::new("y", IoDirection::Out)];
    project.vars = vec![VarDecl::new("run")];
    let mut pou = Pou::new("MAIN", PouLanguage::Fbd);
    pou.fbd = lines;
    project.pous = vec![pou];
    project
}

fn fbd_grid(
    blocks: Vec<linc_plc_core::model::FbdBlock>,
    cells: &[(usize, usize, FbdCell)],
) -> FbdGrid {
    let mut grid = FbdGrid {
        columns: FbdGrid::DEFAULT_COLUMNS,
        rows: 1,
        ..FbdGrid::default()
    };
    for (row, col, cell) in cells {
        grid.set_cell(*row, *col, cell.clone());
    }
    grid.blocks = blocks;
    grid
}

fn grid_project(grid: FbdGrid) -> Project {
    let mut project = fbd_project(Vec::new());
    project.pous[0].fbd_grid = Some(grid);
    project
}

fn block(row: usize, col: usize, kind: &str, ins: usize) -> linc_plc_core::model::FbdBlock {
    linc_plc_core::model::FbdBlock {
        row,
        col,
        ins,
        kind: kind.to_string(),
        instance: String::new(),
        preset: String::new(),
    }
}

// --- IL lines ---------------------------------------------------------------

#[test]
fn an_il_line_reports_the_operand_it_cannot_place() {
    let project = il_project(&["LD nowhere"]);
    assert_eq!(
        found(&project),
        vec![("il_operand".to_string(), "pous[0].il[0]".to_string())]
    );
    assert_eq!(
        messages(&project)[0],
        "'nowhere' is not a declared input, output or variable"
    );
}

#[test]
fn an_il_line_reports_a_word_in_a_bit_accumulator() {
    let mut project = il_project(&["LD 5", "AND run"]);
    project.vars = vec![VarDecl::new("run"), VarDecl::new("w")];
    let codes = codes(&project);
    assert!(codes.contains(&"il_type".to_string()), "{codes:?}");
}

#[test]
fn an_il_line_reports_a_jump_to_a_label_that_is_not_there() {
    let project = il_project(&["JMP nowhere"]);
    assert_eq!(
        found(&project),
        vec![("il_label_missing".to_string(), "pous[0].il[0]".to_string())]
    );
}

#[test]
fn an_il_call_reports_a_block_name_that_is_not_an_instance() {
    let project = il_project(&["CAL TON9(IN:=run, PT:=T#1s)"]);
    let codes = codes(&project);
    assert!(codes.contains(&"cal_block".to_string()), "{codes:?}");
}

#[test]
fn an_il_call_reports_an_argument_it_cannot_use() {
    for (line, wanted) in [
        ("CAL TON0(IN:=nowhere, PT:=T#1s)", "is not a declared input"),
        ("CAL TON0(IN:=run, PT:=nowhere)", "is not a valid preset"),
        ("CAL TON0(XX:=run)", "is not an argument"),
    ] {
        let project = il_project(&[line]);
        let diagnostics = validate(&project);
        assert!(
            diagnostics.iter().any(|item| item.code == "cal_arg"
                && item.message.contains(wanted)
                && item.path == "pous[0].il[0]"),
            "{line}: {diagnostics:?}"
        );
    }
}

#[test]
fn an_il_call_reports_a_pou_that_is_not_there() {
    for line in ["CAL NOWHERE", "CALC NOWHERE"] {
        let project = il_project(&[line]);
        assert!(
            codes(&project).contains(&"cal_target".to_string()),
            "{line}: {:?}",
            validate(&project)
        );
    }
}

// --- grid cells -------------------------------------------------------------

fn cell_project(cells: Vec<Cell>, coil: Coil) -> Project {
    let mut project = ld_project();
    project.pous[0].sections[0].rungs = vec![Rung {
        cells,
        ..Rung::new(Logic::Const { value: true }, coil)
    }];
    project
}

/// A block occupies two positions: its own cell, and the cell to its right, where
/// its pins' values are drawn. Refused rather than clipped, so a block never
/// overruns the row.
#[test]
fn a_block_in_the_last_position_is_refused() {
    let mut cells = vec![Cell::Wire; linc_plc_core::grid::POSITIONS - 1];
    cells.push(Cell::Block {
        block: "TON".to_string(),
        instance: "TON0".to_string(),
        preset: "T#1s".to_string(),
    });
    let project = cell_project(cells, Coil::new("y", CoilKind::Normal));
    let diagnostics = validate(&project);
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|item| item.code == "block_span")
        .map(|item| item.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{diagnostics:?}");
    assert!(errors[0].contains("two positions"), "{}", errors[0]);
}

/// The neighbour is the value column, so anything but a wire would have its
/// symbol drawn under the values.
#[test]
fn a_block_beside_something_other_than_a_wire_is_refused() {
    let project = cell_project(
        vec![
            Cell::Block {
                block: "TON".to_string(),
                instance: "TON0".to_string(),
                preset: "T#1s".to_string(),
            },
            Cell::Contact {
                var: "a".to_string(),
                negated: false,
            },
        ],
        Coil::new("y", CoilKind::Normal),
    );
    assert!(
        codes(&project).iter().any(|code| code == "block_span"),
        "{:?}",
        validate(&project)
    );
}

/// A row that ends on the block is a block at the last position, and a block with
/// a wire after it is what the editor and both importers emit.
#[test]
fn a_block_with_a_wire_after_it_is_accepted() {
    let project = cell_project(
        vec![
            Cell::Wire,
            Cell::Block {
                block: "TON".to_string(),
                instance: "TON0".to_string(),
                preset: "T#1s".to_string(),
            },
            Cell::Wire,
        ],
        Coil::new("y", CoilKind::Normal),
    );
    assert!(
        !codes(&project).iter().any(|code| code == "block_span"),
        "{:?}",
        validate(&project)
    );
}

#[test]
fn a_cell_reports_every_kind_it_cannot_compile() {
    for (cell, wanted, text) in [
        (
            Cell::Block {
                block: "TON".to_string(),
                instance: "TON9".to_string(),
                preset: "T#1s".to_string(),
            },
            "cell_block",
            "is not a valid block",
        ),
        (
            Cell::Compare {
                op: "XX".to_string(),
                left: "MW0".to_string(),
                right: "1".to_string(),
            },
            "cell_compare",
            "is not a compare operator",
        ),
        (
            Cell::Compare {
                op: "GT".to_string(),
                left: "nope".to_string(),
                right: "1".to_string(),
            },
            "cell_compare_operand",
            "is not a word",
        ),
        (
            Cell::Compute {
                op: "XX".to_string(),
                target: "MW0".to_string(),
                left: "1".to_string(),
                right: "1".to_string(),
            },
            "cell_compute",
            "is not a compute operator",
        ),
        (
            Cell::Compute {
                op: "MOVE".to_string(),
                target: "5".to_string(),
                left: "1".to_string(),
                right: "".to_string(),
            },
            "compute_target",
            "is not a place to write",
        ),
        (
            Cell::Compute {
                op: "MOVE".to_string(),
                target: "MW0".to_string(),
                left: "nope".to_string(),
                right: "".to_string(),
            },
            "cell_compute_operand",
            "is not a word",
        ),
    ] {
        let project = cell_project(vec![cell], Coil::new("y", CoilKind::Normal));
        let diagnostics = validate(&project);
        assert!(
            diagnostics.iter().any(|item| item.code == wanted
                && item.message.contains(text)
                && item.path == "sections[0].rungs[0].cells[0]"),
            "{wanted}: {diagnostics:?}"
        );
    }
}

#[test]
fn a_contact_cell_reports_an_operand_it_cannot_find() {
    let project = cell_project(
        vec![Cell::Contact {
            var: "nowhere".to_string(),
            negated: false,
        }],
        Coil::new("y", CoilKind::Normal),
    );
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "cell_operand" && item.message.contains("not a declared")),
        "{diagnostics:?}"
    );
}

// --- coils ------------------------------------------------------------------

#[test]
fn a_coil_reports_an_expression_it_does_not_read() {
    let mut coil = Coil::new("y", CoilKind::Normal);
    coil.expr = Some("y = 1".to_string());
    let project = cell_project(Vec::new(), coil);
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "coil_expr" && item.path == "sections[0].rungs[0].coil.expr"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_compute_coil_needs_its_expression_and_its_own_row() {
    let mut coil = Coil::new("", CoilKind::Compute);
    coil.expr = None;
    let project = cell_project(Vec::new(), coil);
    assert!(codes(&project).contains(&"coil_expr".to_string()));

    let compute = Cell::Compute {
        op: "MOVE".to_string(),
        target: "MW0".to_string(),
        left: "1".to_string(),
        right: String::new(),
    };
    let mut coil = Coil::new("", CoilKind::Compute);
    coil.expr = Some("MW0 = 1".to_string());
    let project = cell_project(vec![compute], coil);
    let diagnostics = validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "cell_compute"
            && item
                .message
                .contains("both a compute cell and a compute coil")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_label_coil_reports_a_target_it_cannot_find() {
    for (kind, wanted, text) in [
        (CoilKind::Jump, "rung_label_missing", "needs the label"),
        (CoilKind::Call, "call_target_missing", "needs the name"),
    ] {
        let project = cell_project(Vec::new(), Coil::new("", kind));
        assert!(
            messages(&project).iter().any(|item| item.contains(text)),
            "{wanted}: {:?}",
            validate(&project)
        );
    }
    let project = cell_project(Vec::new(), Coil::new("nowhere", CoilKind::Jump));
    assert!(codes(&project).contains(&"jump_target_missing".to_string()));
    let project = cell_project(Vec::new(), Coil::new("NOPE", CoilKind::Call));
    assert!(codes(&project).contains(&"call_target_missing".to_string()));
}

#[test]
fn a_counter_member_coil_only_belongs_to_a_counter() {
    let project = cell_project(Vec::new(), Coil::new("TON0.R", CoilKind::Normal));
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "coil_target"
                && item.message.contains("only a counter has one")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_coil_on_a_word_point_is_an_operand_side_error() {
    let mut project = cell_project(Vec::new(), Coil::new("level", CoilKind::Normal));
    project.io = vec![
        IoPoint::new("y", IoDirection::Out),
        IoPoint {
            name: "level".to_string(),
            direction: IoDirection::In,
            kind: IoKind::Word,
            comment: String::new(),
        },
    ];
    let diagnostics = validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "io_kind"
            && item
                .message
                .contains("cannot be read as a bit contact or coil")),
        "{diagnostics:?}"
    );
}

// --- ladder nodes -----------------------------------------------------------

#[test]
fn a_ladder_node_reports_a_block_it_cannot_compile() {
    let mut project = ld_project();
    project.pous[0].sections[0].rungs[0].logic = Logic::Block {
        block: "FROB".to_string(),
        instance: "FROB0".to_string(),
        preset: String::new(),
    };
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "unknown_block"
                && item.path == "sections[0].rungs[0].logic.block"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_ladder_node_reports_a_compare_and_its_operands() {
    let mut project = ld_project();
    project.pous[0].sections[0].rungs[0].logic = Logic::Compare {
        operator: "XX".to_string(),
        left: "nope".to_string(),
        right: "1".to_string(),
    };
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "compare_operator" && item.path.ends_with(".operator")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "compare_operand" && item.path.ends_with(".left")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_ladder_contact_reports_a_name_it_cannot_find() {
    let mut project = ld_project();
    project.pous[0].sections[0].rungs[0].logic = Logic::Contact {
        var: "nowhere".to_string(),
        negated: false,
    };
    let diagnostics = validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "unknown_symbol"
            && item.message.contains("contact `nowhere` is not declared")),
        "{diagnostics:?}"
    );
}

// --- FBD, both shapes -------------------------------------------------------

#[test]
fn a_line_fbd_block_reports_what_it_cannot_read_or_write() {
    let mut line = FbdLine::new("nowhere", "AND");
    line.inputs = vec!["nowhere".to_string()];
    let project = fbd_project(vec![line]);
    let diagnostics = validate(&project);
    let reported: Vec<&str> = diagnostics.iter().map(|item| item.code.as_str()).collect();
    assert!(reported.contains(&"fbd_output"), "{diagnostics:?}");
    assert!(reported.contains(&"fbd_input"), "{diagnostics:?}");

    let project = fbd_project(vec![FbdLine::new("y", "FROB")]);
    assert!(codes(&project).contains(&"fbd_block".to_string()));

    let project = fbd_project(vec![FbdLine::new("y", "NOT")]);
    assert!(
        validate(&project)
            .iter()
            .any(|item| item.code == "fbd_arity"),
        "NOT without an input"
    );

    let mut line = FbdLine::new("y", "AND");
    line.inputs = vec!["@7".to_string()];
    let project = fbd_project(vec![line]);
    assert!(codes(&project).contains(&"fbd_ref".to_string()));
}

#[test]
fn a_grid_fbd_block_reports_its_kind_and_its_terminals() {
    let project = grid_project(fbd_grid(vec![block(0, 1, "FROB", 1)], &[]));
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "fbd_block" && item.path == "pous[0].fbd_grid.blocks[0].kind"),
        "{diagnostics:?}"
    );

    let project = grid_project(fbd_grid(
        vec![block(0, 1, "AND", 2)],
        &[(
            0,
            0,
            FbdCell::In {
                operand: "nowhere".to_string(),
            },
        )],
    ));
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "fbd_open" && item.message.contains("no input")),
        "{diagnostics:?}"
    );

    let project = grid_project(fbd_grid(vec![block(0, 1, "GT", 2)], &[]));
    assert!(codes(&project).contains(&"fbd_input".to_string()));
}

#[test]
fn a_grid_fbd_output_names_a_place_to_store() {
    let project = grid_project(fbd_grid(
        vec![block(0, 1, "AND", 1)],
        &[
            (
                0,
                0,
                FbdCell::In {
                    operand: "run".to_string(),
                },
            ),
            (
                0,
                2,
                FbdCell::Out {
                    operand: "nowhere".to_string(),
                },
            ),
        ],
    ));
    let diagnostics = validate(&project);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "fbd_output" && item.path.ends_with(".row")),
        "{diagnostics:?}"
    );
}

// --- the structure that stayed ---------------------------------------------

#[test]
fn structural_refusals_are_still_the_validators_own() {
    // No POU at all.
    let project = Project::new("empty");
    assert!(codes(&project).contains(&"no_sections".to_string()));

    // Two coils writing one target.
    let mut project = ld_project();
    let rung = Rung::new(
        Logic::Const { value: true },
        Coil::new("y", CoilKind::Normal),
    );
    project.pous[0].sections[0].rungs = vec![rung.clone(), rung];
    assert!(codes(&project).contains(&"duplicate_coil".to_string()));

    // A nest deeper than the limit.
    let mut project = ld_project();
    let mut logic = Logic::Const { value: true };
    for _ in 0..80 {
        logic = Logic::Series { items: vec![logic] };
    }
    project.pous[0].sections[0].rungs[0].logic = logic;
    assert!(codes(&project).contains(&"too_deep".to_string()));
}

#[test]
fn the_bank_and_edge_caps_are_the_profiles_and_are_reported_here() {
    let mut project = ld_project();
    project.io = (0..300)
        .map(|index| IoPoint {
            name: format!("w{index}"),
            direction: IoDirection::In,
            kind: IoKind::Word,
            comment: String::new(),
        })
        .collect();
    let diagnostics = validate(&project);
    assert!(
        diagnostics.iter().any(|item| item.code == "too_large"
            && item
                .message
                .contains("marker words plus the word and float points")),
        "{diagnostics:?}"
    );

    let edges = |count: usize| {
        let mut project = cell_project(Vec::new(), Coil::new("y", CoilKind::Normal));
        project.pous[0].sections[0].rungs[0].cells = (0..count)
            .map(|_| Cell::Edge {
                var: "run".to_string(),
                rising: true,
            })
            .collect();
        codes(&project)
    };
    assert!(!edges(32).contains(&"edge_limit".to_string()));
    assert!(edges(33).contains(&"edge_limit".to_string()));
}
