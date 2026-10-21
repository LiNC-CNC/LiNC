//! Touch Probe screen following Probe Basic's category-first workflow.

use egui::{
    Align, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use linc_core::command::{MachineCommand, MdiOrigin, ProbeTarget};
use linc_core::model::{format_clock, MachineVariant};
use linc_core::probe::{operations_in, ProbeCategory, ProbeDiagram, ProbeMode, ProbeOperation};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    common::page_header(
        ui,
        "TOUCH PROBE",
        "Probe Basic-compatible routines. Explicit RUN PROBE; no second stage or cycle-start.",
    );
    if !app.snapshot.probe.enabled || app.profile.variant == MachineVariant::Lathe {
        theme::banner(
            ui,
            theme::WARNING,
            "Touch probing is disabled for this profile; the page remains read-only.",
        );
        return;
    }
    category_strip(app, ui);
    ui.add_space(4.0);
    let available = ui.available_width();
    egui::ScrollArea::vertical()
        .id_salt("probe-body")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new((available * 0.62).max(540.0), 520.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        operation_grid(app, ui);
                        selected_operation_card(app, ui);
                        result_card(app, ui);
                    },
                );
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(
                        (available - (available * 0.62).max(540.0) - 8.0).max(340.0),
                        520.0,
                    ),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| setup_card(app, ui),
                );
            });
        });
}

fn category_strip(app: &mut LincApp, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for category in ProbeCategory::ALL {
            let active = app.probe.category == category;
            if common::page_tab(ui, category.label(), active).clicked() {
                app.probe.category = category;
                if let Some(first) = operations_in(category).first() {
                    app.probe.operation = first.id.to_string();
                }
            }
        }
    });
}

fn operation_grid(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, app.probe.category.label());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} OPERATIONS",
                        operations_in(app.probe.category).len()
                    ))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
                );
            });
        });
        let category_operations = operations_in(app.probe.category);
        if category_operations.is_empty() {
            ui.label(
                RichText::new(if app.probe.category == ProbeCategory::ProbeHelp {
                    "Reference notes for the Probe Basic category workflow. Select a probing category above to run an operation."
                } else {
                    "No operations are configured for this category in the current bundle."
                })
                .size(theme::font_size(11.0))
                .color(theme::MUTED),
            );
        }
        ui.horizontal_wrapped(|ui| {
            for operation in category_operations {
                let selected = app.probe.operation == operation.id;
                let response = operation_card(ui, operation, selected);
                if response.clicked() {
                    app.probe.operation = operation.id.to_string();
                    app.probe.error = None;
                }
            }
        });
    });
}

fn operation_card(ui: &mut Ui, operation: &ProbeOperation, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(128.0, 94.0), Sense::click());
    let fill = if selected {
        theme::ACCENT_DARK
    } else {
        theme::PANEL_RAISED
    };
    ui.painter().rect_filled(rect, 5, fill);
    ui.painter().rect_stroke(
        rect,
        5,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                theme::ACCENT
            } else {
                theme::BORDER
            },
        ),
        StrokeKind::Inside,
    );
    let diagram = Rect::from_min_size(rect.min + Vec2::new(30.0, 8.0), Vec2::new(68.0, 52.0));
    draw_diagram(
        ui,
        diagram,
        operation.diagram,
        if selected {
            theme::ACCENT
        } else {
            theme::MUTED
        },
    );
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - 22.0),
        Align2::CENTER_CENTER,
        operation.name,
        FontId::proportional(theme::font_size(9.5)),
        if selected { theme::FG } else { theme::MUTED },
    );
    response
}

