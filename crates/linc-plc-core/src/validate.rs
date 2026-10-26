//! Validation of the project model. Never panics; every problem becomes a
//! diagnostic so the UI can show all of them at once.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{IoDirection, Logic, Project};
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

    if project.sections.is_empty() {
        diagnostics.push(Diagnostic::error(
            "no_sections",
            "project has no sections",
            "sections",
        ));
    }

    let rung_count: usize = project
        .sections
        .iter()
        .map(|section| section.rungs.len())
        .sum();
    if rung_count > MAX_RUNGS {
        diagnostics.push(Diagnostic::error(
            "too_large",
            format!("project declares {rung_count} rungs (maximum {MAX_RUNGS})"),
            "project",
        ));
    }

    let mut section_names: HashSet<&str> = HashSet::with_capacity(project.sections.len());
    for (section_index, section) in project.sections.iter().enumerate() {
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
