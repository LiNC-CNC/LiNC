//! Instruction rules: what one instruction may say, in this dialect.
//!
//! M4's split. `validate` owns the document and its structure — sections, POU
//! names, labels, call cycles, duplicate coils, nesting depth, the 256-slot bank
//! cap's *report* — and walks the program. This module owns the rules that are
//! about instructions: which operand a mnemonic takes, which side of the image a
//! name lives on, which block a name names, what a compute may write.
//!
//! The validator walks, this answers. Diagnostics, codes, paths and message text
//! are the ones the inline checks produced before the split, in the same order,
//! so every refusal's test still pins the same words.

use std::collections::{HashMap, HashSet};

use crate::diagnostic::Diagnostic;
use crate::grid::Cell;
use crate::il::parse_call;
use crate::model::{CoilKind, FbdBlock, FbdGrid, FbdLine, IoKind, Logic, Rung};
use crate::profile::resolve::{self, marker_range};
use crate::profile::spec::{self, IlForm};
use crate::profile::SymbolLookup;

/// The project facts one instruction check may need. References only: the
/// validator owns them and lends them for the length of one call.
pub struct Ctx<'a> {
    /// Where this instruction is, e.g. `pous[0].il[3]` or
    /// `sections[0].rungs[1].cells[2]`. Child paths are built from it.
    pub path: &'a str,
    /// Declared input, output and variable names, lowercased.
    pub known: &'a HashSet<String>,
    /// Declared word and float points, lowercased, by bank slot.
    pub word_slots: &'a SymbolLookup,
    /// Declared I/O points and their kinds, by name.
    pub io_kinds: &'a HashMap<&'a str, IoKind>,
    /// Declared variables and their types, by lowercased name: what a contact may
    /// not read and a word operand may.
    pub var_types: &'a HashMap<String, crate::model::VarType>,
    /// The typed declarations that own bank slots: what a bit-of-word operand
    /// (`Bits[0].0`) resolves its element and its width against.
    pub word_vars: &'a [crate::profile::layout::WordVar],
    /// Declared arrays of structs: a preset may name one field of one element.
    pub structs: &'a [crate::profile::layout::StructLayout],
    /// Declared instances, lowercased name to the function block's upper-cased name:
    /// what a call may target, and what its pins must exist on.
    pub instances: &'a HashMap<String, String>,
    /// Each function block's own variables, by upper-cased POU name.
    pub fb_vars: &'a HashMap<String, &'a [crate::model::VarDecl]>,
    /// Every POU name, uppercased: what a call may name.
    pub pou_names: &'a HashSet<String>,
    /// The rungs of the section this instruction is in: what a jump may name.
    pub section_rungs: &'a [Rung],
    /// Labels declared in this POU, uppercased: what an IL jump may name.
    pub labels: &'a HashSet<String>,
    /// The FBD grid, when the instruction is a block drawn in one.
    pub grid: Option<&'a FbdGrid>,
    /// The dialect the project declares. Validation runs before the scan, so
    /// asking the profile here is compile-time, not the scan loop.
    pub profile: &'a dyn crate::profile::Profile,
}

/// One instruction, as the validator finds it in the model.
pub enum Instr<'a> {
    /// One cell of a ladder grid.
    Cell(&'a Cell),
    /// One node of a ladder tree.
    Logic(&'a Logic),
    /// The `expr` field of a coil kind that does not read it.
    CoilExpr {
        kind: CoilKind,
        var: &'a str,
        expr: &'a str,
    },
    /// A compute coil: its expression is the whole operand.
    ComputeCoil {
        expr: Option<&'a str>,
        /// The rung also draws a compute cell, which ends it before the coil.
        compute_cell: bool,
    },
    /// A jump or call coil: its operand is a label or a POU, not a bit.
    LabelCoil {
        kind: CoilKind,
        var: &'a str,
        /// The rung also draws a compute cell, so it leaves no power bit.
        compute_cell: bool,
        /// The pins a call binds, which a jump has none of.
        args: &'a [crate::model::CallArg],
    },
    Coil {
        var: &'a str,
    },
    /// One block of an FBD grid.
    Fbd {
        block: &'a FbdBlock,
    },
    /// One line of the line-form FBD body (`pou.fbd`), at `index` in it.
    FbdLine {
        line: &'a FbdLine,
        index: usize,
    },
}

/// What one check found, plus the two decisions only the rule can make.
#[derive(Default)]
pub struct Verdict {
    pub diagnostics: Vec<Diagnostic>,
    /// The rule already judged this coil's target — a counter's `R` or `LD`
    /// member — so the caller applies no target rule of its own.
    pub target_judged: bool,
}

/// What one IL line found and the accumulator it leaves behind.
#[derive(Default)]
pub struct IlVerdict {
    pub diagnostics: Vec<Diagnostic>,
    /// The accumulator this line leaves; `None` when it leaves it alone.
    pub accumulator: Option<bool>,
}

impl Verdict {
    fn new(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            target_judged: false,
        }
    }
}

/// Check one stateless instruction.
pub fn check_instruction(instr: &Instr, ctx: &Ctx) -> Vec<Diagnostic> {
    match instr {
        Instr::Cell(cell) => check_cell(cell, ctx),
        Instr::Logic(logic) => check_logic(logic, ctx),
        Instr::CoilExpr { kind, var, expr } => check_coil_expr(*kind, var, expr, ctx),
        Instr::ComputeCoil { expr, compute_cell } => check_compute_coil(*expr, *compute_cell, ctx),
        Instr::LabelCoil {
            kind,
            var,
            compute_cell,
            args,
        } => check_label_coil(*kind, var, *compute_cell, args, ctx),
        Instr::Coil { var } => check_coil(var, ctx).diagnostics,
        Instr::Fbd { block } => check_fbd_block(block, ctx),
        Instr::FbdLine { line, index } => check_fbd_line(line, *index, ctx),
    }
}

