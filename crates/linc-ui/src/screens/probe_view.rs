//! Approved touch-probe page: passive category strip, compact offsets and
//! parameters, the routine matrix, and the probe symbol.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, Vec2,
};
use linc_core::command::{MachineCommand, MdiOrigin};
use linc_core::model::MachineVariant;
use linc_core::probe::ProbeCategory;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

const MAT: Color32 = Color32::from_rgb(0x4b, 0x55, 0x63);
const MAT_EDGE: Color32 = Color32::from_rgb(0x6b, 0x72, 0x80);
const TILE: Color32 = Color32::from_rgb(0x14, 0x16, 0x19);

#[derive(Clone, Copy)]
enum Icon {
    OutsideCorner(u8), // 0 BL, 1 BR, 2 FL, 3 FR
    OutsideSide(u8),   // 0 back, 1 front, 2 left, 3 right
    InsideCorner(u8),  // 0 BL, 1 BR, 2 FL, 3 FR
    InsideSide(u8),    // 0 Y+, 1 Y-, 2 X-, 3 X+
    Surface,
    RoundPocket,
    RoundBoss,
    RoundPocketCentre,
    RectPocket,
    RectBoss,
    RectPocketCentre,
    RidgeY,
    ValleyY,
    ValleyYStart,
    RidgeX,
    ValleyX,
    ValleyXStart,
    EdgeCorner(u8), // 0 X+, 1 X-, 2 Y+, 3 Y-
    EdgeSide(u8),   // 0 back, 1 front, 2 left, 3 right
    CalRoundPocket,
    CalRoundBoss,
    CalSquarePocket,
    CalSquareBoss,
}

#[derive(Clone, Copy)]
struct Card {
    key: &'static str,
    label: &'static str,
    operation: &'static str,
    icon: Icon,
}

const OUTSIDE: &[Card] = &[
    Card {
        key: "7",
        label: "BACK LEFT TOP",
        operation: "back_left_top_corner",
        icon: Icon::OutsideCorner(0),
    },
    Card {
        key: "8",
        label: "BACK TOP SIDE",
        operation: "back_top_side",
        icon: Icon::OutsideSide(0),
    },
    Card {
        key: "9",
        label: "BACK RIGHT TOP",
        operation: "back_right_top_corner",
        icon: Icon::OutsideCorner(1),
    },
    Card {
        key: "4",
        label: "LEFT TOP SIDE",
        operation: "left_top_side",
        icon: Icon::OutsideSide(2),
    },
    Card {
        key: "5",
        label: "SURFACE Z",
        operation: "surface_z",
        icon: Icon::Surface,
    },
    Card {
        key: "6",
        label: "RIGHT TOP SIDE",
        operation: "right_top_side",
        icon: Icon::OutsideSide(3),
    },
    Card {
        key: "1",
        label: "FRONT LEFT TOP",
        operation: "front_left_top_corner",
        icon: Icon::OutsideCorner(2),
    },
    Card {
        key: "2",
        label: "FRONT TOP SIDE",
        operation: "front_top_side",
        icon: Icon::OutsideSide(1),
    },
    Card {
        key: "3",
        label: "FRONT RIGHT TOP",
        operation: "front_right_top_corner",
        icon: Icon::OutsideCorner(3),
    },
];

const INSIDE: &[Card] = &[
    Card {
        key: "7",
        label: "BACK LEFT INSIDE",
        operation: "back_left_inside_corner",
        icon: Icon::InsideCorner(0),
    },
    Card {
        key: "8",
        label: "Y+ EDGE",
        operation: "edge_y_plus",
        icon: Icon::InsideSide(0),
    },
    Card {
        key: "9",
        label: "BACK RIGHT INSIDE",
        operation: "back_right_inside_corner",
        icon: Icon::InsideCorner(1),
    },
    Card {
        key: "4",
        label: "X- EDGE",
        operation: "edge_x_minus",
        icon: Icon::InsideSide(2),
    },
    Card {
        key: "5",
        label: "SURFACE Z",
        operation: "surface_z",
        icon: Icon::Surface,
    },
    Card {
        key: "6",
        label: "X+ EDGE",
        operation: "edge_x_plus",
        icon: Icon::InsideSide(3),
    },
    Card {
        key: "1",
        label: "FRONT LEFT INSIDE",
        operation: "front_left_inside_corner",
        icon: Icon::InsideCorner(2),
    },
    Card {
        key: "2",
        label: "Y- EDGE",
        operation: "edge_y_minus",
        icon: Icon::InsideSide(1),
    },
    Card {
        key: "3",
        label: "FRONT RIGHT INSIDE",
        operation: "front_right_inside_corner",
        icon: Icon::InsideCorner(3),
    },
];

const BOSS: &[Card] = &[
    Card {
        key: "4",
        label: "ROUND POCKET",
        operation: "round_pocket",
        icon: Icon::RoundPocket,
    },
    Card {
        key: "5",
        label: "ROUND BOSS",
        operation: "round_boss",
        icon: Icon::RoundBoss,
    },
    Card {
        key: "6",
        label: "ROUND POCKET CENTRE",
        operation: "round_pocket_center",
        icon: Icon::RoundPocketCentre,
    },
    Card {
        key: "1",
        label: "RECT POCKET",
        operation: "rect_pocket",
        icon: Icon::RectPocket,
    },
    Card {
        key: "2",
        label: "RECT BOSS",
        operation: "rect_boss",
        icon: Icon::RectBoss,
    },
    Card {
        key: "3",
        label: "RECT POCKET CENTRE",
        operation: "rect_pocket_center",
        icon: Icon::RectPocketCentre,
    },
];

const RIDGE: &[Card] = &[
    Card {
        key: "4",
        label: "VALLEY Y CENTRE START",
        operation: "valley_y_center",
        icon: Icon::ValleyYStart,
    },
    Card {
        key: "5",
        label: "RIDGE Y",
        operation: "ridge_y",
        icon: Icon::RidgeY,
    },
    Card {
        key: "6",
        label: "VALLEY Y",
        operation: "valley_y",
        icon: Icon::ValleyY,
    },
    Card {
        key: "1",
        label: "VALLEY X CENTRE START",
        operation: "valley_x_center",
        icon: Icon::ValleyXStart,
    },
    Card {
        key: "2",
        label: "RIDGE X",
        operation: "ridge_x",
        icon: Icon::RidgeX,
    },
    Card {
        key: "3",
        label: "VALLEY X",
        operation: "valley_x",
        icon: Icon::ValleyX,
    },
];

