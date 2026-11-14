//! The ladder document the UI edits and the daemon compiles.

use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

/// Which PLC family's semantics a project is written in.
///
/// One document, two meanings. `iec` is the semantics this tree shipped with:
/// a nine-position grid, the marker-word bank, and a counter whose `Q` is
/// `CV >= PV`.
/// `classicladder`
/// is the one the `.clp` importer now speaks: its counter's `done` is equality,
/// it wraps at 0..9999, it carries empty and full outputs, and its addresses are
/// `%B`/`%I`/`%Q`/`%W` — so an imported program means what it meant upstream
/// instead of being translated into something with the same shape.
///
/// The name is the project's, not the UI's: switching notation (III.3) changes no
/// byte of the document, and this field is a statement about meaning, not display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Dialect {
    #[default]
    #[serde(rename = "iec")]
    Iec,
    #[serde(rename = "classicladder")]
    ClassicLadder,
}

impl Dialect {
    /// The name the document uses, and the one diagnostics print.
    pub fn label(self) -> &'static str {
        match self {
            Self::Iec => "iec",
            Self::ClassicLadder => "classicladder",
        }
    }

    /// Omitted from the JSON when it is `iec`, so an iec project's bytes — and
    /// the program hash over them — are exactly what they were before this field
    /// existed. A retained store is keyed by that hash; adding a field must not
    /// invalidate every store on the machine.
    fn is_default(&self) -> bool {
        *self == Self::Iec
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub schema: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub io: Vec<IoPoint>,
    pub vars: Vec<VarDecl>,
    /// Operands pinned to the watch and trend pages, at most sixteen.
    #[serde(default)]
    pub watch: Vec<String>,
    /// The ladder body of `MAIN` in a version-1 project. Version 2 projects
    /// carry `pous` instead; the field is kept so an old file still decodes and
    /// `main_sections` can find it.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Program organisation units, `MAIN` first, each in its own IEC 61131-3
    /// language. Empty means the version-1 shape: `sections` is `MAIN`.
    #[serde(default)]
    pub pous: Vec<Pou>,
    /// Which dialect's semantics every instruction in this project has.
    #[serde(default, skip_serializing_if = "Dialect::is_default")]
    pub profile: Dialect,
}

/// One program organisation unit, as the POU list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pou {
    pub name: String,
    #[serde(default)]
    pub language: PouLanguage,
    #[serde(default)]
    pub comment: String,
    /// Ladder body, used when `language` is `ld`.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Instruction-list body, one source line per entry, used when `language`
    /// is `il`.
    #[serde(default)]
    pub il: Vec<String>,
    /// Function-block body, one assignment per entry, used when `language` is
    /// `fbd`. Kept for files written before the grid; the compiler prefers
    /// `fbd_grid` when it is present.
    #[serde(default)]
    pub fbd: Vec<FbdLine>,
    /// Function-block body as the grid the editor draws. Absent means the POU
    /// still uses `fbd`.
    #[serde(default)]
    pub fbd_grid: Option<FbdGrid>,
}

/// The IEC 61131-3 languages a POU can be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PouLanguage {
    Il,
    #[default]
    Ld,
    Fbd,
}

impl PouLanguage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Il => "IL",
            Self::Ld => "LD",
            Self::Fbd => "FBD",
        }
    }
}

impl Pou {
    pub fn new(name: impl Into<String>, language: PouLanguage) -> Self {
        Self {
            name: name.into(),
            language,
            comment: String::new(),
            sections: Vec::new(),
            il: Vec::new(),
            fbd: Vec::new(),
            fbd_grid: None,
        }
    }
}

/// One cell of the FBD grid. `Empty` is a hole a block's body occupies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cell", rename_all = "snake_case")]
pub enum FbdCell {
    Empty,
    Wire,
    /// A signal entering from the left; `!` negates it.
    In {
        operand: String,
    },
    /// A signal leaving to the right.
    Out {
        operand: String,
    },
}

/// A block on the grid. `row`/`col` is its anchor: the output leaves there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FbdBlock {
    pub row: usize,
    pub col: usize,
    /// Rows the block spans, which is how many inputs it draws.
    pub ins: usize,
    pub kind: String,
    #[serde(default)]
    pub instance: String,
    #[serde(default)]
    pub preset: String,
}

