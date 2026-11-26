//! The closed machine command set.
//!
//! Machine-control commands (cycle, jog, spindle start/stop, coolant,
//! overrides, E-stop) are intentionally not representable, with two bounded
//! exceptions: `RecoverLimit` issues a single increment away from a tripped
//! hard limit, and `StepSpindleSpeed` adds or subtracts one INI increment from
//! the speed of a spindle that is already turning. Neither can express a
//! distance, an absolute speed, a free jog or a spindle start.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    AxisMap, InterpState, MachinePower, MachineSnapshot, ProbeResult, ToolChangeToken, ToolRow,
};

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
    /// The manual screens' numeric spindle speed, when the spindle is stopped:
    /// a bare `S` block sets the interpreter's word, which is what the panel's
    /// own SPD keys start with. It is not MDI mode's word, so it is allowed
    /// with the panel in JOG, INC or MPG.
    SpindleSpeed,
}

impl MdiOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mdi => "MDI",
            Self::Probe => "PROBE",
            Self::ToolChange => "TOOL CHANGE",
            Self::ManualToolChange => "MANUAL CHANGE",
            Self::ProgHome => "PROG.HOME",
            Self::SpindleSpeed => "SPINDLE SPEED",
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
    /// Switch the machine off for a latched probe trip, for
    /// `halui.machine.off`. A level: true for as long as the trip is latched,
    /// false once it is reset. halui acts on the rising edge, so the held level
    /// is the whole response and the operator can power back on without being
    /// switched off again.
    MachineOff,
    /// The external link is down, settled and no client run holds a lease; the
    /// machine may move. The machine HAL ANDs this into cycle start, resume, jog
    /// enables and homing, because those reach halui from the panel and never
    /// pass through LiNC's command set. `true` when the profile does not manage
    /// the link, so a machine without `[network]` keeps moving.
    NetworkSafe,
    /// The buffer on screen is the program the interpreter holds, it holds no
    /// syntax fault, and it has not been edited since. The machine HAL ANDs this
    /// into cycle start, the way it ANDs `net-safe`: Low while a file is refused,
    /// dirty, or not the loaded one, and after an unload.
    ProgramOk,
}

impl LincOutput {
    pub const ALLOWED: [LincOutput; 8] = [
        LincOutput::HmiAlive,
        LincOutput::ToolChangeConfirm,
        LincOutput::StatusDisplay,
        LincOutput::JogIncrement,
        LincOutput::JogSpeed,
        LincOutput::MachineOff,
        LincOutput::NetworkSafe,
        LincOutput::ProgramOk,
    ];