/// Is this a mnemonic that calls a POU? The call-graph walk is structural, but
/// which names are calls is the instruction set's answer.
pub fn is_call_mnemonic(mnemonic: &str) -> bool {
    matches!(
        spec::spec(mnemonic).map(|spec| spec.il),
        Some(IlForm::Call(_))
    )
}

/// Check a bit coil's target, keeping the verdict: a counter's `R`/`LD` member is
/// the coil's whole operand, so the caller skips its own target rules.
pub fn coil(instr: &Instr, ctx: &Ctx) -> Verdict {
    match instr {
        Instr::Coil { var } => check_coil(var, ctx),
        _ => Verdict::new(check_instruction(instr, ctx)),
    }
}

/// Check one IL line, which cannot be stateless: a word operation leaves a word
/// in the accumulator and the next line's store has to see that.
pub fn check_il_line(
    mnemonic: &str,
    operand: &str,
    accumulator: Option<bool>,
    ctx: &Ctx,
) -> IlVerdict {
    let mut diagnostics = Vec::new();
    // A word operand is whatever the profile resolves: a marker word, a time, a
    // literal, a block member or a declared word point. The same function the
    // compiler addresses with, so flow and code cannot disagree.
    let operand_is_word = resolve::resolve(operand, ctx.word_slots).is_ok();
    if !operand.trim().is_empty() {
        // Only a mnemonic that fixes the operand's side is checked here:
        // `LD`/`LDN`/`ST`/`STN`/`=` take either side, and the type flow reports
        // an accumulator mismatch for those.
        if takes_bit_operand(mnemonic) {
            io_kind(operand, true, ctx.path, ctx, &mut diagnostics);
        } else if takes_word_operand(mnemonic) {
            io_kind(operand, false, ctx.path, ctx, &mut diagnostics);
        }
    }
    let mut leaves = None;
    match spec::spec(mnemonic).map(|spec| spec.il) {
        Some(IlForm::Store | IlForm::StoreNot) => {
            if let Some(current) = accumulator {
                if current != operand_is_word {
                    diagnostics.push(Diagnostic::error(
                        "il_type",
                        format!(
                            "'{mnemonic}' stores a {}, but the accumulator holds a {}",
                            if operand_is_word { "word" } else { "bit" },
                            if current { "word" } else { "bit" },
                        ),
                        ctx.path,
                    ));
                }
            }
        }
        Some(IlForm::Push | IlForm::PushNot) => leaves = Some(operand_is_word),
        Some(
            IlForm::And
            | IlForm::AndNot
            | IlForm::Or
            | IlForm::OrNot
            | IlForm::Xor
            | IlForm::XorNot
            | IlForm::Not,
        ) => {
            if accumulator == Some(true) {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a bit in the accumulator, not a word"),
                    ctx.path,
                ));
            }
        }
        // The four forms below stop the line: their rule already names what a
        // mismatch would be, and their operand is not a plain value.
        Some(IlForm::Jump(..)) => {
            if accumulator == Some(true) {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a bit result to test, not a word"),
                    ctx.path,
                ));
            }
            if !ctx.labels.contains(&operand.trim().to_ascii_uppercase()) {
                diagnostics.push(Diagnostic::error(
                    "il_label_missing",
                    format!("'{operand}' is not a label in this POU; declare it as `LABEL:`"),
                    ctx.path,
                ));
            }
            return IlVerdict {
                diagnostics,
                accumulator: None,
            };
        }
        // `RET` and `RETC`/`RETCN` take no operand, so they stop the line and no
        // operand rule applies.
        Some(IlForm::Return(_)) => {
            if accumulator == Some(true) {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a bit result to test, not a word"),
                    ctx.path,
                ));
            }
            return IlVerdict {
                diagnostics,
                accumulator: None,
            };
        }
        Some(IlForm::Call(Some(_))) => {
            if accumulator == Some(true) {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a bit result to test, not a word"),
                    ctx.path,
                ));
            }
            if !ctx.pou_names.contains(&operand.trim().to_ascii_uppercase()) {
                diagnostics.push(Diagnostic::error(
                    "cal_target",
                    format!(
                        "'{operand}' is not a POU in this project; CALC calls a POU, not a block"
                    ),
                    ctx.path,
                ));
            }
            return IlVerdict {
                diagnostics,
                accumulator: None,
            };
        }
        Some(IlForm::Call(None)) => {
            check_call(operand, ctx, &mut diagnostics);
            return IlVerdict {
                diagnostics,
                accumulator: None,
            };
        }
        Some(IlForm::Compare(_)) => {
            if let Some(false) = accumulator {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a word in the accumulator, not a bit"),
                    ctx.path,
                ));
            }
            return IlVerdict {
                diagnostics,
                accumulator: Some(false),
            };
        }
        Some(IlForm::Word(_)) => {
            if let Some(false) = accumulator {
                diagnostics.push(Diagnostic::error(
                    "il_type",
                    format!("'{mnemonic}' needs a word in the accumulator, not a bit"),
                    ctx.path,
                ));
            }
            // Arithmetic leaves a word, a compare a bit.
            return IlVerdict {
                diagnostics,
                accumulator: Some(true),
            };
        }
        Some(IlForm::Set | IlForm::Reset | IlForm::None) | None => {}
    }
    // A load, a store, a combinator or a bare operand still has to name
    // something this project declares.
    if resolve::marker_word_slot(operand).is_none()
        && operand.trim().parse::<i32>().is_err()
        && !ctx.known.contains(&operand.to_ascii_lowercase())
    {
        diagnostics.push(Diagnostic::error(
            "il_operand",
            format!("'{operand}' is not a declared input, output or variable"),
            ctx.path,
        ));
    }
    IlVerdict {
        diagnostics,
        accumulator: leaves,
    }
}

