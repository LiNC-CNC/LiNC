//! One bounded escape move off a tripped hard limit.
//!
//! The numbers and the refusal order live here so the dialog text, the two
//! backends and the tests cannot disagree. Nothing here touches a controller:
//! the caller supplies the snapshot it observed, and only the direction is
//! derived.
//!
//! The step is stated in millimetres because that is the unit an operator
//! reasons about. LinuxCNC's jog command takes machine units, so the plan
//! converts through the INI's `[TRAJ] LINEAR_UNITS`; an inch machine moves
//! 5 mm, not 5 in.

use serde::{Deserialize, Serialize};

use crate::command::MachineError;
use crate::model::{LimitSide, MachineSnapshot, PanelMode};

/// How far one recovery step travels.
pub const STEP_MM: f64 = 5.0;
/// Feed for a recovery step, millimetres per second.
pub const FEED_MM_S: f64 = 5.0;
/// Most steps one recovery attempt may issue.
pub const MAX_STEPS: u8 = 4;

/// The signed increment and feed for one step off `side`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecoveryPlan {
    pub side: LimitSide,
    /// Signed machine-unit increment: negative from a max limit.
    pub increment: f64,
    /// Machine units per second.
    pub feed: f64,
}

/// Machine units per millimetre, from the INI's linear-unit name.
pub fn units_per_mm(linear_units: &str) -> f64 {
    if linear_units.trim().to_ascii_lowercase().starts_with("in") {
        1.0 / 25.4
    } else {
        1.0
    }
}

/// The plan for one step away from `side`, in the machine's own units.
pub fn plan(side: LimitSide, linear_units: &str) -> RecoveryPlan {
    let scale = units_per_mm(linear_units);
    RecoveryPlan {
        side,
        increment: side.away() * STEP_MM * scale,
        feed: FEED_MM_S * scale,
    }
}

/// Refuse recovery unless the machine is in a state where one bounded step
/// away from `axis` is safe. Both backends call this; the UI calls it too so a
/// disabled RECOVER button can name the reason before a command is sent.
///
/// The order is the operator's order, not the code's: a fault they can see on
/// the panel first, then a mode they can change, then the state of the joint.
pub fn check_gates(snapshot: &MachineSnapshot, axis: &str) -> Result<LimitSide, MachineError> {
    if snapshot.cabinet_on == Some(false) {
        return Err(MachineError::CabinetOff);
    }
    if snapshot.machine.estop {
        return Err(MachineError::EmergencyStop);
    }
    if !snapshot.panel.connected || snapshot.panel.communication_error || snapshot.meta.stale {
        return Err(MachineError::PanelOffline);
    }
    if !snapshot.panel.mode.is_valid() {
        return Err(MachineError::InvalidPanelMode);
    }
    if !matches!(
        snapshot.panel.mode.mode(),
        Some(PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg)
    ) {
        return Err(MachineError::WrongPanelMode);
    }
    if snapshot.task.running {
        return Err(MachineError::ProgramRunning);
    }
    if snapshot.task.interp != crate::model::InterpState::Idle {
        return Err(MachineError::NotIdle);
    }
    if snapshot.spindles.iter().any(|spindle| spindle.on) {
        return Err(MachineError::SpindleRunning);
    }
    if snapshot.probe_trip_active() {
        return Err(MachineError::ProbeTripped);
    }
    if snapshot.machine.homing.iter().any(|(_, homing)| *homing) {
        return Err(MachineError::HomingInProgress);
    }
    if !matches!(
        snapshot.tool_change.status,
        crate::model::ToolChangeStatus::Idle | crate::model::ToolChangeStatus::Complete
    ) {
        return Err(MachineError::ToolChangePending);
    }
    let tripped = snapshot.tripped_axes();
    if tripped.len() > 1 {
        return Err(MachineError::MultipleLimitsTripped);
    }
    match tripped.first() {
        Some((name, side)) if name.eq_ignore_ascii_case(axis) => Ok(*side),
        _ => Err(MachineError::LimitNotTripped {
            axis: axis.to_ascii_uppercase(),
        }),
    }
}

/// The banner and dialog share one spelling of a tripped side.
pub fn side_label(side: LimitSide) -> &'static str {
    match side {
        LimitSide::Min => "MINUS",
        LimitSide::Max => "PLUS",
    }
}

/// The direction the axis can still move, worded for the dialog.
pub fn away_label(side: LimitSide) -> &'static str {
    match side {
        LimitSide::Min => "POSITIVE",
        LimitSide::Max => "NEGATIVE",
    }
}

/// A hard limit's user-facing sign, matching the status bar's existing spelling
/// (`X-` for the minimum hard limit, `X+` for the maximum).
pub fn hard_limit_label(axis: &str, side: LimitSide) -> String {
    match side {
        LimitSide::Min => format!("{axis}-"),
        LimitSide::Max => format!("{axis}+"),
    }
}