    pub fn pin_name(self) -> &'static str {
        match self {
            Self::HmiAlive => "linc.hmi-alive",
            Self::ToolChangeConfirm => "linc.tool-change-confirm",
            Self::StatusDisplay => "linc.status-display",
            Self::JogIncrement => "linc.jog-increment",
            Self::JogSpeed => "linc.jog-speed",
            Self::MachineOff => "linc.machine-off",
            Self::NetworkSafe => "linc.net-safe",
            Self::ProgramOk => "linc.program-ok",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::HmiAlive => "hmi-alive",
            Self::ToolChangeConfirm => "tool-change-confirm",
            Self::StatusDisplay => "status-display",
            Self::JogIncrement => "jog-increment",
            Self::JogSpeed => "jog-speed",
            Self::MachineOff => "machine-off",
            Self::NetworkSafe => "net-safe",
            Self::ProgramOk => "program-ok",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "hmi-alive" | "linc.hmi-alive" => Some(Self::HmiAlive),
            "tool-change-confirm" | "linc.tool-change-confirm" => Some(Self::ToolChangeConfirm),
            "status-display" | "linc.status-display" => Some(Self::StatusDisplay),
            "jog-increment" | "linc.jog-increment" => Some(Self::JogIncrement),
            "jog-speed" | "linc.jog-speed" => Some(Self::JogSpeed),
            "machine-off" | "linc.machine-off" => Some(Self::MachineOff),
            "net-safe" | "linc.net-safe" => Some(Self::NetworkSafe),
            "program-ok" | "linc.program-ok" => Some(Self::ProgramOk),
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
    /// Which value type each output accepts. This is a `matches!` list rather
    /// than three `match output` arms, so a new output that is missing here
    /// compiles and then fails at runtime with `does not accept` - the reason
    /// `every_declared_output_accepts_a_value_type` exists below.
    pub fn matches(self, output: LincOutput) -> bool {
        matches!(
            (self, output),
            (HalValue::Bit(_), LincOutput::HmiAlive)
                | (HalValue::Bit(_), LincOutput::ToolChangeConfirm)
                | (HalValue::S32(_), LincOutput::StatusDisplay)
                | (HalValue::Float(_), LincOutput::JogIncrement)
                | (HalValue::Float(_), LincOutput::JogSpeed)
                | (HalValue::Bit(_), LincOutput::MachineOff)
                | (HalValue::Bit(_), LincOutput::NetworkSafe)
                | (HalValue::Bit(_), LincOutput::ProgramOk)
        )
    }
}

/// Which way one spindle speed step goes.
///
/// A direction, not an amount: LinuxCNC adds or subtracts
/// `[SPINDLE_n]INCREMENT` itself, so the HMI cannot name a speed, cross zero or
/// move the spindle in one step it did not take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpindleStep {
    Slower,
    Faster,
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
    /// Load a tool and apply its length compensation in one go: `Tn M6` then
    /// `G43 Hn`. The TOOLS page's `T` shortcut uses this instead of a bare tool
    /// change, so the new height offset is in effect immediately.
    LoadTool {
        tool: u16,
    },
    ConfirmManualToolChange {
        token: ToolChangeToken,
    },
    SetCurrentTool {
        tool: u16,
    },
    /// Arm the controller's limits override for `axis`. LinuxCNC masks every
    /// limit that is tripped *at this moment* and clears the mask when the jog
    /// it allows finishes; there is no disarm command, and the old name
    /// `toggle` never described that. Only the recovery dialog calls it.
    ArmLimitsOverride {
        axis: String,
    },
    /// One bounded step off a tripped hard limit. The backend re-reads the
    /// controller, derives the side, the sign and the jog mode itself, and
    /// refuses unless `axis` is the only tripped axis.
    RecoverLimit {
        axis: String,
    },
    /// One step on the programmed speed of the spindle that is already turning:
    /// the same action as Axis's `-`/`+`. LinuxCNC adds or subtracts
    /// `[SPINDLE_n]INCREMENT` (100 rpm unless the INI says otherwise) to the
    /// motion controller's speed, keeps the sign, and clamps to the spindle's
    /// `MAX_FORWARD_VELOCITY`/`MAX_REVERSE_VELOCITY`.
    ///
    /// This is not spindle override: the override factor and the panel knob are
    /// untouched. It also does not rewrite the interpreter's S word, so the next
    /// `S` in a running program puts the programmed speed back.
    StepSpindleSpeed {
        step: SpindleStep,
    },
    /// Put the spindle at `rpm`. A running spindle changes speed under it; a
    /// stopped one gets the interpreter's S word, so the panel's next start
    /// runs at `rpm` and nothing turns now. Bounded by
    /// `[DISPLAY]MAX_SPINDLE_SPEED` and the machine's own spindle limits.
    ///
    /// The running path sends `EMC_SPINDLE_ON` with the direction the operator
    /// can see, because `EMC_SPINDLE_SPEED` takes its sign from the
    /// interpreter's remembered direction and a panel start never sets that.
    /// The stopped path sets the interpreter's word, which is the value the
    /// panel starts with. The override is untouched on both paths, and the next
    /// `S` in a running program still wins.
    SetSpindleSpeed {
        rpm: f64,
    },
    LoadProgram {
        path: PathBuf,
        expected_revision: Option<u64>,
    },
    /// Unload the program: the controller closes it (`EMC_TASK_PLAN_CLOSE`) and
    /// starts holding nothing, so AUTO has nothing to run.
    ///
    /// This is what disarms the external-link interlock: with a program loaded
    /// the link stays down (armed), and unloading is the operator's deliberate
    /// "done cutting" signal that lets it come back up.
    UnloadProgram,
    /// Write `content` to `path` and make that file the loaded program.
    ///
    /// The write alone is not enough: LinuxCNC keeps the program it opened in
    /// the interpreter, so `AUTO` would still start the text loaded before the
    /// edit. The backend reopens the file after the write; the simulator
    /// replaces its program state the same way.
    SaveProgram {
        path: PathBuf,
        expected_revision: u64,
        content: String,
    },
    /// Write `content` to `path` and change nothing else.
    ///
    /// This is the save a prepared document needs: the controller does not run
    /// that file, so the write neither opens the program nor asks for the
    /// panel's EDIT position. It still needs a live panel, a valid panel mode
    /// and protect known off, and it refuses the loaded program's own path.
    SaveProgramFile {
        path: PathBuf,
        content: String,
    },
    /// Write `content` to `path`, then open that file as the loaded program.
    ///
    /// This is the handoff: the prepared buffer becomes the controller's
    /// program in one press. The write happens first so the text the operator
    /// checked is the text that opens, and the open runs the same sequence a
    /// load does, capturing the live offsets before the interpreter resets.
    /// It needs the prepared write's gate plus an interpreter that is idle and
    /// not running; it never starts, pauses or resumes the machine.
    LoadPreparedProgram {
        path: PathBuf,
        content: String,
    },
    SetWorkOffset {
        index: u8,
        values: AxisMap<f64>,
        rotation: Option<f64>,
    },
    /// Make work system `index` (`G54`..`G59.3`) the active one. This selects
    /// the coordinate system; it does not write the stored row. The OFFSETS
    /// page's `A` key is the only caller.
    ActivateWorkOffset {
        index: u8,
    },
    /// Set the global EXT shift: each named axis's shift becomes that axis's
    /// value in `values`, in machine units.
    ///
    /// `G92` does not assign the register; it makes the current point read a
    /// coordinate, and the interpreter derives the register from that. So each
    /// backend computes the word from the position and the register it reports
    /// and re-reads the register afterwards. The work position alone is wrong:
    /// it already has the old register in it, and a second edit would land on
    /// the sum of the two.
    SetG92Offset {
        values: AxisMap<f64>,
    },
    /// Clear the global EXT shift with `G92.1`, every axis at once. Its own
    /// command rather than an MDI line, so it passes the offset write gate.
    ClearG92Offset,
    /// Touch off: make the current position read zero in work offset `index`
    /// for `axes`. Issues `G10 L20`, so LinuxCNC computes the offset itself
    /// rather than the HMI deriving it from a possibly stale snapshot.
    ZeroWorkOffset {
        index: u8,
        axes: Vec<String>,
    },
    /// Set the current position to read `values` in work offset `index`:
    /// `G10 L20` with those coordinates, so the controller computes the offset
    /// from the position it holds instead of the HMI deriving it.
    SetWorkPosition {
        index: u8,
        values: AxisMap<f64>,
    },
    SetToolTableRow {
        row: ToolRow,
        /// The row being renumbered, when `row.tool` differs.
        old_tool: Option<u16>,
        expected_revision: u64,
    },
    DeleteToolTableRow {
        tool: u16,
        expected_revision: u64,
    },
    /// Ask an optional external tool-table service to synchronize now. The
    /// Loobric backend wrapper intercepts this; a backend without the service
    /// refuses it.
    SyncToolTable,
    /// Reload the tool table file into the backend and controller. This is the
    /// follow-up to an external sync that pulled changes; the UI never sends
    /// it directly.
    ReloadToolTable,
    ApplyProbeResult {
        result: ProbeResult,
        target: ProbeTarget,
    },
    ResetProbeCalibration,
    SetHalOutput {
        output: LincOutput,
        value: HalValue,
    },
    /// One press of the panel's CYCLE START key, decided by the HMI.
    ///
    /// Sent to the controller as `EMC_TASK_PLAN_RUN` (or `EMC_TASK_PLAN_RESUME`
    /// for a paused program), not through a HAL pin: `halui` fires
    /// `halui.program.run` on *either* edge of its pin, so a pulse from the HMI
    /// produced two run commands per press - the second refused, and after a
    /// reset the refusals are what the operator saw as a dead key.
    CycleStart,
    /// Start the loaded program at `line`, skipping everything before it.
    ///
    /// A line number cannot come from the panel. It still requires the panel to
    /// be in AUTO, so the panel remains the authority over whether the machine
    /// may run.
    RunFromLine {
        line: u32,
    },
    /// Stop and reset the running program: the same action as the panel's K4
    /// (`halui.program.stop`). It never starts motion.
    AbortProgram,
    /// View-only: clear the live tool trail and re-seed at the current tip.
    /// This never changes machine state or the loaded program.
    ClearTrail,
}