fn check_call(operand: &str, ctx: &Ctx, diagnostics: &mut Vec<Diagnostic>) {
    if let Some((name, args)) = parse_call(operand) {
        let block = crate::compile::parse_block(&name, &name, "");
        if block.is_none() {
            diagnostics.push(Diagnostic::error(
                "cal_block",
                format!(
                    "'{name}' is not a block instance; use TON0-3, TOF0-3, TP0-3, CTU0-3 or CTD0-3"
                ),
                ctx.path,
            ));
        }
        for (key, value) in args {
            match key.as_str() {
                "IN" | "CU" | "CD" => {
                    if !ctx.known.contains(&value.to_ascii_lowercase())
                        && !value.eq_ignore_ascii_case("TRUE")
                        && !value.eq_ignore_ascii_case("FALSE")
                    {
                        diagnostics.push(Diagnostic::error(
                            "cal_arg",
                            format!("'{value}' is not a declared input"),
                            ctx.path,
                        ));
                    }
                }
                "PT" | "PV" => {
                    let timer = block.map(|(kind, _, _)| kind.is_timer()).unwrap_or(true);
                    // A literal preset, a marker word, or a declared word point;
                    // a block member is a readable value but not a preset
                    // source.
                    let ok = crate::compile::preset_source(&value, timer).is_some()
                        || ctx
                            .word_slots
                            .contains_key(&value.trim().to_ascii_lowercase());
                    if !ok {
                        diagnostics.push(Diagnostic::error(
                            "cal_arg",
                            format!("'{value}' is not a valid preset or a declared word"),
                            ctx.path,
                        ));
                    }
                }
                other => diagnostics.push(Diagnostic::error(
                    "cal_arg",
                    format!("'{other}' is not an argument; use IN, CU, CD, PT or PV"),
                    ctx.path,
                )),
            }
        }
    } else if !ctx.pou_names.contains(&operand.trim().to_ascii_uppercase()) {
        diagnostics.push(Diagnostic::error(
            "cal_target",
            format!("'{operand}' is not a POU in this project"),
            ctx.path,
        ));
    }
}

/// A mnemonic whose operand is a bit: the combinators and the coil forms.
fn takes_bit_operand(mnemonic: &str) -> bool {
    matches!(
        spec::spec(mnemonic).map(|spec| spec.il),
        Some(
            IlForm::And
                | IlForm::AndNot
                | IlForm::Or
                | IlForm::OrNot
                | IlForm::Xor
                | IlForm::XorNot
                | IlForm::Not
                | IlForm::Set
                | IlForm::Reset
        )
    )
}

/// A mnemonic whose operand is a word: arithmetic and compares.
fn takes_word_operand(mnemonic: &str) -> bool {
    matches!(
        spec::spec(mnemonic).map(|spec| spec.il),
        Some(IlForm::Word(_) | IlForm::Compare(_))
    )
}

/// One grid cell.
fn check_cell(cell: &Cell, ctx: &Ctx) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if let Cell::Block {
        block,
        instance,
        preset,
    } = cell
    {
        if crate::compile::parse_block(block, instance, preset).is_none() {
            diagnostics.push(Diagnostic::error(
                "cell_block",
                format!(
                    "'{instance}' with preset '{preset}' is not a valid block; use \
                     TON0-3, TOF0-3, TP0-3, CTU0-3 or CTD0-3 and a preset like T#4s"
                ),
                ctx.path,
            ));
        }
    } else if let Cell::Compare { op, left, right } = cell {
        if crate::compile::compare_op(op).is_none() {
            diagnostics.push(Diagnostic::error(
                "cell_compare",
                format!("'{op}' is not a compare operator; use GT, GE, EQ, NE, LE or LT"),
                ctx.path,
            ));
        }
        for operand in [left, right] {
            io_kind(operand, false, ctx.path, ctx, &mut diagnostics);
            if !word_known(operand, ctx) {
                diagnostics.push(Diagnostic::error(
                    "cell_compare_operand",
                    format!("'{operand}' is not a word, a number, a time or a block member"),
                    ctx.path,
                ));
            }
        }
    } else if let Cell::Compute {
        op,
        target,
        left,
        right,
    } = cell
    {
        if crate::compile::MathOp::parse(op).is_none() {
            diagnostics.push(Diagnostic::error(
                "cell_compute",
                format!("'{op}' is not a compute operator; use ADD, SUB, MUL, DIV or MOVE"),
                ctx.path,
            ));
        }
        if !writable_target(target, ctx) {
            diagnostics.push(Diagnostic::error(
                "compute_target",
                format!(
                    "'{target}' is not a place to write; use {}, a word point or a block member such as TON0.PT",
                    marker_range()
                ),
                ctx.path,
            ));
        }
        for operand in [left, right] {
            io_kind(operand, false, ctx.path, ctx, &mut diagnostics);
            if !word_known(operand, ctx) {
                diagnostics.push(Diagnostic::error(
                    "cell_compute_operand",
                    format!("'{operand}' is not a word, a number, a time or a block member"),
                    ctx.path,
                ));
            }
        }
    } else if let Cell::Expr { text } = cell {
        check_expression(text, false, ctx.path, ctx.word_slots, &mut diagnostics);
    } else if let Some(operand) = cell.operand() {
        io_kind(operand, true, ctx.path, ctx, &mut diagnostics);
        let known = ctx.known.contains(&operand.to_ascii_lowercase())
            || ctx.profile.bit_member(operand).is_some();
        if !known {
            diagnostics.push(Diagnostic::error(
                "cell_operand",
                format!("'{operand}' is not a declared input, output or variable"),
                ctx.path,
            ));
        }
    }
    diagnostics
}

