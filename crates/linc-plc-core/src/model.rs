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
    pub sections: Vec<Section>,
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
                    comment: "The seal drives the run output.".to_string(),
                    logic: Logic::contact("seal", false),
                    coil: Coil::new("run", CoilKind::Normal),
                },
                Rung {
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
