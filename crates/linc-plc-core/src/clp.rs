//! ClassicLadder `.clp` project importer.
//!
//! A `.clp` is a plain-text concatenation of the project's parameter files, and
//! an unpacked project directory is the same set of files. Both shapes are
//! accepted here, as ClassicLadder's `LoadProjectFiles` accepts them.
//!
//! The import is all-or-nothing like `plcopen::import`: any construct that has no
//! faithful LiNC equivalent returns a diagnostic and no project. The element
//! model is ClassicLadder's: a rung is a 10-wide, 6-tall grid of elements, big
//! elements occupy a footprint whose "alive" cell is the top-right corner, and a
//! vertical `ConnectedWithTop` bit joins a cell to the row above.
//!
//! Read Appendix A of `docs/PLC-PORT-HANDOFF.md` (the refusal codes, as shipped)
//! and the `.clp import` section of `docs/plc.md` (the operator-facing
//! description) before changing the mapping. The four facts that are easy to
//! get wrong:
//!
//! - rung order is the `NextRung` chain, never the file numbering;
//! - a counter's `done` is equality while LiNC's `CTU.Q` is `>=`, so `done` is
//!   synthesised as an equality compare and `Q` is never used;
//! - `arithmetic_expressions.csv` stores operands as `@type/number@`, not as the
//!   `%C0.V` text the editor shows;
//! - a jump targets a rung *number* and a call targets a *subroutine number*.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::grid::Cell;
use crate::model::{
    Coil, CoilKind, IoDirection, IoKind, IoPoint, Pou, PouLanguage, Project, Rung, Section, VarDecl,
};
use crate::plcopen::operand_name;
use crate::profile::resolve::{block_instance, block_member, Member};
use crate::profile::spec::FbName;
use crate::validate::{validate, Diagnostic, Severity};

/// Rung grid width, from `classicladder.h` (`RUNG_WIDTH`).
const RUNG_WIDTH: usize = 10;
/// Rung grid height (`RUNG_HEIGHT`).
const RUNG_HEIGHT: usize = 6;
/// Element positions before the coil, from `grid::POSITIONS`.
const POSITIONS: usize = crate::grid::POSITIONS;
/// Instances per block kind, from `compile::FB_PER_TYPE`.
const FB_PER_TYPE: u16 = crate::compile::FB_PER_TYPE;
/// ClassicLadder's counter wrap: `CV` stays inside 0..=9999.
const COUNTER_WRAP: i32 = 9999;

// Element types (`ELE_*`, classicladder.h).
const ELE_FREE: i32 = 0;
const ELE_INPUT: i32 = 1;
const ELE_INPUT_NOT: i32 = 2;
const ELE_RISING_INPUT: i32 = 3;
const ELE_FALLING_INPUT: i32 = 4;
const ELE_CONNECTION: i32 = 9;
const ELE_TIMER: i32 = 10;
const ELE_MONOSTABLE: i32 = 11;
const ELE_COUNTER: i32 = 12;
const ELE_TIMER_IEC: i32 = 13;
const ELE_COMPAR: i32 = 20;
const ELE_OUTPUT: i32 = 50;
const ELE_OUTPUT_NOT: i32 = 51;
const ELE_OUTPUT_SET: i32 = 52;
const ELE_OUTPUT_RESET: i32 = 53;
const ELE_OUTPUT_JUMP: i32 = 54;
const ELE_OUTPUT_CALL: i32 = 55;
const ELE_OUTPUT_OPERATE: i32 = 60;
const ELE_UNUSABLE: i32 = 99;

// Variable types (`VAR_*`, classicladder.h). The comment is the spelling the
// editor shows; the number is what a file stores.
const VAR_MEM_BIT: i32 = 0;
const VAR_TIMER_DONE: i32 = 10;
const VAR_TIMER_IEC_DONE: i32 = 15;
const VAR_MONOSTABLE_RUNNING: i32 = 20;
const VAR_COUNTER_DONE: i32 = 25;
const VAR_COUNTER_EMPTY: i32 = 26;
const VAR_COUNTER_FULL: i32 = 27;
const VAR_PHYS_INPUT: i32 = 50;
const VAR_PHYS_OUTPUT: i32 = 60;
const VAR_MEM_WORD: i32 = 200;
const VAR_TIMER_PRESET: i32 = 230;
const VAR_TIMER_VALUE: i32 = 231;
const VAR_MONOSTABLE_PRESET: i32 = 240;
const VAR_MONOSTABLE_VALUE: i32 = 241;
const VAR_COUNTER_PRESET: i32 = 250;
const VAR_COUNTER_VALUE: i32 = 251;
const VAR_TIMER_IEC_PRESET: i32 = 260;
const VAR_TIMER_IEC_VALUE: i32 = 261;
const VAR_PHYS_WORD_INPUT: i32 = 270;
const VAR_PHYS_WORD_OUTPUT: i32 = 280;
const VAR_PHYS_FLOAT_INPUT: i32 = 300;
const VAR_PHYS_FLOAT_OUTPUT: i32 = 310;

/// Timer modes (`TIMER_IEC_MODE_*`).
const TIMER_IEC_MODE_ON: i32 = 0;
const TIMER_IEC_MODE_OFF: i32 = 1;
const TIMER_IEC_MODE_PULSE: i32 = 2;

/// Import a `.clp` container.
pub fn import(text: &str) -> Result<Project, Vec<Diagnostic>> {
    let members = split_container(text)?;
    import_members(&members)
}

/// Import an unpacked project directory: the same members, as files.
pub fn import_dir(directory: &Path) -> Result<Project, Vec<Diagnostic>> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        refuse(
            "clp_container",
            format!("cannot read {}: {error}", directory.display()),
        )
    })?;
    let mut members = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).map_err(|error| {
            refuse(
                "clp_container",
                format!("cannot read {}: {error}", path.display()),
            )
        })?;
        members.insert(name.to_string(), text);
    }
    import_members(&members)
}

