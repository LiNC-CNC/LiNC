//! Validation of the project model. Never panics; every problem becomes a
//! diagnostic so the UI can show all of them at once.

use std::collections::{HashMap, HashSet};

use crate::il::{parse_line, IlLine};
use crate::model::{
    FbdGrid, IoDirection, IoKind, Logic, PouLanguage, Project, Rung, Section, VarDecl, VarType,
};
use crate::profile::instr::{Ctx, Instr};
use crate::profile::Profile;
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

pub use crate::diagnostic::{Diagnostic, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolClass {
    Input,
    Output,
    Var,
}

/// The project facts the profile's instruction rules read, bundled once and lent
/// to each check. The validator owns them; nothing here decides a rule.
#[derive(Clone, Copy)]
struct Facts<'a> {
    profile: &'a dyn crate::profile::Profile,
    known: &'a HashSet<String>,
    word_slots: &'a HashMap<String, u16>,
    io_kinds: &'a HashMap<&'a str, IoKind>,
    var_types: &'a HashMap<String, VarType>,
    word_vars: &'a [crate::profile::layout::WordVar],
    instances: &'a HashMap<String, String>,
    fb_vars: &'a HashMap<String, &'a [VarDecl]>,
    pou_names: &'a HashSet<String>,
    section_rungs: &'a [Rung],
    labels: &'a HashSet<String>,
    grid: Option<&'a FbdGrid>,
}