/// The result label of a prepared-file write. The shell matches result events on
/// it, so the string lives here beside the label that sends it.
pub const PREPARED_SAVE_LABEL: &str = "Save prepared program";
/// The result label of the prepared handoff, for the same reason.
pub const PREPARED_LOAD_LABEL: &str = "Load prepared program";

impl MachineCommand {
    pub fn label(&self) -> &'static str {
        match self {
            Self::IssueMdi { .. } => "Issue MDI",
            Self::IssueToolChange { .. } => "Tool change",
            Self::LoadTool { .. } => "Load tool with height compensation",
            Self::ConfirmManualToolChange { .. } => "Confirm manual tool change",
            Self::SetCurrentTool { .. } => "Set current tool",
            Self::ArmLimitsOverride { .. } => "Arm limits override",
            Self::RecoverLimit { .. } => "Recover limit",
            Self::StepSpindleSpeed { .. } => "Step spindle speed",
            Self::SetSpindleSpeed { .. } => "Set spindle speed",
            Self::LoadProgram { .. } => "Load program",
            Self::UnloadProgram => "Unload program",
            Self::SaveProgram { .. } => "Save program",
            Self::SaveProgramFile { .. } => PREPARED_SAVE_LABEL,
            Self::LoadPreparedProgram { .. } => PREPARED_LOAD_LABEL,
            Self::SetWorkOffset { .. } => "Set work offset",
            Self::ActivateWorkOffset { .. } => "Activate work offset",
            Self::SetG92Offset { .. } => "Set G92 offset",
            Self::ClearG92Offset => "Clear G92 offset",
            Self::ZeroWorkOffset { .. } => "Zero work offset",
            Self::SetWorkPosition { .. } => "Set work position",
            Self::RunFromLine { .. } => "Run from line",
            Self::CycleStart => "Cycle start",
            Self::SetToolTableRow { .. } => "Set tool table row",
            Self::DeleteToolTableRow { .. } => "Delete tool table row",
            Self::SyncToolTable => "Sync tool table",
            Self::ReloadToolTable => "Reload tool table",
            Self::ApplyProbeResult { .. } => "Apply probe result",
            Self::ResetProbeCalibration => "Reset probe calibration",
            Self::SetHalOutput { .. } => "Set HAL output",
            Self::AbortProgram => "Abort program",
            Self::ClearTrail => "Clear live trail",
        }
    }
}