/// The banner text for one tripped axis. `None` when nothing is tripped.
pub fn banner_text(snapshot: &MachineSnapshot) -> Option<String> {
    let tripped = snapshot.tripped_axes();
    let (axis, side) = tripped.first()?;
    let limit = hard_limit_label(axis, *side);
    if tripped.len() > 1 {
        return Some(format!(
            "LIMIT SWITCHES — {} TRIPPED — MOTION DISABLED.",
            tripped
                .iter()
                .map(|(axis, side)| hard_limit_label(axis, *side))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let motion = if snapshot.machine.on {
        "MOTION ENABLED"
    } else {
        "MOTION DISABLED"
    };
    Some(format!("LIMIT SWITCH — {limit} — {motion}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LimitState, MachineVariant};

    /// A snapshot that may recover X's maximum limit: every gate passes.
    fn ready() -> MachineSnapshot {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.machine.estop = false;
        snapshot.panel.connected = true;
        snapshot.panel.communication_error = false;
        snapshot.panel.mode = crate::model::PanelModeValidity::Valid(PanelMode::Jog);
        snapshot.task.interp = crate::model::InterpState::Idle;
        snapshot.machine.limits.insert(
            "X",
            LimitState {
                max_hard: true,
                ..Default::default()
            },
        );
        snapshot
    }

    #[test]
    fn every_gate_refuses_recovery_with_its_own_reason() {
        use crate::model::{InterpState, ToolChangeStatus};

        assert_eq!(check_gates(&ready(), "X"), Ok(LimitSide::Max));

        let mut snapshot = ready();
        snapshot.cabinet_on = Some(false);
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::CabinetOff)
        ));

        let mut snapshot = ready();
        snapshot.machine.estop = true;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::EmergencyStop)
        ));

        let mut snapshot = ready();
        snapshot.panel.connected = false;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::PanelOffline)
        ));

        let mut snapshot = ready();
        snapshot.meta.stale = true;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::PanelOffline)
        ));

        let mut snapshot = ready();
        snapshot.panel.mode = crate::model::PanelModeValidity::Valid(crate::model::PanelMode::Auto);
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::WrongPanelMode)
        ));

        let mut snapshot = ready();
        snapshot.task.running = true;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::ProgramRunning)
        ));

        let mut snapshot = ready();
        snapshot.task.interp = InterpState::Reading;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::NotIdle)
        ));

        let mut snapshot = ready();
        snapshot.spindles.push(crate::model::SpindleState {
            index: 0,
            name: String::new(),
            commanded_speed: 0.0,
            actual_speed: 0.0,
            load: None,
            override_factor: 1.0,
            direction: 1,
            on: true,
            live_tool: false,
            live_tool_axial: false,
            live_tool_max_rpm: 0.0,
        });
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::SpindleRunning)
        ));

        let mut snapshot = ready();
        snapshot.machine.homing.insert("X", true);
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::HomingInProgress)
        ));

        let mut snapshot = ready();
        snapshot.tool_change.status = ToolChangeStatus::ManualPrompt;
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::ToolChangePending)
        ));

        let mut snapshot = ready();
        snapshot.machine.limits.insert(
            "Y",
            LimitState {
                min_hard: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            check_gates(&snapshot, "X"),
            Err(MachineError::MultipleLimitsTripped)
        ));

        let snapshot = ready();
        assert!(matches!(
            check_gates(&snapshot, "Y"),
            Err(MachineError::LimitNotTripped { ref axis }) if axis == "Y"
        ));
    }

    #[test]
    fn the_banner_names_every_tripped_hard_limit() {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.machine.limits.insert(
            "X",
            LimitState {
                max_hard: true,
                ..Default::default()
            },
        );
        assert_eq!(
            banner_text(&snapshot).as_deref(),
            Some("LIMIT SWITCH — X+ — MOTION DISABLED.")
        );
        snapshot.machine.on = true;
        assert_eq!(
            banner_text(&snapshot).as_deref(),
            Some("LIMIT SWITCH — X+ — MOTION ENABLED.")
        );
        snapshot.machine.limits.insert(
            "Z",
            LimitState {
                min_hard: true,
                ..Default::default()
            },
        );
        assert_eq!(
            banner_text(&snapshot).as_deref(),
            Some("LIMIT SWITCHES — X+ Z- TRIPPED — MOTION DISABLED.")
        );
        snapshot.machine.limits = crate::model::AxisMap::new();
        assert_eq!(banner_text(&snapshot), None);
    }

    #[test]
    fn a_max_limit_moves_negative_and_a_min_limit_moves_positive() {
        let max = plan(LimitSide::Max, "mm");
        let min = plan(LimitSide::Min, "mm");
        assert_eq!(max.increment, -STEP_MM);
        assert_eq!(min.increment, STEP_MM);
        assert_eq!(max.feed, FEED_MM_S);
        assert_eq!(min.feed, FEED_MM_S);
        assert_eq!(max.side, LimitSide::Max);
        assert_eq!(min.side, LimitSide::Min);
    }

    #[test]
    fn the_step_is_five_millimetres_in_inches_too() {
        let plan = plan(LimitSide::Max, "in");
        assert!((plan.increment + 5.0 / 25.4).abs() < 1e-12, "{plan:?}");
        assert!((plan.feed - 5.0 / 25.4).abs() < 1e-12, "{plan:?}");
        assert_eq!(units_per_mm("MM"), 1.0);
        assert_eq!(units_per_mm(" mm"), 1.0);
        assert_eq!(units_per_mm("INCH"), 1.0 / 25.4);
    }

    #[test]
    fn the_cap_is_four_steps_of_five_millimetres() {
        assert_eq!(MAX_STEPS, 4);
        assert_eq!(f64::from(MAX_STEPS) * STEP_MM, 20.0);
    }

    #[test]
    fn the_side_labels_are_the_two_the_dialog_uses() {
        assert_eq!(side_label(LimitSide::Min), "MINUS");
        assert_eq!(side_label(LimitSide::Max), "PLUS");
        assert_eq!(away_label(LimitSide::Min), "POSITIVE");
        assert_eq!(away_label(LimitSide::Max), "NEGATIVE");
        assert_eq!(hard_limit_label("X", LimitSide::Min), "X-");
        assert_eq!(hard_limit_label("X", LimitSide::Max), "X+");
    }
}