fn draw_diagram(ui: &Ui, rect: Rect, kind: ProbeDiagram, color: Color32) {
    let painter = ui.painter_at(rect);
    let stroke = Stroke::new(1.4, color);
    let faint = Stroke::new(1.0, color.linear_multiply(0.5));
    let center = rect.center();
    match kind {
        ProbeDiagram::Corner | ProbeDiagram::InsideCorner => {
            painter.rect_stroke(rect.shrink(8.0), 2, faint, StrokeKind::Inside);
            let p = if kind == ProbeDiagram::Corner {
                rect.left_top() + Vec2::new(8.0, 8.0)
            } else {
                rect.right_bottom() - Vec2::new(8.0, 8.0)
            };
            painter.circle_filled(p, 3.0, theme::SUCCESS);
            painter.line_segment([p, p + Vec2::new(18.0, -12.0)], stroke);
            painter.text(
                p + Vec2::new(18.0, -16.0),
                Align2::LEFT_BOTTOM,
                "T99",
                FontId::monospace(theme::font_size(8.0)),
                color,
            );
        }
        ProbeDiagram::RoundBoss | ProbeDiagram::RoundPocket => {
            let radius = 22.0;
            painter.circle_stroke(center, radius, stroke);
            painter.line_segment(
                [
                    center - Vec2::new(0.0, radius),
                    center + Vec2::new(0.0, radius),
                ],
                faint,
            );
            painter.circle_filled(center, 2.0, theme::SUCCESS);
        }
        ProbeDiagram::RectBoss | ProbeDiagram::RectPocket => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::new(46.0, 34.0)),
                2,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment(
                [center - Vec2::new(23.0, 0.0), center + Vec2::new(23.0, 0.0)],
                faint,
            );
        }
        ProbeDiagram::Ridge | ProbeDiagram::Valley => {
            painter.line_segment(
                [
                    rect.left_center() + Vec2::new(8.0, -12.0),
                    rect.right_center() + Vec2::new(-8.0, -12.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    rect.left_center() + Vec2::new(8.0, 12.0),
                    rect.right_center() + Vec2::new(-8.0, 12.0),
                ],
                faint,
            );
            painter.circle_filled(center, 2.5, theme::SUCCESS);
        }
        ProbeDiagram::EdgeAngle => {
            painter.line_segment(
                [
                    rect.left_bottom() + Vec2::new(8.0, -8.0),
                    center + Vec2::new(-6.0, -14.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + Vec2::new(-6.0, -14.0),
                    rect.right_bottom() + Vec2::new(-8.0, -8.0),
                ],
                stroke,
            );
            painter.circle_filled(center + Vec2::new(-6.0, -30.0), 2.5, theme::SUCCESS);
        }
        ProbeDiagram::Surface => {
            painter.line_segment(
                [
                    rect.left_center() + Vec2::new(8.0, 12.0),
                    rect.right_center() + Vec2::new(-8.0, 12.0),
                ],
                stroke,
            );
            painter.line_segment(
                [center + Vec2::new(0.0, -26.0), center + Vec2::new(0.0, 8.0)],
                stroke,
            );
            painter.circle_stroke(
                center + Vec2::new(0.0, 8.0),
                4.0,
                Stroke::new(1.3, theme::SUCCESS),
            );
        }
        ProbeDiagram::Calibration => {
            painter.circle_stroke(center, 18.0, stroke);
            painter.circle_stroke(center, 8.0, faint);
            painter.line_segment(
                [center - Vec2::new(24.0, 0.0), center + Vec2::new(24.0, 0.0)],
                faint,
            );
            painter.line_segment(
                [center - Vec2::new(0.0, 24.0), center + Vec2::new(0.0, 24.0)],
                faint,
            );
        }
        ProbeDiagram::Help => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(theme::font_size(30.0)),
                color,
            );
        }
    }
}

