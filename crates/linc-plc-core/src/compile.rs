//! Validation and bytecode lowering.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::il::{lower_line, parse_line, IlLine};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    PushConst(bool),
    Push(u16),
    Not,
    And,
    Or,
    Xor,
    Store(u16),
    Set(u16),
    Reset(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub ops: Vec<Op>,
    /// Value slots in the scan image: inputs, then outputs, then variables.
    pub var_count: u16,
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
    let pous = project.all_pous();
    // IL operands are typed by hand; accept any spelling of a declared name.
    let lookup_ci: HashMap<String, u16> = lookup
        .iter()
        .map(|(name, index)| (name.to_ascii_lowercase(), *index))
        .collect();
    for pou in &pous {
        match pou.language {
            PouLanguage::Ld => {
                for section in &pou.sections {
                    let mut index = 0;
                    while index < section.rungs.len() {
                        if section.rungs[index].cells.is_empty() {
                            let rung = &section.rungs[index];
                            lower_logic(&rung.logic, &lookup, &mut ops);
                            store_coil(&mut ops, &lookup, &rung.coil);
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
                            let (logic, used) = crate::grid::lower_rung(&run, offset);
                            lower_logic(&logic, &lookup, &mut ops);
                            store_coil(&mut ops, &lookup, &section.rungs[index + offset].coil);
                            offset += used;
                        }
                        index += run.len();
                    }
                }
            }
            PouLanguage::Il => {
                for line in &pou.il {
                    if let IlLine::Instruction { mnemonic, operand } = parse_line(line) {
                        let index = lookup
                            .get(operand.as_str())
                            .or_else(|| lookup_ci.get(&operand.to_ascii_lowercase()))
                            .copied()
                            .unwrap_or(0);
                        lower_line(&mnemonic, index, &mut ops);
                    }
                }
            }
            // Validation refuses these before lowering runs.
            PouLanguage::Fbd => {}
        }
    }

    let program = Program {
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

    let bytes = serde_json::to_vec(project).unwrap_or_default();
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

fn store_coil(ops: &mut Vec<Op>, lookup: &HashMap<String, u16>, coil: &crate::model::Coil) {
    let index = lookup.get(coil.var.as_str()).copied().unwrap_or(0);
    ops.push(match coil.kind {
        CoilKind::Normal => Op::Store(index),
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
