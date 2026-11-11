//! Validation of the project model. Never panics; every problem becomes a
//! diagnostic so the UI can show all of them at once.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::il::{parse_line, word_operand, IlLine, UNSUPPORTED};
use crate::model::{IoDirection, Logic, PouLanguage, Project, Section};
use crate::SCHEMA_VERSION;

/// Maximum accepted logic nesting depth.
const MAX_DEPTH: usize = 64;
/// Maximum number of io points plus variables.
const MAX_SYMBOLS: usize = 4096;
/// Maximum number of rungs across all sections.
const MAX_RUNGS: usize = 4096;
/// Maximum project-name length, in characters.
const MAX_NAME: usize = 64;

const RESERVED: [&str; 5] = ["alive", "scan-count", "scan-time-us", "fault", "overruns"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub path: String,
}

impl Diagnostic {
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }

    pub fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolClass {
    Input,
    Output,
    Var,
}

pub fn validate(project: &Project) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if project.schema != SCHEMA_VERSION {
        diagnostics.push(Diagnostic::error(
            "schema",
            format!(
                "schema {} is not supported (expected {SCHEMA_VERSION})",
                project.schema
            ),
            "schema",
        ));
    }

    if project.name.trim().is_empty() {
        diagnostics.push(Diagnostic::error(
            "name",
            "project name must not be empty",
            "name",
        ));
    } else if project.name.chars().count() > MAX_NAME {
        diagnostics.push(Diagnostic::error(
            "name",
            format!("project name is longer than {MAX_NAME} characters"),
            "name",
        ));
    }

    let symbol_count = project.io.len() + project.vars.len();
    if symbol_count > MAX_SYMBOLS {
        diagnostics.push(Diagnostic::error(
            "too_large",
            format!("project declares {symbol_count} symbols (maximum {MAX_SYMBOLS})"),
            "project",
        ));
    }

    let mut symbols: HashMap<&str, SymbolClass> = HashMap::with_capacity(symbol_count);

    for (index, point) in project.io.iter().enumerate() {
        let path = format!("io[{index}].name");
        check_name(&point.name, &path, &mut diagnostics);
        let class = match point.direction {
            IoDirection::In => SymbolClass::Input,
            IoDirection::Out => SymbolClass::Output,
        };
        register(&mut symbols, &point.name, class, &path, &mut diagnostics);
    }

    for (index, var) in project.vars.iter().enumerate() {
        let path = format!("vars[{index}].name");
        check_name(&var.name, &path, &mut diagnostics);
        register(
            &mut symbols,
            &var.name,
            SymbolClass::Var,
            &path,
            &mut diagnostics,
        );
    }

    let pous = project.all_pous();
    let known: HashSet<String> = project
        .io
        .iter()
        .map(|point| point.name.to_ascii_lowercase())
        .chain(project.vars.iter().map(|var| var.name.to_ascii_lowercase()))
        .collect();
    for (index, pou) in pous.iter().enumerate() {
        match pou.language {
            PouLanguage::Ld => {}
            PouLanguage::Il => {
                for (line_index, line) in pou.il.iter().enumerate() {
                    let path = format!("pous[{index}].il[{line_index}]");
                    match parse_line(line) {
                        IlLine::Instruction { mnemonic, operand } => {
                            if UNSUPPORTED.contains(&mnemonic.as_str()) {
                                diagnostics.push(Diagnostic::error(
                                    "il_unsupported",
                                    format!("'{mnemonic}' is not implemented yet"),
                                    path,
                                ));
                            } else if word_operand(&operand).is_none()
                                && operand.trim().parse::<i16>().is_err()
                                && !known.contains(&operand.to_ascii_lowercase())
                            {
                                diagnostics.push(Diagnostic::error(
                                    "il_operand",
                                    format!(
                                        "'{operand}' is not a declared input, output or variable"
                                    ),
                                    path,
                                ));
                            }
                        }
                        IlLine::Label(name) => diagnostics.push(Diagnostic::error(
                            "il_label",
                            format!("label '{name}' needs jumps, which are not implemented yet"),
                            path,
                        )),
                        IlLine::Bad(message) => {
                            diagnostics.push(Diagnostic::error("il", message, path))
                        }
                        IlLine::Blank | IlLine::Comment => {}
                    }
                }
            }
            PouLanguage::Fbd => diagnostics.push(Diagnostic::error(
                "language",
                format!(
                    "POU '{}' is FBD; that compiler is not implemented yet",
                    pou.name
                ),
                format!("pous[{index}].language"),
            )),
        }
        if pou.name.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "pou_name",
                "POU name must not be empty",
                format!("pous[{index}].name"),
            ));
        }
    }
    let sections: Vec<&Section> = pous.iter().flat_map(|pou| pou.sections.iter()).collect();

    let edge_contacts = pous
        .iter()
        .flat_map(|pou| pou.sections.iter())
        .flat_map(|section| section.rungs.iter())
        .flat_map(|rung| rung.cells.iter())
        .filter(|cell| matches!(cell, crate::grid::Cell::Edge { .. }))
        .count();
    if edge_contacts > usize::from(crate::compile::MAX_EDGE_CONTACTS) {
        diagnostics.push(Diagnostic::error(
            "edge_limit",
            format!(
                "{edge_contacts} edge contacts; at most {} are supported",
                crate::compile::MAX_EDGE_CONTACTS
            ),
            "sections",
        ));
    }

    // An IL-only project has no rungs; that is a program, not an empty one.
    let il_only = pous
        .iter()
        .any(|pou| pou.language == PouLanguage::Il && !pou.il.is_empty());
    if sections.is_empty() && !il_only {
        diagnostics.push(Diagnostic::error(
            "no_sections",
            "project has no sections",
            "sections",
        ));
    }

    let rung_count: usize = sections.iter().map(|section| section.rungs.len()).sum();
    if rung_count > MAX_RUNGS {
        diagnostics.push(Diagnostic::error(
            "too_large",
            format!("project declares {rung_count} rungs (maximum {MAX_RUNGS})"),
            "project",
        ));
    }

    let mut section_names: HashSet<&str> = HashSet::with_capacity(sections.len());
    for (section_index, section) in sections.iter().enumerate() {
        let section_path = format!("sections[{section_index}]");
        if section.name.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "section",
                "section name must not be empty",
                format!("{section_path}.name"),
            ));
        } else if !section_names.insert(&section.name) {
            diagnostics.push(Diagnostic::error(
                "section",
                format!("duplicate section name `{}`", section.name),
                format!("{section_path}.name"),
            ));
        }

        let mut coils: HashSet<&str> = HashSet::new();
        for (rung_index, rung) in section.rungs.iter().enumerate() {
            let rung_path = format!("{section_path}.rungs[{rung_index}]");
            let logic_path = format!("{rung_path}.logic");
            // The ladder grid, when the editor wrote one, is compiled instead
            // of the tree. A cell kind without a compiler is named here.
            for (cell_index, cell) in rung.cells.iter().enumerate() {
                if let Some(feature) = cell.unsupported() {
                    diagnostics.push(Diagnostic::error(
                        "cell_unsupported",
                        format!("{feature} are not implemented yet"),
                        format!("{rung_path}.cells[{cell_index}]"),
                    ));
                }
                if matches!(cell, crate::grid::Cell::Link) && rung_index + 1 == section.rungs.len()
                {
                    diagnostics.push(Diagnostic::error(
                        "link_without_branch",
                        "a link has no row below to join",
                        format!("{rung_path}.cells[{cell_index}]"),
                    ));
                }
                if let crate::grid::Cell::Block {
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
                            format!("{rung_path}.cells[{cell_index}]"),
                        ));
                    }
                } else if let crate::grid::Cell::Compare { op, left, right } = cell {
                    if crate::compile::compare_op(op).is_none() {
                        diagnostics.push(Diagnostic::error(
                            "cell_compare",
                            format!(
                                "'{op}' is not a compare operator; use GT, GE, EQ, NE, LE or LT"
                            ),
                            format!("{rung_path}.cells[{cell_index}]"),
                        ));
                    }
                    for operand in [left, right] {
                        if crate::compile::word_operand_value(operand).is_none() {
                            diagnostics.push(Diagnostic::error(
                                "cell_compare_operand",
                                format!(
                                    "'{operand}' is not a word, a number, a time or a block member"
                                ),
                                format!("{rung_path}.cells[{cell_index}]"),
                            ));
                        }
                    }
                } else if let crate::grid::Cell::Compute {
                    op,
                    target,
                    left,
                    right,
                } = cell
                {
                    if crate::compile::MathOp::parse(op).is_none()
                        || crate::il::word_operand(target).is_none()
                    {
                        diagnostics.push(Diagnostic::error(
                            "cell_compute",
                            format!(
                                "'{target}':='{op}' is not a compute coil; use MW0-15 and ADD, SUB, MUL, DIV or MOVE"
                            ),
                            format!("{rung_path}.cells[{cell_index}]"),
                        ));
                    }
                    for operand in [left, right] {
                        if crate::compile::word_operand_value(operand).is_none() {
                            diagnostics.push(Diagnostic::error(
                                "cell_compute_operand",
                                format!(
                                    "'{operand}' is not a word, a number, a time or a block member"
                                ),
                                format!("{rung_path}.cells[{cell_index}]"),
                            ));
                        }
                    }
                } else if let Some(operand) = cell.operand() {
                    let known = symbols
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case(operand));
                    if !known {
                        diagnostics.push(Diagnostic::error(
                            "cell_operand",
                            format!("'{operand}' is not a declared input, output or variable"),
                            format!("{rung_path}.cells[{cell_index}]"),
                        ));
                    }
                }
            }
            if logic_depth(&rung.logic) > MAX_DEPTH {
                diagnostics.push(Diagnostic::error(
                    "too_deep",
                    format!("logic nesting is deeper than {MAX_DEPTH} levels"),
                    &logic_path,
                ));
            } else {
                check_logic(&rung.logic, &logic_path, &symbols, &mut diagnostics);
            }

            let coil_path = format!("{rung_path}.coil.var");
            match symbols.get(rung.coil.var.as_str()) {
                None => diagnostics.push(Diagnostic::error(
                    "unknown_symbol",
                    format!("coil target `{}` is not declared", rung.coil.var),
                    &coil_path,
                )),
                Some(SymbolClass::Input) => diagnostics.push(Diagnostic::error(
                    "coil_target",
                    format!(
                        "`{}` is an input and cannot be a coil target",
                        rung.coil.var
                    ),
                    &coil_path,
                )),
                Some(_) => {
                    if !coils.insert(&rung.coil.var) {
                        diagnostics.push(Diagnostic::error(
                            "duplicate_coil",
                            format!(
                                "`{}` is driven by more than one coil in this section",
                                rung.coil.var
                            ),
                            &coil_path,
                        ));
                    }
                }
            }
        }
    }

    diagnostics
}

