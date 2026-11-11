//! The ladder document the UI edits and the daemon compiles.

use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub schema: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub io: Vec<IoPoint>,
    pub vars: Vec<VarDecl>,
    /// The ladder body of `MAIN` in a version-1 project. Version 2 projects
    /// carry `pous` instead; the field is kept so an old file still decodes and
    /// `main_sections` can find it.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Program organisation units, `MAIN` first, each in its own IEC 61131-3
    /// language. Empty means the version-1 shape: `sections` is `MAIN`.
    #[serde(default)]
    pub pous: Vec<Pou>,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoPoint {
    pub name: String,
    pub direction: IoDirection,
    pub kind: IoKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoDirection {
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    Bit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarDecl {
    pub name: String,
    pub kind: VarKind,
    #[serde(default)]
    pub retain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarKind {
    Bit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub rungs: Vec<Rung>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rung {
    #[serde(default)]
    pub comment: String,
    pub logic: Logic,
    pub coil: Coil,
    /// The ladder grid this row was drawn on. Empty means the rung is still the
    /// version-2 tree in `logic`; a non-empty grid is lowered by the compiler.
    #[serde(default)]
    pub cells: Vec<crate::grid::Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Logic {
    Const { value: bool },
    Contact { var: String, negated: bool },
    Series { items: Vec<Logic> },
    Parallel { items: Vec<Logic> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coil {
    pub var: String,
    pub kind: CoilKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoilKind {
    Normal,
    Set,
    Reset,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            name: name.into(),
            description: String::new(),
            io: Vec::new(),
            vars: Vec::new(),
            sections: Vec::new(),
            pous: Vec::new(),
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
        if !self.pous.is_empty() {
            return;
        }
        self.pous = self.all_pous();
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
                    cells: Vec::new(),
                    comment: "Seal in: start picks the seal up, stop breaks it.".to_string(),
                    logic: Logic::series(vec![
                        Logic::parallel(vec![
                            Logic::contact("start", false),
                            Logic::contact("seal", false),
                        ]),
                        Logic::contact("stop", true),
                    ]),
                    coil: Coil::new("seal", CoilKind::Normal),
                },
                Rung {
                    cells: Vec::new(),
                    comment: "The seal drives the run output.".to_string(),
                    logic: Logic::contact("seal", false),
                    coil: Coil::new("run", CoilKind::Normal),
                },
                Rung {
                    cells: Vec::new(),
                    comment: "Stopping while running latches the alarm.".to_string(),
                    logic: Logic::series(vec![
                        Logic::contact("stop", false),
                        Logic::contact("run", false),
                    ]),
                    coil: Coil::new("alarm", CoilKind::Set),
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
        }
    }
}

impl VarDecl {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: VarKind::Bit,
            retain: false,
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

impl Rung {
    pub fn new(logic: Logic, coil: Coil) -> Self {
        Self {
            comment: String::new(),
            logic,
            coil,
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
        }
    }
}