/// The panel and power gates every spindle command shares.
///
/// The panel stays the authority: without it the HMI cannot see the mode the
/// operator selected, and a stale panel is the same as no panel.
fn check_spindle_panel(snapshot: &MachineSnapshot) -> Result<(), MachineError> {
    if !snapshot.panel.connected || snapshot.panel.communication_error {
        return Err(MachineError::PanelOffline);
    }
    if !snapshot.panel.mode.is_valid() {
        return Err(MachineError::InvalidPanelMode);
    }
    if snapshot.task.power != MachinePower::On {
        return Err(MachineError::MachineOff);
    }
    Ok(())
}

/// The spindle a trim command may act on.
///
/// It must be turning because `EMC_SPINDLE_INCREASE` and
/// `EMC_SPINDLE_DECREASE` skip a stopped spindle without an error, and a key
/// that does nothing is worse than one that says why.
fn spindle_for_trim(
    snapshot: &MachineSnapshot,
) -> Result<&crate::model::SpindleState, MachineError> {
    check_spindle_panel(snapshot)?;
    let Some(spindle) = snapshot.spindles.first() else {
        return Err(MachineError::SpindleStopped);
    };
    if !spindle.on || spindle.commanded_speed == 0.0 || spindle.direction == 0 {
        return Err(MachineError::SpindleStopped);
    }
    Ok(spindle)
}

/// Rules for writing a program file without opening it, shared by the
/// simulator and the machine backend so the two cannot drift.
///
/// This is the save policy minus the EDIT-position and idle conditions: a
/// prepared document is written while the machine runs and in any valid panel
/// mode. The panel still has to be live and its mode valid, and protect has to
/// be known off - an unknown protect state counts as on.
///
/// While the task runs, a write under one of the machine's `SUBROUTINE_PATH`
/// directories is refused: the interpreter reopens a subroutine file at the
/// moment an `o<name> call` executes, so replacing one would change the text
/// AUTO runs without touching the loaded program's own file.
pub fn check_program_file_write(
    snapshot: &MachineSnapshot,
    target: &Path,
    subroutine_paths: &[PathBuf],
) -> Result<(), MachineError> {
    check_program_file_panel(snapshot)?;
    if snapshot.task.running && path_is_under_any(target, subroutine_paths) {
        return Err(MachineError::SubroutineInUse);
    }
    Ok(())
}