/// The FBD grid: cells for signals, a table for blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FbdGrid {
    #[serde(default)]
    pub columns: usize,
    #[serde(default)]
    pub rows: usize,
    #[serde(default)]
    pub cells: Vec<Vec<FbdCell>>,
    #[serde(default)]
    pub blocks: Vec<FbdBlock>,
}

impl FbdGrid {
    pub const DEFAULT_COLUMNS: usize = 12;

    pub fn cell(&self, row: usize, col: usize) -> FbdCell {
        self.cells
            .get(row)
            .and_then(|line| line.get(col))
            .cloned()
            .unwrap_or(FbdCell::Empty)
    }

    pub fn set_cell(&mut self, row: usize, col: usize, cell: FbdCell) {
        // Grow the columns first: the rows were padded to the *old* `columns`, so a
        // grid that started at zero columns silently dropped every cell it was
        // given (`line.get_mut(col)` found nothing).
        self.columns = self.columns.max(col + 1);
        while self.cells.len() <= row {
            self.cells.push(vec![FbdCell::Empty; self.columns]);
        }
        for line in self.cells.iter_mut() {
            while line.len() < self.columns {
                line.push(FbdCell::Empty);
            }
        }
        while self.rows <= row {
            self.rows += 1;
        }
        if let Some(line) = self.cells.get_mut(row) {
            if let Some(slot) = line.get_mut(col) {
                *slot = cell;
            }
        }
    }

    /// Name of the operand string for a block's input at `row`, or `None` when
    /// the cell holds no signal. An empty cell next to a block's output is the
    /// chaining case and yields `Some("")`.
    pub fn input_at(&self, row: usize, col: usize) -> Option<String> {
        match self.cell(row, col) {
            FbdCell::In { operand } => Some(operand),
            FbdCell::Wire => Some(String::new()),
            _ => None,
        }
    }

    /// Lay the line-based body out on the grid, one block per line, in order.
    ///
    /// Evaluation order is preserved because each line gets its own rows, so
    /// block `n` on the grid is line `n` and an `@n` reference still names the
    /// same block.
    pub fn from_lines(lines: &[FbdLine]) -> Self {
        let columns = Self::DEFAULT_COLUMNS;
        let mut grid = FbdGrid {
            columns,
            rows: 0,
            cells: Vec::new(),
            blocks: Vec::new(),
        };
        let mut row = 0;
        for line in lines {
            let ins = line.inputs.len().max(1);
            let col = (columns / 2).max(1);
            for (index, input) in line.inputs.iter().enumerate() {
                grid.set_cell(
                    row + index,
                    col.saturating_sub(1),
                    FbdCell::In {
                        operand: input.clone(),
                    },
                );
            }
            grid.set_cell(
                row,
                col + 1,
                FbdCell::Out {
                    operand: line.output.clone(),
                },
            );
            for index in 1..ins {
                for column in col.saturating_sub(1)..=col + 1 {
                    if matches!(grid.cell(row + index, column), FbdCell::Empty) {
                        grid.set_cell(row + index, column, FbdCell::Empty);
                    }
                }
            }
            grid.blocks.push(FbdBlock {
                row,
                col,
                ins,
                kind: line.block.clone(),
                instance: String::new(),
                preset: line.preset.clone(),
            });
            row += ins;
        }
        grid.rows = row.max(grid.cells.len());
        grid
    }
}

/// One function-block assignment: `output := block(inputs…)`.
///
/// A block's output may feed another's
/// input. Here a line is the same idea named once: an input may be any declared
/// operand, including the output of an earlier line, which is how blocks nest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FbdLine {
    /// The operand the block drives.
    pub output: String,
    /// `AND`, `OR`, `XOR`, `NOT`, a timer or counter instance (`TON0`), or a
    /// compare operator (`GT`, `GE`, `EQ`, `NE`, `LE`, `LT`).
    pub block: String,
    /// Block inputs, in order. A compare takes two word values, `NOT` one bit,
    /// a timer or counter one bit.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Timer preset (`T#4s`) or counter preset (`10`).
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub comment: String,
}