const EDGE: &[Card] = &[
    Card {
        key: "7",
        label: "CORNER X+",
        operation: "corner_x_plus_edge_angle",
        icon: Icon::EdgeCorner(0),
    },
    Card {
        key: "8",
        label: "TOP BACK EDGE",
        operation: "top_back_edge_angle",
        icon: Icon::EdgeSide(0),
    },
    Card {
        key: "9",
        label: "CORNER X-",
        operation: "corner_x_minus_edge_angle",
        icon: Icon::EdgeCorner(1),
    },
    Card {
        key: "4",
        label: "LEFT EDGE",
        operation: "top_left_edge_angle",
        icon: Icon::EdgeSide(2),
    },
    Card {
        key: "5",
        label: "SURFACE Z",
        operation: "surface_z",
        icon: Icon::Surface,
    },
    Card {
        key: "6",
        label: "RIGHT EDGE",
        operation: "top_right_edge_angle",
        icon: Icon::EdgeSide(3),
    },
    Card {
        key: "1",
        label: "CORNER Y+",
        operation: "corner_y_plus_edge_angle",
        icon: Icon::EdgeCorner(2),
    },
    Card {
        key: "2",
        label: "TOP FRONT EDGE",
        operation: "top_front_edge_angle",
        icon: Icon::EdgeSide(1),
    },
    Card {
        key: "3",
        label: "CORNER Y-",
        operation: "corner_y_minus_edge_angle",
        icon: Icon::EdgeCorner(3),
    },
];

const CALIBRATE: &[Card] = &[
    Card {
        key: "1",
        label: "ROUND POCKET",
        operation: "cal_round_pocket",
        icon: Icon::CalRoundPocket,
    },
    Card {
        key: "2",
        label: "ROUND BOSS",
        operation: "cal_round_boss",
        icon: Icon::CalRoundBoss,
    },
    Card {
        key: "3",
        label: "SQUARE POCKET",
        operation: "cal_square_pocket",
        icon: Icon::CalSquarePocket,
    },
    Card {
        key: "4",
        label: "SQUARE BOSS",
        operation: "cal_square_boss",
        icon: Icon::CalSquareBoss,
    },
];

fn cards_for(category: ProbeCategory) -> &'static [Card] {
    match category {
        ProbeCategory::OutsideCorners => OUTSIDE,
        ProbeCategory::InsideCorners => INSIDE,
        ProbeCategory::BossAndPocket => BOSS,
        ProbeCategory::RidgeAndValley => RIDGE,
        ProbeCategory::EdgeAngle => EDGE,
        ProbeCategory::Calibrate => CALIBRATE,
        _ => &[],
    }
}

fn category_label(category: ProbeCategory) -> &'static str {
    match category {
        ProbeCategory::OutsideCorners => "OUTSIDE CORNERS",
        ProbeCategory::InsideCorners => "INSIDE CORNERS",
        ProbeCategory::BossAndPocket => "BOSS + POCKET",
        ProbeCategory::RidgeAndValley => "RIDGE + VALLEY",
        ProbeCategory::EdgeAngle => "EDGE ANGLE",
        ProbeCategory::Calibrate => "CALIBRATE",
        ProbeCategory::ProbeHelp => "HELP",
        ProbeCategory::SurfaceZ => "SURFACE / Z",
        ProbeCategory::RotaryAxis => "ROTARY AXIS",
    }
}

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    if !app.snapshot.probe.enabled || app.profile.variant == MachineVariant::Lathe {
        theme::banner(
            ui,
            theme::WARNING,
            "Touch probing is disabled for this profile; the page remains read-only.",
        );
        return;
    }
    common::focus_anchor(ui, "probe-page-focus");
    handle_keys(app, ui);
    category_tabs(app, ui);
    ui.add_space(6.0);
    if app.probe.category == ProbeCategory::Calibrate {
        calibrate_view(app, ui);
        return;
    }
    if app.probe.category == ProbeCategory::ProbeHelp {
        help_view(app, ui);
        return;
    }
    let available = ui.available_width();
    let height = ui.available_height().max(380.0);
    let gap = 8.0;
    let right_w = 150.0;
    let left_w = (available * 0.40).clamp(330.0, 470.0);
    let center_w = (available - left_w - right_w - 2.0 * gap).max(300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("probe-left")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        parameters_card(app, ui);
                        ui.add_space(6.0);
                        results_card(app, ui);
                    });
            },
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            Vec2::new(center_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| matrix_card(app, ui, height),
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            Vec2::new(right_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| probe_symbol(app, ui, height),
        );
    });
}

fn category_tabs(app: &mut LincApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.x = 16.0;
    ui.horizontal(|ui| {
        for category in ProbeCategory::ALL {
            let active = app.probe.category == category;
            let text = RichText::new(category_label(category))
                .size(theme::font_size(9.0))
                .color(if active { theme::ACCENT } else { theme::MUTED })
                .strong();
            let response = ui.add(egui::Label::new(text).sense(Sense::hover()));
            if active {
                ui.painter().line_segment(
                    [
                        Pos2::new(response.rect.left(), response.rect.bottom() + 2.0),
                        Pos2::new(response.rect.right(), response.rect.bottom() + 2.0),
                    ],
                    Stroke::new(2.0, theme::ACCENT),
                );
            }
        }
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let count = ProbeCategory::ALL
                .iter()
                .position(|category| *category == app.probe.category)
                .unwrap_or(0)
                + 1;
            ui.label(
                RichText::new(format!("{count}/{}", ProbeCategory::ALL.len()))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new("OFFSET NEXT")
                    .size(theme::font_size(8.0))
                    .color(theme::MUTED),
            );
            let (rect, _) = ui.allocate_exact_size(Vec2::new(54.0, 18.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(3), theme::PANEL_RAISED);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(3),
                Stroke::new(1.0, theme::BORDER),
                StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "OFFSET",
                FontId::monospace(theme::font_size(8.0)),
                theme::FG,
            );
        });
    });
    let y = ui.cursor().top();
    ui.painter().line_segment(
        [
            Pos2::new(ui.min_rect().left(), y),
            Pos2::new(ui.min_rect().right(), y),
        ],
        Stroke::new(1.0, theme::BORDER),
    );
}