fn check_program_file_panel(snapshot: &MachineSnapshot) -> Result<(), MachineError> {
    if !snapshot.panel.connected || snapshot.panel.communication_error {
        return Err(MachineError::PanelOffline);
    }
    if !snapshot.panel.mode.is_valid() {
        return Err(MachineError::InvalidPanelMode);
    }
    if snapshot.panel.protect_effective() {
        return Err(MachineError::ProgramProtected);
    }
    Ok(())
}

/// Rules for handing the prepared program to the controller: the prepared
/// write's gate plus an interpreter that is idle and not running.
pub fn check_prepared_handoff(snapshot: &MachineSnapshot) -> Result<(), MachineError> {
    check_program_file_panel(snapshot)?;
    if snapshot.task.running {
        return Err(MachineError::ProgramRunning);
    }
    if snapshot.task.interp != InterpState::Idle {
        return Err(MachineError::NotIdle);
    }
    Ok(())
}

/// Whether `target` resolves under one of the configured subroutine
/// directories. A directory that does not resolve is compared as written; a
/// target that does not exist is resolved through its parent, so the check
/// works before the file is created.
fn path_is_under_any(target: &Path, dirs: &[PathBuf]) -> bool {
    let Some(target) = resolved_for_compare(target) else {
        return false;
    };
    dirs.iter().any(|dir| {
        let dir = resolved_for_compare(dir).unwrap_or_else(|| dir.clone());
        target.starts_with(&dir)
    })
}

/// Replace `canonical` with `content` in one step: a sibling temporary file,
/// flushed to disk and renamed over the target, so an interrupted write leaves
/// the old program whole.
///
/// Both backends write program files through this, so the durability rules
/// cannot drift between the simulator and the machine.
pub fn write_program_file(canonical: &Path, content: &str) -> Result<(), MachineError> {
    let parent = canonical.parent().ok_or(MachineError::UnsafePath)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write as _;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(canonical)
        .map_err(|error| MachineError::Io(error.error.to_string()))?;
    Ok(())
}

/// Refuse a prepared-file write that would land on the program the controller
/// holds.
///
/// Both sides are resolved through the filesystem, so a different spelling of
/// the loaded path - a symlinked name, or a path carrying a `..` parent
/// component - is refused rather than the running program's file rewritten.
pub fn check_write_target_not_loaded(
    snapshot: &MachineSnapshot,
    target: &Path,
) -> Result<(), MachineError> {
    let Some(loaded) = snapshot.program.path.as_deref() else {
        return Ok(());
    };
    if same_program_file(target, loaded) {
        return Err(MachineError::LoadedProgramPath);
    }
    Ok(())
}

/// Whether two paths name the same program file.
///
/// Both sides go through the filesystem first, so a symlinked name or a path
/// carrying a `..` parent component compares as the file it resolves to. A
/// file the controller holds can be gone from disk while the interpreter still
/// runs it, so an unresolvable pair compares through its lexical absolute
/// form instead of passing.
pub fn same_program_file(first: &Path, second: &Path) -> bool {
    match (resolved_for_compare(first), resolved_for_compare(second)) {
        (Some(first), Some(second)) => first == second,
        (first, second) => match (
            lexical_absolute(first.as_deref()),
            lexical_absolute(second.as_deref()),
        ) {
            (Some(first), Some(second)) => first == second,
            _ => false,
        },
    }
}

/// Resolve a path for comparison: the file itself when it exists, otherwise its
/// parent plus its name, so a target that has not been written yet still
/// compares against the directory it will land in.
fn resolved_for_compare(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    std::fs::canonicalize(parent)
        .ok()
        .map(|parent| parent.join(name))
}

/// The path with `.` and `..` removed against the current directory, for the
/// comparison when neither side resolves on disk.
fn lexical_absolute(path: Option<&Path>) -> Option<PathBuf> {
    let path = path?;
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().ok()?
    };
    let mut out = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// Rules for a spindle speed step, shared by the simulator and the machine
