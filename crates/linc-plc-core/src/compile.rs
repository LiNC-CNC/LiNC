//! Validation and bytecode lowering.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::il::{lower_line, parse_line, IlLine, IlOperand};
use crate::model::{CoilKind, IoDirection, Logic, PouLanguage, Project};
use crate::validate::{validate, Diagnostic, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Input,
    Output,
    Var,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub index: u16,
    pub kind: SymbolKind,
    pub retain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub index: u16,
}

/// Standard function blocks, four instances each, as ZX-PLC numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbKind {
    Ton,
    Tof,
    Tp,
    Ctu,
    Ctd,
    /// `R_TRIG` / `RT`: a rising edge on CLK.
    Rt,
    /// `F_TRIG` / `RF`: a falling edge on CLK.
    Rf,
    /// Set-dominant latch (`SR`): S1 or (Q and not R).
    Sr,
    /// Reset-dominant latch (`RS`): not R1 and (S or Q).
    Rs,
}

impl FbKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "TON" => Some(Self::Ton),
            "TOF" => Some(Self::Tof),
            "TP" => Some(Self::Tp),
            "CTU" => Some(Self::Ctu),
            "CTD" => Some(Self::Ctd),
            "RT" | "R_TRIG" => Some(Self::Rt),
            "RF" | "F_TRIG" => Some(Self::Rf),
            "SR" => Some(Self::Sr),
            "RS" => Some(Self::Rs),
            _ => None,
        }
    }

    pub fn code(self) -> u16 {
        match self {
            Self::Ton => 0,
            Self::Tof => 1,
            Self::Tp => 2,
            Self::Ctu => 3,
            Self::Ctd => 4,
            Self::Rt => 5,
            Self::Rf => 6,
            Self::Sr => 7,
            Self::Rs => 8,
        }
    }

    pub fn is_timer(self) -> bool {
        matches!(self, Self::Ton | Self::Tof | Self::Tp)
    }

    /// Latches take two inputs; the rest take one.
    pub fn is_latch(self) -> bool {
        matches!(self, Self::Sr | Self::Rs)
    }
}

/// Arithmetic of a compute coil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathOp {
    Add,
    Sub,
    Mul,
    Div,
    Move,
}

impl MathOp {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "ADD" | "+" => Some(Self::Add),
            "SUB" | "-" => Some(Self::Sub),
            "MUL" | "*" => Some(Self::Mul),
            "DIV" | "/" => Some(Self::Div),
            "MOVE" | ":=" => Some(Self::Move),
            _ => None,
        }
    }
}

/// Edge contacts a project may declare, the same 32 ZX-PLC allows.
pub const MAX_EDGE_CONTACTS: u16 = 32;

/// Instances per function-block type.
pub const FB_PER_TYPE: u16 = 4;

/// Word slots in the bank. ZX-PLC has `MW0`-`MW15`; keeping the same sixteen
/// means an existing ZX program's `MW` numbers mean the same thing here.
pub const WORD_COUNT: u16 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    PushConst(bool),
    Push(u16),
    /// Read the value being built this scan rather than the previous image.
    /// Function-block lines chain inside one scan, so their inputs must see a
    /// value an earlier line stored.
    PushNext(u16),
    Not,
    And,
    Or,
    Xor,
    Store(u16),
    Set(u16),
    Reset(u16),
    /// Push word slot `MWn` on the word stack.
    LoadWord(u16),
    /// Push a literal on the word stack.
    LoadConst(i16),
    /// Pop a word and write it to `MWn`.
    StoreWord(u16),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Pop two words and push the boolean result on the bit stack.
    CmpGt,
    CmpGe,
    CmpEq,
    CmpNe,
    CmpLe,
    CmpLt,
    /// Run a standard function block: pop its input, push `Q`.
    Fb {
        kind: FbKind,
        instance: u16,
        preset: i16,
    },
    /// Push the block's `ET` (timers) or `CV` (counters) on the word stack.
    FbWord(u16),
    /// Push the block's `Q` on the bit stack.
    FbQ(u16),
    /// A two-input latch: pop the reset bit, then the set bit.
    Latch {
        kind: FbKind,
        instance: u16,
    },
    /// Pop the power bit; when it is true, clear `CV` (`CTU0.R`).
    FbReset(u16),
    /// Pop the power bit; when it is true, load `PV` into `CV` (`CTD0.LD`).
    FbLoad(u16),
    /// Push the block's preset (`PT` / `PV`) on the word stack.
    FbPreset(u16),
    /// Edge contact: push true on the scan the operand changed, then remember
    /// the value in slot `slot` for the next scan.
    Edge {
        slot: u16,
        var: u16,
        rising: bool,
    },
    /// A compute coil: pop the power bit and, when it is true, write the result
    /// to `MWtarget`. It ends the rung; there is no coil output.
    Compute {
        op: MathOp,
        target: u16,
        left: WordOperand,
        right: WordOperand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub ops: Vec<Op>,
    /// Value slots in the scan image: inputs, then outputs, then variables.
    pub var_count: u16,
    /// Word slots in the integer bank.
    pub word_count: u16,
    /// Function-block instances: `FB_PER_TYPE` per kind, index `kind*4 + n`.
    pub fb_count: u16,
    /// Previous-value slots for edge contacts.
    pub edge_count: u16,
    pub input_count: u16,
    pub output_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub program: Program,
    pub symbols: Vec<Symbol>,
    pub inputs: Vec<Binding>,
    pub outputs: Vec<Binding>,
    pub hash: String,
    pub warnings: Vec<Diagnostic>,
}