fn parameters_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 12.0;
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.spacing_mut().button_padding.y = 1.0;
        ui.style_mut().text_styles.insert(
            egui::TextStyle::Body,
            FontId::monospace(theme::font_size(9.0)),
        );
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBING PARAMETERS");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("WCS {}", app.probe.wcs))
                        .monospace()
                        .size(theme::font_size(8.0))
                        .color(theme::SUCCESS),
                );
                ui.label(
                    RichText::new("←→ FIELD · ↑↓ ADJUST")
                        .size(theme::font_size(7.5))
                        .color(theme::MUTED),
                );
            });
        });
        let mut measure = app.probe.measure_only;
        if ui
            .checkbox(
                &mut measure,
                RichText::new("PROBE POSITION ONLY").size(theme::font_size(8.0)),
            )
            .changed()
        {
            app.probe.measure_only = measure;
            app.probe.params.probe_mode = if measure {
                linc_core::probe::ProbeMode::MeasureOnly
            } else {
                linc_core::probe::ProbeMode::ApplyWcs
            };
        }
        ui.label(
            RichText::new("OFF = APPLY TO ACTIVE WCS")
                .size(theme::font_size(7.5))
                .color(theme::MUTED),
        );
        egui::Grid::new("probe-params-grid")
            .num_columns(4)
            .spacing([6.0, 1.0])
            .show(ui, |ui| {
                param_pair(
                    ui,
                    "PROBE TOOL",
                    &mut ParamValue::U16(&mut app.probe.params.probe_tool_number),
                    "SLOW FEED",
                    &mut ParamValue::F64(&mut app.probe.params.slow_feed),
                );
                param_pair(
                    ui,
                    "FAST FEED",
                    &mut ParamValue::F64(&mut app.probe.params.fast_feed),
                    "TRAVERSE",
                    &mut ParamValue::F64(&mut app.probe.params.traverse_feed),
                );
                param_pair(
                    ui,
                    "MAX XY",
                    &mut ParamValue::F64(&mut app.probe.params.max_xy_distance),
                    "XY CLEAR",
                    &mut ParamValue::F64(&mut app.probe.params.xy_clearance),
                );
                param_pair(
                    ui,
                    "MAX Z",
                    &mut ParamValue::F64(&mut app.probe.params.max_z_distance),
                    "Z CLEAR",
                    &mut ParamValue::F64(&mut app.probe.params.z_clearance),
                );
                param_pair(
                    ui,
                    "EXTRA DEPTH",
                    &mut ParamValue::F64(&mut app.probe.params.extra_depth),
                    "STEP OFF",
                    &mut ParamValue::F64(&mut app.probe.params.step_off_width),
                );
                param_pair(
                    ui,
                    "EDGE WIDTH",
                    &mut ParamValue::F64(&mut app.probe.params.edge_width),
                    "TIP",
                    &mut ParamValue::F64(&mut app.probe.params.diameter_hint),
                );
            });
        if let Some(error) = &app.probe.error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(8.0))
                    .color(theme::ERROR),
            );
        }
    });
}

enum ParamValue<'a> {
    F64(&'a mut f64),
    U16(&'a mut u16),
}

fn param_pair(ui: &mut Ui, l1: &str, v1: &mut ParamValue, l2: &str, v2: &mut ParamValue) {
    param_label(ui, l1);
    param_field(ui, v1);
    param_label(ui, l2);
    param_field(ui, v2);
    ui.end_row();
}

fn param_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::font_size(7.5))
            .color(theme::MUTED),
    );
}

fn param_field(ui: &mut Ui, value: &mut ParamValue) {
    let width = 56.0;
    let height = 14.0;
    match value {
        ParamValue::F64(v) => {
            ui.add_sized(
                [width, height],
                egui::DragValue::new(*v).speed(0.5).max_decimals(3),
            );
        }
        ParamValue::U16(v) => {
            let mut f = **v as f64;
            if ui
                .add_sized([width, height], egui::DragValue::new(&mut f).speed(1.0))
                .changed()
            {
                **v = f.round().clamp(0.0, u16::MAX as f64) as u16;
            }
        }
    }
}

fn results_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 12.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.spacing_mut().button_padding.y = 1.0;
        ui.style_mut().text_styles.insert(
            egui::TextStyle::Body,
            FontId::monospace(theme::font_size(9.0)),
        );
        theme::section_header(ui, "PROBE RESULTS");
        ui.horizontal(|ui| {
            if small_button(ui, "UPDATE PROBE PARAMS", 168.0).clicked() {
                let block = app.probe.params.parameter_update_block();
                issue_probe_mdi(app, block, "probe parameters updated");
            }
            if small_button(ui, "RESET ALL DATA", 112.0).clicked() {
                issue_probe_mdi(app, "o<reset_all_data> call".into(), "probe data reset");
            }
        });
        ui.horizontal(|ui| {
            if small_button(ui, "X DATA RESET", 100.0).clicked() {
                issue_probe_mdi(app, "o<x_data_reset> call".into(), "probe X data reset");
            }
            if small_button(ui, "Y DATA RESET", 100.0).clicked() {
                issue_probe_mdi(app, "o<y_data_reset> call".into(), "probe Y data reset");
            }
        });
        let result = app.snapshot.probe.last_result.as_ref();
        let value = |index: usize| {
            result
                .and_then(|r| r.values.get(index))
                .copied()
                .unwrap_or(0.0)
        };
        egui::Grid::new("probe-results-image-grid")
            .num_columns(6)
            .spacing([4.0, 3.0])
            .show(ui, |ui| {
                result_field(ui, "X- PROBED", value(4));
                result_field(ui, "X+ PROBED", value(5));
                result_field(ui, "X WIDTH", value(0));
                ui.end_row();
                result_field(ui, "Y- PROBED", value(6));
                result_field(ui, "Y+ PROBED", value(7));
                result_field(ui, "Y WIDTH", value(2));
                ui.end_row();
                result_field(ui, "Z- PROBED", value(8));
                result_field(ui, "DIAM", value(9));
                result_field(ui, "X CENTER", value(1));
                ui.end_row();
                result_field(ui, "EDGE DELTA", value(10));
                result_field(ui, "EDGE ANGLE", value(11));
                result_field(ui, "Y CENTER", value(3));
                ui.end_row();
            });
        if let Some(cal) = app.snapshot.probe.calibration_offset {
            ui.label(
                RichText::new(format!("CAL OFFSET {cal:+.4}"))
                    .monospace()
                    .size(theme::font_size(7.5))
                    .color(theme::MUTED),
            );
        }
    });
}

fn small_button(ui: &mut Ui, text: &str, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::click());
    let fill = if response.hovered() {
        theme::PANEL_RAISED
    } else {
        theme::SURFACE
    };
    ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(8.0),
        theme::FG,
    );
    response
}

fn result_field(ui: &mut Ui, label: &str, value: f64) {
    ui.label(
        RichText::new(label)
            .size(theme::font_size(7.5))
            .color(theme::MUTED),
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(66.0, 14.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), theme::PANEL_RAISED);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{value:.4}"),
        FontId::monospace(theme::font_size(9.0)),
        theme::FG,
    );
}