/// One node of a ladder tree. The caller walks; this judges the node.
fn check_logic(logic: &Logic, ctx: &Ctx) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let path = ctx.path;
    match logic {
        Logic::Const { .. } => {}
        Logic::Expr { text } => {
            check_expression(
                text,
                false,
                &format!("{path}.text"),
                ctx.word_slots,
                &mut diagnostics,
            );
        }
        Logic::Contact { var, .. } | Logic::Edge { var, .. } => {
            io_kind(var, true, &format!("{path}.var"), ctx, &mut diagnostics);
            // A bit of a word is an operand in its own right; the two refusals that
            // are not about the word's declaration fall through to "not declared".
            let undeclared = matches!(
                ctx.word_bit(var),
                Err(crate::profile::layout::WordBitError::NotABit)
                    | Err(crate::profile::layout::WordBitError::Unknown)
            );
            if undeclared
                && !ctx.known.contains(&var.to_ascii_lowercase())
                && ctx.profile.bit_member(var).is_none()
            {
                diagnostics.push(Diagnostic::error(
                    "unknown_symbol",
                    format!("contact `{var}` is not declared"),
                    format!("{path}.var"),
                ));
            }
            // An edge contact keeps its previous value in a bit-image slot, and a bit
            // of a word has none: refusing is cheaper than watching the wrong bit.
            if matches!(logic, Logic::Edge { .. }) && ctx.word_bit(var).is_ok() {
                diagnostics.push(Diagnostic::error(
                    "edge_unsupported",
                    format!("an edge contact on `{var}` has no form yet; use a plain contact"),
                    format!("{path}.var"),
                ));
            }
        }
        Logic::Block {
            block,
            instance,
            preset,
            args,
        } => {
            if let Some(crate::profile::spec::Family::Analog(name)) =
                crate::profile::spec::spec(block).map(|spec| spec.family)
            {
                return check_analog(block, name, args, ctx);
            }
            // The block and the instance first, with the empty preset that every kind
            // has a default for, so an unreadable preset is not reported as an
            // unknown block.
            let timer = crate::compile::parse_block(block, instance, "")
                .map(|(kind, _, _)| kind.is_timer());
            match timer {
                None => diagnostics.push(Diagnostic::error(
                    "unknown_block",
                    format!("`{block}` is not a block; use TON/TOF/TP/CTU/CTD/SR/RS/RT/RF"),
                    format!("{path}.block"),
                )),
                Some(timer) => {
                    if let Err(message) = check_preset(preset, timer, ctx) {
                        diagnostics.push(Diagnostic::error(
                            "cal_arg",
                            message,
                            format!("{path}.preset"),
                        ));
                    }
                }
            }
        }
        Logic::Compare {
            operator,
            left,
            right,
        } => {
            if crate::compile::compare_op(operator).is_none() {
                diagnostics.push(Diagnostic::error(
                    "compare_operator",
                    format!("`{operator}` is not GT, GE, EQ, NE, LE or LT"),
                    format!("{path}.operator"),
                ));
            }
            for operand in [left, right] {
                io_kind(
                    operand,
                    false,
                    &format!("{path}.left"),
                    ctx,
                    &mut diagnostics,
                );
                if resolve::resolve(operand, ctx.word_slots).is_err() {
                    diagnostics.push(Diagnostic::error(
                        "compare_operand",
                        format!("`{operand}` is not a word, a number or a block member"),
                        format!("{path}.left"),
                    ));
                }
            }
        }
        // Series and Parallel are the tree's shape, not an instruction: the
        // caller's walk reports their emptiness and descends.
        Logic::Series { .. } | Logic::Parallel { .. } => {}
    }
    diagnostics
}

/// A coil kind that does not read `expr` refuses it rather than ignoring it. The
/// caller only asks when the kind is not a compute coil, so this always reports.
fn check_coil_expr(kind: CoilKind, var: &str, _expr: &str, ctx: &Ctx) -> Vec<Diagnostic> {
    debug_assert_ne!(
        kind,
        CoilKind::Compute,
        "a compute coil reads its expression"
    );
    vec![Diagnostic::error(
        "coil_expr",
        format!("`expr` belongs to a compute coil; `{var}` does not read it"),
        format!("{}.coil.expr", ctx.path),
    )]
}

fn check_compute_coil(expr: Option<&str>, compute_cell: bool, ctx: &Ctx) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if compute_cell {
        // A compute cell ends the rung before the coil, so the assignment would
        // never run.
        diagnostics.push(Diagnostic::error(
            "cell_compute",
            "a rung cannot carry both a compute cell and a compute coil",
            format!("{}.coil.var", ctx.path),
        ));
    }
    match expr {
        Some(text) => check_expression(
            text,
            true,
            &format!("{}.coil.expr", ctx.path),
            ctx.word_slots,
            &mut diagnostics,
        ),
        None => diagnostics.push(Diagnostic::error(
            "coil_expr",
            "a compute coil needs an expression: `target = value`",
            format!("{}.coil.expr", ctx.path),
        )),
    }
    diagnostics
}