/// Import a path: a directory as `LoadProjectFiles` reads it, or a `.clp` file.
pub fn import_path(path: &Path) -> Result<Project, Vec<Diagnostic>> {
    if path.is_dir() {
        return import_dir(path);
    }
    let text = std::fs::read_to_string(path).map_err(|error| {
        refuse(
            "clp_container",
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    import(&text)
}

fn refuse(code: &str, message: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic::error(code, message, "clp")]
}

// --- container --------------------------------------------------------------

type Members = BTreeMap<String, String>;

/// Split a `.clp` container into its members.
///
/// `SplitFiles` writes `_FILE-<name>` before a member and `_/FILE-<name>` after
/// it, and skips blank lines between members. CRLF is accepted, and a member may
/// contain blank lines because its own grammar decides what they mean.
fn split_container(text: &str) -> Result<Members, Vec<Diagnostic>> {
    let mut lines = text.lines().map(|line| line.trim_end_matches('\r'));
    let first = lines.next().unwrap_or("").trim_start_matches('\u{feff}');
    if !first.starts_with("_FILES_CLASSICLADDER") {
        return Err(refuse(
            "clp_container",
            "the file does not start with `_FILES_CLASSICLADDER`",
        ));
    }
    let mut members = Members::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in lines {
        if let Some(rest) = line.strip_prefix("_FILE-") {
            if current.is_some() {
                return Err(refuse(
                    "clp_container",
                    "a member starts before the previous one ends",
                ));
            }
            let name = rest.trim().to_string();
            if name.is_empty() {
                return Err(refuse("clp_container", "a member has an empty name"));
            }
            current = Some((name, Vec::new()));
            continue;
        }
        if line.starts_with("_/FILE-") {
            let Some((name, body)) = current.take() else {
                return Err(refuse(
                    "clp_container",
                    "a member ends without having started",
                ));
            };
            if members.insert(name.clone(), body.join("\n")).is_some() {
                return Err(refuse(
                    "clp_container",
                    format!("member `{name}` appears twice"),
                ));
            }
            continue;
        }
        if line.starts_with("_/FILES_CLASSICLADDER") {
            if current.is_some() {
                return Err(refuse(
                    "clp_container",
                    "the container ends inside a member",
                ));
            }
            return Ok(members);
        }
        match current.as_mut() {
            Some((_, body)) => body.push(line.to_string()),
            None if line.trim().is_empty() => {}
            None => {
                return Err(refuse(
                    "clp_container",
                    format!("`{}` appears outside a member", line.trim()),
                ))
            }
        }
    }
    match current {
        Some((name, _)) => Err(refuse(
            "clp_container",
            format!("member `{name}` is never closed"),
        )),
        None => Ok(members),
    }
}

// --- small parsers ----------------------------------------------------------

/// One cell of a rung grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Element {
    ele: i32,
    /// `ConnectedWithTop`: 1 when a vertical link joins this cell to the row
    /// above at the same column.
    top: bool,
    var_type: i32,
    var_num: i32,
    /// `Some` when `IndexedVarType != -1`: indirect addressing.
    indexed: Option<(i32, i32)>,
}

impl Default for Element {
    fn default() -> Self {
        Self {
            ele: ELE_FREE,
            top: false,
            var_type: 0,
            var_num: 0,
            indexed: None,
        }
    }
}

impl Element {
    fn is_free(&self) -> bool {
        matches!(self.ele, ELE_FREE | ELE_CONNECTION)
    }
}

#[derive(Debug, Clone)]
struct RawRung {
    index: usize,
    label: String,
    comment: String,
    prev: i32,
    next: i32,
    grid: [[Element; RUNG_WIDTH]; RUNG_HEIGHT],
}

impl RawRung {
    fn at(&self, x: usize, y: usize) -> Element {
        self.grid[y][x]
    }
}

#[derive(Debug, Clone)]
struct RawSection {
    name: String,
    language: i32,
    subroutine: i32,
    first: i32,
    last: i32,
    sequential: i32,
}

fn members_required(members: &Members, name: &str) -> Result<String, Vec<Diagnostic>> {
    members
        .get(name)
        .cloned()
        .ok_or_else(|| refuse("clp_member", format!("the project has no `{name}` member")))
}

/// Is a member present and carrying anything beyond comments, headers and blank
/// lines? `sequential.csv` and `modbusioconf.csv` are refused when it is.
fn member_has_rows(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with(';') && !line.starts_with('#')
    })
}

/// Split `a,b,c` into trimmed fields, keeping empty ones.
fn fields(line: &str) -> Vec<&str> {
    line.split(',').map(str::trim).collect()
}

fn parse_int(text: &str) -> Option<i32> {
    text.trim().parse::<i32>().ok()
}

/// Parse one rung file. `path` is used for diagnostics.
fn parse_rung(name: &str, text: &str, index: usize) -> Result<RawRung, Vec<Diagnostic>> {
    let mut rung = RawRung {
        index,
        label: String::new(),
        comment: String::new(),
        prev: -1,
        next: -1,
        grid: [[Element::default(); RUNG_WIDTH]; RUNG_HEIGHT],
    };
    let mut row = 0usize;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            if let Some(value) = rest.strip_prefix("VER=") {
                // `LoadRung` refuses only a major version above 2.
                let major = value.split('.').next().and_then(parse_int).ok_or_else(|| {
                    refuse(
                        "clp_version",
                        format!("{name}: `#VER={value}` is not a version"),
                    )
                })?;
                if major > 2 {
                    return Err(refuse(
                        "clp_version",
                        format!(
                            "{name}: rung version {value} is not supported (2.x is the newest)"
                        ),
                    ));
                }
            } else if let Some(value) = rest.strip_prefix("LABEL=") {
                rung.label = value.trim().to_string();
            } else if let Some(value) = rest.strip_prefix("COMMENT=") {
                rung.comment = value.trim().to_string();
            } else if let Some(value) = rest.strip_prefix("PREVRUNG=") {
                rung.prev = parse_int(value).unwrap_or(-1);
            } else if let Some(value) = rest.strip_prefix("NEXTRUNG=") {
                rung.next = parse_int(value).unwrap_or(-1);
            } else {
                return Err(refuse(
                    "clp_rung_syntax",
                    format!("{name}: unknown header `{trimmed}`"),
                ));
            }
            continue;
        }
        if row >= RUNG_HEIGHT {
            return Err(refuse(
                "clp_rung_syntax",
                format!("{name}: more than {RUNG_HEIGHT} rows"),
            ));
        }
        let cells = fields(line);
        if cells.len() > RUNG_WIDTH {
            return Err(refuse(
                "clp_rung_syntax",
                format!(
                    "{name}: row {row} has {} cells, the grid is {RUNG_WIDTH}",
                    cells.len()
                ),
            ));
        }
        for (x, cell) in cells.iter().enumerate() {
            rung.grid[row][x] = parse_element(name, cell, row, x)?;
        }
        row += 1;
    }
    if row == 0 {
        return Err(refuse(
            "clp_rung_syntax",
            format!("{name}: the rung has no cell rows"),
        ));
    }
    Ok(rung)
}

/// `Type-ConnectedWithTop-VarType/VarNum[IndexedType/IndexedNum]`.
fn parse_element(
    name: &str,
    text: &str,
    row: usize,
    column: usize,
) -> Result<Element, Vec<Diagnostic>> {
    let text = text.trim();
    let syntax = |detail: &str| {
        refuse(
            "clp_rung_syntax",
            format!("{name}: row {row} column {column}: `{text}` {detail}"),
        )
    };
    let (base, indexed) = match text.split_once('[') {
        Some((base, rest)) => {
            let inner = rest
                .strip_suffix(']')
                .ok_or_else(|| syntax("has an unclosed `[`"))?;
            let (kind, number) = inner
                .split_once('/')
                .ok_or_else(|| syntax("has a malformed indirect operand"))?;
            let kind = parse_int(kind).ok_or_else(|| syntax("has a malformed index type"))?;
            let number = parse_int(number).ok_or_else(|| syntax("has a malformed index number"))?;
            (base, Some((kind, number)))
        }
        None => (text, None),
    };
    let mut parts = base.splitn(3, '-');
    let ele = parts.next().and_then(parse_int);
    let top = parts.next().and_then(parse_int);
    let reference = parts.next();
    let (Some(ele), Some(top), Some(reference)) = (ele, top, reference) else {
        return Err(syntax("is not `Type-Top-VarType/VarNum`"));
    };
    let (var_type, var_num) = reference
        .split_once('/')
        .ok_or_else(|| syntax("is missing `VarType/VarNum`"))?;
    let var_type = parse_int(var_type).ok_or_else(|| syntax("has a malformed variable type"))?;
    let var_num = parse_int(var_num).ok_or_else(|| syntax("has a malformed variable number"))?;
    Ok(Element {
        ele,
        top: top != 0,
        var_type,
        var_num,
        indexed,
    })
}

