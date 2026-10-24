//! Touch Probe screen following Probe Basic's category-first workflow.

use egui::Ui;
use linc_core::command::{MachineCommand, MdiOrigin, ProbeTarget};
use linc_core::probe::{
    probe_write_action, ProbeMode, ProbeOperation, ProbeParameters, ProbeWriteAction,
};

use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    super::probe_view::show(app, ui);
}

pub fn enter_first(app: &mut LincApp) {
    super::probe_view::enter_first(app);
}

/// Every entry into the page starts with no routine selected and no editor
/// open: Enter must follow a fresh number selection.
pub fn clear_selection(app: &mut LincApp) {
    super::probe_view::clear_selection(app);
}

pub fn category_step(app: &mut LincApp, forward: bool) {
    super::probe_view::category_step(app, forward);
}

/// Build the parameter block for one issued routine without changing the
/// operator's saved mode. Calibration is always measurement-only in this UI;
/// its result is stored by the calibration routine, never by generic Apply.
fn parameters_for_run(params: &ProbeParameters, operation: &ProbeOperation) -> ProbeParameters {
    let mut issued = params.clone();
    if operation.calibration {
        issued.probe_mode = ProbeMode::MeasureOnly;
        issued.wcs_rotation = false;
    }
    issued
}

/// The write gate for one intentional controller write. The action comes from
/// `linc_core::probe::probe_write_action`, the classifier both backends gate
/// on, so the UI cannot protect a block that a backend allows (or the reverse).
/// PROTECT is checked first: it is the cause named on the banner, and the
/// operator has to see it even when the machine is also not ready.
fn write_block_reason(action: ProbeWriteAction, app: &LincApp) -> Option<String> {
    let (protect, not_ready) = match action {
        ProbeWriteAction::DirectWco => (
            "PROTECT ON — direct WCO blocked",
            "direct WCO requires machine ON, a valid panel and an idle interpreter",
        ),
        ProbeWriteAction::EdgeAngleRotation => (
            "PROTECT ON — edge-angle R0 reset blocked even in POSITION mode",
            "edge-angle R0 reset requires machine ON, a valid panel and an idle interpreter",
        ),
        // Names every parameter a calibration routine writes: the cal_* family
        // rewrites #3032, and probe_spindle_nose.ngc:65-68 also writes the
        // spindle reference parameters, so one wording has to cover both.
        ProbeWriteAction::CalibrationRun => (
            "PROTECT ON — calibration Run writes controller #3032 and the spindle reference parameters #3007/#3010",
            "calibration requires machine ON, a valid panel and an idle interpreter",
        ),
        ProbeWriteAction::CalibrationReset => (
            "PROTECT ON — calibration reset blocked",
            "calibration reset requires machine ON, a valid panel, an idle interpreter and PROTECT off",
        ),
        ProbeWriteAction::ParameterUpdate | ProbeWriteAction::ResultReset => (
            "PROTECT ON — probe parameter and result writes are blocked",
            "probe parameter writes require machine ON, a valid panel and an idle interpreter",
        ),
        // A routine call with no typed parameter block cannot be proven to be a
        // measurement, so it never reaches the controller.
        ProbeWriteAction::UnknownRun => {
            return Some(
                "probe run cannot be classified as a measurement; it is refused".into(),
            )
        }
    };
    if app.snapshot.panel.protect_effective() {
        return Some(protect.into());
    }
    if !app.snapshot.writes_allowed() {
        return Some(not_ready.into());
    }
    None
}

/// The reason one Probe MDI block may not be sent, or `None` when the shared
/// classifier finds no controller write in it. Every explicit Probe MDI action
/// (parameter update, both data resets) goes through this.
pub(crate) fn mdi_write_block_reason(app: &LincApp, block: &str) -> Option<String> {
    probe_write_action(block).and_then(|action| write_block_reason(action, app))
}

/// A calibration reset is issued as its own command rather than as MDI, so it
/// names its action directly; the gate is the same one.
pub(crate) fn calibration_reset_block_reason(app: &LincApp) -> Option<String> {
    write_block_reason(ProbeWriteAction::CalibrationReset, app)
}

/// Preconditions of a Run that are not about what it writes: DIRECT needs an
/// action to apply, and both DIRECT and Edge Angle need an active work offset.
fn run_precondition_reason(
    operation: &ProbeOperation,
    mode: ProbeMode,
    active_wcs: Option<u8>,
) -> Option<String> {
    let direct = mode == ProbeMode::ApplyWcs;
    let angle_reset = operation.category == linc_core::probe::ProbeCategory::EdgeAngle;
    if direct && operation.apply_terms().is_empty() {
        return Some(format!("{} has no direct-WCO action", operation.name));
    }
    let active_wcs = active_wcs.filter(|index| (1..=9).contains(index));
    if angle_reset && active_wcs.is_none() {
        return Some("edge-angle R0 reset requires an active work offset".into());
    }
    if direct && active_wcs.is_none() {
        return Some("direct WCO requires an active work offset".into());
    }
    None
}