pub fn compile(project: &Project) -> Result<Compiled, Vec<Diagnostic>> {
    let diagnostics = validate(project);
    if diagnostics
        .iter()
        .any(|item| item.severity == Severity::Error)
    {
        return Err(diagnostics);
    }

    let capacity = project.io.len() + project.vars.len();
    let mut symbols = Vec::with_capacity(capacity);
    let mut lookup: HashMap<String, u16> = HashMap::with_capacity(capacity);
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    for point in project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::In)
    {
        let index = intern(
            &mut symbols,
            &mut lookup,
            &point.name,
            SymbolKind::Input,
            false,
        );
        inputs.push(Binding {
            name: point.name.clone(),
            index,
        });
    }
    for point in project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::Out)
    {
        let index = intern(
            &mut symbols,
            &mut lookup,
            &point.name,
            SymbolKind::Output,
            false,
        );
        outputs.push(Binding {
            name: point.name.clone(),
            index,
        });
    }
    for var in &project.vars {
        intern(
            &mut symbols,
            &mut lookup,
            &var.name,
            SymbolKind::Var,
            var.retain,
        );
    }

    let mut ops = Vec::new();
    let mut edges = 0u16;
    let pous = project.all_pous();
    // IL operands are typed by hand; accept any spelling of a declared name.
    let lookup_ci: HashMap<String, u16> = lookup
        .iter()
        .map(|(name, index)| (name.to_ascii_lowercase(), *index))
        .collect();
    // A POU that another POU calls is a routine: it runs where it is called,
    // not on its own. With no calls every POU is a program, which is how the
    // compiler behaved before `CAL` existed.
    let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pou in &pous {
        for line in &pou.il {
            if let IlLine::Instruction { mnemonic, operand } = parse_line(line) {
                if mnemonic == "CAL" {
                    called.insert(operand.trim().to_ascii_uppercase());
                }
            }
        }
    }
    let by_name: HashMap<String, &crate::model::Pou> = pous
        .iter()
        .map(|pou| (pou.name.to_ascii_uppercase(), pou))
        .collect();
    // MAIN is the entry point, as the ZX manual states: the scan runs MAIN and
    // MAIN calls the rest. A project without MAIN (older files, tests) keeps
    // the previous rule so nothing stops running silently.
    let main = pous
        .iter()
        .find(|pou| pou.name.eq_ignore_ascii_case("MAIN"));
    match main {
        Some(main) => lower_pou(main, &by_name, &lookup, &lookup_ci, &mut ops, &mut edges, 0),
        None => {
            for pou in &pous {
                if called.contains(&pou.name.to_ascii_uppercase()) {
                    continue;
                }
                lower_pou(pou, &by_name, &lookup, &lookup_ci, &mut ops, &mut edges, 0);
            }
        }
    }

    let program = Program {
        word_count: WORD_COUNT,
        fb_count: FB_PER_TYPE * 9,
        edge_count: edges,
        ops,
        var_count: symbols.len() as u16,
        input_count: inputs.len() as u16,
        output_count: outputs.len() as u16,
    };
    let warnings = diagnostics
        .into_iter()
        .filter(|item| item.severity == Severity::Warning)
        .collect();

    Ok(Compiled {
        program,
        symbols,
        inputs,
        outputs,
        hash: project_hash(project),
        warnings,
    })
}