fn selected_operation_card(app: &mut LincApp, ui: &mut Ui) {
    let Some(operation) = app.probe.operation() else {
        return;
    };
    theme::card(ui, |ui| {
        ui.label(
            RichText::new(operation.name)
                .size(theme::font_size(16.0))
                .strong()
                .color(theme::FG),
        );
        ui.label(
            RichText::new(operation.description)
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(format!("o<{}> call", operation.call))
                .monospace()
                .size(theme::font_size(12.0))
                .color(theme::C2),
        );
        ui.label(
            RichText::new("Parameters are passed through the Probe Basic #3014–#3036 contract.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

fn setup_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBE SETUP");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(if app.probe.measure_only {
                        "MEASURE ONLY"
                    } else {
                        "APPLY WCS"
                    })
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::ACCENT),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("WORK OFFSET")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            for index in 1..=6u8 {
                let wcs = format!("G{}", 53 + index);
                if common::page_tab(ui, &wcs, app.probe.wcs == wcs).clicked() {
                    app.probe.wcs = wcs;
                    app.probe.apply_target = index;
                }
            }
        });
        ui.horizontal(|ui| {
            if common::page_tab(ui, "APPLY WCS", !app.probe.measure_only).clicked() {
                app.probe.measure_only = false;
                app.probe.params.probe_mode = ProbeMode::ApplyWcs;
            }
            if common::page_tab(ui, "MEASURE ONLY", app.probe.measure_only).clicked() {
                app.probe.measure_only = true;
                app.probe.params.probe_mode = ProbeMode::MeasureOnly;
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("probe-params")
            .max_height(330.0)
            .show(ui, |ui| {
                egui::Grid::new("probe-params")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        drag_u16(
                            ui,
                            "PROBE TOOL",
                            &mut app.probe.params.probe_tool_number,
                            1.0,
                            0.0..=999.0,
                        );
                        drag(
                            ui,
                            "SLOW FEED",
                            &mut app.probe.params.slow_feed,
                            1.0,
                            0.1..=10000.0,
                        );
                        drag(
                            ui,
                            "FAST FEED",
                            &mut app.probe.params.fast_feed,
                            5.0,
                            0.1..=20000.0,
                        );
                        drag(
                            ui,
                            "TRAVERSE",
                            &mut app.probe.params.traverse_feed,
                            10.0,
                            0.1..=30000.0,
                        );
                        drag(
                            ui,
                            "MAX XY",
                            &mut app.probe.params.max_xy_distance,
                            0.5,
                            0.01..=1000.0,
                        );
                        drag(
                            ui,
                            "XY CLEAR",
                            &mut app.probe.params.xy_clearance,
                            0.5,
                            0.01..=1000.0,
                        );
                        drag(
                            ui,
                            "MAX Z",
                            &mut app.probe.params.max_z_distance,
                            0.5,
                            0.01..=1000.0,
                        );
                        drag(
                            ui,
                            "Z CLEAR",
                            &mut app.probe.params.z_clearance,
                            0.5,
                            0.01..=1000.0,
                        );
                        drag(
                            ui,
                            "EXTRA DEPTH",
                            &mut app.probe.params.extra_depth,
                            0.1,
                            -100.0..=100.0,
                        );
                        drag(
                            ui,
                            "STEP OFF",
                            &mut app.probe.params.step_off_width,
                            0.1,
                            0.01..=100.0,
                        );
                        drag(
                            ui,
                            "EDGE WIDTH",
                            &mut app.probe.params.edge_width,
                            0.1,
                            0.01..=100.0,
                        );
                        drag(
                            ui,
                            "TIP Ø",
                            &mut app.probe.params.diameter_hint,
                            0.1,
                            0.0..=1000.0,
                        );
                        drag(
                            ui,
                            "X BOSS",
                            &mut app.probe.params.x_hint_boss_pocket,
                            0.5,
                            -1000.0..=1000.0,
                        );
                        drag(
                            ui,
                            "Y BOSS",
                            &mut app.probe.params.y_hint_boss_pocket,
                            0.5,
                            -1000.0..=1000.0,
                        );
                        drag(
                            ui,
                            "X RIDGE",
                            &mut app.probe.params.x_hint_ridge_valley,
                            0.5,
                            -1000.0..=1000.0,
                        );
                        drag(
                            ui,
                            "Y RIDGE",
                            &mut app.probe.params.y_hint_ridge_valley,
                            0.5,
                            -1000.0..=1000.0,
                        );
                        drag(
                            ui,
                            "WCS ROT",
                            &mut app.probe.params.wcs_rotation,
                            0.1,
                            -360.0..=360.0,
                        );
                        drag(
                            ui,
                            "CAL OFFSET",
                            &mut app.probe.params.calibration_offset,
                            0.01,
                            -100.0..=100.0,
                        );
                        drag(
                            ui,
                            "CAL Ø",
                            &mut app.probe.params.calibration_diameter,
                            0.1,
                            0.0..=1000.0,
                        );
                        drag(
                            ui,
                            "X CAL",
                            &mut app.probe.params.x_cal_width,
                            0.1,
                            0.0..=1000.0,
                        );
                        drag(
                            ui,
                            "Y CAL",
                            &mut app.probe.params.y_cal_width,
                            0.1,
                            0.0..=1000.0,
                        );
                        drag_u8(
                            ui,
                            "SQ AXIS",
                            &mut app.probe.params.square_cal_axis,
                            1.0,
                            0.0..=3.0,
                        );
                    });
            });
        if let Some(error) = &app.probe.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
        ui.horizontal(|ui| {
            if theme::accent_button(ui, "RUN PROBE", true).clicked() {
                run(app);
            }
            if theme::action_button(ui, "APPLY RESULT", app.snapshot.probe.last_result.is_some())
                .clicked()
            {
                apply(app);
            }
            if theme::action_button(ui, "RESET CAL", true).clicked() {
                app.send(MachineCommand::ResetProbeCalibration);
                app.toast("probe calibration reset queued", theme::ACCENT);
            }
        });
        ui.label(
            RichText::new("Requires ON, homed, idle, T99 in spindle, no active program.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

fn drag(
    ui: &mut Ui,
    label: &str,
    value: &mut f64,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
) {
    ui.label(
        RichText::new(label)
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    ui.add(egui::DragValue::new(value).speed(speed).range(range));
    ui.end_row();
}

fn drag_u16(
    ui: &mut Ui,
    label: &str,
    value: &mut u16,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
) {
    let mut value_f = *value as f64;
    ui.label(
        RichText::new(label)
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    if ui
        .add(egui::DragValue::new(&mut value_f).speed(speed).range(range))
        .changed()
    {
        *value = value_f.round().clamp(0.0, u16::MAX as f64) as u16;
    }
    ui.end_row();
}

fn drag_u8(
    ui: &mut Ui,
    label: &str,
    value: &mut u8,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
) {
    let mut value_f = *value as f64;
    ui.label(
        RichText::new(label)
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    if ui
        .add(egui::DragValue::new(&mut value_f).speed(speed).range(range))
        .changed()
    {
        *value = value_f.round().clamp(0.0, u8::MAX as f64) as u8;
    }
    ui.end_row();
}

fn result_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "RESULT / HISTORY");
        if let Some(result) = &app.snapshot.probe.last_result {
            ui.label(
                RichText::new(format!(
                    "{}  /  {}  /  {}",
                    result.routine, result.wcs, result.mode
                ))
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::SUCCESS),
            );
            // `#1001`-`#1009` are named measurements, not axis positions.
            // Label them by meaning so a width is never shown as a coordinate.
            let measurements = linc_core::probe::ParameterResults::from_values(&result.values)
                .measurements(result.family);
            egui::Grid::new("probe-measurements")
                .num_columns(3)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for measurement in &measurements {
                        ui.label(
                            RichText::new(format!("#{}", measurement.parameter))
                                .monospace()
                                .size(theme::font_size(9.0))
                                .color(theme::MUTED),
                        );
                        ui.label(
                            RichText::new(measurement.slot.label())
                                .monospace()
                                .size(theme::font_size(10.0))
                                .color(theme::C1),
                        );
                        ui.label(
                            RichText::new(format!("{:+.4}", measurement.value))
                                .monospace()
                                .size(theme::font_size(11.0))
                                .color(theme::FG),
                        );
                        ui.end_row();
                    }
                });
            if !result.measured.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("APPLY WRITES")
                            .size(theme::font_size(8.5))
                            .color(theme::MUTED),
                    );
                    for (axis, value) in result.measured.iter() {
                        ui.label(
                            RichText::new(format!("{axis} {value:+.4}"))
                                .monospace()
                                .size(theme::font_size(11.0))
                                .color(theme::SUCCESS),
                        );
                    }
                });
            }
            match result.error {
                Some(error) => theme::kv_row(ui, "ERROR", format!("{error:.4}")),
                None => theme::kv_row(ui, "ERROR", "—"),
            }
        } else {
            ui.label(
                RichText::new("NO PROBE RESULT")
                    .size(theme::font_size(11.0))
                    .color(theme::MUTED),
            );
        }
        if !app.snapshot.probe.history.is_empty() {
            ui.label(
                RichText::new("HISTORY")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            egui::ScrollArea::vertical()
                .id_salt("probe-history")
                .max_height(100.0)
                .show(ui, |ui| {
                    for result in app.snapshot.probe.history.iter().take(10) {
                        ui.label(
                            RichText::new(format!(
                                "{}  {}  {}",
                                format_clock(result.timestamp_ms),
                                result.routine,
                                result.wcs
                            ))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                        );
                    }
                });
        }
        if let Some(cal) = app.snapshot.probe.calibration_offset {
            theme::kv_row(ui, "CALIBRATION", format!("{cal:+.4}"));
        }
    });
}

pub fn category_step(app: &mut LincApp, forward: bool) {
    let index = ProbeCategory::ALL
        .iter()
        .position(|category| *category == app.probe.category)
        .unwrap_or(0);
    let next = if forward {
        (index + 1) % ProbeCategory::ALL.len()
    } else {
        (index + ProbeCategory::ALL.len() - 1) % ProbeCategory::ALL.len()
    };
    app.probe.category = ProbeCategory::ALL[next];
    if let Some(first) = operations_in(app.probe.category).first() {
        app.probe.operation = first.id.to_string();
    }
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
        ProbeTarget::WorkOffset(app.probe.apply_target.clamp(1, 6))
    };
    app.send(MachineCommand::ApplyProbeResult { result, target });
    app.toast("probe result apply queued", theme::ACCENT);
}