fn check_label_coil(
    kind: CoilKind,
    var: &str,
    compute_cell: bool,
    args: &[crate::model::CallArg],
    ctx: &Ctx,
) -> Vec<Diagnostic> {
    let path = format!("{}.coil.var", ctx.path);
    let path = path.as_str();
    if compute_cell {
        // A compute cell consumes the power bit and pushes nothing, so the coil
        // after it would test garbage.
        return vec![Diagnostic::error(
            "cell_compute",
            format!(
                "a compute row leaves no power bit, so its coil cannot be a {}",
                if kind == CoilKind::Jump {
                    "jump"
                } else {
                    "call"
                }
            ),
            path,
        )];
    }
    let target = var.trim();
    match kind {
        CoilKind::Jump => {
            if target.is_empty() {
                return vec![Diagnostic::error(
                    "rung_label_missing",
                    "a jump coil needs the label of the rung to jump to",
                    path,
                )];
            }
            let wanted = target.to_ascii_uppercase();
            let found = ctx
                .section_rungs
                .iter()
                .any(|rung| rung.label.trim().to_ascii_uppercase() == wanted);
            if found {
                Vec::new()
            } else {
                vec![Diagnostic::error(
                    "jump_target_missing",
                    format!("no rung in this section carries the label '{target}'"),
                    path,
                )]
            }
        }
        CoilKind::Call => {
            if target.is_empty() {
                return vec![Diagnostic::error(
                    "call_target_missing",
                    "a call coil needs the name of the POU to call",
                    path,
                )];
            }
            // A call names a POU, or an instance of a function block. An instance is
            // the one with data, and its pins are its block's interface.
            let upper = target.to_ascii_uppercase();
            let block = if ctx.pou_names.contains(&upper)
                && !ctx.instances.contains_key(&target.to_ascii_lowercase())
            {
                Some(upper)
            } else if let Some(block) = ctx.instances.get(&target.to_ascii_lowercase()) {
                Some(block.clone())
            } else if ctx.pou_names.contains(&upper) {
                Some(upper)
            } else {
                None
            };
            let Some(block) = block else {
                return vec![Diagnostic::error(
                    "call_target_missing",
                    format!("'{target}' is not a POU and not an instance of one"),
                    path,
                )];
            };
            check_call_args(block.as_str(), args, path, ctx)
        }
        _ => Vec::new(),
    }
}

/// An analogue block's pins: every pin it needs, the destination writable, and a
/// numeric member that is a literal because the scan bakes it.
fn check_analog(
    block: &str,
    name: crate::profile::spec::AnalogName,
    args: &[crate::model::CallArg],
    ctx: &Ctx,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let path = format!("{}.args", ctx.path);
    let Some(spec) = crate::profile::spec::spec(block) else {
        return diagnostics;
    };
    let pin_of = |pin: &str| {
        args.iter()
            .find(|argument| argument.pin.eq_ignore_ascii_case(pin))
    };
    // Only the pins a block cannot work without are required: a PID's setpoint may
    // come from a master loop, and its members have defaults.
    let required: &[&str] = match name {
        crate::profile::spec::AnalogName::Scale => spec.pins.0,
        crate::profile::spec::AnalogName::Pid => &["IN"],
    };
    for pin in required {
        if pin_of(pin).is_none() {
            diagnostics.push(Diagnostic::error(
                "analog_pin",
                format!("`{block}` needs its `{pin}` pin"),
                &path,
            ));
        }
    }
    if name == crate::profile::spec::AnalogName::Pid
        && pin_of("SP").is_none()
        && pin_of("MASTER").is_none()
    {
        diagnostics.push(Diagnostic::error(
            "analog_pin",
            "`PID` needs a setpoint pin or a master loop",
            &path,
        ));
    }
    let Some(dest) = spec.pins.1.first() else {
        return diagnostics;
    };
    match pin_of(dest) {
        None => diagnostics.push(Diagnostic::error(
            "analog_pin",
            format!("`{block}` needs a destination on `{dest}`"),
            &path,
        )),
        Some(argument) => {
            if crate::profile::resolve::resolve(&argument.var, ctx.word_slots).is_err() {
                diagnostics.push(Diagnostic::error(
                    "analog_pin",
                    format!(
                        "'{dest}' is bound to '{}', which is not a word",
                        argument.var
                    ),
                    &path,
                ));
            }
        }
    }
    if name == crate::profile::spec::AnalogName::Pid {
        // The setup is baked into the block's op, so a member that is not a literal
        // cannot be one.
        for pin in [
            "KP", "KI", "KD", "MAXI", "MINI", "MAXS", "MINS", "MAXCV", "MINCV", "UPD",
        ] {
            if let Some(argument) = pin_of(pin) {
                if argument.var.trim().parse::<f32>().is_err() {
                    diagnostics.push(Diagnostic::error(
                        "analog_pin",
                        format!(
                            "'{pin}' is '{}', and a controller member must be a number",
                            argument.var
                        ),
                        &path,
                    ));
                }
            }
        }
    }
    diagnostics
}

/// Is this text a preset the scan can read? A literal, a time, a marker word, a
/// declared word, or one element of a declared array.
///
/// `Err` carries the sentence, so a literal index past the array says which array and
/// how long it is instead of "not a valid preset".
fn check_preset(text: &str, timer: bool, ctx: &Ctx) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    if crate::compile::preset_source(text, timer).is_some()
        || ctx.word_slots.contains_key(&text.to_ascii_lowercase())
    {
        return Ok(());
    }
    // One field of one element of an array of structs.
    if let Some(operand) = crate::model::ArrayField::parse(text) {
        let lower = operand.array.to_ascii_lowercase();
        let Some(structure) = ctx
            .structs
            .iter()
            .find(|structure| structure.name.to_ascii_lowercase() == lower)
        else {
            return Err(format!(
                "'{}' is not a declared array of structs",
                operand.array
            ));
        };
        if !structure
            .fields
            .iter()
            .any(|(name, _, _)| name.eq_ignore_ascii_case(&operand.field))
        {
            return Err(format!(
                "'{text}' names no field of `{}` ({})",
                structure.name, structure.ty
            ));
        }
        if let Ok(number) = operand.index.trim().parse::<i64>() {
            if number < 0 || number >= i64::from(structure.elements) {
                return Err(format!(
                    "'{text}' is element {number} of `{}` ({}[{}]), which has {} elements",
                    structure.name, structure.ty, structure.elements, structure.elements
                ));
            }
        } else if !ctx
            .word_slots
            .contains_key(&operand.index.to_ascii_lowercase())
        {
            return Err(format!(
                "'{}' is not a declared word, so it cannot index `{}`",
                operand.index, structure.name
            ));
        }
        return Ok(());
    }
    let Some(operand) = crate::model::ArrayElement::parse(text) else {
        return Err(format!("'{text}' is not a valid preset or a declared word"));
    };
    let lower = operand.array.to_ascii_lowercase();
    let Some(var) = ctx
        .word_vars
        .iter()
        .find(|var| var.name.to_ascii_lowercase() == lower)
    else {
        return Err(format!("'{}' is not a declared array", operand.array));
    };
    let elements = var.ty.elements();
    match operand.literal() {
        Some(number) if number < 0 || number >= i64::from(elements) => Err(format!(
            "'{text}' is element {number} of `{}` ({}), which has {elements} elements",
            var.name,
            var.ty.label()
        )),
        Some(_) => Ok(()),
        // A declared index is read at run time and clamped into the array by the
        // scan, so there is nothing static to refuse.
        None => Ok(()),
    }
}