/// Stable FNV-1a 64-bit digest of the serialised project, as 16 lowercase hex digits.
pub fn project_hash(project: &Project) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    // The watch list is a view, not the program: pinning an operand must not
    // make a downloaded program look stale.
    let mut canonical = project.clone();
    canonical.watch.clear();
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

fn intern(
    symbols: &mut Vec<Symbol>,
    lookup: &mut HashMap<String, u16>,
    name: &str,
    kind: SymbolKind,
    retain: bool,
) -> u16 {
    let index = symbols.len() as u16;
    symbols.push(Symbol {
        name: name.to_string(),
        index,
        kind,
        retain,
    });
    lookup.insert(name.to_string(), index);
    index
}

/// Lower one POU. `CAL` reaches another POU by lowering its body here, which is
/// what a call means in a scan-based PLC: no locals, no stack frame, the
/// callee's coils and timers are the same ones it owns when it runs alone.
fn lower_pou(
    pou: &crate::model::Pou,
    pous: &HashMap<String, &crate::model::Pou>,
    lookup: &HashMap<String, u16>,
    lookup_ci: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
    depth: u8,
) {
    if depth > 8 {
        return;
    }
    match pou.language {
        PouLanguage::Ld => {
            for section in &pou.sections {
                let mut index = 0;
                while index < section.rungs.len() {
                    if section.rungs[index].cells.is_empty() {
                        let rung = &section.rungs[index];
                        lower_logic(&rung.logic, lookup, ops);
                        store_coil(ops, lookup, &rung.coil);
                        index += 1;
                        continue;
                    }
                    // A run of grid rows. A link in one row makes the next
                    // row its parallel branch, so the run is lowered as a
                    // stack rather than one rung at a time.
                    let mut run: Vec<Vec<crate::grid::Cell>> = Vec::new();
                    while let Some(row) = section.rungs.get(index + run.len()) {
                        if row.cells.is_empty() {
                            break;
                        }
                        run.push(row.cells.clone());
                    }
                    let mut offset = 0;
                    while offset < run.len() {
                        let (used, compute) = lower_grid_rung(&run, offset, lookup, ops, edges);
                        if !compute {
                            store_coil(ops, lookup, &section.rungs[index + offset].coil);
                        }
                        offset += used;
                    }
                    index += run.len();
                }
            }
        }
        PouLanguage::Il => {
            for line in &pou.il {
                if let IlLine::Instruction { mnemonic, operand } = parse_line(line) {
                    if mnemonic == "RET" {
                        // Inlining already ends at the end of the body, so a
                        // trailing RET needs no op.
                        continue;
                    }
                    if mnemonic == "CAL" {
                        // `CAL TON0(IN:=x, PT:=T#1s)` runs a block; a bare name
                        // is a POU call.
                        if let Some((name, args)) = crate::il::parse_call(&operand) {
                            if let Some((kind, instance, mut preset)) =
                                parse_block(&name, &name, "")
                            {
                                let input = args
                                    .iter()
                                    .find(|(key, _)| matches!(key.as_str(), "IN" | "CU" | "CD"))
                                    .map(|(_, value)| value.as_str())
                                    .unwrap_or("FALSE");
                                if let Some((_, value)) = args
                                    .iter()
                                    .find(|(key, _)| matches!(key.as_str(), "PT" | "PV"))
                                {
                                    preset = parse_preset(value, kind.is_timer()).unwrap_or(preset);
                                }
                                push_bit_operand(input, lookup, ops);
                                ops.push(Op::Fb {
                                    kind,
                                    instance,
                                    preset,
                                });
                            }
                            continue;
                        }
                        if let Some(callee) = pous.get(&operand.trim().to_ascii_uppercase()) {
                            lower_pou(callee, pous, lookup, lookup_ci, ops, edges, depth + 1);
                        }
                        continue;
                    }
                    lower_line(&mnemonic, il_operand(&operand, lookup, lookup_ci), ops);
                }
            }
        }
        PouLanguage::Fbd => match pou.fbd_grid.as_ref().filter(|grid| !grid.blocks.is_empty()) {
            Some(grid) => lower_fbd_grid(grid, lookup, ops),
            None => {
                for index in 0..pou.fbd.len() {
                    lower_fbd_line(&pou.fbd, index, lookup, ops);
                }
            }
        },
    }
}