fn issue_probe_mdi(app: &mut LincApp, block: String, toast: &str) {
    if let Err(error) = app.snapshot.issue_allowed() {
        app.probe.error = Some(error);
        return;
    }
    app.send(MachineCommand::IssueMdi {
        block,
        origin: MdiOrigin::Probe,
    });
    app.toast(toast, theme::ACCENT);
}

fn matrix_card(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let cards = cards_for(app.probe.category);
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(0.0));
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBING ROUTINES");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} ROUTINES", cards.len()))
                        .monospace()
                        .size(theme::font_size(8.0))
                        .color(theme::MUTED),
                );
            });
        });
        ui.label(
            RichText::new(category_label(app.probe.category))
                .size(theme::font_size(8.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        let gap = 6.0;
        let card_w = ((ui.available_width() - gap * 2.0) / 3.0).max(88.0);
        let rows = cards.len().div_ceil(3);
        let card_h = if rows > 2 { 98.0 } else { 112.0 };
        for row in 0..rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..3 {
                    let index = row * 3 + col;
                    if let Some(card) = cards.get(index) {
                        let texture = app.probe.texture(ui.ctx(), card.operation);
                        if probe_card(
                            ui,
                            card,
                            app.probe.operation == card.operation,
                            card_w,
                            card_h,
                            texture.as_ref(),
                        ) {
                            app.probe.operation = card.operation.to_string();
                            app.probe.error = None;
                        }
                    } else {
                        ui.allocate_space(Vec2::new(card_w, card_h));
                    }
                }
            });
            ui.add_space(4.0);
        }
        if let Some(hint) = hint_row(app) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("HINT")
                        .monospace()
                        .size(theme::font_size(8.0))
                        .color(theme::MUTED),
                );
                for (label, value) in hint {
                    ui.label(
                        RichText::new(label)
                            .size(theme::font_size(8.0))
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new(value)
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::FG),
                    );
                }
            });
        }
        let _ = height;
    });
}

fn hint_row(app: &LincApp) -> Option<Vec<(String, String)>> {
    match app.probe.category {
        ProbeCategory::BossAndPocket => Some(vec![
            (
                "DIAM".into(),
                format!("{:.1}", app.probe.params.diameter_hint),
            ),
            (
                "X".into(),
                format!("{:.1}", app.probe.params.x_hint_boss_pocket),
            ),
            (
                "Y".into(),
                format!("{:.1}", app.probe.params.y_hint_boss_pocket),
            ),
        ]),
        ProbeCategory::RidgeAndValley => Some(vec![
            (
                "X".into(),
                format!("{:.1}", app.probe.params.x_hint_ridge_valley),
            ),
            (
                "Y".into(),
                format!("{:.1}", app.probe.params.y_hint_ridge_valley),
            ),
        ]),
        _ => None,
    }
}

fn probe_card(
    ui: &mut Ui,
    card: &Card,
    selected: bool,
    width: f32,
    height: f32,
    texture: Option<&egui::TextureHandle>,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let fill = if selected {
        Color32::from_rgb(0x13, 0x28, 0x42)
    } else {
        theme::PANEL
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(5),
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
    let tile_side = (width - 18.0).clamp(30.0, 62.0);
    let tile = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.top() + 10.0 + tile_side / 2.0),
        Vec2::splat(tile_side),
    );
    ui.painter().rect_filled(tile, CornerRadius::same(3), TILE);
    ui.painter().rect_stroke(
        tile,
        CornerRadius::same(3),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    if let Some(texture) = texture {
        ui.painter().image(
            texture.id(),
            tile,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        draw_icon(ui, tile, card.icon, selected);
    }
    let key = Rect::from_min_size(rect.min + Vec2::new(5.0, 5.0), Vec2::new(18.0, 15.0));
    ui.painter()
        .rect_filled(key, CornerRadius::same(3), theme::PANEL_RAISED);
    ui.painter().rect_stroke(
        key,
        CornerRadius::same(3),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        key.center(),
        Align2::CENTER_CENTER,
        card.key,
        FontId::monospace(theme::font_size(8.0)),
        if selected {
            theme::ACCENT
        } else {
            theme::MUTED
        },
    );
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - 10.0),
        Align2::CENTER_CENTER,
        card.label,
        FontId::proportional(7.0),
        if selected { theme::FG } else { theme::MUTED },
    );
    response.clicked()
}