fn parse_sections(text: &str) -> Result<Vec<RawSection>, Vec<Diagnostic>> {
    let mut names: HashMap<usize, String> = HashMap::new();
    let mut sections = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            if let Some(value) = rest.strip_prefix("VER=") {
                let major = value.split('.').next().and_then(parse_int).unwrap_or(0);
                if major > 1 {
                    return Err(refuse(
                        "clp_version",
                        format!(
                            "sections.csv: version {value} is not supported (1.x is the newest)"
                        ),
                    ));
                }
            } else if let Some(rest) = rest.strip_prefix("NAME") {
                let (number, name) = rest.split_once('=').ok_or_else(|| {
                    refuse(
                        "clp_member",
                        format!("sections.csv: `{trimmed}` names no section"),
                    )
                })?;
                let number = parse_int(number).ok_or_else(|| {
                    refuse(
                        "clp_member",
                        format!("sections.csv: `{trimmed}` has no number"),
                    )
                })? as usize;
                names.insert(number, name.trim().to_string());
            }
            continue;
        }
        let fields = fields(trimmed);
        if fields.len() != 6 {
            return Err(refuse(
                "clp_member",
                format!("sections.csv: `{trimmed}` is not `number,language,sr,first,last,page`"),
            ));
        }
        let numbers = fields
            .iter()
            .map(|field| parse_int(field))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                refuse(
                    "clp_member",
                    format!("sections.csv: `{trimmed}` has a non-number"),
                )
            })?;
        sections.push(RawSection {
            name: names
                .get(&(numbers[0] as usize))
                .cloned()
                .unwrap_or_default(),
            language: numbers[1],
            subroutine: numbers[2],
            first: numbers[3],
            last: numbers[4],
            sequential: numbers[5],
        });
    }
    if sections.is_empty() {
        return Err(refuse("clp_member", "sections.csv declares no section"));
    }
    Ok(sections)
}

/// `symbols.csv`: `VarName,Symbol,Comment`, where `VarName` is `%B7`.
fn parse_symbols(text: &str) -> Result<HashMap<String, String>, Vec<Diagnostic>> {
    let mut symbols = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r').trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let fields = fields(trimmed);
        let Some(var) = fields.first() else {
            continue;
        };
        if var.is_empty() {
            continue;
        }
        // A symbol may contain commas; only the first two are structural.
        let name = fields.get(1).copied().unwrap_or("");
        if !name.is_empty() {
            symbols.insert(var.to_ascii_uppercase(), name.to_string());
        }
    }
    Ok(symbols)
}

fn parse_counters(text: &str) -> Result<Vec<i32>, Vec<Diagnostic>> {
    let mut presets = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r').trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let preset = parse_int(trimmed).ok_or_else(|| {
            refuse(
                "clp_member",
                format!("counters.csv: `{trimmed}` is not a preset"),
            )
        })?;
        presets.push(preset);
    }
    Ok(presets)
}

/// `timers_iec.csv`: `base,preset,mode`. The preset is already milliseconds.
fn parse_iec_timers(text: &str) -> Result<Vec<(i32, i32)>, Vec<Diagnostic>> {
    let mut timers = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r').trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let fields = fields(trimmed);
        if fields.len() < 3 {
            return Err(refuse(
                "clp_member",
                format!("timers_iec.csv: `{trimmed}` is not `base,preset,mode`"),
            ));
        }
        let preset = parse_int(fields[1]).ok_or_else(|| {
            refuse(
                "clp_member",
                format!("timers_iec.csv: `{trimmed}` has a non-number preset"),
            )
        })?;
        let mode = parse_int(fields[2]).ok_or_else(|| {
            refuse(
                "clp_member",
                format!("timers_iec.csv: `{trimmed}` has a non-number mode"),
            )
        })?;
        if !(TIMER_IEC_MODE_ON..=TIMER_IEC_MODE_PULSE).contains(&mode) {
            return Err(refuse(
                "clp_member",
                format!("timers_iec.csv: timer {} has mode {mode}", timers.len()),
            ));
        }
        timers.push((preset, mode));
    }
    Ok(timers)
}

/// `arithmetic_expressions.csv`: `0000,<expression>` lines, indexed by number.
fn parse_expressions(text: &str) -> Result<HashMap<i32, String>, Vec<Diagnostic>> {
    let mut expressions = HashMap::new();
    let mut next = 0i32;
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r').trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let (number, expression) = match trimmed.split_once(',') {
            Some((number, rest))
                if number.chars().all(|c| c.is_ascii_digit()) && !number.is_empty() =>
            {
                let number = parse_int(number).ok_or_else(|| {
                    refuse(
                        "clp_member",
                        format!("arithmetic_expressions.csv: `{number}` is not a number"),
                    )
                })?;
                (number, rest.trim())
            }
            // Older files store one expression per line, in order.
            _ => (next, trimmed),
        };
        expressions.insert(number, expression.to_string());
        next = number + 1;
    }
    Ok(expressions)
}

// --- import ---------------------------------------------------------------

/// Import the project's members: sections, rungs, parameters and expressions.
fn import_members(members: &Members) -> Result<Project, Vec<Diagnostic>> {
    if members
        .get("sequential.csv")
        .is_some_and(|text| member_has_rows(text))
    {
        return Err(refuse(
            "clp_sfc",
            "sequential.csv carries SFC pages, which LiNC does not implement",
        ));
    }
    if members
        .get("modbusioconf.csv")
        .is_some_and(|text| member_has_rows(text))
    {
        return Err(refuse(
            "clp_modbus",
            "modbusioconf.csv configures Modbus I/O, which LiNC does not implement",
        ));
    }

    let sections_text = members_required(members, "sections.csv")?;
    let raw_sections = parse_sections(&sections_text)?;
    let symbols = match members.get("symbols.csv") {
        Some(text) => parse_symbols(text)?,
        None => HashMap::new(),
    };
    let counters = match members.get("counters.csv") {
        Some(text) => parse_counters(text)?,
        None => Vec::new(),
    };
    let iec_timers = match members.get("timers_iec.csv") {
        Some(text) => parse_iec_timers(text)?,
        None => Vec::new(),
    };
    let expressions = match members.get("arithmetic_expressions.csv") {
        Some(text) => parse_expressions(text)?,
        None => HashMap::new(),
    };

    let mut rungs: BTreeMap<usize, RawRung> = BTreeMap::new();
    for (name, text) in members {
        let Some(rest) = name.strip_prefix("rung_") else {
            continue;
        };
        // `LoadAllRungs` builds `rung_<n>.csv`; the suffixless spelling is
        // accepted too, because an unpacked project may carry either.
        let rest = rest.strip_suffix(".csv").unwrap_or(rest);
        let index = parse_int(rest).ok_or_else(|| {
            refuse(
                "clp_member",
                format!("`{name}` does not name a rung number"),
            )
        })?;
        if index < 0 {
            return Err(refuse(
                "clp_member",
                format!("`{name}` has a negative rung number"),
            ));
        }
        rungs.insert(index as usize, parse_rung(name, text, index as usize)?);
    }
    if rungs.is_empty() {
        return Err(refuse("clp_member", "the project has no `rung_<n>` member"));
    }

    // One POU per section. A subroutine section is named after its number, so a
    // call coil can reach it; main sections keep no POU called MAIN, because the
    // compiler would then treat the others as uncalled routines.
    let mut pou_names: HashMap<usize, String> = HashMap::new();
    let mut subroutine_pous: HashMap<i32, String> = HashMap::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for (position, section) in raw_sections.iter().enumerate() {
        // The language decides: a ladder section's SequentialPage is 0 in real
        // files, so only `SECTION_IN_SEQUENTIAL` marks an SFC section.
        if section.language == 1 {
            return Err(refuse(
                "clp_sfc",
                format!(
                    "section `{}` is a sequential section; LiNC has no SFC",
                    if section.name.is_empty() {
                        format!("{position}")
                    } else {
                        section.name.clone()
                    }
                ),
            ));
        }
        if section.language != 0 {
            return Err(refuse(
                "clp_member",
                format!(
                    "section language {} is not one LiNC knows",
                    section.language
                ),
            ));
        }
        let _ = section.sequential;
        let base = if section.name.trim().is_empty() {
            if section.subroutine >= 0 {
                format!("sr{}", section.subroutine)
            } else {
                format!("section{}", position + 1)
            }
        } else {
            operand_name(&section.name)
        };
        if base.eq_ignore_ascii_case("main") {
            return Err(refuse(
                "clp_member",
                format!(
                    "section `{}` is named MAIN; rename it in ClassicLadder, because LiNC would make every other POU an uncalled routine",
                    section.name
                ),
            ));
        }
        let mut name = base.clone();
        let mut suffix = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{base}-{suffix}");
            suffix += 1;
        }
        if section.subroutine >= 0 {
            subroutine_pous.insert(section.subroutine, name.clone());
        }
        pou_names.insert(position, name);
    }

    // A counter's direction comes from its wiring, and a done contact may sit in
    // a rung before the counter element, so the directions are known before any
    // rung is mapped. A counter rung also expands to several LiNC rungs, so a
    // jump may not target it.
    let mut counter_kind: HashMap<i32, FbName> = HashMap::new();
    let mut multi: HashSet<usize> = HashSet::new();
    for (index, rung) in &rungs {
        let Some((x, y)) = counter_position(rung) else {
            continue;
        };
        multi.insert(*index);
        let number = rung.at(x, y).var_num;
        let kind = counter_direction(rung, x, y)?;
        if let Some(existing) = counter_kind.insert(number, kind) {
            if existing != kind {
                return Err(refuse(
                    "clp_rung_syntax",
                    format!(
                        "counter {number} is drawn as both a {} and a {}",
                        existing.text(),
                        kind.text()
                    ),
                ));
            }
        }
    }

    let mut importer = Importer {
        symbols,
        counters,
        iec_timers,
        expressions,
        counter_kind,
        io: Vec::new(),
        vars: Vec::new(),
        names: HashMap::new(),
        diagnostics: Vec::new(),
    };

    let mut pous = Vec::new();
    for (position, section) in raw_sections.iter().enumerate() {
        let chain = walk_chain(section, &rungs)?;
        let labels = label_rungs(&chain, &rungs);
        let mut out = Vec::new();
        for index in &chain {
            let raw = &rungs[index];
            let mapped = importer.map_rung(raw, &labels, &subroutine_pous, &multi)?;
            out.extend(mapped);
        }
        pous.push(Pou {
            name: pou_names[&position].clone(),
            language: PouLanguage::Ld,
            comment: String::new(),
            sections: vec![Section {
                name: if section.name.trim().is_empty() {
                    format!("main{}", position + 1)
                } else {
                    operand_name(&section.name)
                },
                rungs: out,
            }],
            il: Vec::new(),
            fbd: Vec::new(),
            fbd_grid: None,
        });
    }

    let mut project = Project::new(if pous.len() == 1 {
        pous[0].name.clone()
    } else {
        "classicladder".to_string()
    });
    project.io = importer.io;
    project.vars = importer.vars;
    project.pous = pous;

    // Validation runs on the imported project, exactly as it does after an edit.
    let mut diagnostics = importer.diagnostics;
    let report = validate(&project);
    diagnostics.extend(report.iter().cloned());
    if diagnostics
        .iter()
        .any(|item| item.severity == Severity::Error)
    {
        return Err(diagnostics);
    }
    Ok(project)
}