/// backend so the two cannot drift.
///
/// `increment` is the INI's `[SPINDLE_n]INCREMENT`; the step itself is applied
/// by LinuxCNC, so the value is used only to know where the step would land.
/// `ceiling` is `[DISPLAY]MAX_SPINDLE_SPEED`, and `0.0` means the profile names
/// none.
pub fn check_spindle_step(
    snapshot: &MachineSnapshot,
    step: SpindleStep,
    increment: f64,
    ceiling: f64,
) -> Result<(), MachineError> {
    let spindle = spindle_for_trim(snapshot)?;
    // Reverse reads as a negative speed, so the ceiling is on the magnitude:
    // `Faster` always means further from zero, whichever way the spindle turns.
    if step == SpindleStep::Faster
        && ceiling > 0.0
        && spindle.commanded_speed.abs() + increment > ceiling
    {
        return Err(MachineError::SpindleSpeedLimit);
    }
    Ok(())
}

/// Rules for an absolute spindle speed, shared the same way.
///
/// Unlike a step, this does not need the spindle to be turning. With it
/// stopped, the number becomes the interpreter's S word and the panel's next
/// start runs at it; nothing turns now. The interpreter has to be idle because
/// the stopped path runs a `S` block, and a running program owns that word.
pub fn check_spindle_speed(
    snapshot: &MachineSnapshot,
    rpm: f64,
    ceiling: f64,
) -> Result<(), MachineError> {
    check_spindle_panel(snapshot)?;
    if snapshot.probe_trip_active() {
        return Err(MachineError::ProbeTripped);
    }
    if snapshot.task.interp != InterpState::Idle || snapshot.task.running {
        return Err(MachineError::NotIdle);
    }
    if snapshot.program.running {
        return Err(MachineError::ProgramRunning);
    }
    if !rpm.is_finite() || rpm <= 0.0 {
        return Err(MachineError::Invalid(
            "spindle speed must be greater than zero".into(),
        ));
    }
    if ceiling > 0.0 && rpm > ceiling {
        return Err(MachineError::SpindleSpeedLimit);
    }
    Ok(())
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
    #[error("probe tripped; reset the controller first")]
    ProbeTripped,
    #[error("cabinet is off")]
    CabinetOff,
    #[error("emergency stop is active")]
    EmergencyStop,
    #[error("spindle is running")]
    SpindleRunning,
    #[error("spindle is not turning")]
    SpindleStopped,
    #[error("spindle speed is at its maximum")]
    SpindleSpeedLimit,
    #[error("homing is in progress")]
    HomingInProgress,
    #[error("a tool change is pending")]
    ToolChangePending,
    #[error("{axis} is not on a limit switch")]
    LimitNotTripped { axis: String },
    #[error("more than one axis is on a limit switch")]
    MultipleLimitsTripped,
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
    #[error("external link is up for a sync; motion waits for it to be down")]
    NetworkBusy,
    #[error("path is outside NC_FILES or is not a regular file")]
    UnsafePath,
    #[error("the write target is a subroutine the running program may call")]
    SubroutineInUse,
    #[error("the write target is the program the controller has loaded")]
    LoadedProgramPath,
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
            Self::ProbeTripped => "PROBE TRIPPED",
            Self::CabinetOff => "CABINET OFF",
            Self::EmergencyStop => "E-STOP ACTIVE",
            Self::SpindleRunning => "SPINDLE RUNNING",
            Self::SpindleStopped => "SPINDLE STOPPED",
            Self::SpindleSpeedLimit => "SPINDLE AT LIMIT",
            Self::HomingInProgress => "HOMING",
            Self::ToolChangePending => "TOOL CHANGE",
            Self::LimitNotTripped { .. } => "NOT ON A LIMIT",
            Self::MultipleLimitsTripped => "MULTIPLE LIMITS",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::Invalid(_) => "INVALID",
            Self::Io(_) => "I/O",
            Self::Backend(_) => "BACKEND",
            Self::Hal(_) => "HAL",
            Self::NoPendingToolChange => "NO PENDING CHANGE",
            Self::StaleToolChangeToken => "STALE TOKEN",
            Self::RevisionConflict => "REVISION CONFLICT",
            Self::NetworkBusy => "NETWORK BUSY",
            Self::UnsafePath => "UNSAFE PATH",
            Self::SubroutineInUse => "SUBROUTINE IN USE",
            Self::LoadedProgramPath => "LOADED PROGRAM",
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

/// Does this MDI block ask the spindle to turn?
///
/// `M3`, `M4` and `M19` start it: forward, reverse and orient. `M5` stops it and
/// `S` only sets the commanded speed, so neither counts as a start and both stay
/// allowed during a tool change — the operator may be recovering from a spindle
/// someone left running. Parenthesised and `;` comments are skipped, and the
/// integer part of the word is what counts, so `M3.1` (start, then wait for
/// at-speed) starts it too.
pub fn starts_spindle(block: &str) -> bool {
    let mut code = String::with_capacity(block.len());
    let mut depth = 0usize;
    for character in block.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => code.push(character),
            _ => {}
        }
    }
    let code = code.split(';').next().unwrap_or(&code);
    code.split_whitespace()
        .filter_map(|word| {
            let digits = word.strip_prefix('M').or_else(|| word.strip_prefix('m'))?;
            digits
                .split('.')
                .next()
                .unwrap_or(digits)
                .parse::<u32>()
                .ok()
        })
        .any(|number| matches!(number, 3 | 4 | 19))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numeric entry takes a speed from a stopped spindle, because the
    /// number becomes the S word the panel starts with. A step on the same
    /// spindle is refused: LinuxCNC skips it at zero speed.
    #[test]
    fn a_stopped_spindle_takes_a_speed_but_not_a_step() {
        use crate::model::{MachineVariant, PanelMode, PanelModeValidity, SpindleState};
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel.connected = true;
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Jog);
        snapshot.task.power = MachinePower::On;
        snapshot.task.interp = InterpState::Idle;
        snapshot.spindles.push(SpindleState {
            index: 0,
            name: "MAIN SPINDLE".into(),
            commanded_speed: 0.0,
            actual_speed: 0.0,
            load: None,
            override_factor: 1.0,
            direction: 0,
            on: false,
            live_tool: false,
            live_tool_axial: false,
            live_tool_max_rpm: 0.0,
        });

        assert_eq!(check_spindle_speed(&snapshot, 1200.0, 0.0), Ok(()));
        assert_eq!(
            check_spindle_step(&snapshot, SpindleStep::Faster, 100.0, 0.0),
            Err(MachineError::SpindleStopped),
            "a step still needs a turning spindle"
        );
        assert_eq!(
            check_spindle_speed(&snapshot, 1300.0, 1200.0),
            Err(MachineError::SpindleSpeedLimit)
        );
        assert!(matches!(
            check_spindle_speed(&snapshot, 0.0, 0.0),
            Err(MachineError::Invalid(_))
        ));

        // The stopped path runs a `S` block, so a busy interpreter refuses it.
        snapshot.task.interp = InterpState::Reading;
        assert_eq!(
            check_spindle_speed(&snapshot, 1200.0, 0.0),
            Err(MachineError::NotIdle)
        );
    }

    #[test]
    fn a_prepared_file_write_needs_a_live_panel_and_protect_off() {
        use crate::model::{MachineVariant, PanelMode, PanelModeValidity};
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel.connected = true;
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Auto);
        snapshot.panel.program_protect = false;
        snapshot.panel.program_protect_known = true;

        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Ok(())
        );

        snapshot.panel.connected = false;
        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Err(MachineError::PanelOffline)
        );
        snapshot.panel.connected = true;
        snapshot.panel.communication_error = true;
        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Err(MachineError::PanelOffline),
            "a stale panel is the same as no panel"
        );
        snapshot.panel.communication_error = false;

        snapshot.panel.mode = PanelModeValidity::Unknown;
        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Err(MachineError::InvalidPanelMode)
        );
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Auto);

        snapshot.panel.program_protect = true;
        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Err(MachineError::ProgramProtected)
        );
        snapshot.panel.program_protect = false;
        snapshot.panel.program_protect_known = false;
        assert_eq!(
            check_program_file_write(&snapshot, Path::new("prep.ngc"), &[]),
            Err(MachineError::ProgramProtected),
            "an unknown protect state counts as on"
        );
    }

    #[test]
    fn a_prepared_write_keeps_off_a_subroutine_the_run_may_call() {
        use crate::model::{MachineVariant, PanelMode, PanelModeValidity};
        let dir = tempfile::tempdir().unwrap();
        let subs = dir.path().join("subs");
        std::fs::create_dir_all(&subs).unwrap();
        let target = subs.join("probe.ngc");
        let outside = dir.path().join("part.ngc");
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel.connected = true;
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Auto);
        snapshot.panel.program_protect = false;
        snapshot.panel.program_protect_known = true;

        // Idle: the interpreter is not holding a call open, so the write is
        // allowed even under SUBROUTINE_PATH.
        assert_eq!(
            check_program_file_write(&snapshot, &target, std::slice::from_ref(&subs)),
            Ok(())
        );
        snapshot.task.running = true;
        assert_eq!(
            check_program_file_write(&snapshot, &target, std::slice::from_ref(&subs)),
            Err(MachineError::SubroutineInUse)
        );
        assert_eq!(
            check_program_file_write(&snapshot, &outside, std::slice::from_ref(&subs)),
            Ok(()),
            "a part file outside the subroutine dirs is still writable mid-run"
        );
    }

    #[test]
    fn a_spindle_start_is_recognised_and_a_stop_is_not() {
        for block in ["M3", "M4 S1000", "G0 X0 M3", "M19 R0", "M3.1", "m4"] {
            assert!(starts_spindle(block), "{block} starts the spindle");
        }
        for block in [
            "M5",
            "G97 S300 M5",
            "M30",
            "G0 X0",
            "S1000",
            "(M3) G0 X0",
            "G0 X0 ; M3",
            "(nested (M4) comment) M5",
        ] {
            assert!(!starts_spindle(block), "{block} does not start it");
        }
    }

    #[test]
    fn only_declared_hal_outputs_are_representable() {
        assert_eq!(LincOutput::ALLOWED.len(), 8);
        assert!(LincOutput::from_name("linc.hmi-alive").is_some());
        assert!(LincOutput::from_name("linc.status-display").is_some());
        assert!(LincOutput::from_name("linc.tool-change-confirm").is_some());
        assert!(LincOutput::from_name("linc.jog-increment").is_some());
        assert!(LincOutput::from_name("linc.jog-speed").is_some());
        assert_eq!(
            LincOutput::from_name("linc.machine-off"),
            Some(LincOutput::MachineOff)
        );
        assert_eq!(
            LincOutput::from_name("linc.net-safe"),
            Some(LincOutput::NetworkSafe),
            "the interlock is a declared output, not a generic write"
        );
        assert!(LincOutput::from_name("motion.spindle-on").is_none());
        assert!(LincOutput::from_name("cnc_panel.cycle_start").is_none());
    }

    #[test]
    fn hal_value_types_match_outputs() {
        assert!(HalValue::Bit(true).matches(LincOutput::HmiAlive));
        assert!(HalValue::Bit(true).matches(LincOutput::ToolChangeConfirm));
        assert!(HalValue::S32(54).matches(LincOutput::StatusDisplay));
        assert!(!HalValue::Float(1.0).matches(LincOutput::StatusDisplay));
        // The interlock is a bit. Added after a deployed build rejected it with
        // `linc.net-safe does not accept Bit(true)` and left the gate shut.
        assert!(HalValue::Bit(true).matches(LincOutput::NetworkSafe));
        assert!(!HalValue::S32(1).matches(LincOutput::NetworkSafe));
    }

    /// Every declared output must accept some value type. A new variant that is
    /// missing from `HalValue::matches` compiles (that list is a `matches!`
    /// macro, not an exhaustive match) and only fails at runtime, which is how
    /// `net-safe` reached a machine without a working writer.
    #[test]
    fn every_declared_output_accepts_a_value_type() {
        for output in LincOutput::ALLOWED {
            let accepted = HalValue::Bit(false).matches(output)
                || HalValue::S32(0).matches(output)
                || HalValue::Float(0.0).matches(output);
            assert!(accepted, "{output:?} accepts no value type");
        }
    }

    /// The probe trip's machine-off response is a HAL bit under its own name.
    #[test]
    fn machine_off_round_trips_through_every_name_and_takes_a_bit() {
        let output = LincOutput::MachineOff;
        assert_eq!(output.pin_name(), "linc.machine-off");
        assert_eq!(output.short_name(), "machine-off");
        assert_eq!(LincOutput::from_name(output.pin_name()), Some(output));
        assert_eq!(LincOutput::from_name(output.short_name()), Some(output));
        assert!(HalValue::Bit(true).matches(output));
        assert!(HalValue::Bit(false).matches(output));
        assert!(!HalValue::S32(1).matches(output));
        assert!(!HalValue::Float(1.0).matches(output));
        assert!(LincOutput::ALLOWED.contains(&output));
    }
}