impl FbdLine {
    pub fn new(output: impl Into<String>, block: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            block: block.into(),
            inputs: Vec::new(),
            preset: String::new(),
            comment: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoPoint {
    pub name: String,
    pub direction: IoDirection,
    pub kind: IoKind,
    /// Free text shown next to the operand, as a symbol table has.
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoDirection {
    In,
    Out,
}

/// What a bound point carries.
///
/// `Word` is a 32-bit `s32` HAL pin; `Float` is a `float` pin the VM sees as the
/// integer view of it, which is ClassicLadder's `%IF`/`%QF` behaviour. The kind
/// decides the pin type the daemon creates and which bank region the value lives
/// in, so a point cannot be read as a kind it was not declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    #[default]
    Bit,
    Word,
    Float,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarDecl {
    pub name: String,
    pub kind: VarKind,
    /// The declared type, as the importer read it.
    ///
    /// Additive and defaulted: a version-1 project has no `ty` key and decodes to
    /// `Bool`, and a `Bool` is not written back, so a project that declares no
    /// types serialises byte for byte as it did and its `project_hash` does not
    /// move.
    #[serde(default, skip_serializing_if = "VarType::is_bool")]
    pub ty: VarType,
    /// Initial word values, in element order, for a declaration that owns bank
    /// slots: the six durations of a `DINT[6]` are six of these.
    ///
    /// Additive and skipped when empty, so a project that declares no initial
    /// values serialises exactly as it did. A bit variable has none: a bit image
    /// starts false, and a retained bit is what a store is for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub init: Vec<i32>,
    #[serde(default)]
    pub retain: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarKind {
    Bit,
}

/// A declared variable's type.
///
/// The importer reads it, the word bank gives the word types a slot, and a
/// refusal can name it. `Derived` is a type this project declares itself: the name
/// is kept rather than resolved, so the refusal says `MotorStarter` and not
/// "unknown type".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarType {
    #[default]
    Bool,
    Int,
    Dint,
    Real,
    /// Milliseconds, as `s32` — the bank's unit for a time value.
    Time,
    /// A type the project declares in its own `<dataTypes>`.
    Derived(String),
    Array {
        base: Box<VarType>,
        len: u32,
    },
}

impl VarType {
    /// How many word-bank slots the declaration owns. A bit variable owns none:
    /// it lives in the bit image. A structured type owns none either, and a
    /// declaration of one is refused by validation rather than laid out.
    pub fn slots(&self) -> u16 {
        match self {
            Self::Bool | Self::Derived(_) => 0,
            Self::Int | Self::Dint | Self::Real | Self::Time => 1,
            Self::Array { base, len } => base
                .slots()
                .saturating_mul(u16::try_from(*len).unwrap_or(u16::MAX)),
        }
    }

    /// The declared type as an operator reads it: `DINT`, `INT[1]`, `Recipe[3]`.
    pub fn label(&self) -> String {
        match self {
            Self::Bool => "BOOL".to_string(),
            Self::Int => "INT".to_string(),
            Self::Dint => "DINT".to_string(),
            Self::Real => "REAL".to_string(),
            Self::Time => "TIME".to_string(),
            Self::Derived(name) => name.clone(),
            Self::Array { base, len } => format!("{}[{len}]", base.label()),
        }
    }

    pub fn is_bool(&self) -> bool {
        *self == Self::Bool
    }

    /// How many elements the declaration has: an array's length, otherwise one.
    pub fn elements(&self) -> u16 {
        match self {
            Self::Array { base, len } => base
                .elements()
                .saturating_mul(u16::try_from(*len).unwrap_or(u16::MAX)),
            _ => 1,
        }
    }

    /// How many bits one element of this type has, when a program may address them.
    ///
    /// `None` for a bit variable (it has no word to address into), a `REAL` (its
    /// 32 bits are a value, not a field of flags) and a structured type. `TIME` is
    /// a 32-bit count of milliseconds, so its bits are addressable like a `DINT`.
    pub fn bit_width(&self) -> Option<u8> {
        match self {
            Self::Int => Some(16),
            Self::Dint | Self::Time => Some(32),
            Self::Array { base, .. } => base.bit_width(),
            Self::Bool | Self::Real | Self::Derived(_) => None,
        }
    }

    /// A type this build can name but has no storage for: a structured type, or an
    /// array of one.
    pub fn is_structured(&self) -> bool {
        match self {
            Self::Derived(_) => true,
            Self::Array { base, .. } => base.is_structured(),
            _ => false,
        }
    }
}

/// One bit inside a declared word: `Bits[0].0` in a project, a marker word's bit in
/// an address.
///
/// The form is an operand, not a value: a contact reads the bit and a coil writes
/// it, while the word around it keeps its other bits. The spelling is parsed in one
/// place so the compiler, the validator and the editor cannot disagree about where
/// the bit is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordBit {
    /// The word the bit lives in: a declared variable's name, or a marker word's
    /// own spelling, which the profile owns.
    pub word: String,
    /// The array element the bit belongs to, when the operand names one.
    pub element: Option<u32>,
    /// The bit within the word, counted from its least significant end.
    pub bit: u8,
}

impl WordBit {
    /// Read one operand's spelling: `word.bit`, `word[element].bit`, or a marker
    /// word followed by its bit.
    ///
    /// `None` means the text is not this form. The bit index fits a byte here and
    /// is checked against the declaration's width where the declaration is known.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (word, bit) = text.rsplit_once('.')?;
        let bit = bit.trim().parse::<u8>().ok()?;
        let word = word.trim();
        if word.is_empty() {
            return None;
        }
        let (word, element) = match word.strip_suffix(']') {
            Some(head) => {
                let (name, index) = head.rsplit_once('[')?;
                let element = index.trim().parse::<u32>().ok()?;
                (name.trim(), Some(element))
            }
            None => (word, None),
        };
        if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            word: word.to_string(),
            element,
            bit,
        })
    }
}