/// Walk a section's rung chain from `FirstRung` along `NextRung`.
///
/// The file numbering is not the order: a real project can run `1 -> 0 -> 2`.
/// A chain that leaves the section, revisits a rung, or fails to reach every
/// rung between `FirstRung` and `LastRung` is refused rather than truncated.
fn walk_chain(
    section: &RawSection,
    rungs: &BTreeMap<usize, RawRung>,
) -> Result<Vec<usize>, Vec<Diagnostic>> {
    let label = if section.name.is_empty() {
        String::new()
    } else {
        format!(" `{}`", section.name)
    };
    if section.first < 0 {
        return Err(refuse(
            "clp_rung_chain",
            format!("section{label} has no first rung"),
        ));
    }
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    let mut current = section.first;
    loop {
        let index = current as usize;
        let Some(rung) = rungs.get(&index) else {
            return Err(refuse(
                "clp_rung_chain",
                format!("section{label} refers to rung {index}, which has no file"),
            ));
        };
        if !seen.insert(index) {
            return Err(refuse(
                "clp_rung_chain",
                format!("the chain of section{label} returns to rung {index}"),
            ));
        }
        order.push(index);
        if current == section.last {
            break;
        }
        if rung.next < 0 {
            return Err(refuse(
                "clp_rung_chain",
                format!(
                    "rung {index} ends the chain of section{label} before LastRung {}",
                    section.last
                ),
            ));
        }
        current = rung.next;
        if order.len() > rungs.len() {
            return Err(refuse(
                "clp_rung_chain",
                format!("the chain of section{label} never reaches LastRung"),
            ));
        }
    }
    if section.first <= section.last {
        for index in section.first as usize..=section.last as usize {
            if rungs.contains_key(&index) && !seen.contains(&index) {
                return Err(refuse(
                    "clp_rung_chain",
                    format!("rung {index} is inside section{label} but the chain never reaches it"),
                ));
            }
        }
    }
    Ok(order)
}

/// Each rung's LiNC label: its `#LABEL` when it has one, else `r<number>`.
///
/// ClassicLadder jumps target a rung *number*, so the label table is the bridge;
/// a duplicate `#LABEL` keeps the first and falls back to the number for the
/// rest, which keeps every jump resolvable.
fn label_rungs(chain: &[usize], rungs: &BTreeMap<usize, RawRung>) -> HashMap<usize, String> {
    let mut labels = HashMap::new();
    let mut used = HashSet::new();
    for index in chain {
        let declared = rungs[index].label.trim();
        let label = if declared.is_empty() || !used.insert(declared.to_string()) {
            let generated = format!("r{index}");
            used.insert(generated.clone());
            generated
        } else {
            declared.to_string()
        };
        labels.insert(*index, label);
    }
    labels
}

// --- import ---------------------------------------------------------------

struct Importer {
    symbols: HashMap<String, String>,
    counters: Vec<i32>,
    iec_timers: Vec<(i32, i32)>,
    expressions: HashMap<i32, String>,
    /// Counter number -> the direction its wiring selects (`CTU` / `CTD`).
    counter_kind: HashMap<i32, FbName>,
    io: Vec<IoPoint>,
    vars: Vec<VarDecl>,
    /// ClassicLadder name (`%B7`) -> LiNC name, so one variable is declared once
    /// and two variables never merge.
    names: HashMap<String, String>,
    diagnostics: Vec<Diagnostic>,
}

impl Importer {
    /// The LiNC name for a ClassicLadder variable, declaring it on first use.
    fn declare(
        &mut self,
        cl: &str,
        generated: &str,
        direction: Option<IoDirection>,
        kind: IoKind,
    ) -> Result<String, Vec<Diagnostic>> {
        if let Some(name) = self.names.get(cl) {
            return Ok(name.clone());
        }
        let name = self
            .symbols
            .get(&cl.to_ascii_uppercase())
            .map(|raw| operand_name(raw))
            .unwrap_or_else(|| operand_name(generated));
        if let Some(other) = self
            .names
            .iter()
            .find(|(_, existing)| **existing == name)
            .map(|(other, _)| other.clone())
        {
            return Err(refuse(
                "duplicate_symbol",
                format!("`{cl}` and `{other}` both import as `{name}`; symbols.csv must not merge two variables"),
            ));
        }
        self.names.insert(cl.to_string(), name.clone());
        match direction {
            Some(direction) => self.io.push(IoPoint {
                name: name.clone(),
                direction,
                kind,
                comment: String::new(),
            }),
            None => {
                let mut var = VarDecl::new(name.clone());
                var.comment = self
                    .symbols
                    .get(&cl.to_ascii_uppercase())
                    .cloned()
                    .unwrap_or_default();
                self.vars.push(var);
            }
        }
        Ok(name)
    }