impl Facts<'_> {
    fn ctx<'b>(&'b self, path: &'b str) -> Ctx<'b> {
        Ctx {
            path,
            profile: self.profile,
            known: self.known,
            word_slots: self.word_slots,
            io_kinds: self.io_kinds,
            var_types: self.var_types,
            word_vars: self.word_vars,
            instances: self.instances,
            fb_vars: self.fb_vars,
            pou_names: self.pou_names,
            section_rungs: self.section_rungs,
            labels: self.labels,
            grid: self.grid,
        }
    }
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

    // The document's dialect, chosen once: validation is a compile-time pass and
    // may ask the profile; the scan never does.
    let profile = crate::profile::for_dialect(project.profile);

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

    // A point's declared kind decides what it may be used as. `Word` and
    // `Float` both live in the word bank (a float pin is its integer view), so
    // the word side accepts either and the bit side accepts only `Bit`.
    let mut io_kinds: HashMap<&str, IoKind> = HashMap::with_capacity(project.io.len());
    for point in &project.io {
        io_kinds.insert(point.name.as_str(), point.kind);
    }
    // Every POU, once: a function block names the type of its instances, so the
    // declarations are read with the blocks in hand.
    let pous = project.all_pous();
    // The declared types, by lowercased name: an operand's type decides which side
    // of the program it may be used on, and a structured declaration has no storage.
    let mut var_types: HashMap<String, VarType> = HashMap::with_capacity(project.vars.len());
    for (index, var) in project.vars.iter().enumerate() {
        var_types.insert(var.name.to_ascii_lowercase(), var.ty.clone());
        // A derived type that names one of the project's function blocks is an
        // *instance*: it has data, one set per instance. Any other derived type is a
        // structure this build cannot lay out.
        let is_instance = match &var.ty {
            VarType::Derived(name) => pous.iter().any(|pou| {
                pou.kind == crate::model::PouKind::FunctionBlock
                    && pou.name.eq_ignore_ascii_case(name)
            }),
            _ => false,
        };
        if var.ty.is_structured() && !is_instance {
            diagnostics.push(Diagnostic::error(
                "var_type",
                format!(
                    "variable '{}' has type {}; a structured type has no storage in this build",
                    var.name,
                    var.ty.label()
                ),
                format!("vars[{index}].ty"),
            ));
        }
    }
    // The expression resolver reads the same name -> slot map and the same bank
    // geometry the compiler binds, so a name one accepts is a name the other can
    // resolve, and the two cannot disagree about a slot.
    let bank = crate::profile::layout::word_points(project);
    let layout = bank.layout;
    let mut word_slots: HashMap<String, u16> = bank
        .io
        .iter()
        .map(|(name, slot, _, _)| (name.to_ascii_lowercase(), *slot))
        .collect();
    // A typed declaration addresses its first slot by name, exactly as the compiler
    // binds it, so an operand resolves to the slot the scan reads.
    for var in &bank.vars {
        word_slots.insert(var.name.to_ascii_lowercase(), var.slot);
    }
    let max_words = crate::profile::profile().max_words();
    if layout.total() > max_words {
        diagnostics.push(Diagnostic::error(
            "too_large",
            format!(
                "the word bank needs {} slots ({} marker words plus the word points, float points and typed declarations); the maximum is {max_words}",
                layout.total(),
                layout.mw
            ),
            "io",
        ));
    }

    let pou_names: HashSet<String> = pous
        .iter()
        .map(|pou| pou.name.to_ascii_uppercase())
        .collect();
    let known: HashSet<String> = project
        .io
        .iter()
        .map(|point| point.name.to_ascii_lowercase())
        .chain(project.vars.iter().map(|var| var.name.to_ascii_lowercase()))
        .collect();
    // A function block is a POU with its own variables, and a project variable whose
    // type names one is an *instance* of it: its fields are operands in the programs
    // that declare it.
    let fb_vars: HashMap<String, &[VarDecl]> = pous
        .iter()
        .filter(|pou| pou.kind == crate::model::PouKind::FunctionBlock)
        .map(|pou| (pou.name.to_ascii_uppercase(), pou.vars.as_slice()))
        .collect();
    let instances: HashMap<String, String> = project
        .vars
        .iter()
        .filter_map(|var| match &var.ty {
            VarType::Derived(name) if fb_vars.contains_key(&name.to_ascii_uppercase()) => {
                Some((var.name.to_ascii_lowercase(), name.to_ascii_uppercase()))
            }
            _ => None,
        })
        .collect();

    for (index, pou) in pous.iter().enumerate() {
        match pou.language {
            PouLanguage::Ld => {}
            PouLanguage::Il => {
                // Type flow: a word operation on a bit accumulator computes
                // garbage, so it is reported rather than silently run.
                let mut accumulator: Option<bool> = None;
                // Jumps may point forward, so every label is known before the
                // instructions are checked.
                let mut labels: HashSet<String> = HashSet::new();
                for (line_index, line) in pou.il.iter().enumerate() {
                    if let IlLine::Label(name) = parse_line(line) {
                        if !labels.insert(name.to_ascii_uppercase()) {
                            diagnostics.push(Diagnostic::error(
                                "il_label",
                                format!("label '{name}' is declared twice in this POU"),
                                format!("pous[{index}].il[{line_index}]"),
                            ));
                        }
                    }
                }
                let facts = Facts {
                    profile,
                    known: &known,
                    word_slots: &word_slots,
                    io_kinds: &io_kinds,
                    var_types: &var_types,
                    word_vars: &bank.vars,
                    instances: &instances,
                    fb_vars: &fb_vars,
                    pou_names: &pou_names,
                    section_rungs: &[],
                    labels: &labels,
                    grid: None,
                };
                for (line_index, line) in pou.il.iter().enumerate() {
                    let path = format!("pous[{index}].il[{line_index}]");
                    match parse_line(line) {
                        IlLine::Instruction { mnemonic, operand } => {
                            // The rules are the profile's; the flow they carry
                            // (a word operation leaves a word) is the only state
                            // the validator has to keep.
                            let verdict = crate::profile::instr::check_il_line(
                                &mnemonic,
                                &operand,
                                accumulator,
                                &facts.ctx(&path),
                            );
                            diagnostics.extend(verdict.diagnostics);
                            if let Some(next) = verdict.accumulator {
                                accumulator = Some(next);
                            }
                        }
                        IlLine::Label(_) => {}
                        IlLine::Bad(message) => {
                            diagnostics.push(Diagnostic::error("il", message, path))
                        }
                        IlLine::Blank | IlLine::Comment => {}
                    }
                }
            }
            PouLanguage::Fbd => {
                let facts = Facts {
                    profile,
                    known: &known,
                    word_slots: &word_slots,
                    io_kinds: &io_kinds,
                    var_types: &var_types,
                    word_vars: &bank.vars,
                    instances: &instances,
                    fb_vars: &fb_vars,
                    pou_names: &pou_names,
                    section_rungs: &[],
                    labels: &HashSet::new(),
                    grid: None,
                };
                if let Some(grid) = pou.fbd_grid.as_ref().filter(|grid| !grid.blocks.is_empty()) {
                    let facts = Facts {
                        grid: Some(grid),
                        ..facts
                    };
                    for (block_index, block) in grid.blocks.iter().enumerate() {
                        let path = format!("pous[{index}].fbd_grid.blocks[{block_index}]");
                        diagnostics.extend(crate::profile::instr::check_instruction(
                            &Instr::Fbd { block },
                            &facts.ctx(&path),
                        ));
                    }
                    if pou.name.trim().is_empty() {
                        diagnostics.push(Diagnostic::error(
                            "pou_name",
                            "POU name must not be empty",
                            format!("pous[{index}].name"),
                        ));
                    }
                    continue;
                }
                for (line_index, line) in pou.fbd.iter().enumerate() {
                    let path = format!("pous[{index}].fbd[{line_index}]");
                    diagnostics.extend(crate::profile::instr::check_instruction(
                        &Instr::FbdLine {
                            line,
                            index: line_index,
                        },
                        &facts.ctx(&path),
                    ));
                }
            }
        }
        if pou.name.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "pou_name",
                "POU name must not be empty",
                format!("pous[{index}].name"),
            ));
        }
        // An instance's variables are bits today: a word pin would need a slot per
        // instance in the bank, which is a later step.
        if pou.kind == crate::model::PouKind::FunctionBlock {
            for (var_index, var) in pou.vars.iter().enumerate() {
                if !var.ty.is_bool() {
                    diagnostics.push(Diagnostic::error(
                        "fb_var_type",
                        format!(
                            "function block '{}' declares '{}' as {}; a block's variable is a BOOL for now",
                            pou.name,
                            var.name,
                            var.ty.label()
                        ),
                        format!("pous[{index}].vars[{var_index}].ty"),
                    ));
                }
            }
        }
    }
    // A call cycle is an error: inlining would stop at the depth guard and the
    // program would silently do half of what it says.
    let calls: HashMap<String, Vec<String>> = pous
        .iter()
        .map(|pou| {
            let mut targets = pou
                .il
                .iter()
                .filter_map(|line| match parse_line(line) {
                    IlLine::Instruction { mnemonic, operand }
                        if crate::profile::instr::is_call_mnemonic(&mnemonic) =>
                    {
                        let target = operand.trim();
                        // `CAL TON0(IN:=..)` is a block, not a POU.
                        (!target.contains('(')).then(|| target.to_ascii_uppercase())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            // A call coil is a call edge in the graph too, so a ladder-only
            // recursion is caught by the same walk.
            targets.extend(
                pou.sections
                    .iter()
                    .flat_map(|section| section.rungs.iter())
                    .filter(|rung| rung.coil().kind == crate::model::CoilKind::Call)
                    .map(|rung| rung.coil().var.trim().to_ascii_uppercase())
                    .filter(|name| !name.is_empty()),
            );
            (pou.name.to_ascii_uppercase(), targets)
        })
        .collect();
    for pou in &pous {
        let start = pou.name.to_ascii_uppercase();
        let mut stack = vec![start.clone()];
        if let Some(cycle) = find_cycle(&start, &calls, &mut stack) {
            diagnostics.push(Diagnostic::error(
                "cal_cycle",
                format!("'{}' calls itself through {}", pou.name, cycle.join(" -> ")),
                "pous",
            ));
        }
    }

    // With MAIN as the entry, a POU that nothing calls never runs.
    if let Some(main) = pous
        .iter()
        .find(|pou| pou.name.eq_ignore_ascii_case("MAIN"))
    {
        for pou in &pous {
            let name = pou.name.to_ascii_uppercase();
            if name == main.name.to_ascii_uppercase() {
                continue;
            }
            if pou.kind == crate::model::PouKind::FunctionBlock {
                // A function block runs where it is instantiated, not on its own.
                continue;
            }
            let reached = calls
                .get(&main.name.to_ascii_uppercase())
                .map(|targets| targets.iter().any(|target| target == &name))
                .unwrap_or(false)
                || calls.values().flatten().any(|target| target == &name);
            if !reached {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code: "pou_unreachable".to_string(),
                    message: format!(
                        "'{}' is never called, so the scan does not run it; call it from MAIN",
                        pou.name
                    ),
                    path: format!(
                        "pous[{index}].name",
                        index = pous
                            .iter()
                            .position(|item| item.name == pou.name)
                            .unwrap_or(0)
                    ),
                });
            }
        }
    }

    // What a body may name: the project's symbols, its own variables when it is a
    // function block, and every instance's fields.
    let mut per_pou_known: Vec<HashSet<String>> = Vec::with_capacity(pous.len());
    for pou in &pous {
        let mut known_here = known.clone();
        for var in &pou.vars {
            known_here.insert(var.name.to_ascii_lowercase());
        }
        for (instance, fb) in &instances {
            if let Some(fields) = fb_vars.get(fb) {
                for field in *fields {
                    known_here.insert(format!("{instance}.{}", field.name.to_ascii_lowercase()));
                }
            }
        }
        per_pou_known.push(known_here);
    }

    let mut sections: Vec<(&Section, &HashSet<String>)> = Vec::new();
    for (index, pou) in pous.iter().enumerate() {
        let known_here = per_pou_known.get(index).unwrap_or(&known);
        for section in &pou.sections {
            sections.push((section, known_here));
        }
    }

    let edge_contacts = pous
        .iter()
        .flat_map(|pou| pou.sections.iter())
        .flat_map(|section| section.rungs.iter())
        .flat_map(|rung| rung.cells.iter())
        .filter(|cell| matches!(cell, crate::grid::Cell::Edge { .. }))
        .count();
    let max_edges = crate::profile::profile().max_edge_contacts();
    if edge_contacts > usize::from(max_edges) {
        diagnostics.push(Diagnostic::error(
            "edge_limit",
            format!("{edge_contacts} edge contacts; at most {max_edges} are supported"),
            "sections",
        ));
    }

    // An IL-only project has no rungs; that is a program, not an empty one.
    let program_only = pous.iter().any(|pou| match pou.language {
        PouLanguage::Il => !pou.il.is_empty(),
        PouLanguage::Fbd => {
            !pou.fbd.is_empty()
                || pou
                    .fbd_grid
                    .as_ref()
                    .map(|grid| !grid.blocks.is_empty())
                    .unwrap_or(false)
        }
        PouLanguage::Ld => false,
    });
    if sections.is_empty() && !program_only {
        diagnostics.push(Diagnostic::error(
            "no_sections",
            "project has no sections",
            "sections",
        ));
    }

    let rung_count: usize = sections
        .iter()
        .map(|(section, _)| section.rungs.len())
        .sum();
    if rung_count > MAX_RUNGS {
        diagnostics.push(Diagnostic::error(
            "too_large",
            format!("project declares {rung_count} rungs (maximum {MAX_RUNGS})"),
            "project",
        ));
    }

    let mut section_names: HashSet<&str> = HashSet::with_capacity(sections.len());
    // A function block owns its variables, so its body's operands resolve against
    // its own interface and the instance fields of the project.
    for (section_index, (section, known)) in sections.iter().enumerate() {
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

        // A label is local to its section and names a jump target, so it must be
        // unique here. Two rungs with one name would leave the target ambiguous.
        let mut section_labels: HashSet<String> = HashSet::new();
        for (rung_index, rung) in section.rungs.iter().enumerate() {
            let label = rung.label.trim();
            if label.is_empty() {
                continue;
            }
            if !section_labels.insert(label.to_ascii_uppercase()) {
                diagnostics.push(Diagnostic::error(
                    "rung_label_duplicate",
                    format!("label '{label}' is declared twice in this section"),
                    format!("{section_path}.rungs[{rung_index}].label"),
                ));
            }
        }

        // Per target: which coil kinds wrote it. Write conflicts with anything,
        // Set and Reset together are a latch, and set-set or reset-reset are
        // ambiguous.
        let mut coils: HashMap<String, (bool, bool, bool)> = HashMap::new();
        // Ladder instructions read their own section and no labels: a jump coil
        // names a rung label, which is checked against these rungs.
        let no_labels: HashSet<String> = HashSet::new();
        let facts = Facts {
            profile,
            known,
            word_slots: &word_slots,
            io_kinds: &io_kinds,
            var_types: &var_types,
            word_vars: &bank.vars,
            instances: &instances,
            fb_vars: &fb_vars,
            pou_names: &pou_names,
            section_rungs: &section.rungs,
            labels: &no_labels,
            grid: None,
        };
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
                if let crate::grid::Cell::Block { .. } = cell {
                    // A block occupies two positions: its own cell and the cell to
                    // its right, where its pins' values are drawn outside the body.
                    // A block with nothing to its right would have its values
                    // clipped; a block against a symbol would draw that symbol under
                    // its values. So the neighbour must be a wire, or the row must
                    // end there.
                    let last = cell_index + 1 == crate::grid::POSITIONS;
                    let neighbour = rung.cells.get(cell_index + 1);
                    if last
                        || matches!(neighbour, Some(neighbour) if !matches!(neighbour, crate::grid::Cell::Wire))
                    {
                        diagnostics.push(Diagnostic::error(
                            "block_span",
                            "a block occupies two positions: its own cell and the wire to its right",
                            format!("{rung_path}.cells[{cell_index}]"),
                        ));
                    }
                }
                if matches!(cell, crate::grid::Cell::Link) && rung_index + 1 == section.rungs.len()
                {
                    diagnostics.push(Diagnostic::error(
                        "link_without_branch",
                        "a link has no row below to join",
                        format!("{rung_path}.cells[{cell_index}]"),
                    ));
                }
                let cell_path = format!("{rung_path}.cells[{cell_index}]");
                diagnostics.extend(crate::profile::instr::check_instruction(
                    &Instr::Cell(cell),
                    &facts.ctx(&cell_path),
                ));
            }
            if logic_depth(&rung.logic) > MAX_DEPTH {
                diagnostics.push(Diagnostic::error(
                    "too_deep",
                    format!("logic nesting is deeper than {MAX_DEPTH} levels"),
                    &logic_path,
                ));
            } else {
                check_logic(&rung.logic, &logic_path, &facts, &mut diagnostics);
            }

            if rung.is_comment_row() {
                continue;
            }
            // A rung with no coil is a block whose result nothing stores: the
            // cells are the whole rung, and there is no target to check.
            if !rung.has_coil() {
                continue;
            }
            let coil_path = format!("{rung_path}.coil.var");
            let ctx = facts.ctx(&rung_path);
            // A compute cell ends the rung before the coil, so an instruction
            // after it would never run or would test a bit nobody pushed.
            let compute_cell = rung
                .cells
                .iter()
                .any(|cell| matches!(cell, crate::grid::Cell::Compute { .. }));
            // `expr` is a compute coil's operand. On any other kind it would be
            // silently ignored, so it is refused instead.
            if rung.coil().kind != crate::model::CoilKind::Compute {
                if let Some(expr) = rung.coil().expr.as_deref() {
                    diagnostics.extend(crate::profile::instr::check_instruction(
                        &Instr::CoilExpr {
                            kind: rung.coil().kind,
                            var: &rung.coil().var,
                            expr,
                        },
                        &ctx,
                    ));
                }
            }
            // A compute coil writes through its expression: its slot is unused,
            // and its text is the whole operand.
            if rung.coil().kind == crate::model::CoilKind::Compute {
                diagnostics.extend(crate::profile::instr::check_instruction(
                    &Instr::ComputeCoil {
                        expr: rung.coil().expr.as_deref(),
                        compute_cell,
                    },
                    &ctx,
                ));
                continue;
            }
            // A jump or call coil's operand is a label or a POU name, not a
            // writable bit, so it leaves the bit-coil rules alone.
            if matches!(
                rung.coil().kind,
                crate::model::CoilKind::Jump | crate::model::CoilKind::Call
            ) {
                diagnostics.extend(crate::profile::instr::check_instruction(
                    &Instr::LabelCoil {
                        kind: rung.coil().kind,
                        var: &rung.coil().var,
                        compute_cell,
                        args: &rung.coil().args,
                    },
                    &ctx,
                ));
                continue;
            }
            let verdict = crate::profile::instr::coil(
                &Instr::Coil {
                    var: &rung.coil().var,
                },
                &ctx,
            );
            diagnostics.extend(verdict.diagnostics);
            // A counter's reset/load is a coil on the instance, not on a bit.
            if verdict.target_judged {
                continue;
            }
            // A compute rung writes through its `Compute` cell, so its coil slot is
            // unused: neither an empty target nor a duplicate there means anything.
            if compute_cell {
                continue;
            }
            // A blank coil is the linked continuation row: the row holds the
            // branch under the `Link` above it and the link's row carries the
            // coil. A blank coil anywhere else is still an error.
            let linked_continuation = rung.coil().var.trim().is_empty()
                && rung_index > 0
                && section.rungs[rung_index - 1]
                    .cells
                    .iter()
                    .any(|cell| matches!(cell, crate::grid::Cell::Link));
            if linked_continuation {
                continue;
            }
            // A coil on a bit of a declared word writes that bit: it is a declared
            // target, checked against the word's declaration by the profile's rule
            // (which also reports an index past the bound).
            let class = symbols
                .get(rung.coil().var.as_str())
                .copied()
                // A function block's body writes its own variables.
                .or_else(|| {
                    known
                        .contains(&rung.coil().var.to_ascii_lowercase())
                        .then_some(SymbolClass::Var)
                })
                .or_else(|| {
                    crate::profile::layout::word_bit(rung.coil().var.as_str(), &bank.vars)
                        .ok()
                        .map(|_| SymbolClass::Var)
                });
            match class {
                None => diagnostics.push(Diagnostic::error(
                    "unknown_symbol",
                    format!("coil target `{}` is not declared", rung.coil().var),
                    &coil_path,
                )),
                Some(SymbolClass::Input) => diagnostics.push(Diagnostic::error(
                    "coil_target",
                    format!(
                        "`{}` is an input and cannot be a coil target",
                        rung.coil().var
                    ),
                    &coil_path,
                )),
                Some(_) => {
                    // A Set and a Reset on one target are a latch, not a
                    // conflict: only two writes of the same kind are ambiguous.
                    let class = match rung.coil().kind {
                        crate::model::CoilKind::Set => "set",
                        crate::model::CoilKind::Reset => "reset",
                        _ => "write",
                    };
                    let seen = coils
                        .entry(rung.coil().var.clone())
                        .or_insert((false, false, false));
                    let conflict = match class {
                        "set" => seen.1 || seen.0,
                        "reset" => seen.2 || seen.0,
                        _ => seen.0 || seen.1 || seen.2,
                    };
                    if conflict {
                        diagnostics.push(Diagnostic::error(
                            "duplicate_coil",
                            format!(
                                "`{}` has more than one {class} coil in this section",
                                rung.coil().var
                            ),
                            &coil_path,
                        ));
                    }
                    match class {
                        "set" => seen.1 = true,
                        "reset" => seen.2 = true,
                        _ => seen.0 = true,
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

fn find_cycle(
    name: &str,
    calls: &HashMap<String, Vec<String>>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    for target in calls.get(name).map(Vec::as_slice).unwrap_or(&[]) {
        if let Some(position) = stack.iter().position(|entry| entry == target) {
            let mut cycle = stack[position..].to_vec();
            cycle.push(target.clone());
            return Some(cycle);
        }
        stack.push(target.clone());
        if let Some(cycle) = find_cycle(target, calls, stack) {
            return Some(cycle);
        }
        stack.pop();
    }
    None
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

fn check_logic(logic: &Logic, path: &str, facts: &Facts, diagnostics: &mut Vec<Diagnostic>) {
    // The node's rule is the profile's; the walk and the emptiness warnings are
    // the tree's shape, which is this module's job.
    diagnostics.extend(crate::profile::instr::check_instruction(
        &Instr::Logic(logic),
        &facts.ctx(path),
    ));
    match logic {
        Logic::Series { items } => check_items(items, path, true, facts, diagnostics),
        Logic::Parallel { items } => check_items(items, path, false, facts, diagnostics),
        _ => {}
    }
}

fn check_items(
    items: &[Logic],
    path: &str,
    series: bool,
    facts: &Facts,
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
        check_logic(item, &format!("{path}.items[{index}]"), facts, diagnostics);
    }
}