/// One element of a declared array, read as an operand: `Durations[Phase]`.
///
/// The array and the index are separate so each is resolved against its own
/// declaration: the array decides the slot and the element count, the index is
/// either a number or a declared word read each scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayElement {
    pub array: String,
    pub index: String,
}

impl ArrayElement {
    /// Read one operand's spelling. The bracket has to be the whole tail, so
    /// `Bits[0].0` is a bit of a word and not this form.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let inner = text.strip_suffix(']')?;
        let (array, index) = inner.split_once('[')?;
        let array = array.trim();
        let index = index.trim();
        if array.is_empty() || index.is_empty() || index.contains(['[', ']']) {
            return None;
        }
        Some(Self {
            array: array.to_string(),
            index: index.to_string(),
        })
    }

    /// The element number, when the index is a literal.
    pub fn literal(&self) -> Option<i64> {
        self.index.parse::<i64>().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub rungs: Vec<Rung>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rung {
    /// The label a jump coil in this section can name. It marks the point
    /// before this rung's logic, and it is local to its section: two sections
    /// may each have a `LOOP`.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub comment: String,
    pub logic: Logic,
    /// The coil that ends this rung, if it has one.
    ///
    /// A rung may have none: a block that feeds no coil is a real rung in
    /// IEC 61131-3 and in the PLCYard corpus, and `coil: Some(..)` on every rung
    /// forced the importer to drop it (or invent a coil). Additive and
    /// defaulted, so a version-1 file with `"coil": {..}` still decodes to
    /// `Some`, and one without the key decodes to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coil: Option<Coil>,
    /// The ladder grid this row was drawn on. Empty means the rung is still the
    /// version-2 tree in `logic`; a non-empty grid is lowered by the compiler.
    #[serde(default)]
    pub cells: Vec<crate::grid::Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Logic {
    /// A function block in a rung: the power so far is its input, `Q` the power
    /// on. `preset` is the `PT`/`PV` a block was imported or drawn with.
    Block {
        block: String,
        #[serde(default)]
        instance: String,
        #[serde(default)]
        preset: String,
    },
    /// A rising or falling edge contact.
    Edge {
        var: String,
        rising: bool,
    },
    /// A compare contact over two word values. `operator` is GT, GE, EQ, NE, LE
    /// or LT; the tag already owns the name `op`.
    Compare {
        operator: String,
        left: String,
        right: String,
    },
    Const {
        value: bool,
    },
    Contact {
        var: String,
        negated: bool,
    },
    /// A free-text boolean expression, the tree form of `Cell::Expr`. The text
    /// is the ClassicLadder operand spelling after import (a comparison over
    /// marker words), parsed by `crate::expr`.
    Expr {
        text: String,
    },
    Series {
        items: Vec<Logic>,
    },
    Parallel {
        items: Vec<Logic>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coil {
    pub var: String,
    pub kind: CoilKind,
    /// A compute coil's assignment text (an assignment over marker words). Only
    /// `CoilKind::Compute`
    /// reads it; validation refuses the field on any other kind rather than
    /// ignoring it.
    #[serde(default)]
    pub expr: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoilKind {
    Normal,
    /// `-[\]-`: the coil is energised while the rung is **not** passing.
    Negated,
    Set,
    Reset,
    /// `-[J]-`: jump to the rung whose label is `coil.var`, in this section.
    /// The rung's power decides: powered jumps, unpowered falls through.
    Jump,
    /// `-[C]-`: call the POU named by `coil.var` and return. The callee is
    /// inlined, so a call has no locals and no stack frame.
    Call,
    /// `-[=]-`: evaluate `coil.expr` as `target = value` and write it, while the
    /// rung passes. `coil.var` is unused; the target is inside the expression.
    Compute,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            name: name.into(),
            description: String::new(),
            io: Vec::new(),
            vars: Vec::new(),
            watch: Vec::new(),
            sections: Vec::new(),
            pous: Vec::new(),
            profile: Dialect::Iec,
        }
    }

    /// The POU list the UI and the compiler share.
    ///
    /// A version-1 project has no `pous`; its `sections` are `MAIN`, so the
    /// caller gets one entry and every older file keeps working. The clone is
    /// cheap and only happens on load and on a POU edit, not per scan.
    pub fn all_pous(&self) -> Vec<Pou> {
        if self.pous.is_empty() {
            vec![Pou {
                name: "MAIN".to_string(),
                language: PouLanguage::Ld,
                comment: self.description.clone(),
                sections: self.sections.clone(),
                il: Vec::new(),
                fbd: Vec::new(),
                fbd_grid: None,
            }]
        } else {
            self.pous.clone()
        }
    }

    /// Replace the POU list, folding it back into the version-1 fields when it
    /// is the single `MAIN` ladder body so both representations agree.
    pub fn set_pous(&mut self, pous: Vec<Pou>) {
        self.pous = pous;
        if let Some(main) = self.pous.first() {
            if main.name.eq_ignore_ascii_case("main")
                && main.language == PouLanguage::Ld
                && self.pous.len() == 1
            {
                self.sections = main.sections.clone();
            }
        }
    }

    /// A version-1 file written before POUs existed must not lose its ladder.
    pub fn normalise(&mut self) {
        if self.pous.is_empty() {
            self.pous = self.all_pous();
        }
        // A ladder written as `Logic` trees is laid out as grid rows here, once,
        // so the editor has one model: every rung is cells.
        let mut pous = self.pous.clone();
        for pou in pous.iter_mut() {
            if pou.language != PouLanguage::Ld {
                continue;
            }
            for section in pou.sections.iter_mut() {
                crate::grid::migrate_section(section);
            }
        }
        self.pous = pous.clone();
        if pous.len() == 1 && pous[0].name.eq_ignore_ascii_case("main") {
            self.sections = pous[0].sections.clone();
        }
    }

    /// A small valid project: series and parallel logic, a Set and a Reset coil.
    ///
    /// The duplicate-coil rule allows one coil per target per section, so three
    /// rungs need three distinct coil targets: the `run` output and two internal
    /// variables.
    pub fn example() -> Self {
        let mut project = Project::new("example");
        project.description = "Two buttons latch a run output and reset an alarm.".to_string();
        project.io = vec![
            IoPoint::new("start", IoDirection::In),
            IoPoint::new("stop", IoDirection::In),
            IoPoint::new("run", IoDirection::Out),
        ];
        project.vars = vec![VarDecl::new("seal"), VarDecl::new("alarm")];
        project.sections = vec![Section {
            name: "main".to_string(),
            rungs: vec![
                Rung {
                    label: String::new(),
                    cells: Vec::new(),
                    comment: "Seal in: start picks the seal up, stop breaks it.".to_string(),
                    logic: Logic::series(vec![
                        Logic::parallel(vec![
                            Logic::contact("start", false),
                            Logic::contact("seal", false),
                        ]),
                        Logic::contact("stop", true),
                    ]),
                    coil: Some(Coil::new("seal", CoilKind::Normal)),
                },
                Rung {
                    label: String::new(),
                    cells: Vec::new(),
                    comment: "The seal drives the run output.".to_string(),
                    logic: Logic::contact("seal", false),
                    coil: Some(Coil::new("run", CoilKind::Normal)),
                },
                Rung {
                    label: String::new(),
                    cells: Vec::new(),
                    comment: "Stopping while running latches the alarm.".to_string(),
                    // The falling edge of `run` is the stop that must be caught:
                    // with sequential rungs `run` is already low by the time this
                    // rung reads it, so a level on `run` could never see the stop
                    // that turned it off.
                    logic: Logic::series(vec![
                        Logic::contact("stop", false),
                        Logic::Edge {
                            var: "run".to_string(),
                            rising: false,
                        },
                    ]),
                    coil: Some(Coil::new("alarm", CoilKind::Set)),
                },
            ],
        }];
        project
    }
}

impl IoPoint {
    pub fn new(name: impl Into<String>, direction: IoDirection) -> Self {
        Self {
            name: name.into(),
            direction,
            kind: IoKind::Bit,
            comment: String::new(),
        }
    }
}

impl VarDecl {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: VarKind::Bit,
            ty: VarType::Bool,
            init: Vec::new(),
            retain: false,
            comment: String::new(),
        }
    }

    /// A declared variable with a word type: it owns bank slots instead of a bit.
    pub fn typed(name: impl Into<String>, ty: VarType) -> Self {
        Self {
            ty,
            ..Self::new(name)
        }
    }
}

impl Section {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rungs: Vec::new(),
        }
    }
}