fn draw_icon(ui: &Ui, r: Rect, icon: Icon, _selected: bool) {
    let p = ui.painter_at(r);
    let c = r.center();
    let inset = 4.0;
    let b = r.shrink(inset);
    let cross = |p: &egui::Painter, at: Pos2| {
        p.circle_stroke(at, 4.0, Stroke::new(1.2, theme::SUCCESS));
        for (dx, dy) in [(0.0, -1.0), (0.0, 1.0), (-1.0, 0.0), (1.0, 0.0)] {
            p.line_segment(
                [at, at + Vec2::new(dx, dy) * 7.0],
                Stroke::new(1.0, theme::SUCCESS),
            );
        }
    };
    let ruby = |p: &egui::Painter, at: Pos2| {
        p.circle_filled(at, 3.0, theme::C2);
        p.circle_filled(
            at - Vec2::new(0.8, 0.8),
            0.9,
            Color32::from_rgb(0xe9, 0xd5, 0xff),
        );
    };
    let arrow = |p: &egui::Painter, a: Pos2, z: Pos2| {
        p.line_segment([a, z], Stroke::new(1.9, theme::ACCENT));
        let d = (z - a).normalized();
        let n = Vec2::new(-d.y, d.x);
        p.add(egui::Shape::convex_polygon(
            vec![z, z - d * 5.5 + n * 3.0, z - d * 5.5 - n * 3.0],
            theme::ACCENT,
            Stroke::NONE,
        ));
    };
    let material = |p: &egui::Painter, rect: Rect| {
        p.rect_filled(rect, CornerRadius::same(1), MAT);
    };
    let edge = |p: &egui::Painter, a: Pos2, z: Pos2| {
        p.line_segment([a, z], Stroke::new(1.2, MAT_EDGE));
    };

    match icon {
        Icon::OutsideCorner(kind) => {
            let m = match kind {
                0 => Rect::from_min_max(c, b.max),
                1 => Rect::from_min_max(Pos2::new(b.left(), c.y), Pos2::new(c.x, b.bottom())),
                2 => Rect::from_min_max(Pos2::new(c.x, b.top()), Pos2::new(b.right(), c.y)),
                _ => Rect::from_min_max(b.min, c),
            };
            material(&p, m);
            match kind {
                0 => {
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(b.right(), c.y));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(c.x, b.bottom()));
                }
                1 => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(c.x, b.bottom()));
                }
                2 => {
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(b.right(), c.y));
                }
                _ => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                }
            }
            let off = 9.0;
            match kind {
                0 => {
                    arrow(&p, Pos2::new(c.x + off, b.top()), Pos2::new(c.x + off, c.y));
                    arrow(
                        &p,
                        Pos2::new(b.left(), c.y + off),
                        Pos2::new(c.x, c.y + off),
                    );
                    ruby(&p, c + Vec2::new(off, off));
                }
                1 => {
                    arrow(&p, Pos2::new(c.x - off, b.top()), Pos2::new(c.x - off, c.y));
                    arrow(
                        &p,
                        Pos2::new(b.right(), c.y + off),
                        Pos2::new(c.x, c.y + off),
                    );
                    ruby(&p, c + Vec2::new(-off, off));
                }
                2 => {
                    arrow(
                        &p,
                        Pos2::new(c.x + off, b.bottom()),
                        Pos2::new(c.x + off, c.y),
                    );
                    arrow(
                        &p,
                        Pos2::new(b.left(), c.y - off),
                        Pos2::new(c.x, c.y - off),
                    );
                    ruby(&p, c + Vec2::new(off, -off));
                }
                _ => {
                    arrow(
                        &p,
                        Pos2::new(c.x - off, b.bottom()),
                        Pos2::new(c.x - off, c.y),
                    );
                    arrow(
                        &p,
                        Pos2::new(b.right(), c.y - off),
                        Pos2::new(c.x, c.y - off),
                    );
                    ruby(&p, c + Vec2::new(-off, -off));
                }
            }
            cross(&p, c);
        }
        Icon::OutsideSide(kind) => {
            let m = match kind {
                0 => Rect::from_min_max(Pos2::new(b.left(), c.y), b.max),
                1 => Rect::from_min_max(b.min, Pos2::new(b.right(), c.y)),
                2 => Rect::from_min_max(Pos2::new(c.x, b.top()), b.max),
                _ => Rect::from_min_max(b.min, Pos2::new(c.x, b.bottom())),
            };
            material(&p, m);
            let off = 10.0;
            match kind {
                0 => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(b.right(), c.y));
                    arrow(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                    ruby(&p, c + Vec2::new(0.0, off));
                }
                1 => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(b.right(), c.y));
                    arrow(&p, Pos2::new(c.x, b.bottom()), Pos2::new(c.x, c.y));
                    ruby(&p, c - Vec2::new(0.0, off));
                }
                2 => {
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, b.bottom()));
                    arrow(&p, Pos2::new(b.right(), c.y), Pos2::new(c.x, c.y));
                    ruby(&p, c + Vec2::new(off, 0.0));
                }
                _ => {
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, b.bottom()));
                    arrow(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                    ruby(&p, c - Vec2::new(off, 0.0));
                }
            }
            cross(&p, c);
        }
        Icon::InsideCorner(kind) => {
            // The same block but with material on the opposite side of the corner.
            match kind {
                0 => {
                    material(&p, Rect::from_min_max(b.min, Pos2::new(b.right(), c.y)));
                    material(&p, Rect::from_min_max(b.min, Pos2::new(c.x, b.bottom())));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(c.x, b.bottom()));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(b.right(), c.y));
                }
                1 => {
                    material(&p, Rect::from_min_max(b.min, Pos2::new(b.right(), c.y)));
                    material(&p, Rect::from_min_max(Pos2::new(c.x, b.top()), b.max));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(c.x, b.bottom()));
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                }
                2 => {
                    material(&p, Rect::from_min_max(Pos2::new(b.left(), c.y), b.max));
                    material(&p, Rect::from_min_max(b.min, Pos2::new(c.x, b.bottom())));
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                    edge(&p, Pos2::new(c.x, c.y), Pos2::new(b.right(), c.y));
                }
                _ => {
                    material(&p, Rect::from_min_max(Pos2::new(b.left(), c.y), b.max));
                    material(&p, Rect::from_min_max(Pos2::new(c.x, b.top()), b.max));
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                }
            }
            let off = 10.0;
            match kind {
                0 => {
                    arrow(
                        &p,
                        Pos2::new(c.x + off, b.bottom()),
                        Pos2::new(c.x + off, c.y),
                    );
                    arrow(
                        &p,
                        Pos2::new(b.right(), c.y + off),
                        Pos2::new(c.x, c.y + off),
                    );
                    ruby(&p, c + Vec2::new(-off, -off));
                }
                1 => {
                    arrow(
                        &p,
                        Pos2::new(c.x - off, b.bottom()),
                        Pos2::new(c.x - off, c.y),
                    );
                    arrow(
                        &p,
                        Pos2::new(b.left(), c.y + off),
                        Pos2::new(c.x, c.y + off),
                    );
                    ruby(&p, c + Vec2::new(off, -off));
                }
                2 => {
                    arrow(&p, Pos2::new(c.x + off, b.top()), Pos2::new(c.x + off, c.y));
                    arrow(
                        &p,
                        Pos2::new(b.right(), c.y - off),
                        Pos2::new(c.x, c.y - off),
                    );
                    ruby(&p, c + Vec2::new(-off, off));
                }
                _ => {
                    arrow(&p, Pos2::new(c.x - off, b.top()), Pos2::new(c.x - off, c.y));
                    arrow(
                        &p,
                        Pos2::new(b.left(), c.y - off),
                        Pos2::new(c.x, c.y - off),
                    );
                    ruby(&p, c + Vec2::new(off, off));
                }
            }
            cross(&p, c);
        }
        Icon::InsideSide(kind) => {
            let m = match kind {
                0 => Rect::from_min_max(b.min, Pos2::new(b.right(), c.y)),
                1 => Rect::from_min_max(Pos2::new(b.left(), c.y), b.max),
                2 => Rect::from_min_max(b.min, Pos2::new(c.x, b.bottom())),
                _ => Rect::from_min_max(Pos2::new(c.x, b.top()), b.max),
            };
            material(&p, m);
            match kind {
                0 => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(b.right(), c.y));
                    arrow(&p, Pos2::new(c.x, b.bottom()), Pos2::new(c.x, c.y));
                    ruby(&p, c - Vec2::new(0.0, 10.0));
                }
                1 => {
                    edge(&p, Pos2::new(b.left(), c.y), Pos2::new(b.right(), c.y));
                    arrow(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y));
                    ruby(&p, c + Vec2::new(0.0, 10.0));
                }
                2 => {
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, b.bottom()));
                    arrow(&p, Pos2::new(b.right(), c.y), Pos2::new(c.x, c.y));
                    ruby(&p, c - Vec2::new(10.0, 0.0));
                }
                _ => {
                    edge(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, b.bottom()));
                    arrow(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x, c.y));
                    ruby(&p, c + Vec2::new(10.0, 0.0));
                }
            }
            cross(&p, c);
        }
        Icon::Surface => {
            material(&p, b);
            cross(&p, c);
            ruby(&p, c);
        }
        Icon::RoundPocket | Icon::RoundBoss | Icon::RoundPocketCentre => {
            let hole = matches!(icon, Icon::RoundPocket | Icon::RoundPocketCentre);
            if hole {
                material(&p, b);
            }
            p.circle_filled(c, 16.0, if hole { TILE } else { MAT });
            if matches!(icon, Icon::RoundPocketCentre) {
                p.circle_stroke(c, 16.0, Stroke::new(6.0, MAT));
            }
            if hole {
                for (a, z) in [
                    (Pos2::new(c.x, c.y - 6.0), Pos2::new(c.x, c.y - 16.0)),
                    (Pos2::new(c.x, c.y + 6.0), Pos2::new(c.x, c.y + 16.0)),
                    (Pos2::new(c.x - 6.0, c.y), Pos2::new(c.x - 16.0, c.y)),
                    (Pos2::new(c.x + 6.0, c.y), Pos2::new(c.x + 16.0, c.y)),
                ] {
                    arrow(&p, a, z);
                }
            } else {
                for (a, z) in [
                    (Pos2::new(c.x, c.y - 16.0), Pos2::new(c.x, c.y - 6.0)),
                    (Pos2::new(c.x, c.y + 16.0), Pos2::new(c.x, c.y + 6.0)),
                    (Pos2::new(c.x - 16.0, c.y), Pos2::new(c.x - 6.0, c.y)),
                    (Pos2::new(c.x + 16.0, c.y), Pos2::new(c.x + 6.0, c.y)),
                ] {
                    arrow(&p, a, z);
                }
            }
            cross(&p, c);
            ruby(&p, if hole { c - Vec2::new(20.0, 0.0) } else { c });
        }
        Icon::RectPocket | Icon::RectBoss | Icon::RectPocketCentre => {
            let hole = matches!(icon, Icon::RectPocket | Icon::RectPocketCentre);
            if hole {
                material(&p, b);
            }
            let rr = Rect::from_center_size(c, Vec2::new(32.0, 22.0));
            p.rect_filled(rr, CornerRadius::same(1), if hole { TILE } else { MAT });
            if matches!(icon, Icon::RectPocketCentre) {
                p.rect_stroke(
                    rr,
                    CornerRadius::same(1),
                    Stroke::new(6.0, MAT),
                    StrokeKind::Inside,
                );
            }
            if hole {
                for (a, z) in [
                    (Pos2::new(c.x, c.y - 5.0), Pos2::new(c.x, c.y - 11.0)),
                    (Pos2::new(c.x, c.y + 5.0), Pos2::new(c.x, c.y + 11.0)),
                    (Pos2::new(c.x - 9.0, c.y), Pos2::new(c.x - 16.0, c.y)),
                    (Pos2::new(c.x + 9.0, c.y), Pos2::new(c.x + 16.0, c.y)),
                ] {
                    arrow(&p, a, z);
                }
            } else {
                for (a, z) in [
                    (Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y - 11.0)),
                    (Pos2::new(c.x, b.bottom()), Pos2::new(c.x, c.y + 11.0)),
                    (Pos2::new(b.left(), c.y), Pos2::new(c.x - 16.0, c.y)),
                    (Pos2::new(b.right(), c.y), Pos2::new(c.x + 16.0, c.y)),
                ] {
                    arrow(&p, a, z);
                }
            }
            cross(&p, c);
            ruby(&p, if hole { c - Vec2::new(20.0, 0.0) } else { c });
        }
        Icon::RidgeY | Icon::ValleyY | Icon::ValleyYStart => {
            let valley = !matches!(icon, Icon::RidgeY);
            if valley {
                material(
                    &p,
                    Rect::from_min_max(
                        Pos2::new(b.left(), b.top()),
                        Pos2::new(b.right(), c.y - 8.0),
                    ),
                );
                material(
                    &p,
                    Rect::from_min_max(Pos2::new(b.left(), c.y + 8.0), b.max),
                );
                arrow(&p, Pos2::new(c.x, c.y - 4.0), Pos2::new(c.x, c.y - 8.0));
                arrow(&p, Pos2::new(c.x, c.y + 4.0), Pos2::new(c.x, c.y + 8.0));
            } else {
                material(
                    &p,
                    Rect::from_min_max(
                        Pos2::new(b.left(), c.y - 8.0),
                        Pos2::new(b.right(), c.y + 8.0),
                    ),
                );
                arrow(&p, Pos2::new(c.x, b.top()), Pos2::new(c.x, c.y - 8.0));
                arrow(&p, Pos2::new(c.x, b.bottom()), Pos2::new(c.x, c.y + 8.0));
            }
            cross(&p, c);
            ruby(
                &p,
                if matches!(icon, Icon::ValleyYStart) {
                    c - Vec2::new(0.0, 10.0)
                } else {
                    c
                },
            );
        }
        Icon::RidgeX | Icon::ValleyX | Icon::ValleyXStart => {
            let valley = !matches!(icon, Icon::RidgeX);
            if valley {
                material(
                    &p,
                    Rect::from_min_max(b.min, Pos2::new(c.x - 8.0, b.bottom())),
                );
                material(&p, Rect::from_min_max(Pos2::new(c.x + 8.0, b.top()), b.max));
                arrow(&p, Pos2::new(c.x - 4.0, c.y), Pos2::new(c.x - 8.0, c.y));
                arrow(&p, Pos2::new(c.x + 4.0, c.y), Pos2::new(c.x + 8.0, c.y));
            } else {
                material(
                    &p,
                    Rect::from_min_max(
                        Pos2::new(c.x - 8.0, b.top()),
                        Pos2::new(c.x + 8.0, b.bottom()),
                    ),
                );
                arrow(&p, Pos2::new(b.left(), c.y), Pos2::new(c.x - 8.0, c.y));
                arrow(&p, Pos2::new(b.right(), c.y), Pos2::new(c.x + 8.0, c.y));
            }
            cross(&p, c);
            ruby(
                &p,
                if matches!(icon, Icon::ValleyXStart) {
                    c - Vec2::new(10.0, 0.0)
                } else {
                    c
                },
            );
        }
        Icon::EdgeCorner(_kind) | Icon::EdgeSide(_kind) => {
            let poly = vec![
                b.left_top() + Vec2::new(5.0, 3.0),
                b.right_top() + Vec2::new(-4.0, 0.0),
                b.right_bottom() + Vec2::new(0.0, -4.0),
                b.left_bottom() + Vec2::new(2.0, 0.0),
            ];
            p.add(egui::Shape::convex_polygon(
                poly,
                MAT,
                Stroke::new(1.0, MAT_EDGE),
            ));
            let target = match icon {
                Icon::EdgeCorner(0) => b.left_top() + Vec2::new(7.0, 7.0),
                Icon::EdgeCorner(1) => b.right_top() + Vec2::new(-7.0, 7.0),
                Icon::EdgeCorner(2) => b.left_bottom() + Vec2::new(7.0, -7.0),
                Icon::EdgeCorner(3) => b.right_bottom() + Vec2::new(-7.0, -7.0),
                Icon::EdgeSide(0) => Pos2::new(c.x, b.top() + 2.0),
                Icon::EdgeSide(1) => Pos2::new(c.x, b.bottom() - 2.0),
                Icon::EdgeSide(2) => Pos2::new(b.left() + 2.0, c.y),
                _ => Pos2::new(b.right() - 2.0, c.y),
            };
            cross(&p, target);
            arrow(
                &p,
                target - Vec2::new(0.0, 12.0),
                target - Vec2::new(0.0, 3.0),
            );
            arrow(
                &p,
                target - Vec2::new(12.0, 0.0),
                target - Vec2::new(3.0, 0.0),
            );
            ruby(&p, target + Vec2::new(7.0, 8.0));
        }
        Icon::CalRoundPocket | Icon::CalRoundBoss | Icon::CalSquarePocket | Icon::CalSquareBoss => {
            let pocket = matches!(icon, Icon::CalRoundPocket | Icon::CalSquarePocket);
            let round = matches!(icon, Icon::CalRoundPocket | Icon::CalRoundBoss);
            if round {
                if pocket {
                    p.circle_stroke(c, 15.0, Stroke::new(6.0, MAT));
                } else {
                    p.circle_filled(c, 15.0, MAT);
                }
            } else if pocket {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::new(30.0, 24.0)),
                    CornerRadius::same(1),
                    Stroke::new(6.0, MAT),
                    StrokeKind::Inside,
                );
            } else {
                p.rect_filled(
                    Rect::from_center_size(c, Vec2::new(30.0, 24.0)),
                    CornerRadius::same(1),
                    MAT,
                );
            }
            cross(&p, c);
            ruby(&p, if pocket { c - Vec2::new(14.0, 0.0) } else { c });
        }
    }
}

