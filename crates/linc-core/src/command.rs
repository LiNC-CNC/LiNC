//! The closed machine command set.
//!
//! Machine-control commands (cycle, jog, spindle, coolant, overrides, E-stop)
//! are intentionally not representable.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{AxisMap, ProbeResult, ToolChangeToken, ToolRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MdiOrigin {
    /// Explicit MDI screen command.
    Mdi,
    /// Touch probe routine.
    Probe,
    /// `Tn M6` from the Tools page.
    ToolChange,
    /// Operator confirmation of a manual tool change.
    ManualToolChange,
    /// Panel PROG.HOME path (observed by the HMI, issued by the panel service).
    ProgHome,
}

impl MdiOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mdi => "MDI",
            Self::Probe => "PROBE",
            Self::ToolChange => "TOOL CHANGE",
            Self::ManualToolChange => "MANUAL CHANGE",
            Self::ProgHome => "PROG.HOME",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProbeTarget {
    WorkOffset(u8),
    Calibration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LincOutput {
    HmiAlive,
    ToolChangeConfirm,
    StatusDisplay,
    /// Selected jog increment in machine units, for `halui.axis.L.increment`.
    /// 0.0 means continuous jog. Chosen in the HMI because the panel has no
    /// increment selector, matching how Probe Basic picks it in its GUI.
    JogIncrement,
    /// Jog speed in machine units per second, for `halui.axis.jog-speed`.
    /// The HMI scales the INI's default linear velocity by the operator's
    /// percentage, so the machine HAL does not need a multiplier component.
    JogSpeed,
}

impl LincOutput {
    pub const ALLOWED: [LincOutput; 5] = [
        LincOutput::HmiAlive,
        LincOutput::ToolChangeConfirm,
        LincOutput::StatusDisplay,
        LincOutput::JogIncrement,
        LincOutput::JogSpeed,
    ];

    pub fn pin_name(self) -> &'static str {
        match self {
            Self::HmiAlive => "linc.hmi-alive",
            Self::ToolChangeConfirm => "linc.tool-change-confirm",
            Self::StatusDisplay => "linc.status-display",
            Self::JogIncrement => "linc.jog-increment",
            Self::JogSpeed => "linc.jog-speed",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::HmiAlive => "hmi-alive",
            Self::ToolChangeConfirm => "tool-change-confirm",
            Self::StatusDisplay => "status-display",
            Self::JogIncrement => "jog-increment",
            Self::JogSpeed => "jog-speed",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "hmi-alive" | "linc.hmi-alive" => Some(Self::HmiAlive),
            "tool-change-confirm" | "linc.tool-change-confirm" => Some(Self::ToolChangeConfirm),
            "status-display" | "linc.status-display" => Some(Self::StatusDisplay),
            "jog-increment" | "linc.jog-increment" => Some(Self::JogIncrement),
            "jog-speed" | "linc.jog-speed" => Some(Self::JogSpeed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum HalValue {
    Bit(bool),
    S32(i32),
    Float(f64),
}

impl HalValue {
    pub fn matches(self, output: LincOutput) -> bool {
        matches!(
            (self, output),
            (HalValue::Bit(_), LincOutput::HmiAlive)
                | (HalValue::Bit(_), LincOutput::ToolChangeConfirm)
                | (HalValue::S32(_), LincOutput::StatusDisplay)
                | (HalValue::Float(_), LincOutput::JogIncrement)
                | (HalValue::Float(_), LincOutput::JogSpeed)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MachineCommand {
    IssueMdi {
        block: String,
        origin: MdiOrigin,
    },
    IssueToolChange {
        tool: u16,
    },
    ConfirmManualToolChange {
        token: ToolChangeToken,
    },
    SetCurrentTool {
        tool: u16,
    },
    /// Toggle the controller's limits override, which accepts jogging off a
    /// limit switch. The controller owns the state; this only asks it to flip.
    ToggleLimitsOverride,
    LoadProgram {
        path: PathBuf,
        expected_revision: Option<u64>,
    },
    SaveProgram {
        path: PathBuf,
        expected_revision: u64,
        content: String,
    },
    SetWorkOffset {
        index: u8,
        values: AxisMap<f64>,
        rotation: Option<f64>,
    },
    SetG92Offset {
        values: AxisMap<f64>,
    },
    /// Touch off: make the current position read zero in work offset `index`
    /// for `axes`. Issues `G10 L20`, so LinuxCNC computes the offset itself
    /// rather than the HMI deriving it from a possibly stale snapshot.
    ZeroWorkOffset {
        index: u8,
        axes: Vec<String>,
    },
    SetToolTableRow {
        row: ToolRow,
        expected_revision: u64,
    },
    ApplyProbeResult {
        result: ProbeResult,
        target: ProbeTarget,
    },
    ResetProbeCalibration,
    SetHalOutput {
        output: LincOutput,
        value: HalValue,
    },
    /// Start the loaded program at `line`, skipping everything before it.
    ///
    /// This is the one program-start the HMI initiates: a line number cannot
    /// come from the panel. It still requires the panel to be in AUTO, so the
    /// panel remains the authority over whether the machine may run.
    RunFromLine {
        line: u32,
    },
    /// View-only: clear the live tool trail and re-seed at the current tip.
    /// This never changes machine state or the loaded program.
    ClearTrail,
}

impl MachineCommand {
    pub fn label(&self) -> &'static str {
        match self {
            Self::IssueMdi { .. } => "Issue MDI",
            Self::IssueToolChange { .. } => "Tool change",
            Self::ConfirmManualToolChange { .. } => "Confirm manual tool change",
            Self::SetCurrentTool { .. } => "Set current tool",
            Self::ToggleLimitsOverride => "Toggle limits override",
            Self::LoadProgram { .. } => "Load program",
            Self::SaveProgram { .. } => "Save program",
            Self::SetWorkOffset { .. } => "Set work offset",
            Self::SetG92Offset { .. } => "Set G92 offset",
            Self::ZeroWorkOffset { .. } => "Zero work offset",
            Self::RunFromLine { .. } => "Run from line",
            Self::SetToolTableRow { .. } => "Set tool table row",
            Self::ApplyProbeResult { .. } => "Apply probe result",
            Self::ResetProbeCalibration => "Reset probe calibration",
            Self::SetHalOutput { .. } => "Set HAL output",
            Self::ClearTrail => "Clear live trail",
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Serialize, Deserialize)]
pub enum MachineError {
    #[error("machine is not connected")]
    NotConnected,
    #[error("panel link is not available")]
    PanelOffline,
    #[error("panel mode is invalid; failing closed")]
    InvalidPanelMode,
    #[error("program protect is ON (or unknown); editing is locked")]
    ProgramProtected,
    #[error("machine is not ON")]
    MachineOff,
    #[error("axes are not homed")]
    NotHomed,
    #[error("interpreter is not idle")]
    NotIdle,
    #[error("a program is running")]
    ProgramRunning,
    #[error("panel mode does not permit this action")]
    WrongPanelMode,
    #[error("unsupported command: {0}")]
    Unsupported(String),
    #[error("invalid command: {0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("HAL error: {0}")]
    Hal(String),
    #[error("tool change is not pending")]
    NoPendingToolChange,
    #[error("stale or invalid tool-change token")]
    StaleToolChangeToken,
    #[error("tool table revision conflict")]
    RevisionConflict,
    #[error("path is outside NC_FILES or is not a regular file")]
    UnsafePath,
}

impl From<std::io::Error> for MachineError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl MachineError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConnected => "NOT CONNECTED",
            Self::PanelOffline => "PANEL OFFLINE",
            Self::InvalidPanelMode => "INVALID MODE",
            Self::ProgramProtected => "PROTECT ON",
            Self::MachineOff => "MACHINE OFF",
            Self::NotHomed => "NOT HOMED",
            Self::NotIdle => "NOT IDLE",
            Self::ProgramRunning => "RUNNING",
            Self::WrongPanelMode => "WRONG MODE",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::Invalid(_) => "INVALID",
            Self::Io(_) => "I/O",
            Self::Backend(_) => "BACKEND",
            Self::Hal(_) => "HAL",
            Self::NoPendingToolChange => "NO PENDING CHANGE",
            Self::StaleToolChangeToken => "STALE TOKEN",
            Self::RevisionConflict => "REVISION CONFLICT",
            Self::UnsafePath => "UNSAFE PATH",
        }
    }
}

/// Result of a command, delivered asynchronously to the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub id: u64,
    pub command: String,
    pub result: Result<(), MachineError>,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BackendEvent {
    Command(CommandResult),
    Log(crate::model::Alarm),
    SnapshotLost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_declared_hal_outputs_are_representable() {
        assert_eq!(LincOutput::ALLOWED.len(), 5);
        assert!(LincOutput::from_name("linc.hmi-alive").is_some());
        assert!(LincOutput::from_name("linc.status-display").is_some());
        assert!(LincOutput::from_name("linc.tool-change-confirm").is_some());
        assert!(LincOutput::from_name("linc.jog-increment").is_some());
        assert!(LincOutput::from_name("linc.jog-speed").is_some());
        assert!(LincOutput::from_name("motion.spindle-on").is_none());
        assert!(LincOutput::from_name("cnc_panel.cycle_start").is_none());
    }

    #[test]
    fn hal_value_types_match_outputs() {
        assert!(HalValue::Bit(true).matches(LincOutput::HmiAlive));
        assert!(HalValue::Bit(true).matches(LincOutput::ToolChangeConfirm));
        assert!(HalValue::S32(54).matches(LincOutput::StatusDisplay));
        assert!(!HalValue::Float(1.0).matches(LincOutput::StatusDisplay));
    }
}