/// A blank coil, for the read path of a rung that has none: the editor, the
/// compiler and the validator read `rung.coil()` and ask `rung.has_coil()`
/// before they write.
static BLANK_COIL: Coil = Coil {
    var: String::new(),
    kind: CoilKind::Normal,
    expr: None,
};

impl Rung {
    /// The rung's coil, or a blank one when it has none. A blank coil is the
    /// no-coil answer for readers that only need a default; `has_coil` is the
    /// question to ask before writing or reporting.
    pub fn coil(&self) -> &Coil {
        self.coil.as_ref().unwrap_or(&BLANK_COIL)
    }

    pub fn has_coil(&self) -> bool {
        self.coil.is_some()
    }

    /// The coil for editing: a rung with none gets a blank one, because an edit
    /// is about to name it.
    pub fn coil_mut(&mut self) -> &mut Coil {
        self.coil
            .get_or_insert_with(|| Coil::new("", CoilKind::Normal))
    }

    /// Put a coil on the rung, replacing any it had.
    pub fn set_coil(&mut self, coil: Coil) {
        self.coil = Some(coil);
    }

    /// A rung with no coil: cells drive outputs of their own, or the rung is a
    /// block whose result nothing stores.
    pub fn without_coil(logic: Logic) -> Self {
        Self {
            label: String::new(),
            comment: String::new(),
            logic,
            coil: None,
            cells: Vec::new(),
        }
    }