fn probe_symbol(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(0.0));
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBE");
            ui.label(
                RichText::new("T99")
                    .monospace()
                    .size(theme::font_size(8.0))
                    .color(theme::MUTED),
            );
        });
        let draw_h = 210.0;
        let spare = (ui.available_height() - draw_h).max(0.0);
        if spare > 0.0 {
            ui.add_space(spare / 2.0);
        }
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), draw_h), Sense::hover());
        let p = ui.painter_at(rect);
        let c = Pos2::new(rect.center().x, rect.top() + 70.0);
        p.line_segment(
            [Pos2::new(c.x, rect.top() + 8.0), Pos2::new(c.x, c.y - 34.0)],
            Stroke::new(2.0, theme::MUTED),
        );
        p.rect_filled(
            Rect::from_center_size(Pos2::new(c.x, c.y - 28.0), Vec2::new(34.0, 12.0)),
            CornerRadius::same(2),
            theme::PANEL_RAISED,
        );
        p.rect_filled(
            Rect::from_center_size(Pos2::new(c.x, c.y - 10.0), Vec2::new(22.0, 42.0)),
            CornerRadius::same(8),
            theme::PANEL_RAISED,
        );
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(c.x - 11.0, c.y + 11.0),
                Pos2::new(c.x + 11.0, c.y + 11.0),
                Pos2::new(c.x + 4.0, c.y + 42.0),
                Pos2::new(c.x - 4.0, c.y + 42.0),
            ],
            theme::PANEL_RAISED,
            Stroke::new(1.0, theme::MUTED),
        ));
        p.line_segment(
            [Pos2::new(c.x, c.y + 42.0), Pos2::new(c.x, c.y + 82.0)],
            Stroke::new(1.5, theme::MUTED),
        );
        p.circle_filled(Pos2::new(c.x, c.y + 84.0), 4.0, theme::C2);
        p.circle_stroke(
            Pos2::new(c.x, c.y + 84.0),
            9.0,
            Stroke::new(1.0, theme::SUCCESS),
        );
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.left() + 10.0, rect.bottom() - 28.0),
                Vec2::new(rect.width() - 20.0, 8.0),
            ),
            CornerRadius::same(1),
            MAT,
        );
        let led = if app.snapshot.probe.running {
            theme::WARNING
        } else if app.snapshot.probe.error.is_some() {
            theme::ERROR
        } else {
            theme::SUCCESS
        };
        // The LED is on the probe body, not floating beside it.
        let led_at = Pos2::new(c.x, c.y - 2.0);
        p.circle_filled(led_at, 5.0, led.linear_multiply(0.18));
        p.circle_filled(led_at, 3.0, led);
        p.text(
            Pos2::new(rect.left() + 10.0, rect.bottom() - 10.0),
            Align2::LEFT_CENTER,
            "PROBE INPUT  OPEN",
            FontId::monospace(theme::font_size(6.5)),
            theme::MUTED,
        );
    });
}