/// A call's pins: every pin the call binds must exist on the function block, with the
/// direction the argument was written in, and the operand it binds must be something
/// this project declares.
///
/// A POU that is not a function block has no pins, so an argument on it is refused.
fn check_call_args(
    block: &str,
    args: &[crate::model::CallArg],
    path: &str,
    ctx: &Ctx,
) -> Vec<Diagnostic> {
    if args.is_empty() {
        return Vec::new();
    }
    let Some(fields) = ctx.fb_vars.get(block) else {
        return vec![Diagnostic::error(
            "call_arg",
            format!("'{block}' has no pins, so the call cannot bind one"),
            path,
        )];
    };
    let mut diagnostics = Vec::new();
    for argument in args {
        let pin = fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(&argument.pin));
        let Some(pin) = pin else {
            diagnostics.push(Diagnostic::error(
                "call_arg",
                format!("'{block}' has no pin '{}'", argument.pin),
                path,
            ));
            continue;
        };
        let wanted = if argument.out {
            Some(crate::model::IoDirection::Out)
        } else {
            Some(crate::model::IoDirection::In)
        };
        if pin.direction.is_some() && pin.direction != wanted {
            diagnostics.push(Diagnostic::error(
                "call_arg",
                format!(
                    "'{}' is {} on '{block}', so it is bound with `{}`",
                    argument.pin,
                    if pin.direction == Some(crate::model::IoDirection::In) {
                        "an input"
                    } else {
                        "an output"
                    },
                    if pin.direction == Some(crate::model::IoDirection::In) {
                        ":="
                    } else {
                        "=>"
                    }
                ),
                path,
            ));
            continue;
        }
        // What the pin is bound to has to be an operand this project declares. A
        // word pin binds a word; the corpus's pins are bits.
        if !ctx.known.contains(&argument.var.to_ascii_lowercase())
            && !ctx.known.contains(&argument.pin.to_ascii_lowercase())
            && crate::profile::resolve::marker_word_slot(&argument.var).is_none()
            && argument.var.parse::<i32>().is_err()
        {
            diagnostics.push(Diagnostic::error(
                "call_arg",
                format!(
                    "'{}' is bound to '{}', which is not declared",
                    argument.pin, argument.var
                ),
                path,
            ));
        }
    }
    diagnostics
}

/// A bit coil's target: the operand side, and the counter-member rule. A
/// counter's reset or load is a coil on the instance, not on a bit.
fn check_coil(var: &str, ctx: &Ctx) -> Verdict {
    let mut diagnostics = Vec::new();
    let path = format!("{}.coil.var", ctx.path);
    let mut verdict = Verdict::new(Vec::new());
    if !var.trim().is_empty() {
        io_kind(var, true, &path, ctx, &mut diagnostics);
    }
    // A coil on a block member — `CTU0.R`, `CTD0.LD`, ClassicLadder's
    // `.CU`/`.CD`/`.L` — is a write to that block. Only a counter has them; the
    // profile says which spellings exist, and the compiler reads the same table.
    if let Some((_, member)) = ctx.profile.bit_member(var) {
        if member.is_writable() {
            let kind = crate::compile::parse_block(var, var, "").map(|(kind, _, _)| kind);
            if !kind.is_some_and(crate::compile::FbKind::is_counter) {
                diagnostics.push(Diagnostic::error(
                    "coil_target",
                    format!("`{var}` is not a counter member; only a counter has one"),
                    path,
                ));
            }
            verdict.target_judged = true;
        }
    }
    verdict.diagnostics = diagnostics;
    verdict
}