/// Push a bit operand: `TRUE`, a declared symbol, or false when unknown.
fn push_bit_operand(operand: &str, lookup: &HashMap<String, u16>, ops: &mut Vec<Op>) {
    if operand.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(index) = lookup.get(operand.trim()) {
        ops.push(Op::Push(*index));
    } else {
        ops.push(Op::PushConst(false));
    }
}

/// Lower one function-block assignment: build the block from its inputs, then
/// store its result in the output operand.
fn lower_fbd_line(
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
) {
    let Some(line) = lines.get(index) else {
        return;
    };
    lower_fbd_block(lines, index, lookup, ops, 0);
    let (negated, name) = match line.output.trim().strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line.output.trim()),
    };
    if negated {
        ops.push(Op::Not);
    }
    if let Some(target) = lookup.get(name) {
        ops.push(Op::Store(*target));
    }
}

/// Lower a drawn grid. Blocks are evaluated in reading order (row, then
/// column), which is the order the editor enforces: a block may only take its
/// input from something to its left or above.
fn lower_fbd_grid(grid: &crate::model::FbdGrid, lookup: &HashMap<String, u16>, ops: &mut Vec<Op>) {
    let mut order = grid
        .blocks
        .iter()
        .enumerate()
        .collect::<Vec<(usize, &crate::model::FbdBlock)>>();
    order.sort_by_key(|(_, block)| (block.row, block.col));
    for (index, block) in order {
        lower_fbd_grid_block(grid, index, lookup, ops);
        // The output cell to the right of the anchor stores the result.
        if let crate::model::FbdCell::Out { operand } = grid.cell(block.row, block.col + 1) {
            let (negated, name) = split_negation(&operand);
            if negated {
                ops.push(Op::Not);
            }
            let name = name.trim();
            // A word target is written by the compute block itself.
            if crate::il::word_operand(name).is_none() {
                if let Some(slot) = lookup.get(name) {
                    ops.push(Op::Store(*slot));
                }
            }
        }
    }
}

/// `!start` -> (true, "start").
fn split_negation(operand: &str) -> (bool, &str) {
    match operand.trim().strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, operand.trim()),
    }
}