fn calibrate_view(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    let height = ui.available_height().max(380.0);
    let right_w = 150.0;
    let gap = 8.0;
    let panel_w = (available - right_w - gap).max(400.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(panel_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| calibration_panel(app, ui),
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            Vec2::new(right_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| probe_symbol(app, ui, height),
        );
    });
}

fn calibration_panel(app: &mut LincApp, ui: &mut Ui) {
    ui.spacing_mut().interact_size.y = 12.0;
    ui.spacing_mut().button_padding.y = 1.0;
    ui.style_mut().text_styles.insert(
        egui::TextStyle::Body,
        FontId::monospace(theme::font_size(9.0)),
    );
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBE CALIBRATION");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("KNOWN RING / PIN")
                        .size(theme::font_size(7.5))
                        .color(theme::MUTED),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("CALIBRATION OFFSET")
                    .size(theme::font_size(8.0))
                    .color(theme::MUTED),
            );
            ui.add_sized(
                [76.0, 14.0],
                egui::DragValue::new(&mut app.probe.params.calibration_offset).speed(0.01),
            );
            ui.label(
                RichText::new("mm")
                    .size(theme::font_size(8.0))
                    .color(theme::MUTED),
            );
            if theme::action_button(ui, "RESET CAL", true).clicked() {
                app.send(MachineCommand::ResetProbeCalibration);
                app.toast("probe calibration reset queued", theme::ACCENT);
            }
        });
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(350.0, 230.0),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    for row in CALIBRATE.chunks(2) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for card in row {
                                let texture = app.probe.texture(ui.ctx(), card.operation);
                                if probe_card(
                                    ui,
                                    card,
                                    app.probe.operation == card.operation,
                                    170.0,
                                    104.0,
                                    texture.as_ref(),
                                ) {
                                    app.probe.operation = card.operation.to_string();
                                    app.probe.error = None;
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                },
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("CALIBRATION DIAMETER")
                        .size(theme::font_size(8.0))
                        .color(theme::MUTED),
                );
                ui.add_sized(
                    [96.0, 14.0],
                    egui::DragValue::new(&mut app.probe.params.calibration_diameter).speed(0.1),
                );
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("WIDTH X")
                            .size(theme::font_size(8.0))
                            .color(theme::MUTED),
                    );
                    ui.add_sized(
                        [70.0, 14.0],
                        egui::DragValue::new(&mut app.probe.params.x_cal_width).speed(0.1),
                    );
                    ui.label(
                        RichText::new("Y")
                            .size(theme::font_size(8.0))
                            .color(theme::MUTED),
                    );
                    ui.add_sized(
                        [70.0, 14.0],
                        egui::DragValue::new(&mut app.probe.params.y_cal_width).speed(0.1),
                    );
                });
                ui.add_space(6.0);
                if theme::accent_button(ui, "CAL ON AVG XY ERROR", true).clicked() {
                    super::probe::run(app);
                }
                ui.horizontal(|ui| {
                    if theme::action_button(ui, "CAL ON X ERROR", true).clicked() {
                        super::probe::run(app);
                    }
                    if theme::action_button(ui, "CAL ON Y ERROR", true).clicked() {
                        super::probe::run(app);
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("CALIBRATION OFFSET UPDATES AFTER A RUN.")
                        .size(theme::font_size(7.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("KEYS 1-4 HIGHLIGHT · ENTER RUNS")
                        .size(theme::font_size(7.0))
                        .color(theme::MUTED),
                );
                if let Some(error) = &app.probe.error {
                    ui.label(
                        RichText::new(error)
                            .size(theme::font_size(8.0))
                            .color(theme::ERROR),
                    );
                }
            });
        });
    });
}