/// The current reason Run is unavailable, or `None` when validation passes.
/// Keeping this separate from `run` lets the page explain a blocked action
/// without issuing an invalid command.
pub(crate) fn run_block_reason(app: &LincApp) -> Option<String> {
    if !app.snapshot.probe.enabled {
        return Some("touch probing is disabled".into());
    }
    let operation = app.probe.operation()?;
    if let Err(error) = app.snapshot.issue_allowed() {
        return Some(error);
    }
    let issued = parameters_for_run(&app.probe.params, operation);
    if let Err(error) = issued.validate_for_operation(
        app.snapshot.probe.enabled,
        app.snapshot.tool_change.current_tool,
        operation,
    ) {
        return Some(error);
    }
    if let Some(reason) = run_precondition_reason(
        operation,
        issued.probe_mode,
        app.snapshot.offsets.active_index,
    ) {
        return Some(reason);
    }
    // What the Run writes, decided by the shared classifier: `None` is an
    // ordinary measurement; every write action takes the PROTECT gate.
    mdi_write_block_reason(app, &issued.mdi_text(operation))
}

pub fn run(app: &mut LincApp) {
    app.probe.error = None;
    // A calibration may have completed since the last painted frame. Never
    // send the older value back in the parameter update preceding this run.
    app.sync_probe_calibration();
    let Some(operation) = app.probe.operation() else {
        app.probe.error = Some("select an operation".into());
        return;
    };
    if let Some(error) = run_block_reason(app) {
        app.probe.error = Some(error);
        return;
    }
    let issued = parameters_for_run(&app.probe.params, operation);
    let block = issued.mdi_text(operation);
    app.probe.last_run = Some(std::time::Instant::now());
    app.probe.last_run_operation = Some(operation.id.into());
    app.send(MachineCommand::IssueMdi {
        block,
        origin: MdiOrigin::Probe,
    });
    app.toast(
        format!(
            "RUN PROBE: {} · {}",
            operation.name,
            if operation.calibration {
                "POSITION"
            } else if issued.probe_mode == ProbeMode::ApplyWcs {
                "DIRECT WCO"
            } else {
                "POSITION"
            }
        ),
        theme::ACCENT,
    );
}

/// Why Apply is unavailable for the snapshot's originating result. This never
/// consults the currently highlighted routine: selection and result are
/// deliberately independent.
pub(crate) fn apply_block_reason(app: &LincApp) -> Option<String> {
    let Some(result) = app.snapshot.probe.last_result.as_ref() else {
        return Some("no probe result to apply".into());
    };
    if let Err(error) = result.apply_error(app.snapshot.offsets.active_index) {
        return Some(error);
    }
    if app.snapshot.panel.protect_effective() {
        return Some("PROTECT ON — probe apply blocked".into());
    }
    if !app.snapshot.writes_allowed() {
        return Some("Apply requires machine ON, a valid panel and an idle interpreter".into());
    }
    if app.snapshot.active_wcs().is_none() {
        return Some("no valid active work offset".into());
    }
    None
}