/// One FBD block: its kind, its inputs and what its output names.
fn check_fbd_block(block: &FbdBlock, ctx: &Ctx) -> Vec<Diagnostic> {
    // Only the grid walk builds `Instr::Fbd`, and it always lends its grid; a
    // caller that does not gets no rule rather than a panic.
    let Some(grid) = ctx.grid else {
        return Vec::new();
    };
    let path = ctx.path;
    let name = block.kind.trim().to_ascii_uppercase();
    let reference = |input: &str| -> Option<usize> {
        input
            .trim()
            .strip_prefix('@')
            .and_then(|number| number.parse::<usize>().ok())
    };
    let bit_ok = |input: &str| -> bool {
        let input = input.trim().trim_start_matches('!');
        input.is_empty()
            || input.eq_ignore_ascii_case("TRUE")
            || input.eq_ignore_ascii_case("FALSE")
            || ctx.known.contains(&input.to_ascii_lowercase())
            || reference(input)
                .map(|target| target >= 1 && target <= grid.blocks.len())
                .unwrap_or(false)
    };
    let inputs = (0..block.ins.max(1))
        .map(|offset| {
            grid.input_at(block.row + offset, block.col.saturating_sub(1))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();

    let mut diagnostics = Vec::new();
    let mut needs_bit = false;
    let mut needs_two_bits = false;
    let mut needs_words = false;
    let mut compute = false;
    let form = spec::spec(&name).map(|spec| spec.il);
    if matches!(
        form,
        Some(IlForm::And | IlForm::Or | IlForm::Xor | IlForm::Not)
    ) {
        needs_bit = true;
    } else if let Some((kind, _, _)) = crate::compile::parse_block(&name, &name, &block.preset) {
        if kind.is_latch() {
            needs_two_bits = true;
        } else {
            needs_bit = true;
        }
    } else if crate::compile::compare_op(&name).is_some() {
        needs_words = true;
    } else if crate::compile::MathOp::parse(&name).is_some() {
        compute = true;
    } else {
        return vec![Diagnostic::error(
            "fbd_block",
            format!("'{name}' is not a block"),
            format!("{path}.kind"),
        )];
    }

    let inputs_path = format!("{path}.row");
    for input in inputs.iter().take(if needs_two_bits { 2 } else { 1 }) {
        io_kind(
            input.trim().trim_start_matches('!'),
            needs_bit || needs_two_bits,
            &inputs_path,
            ctx,
            &mut diagnostics,
        );
    }
    if needs_bit && (inputs.is_empty() || !bit_ok(&inputs[0])) {
        diagnostics.push(Diagnostic::error(
            "fbd_open",
            format!("{name} has no input to read"),
            format!("{path}.row"),
        ));
    }
    if needs_two_bits && !inputs.get(1).map(|input| bit_ok(input)).unwrap_or(false) {
        diagnostics.push(Diagnostic::error(
            "fbd_open",
            format!("{name} needs two inputs"),
            format!("{path}.row"),
        ));
    }
    if (needs_words || compute)
        && !inputs
            .iter()
            .take(2)
            .all(|input| word_known(input.trim().trim_start_matches('!'), ctx))
    {
        diagnostics.push(Diagnostic::error(
            "fbd_input",
            format!("{name} takes two word values"),
            format!("{path}.row"),
        ));
    }
    match grid.cell(block.row, block.col + 1) {
        crate::model::FbdCell::Out { operand } => {
            let target = operand.trim().trim_start_matches('!');
            if compute {
                // A compute block writes a word; a literal or a bit output is
                // not a place to put the result.
                if !writable_target(target, ctx) {
                    diagnostics.push(Diagnostic::error(
                        "compute_target",
                        format!(
                            "'{operand}' is not a place to write; use {}, a word point or a block member such as TON0.PT",
                            marker_range()
                        ),
                        format!("{path}.row"),
                    ));
                }
            } else {
                let declared = ctx.known.contains(&target.to_ascii_lowercase());
                let word = resolve::marker_word_slot(target).is_some();
                if !declared && !word {
                    diagnostics.push(Diagnostic::error(
                        "fbd_output",
                        format!("'{operand}' is not a declared output or variable"),
                        format!("{path}.row"),
                    ));
                }
            }
        }
        _ => diagnostics.push(Diagnostic::error(
            "fbd_dangling",
            format!("{name} has nothing on its output"),
            format!("{path}.row"),
        )),
    }
    diagnostics
}

/// One block of the line-form FBD body: its output, its refs, its inputs and the
/// arity its kind takes.
fn check_fbd_line(line: &FbdLine, index: usize, ctx: &Ctx) -> Vec<Diagnostic> {
    let path = ctx.path;
    let block = line.block.trim().to_ascii_uppercase();
    let output_name = line
        .output
        .trim()
        .strip_prefix('!')
        .unwrap_or_else(|| line.output.trim());
    let mut diagnostics = Vec::new();
    if !ctx.known.contains(&output_name.to_ascii_lowercase()) {
        diagnostics.push(Diagnostic::error(
            "fbd_output",
            format!("'{}' is not a declared output or variable", line.output),
            format!("{path}.output"),
        ));
    }
    let reference = |input: &str| -> Option<usize> {
        input
            .trim()
            .strip_prefix('@')
            .and_then(|number| number.parse::<usize>().ok())
    };
    for input in &line.inputs {
        if let Some(target) = reference(input) {
            if target == 0 || target > index {
                diagnostics.push(Diagnostic::error(
                    "fbd_ref",
                    format!(
                        "'{input}' is not an earlier line; a block can only read a pin above it"
                    ),
                    format!("{path}.inputs"),
                ));
            }
        }
    }
    let bit_ok = line.inputs.iter().all(|input| {
        let input = input.trim().strip_prefix('!').unwrap_or(input.trim());
        input.eq_ignore_ascii_case("TRUE")
            || input.eq_ignore_ascii_case("FALSE")
            || ctx.known.contains(&input.to_ascii_lowercase())
            || reference(input).map(|target| target >= 1).unwrap_or(false)
    });
    let word_block = crate::compile::compare_op(&block).is_some();
    for input in &line.inputs {
        io_kind(
            input.trim().trim_start_matches('!'),
            !word_block,
            &format!("{path}.inputs"),
            ctx,
            &mut diagnostics,
        );
    }
    let form = spec::spec(&block).map(|spec| spec.il);
    if matches!(
        form,
        Some(IlForm::And | IlForm::Or | IlForm::Xor | IlForm::Not)
    ) {
        if !bit_ok {
            diagnostics.push(Diagnostic::error(
                "fbd_input",
                format!("{block} takes declared inputs"),
                format!("{path}.inputs"),
            ));
        }
        let not = matches!(form, Some(IlForm::Not));
        let needed = if not { 1 } else { 2 };
        if not && line.inputs.len() != needed {
            diagnostics.push(Diagnostic::error(
                "fbd_arity",
                "NOT takes one input",
                format!("{path}.inputs"),
            ));
        }
    } else if let Some((kind, _, _)) = crate::compile::parse_block(&block, &block, &line.preset) {
        let needed = if kind.is_latch() { 2 } else { 1 };
        if !bit_ok || line.inputs.len() < needed {
            diagnostics.push(Diagnostic::error(
                "fbd_input",
                format!(
                    "{block} takes {needed} declared input{}",
                    if needed == 1 { "" } else { "s" }
                ),
                format!("{path}.inputs"),
            ));
        }
    } else if crate::compile::parse_block(&block, &block, &line.preset).is_some() {
        if !bit_ok || line.inputs.is_empty() {
            diagnostics.push(Diagnostic::error(
                "fbd_input",
                format!("{block} takes one declared input"),
                format!("{path}.inputs"),
            ));
        }
    } else if crate::compile::compare_op(&block).is_some() {
        if line.inputs.len() != 2 || !line.inputs.iter().all(|input| word_known(input, ctx)) {
            diagnostics.push(Diagnostic::error(
                "fbd_input",
                format!("{block} takes two word values"),
                format!("{path}.inputs"),
            ));
        }
    } else {
        diagnostics.push(Diagnostic::error(
            "fbd_block",
            format!("'{block}' is not a block; use AND, OR, XOR, NOT, a TON/TOF/TP/CTU/CTD instance or a compare"),
            format!("{path}.block"),
        ));
    }
    diagnostics
}

/// `io_kind`: a declared point used on the wrong side of the model. A bit point
/// has no word slot and a word point has no bit slot, so accepting the name
/// would silently address the wrong image; validation refuses it instead.
impl Ctx<'_> {
    /// A bit of a declared word, or why the text is not one.
    fn word_bit(&self, text: &str) -> Result<(u16, u8), crate::profile::layout::WordBitError> {
        crate::profile::layout::word_bit(text, self.word_vars)
    }
}

fn io_kind(name: &str, want_bit: bool, path: &str, ctx: &Ctx, diagnostics: &mut Vec<Diagnostic>) {
    let name = name.trim();
    // `Bits[0].0` and `%MW3.5` are bits of a word: a bit operand, never a word one.
    // The declaration decides the element and the width, so an index past either is
    // refused here rather than read from the next slot.
    match ctx.word_bit(name) {
        Ok(_) => {
            if !want_bit {
                diagnostics.push(Diagnostic::error(
                    "word_bit",
                    format!("`{name}` is one bit of a word and cannot be used as a word value"),
                    path,
                ));
            }
            return;
        }
        Err(error) => {
            use crate::profile::layout::WordBitError;
            if !matches!(error, WordBitError::NotABit) {
                // A word this build cannot address into, or an index past the bound:
                // the sentence names the declaration and the width.
                let message = match error {
                    WordBitError::Unknown => match crate::model::WordBit::parse(name)
                        .and_then(|operand| ctx.var_types.get(&operand.word.to_ascii_lowercase()))
                    {
                        // The word is declared, but as a bit: it has no bits to
                        // address into, and saying "not declared" would be false.
                        Some(ty) if ty.is_bool() => WordBitError::NoBits {
                            declared: format!("`{name}` (BOOL)"),
                        }
                        .message(name),
                        _ => error.message(name),
                    },
                    other => other.message(name),
                };
                diagnostics.push(Diagnostic::error("word_bit", message, path));
                return;
            }
        }
    }
    // A declared variable is typed too, and the type decides which side it may be
    // used on: a contact reads a BOOL, a word operand reads INT, DINT, REAL or TIME.
    // The message names the variable and the type it was declared with.
    if let Some(ty) = ctx.var_types.get(&name.to_ascii_lowercase()) {
        if want_bit && !ty.is_bool() {
            diagnostics.push(Diagnostic::error(
                "var_type",
                format!(
                    "`{name}` is {} and cannot be read as a bit contact or coil; \
                     a contact reads a BOOL",
                    ty.label()
                ),
                path,
            ));
        } else if !want_bit && ty.is_bool() {
            diagnostics.push(Diagnostic::error(
                "var_type",
                format!("`{name}` is BOOL and cannot be used as a word value; declare it INT, DINT, REAL or TIME"),
                path,
            ));
        }
        return;
    }
    let Some(kind) = ctx.io_kinds.get(name) else {
        return;
    };
    if want_bit {
        if *kind != IoKind::Bit {
            diagnostics.push(Diagnostic::error(
                "io_kind",
                format!(
                    "`{name}` is a {} point and cannot be read as a bit contact or coil",
                    kind_label(*kind)
                ),
                path,
            ));
        }
    } else if *kind == IoKind::Bit {
        diagnostics.push(Diagnostic::error(
            "io_kind",
            format!("`{name}` is a bit point and cannot be used as a word value"),
            path,
        ));
    }
}

fn kind_label(kind: IoKind) -> &'static str {
    match kind {
        IoKind::Bit => "bit",
        IoKind::Word => "word",
        IoKind::Float => "float",
    }
}

/// Is this text a word value? The resolver decides, so validation accepts
/// exactly what the compiler can address.
fn word_known(text: &str, ctx: &Ctx) -> bool {
    resolve::resolve(text, ctx.word_slots).is_ok()
}

/// Is this a place a compute may write? A bank slot or a block member; never a
/// literal.
fn writable_target(text: &str, ctx: &Ctx) -> bool {
    resolve::resolve(text, ctx.word_slots).is_ok_and(|operand| operand.is_writable())
}

/// Check one expression, mapping the parser's refusal to a diagnostic with its
/// own code and the byte offset of the offending token.
fn check_expression(
    text: &str,
    assignment: bool,
    path: &str,
    word_slots: &SymbolLookup,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let result = if assignment {
        crate::expr::lower_assignment(text, word_slots).map(|_| ())
    } else {
        crate::expr::lower_bit(text, word_slots).map(|_| ())
    };
    if let Err(error) = result {
        diagnostics.push(Diagnostic::error(
            error.code(),
            format!("{} (at byte {})", error.message, error.offset),
            path,
        ));
    }
}