    /// The name of a bit variable. `%B`, `%I` and `%Q` are the bit types; the
    /// done bits are handled by their owner because they are block members.
    fn bit_name(&mut self, vt: i32, vn: i32) -> Result<String, Vec<Diagnostic>> {
        let (prefix, direction) = match vt {
            VAR_MEM_BIT => ("B", None),
            VAR_PHYS_INPUT => ("I", Some(IoDirection::In)),
            VAR_PHYS_OUTPUT => ("Q", Some(IoDirection::Out)),
            other => {
                return Err(refuse(
                    "clp_variable",
                    format!("variable type {other} is not a bit LiNC can name"),
                ))
            }
        };
        let cl = format!("%{prefix}{vn}");
        self.declare(
            &cl,
            &format!("{}{vn}", prefix.to_ascii_lowercase()),
            direction,
            IoKind::Bit,
        )
    }

    /// The text of a word operand, for a compare or an arithmetic expression.
    fn word_text(&mut self, vt: i32, vn: i32) -> Result<String, Vec<Diagnostic>> {
        let io = |this: &mut Self, prefix: &str, direction, kind| {
            let cl = format!("%{prefix}{vn}");
            this.declare(
                &cl,
                &format!("{}{vn}", prefix.to_ascii_lowercase()),
                Some(direction),
                kind,
            )
        };
        match vt {
            VAR_MEM_WORD => {
                if !(0..crate::profile::resolve::marker_word_count() as i32).contains(&vn) {
                    return Err(refuse(
                        "clp_variable",
                        format!(
                            "`%W{vn}` is outside the marker words {}",
                            crate::profile::resolve::marker_range()
                        ),
                    ));
                }
                Ok(crate::profile::resolve::marker_word(vn as u16))
            }
            VAR_PHYS_WORD_INPUT => io(self, "IW", IoDirection::In, IoKind::Word),
            VAR_PHYS_WORD_OUTPUT => io(self, "QW", IoDirection::Out, IoKind::Word),
            VAR_PHYS_FLOAT_INPUT => io(self, "IF", IoDirection::In, IoKind::Float),
            VAR_PHYS_FLOAT_OUTPUT => io(self, "QF", IoDirection::Out, IoKind::Float),
            VAR_TIMER_IEC_PRESET => {
                Ok(block_member(self.timer_kind(vn)?, vn, Member::Preset))
            }
            VAR_TIMER_IEC_VALUE => Ok(block_member(self.timer_kind(vn)?, vn, Member::Value)),
            VAR_COUNTER_PRESET => Ok(block_member(self.counter_prefix(vn)?, vn, Member::Preset)),
            VAR_COUNTER_VALUE => Ok(block_member(self.counter_prefix(vn)?, vn, Member::Value)),
            VAR_TIMER_PRESET | VAR_TIMER_VALUE | VAR_MONOSTABLE_PRESET | VAR_MONOSTABLE_VALUE => {
                Err(refuse(
                    "clp_legacy_timer",
                    format!("variable type {vt} belongs to a legacy timer or monostable, which is not imported"),
                ))
            }
            other => Err(refuse(
                "clp_variable",
                format!("variable type {other} is not a word LiNC can name"),
            )),
        }
    }

    fn timer_kind(&self, n: i32) -> Result<FbName, Vec<Diagnostic>> {
        let (preset, mode) = self.iec_timers.get(n as usize).copied().ok_or_else(|| {
            refuse(
                "clp_member",
                format!("timer {n} is used but timers_iec.csv has no row {n}"),
            )
        })?;
        let _ = preset;
        Ok(match mode {
            TIMER_IEC_MODE_ON => FbName::Ton,
            TIMER_IEC_MODE_OFF => FbName::Tof,
            TIMER_IEC_MODE_PULSE => FbName::Tp,
            other => {
                return Err(refuse(
                    "clp_member",
                    format!("timer {n} has mode {other}, which is not ON, OFF or PULSE"),
                ))
            }
        })
    }

    fn counter_prefix(&self, n: i32) -> Result<FbName, Vec<Diagnostic>> {
        self.counter_kind.get(&n).copied().ok_or_else(|| {
            refuse(
                "clp_rung_syntax",
                format!("counter {n} is read but the ladder has no counter {n} element"),
            )
        })
    }

