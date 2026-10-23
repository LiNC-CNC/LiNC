//! Touch Probe screen following Probe Basic's category-first workflow.

use egui::Ui;
use linc_core::command::{MachineCommand, MdiOrigin, ProbeTarget};
use linc_core::probe::ProbeMode;

use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    super::probe_view::show(app, ui);
}

pub fn category_step(app: &mut LincApp, forward: bool) {
    super::probe_view::category_step(app, forward);
}

pub fn run(app: &mut LincApp) {
    app.probe.error = None;
    if !app.snapshot.probe.enabled {
        app.probe.error = Some("touch probing is disabled".into());
        return;
    }
    let Some(operation) = app.probe.operation() else {
        app.probe.error = Some("select an operation".into());
        return;
    };
    if let Err(error) = app.snapshot.issue_allowed() {
        app.probe.error = Some(error);
        return;
    }
    if app.snapshot.tool_change.current_tool != app.probe.params.probe_tool_number {
        app.probe.error = Some(format!(
            "probe tool T{} is not in the spindle (current T{})",
            app.probe.params.probe_tool_number, app.snapshot.tool_change.current_tool
        ));
        return;
    }
    app.probe.params.probe_mode = if app.probe.measure_only {
        ProbeMode::MeasureOnly
    } else {
        ProbeMode::ApplyWcs
    };
    if let Err(error) = app.probe.params.validate(
        app.snapshot.probe.enabled,
        app.snapshot.tool_change.current_tool,
    ) {
        app.probe.error = Some(error);
        return;
    }
    let block = app.probe.params.mdi_text(operation);
    app.probe.last_run = Some(std::time::Instant::now());
    app.send(MachineCommand::IssueMdi {
        block,
        origin: MdiOrigin::Probe,
    });
    app.toast(format!("RUN PROBE: {}", operation.name), theme::ACCENT);
}

pub fn apply(app: &mut LincApp) {
    let Some(result) = app.snapshot.probe.last_result.clone() else {
        app.probe.error = Some("no probe result to apply".into());
        return;
    };
    if app.snapshot.panel.protect_effective() {
        app.probe.error = Some("PROTECT ON — probe apply blocked".into());
        return;
    }
    let target = if app
        .probe
        .operation()
        .map(|operation| operation.calibration)
        .unwrap_or(false)
    {
        ProbeTarget::Calibration
    } else {
        ProbeTarget::WorkOffset(app.probe.apply_target.clamp(1, 9))
    };
    app.send(MachineCommand::ApplyProbeResult { result, target });
    app.toast("probe result apply queued", theme::ACCENT);
}