pub fn apply(app: &mut LincApp) {
    app.probe.error = None;
    let Some(result) = app.snapshot.probe.last_result.clone() else {
        app.probe.error = Some("no probe result to apply".into());
        return;
    };
    if let Some(error) = apply_block_reason(app) {
        app.probe.error = Some(error);
        return;
    }
    let Some(index) = app.snapshot.active_wcs() else {
        app.probe.error = Some("no valid active work offset".into());
        return;
    };
    let operation = linc_core::probe::operation(&result.operation);
    app.send(MachineCommand::ApplyProbeResult {
        result,
        target: ProbeTarget::WorkOffset(index),
    });
    app.toast(
        format!(
            "APPLY {} -> {}",
            operation
                .map(|operation| operation.name)
                .unwrap_or("PROBE RESULT"),
            linc_core::model::wcs_name(index)
        ),
        theme::ACCENT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_mode_survives_normal_run_preparation() {
        let mut params = ProbeParameters::default();
        params.probe_mode = ProbeMode::ApplyWcs;
        params.wcs_rotation = true;
        let operation = linc_core::probe::operation("top_back_edge_angle").unwrap();
        let issued = parameters_for_run(&params, operation);
        assert_eq!(issued.probe_mode, ProbeMode::ApplyWcs);
        assert!(issued.wcs_rotation);
        assert_eq!(params.probe_mode, ProbeMode::ApplyWcs);
    }

    #[test]
    fn calibration_clone_cannot_write_wcs_or_rotation() {
        let mut params = ProbeParameters::default();
        params.probe_mode = ProbeMode::ApplyWcs;
        params.wcs_rotation = true;
        let operation = linc_core::probe::operation("cal_round_pocket").unwrap();
        let issued = parameters_for_run(&params, operation);
        assert_eq!(issued.probe_mode, ProbeMode::MeasureOnly);
        assert!(!issued.wcs_rotation);
        // The operator's settings for ordinary routines are not rewritten.
        assert_eq!(params.probe_mode, ProbeMode::ApplyWcs);
        assert!(params.wcs_rotation);
    }

    #[test]
    fn the_shared_classifier_matches_every_ui_action() {
        let params = ProbeParameters::default();
        let block = |id: &str| params.mdi_text(linc_core::probe::operation(id).unwrap());

        // Ordinary POSITION measurement: the preamble is setup, not a write.
        for id in ["surface_z", "back_left_top_corner", "round_boss"] {
            assert_eq!(probe_write_action(&block(id)), None, "{id}");
        }
        // Edge Angle clears rotation with R0 in every mode.
        assert_eq!(
            probe_write_action(&block("top_back_edge_angle")),
            Some(ProbeWriteAction::EdgeAngleRotation)
        );
        // Calibration is decided by the routine's flag, not its category:
        // spindle_nose is SurfaceZ yet writes #3010/#3007.
        for id in [
            "cal_round_boss",
            "cal_round_pocket",
            "cal_square_boss",
            "cal_square_pocket",
            "spindle_nose",
        ] {
            assert_eq!(
                probe_write_action(&block(id)),
                Some(ProbeWriteAction::CalibrationRun),
                "{id}"
            );
        }
        // The explicit actions, and a calibration reset.
        assert_eq!(
            probe_write_action(&params.parameter_update_block()),
            Some(ProbeWriteAction::ParameterUpdate)
        );
        for block in [
            "o<reset_all_data> call",
            "o<x_data_reset> call",
            "o<y_data_reset> call",
        ] {
            assert_eq!(
                probe_write_action(block),
                Some(ProbeWriteAction::ResultReset),
                "{block}"
            );
        }
        assert_eq!(
            probe_write_action("o<probe_cal_reset> call"),
            Some(ProbeWriteAction::CalibrationReset)
        );
        // A routine call with no typed parameter block inherits a mode nobody
        // can trust, so it fails closed instead of passing as measure-only.
        // (probe_z_minus_wco is the SURFACE Z routine's call.)
        assert_eq!(
            probe_write_action("o<probe_z_minus_wco> call"),
            Some(ProbeWriteAction::UnknownRun)
        );
    }

    #[test]
    fn direct_wco_is_a_write_in_the_issued_block() {
        let mut params = ProbeParameters::default();
        params.probe_mode = ProbeMode::ApplyWcs;
        let surface = linc_core::probe::operation("surface_z").unwrap();
        assert_eq!(
            probe_write_action(&params.mdi_text(surface)),
            Some(ProbeWriteAction::DirectWco)
        );
        // A calibration run is classified as calibration whatever the mode was:
        // the UI issues it as POSITION, and it is still a write.
        let calibration = linc_core::probe::operation("cal_round_boss").unwrap();
        let issued = parameters_for_run(&params, calibration);
        assert_eq!(issued.probe_mode, ProbeMode::MeasureOnly);
        assert_eq!(
            probe_write_action(&issued.mdi_text(calibration)),
            Some(ProbeWriteAction::CalibrationRun)
        );
    }

    #[test]
    fn angle_r0_reset_is_a_write_even_in_position_mode() {
        let angle = linc_core::probe::operation("top_back_edge_angle").unwrap();
        let params = ProbeParameters::default();
        assert_eq!(
            probe_write_action(&params.mdi_text(angle)),
            Some(ProbeWriteAction::EdgeAngleRotation)
        );
        assert_eq!(
            run_precondition_reason(angle, ProbeMode::MeasureOnly, Some(1)),
            None
        );
        assert_eq!(
            run_precondition_reason(angle, ProbeMode::MeasureOnly, None).as_deref(),
            Some("edge-angle R0 reset requires an active work offset")
        );
    }

    #[test]
    fn ordinary_position_measurement_has_no_write_action() {
        let surface = linc_core::probe::operation("surface_z").unwrap();
        let params = ProbeParameters::default();
        assert_eq!(probe_write_action(&params.mdi_text(surface)), None);
        assert_eq!(
            run_precondition_reason(surface, ProbeMode::ApplyWcs, None).as_deref(),
            Some("direct WCO requires an active work offset")
        );
    }
}