/// Push one grid block's result on the stack.
fn lower_fbd_grid_block(
    grid: &crate::model::FbdGrid,
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
) {
    let Some(block) = grid.blocks.get(index) else {
        ops.push(Op::PushConst(false));
        return;
    };
    let name = block.kind.trim().to_ascii_uppercase();
    let mut inputs = Vec::new();
    for offset in 0..block.ins.max(1) {
        let row = block.row + offset;
        let mut operand = grid
            .input_at(row, block.col.saturating_sub(1))
            .unwrap_or_default();
        if operand.trim().is_empty() {
            // Chaining: the value on the wire is the block to the left whose
            // output leaves on this row.
            operand = chained_source(grid, index, row);
        }
        inputs.push(operand);
    }
    match name.as_str() {
        "AND" | "OR" | "XOR" => {
            if inputs.is_empty() {
                ops.push(Op::PushConst(name == "AND"));
            } else {
                for (position, input) in inputs.iter().enumerate() {
                    lower_fbd_grid_bit(input, grid, index, lookup, ops, 0);
                    if position > 0 {
                        ops.push(match name.as_str() {
                            "AND" => Op::And,
                            "OR" => Op::Or,
                            _ => Op::Xor,
                        });
                    }
                }
            }
        }
        "NOT" => {
            lower_fbd_grid_bit(&inputs[0], grid, index, lookup, ops, 0);
            ops.push(Op::Not);
        }
        _ => {
            if let Some((kind, instance, preset)) = parse_block(&name, &name, &block.preset) {
                if kind.is_latch() {
                    lower_fbd_grid_bit(
                        inputs.first().map(String::as_str).unwrap_or("FALSE"),
                        grid,
                        index,
                        lookup,
                        ops,
                        0,
                    );
                    lower_fbd_grid_bit(
                        inputs.get(1).map(String::as_str).unwrap_or("FALSE"),
                        grid,
                        index,
                        lookup,
                        ops,
                        0,
                    );
                    ops.push(Op::Latch { kind, instance });
                    return;
                }
                lower_fbd_grid_bit(
                    inputs.first().map(String::as_str).unwrap_or("FALSE"),
                    grid,
                    index,
                    lookup,
                    ops,
                    0,
                );
                ops.push(Op::Fb {
                    kind,
                    instance,
                    preset,
                });
            } else if let Some(compare) = compare_op(&name) {
                let (Some(left), Some(right)) = (
                    word_operand_value(inputs.first().map(String::as_str).unwrap_or("0")),
                    word_operand_value(inputs.get(1).map(String::as_str).unwrap_or("0")),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                emit_word(ops, left);
                emit_word(ops, right);
                ops.push(compare);
            } else if let Some(math) = MathOp::parse(&name) {
                // A compute block: `row` holds A, the next holds B, and the
                // result goes to the word named on its output cell.
                let target = match grid.cell(block.row, block.col + 1) {
                    crate::model::FbdCell::Out { operand } => operand,
                    _ => String::new(),
                };
                let (Some(left), Some(right), Some(target)) = (
                    word_operand_value(inputs.first().map(String::as_str).unwrap_or("0")),
                    word_operand_value(inputs.get(1).map(String::as_str).unwrap_or("0")),
                    crate::il::word_operand(&target),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                ops.push(Op::PushConst(true));
                ops.push(Op::Compute {
                    op: math,
                    target,
                    left,
                    right,
                });
            } else {
                ops.push(Op::PushConst(false));
            }
        }
    }
}

/// The block whose output leaves on `row` to the left of `index`, if any.
fn chained_source(grid: &crate::model::FbdGrid, index: usize, row: usize) -> String {
    let mut best: Option<(usize, String)> = None;
    for (position, block) in grid.blocks.iter().enumerate() {
        if position == index || block.row != row {
            continue;
        }
        if let crate::model::FbdCell::Out { operand } = grid.cell(block.row, block.col + 1) {
            if best
                .as_ref()
                .map(|(col, _)| block.col < *col)
                .unwrap_or(true)
            {
                best = Some((block.col, operand));
            }
        }
    }
    best.map(|(_, operand)| operand).unwrap_or_default()
}

/// A bit input of a grid block: `@n` names another block, `!` negates, and an
/// empty string is a chain that produced nothing.
fn lower_fbd_grid_bit(
    input: &str,
    grid: &crate::model::FbdGrid,
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let (negated, rest) = split_negation(input);
    let text = rest.trim();
    if depth < 8 {
        if let Some(reference) = text.strip_prefix('@') {
            if let Ok(number) = reference.parse::<usize>() {
                let target = number.saturating_sub(1);
                if target < index {
                    lower_fbd_grid_block(grid, target, lookup, ops);
                    if negated {
                        ops.push(Op::Not);
                    }
                    return;
                }
            }
        }
    }
    if text.is_empty() {
        ops.push(Op::PushConst(false));
    } else if text.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(slot) = lookup.get(text) {
        ops.push(Op::PushNext(*slot));
    } else {
        ops.push(Op::PushConst(false));
    }
    if negated {
        ops.push(Op::Not);
    }
}

/// Emit one block and leave its result on the stack.
fn lower_fbd_block(
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let Some(line) = lines.get(index) else {
        ops.push(Op::PushConst(false));
        return;
    };
    if depth > 8 {
        ops.push(Op::PushConst(false));
        return;
    }
    let block = line.block.trim().to_ascii_uppercase();
    match block.as_str() {
        "AND" | "OR" | "XOR" => {
            if line.inputs.is_empty() {
                ops.push(Op::PushConst(block == "AND"));
            } else {
                for (position, input) in line.inputs.iter().enumerate() {
                    lower_fbd_bit(input, lines, index, lookup, ops, depth);
                    if position > 0 {
                        ops.push(match block.as_str() {
                            "AND" => Op::And,
                            "OR" => Op::Or,
                            _ => Op::Xor,
                        });
                    }
                }
            }
        }
        "NOT" => {
            lower_fbd_bit(
                line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                lines,
                index,
                lookup,
                ops,
                depth,
            );
            ops.push(Op::Not);
        }
        _ => {
            if let Some((kind, instance, preset)) = parse_block(&block, &block, &line.preset) {
                if kind.is_latch() {
                    lower_fbd_bit(
                        line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                        lines,
                        index,
                        lookup,
                        ops,
                        depth,
                    );
                    lower_fbd_bit(
                        line.inputs.get(1).map(String::as_str).unwrap_or("FALSE"),
                        lines,
                        index,
                        lookup,
                        ops,
                        depth,
                    );
                    ops.push(Op::Latch { kind, instance });
                    return;
                }
                lower_fbd_bit(
                    line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                    lines,
                    index,
                    lookup,
                    ops,
                    depth,
                );
                ops.push(Op::Fb {
                    kind,
                    instance,
                    preset,
                });
            } else if let Some(compare) = compare_op(&block) {
                let (Some(left), Some(right)) = (
                    word_operand_value(line.inputs.first().map(String::as_str).unwrap_or("0")),
                    word_operand_value(line.inputs.get(1).map(String::as_str).unwrap_or("0")),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                emit_word(ops, left);
                emit_word(ops, right);
                ops.push(compare);
            } else {
                ops.push(Op::PushConst(false));
            }
        }
    }
}

/// A bit input: `@n` is the value on line `n`'s output pin, lowered here so a
/// block can feed another without naming an intermediate symbol.
fn lower_fbd_bit(
    input: &str,
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let text = input.trim();
    // A leading `!` is the negated input/output the manual draws as a circle.
    if let Some(rest) = text.strip_prefix('!') {
        lower_fbd_bit(rest, lines, index, lookup, ops, depth);
        ops.push(Op::Not);
        return;
    }
    if let Some(reference) = text.strip_prefix('@') {
        if let Ok(number) = reference.parse::<usize>() {
            let target = number.saturating_sub(1);
            if target < index {
                lower_fbd_block(lines, target, lookup, ops, depth + 1);
                return;
            }
        }
        ops.push(Op::PushConst(false));
        return;
    }
    if text.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(slot) = lookup.get(text) {
        ops.push(Op::PushNext(*slot));
    } else {
        ops.push(Op::PushConst(false));
    }
}

/// Lower one grid row to ops, leaving exactly one power bit on the stack.
///
/// Power starts true at the rail. A contact pops nothing: it pushes its operand
/// and ANDs it with the power so far. A box consumes the power as its input and
/// pushes `Q`, so the rung continues from the block's output. A row with no
/// element is a true rung.
fn lower_cells(
    cells: &[crate::grid::Cell],
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
) -> bool {
    let mut has_power = false;
    for cell in cells {
        match cell {
            crate::grid::Cell::Wire | crate::grid::Cell::Link => continue,
            crate::grid::Cell::Contact { var, negated } => {
                let index = lookup.get(var.as_str()).copied().unwrap_or(0);
                ops.push(Op::Push(index));
                if *negated {
                    ops.push(Op::Not);
                }
                if has_power {
                    ops.push(Op::And);
                }
            }
            crate::grid::Cell::Block {
                block,
                instance,
                preset,
            } => {
                if !has_power {
                    ops.push(Op::PushConst(true));
                }
                if let Some((kind, instance, preset)) = parse_block(block, instance, preset) {
                    ops.push(Op::Fb {
                        kind,
                        instance,
                        preset,
                    });
                } else {
                    // Validation refused this cell; keep the rung alive.
                    ops.push(Op::PushConst(false));
                }
            }
            crate::grid::Cell::Compare { op, left, right } => {
                let (Some(left), Some(right)) =
                    (word_operand_value(left), word_operand_value(right))
                else {
                    continue;
                };
                if !has_power {
                    ops.push(Op::PushConst(true));
                }
                emit_word(ops, left);
                emit_word(ops, right);
                match compare_op(op) {
                    Some(compare) => ops.push(compare),
                    None => continue,
                }
                if has_power {
                    ops.push(Op::And);
                }
                has_power = true;
                continue;
            }
            crate::grid::Cell::Compute {
                op,
                target,
                left,
                right,
            } => {
                let (Some(left), Some(right)) =
                    (word_operand_value(left), word_operand_value(right))
                else {
                    continue;
                };
                let (Some(math), Some(target)) =
                    (MathOp::parse(op), crate::il::word_operand(target))
                else {
                    continue;
                };
                if !has_power {
                    ops.push(Op::PushConst(true));
                }
                ops.push(Op::Compute {
                    op: math,
                    target,
                    left,
                    right,
                });
                // A compute coil ends the rung: there is no coil output.
                return true;
            }
            // Validation refuses these until their semantics exist.
            crate::grid::Cell::Edge { var, rising } => {
                let index = lookup.get(var.as_str()).copied().unwrap_or(0);
                ops.push(Op::Edge {
                    slot: *edges,
                    var: index,
                    rising: *rising,
                });
                *edges = edges.saturating_add(1);
                if has_power {
                    ops.push(Op::And);
                }
            }
        }
        has_power = true;
    }
    if !has_power {
        ops.push(Op::PushConst(true));
    }
    false
}

/// Lower a row and, when it links, the branch below it. Returns rows used.
fn lower_grid_rung(
    run: &[Vec<crate::grid::Cell>],
    index: usize,
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
) -> (usize, bool) {
    let Some(cells) = run.get(index) else {
        ops.push(Op::PushConst(true));
        return (1, false);
    };
    let compute = lower_cells(cells, lookup, ops, edges);
    let mut used = 1;
    if cells
        .iter()
        .any(|cell| matches!(cell, crate::grid::Cell::Link))
        && index + 1 < run.len()
    {
        used += lower_grid_rung(run, index + 1, lookup, ops, edges).0;
        ops.push(Op::Or);
    }
    (used, compute)
}

/// A word-valued operand of a compare contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordOperand {
    Word(u16),
    Const(i16),
    /// The block's `ET` / `CV`.
    Block(u16),
    /// The block's `PT` / `PV`.
    BlockPreset(u16),
}

/// Resolve `MW3`, `T#4s`, `-12`, `TON0.ET` or `CTU1.CV`.
pub fn word_operand_value(text: &str) -> Option<WordOperand> {
    let text = text.trim();
    if let Some(rest) = text
        .strip_suffix(".PT")
        .or_else(|| text.strip_suffix(".PV"))
        .or_else(|| text.strip_suffix(".pt"))
        .or_else(|| text.strip_suffix(".pv"))
    {
        let (_, instance, _) = parse_block(rest, rest, "")?;
        return Some(WordOperand::BlockPreset(instance));
    }
    if let Some(rest) = text
        .strip_suffix(".ET")
        .or_else(|| text.strip_suffix(".CV"))
        .or_else(|| text.strip_suffix(".et"))
        .or_else(|| text.strip_suffix(".cv"))
    {
        let (kind, instance, _) = parse_block(rest, rest, "")?;
        if !kind.is_timer() {
            // `.ET` of a counter and `.CV` of a timer are the same field.
        }
        return Some(WordOperand::Block(instance));
    }
    if let Some(index) = crate::il::word_operand(text) {
        return Some(WordOperand::Word(index));
    }
    if text.starts_with("T#") || text.starts_with("t#") {
        return Some(WordOperand::Const(parse_preset(text, true)?));
    }
    text.parse::<i16>().ok().map(WordOperand::Const)
}

fn emit_word(ops: &mut Vec<Op>, value: WordOperand) {
    match value {
        WordOperand::Word(index) => ops.push(Op::LoadWord(index)),
        WordOperand::Const(value) => ops.push(Op::LoadConst(value)),
        WordOperand::Block(index) => ops.push(Op::FbWord(index)),
        WordOperand::BlockPreset(index) => ops.push(Op::FbPreset(index)),
    }
}

pub fn compare_op(name: &str) -> Option<Op> {
    match name.trim().to_ascii_uppercase().as_str() {
        "GT" => Some(Op::CmpGt),
        "GE" => Some(Op::CmpGe),
        "EQ" => Some(Op::CmpEq),
        "NE" => Some(Op::CmpNe),
        "LE" => Some(Op::CmpLe),
        "LT" => Some(Op::CmpLt),
        _ => None,
    }
}

/// `TON` + `TON2` + `T#4s` -> kind, instance index, preset in ms or counts.
pub fn parse_block(block: &str, instance: &str, preset: &str) -> Option<(FbKind, u16, i16)> {
    let instance = instance.trim();
    let kind = FbKind::parse(block).or_else(|| {
        let letters: String = instance
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        FbKind::parse(&letters)
    })?;
    let digits: String = instance
        .chars()
        .skip_while(|c| c.is_ascii_alphabetic())
        .collect();
    let number = digits.parse::<u16>().unwrap_or(0);
    if number >= FB_PER_TYPE {
        return None;
    }
    let index = kind.code() * FB_PER_TYPE + number;
    let preset = parse_preset(preset, kind.is_timer())?;
    Some((kind, index, preset))
}

/// Presets: `T#4s`, `T#1s500ms`, `T#2m`, or a count. Milliseconds are capped at
/// what the 16-bit `ET` can hold, which is just under 33 seconds.
pub fn parse_preset(text: &str, timer: bool) -> Option<i16> {
    let text = text.trim();
    if timer {
        if text.is_empty() {
            return Some(1000);
        }
        let body = text
            .strip_prefix("T#")
            .or_else(|| text.strip_prefix("t#"))
            .unwrap_or(text);
        let mut total = 0i32;
        let mut digits = String::new();
        let mut rest = body;
        while !rest.is_empty() {
            let (number, tail) = match rest.find(|c: char| c.is_ascii_alphabetic()) {
                Some(index) => rest.split_at(index),
                None => (rest, ""),
            };
            digits.clear();
            digits.push_str(number);
            let value: i32 = digits.parse().ok()?;
            let (unit, tail) = if let Some(tail) = tail.strip_prefix("ms") {
                (1, tail)
            } else if let Some(tail) = tail.strip_prefix('s') {
                (1000, tail)
            } else {
                let tail = tail.strip_prefix('m')?;
                (60_000, tail)
            };
            total += value * unit;
            rest = tail;
        }
        Some(total.clamp(0, i32::from(i16::MAX)) as i16)
    } else {
        if text.is_empty() {
            return Some(10);
        }
        text.parse::<i16>().ok()
    }
}

/// Resolve an IL operand: `MWn`, a decimal literal, or a declared symbol.
fn il_operand(
    operand: &str,
    lookup: &HashMap<String, u16>,
    lookup_ci: &HashMap<String, u16>,
) -> IlOperand {
    let text = operand.trim();
    let bare = text.strip_prefix('%').unwrap_or(text);
    let word = bare
        .strip_prefix("MW")
        .or_else(|| bare.strip_prefix("mw"))
        .and_then(|index| index.parse::<u16>().ok());
    if let Some(index) = word {
        if index < WORD_COUNT {
            return IlOperand::Word(index);
        }
    }
    if let Ok(value) = text.parse::<i16>() {
        return IlOperand::Literal(value);
    }
    let index = lookup
        .get(text)
        .or_else(|| lookup_ci.get(&text.to_ascii_lowercase()))
        .copied()
        .unwrap_or(0);
    IlOperand::Bit(index)
}

fn store_coil(ops: &mut Vec<Op>, lookup: &HashMap<String, u16>, coil: &crate::model::Coil) {
    // `CTU0.R` and `CTD0.LD` are coils on a counter, not on a bit.
    if let Some((rest, load)) = coil
        .var
        .strip_suffix(".R")
        .map(|rest| (rest, false))
        .or_else(|| coil.var.strip_suffix(".LD").map(|rest| (rest, true)))
    {
        if let Some((_, instance, _)) = parse_block(rest, rest, "") {
            ops.push(if load {
                Op::FbLoad(instance)
            } else {
                Op::FbReset(instance)
            });
            return;
        }
    }
    let index = lookup.get(coil.var.as_str()).copied().unwrap_or(0);
    let kind = coil.kind;
    if kind == CoilKind::Negated {
        ops.push(Op::Not);
    }
    ops.push(match kind {
        CoilKind::Normal | CoilKind::Negated => Op::Store(index),
        CoilKind::Set => Op::Set(index),
        CoilKind::Reset => Op::Reset(index),
    });
}

fn lower_logic(logic: &Logic, lookup: &HashMap<String, u16>, ops: &mut Vec<Op>) {
    match logic {
        Logic::Const { value } => ops.push(Op::PushConst(*value)),
        Logic::Contact { var, negated } => {
            ops.push(Op::Push(lookup.get(var.as_str()).copied().unwrap_or(0)));
            if *negated {
                ops.push(Op::Not);
            }
        }
        Logic::Series { items } => lower_nary(items, lookup, ops, Op::And, Op::PushConst(true)),
        Logic::Parallel { items } => lower_nary(items, lookup, ops, Op::Or, Op::PushConst(false)),
    }
}

fn lower_nary(
    items: &[Logic],
    lookup: &HashMap<String, u16>,
    ops: &mut Vec<Op>,
    combine: Op,
    empty: Op,
) {
    if items.is_empty() {
        ops.push(empty);
        return;
    }
    for (index, item) in items.iter().enumerate() {
        lower_logic(item, lookup, ops);
        if index > 0 {
            ops.push(combine);
        }
    }
}