    /// Translate a ClassicLadder expression: `@type/number@` becomes a LiNC
    /// operand, and the rest is already the grammar `expr` parses.
    fn translate(&mut self, text: &str) -> Result<String, Vec<Diagnostic>> {
        if text.contains('[') {
            return Err(refuse(
                "clp_indexed",
                format!("`{text}` uses an indexed operand, which LiNC does not implement"),
            ));
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('@') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let end = after.find('@').ok_or_else(|| {
                refuse(
                    "clp_expression",
                    format!("`{text}`: an `@` operand is not closed"),
                )
            })?;
            let inner = &after[..end];
            let (vt, vn) = inner
                .split_once('/')
                .and_then(|(kind, number)| Some((parse_int(kind)?, parse_int(number)?)))
                .ok_or_else(|| {
                    refuse(
                        "clp_expression",
                        format!("`{text}`: `@{inner}@` is not `@type/number@`"),
                    )
                })?;
            out.push_str(&self.word_text(vt, vn)?);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    // --- rung mapping ------------------------------------------------------

    fn map_rung(
        &mut self,
        raw: &RawRung,
        labels: &HashMap<usize, String>,
        subroutine_pous: &HashMap<i32, String>,
        multi: &HashSet<usize>,
    ) -> Result<Vec<Rung>, Vec<Diagnostic>> {
        for y in 0..RUNG_HEIGHT {
            for x in 0..RUNG_WIDTH {
                if raw.at(x, y).indexed.is_some() {
                    return Err(refuse(
                        "clp_indexed",
                        format!(
                            "rung_{}: the cell at row {y} column {x} uses indirect addressing",
                            raw.index
                        ),
                    ));
                }
            }
        }
        let consumed = footprints(raw)?;
        check_drawn(raw, &consumed)?;
        if counter_position(raw).is_some() {
            return self.map_counter(raw, labels, subroutine_pous);
        }
        // A big element's filler may carry `ConnectedWithTop` as part of the
        // block's drawing; only a link outside a footprint is a branch.
        let tops: Vec<(usize, usize)> = (0..RUNG_HEIGHT)
            .flat_map(|y| (0..RUNG_WIDTH).map(move |x| (x, y)))
            .filter(|(x, y)| raw.at(*x, *y).top && !consumed.contains(&(*x, *y)))
            .collect();
        if tops.is_empty() {
            self.map_single(raw, labels, subroutine_pous, &consumed, multi)
        } else {
            self.map_branch(raw, labels, subroutine_pous, &tops, &consumed, multi)
        }
    }

    /// A rung with one row of cells and its coil, or with one branch below it.
    fn map_single(
        &mut self,
        raw: &RawRung,
        labels: &HashMap<usize, String>,
        subroutine_pous: &HashMap<i32, String>,
        consumed: &HashSet<(usize, usize)>,
        _multi: &HashSet<usize>,
    ) -> Result<Vec<Rung>, Vec<Diagnostic>> {
        let rows: Vec<usize> = (0..RUNG_HEIGHT)
            .filter(|y| row_has_content(raw, *y))
            .collect();
        if rows.len() > 1 {
            return Err(refuse(
                "clp_branch",
                format!(
                    "rung_{} fills rows {:?} without a vertical link; LiNC cannot express that shape",
                    raw.index, rows
                ),
            ));
        }
        let y = rows.first().copied().unwrap_or(0);
        let mut cells = self.map_strip(raw, y, 0, RUNG_WIDTH - 1, consumed)?;
        pad_cells(&mut cells, raw.index)?;
        let coil = self.map_coil(
            raw.at(RUNG_WIDTH - 1, y),
            y,
            raw.index,
            labels,
            subroutine_pous,
            None,
        )?;
        Ok(vec![Rung {
            label: labels[&raw.index].clone(),
            comment: raw.comment.clone(),
            logic: crate::model::Logic::Const { value: true },
            coil: Some(coil),
            cells,
        }])
    }

    /// The one branch shape LiNC can fold: a single vertical link, joining the
    /// row below at one column, with the branch row carrying only cells to the
    /// left of that column.
    fn map_branch(
        &mut self,
        raw: &RawRung,
        labels: &HashMap<usize, String>,
        subroutine_pous: &HashMap<i32, String>,
        tops: &[(usize, usize)],
        consumed: &HashSet<(usize, usize)>,
        _multi: &HashSet<usize>,
    ) -> Result<Vec<Rung>, Vec<Diagnostic>> {
        let branch = |detail: String| {
            refuse(
                "clp_branch",
                format!(
                    "rung_{}: {detail}; LiNC folds only one branch one row below",
                    raw.index
                ),
            )
        };
        if tops.len() != 1 {
            return Err(branch(format!("{} vertical links", tops.len())));
        }
        let (k, y) = tops[0];
        if y == 0 || k == 0 || k >= RUNG_WIDTH - 1 {
            return Err(branch(format!("a link at row {y} column {k}")));
        }
        let main = y - 1;
        // The branch is the row the link sits on; it may only carry cells left
        // of the link, and nothing else may be filled.
        for row in 0..RUNG_HEIGHT {
            if row != main && row != y && row_has_content(raw, row) {
                return Err(branch(format!("content on row {row}")));
            }
        }
        for x in k..RUNG_WIDTH {
            let element = raw.at(x, y);
            if !element.is_free() {
                return Err(branch(format!("a cell right of the link at column {x}")));
            }
        }
        let mut before = self.map_strip(raw, main, 0, k, consumed)?;
        let mut after = self.map_strip(raw, main, k, RUNG_WIDTH - 1, consumed)?;
        // The link costs a position, and trailing wires carry nothing, so they
        // are dropped rather than pushing the rung past nine positions.
        trim_trailing_wires(&mut after);
        before.push(Cell::Link);
        before.extend(after);
        let mut cells = before;
        pad_cells(&mut cells, raw.index)?;
        let coil = self.map_coil(
            raw.at(RUNG_WIDTH - 1, main),
            main,
            raw.index,
            labels,
            subroutine_pous,
            None,
        )?;
        let main_rung = Rung {
            label: labels[&raw.index].clone(),
            comment: raw.comment.clone(),
            logic: crate::model::Logic::Const { value: true },
            coil: Some(coil),
            cells,
        };
        // The branch row is that link's continuation, so its coil is blank.
        let mut branch_cells = self.map_strip(raw, y, 0, k, consumed)?;
        trim_trailing_wires(&mut branch_cells);
        pad_cells(&mut branch_cells, raw.index)?;
        let branch_rung = Rung {
            label: format!("{}-b", labels[&raw.index]),
            comment: String::new(),
            logic: crate::model::Logic::Const { value: true },
            coil: Some(Coil::new("", CoilKind::Normal)),
            cells: branch_cells,
        };
        Ok(vec![main_rung, branch_rung])
    }

    /// One strip of a row: the cells between two columns, with a big element's
    /// fillers skipped and its alive cell emitted as one position.
    fn map_strip(
        &mut self,
        raw: &RawRung,
        y: usize,
        from: usize,
        to: usize,
        consumed: &HashSet<(usize, usize)>,
    ) -> Result<Vec<Cell>, Vec<Diagnostic>> {
        let mut cells = Vec::new();
        for x in from..to {
            if consumed.contains(&(x, y)) {
                continue;
            }
            let element = raw.at(x, y);
            let cell = match element.ele {
                ELE_FREE | ELE_CONNECTION => Cell::Wire,
                ELE_INPUT => self.contact(element.var_type, element.var_num, false, None, raw.index)?,
                ELE_INPUT_NOT => self.contact(element.var_type, element.var_num, true, None, raw.index)?,
                ELE_RISING_INPUT => {
                    self.contact(element.var_type, element.var_num, false, Some(true), raw.index)?
                }
                ELE_FALLING_INPUT => {
                    self.contact(element.var_type, element.var_num, false, Some(false), raw.index)?
                }
                ELE_COMPAR => {
                    let text = self
                        .expressions
                        .get(&element.var_num)
                        .cloned()
                        .ok_or_else(|| {
                            refuse(
                                "clp_expression",
                                format!(
                                    "rung_{}: a compare uses expression {}, which arithmetic_expressions.csv does not define",
                                    raw.index, element.var_num
                                ),
                            )
                        })?;
                    Cell::Expr {
                        text: self.translate(&text)?,
                    }
                }
                ELE_TIMER_IEC => {
                    let n = element.var_num;
                    if !(0..FB_PER_TYPE as i32).contains(&n) {
                        return Err(refuse(
                            "clp_timer_instances",
                            format!("rung_{}: timer {n} is beyond the four LiNC instances", raw.index),
                        ));
                    }
                    let (preset, _) = self.iec_timers.get(n as usize).copied().ok_or_else(|| {
                        refuse(
                            "clp_member",
                            format!("rung_{}: timer {n} is used but timers_iec.csv has no row {n}", raw.index),
                        )
                    })?;
                    let kind = self.timer_kind(n)?;
                    Cell::Block {
                        block: kind.text().to_string(),
                        instance: block_instance(kind, n),
                        preset: crate::profile::resolve::time_preset(preset),
                    }
                }
                ELE_TIMER | ELE_MONOSTABLE => {
                    return Err(refuse(
                        "clp_legacy_timer",
                        format!(
                            "rung_{}: element {} is a legacy timer or monostable, which is not imported; use the IEC timer element",
                            raw.index, element.ele
                        ),
                    ))
                }
                ELE_COUNTER => {
                    return Err(refuse(
                        "clp_rung_syntax",
                        format!("rung_{}: a counter inside a branch", raw.index),
                    ))
                }
                ELE_OUTPUT
                | ELE_OUTPUT_NOT
                | ELE_OUTPUT_SET
                | ELE_OUTPUT_RESET
                | ELE_OUTPUT_JUMP
                | ELE_OUTPUT_CALL
                | ELE_OUTPUT_OPERATE => {
                    return Err(refuse(
                        "clp_rung_syntax",
                        format!(
                            "rung_{}: an output at row {y} column {x} is not in the last column",
                            raw.index
                        ),
                    ))
                }
                other => {
                    return Err(refuse(
                        "clp_rung_syntax",
                        format!("rung_{}: element {other} at row {y} column {x}", raw.index),
                    ))
                }
            };
            cells.push(cell);
        }
        Ok(cells)
    }

    /// A contact, or the synthesised compare that stands in for a counter's
    /// `done` bit (ClassicLadder compares equality, `CTU.Q` is `>=`).
    fn contact(
        &mut self,
        vt: i32,
        vn: i32,
        negated: bool,
        edge: Option<bool>,
        rung: usize,
    ) -> Result<Cell, Vec<Diagnostic>> {
        match vt {
            VAR_COUNTER_DONE => {
                if edge.is_some() {
                    return Err(refuse(
                        "clp_counter_edge",
                        format!("rung_{rung}: an edge contact on counter {vn}'s done output cannot be expressed"),
                    ));
                }
                let preset = self.counters.get(vn as usize).copied().ok_or_else(|| {
                    refuse(
                        "clp_member",
                        format!("the ladder reads counter {vn}'s done but counters.csv has no row {vn}"),
                    )
                })?;
                let kind = self.counter_prefix(vn)?;
                Ok(Cell::Compare {
                    op: if negated { "NE" } else { "EQ" }.to_string(),
                    left: block_member(kind, vn, Member::Value),
                    right: preset.to_string(),
                })
            }
            VAR_COUNTER_EMPTY | VAR_COUNTER_FULL => Err(refuse(
                "clp_counter_overflow",
                format!(
                    "rung_{rung}: counter {vn}'s {} output is read; LiNC has no rollover output and a substitute would be a different function",
                    if vt == VAR_COUNTER_EMPTY { "empty" } else { "full" }
                ),
            )),
            VAR_TIMER_IEC_DONE => {
                if edge.is_some() {
                    return Err(refuse(
                        "clp_timer_contact",
                        format!("rung_{rung}: an edge contact on timer {vn}'s done output cannot be expressed"),
                    ));
                }
                let kind = self.timer_kind(vn)?;
                Ok(Cell::Contact {
                    var: format!("{}.Q", block_instance(kind, vn)),
                    negated,
                })
            }
            VAR_TIMER_DONE | VAR_MONOSTABLE_RUNNING => Err(refuse(
                "clp_legacy_timer",
                format!("rung_{rung}: a contact on a legacy timer or monostable output"),
            )),
            _ => match edge {
                Some(rising) => Ok(Cell::Edge {
                    var: self.bit_name(vt, vn)?,
                    rising,
                }),
                None => Ok(Cell::Contact {
                    var: self.bit_name(vt, vn)?,
                    negated,
                }),
            },
        }
    }

    fn map_coil(
        &mut self,
        element: Element,
        _y: usize,
        rung: usize,
        labels: &HashMap<usize, String>,
        subroutine_pous: &HashMap<i32, String>,
        multi: Option<&HashSet<usize>>,
    ) -> Result<Coil, Vec<Diagnostic>> {
        match element.ele {
            ELE_OUTPUT => Ok(Coil::new(
                self.bit_name(element.var_type, element.var_num)?,
                CoilKind::Normal,
            )),
            ELE_OUTPUT_NOT => Ok(Coil::new(
                self.bit_name(element.var_type, element.var_num)?,
                CoilKind::Negated,
            )),
            ELE_OUTPUT_SET => Ok(Coil::new(
                self.bit_name(element.var_type, element.var_num)?,
                CoilKind::Set,
            )),
            ELE_OUTPUT_RESET => Ok(Coil::new(
                self.bit_name(element.var_type, element.var_num)?,
                CoilKind::Reset,
            )),
            ELE_OUTPUT_JUMP => {
                let target = element.var_num;
                if target < 0 {
                    return Err(refuse(
                        "clp_rung_chain",
                        format!("rung_{rung}: a jump has no target rung"),
                    ));
                }
                if multi.is_some_and(|multi| multi.contains(&(target as usize))) {
                    return Err(refuse(
                        "clp_rung_chain",
                        format!(
                            "rung_{rung}: the jump targets rung {target}, which becomes several LiNC rungs"
                        ),
                    ));
                }
                let label = labels.get(&(target as usize)).ok_or_else(|| {
                    refuse(
                        "clp_rung_chain",
                        format!(
                            "rung_{rung}: the jump targets rung {target}, outside this section"
                        ),
                    )
                })?;
                Ok(Coil::new(label.clone(), CoilKind::Jump))
            }
            ELE_OUTPUT_CALL => {
                let subroutine = element.var_num;
                // Validation refuses a call to a POU that does not exist, and its
                // `call_target_missing` names the target.
                let name = subroutine_pous
                    .get(&subroutine)
                    .cloned()
                    .unwrap_or_else(|| format!("sr{subroutine}"));
                Ok(Coil::new(name, CoilKind::Call))
            }
            ELE_OUTPUT_OPERATE => {
                let text = self
                    .expressions
                    .get(&element.var_num)
                    .cloned()
                    .ok_or_else(|| {
                        refuse(
                            "clp_expression",
                            format!(
                                "rung_{rung}: an operate uses expression {}, which arithmetic_expressions.csv does not define",
                                element.var_num
                            ),
                        )
                    })?;
                Ok(Coil::compute(self.translate(&text)?))
            }
            other => Err(refuse(
                "clp_rung_syntax",
                format!("rung_{rung}: the last column holds element {other}, not an output"),
            )),
        }
    }

    /// A counter rung: ClassicLadder's counter is one element with four input
    /// rows and three output rows, so one rung file becomes several LiNC rungs.
    fn map_counter(
        &mut self,
        raw: &RawRung,
        labels: &HashMap<usize, String>,
        subroutine_pous: &HashMap<i32, String>,
    ) -> Result<Vec<Rung>, Vec<Diagnostic>> {
        let consumed = footprints(raw)?;
        let (x, y) = counter_position(raw).unwrap_or((0, 0));
        let n = raw.at(x, y).var_num;
        if !(0..FB_PER_TYPE as i32).contains(&n) {
            return Err(refuse(
                "clp_counter_instances",
                format!(
                    "rung_{}: counter {n} is beyond the four LiNC instances",
                    raw.index
                ),
            ));
        }
        let preset = self.counters.get(n as usize).copied().ok_or_else(|| {
            refuse(
                "clp_member",
                format!(
                    "rung_{}: counter {n} is used but counters.csv has no row {n}",
                    raw.index
                ),
            )
        })?;
        if preset > COUNTER_WRAP {
            self.diagnostics.push(Diagnostic::warning(
                "clp_counter_wrap",
                format!(
                    "counter {n} has preset {preset}; ClassicLadder wraps at {COUNTER_WRAP} and LiNC saturates"
                ),
                format!("rung_{}", raw.index),
            ));
        }
        let kind = self.counter_prefix(n)?;
        let instance = block_instance(kind, n);

        // The four input rows, and the three output rows to the right.
        let mut strips: Vec<Vec<Cell>> = Vec::new();
        for row in y..y + 4 {
            strips.push(self.map_strip(raw, row, 0, x.saturating_sub(1), &consumed)?);
        }
        let mut right: Vec<Vec<Cell>> = Vec::new();
        for row in y..y + 4 {
            right.push(self.map_strip(raw, row, x + 1, RUNG_WIDTH - 1, &consumed)?);
        }
        let wired: Vec<bool> = (y..y + 4)
            .map(|row| {
                (0..x.saturating_sub(1)).any(|column| {
                    !raw.at(column, row).is_free() && raw.at(column, row).ele != ELE_UNUSABLE
                })
            })
            .collect();
        if wired[2] && wired[3] {
            return Err(refuse(
                "clp_counter_bidirectional",
                format!(
                    "rung_{}: counter {n} has both its up and down inputs wired, which LiNC cannot express as one counter",
                    raw.index
                ),
            ));
        }
        if !wired[0] && !wired[1] && !wired[2] && !wired[3] {
            return Err(refuse(
                "clp_rung_syntax",
                format!("rung_{}: counter {n} has no input wired", raw.index),
            ));
        }
        // Empty and full are rollover outputs LiNC does not have.
        for row in [0usize, 2] {
            if has_content(&right[row]) {
                return Err(refuse(
                    "clp_counter_overflow",
                    format!(
                        "rung_{}: counter {n}'s {} output is read; LiNC has no rollover output",
                        raw.index,
                        if row == 0 { "empty" } else { "full" }
                    ),
                ));
            }
        }
        if has_content(&right[3]) {
            return Err(refuse(
                "clp_rung_syntax",
                format!(
                    "rung_{}: cells to the right of counter {n}'s count-down row",
                    raw.index
                ),
            ));
        }

        let mut out = Vec::new();
        let label = labels[&raw.index].clone();
        let mut extra = 0usize;
        let next_label = |extra: &mut usize| {
            *extra += 1;
            format!("{label}-{extra}")
        };

        // 1. The counting block. ClassicLadder's counter has no coil, so the
        //    rung ends in a compute coil that writes CV back to itself: the
        //    block's output is consumed and nothing observable is written.
        let value = block_member(kind, n, Member::Value);
        let count_row = if wired[2] { 2 } else { 3 };
        let mut cells = strips[count_row].clone();
        cells.push(Cell::Block {
            block: kind.text().to_string(),
            instance: instance.clone(),
            preset: preset.to_string(),
        });
        pad_cells(&mut cells, raw.index)?;
        out.push(Rung {
            label: label.clone(),
            comment: raw.comment.clone(),
            logic: crate::model::Logic::Const { value: true },
            coil: Some(Coil::compute(format!("{value}={value}"))),
            cells,
        });

        // 2. The preset load (down counters), then the reset: applied in the
        //    order `CalcTypeCounter` applies them, so the reset wins.
        if wired[1] {
            if kind == FbName::Ctu {
                return Err(refuse(
                    "clp_counter_preset_load",
                    format!(
                        "rung_{}: counter {n} has its preset input wired but maps to a CTU, which has no load",
                        raw.index
                    ),
                ));
            }
            let mut cells = strips[1].clone();
            pad_cells(&mut cells, raw.index)?;
            out.push(Rung {
                label: next_label(&mut extra),
                comment: String::new(),
                logic: crate::model::Logic::Const { value: true },
                coil: Some(Coil::new(format!("{instance}.LD"), CoilKind::Normal)),
                cells,
            });
        }
        if wired[0] {
            if kind == FbName::Ctd && preset != 0 {
                return Err(refuse(
                    "clp_counter_reset",
                    format!(
                        "rung_{}: counter {n} is a down counter reset to zero but its preset is {preset}; loading the preset is not clearing",
                        raw.index
                    ),
                ));
            }
            let mut cells = strips[0].clone();
            pad_cells(&mut cells, raw.index)?;
            out.push(Rung {
                label: next_label(&mut extra),
                comment: String::new(),
                logic: crate::model::Logic::Const { value: true },
                // A down counter with preset 0: loading PV is `CV := 0`.
                coil: Some(Coil::new(
                    format!(
                        "{instance}.{}",
                        if kind == FbName::Ctu { "R" } else { "LD" }
                    ),
                    CoilKind::Normal,
                )),
                cells,
            });
        }

        // 3. The done output, read by the cells to its right.
        let coil_column = raw.at(RUNG_WIDTH - 1, y + 1);
        if has_content(&right[1]) || !coil_column.is_free() {
            let mut cells = vec![Cell::Compare {
                op: "EQ".to_string(),
                left: block_member(kind, n, Member::Value),
                right: preset.to_string(),
            }];
            cells.extend(right[1].clone());
            pad_cells(&mut cells, raw.index)?;
            let coil =
                self.map_coil(coil_column, y + 1, raw.index, labels, subroutine_pous, None)?;
            out.push(Rung {
                label: next_label(&mut extra),
                comment: String::new(),
                logic: crate::model::Logic::Const { value: true },
                coil: Some(coil),
                cells,
            });
        }
        Ok(out)
    }
}

/// ClassicLadder conducts through drawn cells only: a `ELE_FREE` gap stops the
/// power. LiNC's `Cell::Wire` conducts, so a gap that a `Wire` would bridge has
/// no faithful mapping and is refused.
fn check_drawn(raw: &RawRung, consumed: &HashSet<(usize, usize)>) -> Result<(), Vec<Diagnostic>> {
    for y in 0..RUNG_HEIGHT {
        let last = (0..RUNG_WIDTH)
            .filter(|x| {
                !consumed.contains(&(*x, y)) && {
                    let element = raw.at(*x, y);
                    !element.is_free() && element.ele != ELE_UNUSABLE
                }
            })
            .max();
        let Some(last) = last else {
            continue;
        };
        for x in 0..last {
            if consumed.contains(&(x, y)) {
                continue;
            }
            if raw.at(x, y).ele == ELE_FREE {
                return Err(refuse(
                    "clp_rung_syntax",
                    format!(
                        "rung_{}: row {y} has a gap at column {x}, so the element at column {last} is never powered",
                        raw.index
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// A big element's footprint: the columns and rows it occupies, with the alive
/// cell at the top-right. Every other cell in the footprint must be a filler.
fn footprints(raw: &RawRung) -> Result<HashSet<(usize, usize)>, Vec<Diagnostic>> {
    let mut consumed = HashSet::new();
    for y in 0..RUNG_HEIGHT {
        for x in 0..RUNG_WIDTH {
            let element = raw.at(x, y);
            let (columns, rows) = match element.ele {
                ELE_COMPAR | ELE_OUTPUT_OPERATE => (3, 1),
                ELE_TIMER_IEC | ELE_TIMER | ELE_MONOSTABLE => (2, 2),
                ELE_COUNTER => (2, 4),
                _ => continue,
            };
            if x + 1 < columns || y + rows > RUNG_HEIGHT {
                return Err(refuse(
                    "clp_rung_syntax",
                    format!(
                        "rung_{}: element {} at row {y} column {x} does not fit the grid",
                        raw.index, element.ele
                    ),
                ));
            }
            for fx in (x + 1 - columns)..=x {
                for fy in y..(y + rows) {
                    if (fx, fy) == (x, y) {
                        continue;
                    }
                    let filler = raw.at(fx, fy);
                    if !matches!(filler.ele, ELE_UNUSABLE | ELE_FREE | ELE_CONNECTION) {
                        return Err(refuse(
                            "clp_rung_syntax",
                            format!(
                                "rung_{}: row {fy} column {fx} holds element {} inside a {} block",
                                raw.index, filler.ele, element.ele
                            ),
                        ));
                    }
                    consumed.insert((fx, fy));
                }
            }
        }
    }
    Ok(consumed)
}

/// The direction a counter's wiring selects. Both inputs wired has no single
/// LiNC counter, and neither wired counts nothing.
fn counter_direction(raw: &RawRung, x: usize, y: usize) -> Result<FbName, Vec<Diagnostic>> {
    let number = raw.at(x, y).var_num;
    let wired = |row: usize| {
        (0..x.saturating_sub(1)).any(|column| {
            let element = raw.at(column, row);
            !element.is_free() && element.ele != ELE_UNUSABLE
        })
    };
    match (wired(y + 2), wired(y + 3)) {
        (true, true) => Err(refuse(
            "clp_counter_bidirectional",
            format!(
                "rung_{}: counter {number} has both its up and down inputs wired, which LiNC cannot express as one counter",
                raw.index
            ),
        )),
        (true, false) => Ok(FbName::Ctu),
        (false, true) => Ok(FbName::Ctd),
        (false, false) => Err(refuse(
            "clp_rung_syntax",
            format!("rung_{}: counter {number} has neither an up nor a down input wired", raw.index),
        )),
    }
}

fn counter_position(raw: &RawRung) -> Option<(usize, usize)> {
    for y in 0..RUNG_HEIGHT {
        for x in 0..RUNG_WIDTH {
            if raw.at(x, y).ele == ELE_COUNTER {
                return Some((x, y));
            }
        }
    }
    None
}

fn row_has_content(raw: &RawRung, y: usize) -> bool {
    (0..RUNG_WIDTH).any(|x| {
        let element = raw.at(x, y);
        !element.is_free() && element.ele != ELE_UNUSABLE
    })
}

fn has_content(cells: &[Cell]) -> bool {
    cells.iter().any(|cell| !matches!(cell, Cell::Wire))
}

fn trim_trailing_wires(cells: &mut Vec<Cell>) {
    while matches!(cells.last(), Some(Cell::Wire)) {
        cells.pop();
    }
}

fn pad_cells(cells: &mut Vec<Cell>, rung: usize) -> Result<(), Vec<Diagnostic>> {
    if cells.len() > POSITIONS {
        return Err(refuse(
            "clp_branch",
            format!(
                "rung_{rung}: the rung needs {} element positions; LiNC has {POSITIONS}",
                cells.len()
            ),
        ));
    }
    while cells.len() < POSITIONS {
        cells.push(Cell::Wire);
    }
    Ok(())
}