fn help_view(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    let height = ui.available_height().max(380.0);
    let right_w = 150.0;
    let gap = 8.0;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new((available - right_w - gap).max(400.0), height),
            egui::Layout::top_down(Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PROBE HELP");
                    ui.add_space(4.0);
                    ui.label(RichText::new("PROBE BASIC ROUTINES").size(theme::font_size(9.0)).strong().color(theme::FG));
                    for line in [
                        "1–9 highlight a routine in the current matrix.",
                        "ENTER runs the highlighted routine.",
                        "PgUp/PgDn cycle the category strip.",
                        "OFFSET advances through the tabs; ESC returns to Offsets.",
                        "The probe is T99; the active WCS is shown in Probing Parameters.",
                    ] {
                        ui.label(RichText::new(line).size(theme::font_size(9.0)).color(theme::MUTED));
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("STEP OFF WIDTH").size(theme::font_size(9.0)).strong().color(theme::FG));
                    ui.label(
                        RichText::new("The probe moves off the measured face by this amount before the next move. Extra probe depth and clearance are applied around the routine.")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            },
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            Vec2::new(right_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| probe_symbol(app, ui, height),
        );
    });
}

pub fn handle_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() || ui.ctx().text_edit_focused() {
        return;
    }
    let cards = cards_for(app.probe.category);
    let mut picked: Option<String> = None;
    let mut run_now = false;
    ui.input_mut(|input| {
        for (key, name) in [
            (egui::Key::Num1, "1"),
            (egui::Key::Num2, "2"),
            (egui::Key::Num3, "3"),
            (egui::Key::Num4, "4"),
            (egui::Key::Num5, "5"),
            (egui::Key::Num6, "6"),
            (egui::Key::Num7, "7"),
            (egui::Key::Num8, "8"),
            (egui::Key::Num9, "9"),
        ] {
            if input.consume_key(egui::Modifiers::NONE, key) {
                if let Some(card) = cards.iter().find(|card| card.key == name) {
                    picked = Some(card.operation.to_string());
                }
            }
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            run_now = true;
        }
    });
    if let Some(operation) = picked {
        app.probe.operation = operation;
        app.probe.error = None;
    }
    if run_now && !cards.is_empty() {
        super::probe::run(app);
    }
}

pub fn enter_first(app: &mut LincApp) {
    app.probe.category = ProbeCategory::OutsideCorners;
    app.probe.operation.clear();
    app.probe.error = None;
}

pub fn category_step(app: &mut LincApp, forward: bool) {
    let index = ProbeCategory::ALL
        .iter()
        .position(|c| *c == app.probe.category)
        .unwrap_or(0);
    let next = if forward {
        (index + 1) % ProbeCategory::ALL.len()
    } else {
        (index + ProbeCategory::ALL.len() - 1) % ProbeCategory::ALL.len()
    };
    app.probe.category = ProbeCategory::ALL[next];
    app.probe.operation.clear();
    app.probe.error = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_visible_category_has_resolvable_unique_cards() {
        for category in ProbeCategory::ALL {
            let cards = cards_for(category);
            let mut keys = std::collections::HashSet::new();
            for card in cards {
                assert!(
                    keys.insert(card.key),
                    "{:?} repeats key {}",
                    category,
                    card.key
                );
                assert!(
                    linc_core::probe::operation(card.operation).is_some(),
                    "{:?} references unknown operation {}",
                    category,
                    card.operation
                );
            }
        }
    }

    #[test]
    fn six_routine_tabs_use_the_bottom_half_of_the_keypad() {
        for category in [ProbeCategory::BossAndPocket, ProbeCategory::RidgeAndValley] {
            let keys: Vec<&str> = cards_for(category).iter().map(|card| card.key).collect();
            assert_eq!(keys, ["4", "5", "6", "1", "2", "3"]);
        }
    }

    #[test]
    fn the_surface_tab_is_gone_but_surface_is_a_centre_key() {
        assert!(!ProbeCategory::ALL.contains(&ProbeCategory::SurfaceZ));
        for category in [
            ProbeCategory::OutsideCorners,
            ProbeCategory::InsideCorners,
            ProbeCategory::EdgeAngle,
        ] {
            assert!(cards_for(category)
                .iter()
                .any(|card| card.key == "5" && card.operation == "surface_z"));
        }
    }
}