    /// A row with nothing but a comment: no cells and no coil. It is drawn and
    /// compiled as a comment, not as a rung.
    /// A tree rung that has already been laid out as cells keeps its old tree
    /// in `logic`; this marks it so a second pass does not migrate it twice.
    pub fn logic_migrated(&self) -> bool {
        let _ = self;
        false
    }

    pub fn is_comment_row(&self) -> bool {
        // A jump, call or compute coil is an instruction even with no operand:
        // an empty one is an error the validator must see, not a comment. A rung
        // with no coil at all is a comment only when it draws nothing either.
        let blank = match &self.coil {
            None => true,
            Some(coil) => {
                !matches!(
                    coil.kind,
                    CoilKind::Jump | CoilKind::Call | CoilKind::Compute
                ) && coil.var.trim().is_empty()
            }
        };
        blank
            && self
                .cells
                .iter()
                .all(|cell| matches!(cell, crate::grid::Cell::Wire))
    }

    pub fn new(logic: Logic, coil: Coil) -> Self {
        Self {
            label: String::new(),
            comment: String::new(),
            logic,
            coil: Some(coil),
            cells: Vec::new(),
        }
    }
}

impl Logic {
    pub fn contact(var: impl Into<String>, negated: bool) -> Self {
        Logic::Contact {
            var: var.into(),
            negated,
        }
    }

    pub fn series(items: Vec<Logic>) -> Self {
        Logic::Series { items }
    }

    pub fn parallel(items: Vec<Logic>) -> Self {
        Logic::Parallel { items }
    }
}

impl Coil {
    pub fn new(var: impl Into<String>, kind: CoilKind) -> Self {
        Self {
            var: var.into(),
            kind,
            expr: None,
        }
    }

    /// A compute coil: the assignment text is the operand.
    pub fn compute(expr: impl Into<String>) -> Self {
        Self {
            var: String::new(),
            kind: CoilKind::Compute,
            expr: Some(expr.into()),
        }
    }
}