fn register<'a>(
    symbols: &mut HashMap<&'a str, SymbolClass>,
    name: &'a str,
    class: SymbolClass,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if symbols.contains_key(name) {
        diagnostics.push(Diagnostic::error(
            "duplicate_symbol",
            format!("symbol `{name}` is declared more than once"),
            path,
        ));
        return;
    }
    symbols.insert(name, class);
}

fn check_name(name: &str, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if !valid_name(name) {
        diagnostics.push(Diagnostic::error(
            "bad_name",
            format!("`{name}` must match ^[a-z][a-z0-9_-]{{0,31}}$"),
            path,
        ));
    } else if RESERVED.contains(&name) {
        diagnostics.push(Diagnostic::error(
            "reserved_name",
            format!("`{name}` is reserved by the runtime"),
            path,
        ));
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    let mut length = 1;
    for ch in chars {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-') {
            return false;
        }
        length += 1;
    }
    length <= 32
}

fn logic_depth(root: &Logic) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(root, 1usize)];
    while let Some((node, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        if depth > MAX_DEPTH {
            return depth;
        }
        if let Logic::Series { items } | Logic::Parallel { items } = node {
            for item in items {
                stack.push((item, depth + 1));
            }
        }
    }
    deepest
}

fn check_logic(
    logic: &Logic,
    path: &str,
    symbols: &HashMap<&str, SymbolClass>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match logic {
        Logic::Const { .. } => {}
        Logic::Contact { var, .. } => {
            if !symbols.contains_key(var.as_str()) {
                diagnostics.push(Diagnostic::error(
                    "unknown_symbol",
                    format!("contact `{var}` is not declared"),
                    format!("{path}.var"),
                ));
            }
        }
        Logic::Series { items } => {
            check_items(items, path, true, symbols, diagnostics);
        }
        Logic::Parallel { items } => {
            check_items(items, path, false, symbols, diagnostics);
        }
    }
}

fn check_items(
    items: &[Logic],
    path: &str,
    series: bool,
    symbols: &HashMap<&str, SymbolClass>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if items.is_empty() {
        if series {
            diagnostics.push(Diagnostic::warning(
                "empty_series",
                "empty series compiles as true",
                path,
            ));
        } else {
            diagnostics.push(Diagnostic::warning(
                "empty_parallel",
                "empty parallel compiles as false",
                path,
            ));
        }
    }
    for (index, item) in items.iter().enumerate() {
        check_logic(
            item,
            &format!("{path}.items[{index}]"),
            symbols,
            diagnostics,
        );
    }
}
