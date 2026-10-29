//! Approved touch-probe page: passive category strip, compact offsets and
//! parameters, the routine matrix, and the probe symbol.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, Vec2,
};
use linc_core::command::{MachineCommand, MdiOrigin};
use linc_core::model::{AxisMap, MachineVariant, ProbeResult};
use linc_core::probe::{ApplySource, ProbeCategory, ProbeMode, ProbeOperation, ProbeParameters};

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
        key: "3",
        label: "ROUND POCKET",
        operation: "cal_round_pocket",
        icon: Icon::CalRoundPocket,
    },
    Card {
        key: "4",
        label: "ROUND BOSS",
        operation: "cal_round_boss",
        icon: Icon::CalRoundBoss,
    },
    Card {
        key: "1",
        label: "SQUARE POCKET",
        operation: "cal_square_pocket",
        icon: Icon::CalSquarePocket,
    },
    Card {
        key: "2",
        label: "SQUARE BOSS",
        operation: "cal_square_boss",
        icon: Icon::CalSquareBoss,
    },
];

/// Height of one routine card for the grid height that is left. A card is a
/// pointer target like every other action, so it never goes below `TARGET`;
/// when the column is too short for that the grid clips its overflow and the
/// Run context above it stays visible.
fn routine_card_height(grid_h: f32, rows: usize, gap: f32) -> f32 {
    ((grid_h - gap * rows.saturating_sub(1) as f32) / rows.max(1) as f32).clamp(TARGET, 132.0)
}

/// Number of routine cards a category shows; used by the target-size tests to
/// prove every card was measured, not just the first.
#[cfg(test)]
pub(crate) fn card_count(category: ProbeCategory) -> usize {
    cards_for(category).len()
}

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

/// Geometry the Probe page painted in the last frame. The 1280x720 layout
/// tests read it to prove the pre-Run context and the click targets fit; it is
/// compiled out of release builds.
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct ProbeLayout {
    /// The page area the screen was given, banners already deducted.
    pub page: egui::Rect,
    /// "ENTER RUN ..." line.
    pub run_action: Option<egui::Rect>,
    /// "RUN UNAVAILABLE: ..." line, when Run is refused.
    pub run_reason: Option<egui::Rect>,
    pub run_reason_text: Option<String>,
    /// The numbered routine grid inside the routines card.
    pub routine_grid: Option<egui::Rect>,
    /// One parameter value field: the click target for Ctrl+P/arrow editing.
    pub parameter_target: Option<egui::Rect>,
    /// The whole left column (DRO, parameters, results) as it was laid out.
    pub left_column: Option<egui::Rect>,
    /// The routines card, whose Run context must stay inside the page.
    pub routines_card: Option<egui::Rect>,
    /// The Calibrate tab's panel, which replaces the routine columns.
    pub calibrate_panel: Option<egui::Rect>,
    /// "ENTER RUN ..." as painted, so a pointer pick can be checked in-frame.
    pub run_action_text: Option<String>,
    /// The routine cards as painted, with the operation each one selects.
    pub routine_cards: Vec<(String, egui::Rect)>,
    /// The hint parameters the card drew under the grid.
    pub hints_drawn: Option<Vec<usize>>,
    /// One DRO tile, one calibration card and one calibration button: the
    /// remaining clickable kinds on the page.
    pub dro_target: Option<egui::Rect>,
    /// The hint fields and their labels, in draw order, so the layout test can
    /// prove the stack shares one label column and one field column.
    pub hint_fields: Vec<(usize, egui::Rect)>,
    pub hint_labels: Vec<(usize, egui::Rect)>,
    /// The calibration parameter fields, in draw order.
    pub calibrate_fields: Vec<(usize, egui::Rect)>,
    pub calibrate_card: Option<egui::Rect>,
    pub calibrate_action: Option<egui::Rect>,
}

#[cfg(test)]
impl Default for ProbeLayout {
    fn default() -> Self {
        Self {
            page: egui::Rect::NOTHING,
            run_action: None,
            run_reason: None,
            run_reason_text: None,
            routine_grid: None,
            parameter_target: None,
            left_column: None,
            routines_card: None,
            calibrate_panel: None,
            run_action_text: None,
            routine_cards: Vec::new(),
            hints_drawn: None,
            dro_target: None,
            hint_fields: Vec::new(),
            hint_labels: Vec::new(),
            calibrate_fields: Vec::new(),
            calibrate_card: None,
            calibrate_action: None,
        }
    }
}

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    #[cfg(test)]
    {
        app.probe_layout = ProbeLayout {
            page: ui.available_rect_before_wrap(),
            ..ProbeLayout::default()
        };
    }
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
    let gap = 4.0;
    let right_w = 150.0;
    let left_w = (available * 0.40).clamp(330.0, 470.0);
    let center_w = (available - left_w - right_w - 2.0 * gap).max(300.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        #[cfg_attr(not(test), allow(unused_variables))]
        let left = ui.allocate_ui_with_layout(
            Vec2::new(left_w, height),
            egui::Layout::top_down(Align::Min),
            |ui| {
                probe_dro(app, ui);
                ui.add_space(3.0);
                parameters_card(app, ui);
                ui.add_space(5.0);
                results_card(app, ui);
            },
        );
        #[cfg(test)]
        {
            app.probe_layout.left_column = Some(left.response.rect);
        }
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

fn probe_dro(app: &mut LincApp, ui: &mut Ui) {
    let gap = 4.0;
    let card_w = ((ui.available_width() - gap * 2.0) / 3.0).max(90.0);
    let mut start: Option<String> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for axis in ["X", "Y", "Z"] {
            let (rect, response) = ui.allocate_exact_size(Vec2::new(card_w, 56.0), Sense::click());
            #[cfg(test)]
            {
                app.probe_layout.dro_target.get_or_insert(rect);
            }
            let editing = app
                .probe
                .dro_edit
                .as_ref()
                .map(|edit| edit.axis == axis)
                .unwrap_or(false);
            common::paint_dro_frame(ui, rect, editing);
            let position = app.snapshot.positions.axes.get(axis);
            let work = position.map(|p| p.work).unwrap_or(0.0);
            let machine = position.map(|p| p.machine).unwrap_or(0.0);
            let painter = ui.painter_at(rect);
            painter.text(
                rect.left_top() + Vec2::new(10.0, 4.0),
                Align2::LEFT_TOP,
                axis,
                FontId::proportional(theme::font_size(9.0)),
                theme::C3,
            );
            if editing {
                let field = Rect::from_min_max(
                    Pos2::new(rect.left() + 10.0, rect.top() + 14.0),
                    Pos2::new(rect.right() - 10.0, rect.top() + 42.0),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(field)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let mut buffer = app
                    .probe
                    .dro_edit
                    .as_ref()
                    .map(|edit| edit.buffer.clone())
                    .unwrap_or_default();
                let edit = child.add_sized(
                    [field.width(), field.height()],
                    egui::TextEdit::singleline(&mut buffer)
                        .font(egui::FontId::monospace(theme::font_size(15.0)))
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(2, 0))
                        .id(egui::Id::unique("linc-probe-dro")),
                );
                if app
                    .probe
                    .dro_edit
                    .as_ref()
                    .map(|edit| edit.focus)
                    .unwrap_or(false)
                {
                    edit.request_focus();
                    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), edit.id)
                    {
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(buffer.chars().count()),
                            )));
                        state.store(ui.ctx(), edit.id);
                    }
                    if let Some(slot) = app.probe.dro_edit.as_mut() {
                        slot.focus = false;
                    }
                }
                if edit.changed() {
                    if let Some(slot) = app.probe.dro_edit.as_mut() {
                        slot.buffer = buffer;
                    }
                }
            } else {
                painter.text(
                    Pos2::new(rect.left() + 10.0, rect.top() + 29.0),
                    Align2::LEFT_CENTER,
                    theme::format_position(work, 3),
                    FontId::monospace(theme::font_size(19.0)),
                    theme::FG,
                );
                if response.clicked() {
                    start = Some(axis.to_string());
                }
            }
            painter.text(
                Pos2::new(rect.left() + 10.0, rect.bottom() - 8.0),
                Align2::LEFT_CENTER,
                format!("M {}", theme::format_position(machine, 3)),
                FontId::monospace(theme::font_size(8.5)),
                theme::MUTED,
            );
        }
    });
    if let Some(axis) = start {
        start_probe_dro_edit(app, axis);
    }
}

/// The rect one card occupied between `top` and the cursor after it drew.
#[cfg(test)]
fn card_span(ui: &Ui, top: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(ui.min_rect().min.x, top),
        Pos2::new(ui.min_rect().max.x, ui.cursor().min.y),
    )
}

fn start_probe_dro_edit(app: &mut LincApp, axis: String) {
    if !dro_visible(app.probe.category) {
        return;
    }
    if app.snapshot.offsets.active_index.is_none() {
        app.toast("no active work offset", theme::WARNING);
        return;
    }
    let work = app
        .snapshot
        .positions
        .axes
        .get(&axis)
        .map(|position| position.work)
        .unwrap_or(0.0);
    app.probe.param_focused = false;
    app.probe.param_editing = false;
    app.probe.param_buffer.clear();
    app.probe.dro_edit = Some(crate::state::DroEdit {
        axis,
        buffer: theme::format_position(work, 3),
        focus: true,
    });
}

fn commit_probe_dro_edit(app: &mut LincApp) -> bool {
    let Some(edit) = app.probe.dro_edit.clone() else {
        return false;
    };
    let Some(value) = crate::state::eval_expression(&edit.buffer) else {
        app.toast("not a number or expression", theme::WARNING);
        return false;
    };
    let Some(index) = app.snapshot.offsets.active_index else {
        app.toast("no active work offset", theme::WARNING);
        return false;
    };
    let mut values = AxisMap::new();
    values.insert(edit.axis.clone(), value);
    app.send(MachineCommand::SetWorkPosition { index, values });
    app.probe.dro_edit = None;
    true
}

fn handle_probe_dro_keys(app: &mut LincApp, ui: &mut Ui) -> bool {
    if app.probe.dro_edit.is_none() {
        return false;
    }
    let mut commit = false;
    let mut cancel = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            commit = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            cancel = true;
        }
    });
    if commit {
        commit_probe_dro_edit(app);
    } else if cancel {
        app.probe.dro_edit = None;
    }
    true
}

fn handle_probe_dro_axis_keys(app: &mut LincApp, ui: &mut Ui) -> bool {
    if !dro_visible(app.probe.category) || app.probe.param_editing || app.probe.dro_edit.is_some() {
        return false;
    }
    let mut axis = None;
    ui.input_mut(|input| {
        for (name, key) in [
            ("X", egui::Key::X),
            ("Y", egui::Key::Y),
            ("Z", egui::Key::Z),
        ] {
            if input.consume_key(egui::Modifiers::NONE, key) {
                axis = Some(name.to_string());
            }
        }
    });
    if let Some(axis) = axis {
        start_probe_dro_edit(app, axis);
        true
    } else {
        false
    }
}

fn parameters_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 16.0;
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.spacing_mut().button_padding = Vec2::new(4.0, 1.0);
        ui.style_mut().text_styles.insert(
            egui::TextStyle::Body,
            FontId::monospace(theme::font_size(9.0)),
        );
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.bg_fill = theme::PANEL_RAISED;
        widgets.inactive.bg_stroke = Stroke::new(1.0, theme::BORDER);
        widgets.inactive.corner_radius = CornerRadius::same(2);
        widgets.hovered.bg_fill = theme::PANEL_RAISED;
        widgets.hovered.bg_stroke = Stroke::new(1.0, theme::ACCENT);
        widgets.hovered.corner_radius = CornerRadius::same(2);
        widgets.active.bg_fill = theme::PANEL_RAISED;
        widgets.active.bg_stroke = Stroke::new(1.0, theme::ACCENT);
        widgets.active.corner_radius = CornerRadius::same(2);
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROBING PARAMETERS");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("WCS {}", app.probe.wcs))
                        .monospace()
                        .size(theme::font_size(8.0))
                        .color(theme::SUCCESS),
                );
                ui.add_space(10.0);
                let direct = app.probe.params.probe_mode == ProbeMode::ApplyWcs;
                ui.label(
                    RichText::new(if direct { "W DIRECT WCO" } else { "W POSITION" })
                        .monospace()
                        .size(theme::font_size(8.5))
                        .strong()
                        .color(if direct {
                            theme::WARNING
                        } else {
                            theme::ACCENT
                        }),
                );
                if app.probe.category == ProbeCategory::EdgeAngle {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!(
                            "R ROTATION {}",
                            if app.probe.params.wcs_rotation {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ))
                        .monospace()
                        .size(theme::font_size(8.0))
                        .color(theme::ACCENT),
                    );
                }
            });
        });
        ui.add_space(2.0);
        // Four columns of three, the name inside each field: every parameter
        // value keeps a full target-sized box at 1280x720 without scrolling.
        let column_gap = 6.0;
        let columns = PARAM_COLUMNS as usize;
        let cell_w =
            ((ui.available_width() - column_gap * (columns - 1) as f32) / columns as f32).max(56.0);
        for row in 0..3 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = column_gap;
                for col in 0..columns {
                    let slot = row * columns + col;
                    if let Some(index) = COMMON_PARAMS.get(slot).copied() {
                        param_cell(ui, app, index, cell_w);
                    } else {
                        read_only_field(
                            ui,
                            &format_param_number(app.profile.config.probe.probe_diameter),
                            cell_w,
                            Some("STYLUS DIAM #5410"),
                        );
                    }
                }
            });
        }
        if let Some(error) = app
            .probe
            .error
            .as_deref()
            .or(app.snapshot.probe.error.as_deref())
        {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(8.0))
                    .color(theme::ERROR),
            );
        }
    });
}

/// Labels in absolute Probe Basic value order: index 0 is #3014 and index 22
/// is #3036. The two machine/controller-owned values remain read-only.
pub(crate) const PARAM_LABELS: [&str; 23] = [
    "PROBE TOOL",
    "PROBE SLOW FDRATE",
    "PROBE FAST FDRATE",
    "PROBE TRAVERSE FR",
    "MAX X/Y DISTANCE",
    "X/Y CLEARANCE",
    "MAX Z DISTANCE",
    "Z CLEARANCE",
    "EXTRA PROBE DEPTH",
    "STEP OFF WIDTH",
    "EDGE WIDTH",
    "BOSS/POCKET DIAM",
    "BOSS/POCKET X WIDTH",
    "BOSS/POCKET Y WIDTH",
    "RIDGE/VALLEY X WIDTH",
    "RIDGE/VALLEY Y WIDTH",
    "WCO MODE",
    "WCS ROTATION",
    "CALIBRATION OFFSET",
    "CALIBRATION DIAMETER",
    "CALIBRATION WIDTH X",
    "CALIBRATION WIDTH Y",
    "CALIBRATION AXIS",
];

/// Visual order in the common two-column card. #3025 lives with the other
/// operation-specific hints instead of being mislabeled as stylus diameter.
pub(crate) const COMMON_PARAMS: &[usize] = &[0, 1, 3, 2, 4, 5, 6, 7, 8, 9, 10];

#[cfg(test)]
fn parameter_number(index: usize) -> u16 {
    3014 + index as u16
}

fn parameter_value_editable(index: usize) -> bool {
    matches!(index, 1..=15 | 19..=21)
}

fn format_param_number(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        format!("{value:.1}")
    } else {
        let mut text = format!("{value:.3}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.push('0');
        }
        text
    }
}

fn param_text(params: &ProbeParameters, index: usize) -> String {
    let value = match index {
        0 => return params.probe_tool_number.to_string(),
        1 => params.slow_feed,
        2 => params.fast_feed,
        3 => params.traverse_feed,
        4 => params.max_xy_distance,
        5 => params.xy_clearance,
        6 => params.max_z_distance,
        7 => params.z_clearance,
        8 => params.extra_depth,
        9 => params.step_off_width,
        10 => params.edge_width,
        11 => params.diameter_hint,
        12 => params.x_hint_boss_pocket,
        13 => params.y_hint_boss_pocket,
        14 => params.x_hint_ridge_valley,
        15 => params.y_hint_ridge_valley,
        16 => {
            return if params.probe_mode == ProbeMode::ApplyWcs {
                "DIRECT WCO".into()
            } else {
                "POSITION".into()
            };
        }
        17 => {
            return if params.wcs_rotation {
                "ON".into()
            } else {
                "OFF".into()
            }
        }
        18 => return format!("{:.4}", params.calibration_offset),
        19 => params.calibration_diameter,
        20 => params.x_cal_width,
        21 => params.y_cal_width,
        22 => return calibration_axis_label(params.square_cal_axis).into(),
        _ => return "—".into(),
    };
    format_param_number(value)
}

fn hint_parameters(category: ProbeCategory, operation: Option<&ProbeOperation>) -> Vec<usize> {
    match category {
        ProbeCategory::BossAndPocket => match operation {
            Some(operation) if operation.call.contains("round") => vec![11],
            Some(_) => vec![12, 13],
            None => vec![11, 12, 13],
        },
        ProbeCategory::RidgeAndValley => match operation {
            Some(operation) if operation.call.ends_with("_y") || operation.call.contains("_y_") => {
                vec![15]
            }
            Some(_) => vec![14],
            None => vec![14, 15],
        },
        ProbeCategory::Calibrate => vec![19, 20, 21],
        _ => Vec::new(),
    }
}

/// One label column wide enough for the longest visible hint. A narrower
/// column would wrap or clip a label, and the fields would no longer share one
/// left edge.
fn hint_label_width(ui: &Ui, hints: &[usize]) -> f32 {
    let font = FontId::monospace(theme::font_size(7.5));
    let width = hints
        .iter()
        .map(|index| {
            ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(hint_name(*index).to_string(), font.clone(), theme::MUTED)
                    .size()
                    .x
            })
        })
        .fold(0.0, f32::max);
    width.max(82.0) + 4.0
}

fn editor_parameters_for(
    category: ProbeCategory,
    operation: Option<&ProbeOperation>,
) -> Vec<usize> {
    if category == ProbeCategory::Calibrate {
        return hint_parameters(category, operation);
    }
    let mut indices: Vec<usize> = COMMON_PARAMS
        .iter()
        .copied()
        .filter(|index| parameter_value_editable(*index))
        .collect();
    indices.extend(hint_parameters(category, operation));
    indices
}

fn editor_parameters(app: &LincApp) -> Vec<usize> {
    editor_parameters_for(app.probe.category, app.probe.operation())
}

/// The field `Ctrl+P` selects. Boss + Pocket and Ridge + Valley put their
/// operation-specific hints in the routine pane, so the shortcut starts there;
/// every other tab starts at the first common parameter.
fn default_parameter(category: ProbeCategory, operation: Option<&ProbeOperation>) -> Option<usize> {
    let hint_first = matches!(
        category,
        ProbeCategory::BossAndPocket | ProbeCategory::RidgeAndValley
    );
    if hint_first {
        if let Some(index) = hint_parameters(category, operation).first() {
            return Some(*index);
        }
    }
    editor_parameters_for(category, operation).first().copied()
}

/// Height of one pointer target, the minimum SPEC §5 requires at 1280x720.
const TARGET: f32 = theme::MIN_TARGET;
/// Columns in the parameters grid; Up/Down move one row of this many fields.
const PARAM_COLUMNS: isize = 4;
/// Vertical gap between the stacked routine-hint rows.
const HINT_ROW_GAP: f32 = 3.0;
/// Label column and the gap before a calibration field.
const CALIBRATION_LABEL_WIDTH: f32 = 128.0;
const CALIBRATION_LABEL_GAP: f32 = 16.0;

/// Fields per row in the grid this category draws: the Calibrate panel lists
/// its three dimensions vertically, the main card uses the parameter grid.
fn param_row_step(category: ProbeCategory) -> isize {
    if category == ProbeCategory::Calibrate {
        1
    } else {
        PARAM_COLUMNS
    }
}

fn param_label(ui: &mut Ui, text: &str) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(CALIBRATION_LABEL_WIDTH, TARGET), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::monospace(theme::font_size(7.2)),
        theme::MUTED,
    );
}

/// One parameter value: a target-sized field with its name inside it, so each
/// value owns its rect and no separate label row is needed.
fn param_cell(ui: &mut Ui, app: &mut LincApp, index: usize, width: f32) {
    let text = param_text(&app.probe.params, index);
    param_field_sized(ui, app, index, &text, width, Some(PARAM_LABELS[index]));
}

fn read_only_field(ui: &mut Ui, text: &str, width: f32, caption: Option<&str>) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, TARGET), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), theme::PANEL_RAISED);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    let value_center = match caption {
        Some(caption) => {
            ui.painter().text(
                Pos2::new(rect.left() + 5.0, rect.top() + 9.0),
                Align2::LEFT_CENTER,
                caption,
                FontId::monospace(9.0),
                theme::BORDER,
            );
            Pos2::new(rect.center().x, rect.top() + 28.0)
        }
        None => rect.center(),
    };
    ui.painter().text(
        value_center,
        Align2::CENTER_CENTER,
        text,
        FontId::monospace(theme::font_size(9.0)),
        theme::MUTED,
    );
    rect
}

fn param_field(ui: &mut Ui, app: &mut LincApp, index: usize, text: &str) -> Rect {
    param_field_sized(ui, app, index, text, 64.0, None)
}

/// `caption` draws the parameter name inside the field's top edge, which is how
/// the parameters grid keeps a full target-sized box per value without a
/// separate label row.
fn param_field_sized(
    ui: &mut Ui,
    app: &mut LincApp,
    index: usize,
    text: &str,
    width: f32,
    caption: Option<&str>,
) -> Rect {
    let editable = parameter_value_editable(index);
    let selected = editable && app.probe.param_focused && app.probe.param_selected == index;
    let editing = selected && app.probe.param_editing;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, TARGET), Sense::click());
    #[cfg(test)]
    {
        app.probe_layout.parameter_target.get_or_insert(rect);
    }
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), theme::PANEL_RAISED);
    let border = if editing {
        Stroke::new(1.5, theme::ERROR)
    } else if selected {
        Stroke::new(1.5, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::BORDER)
    };
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(2), border, StrokeKind::Inside);
    let value_center = match caption {
        Some(caption) => {
            ui.painter().text(
                Pos2::new(rect.left() + 5.0, rect.top() + 9.0),
                Align2::LEFT_CENTER,
                caption,
                FontId::monospace(9.0),
                if editable {
                    theme::MUTED
                } else {
                    theme::BORDER
                },
            );
            Pos2::new(rect.center().x, rect.top() + 28.0)
        }
        None => rect.center(),
    };
    if editing {
        // A target-sized field holds a single-line editor band, not a 44 px
        // text box: the caret stays on the field's centre line.
        let inner = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - 6.0, 20.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let mut buffer = std::mem::take(&mut app.probe.param_buffer);
        let response = child.add_sized(
            [inner.width(), inner.height()],
            egui::TextEdit::singleline(&mut buffer)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(2, 0))
                .font(egui::FontId::monospace(theme::font_size(9.0)))
                .id(egui::Id::unique("linc-probe-param")),
        );
        if app.probe.param_edit_focus {
            response.request_focus();
            if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), response.id) {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(buffer.chars().count()),
                    )));
                state.store(ui.ctx(), response.id);
            }
            app.probe.param_edit_focus = false;
        }
        app.probe.param_buffer = buffer;
    } else {
        ui.painter().text(
            value_center,
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(theme::font_size(9.0)),
            if editable { theme::FG } else { theme::MUTED },
        );
        if editable && response.clicked() {
            app.probe.param_selected = index;
            app.probe.param_focused = true;
            app.probe.param_editing = false;
            app.probe.param_buffer.clear();
        }
    }
    rect
}

fn results_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        // The result grid is the dense part of the card: one 44 px target per
        // row would carry the left column past the 1280x720 page.
        ui.spacing_mut().interact_size.y = 12.0;
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.style_mut().text_styles.insert(
            egui::TextStyle::Body,
            FontId::monospace(theme::font_size(9.0)),
        );
        theme::section_header(ui, "PROBE RESULTS");
        let result = app.snapshot.probe.last_result.as_ref();
        let units = if app.snapshot.positions.units.trim().is_empty() {
            "—"
        } else {
            app.snapshot.positions.units.as_str()
        };
        ui.label(
            RichText::new(match result {
                Some(result) => format!(
                    "{} · {} · {} · {units}/deg",
                    result.routine, result.wcs, result.mode
                ),
                None => format!("NO RESULT · {units}/deg"),
            })
            .size(theme::font_size(7.5))
            .color(theme::MUTED),
        );
        let apply_sources = result.map(apply_source_parameters).unwrap_or_default();
        let relevant = result
            .and_then(|result| linc_core::probe::operation(&result.operation))
            .map(|operation| {
                operation
                    .result_parameters()
                    .into_iter()
                    .collect::<std::collections::HashSet<u16>>()
            })
            .unwrap_or_default();
        egui::Grid::new("probe-results-image-grid")
            .num_columns(6)
            .spacing([4.0, 2.0])
            .show(ui, |ui| {
                for fields in [
                    [(1005, "X- PROBED"), (1006, "X+ PROBED"), (1001, "X WIDTH")],
                    [(1007, "Y- PROBED"), (1008, "Y+ PROBED"), (1003, "Y WIDTH")],
                    [(1009, "Z- PROBED"), (1010, "DIAM"), (1002, "X CENTER")],
                    [
                        (1011, "EDGE DELTA"),
                        (1012, "EDGE ANGLE °"),
                        (1004, "Y CENTER"),
                    ],
                ]
                .iter()
                {
                    for (parameter, label) in fields {
                        result_field(
                            ui,
                            label,
                            result.and_then(|result| result.value(*parameter)),
                            relevant.contains(parameter),
                            apply_sources.contains(parameter),
                        );
                    }
                    ui.end_row();
                }
            });
        apply_action_context(app, ui);
    });
}

fn apply_source_parameters(result: &ProbeResult) -> std::collections::HashSet<u16> {
    let mut parameters = std::collections::HashSet::new();
    if let Some(operation) = linc_core::probe::operation(&result.operation) {
        for term in operation.apply_terms() {
            match term.source {
                ApplySource::Slot(parameter) => {
                    parameters.insert(parameter);
                }
                ApplySource::SlotMinusDelta(parameter) | ApplySource::SlotPlusDelta(parameter) => {
                    parameters.insert(parameter);
                    parameters.insert(1011);
                }
                _ => {}
            }
        }
    }
    parameters
}

fn apply_action_context(app: &LincApp, ui: &mut Ui) {
    let active = app.active_wcs_label();
    let Some(result) = app.snapshot.probe.last_result.as_ref() else {
        return;
    };
    let action = format!("A APPLY {} -> {active}", result.routine);
    let reason = super::probe::apply_block_reason(app);
    ui.label(
        RichText::new(action)
            .size(theme::font_size(8.0))
            .strong()
            .color(if reason.is_some() {
                theme::MUTED
            } else {
                theme::ACCENT
            }),
    );
    if let Some(reason) = reason {
        ui.label(
            RichText::new(format!("APPLY UNAVAILABLE: {reason}"))
                .size(theme::font_size(7.5))
                .color(theme::WARNING),
        );
    }
}

#[derive(Clone, Copy)]
enum ProbeDataReset {
    All,
    X,
    Y,
}

fn update_probe_params(app: &mut LincApp) {
    app.probe.error = None;
    app.sync_probe_calibration();
    let validation = match app.probe.operation() {
        Some(operation) => app.probe.params.validate_for_operation(
            app.snapshot.probe.enabled,
            app.snapshot.tool_change.current_tool,
            operation,
        ),
        None => app.probe.params.validate(
            app.snapshot.probe.enabled,
            app.snapshot.tool_change.current_tool,
        ),
    };
    if let Err(error) = validation {
        app.probe.error = Some(error);
        return;
    }
    app.remember_probe_params();
    let block = app.probe.params.parameter_update_block();
    issue_probe_mdi(app, block, "probe parameters updated");
}

fn reset_probe_data(app: &mut LincApp, reset: ProbeDataReset) {
    let (block, toast) = match reset {
        ProbeDataReset::All => ("o<reset_all_data> call", "probe data reset"),
        ProbeDataReset::X => ("o<x_data_reset> call", "probe X data reset"),
        ProbeDataReset::Y => ("o<y_data_reset> call", "probe Y data reset"),
    };
    issue_probe_mdi(app, block.into(), toast);
}

fn result_field(ui: &mut Ui, label: &str, value: Option<f64>, relevant: bool, apply_source: bool) {
    ui.add_sized(
        [58.0, 16.0],
        egui::Label::new(
            RichText::new(label)
                .size(theme::font_size(7.5))
                .color(if relevant { theme::FG } else { theme::MUTED }),
        ),
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(64.0, 16.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), theme::PANEL_RAISED);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(
            if apply_source { 1.5 } else { 1.0 },
            if apply_source {
                theme::ACCENT
            } else {
                theme::BORDER
            },
        ),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        value
            .map(|value| format!("{value:.4}"))
            .unwrap_or_else(|| "—".into()),
        FontId::monospace(theme::font_size(9.0)),
        if value.is_some() {
            if apply_source {
                theme::ACCENT
            } else {
                theme::FG
            }
        } else {
            theme::MUTED
        },
    );
}

/// Issue one explicit Probe MDI block. The block is classified by the shared
/// core classifier, so a protected action is refused before anything is issued
/// and the operator always sees why.
pub(crate) fn issue_probe_mdi(app: &mut LincApp, block: String, toast: &str) {
    app.probe.error = None;
    if let Err(error) = app.snapshot.issue_allowed() {
        app.probe.error = Some(error);
        return;
    }
    if let Some(reason) = super::probe::mdi_write_block_reason(app, &block) {
        app.probe.error = Some(reason);
        return;
    }
    app.send(MachineCommand::IssueMdi {
        block,
        origin: MdiOrigin::Probe,
    });
    app.toast(toast, theme::ACCENT);
}

/// Select one routine. Every entry point goes through this so a pick always
/// closes the parameter editor and clears the last error.
fn select_routine(app: &mut LincApp, operation: &str) {
    app.probe.operation = operation.to_string();
    app.probe.param_focused = false;
    app.probe.param_editing = false;
    app.probe.param_buffer.clear();
    app.probe.error = None;
}

/// Where the routine cards were painted last frame, so a pointer click can pick
/// one before this frame derives the selected routine, hints and Run context.
#[derive(Clone)]
struct CardHits {
    column: Rect,
    cards: Vec<(String, Rect)>,
}

impl Default for CardHits {
    fn default() -> Self {
        Self {
            column: Rect::NOTHING,
            cards: Vec::new(),
        }
    }
}

fn card_hits_id() -> egui::Id {
    egui::Id::unique("linc-probe-card-hits")
}

/// Apply a click on a routine card to the selection before anything is derived
/// from it, so the card, the SELECTED line, the hint row and the Run context
/// agree in the frame the click lands. The column rect guards against a resize
/// between the frame that recorded the rects and the one that reads them.
fn apply_pointer_pick(app: &mut LincApp, ui: &Ui) {
    let Some(position) = ui.input(|input| input.pointer.interact_pos()) else {
        return;
    };
    if !ui.input(|input| input.pointer.any_click()) {
        return;
    }
    let column = ui.max_rect();
    let Some(hits) = ui.data(|data| data.get_temp::<CardHits>(card_hits_id())) else {
        return;
    };
    if hits.column != column {
        return;
    }
    if let Some((operation, _)) = hits.cards.iter().find(|(_, rect)| rect.contains(position)) {
        select_routine(app, operation);
    }
}

fn record_card_hits(ui: &Ui, column: Rect, cards: Vec<(String, Rect)>) {
    ui.data_mut(|data| {
        data.insert_temp(card_hits_id(), CardHits { column, cards });
    });
}

fn matrix_card(app: &mut LincApp, ui: &mut Ui, height: f32) {
    #[cfg_attr(not(test), allow(unused_variables))]
    let card_top = ui.cursor().min.y;
    // A pointer pick is applied before the selection is read, so the card, the
    // SELECTED line, the hints and the Run context all show the new routine in
    // the frame the click lands.
    apply_pointer_pick(app, ui);
    let cards = cards_for(app.probe.category);
    let hints = hint_parameters(app.probe.category, app.probe.operation());
    let column = ui.max_rect();
    theme::card(ui, |ui| {
        // Everything below is laid out against this budget, so the Run context
        // and the grid together cannot grow past the column a banner left.
        let budget = (height - 24.0).max(0.0);
        ui.set_min_height(budget);
        let inner_top = ui.cursor().min.y;
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
        let selected = app
            .probe
            .operation()
            .map(|operation| operation.name)
            .unwrap_or("— SELECT A NUMBERED ROUTINE");
        ui.label(
            RichText::new(format!("SELECTED  {selected}"))
                .size(theme::font_size(8.0))
                .color(theme::FG),
        );
        ui.label(
            RichText::new(format!(
                "LAST RUN  {} · RESULT  {}",
                last_run_name(app),
                app.snapshot
                    .probe
                    .last_result
                    .as_ref()
                    .map(|result| result.routine.as_str())
                    .unwrap_or("—")
            ))
            .size(theme::font_size(7.5))
            .color(theme::MUTED),
        );
        ui.add_space(2.0);
        // The pre-Run action and the reason it is unavailable are placed before
        // the grid, and the grid is sized from what is left: a banner at
        // 1280x720 can only shrink the routine cards, never clip the reason.
        run_action_context(app, ui);
        ui.add_space(2.0);
        let gap = 6.0;
        let card_w = ((ui.available_width() - gap * 2.0) / 3.0).max(88.0);
        let rows = cards.len().div_ceil(3);
        let hint_h = if hints.is_empty() {
            0.0
        } else {
            hints.len() as f32 * TARGET + (hints.len() - 1) as f32 * HINT_ROW_GAP
        };
        let consumed = ui.cursor().min.y - inner_top;
        let grid_h = (budget - consumed - hint_h).max(0.0);
        let card_h = routine_card_height(grid_h, rows, gap);
        // The grid owns exactly what is left and clips its own overflow, so a
        // narrower window shrinks the cards instead of pushing the Run context
        // off the card.
        let grid_rect =
            Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), grid_h));
        let mut grid = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(grid_rect)
                .layout(egui::Layout::top_down(Align::Min)),
        );
        grid.spacing_mut().item_spacing.y = 0.0;
        grid.set_clip_rect(grid_rect.intersect(ui.clip_rect()));
        let mut hits: Vec<(String, Rect)> = Vec::with_capacity(cards.len());
        for row in 0..rows {
            if row > 0 {
                grid.add_space(gap);
            }
            grid.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..3 {
                    let index = row * 3 + col;
                    if let Some(card) = cards.get(index) {
                        let texture = app.probe.texture(ui.ctx(), card.operation);
                        let (clicked, rect) = probe_card(
                            ui,
                            card,
                            app.probe.operation == card.operation,
                            card_w,
                            card_h,
                            texture.as_ref(),
                        );
                        hits.push((card.operation.to_string(), rect));
                        if clicked {
                            select_routine(app, card.operation);
                        }
                    } else {
                        ui.allocate_space(Vec2::new(card_w, card_h));
                    }
                }
            });
        }
        ui.advance_cursor_after_rect(grid_rect);
        record_card_hits(ui, column, hits.clone());
        #[cfg(test)]
        {
            app.probe_layout.routine_grid = Some(grid_rect);
            app.probe_layout.routine_cards = hits;
        }
        if !hints.is_empty() {
            #[cfg(test)]
            {
                app.probe_layout.hints_drawn = Some(hints.clone());
            }
            let label_w = hint_label_width(ui, &hints);
            let field_w = (ui.available_width() - label_w - 8.0).clamp(58.0, 140.0);
            let field_left = ((ui.available_width() - field_w) * 0.5).max(0.0);
            let row_left = (field_left - label_w - 8.0).max(0.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.spacing_mut().item_spacing.y = HINT_ROW_GAP;
                for index in &hints {
                    ui.horizontal(|ui| {
                        ui.add_space(row_left);
                        #[cfg_attr(not(test), allow(unused_variables))]
                        let label = ui.add_sized(
                            [label_w, TARGET],
                            egui::Label::new(
                                RichText::new(hint_name(*index))
                                    .monospace()
                                    .size(theme::font_size(7.5))
                                    .color(theme::MUTED),
                            ),
                        );
                        #[cfg(test)]
                        {
                            app.probe_layout.hint_labels.push((*index, label.rect));
                        }
                        let text = param_text(&app.probe.params, *index);
                        #[cfg_attr(not(test), allow(unused_variables))]
                        let field = param_field_sized(ui, app, *index, &text, field_w, None);
                        #[cfg(test)]
                        {
                            app.probe_layout.hint_fields.push((*index, field));
                        }
                    });
                }
            });
        }
    });
    #[cfg(test)]
    {
        app.probe_layout.routines_card = Some(card_span(ui, card_top));
    }
}

fn hint_name(index: usize) -> &'static str {
    match index {
        11 => "B/P DIAM",
        12 | 14 => "X WIDTH",
        13 | 15 => "Y WIDTH",
        19 => "KNOWN DIAM",
        20 => "KNOWN X WIDTH",
        21 => "KNOWN Y WIDTH",
        _ => "HINT",
    }
}

fn last_run_name(app: &LincApp) -> String {
    app.probe
        .last_run_operation
        .as_deref()
        .or(app.snapshot.probe.last_routine.as_deref())
        .and_then(linc_core::probe::operation)
        .map(|operation| operation.name.into())
        .or_else(|| {
            app.snapshot
                .probe
                .last_result
                .as_ref()
                .map(|result| result.routine.clone())
        })
        .unwrap_or_else(|| "—".into())
}

fn run_action_context(app: &mut LincApp, ui: &mut Ui) {
    let operation = app.probe.operation();
    let action = operation
        .map(|operation| format!("ENTER RUN {}", operation.name))
        .unwrap_or_else(|| "ENTER RUN —".into());
    let reason = match operation {
        Some(_) => super::probe::run_block_reason(app),
        None => Some("select a numbered routine".into()),
    };
    #[cfg(test)]
    {
        app.probe_layout.run_action_text = Some(action.clone());
    }
    #[cfg_attr(not(test), allow(unused_variables))]
    let action_response = ui.label(
        RichText::new(action)
            .size(theme::font_size(8.5))
            .strong()
            .color(if reason.is_some() {
                theme::MUTED
            } else {
                theme::ACCENT
            }),
    );
    #[cfg(test)]
    {
        app.probe_layout.run_action = Some(action_response.rect);
    }
    if let Some(reason) = reason {
        #[cfg_attr(not(test), allow(unused_variables))]
        let reason_response = ui.label(
            RichText::new(format!("RUN UNAVAILABLE: {reason}"))
                .size(theme::font_size(7.5))
                .color(theme::WARNING),
        );
        #[cfg(test)]
        {
            app.probe_layout.run_reason = Some(reason_response.rect);
            app.probe_layout.run_reason_text = Some(format!("RUN UNAVAILABLE: {reason}"));
        }
    }
    if let Some(warning) = run_warning(app) {
        ui.label(
            RichText::new(warning)
                .size(theme::font_size(7.5))
                .color(theme::WARNING),
        );
    }
}

/// What this Run does to the controller beyond measuring. It sits with the Run
/// action, so the operator reads it where Enter is pressed rather than in the
/// parameter card.
fn run_warning(app: &LincApp) -> Option<&'static str> {
    let operation = app.probe.operation();
    if operation.is_some_and(|operation| operation.calibration) {
        return None;
    }
    if app.probe.category == ProbeCategory::EdgeAngle {
        Some("EVERY ANGLE RUN CLEARS ACTIVE WCS ROTATION WITH R0, EVEN IN POSITION MODE.")
    } else if app.probe.params.probe_mode == ProbeMode::ApplyWcs {
        Some("DIRECT WCO WRITES THE ACTIVE WORK OFFSET AT RUN.")
    } else {
        None
    }
}

fn probe_card(
    ui: &mut Ui,
    card: &Card,
    selected: bool,
    width: f32,
    height: f32,
    texture: Option<&egui::TextureHandle>,
) -> (bool, Rect) {
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
    let tile_side = (width - 18.0).min(height - 40.0).clamp(30.0, 84.0);
    let tile = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.top() + 8.0 + tile_side / 2.0),
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
        FontId::monospace(theme::font_size(9.5)),
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
        FontId::proportional(theme::font_size(9.0)),
        if selected { theme::FG } else { theme::MUTED },
    );
    (response.clicked(), rect)
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
            ui.spacing_mut().item_spacing.x = 6.0;
            theme::section_header(ui, "PROBE");
            ui.label(
                RichText::new(format!("T{}", app.snapshot.tool_change.current_tool))
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
        let input_state = match app.snapshot.probe.input_tripped {
            Some(true) => "TRIPPED",
            Some(false) => "READY",
            None => "UNKNOWN",
        };
        let led = if app.snapshot.probe.running {
            theme::WARNING
        } else if app.snapshot.probe.error.is_some() {
            theme::ERROR
        } else {
            match app.snapshot.probe.input_tripped {
                Some(true) => theme::WARNING,
                Some(false) => theme::SUCCESS,
                None => theme::MUTED,
            }
        };
        // The LED is on the probe body, not floating beside it.
        let led_at = Pos2::new(c.x, c.y - 2.0);
        p.circle_filled(led_at, 5.0, led.linear_multiply(0.18));
        p.circle_filled(led_at, 3.0, led);
        p.text(
            Pos2::new(rect.left() + 10.0, rect.bottom() - 10.0),
            Align2::LEFT_CENTER,
            format!("PROBE INPUT {input_state}"),
            FontId::monospace(theme::font_size(7.0)),
            led,
        );
    });
}

fn calibrate_view(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    let height = ui.available_height().max(380.0);
    let right_w = 150.0;
    let gap = 4.0;
    let panel_w = (available - right_w - gap).max(400.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
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

fn calibration_axis_label(axis: u8) -> &'static str {
    match axis {
        0 => "AVG XY",
        1 => "X",
        2 => "Y",
        _ => "INVALID",
    }
}

fn next_calibration_axis(axis: u8) -> u8 {
    if axis < 2 {
        axis + 1
    } else {
        0
    }
}

fn set_calibration_axis(app: &mut LincApp, axis: u8) {
    let axis = axis.min(2);
    if app.probe.params.square_cal_axis != axis {
        app.probe.params.square_cal_axis = axis;
        app.probe.error = None;
        app.remember_probe_params();
    }
}

fn run_calibration(app: &mut LincApp, axis: u8) {
    set_calibration_axis(app, axis);
    super::probe::run(app);
}

fn reset_calibration(app: &mut LincApp) {
    app.probe.error = None;
    if let Err(error) = app.snapshot.issue_allowed() {
        app.probe.error = Some(error);
    } else if let Some(reason) = super::probe::calibration_reset_block_reason(app) {
        app.probe.error = Some(reason);
    } else {
        app.send(MachineCommand::ResetProbeCalibration);
        app.toast("probe calibration reset queued", theme::ACCENT);
    }
}

fn calibration_result_readout(app: &LincApp, ui: &mut Ui) {
    const SLOTS: [(u16, &str); 10] = [
        (1001, "X WIDTH"),
        (1002, "X CENTER"),
        (1003, "Y WIDTH"),
        (1004, "Y CENTER"),
        (1005, "X-"),
        (1006, "X+"),
        (1007, "Y-"),
        (1008, "Y+"),
        (1009, "Z"),
        (1010, "AVG DIAM"),
    ];
    let Some(result) = app.snapshot.probe.last_result.as_ref() else {
        return;
    };
    let Some(operation) = linc_core::probe::operation(&result.operation)
        .filter(|operation| operation.category == ProbeCategory::Calibrate)
    else {
        return;
    };
    let relevant: std::collections::HashSet<u16> =
        operation.result_parameters().into_iter().collect();
    ui.label(
        RichText::new(format!(
            "CAL RESULT {} · {} · {}/deg",
            result.routine, result.mode, app.snapshot.positions.units
        ))
        .size(theme::font_size(7.5))
        .color(theme::MUTED),
    );
    egui::Grid::new("calibration-result-grid")
        .num_columns(6)
        .spacing([4.0, 3.0])
        .show(ui, |ui| {
            for row in SLOTS.chunks(3) {
                for (parameter, label) in row {
                    result_field(
                        ui,
                        label,
                        result.value(*parameter),
                        relevant.contains(parameter),
                        false,
                    );
                }
                ui.end_row();
            }
        });
}

fn calibration_panel(app: &mut LincApp, ui: &mut Ui) {
    #[cfg_attr(not(test), allow(unused_variables))]
    let card_top = ui.cursor().min.y;
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
                    RichText::new("KNOWN RING / PIN · ALWAYS POSITION · NO APPLY")
                        .size(theme::font_size(7.5))
                        .color(theme::MUTED),
                );
            });
        });
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(390.0, 276.0),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    for row in CALIBRATE.chunks(2) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for card in row {
                                let texture = app.probe.texture(ui.ctx(), card.operation);
                                #[cfg_attr(not(test), allow(unused_variables))]
                                let (clicked, rect) = probe_card(
                                    ui,
                                    card,
                                    app.probe.operation == card.operation,
                                    190.0,
                                    128.0,
                                    texture.as_ref(),
                                );
                                #[cfg(test)]
                                {
                                    app.probe_layout.calibrate_card.get_or_insert(rect);
                                }
                                if clicked {
                                    select_routine(app, card.operation);
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                },
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    param_label(ui, "CALIBRATION OFFSET");
                    ui.add_space(CALIBRATION_LABEL_GAP);
                    #[cfg_attr(not(test), allow(unused_variables))]
                    let field = read_only_field(ui, &param_text(&app.probe.params, 18), 64.0, None);
                    #[cfg(test)]
                    {
                        app.probe_layout.calibrate_fields.push((18, field));
                    }
                });
                for index in [19, 20, 21] {
                    ui.horizontal(|ui| {
                        param_label(ui, PARAM_LABELS[index]);
                        ui.add_space(CALIBRATION_LABEL_GAP);
                        let text = param_text(&app.probe.params, index);
                        #[cfg_attr(not(test), allow(unused_variables))]
                        let field = param_field(ui, app, index, &text);
                        #[cfg(test)]
                        {
                            app.probe_layout.calibrate_fields.push((index, field));
                        }
                    });
                }
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!(
                        "V CAL AXIS: {}",
                        calibration_axis_label(app.probe.params.square_cal_axis)
                    ))
                    .size(theme::font_size(8.5))
                    .strong()
                    .color(theme::ACCENT),
                );
                let calibrate = theme::accent_button(ui, "CAL ON AVG XY ERROR", true);
                #[cfg(test)]
                {
                    app.probe_layout
                        .calibrate_action
                        .get_or_insert(calibrate.rect);
                }
                if calibrate.clicked() {
                    run_calibration(app, 0);
                }
                ui.horizontal(|ui| {
                    if theme::action_button(ui, "CAL ON X ERROR", true).clicked() {
                        run_calibration(app, 1);
                    }
                    if theme::action_button(ui, "CAL ON Y ERROR", true).clicked() {
                        run_calibration(app, 2);
                    }
                });
                ui.label(
                    RichText::new("CTRL+P · ARROWS · ENTER EDIT DIMENSIONS")
                        .size(theme::font_size(7.5))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(
                        "CALIBRATION OFFSET IS READ-ONLY; U/RUN CANNOT OVERWRITE NEW CALIBRATION.",
                    )
                    .size(theme::font_size(7.0))
                    .color(theme::MUTED),
                );
                run_action_context(app, ui);
                calibration_result_readout(app, ui);
                if let Some(error) = app.probe.error.as_deref().or(app
                    .snapshot
                    .probe
                    .error
                    .as_deref())
                {
                    ui.label(
                        RichText::new(error)
                            .size(theme::font_size(8.0))
                            .color(theme::ERROR),
                    );
                }
            });
        });
    });
    #[cfg(test)]
    {
        app.probe_layout.calibrate_panel = Some(card_span(ui, card_top));
    }
}

struct HelpTopic {
    title: &'static str,
    pages: &'static [&'static [&'static str]],
}

const HELP_TOPICS: &[HelpTopic] = &[
    HelpTopic {
        title: "WORKFLOW",
        pages: &[
            &[
                "1–9 select a routine; Enter runs the highlighted routine. There is deliberately no selected routine when the page opens or its tab changes.",
                "F8 cycles Offsets → Outside → Inside → Boss+Pocket → Ridge+Valley → EdgeAngle → Calibrate → Help → Offsets. Esc returns to Offsets.",
                "PgUp/PgDn change probe category. In this Help topic, Up/Down select a topic and Left/Right change its page; no pointer scrolling is needed.",
                "The routine display separates SELECTED from LAST RUN / RESULT. Changing the highlighted card never changes which result A will Apply.",
            ],
            &[
                "Run requires machine ON, homed axes, an idle interpreter, no active program, a valid panel, the configured probe tool in the spindle and valid operation parameters.",
                "X/Y/Z edit the visible work-position DROs on routine pages only. Calibrate and Help have no hidden DRO editor; ordinary work offsets remain on the Offsets page.",
                "Ctrl+P focuses the inline parameter editor. Enter opens and commits a value; Esc cancels it. Arithmetic expressions accepted by the other LiNC table editors also work here.",
                "M remains the global Messages key. Probe mode is W, never plain M.",
            ],
        ],
    },
    HelpTopic {
        title: "MODES + ROTATION",
        pages: &[
            &[
                "W toggles POSITION and DIRECT WCO. POSITION measures into fresh result slots; A explicitly applies that originating result to the currently active WCS.",
                "DIRECT WCO writes the active WCS during Run, and a verified write marks the result already applied. An explicit A Apply that the controller confirms does the same, so either way a second A Apply is unavailable. Direct writes require an active WCS and PROTECT off.",
                "The selected mode is saved with probe preferences. The default is POSITION (MeasureOnly); opening the page does not silently force that default.",
                "Work offsets are not selected on this page. The WCS label and Apply target always follow the controller's actual active work offset.",
            ],
            &[
                "On EdgeAngle only, R toggles whether the measured angle is requested for the WCS rotation when the result is applied. The R state is visible beside the mode.",
                "WARNING: every edge-angle routine commands R0 and clears existing active-WCS rotation at Run, even in POSITION mode and even when R is OFF.",
                "R0 happens before the measurement-only branch and requires an active WCS with PROTECT off even in POSITION. Do not run an angle routine unless losing the old rotation is intended.",
                "Apply checks the result's original WCS against the active WCS and reports stale WCS instead of writing a result into a different coordinate system.",
            ],
        ],
    },
    HelpTopic {
        title: "PARAMETERS + HINTS",
        pages: &[
            &[
                "#3015 SLOW FEED is the final-contact feed. Zero is valid and means SINGLE TOUCH: the routine does not perform a separate fast/slow two-touch sequence.",
                "#3016 FAST FEED, #3017 TRAVERSE FEED, #3018/#3020 maximum X/Y/Z travel, #3019/#3021 clearances, #3022 extra depth, #3023 step-off and #3024 edge width remain inline editable.",
                "U UPDATE PARAMS sends the validated common parameter block without starting a routine. Invalid values remain visible for correction and are not sent.",
                "#3014 probe tool is profile-owned and read-only here. #5410 stylus diameter also comes from the machine profile; neither is a UI preference.",
            ],
            &[
                "#3025 is the boss/pocket DIAMETER HINT, not stylus diameter. Round boss/pocket routines use this expected feature diameter.",
                "#3026 and #3027 are rectangular boss/pocket X WIDTH and Y WIDTH feature hints. They are not X/Y centre coordinates and not stylus diameter.",
                "#3028 and #3029 are ridge/valley X WIDTH and Y WIDTH feature hints. X routines need the X hint; Y routines need the Y hint.",
                "The hint row follows the selected operation. All visible feature hints and calibration dimensions use the same Enter/Escape inline editing semantics as the common fields.",
            ],
            &[
                "Run uses core validation for the selected operation. For example, a round routine checks its diameter hint while a rectangular routine checks both required widths; square calibration additionally checks #3036.",
                "Committed parameter edits, W mode changes, EdgeAngle R changes, feature hints and V calibration-axis changes are saved to preferences through LiNC's normal dirty/save mechanism.",
                "Controller-owned #3032 calibration is excluded from preferences. It is synchronized from the live snapshot before U or Run so an older block cannot overwrite a new calibration.",
            ],
        ],
    },
    HelpTopic {
        title: "RESULTS + APPLY",
        pages: &[
            &[
                "Result slots are freshly updated Probe Basic values, not a generic XYZ vector. #1001 = X width, #1002 = X centre, #1003 = Y width, #1004 = Y centre.",
                "#1005 = X- probed position and #1006 = X+ probed position. For an angle routine #1005 instead carries X1/X2 as defined by that operation; the catalogue determines Apply sources.",
                "A cell shows an em dash when this run did not freshly update its slot. Zero is displayed only when zero is an actual fresh result, never as a placeholder.",
            ],
            &[
                "#1007 = Y- probed position, #1008 = Y+ probed position, #1009 = Z surface position and #1010 = averaged diameter. Y-centre routines use #1007/#1008 as Y1/Y2.",
                "#1011 = edge delta and #1012 = edge angle in degrees. Other values use the displayed controller linear units. The result header names its routine, WCS and POSITION/DIRECT WCO mode.",
                "Accent-bordered cells feed Apply for the originating operation. Other fresh result cells remain relevant measurements but are not necessarily written to the WCS.",
            ],
            &[
                "A APPLY names the originating result routine and the live target WCS. It is unavailable with no result, a stale WCS, a result that is already applied, or a no-Apply calibration/spindle/reset operation.",
                "U updates probe parameters. Ctrl+R resets all result data, Ctrl+X resets X data and Ctrl+Y resets Y data. These Ctrl keys are page-local and do not replace their global meanings outside Probe. While a text editor is open on this page (Ctrl+P on a parameter, or a DRO), Ctrl+X is the clipboard chord and does not reset anything.",
                "Apply requires PROTECT off and a valid idle machine. Calibration results update #3032 through their calibration routine and never use generic A Apply.",
            ],
        ],
    },
    HelpTopic {
        title: "CALIBRATION",
        pages: &[
            &[
                "Fit a known ring or pin and select the matching round/square pocket/boss routine. Round calibration uses #3033 known diameter; square calibration uses #3034 known X width and #3035 known Y width.",
                "V cycles #3036 AVG XY → X → Y. AVG XY averages both dimensional errors; X or Y selects one axis error for square calibration. The CAL ON buttons set the same axis explicitly before Run.",
                "Calibration dimensions and axis are saved. Every calibration Run is forced to POSITION on its issued parameter clone, so DIRECT WCO can never make calibration write a work offset.",
            ],
            &[
                "#3032 CALIBRATION OFFSET is live controller state and is read-only in this UI. It updates after calibration and is copied into the next U/Run parameter block automatically.",
                "Ctrl+R RESET CAL clears the stored controller calibration after machine-write checks on the Calibrate tab. On routine tabs Ctrl+R/Ctrl+X/Ctrl+Y instead clear measurement result data only.",
                "Repeat calibration until the measured error is stable, then verify a known feature before production work. Calibration, spindle and reset operations have no generic Apply action.",
            ],
        ],
    },
    HelpTopic {
        title: "ROUTINE FAMILIES",
        pages: &[
            &[
                "Outside and Inside: number keys 1–3 select front features, 7–9 back features and 4/6 side features. Centre key 5 is SURFACE Z on both tabs.",
                "Boss+Pocket and Ridge+Valley use keys 1–6. Round routines use a diameter hint; rectangular routines use feature widths. Centre-start variants start near the feature centre.",
                "Surface Z is also centre key 5 on EdgeAngle. The other angle cards measure X/Y edge angle and delta using the requested edge width.",
            ],
            &[
                "Calibrate uses keys 1–4 for round/square pocket/boss. Help has no routine selection and Enter does not issue motion there.",
                "The current tool shown beside the probe symbol comes from controller tool-change state. PROBE INPUT reports live TRIPPED, READY or UNKNOWN rather than assuming an open input.",
                "Routine artwork scales with the existing matrix; number selection, dense keyboard navigation and the seven approved tabs remain unchanged.",
            ],
        ],
    },
];

fn shifted_help_topic(topic: usize, delta: isize) -> (usize, usize) {
    let next = (topic as isize + delta).clamp(0, HELP_TOPICS.len() as isize - 1) as usize;
    (next, 0)
}

fn shifted_help_page(topic: usize, page: usize, delta: isize) -> usize {
    let count = HELP_TOPICS
        .get(topic)
        .map(|topic| topic.pages.len())
        .unwrap_or(1);
    (page as isize + delta).clamp(0, count as isize - 1) as usize
}

#[derive(Clone, Copy)]
struct HelpFigure {
    image: &'static str,
    caption: &'static str,
    credit: &'static str,
}

/// The figure that belongs to a help page. UI screenshots are LiNC's own;
/// only the three parameter diagrams come from Probe Basic. Pages without a
/// useful figure keep the full width for their text.
fn help_figure(topic: usize, page: usize) -> Option<HelpFigure> {
    let (image, caption, credit) = match (topic, page) {
        (0, 0) => ("outside-corners.png", "LiNC Outside Corners routines", ""),
        (0, 1) => ("parameters.png", "LiNC probing parameters", ""),
        (1, 0) => ("work-offsets.png", "LiNC work offsets", ""),
        (1, 1) => ("edge-angle.png", "LiNC Edge Angle routines", ""),
        (2, 0) => (
            "step-off-width.svg",
            "Step-off width",
            "Probe Basic · GPLv3",
        ),
        (2, 1) => ("probing-hints.png", "LiNC feature hint fields", ""),
        (2, 2) => (
            "extra-probe-depth.svg",
            "Extra probe depth",
            "Probe Basic · GPLv3",
        ),
        (3, 0) | (3, 1) => ("results.png", "LiNC probe results", ""),
        (4, 0) => ("calibrate.png", "LiNC calibration routines", ""),
        (5, 0) => ("boss-and-pocket.png", "LiNC Boss + Pocket routines", ""),
        _ => return None,
    };
    Some(HelpFigure {
        image,
        caption,
        credit,
    })
}

fn help_view(app: &mut LincApp, ui: &mut Ui) {
    let height = ui.available_height().max(420.0);
    app.probe.help_topic = app.probe.help_topic.min(HELP_TOPICS.len() - 1);
    let topic = &HELP_TOPICS[app.probe.help_topic];
    app.probe.help_page = shifted_help_page(app.probe.help_topic, app.probe.help_page, 0);
    let page = topic.pages[app.probe.help_page];
    let figure = help_figure(app.probe.help_topic, app.probe.help_page);

    theme::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.set_min_height((height - 24.0).max(0.0));
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(190.0, (height - 24.0).max(0.0)),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(190.0);
                    theme::section_header(ui, "HELP TOPICS");
                    // Only the selected topic gets the blue selection fill. A
                    // pointer left over the previous row must not look selected
                    // after the keyboard moves the blue box.
                    let widgets = &mut ui.visuals_mut().widgets;
                    widgets.hovered.bg_fill = theme::PANEL_RAISED;
                    widgets.hovered.weak_bg_fill = theme::PANEL_RAISED;
                    widgets.hovered.bg_stroke = Stroke::new(1.0, theme::BORDER);
                    widgets.hovered.fg_stroke = Stroke::new(1.0, theme::FG);
                    widgets.active.bg_fill = theme::PANEL_RAISED;
                    widgets.active.weak_bg_fill = theme::PANEL_RAISED;
                    widgets.active.bg_stroke = Stroke::new(1.0, theme::BORDER);
                    widgets.active.fg_stroke = Stroke::new(1.0, theme::FG);
                    for (index, item) in HELP_TOPICS.iter().enumerate() {
                        let selected = index == app.probe.help_topic;
                        let text = RichText::new(item.title)
                            .size(theme::font_size(11.5))
                            .color(if selected { theme::FG } else { theme::MUTED });
                        if ui.selectable_label(selected, text).clicked() {
                            app.probe.help_topic = index;
                            app.probe.help_page = 0;
                        }
                        ui.add_space(2.0);
                    }
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("↑/↓ TOPIC\n←/→ PAGE\nPGUP/PGDN CATEGORY")
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                },
            );
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::section_header(ui, topic.title);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "TOPIC {}/{} · PAGE {}/{}",
                                app.probe.help_topic + 1,
                                HELP_TOPICS.len(),
                                app.probe.help_page + 1,
                                topic.pages.len()
                            ))
                            .monospace()
                            .size(theme::font_size(9.5))
                            .color(theme::MUTED),
                        );
                    });
                });
                theme::separator(ui);
                ui.add_space(10.0);

                let body_w = ui.available_width();
                let body_h = (height - 100.0).max(0.0);
                let figure_gap = 18.0;
                let figure_w = if figure.is_some() {
                    (body_w * 0.52).clamp(360.0, 500.0)
                } else {
                    0.0
                };
                let text_w = if figure.is_some() {
                    (body_w - figure_w - figure_gap).max(280.0)
                } else {
                    body_w.min(860.0)
                };
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(text_w, body_h),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_max_width(text_w);
                            for line in page {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(*line)
                                            .size(theme::font_size(13.0))
                                            .color(theme::FG),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(10.0);
                            }
                        },
                    );
                    if let Some(figure) = figure {
                        ui.add_space(figure_gap);
                        help_figure_panel(app, ui, figure, figure_w, body_h);
                    }
                });
            });
        });
    });
}

fn help_figure_panel(app: &mut LincApp, ui: &mut Ui, figure: HelpFigure, width: f32, height: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::top_down(Align::Min),
        |ui| {
            ui.set_max_width(width);
            let Some(texture) = app.probe.help_texture(ui.ctx(), figure.image) else {
                ui.label(
                    RichText::new(format!("[figure: {}]", figure.image))
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                return;
            };
            let initial = texture.size_vec2();
            let max_h = (height - 40.0).max(160.0);
            let scale = (width / initial.x).min(max_h / initial.y).min(1.0);
            let size = initial * scale;
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(6)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                        texture.id(),
                        size,
                    )));
                });
            ui.add_space(6.0);
            let label = if figure.credit.is_empty() {
                figure.caption.to_string()
            } else {
                format!("{} · {}", figure.caption, figure.credit)
            };
            ui.label(
                RichText::new(label)
                    .size(theme::font_size(9.5))
                    .color(theme::MUTED),
            );
        },
    );
}

fn parameter_editable(category: ProbeCategory) -> bool {
    category != ProbeCategory::ProbeHelp
}

fn dro_visible(category: ProbeCategory) -> bool {
    !matches!(
        category,
        ProbeCategory::Calibrate | ProbeCategory::ProbeHelp
    )
}

fn results_visible(category: ProbeCategory) -> bool {
    dro_visible(category)
}

fn start_param_edit(app: &mut LincApp) {
    if !parameter_value_editable(app.probe.param_selected)
        || !editor_parameters(app).contains(&app.probe.param_selected)
    {
        return;
    }
    app.probe.param_buffer = param_text(&app.probe.params, app.probe.param_selected);
    app.probe.param_focused = true;
    app.probe.param_editing = true;
    app.probe.param_edit_focus = true;
    app.probe.error = None;
}

fn set_parameter_value(params: &mut ProbeParameters, index: usize, value: f64) -> bool {
    if !parameter_value_editable(index) {
        return false;
    }
    match index {
        1 => params.slow_feed = value,
        2 => params.fast_feed = value,
        3 => params.traverse_feed = value,
        4 => params.max_xy_distance = value,
        5 => params.xy_clearance = value,
        6 => params.max_z_distance = value,
        7 => params.z_clearance = value,
        8 => params.extra_depth = value,
        9 => params.step_off_width = value,
        10 => params.edge_width = value,
        11 => params.diameter_hint = value,
        12 => params.x_hint_boss_pocket = value,
        13 => params.y_hint_boss_pocket = value,
        14 => params.x_hint_ridge_valley = value,
        15 => params.y_hint_ridge_valley = value,
        19 => params.calibration_diameter = value,
        20 => params.x_cal_width = value,
        21 => params.y_cal_width = value,
        _ => return false,
    }
    true
}

fn commit_param_edit(app: &mut LincApp) {
    let index = app.probe.param_selected;
    let Some(value) = crate::state::eval_expression(&app.probe.param_buffer) else {
        app.probe.error = Some(format!(
            "{}: invalid entry {:?}",
            PARAM_LABELS.get(index).copied().unwrap_or("PARAMETER"),
            app.probe.param_buffer
        ));
        return;
    };
    if !value.is_finite() || !set_parameter_value(&mut app.probe.params, index, value) {
        app.probe.error = Some(format!("{}: invalid value", PARAM_LABELS[index]));
        return;
    }
    app.probe.param_editing = false;
    app.probe.param_focused = false;
    app.probe.param_buffer.clear();
    app.probe.param_edit_focus = false;
    app.probe.error = None;
    app.remember_probe_params();
}

fn handle_param_edit_keys(app: &mut LincApp, ui: &mut Ui) {
    let mut commit = false;
    let mut cancel = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            commit = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            cancel = true;
        }
    });
    if commit {
        commit_param_edit(app);
    } else if cancel {
        app.probe.param_editing = false;
        app.probe.param_focused = false;
        app.probe.param_buffer.clear();
    }
}

fn on_stacked_hint(
    category: ProbeCategory,
    operation: Option<&ProbeOperation>,
    selected: usize,
) -> bool {
    matches!(
        category,
        ProbeCategory::BossAndPocket | ProbeCategory::RidgeAndValley
    ) && hint_parameters(category, operation).contains(&selected)
}

fn handle_param_nav_keys(app: &mut LincApp, ui: &mut Ui) -> bool {
    if !app.probe.param_focused
        || !parameter_editable(app.probe.category)
        || app.probe.param_editing
    {
        return false;
    }
    let mut column_delta = 0isize;
    let mut row_delta = 0isize;
    let mut edit = false;
    let mut leave = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            leave = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            column_delta -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            column_delta += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            row_delta -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            row_delta += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            edit = true;
        }
    });
    let selected_is_hint = on_stacked_hint(
        app.probe.category,
        app.probe.operation(),
        app.probe.param_selected,
    );
    let column_delta = if selected_is_hint { 0 } else { column_delta };
    let row_step = if selected_is_hint {
        1
    } else {
        param_row_step(app.probe.category)
    };
    let delta = column_delta + row_delta * row_step;
    if leave {
        app.probe.param_focused = false;
        return true;
    }
    if delta == 0 && !edit {
        return false;
    }
    if delta != 0 {
        let parameters = editor_parameters(app);
        if parameters.is_empty() {
            return false;
        }
        let current = parameters
            .iter()
            .position(|index| *index == app.probe.param_selected)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, parameters.len() as isize - 1);
        app.probe.param_selected = parameters[next as usize];
    }
    if edit {
        start_param_edit(app);
    }
    true
}

fn handle_help_keys(app: &mut LincApp, ui: &mut Ui) {
    let mut topic_delta = 0isize;
    let mut page_delta = 0isize;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            topic_delta -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            topic_delta += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            page_delta -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            page_delta += 1;
        }
    });
    if topic_delta != 0 {
        let (topic, page) = shifted_help_topic(app.probe.help_topic, topic_delta);
        app.probe.help_topic = topic;
        app.probe.help_page = page;
    }
    if page_delta != 0 {
        app.probe.help_page =
            shifted_help_page(app.probe.help_topic, app.probe.help_page, page_delta);
    }
}

fn handle_probe_mode_keys(app: &mut LincApp, ui: &mut Ui) -> bool {
    let category = app.probe.category;
    let mut toggle_mode = false;
    let mut toggle_rotation = false;
    let mut cycle_calibration = false;
    let mut reset_calibration_now = false;
    ui.input_mut(|input| {
        if dro_visible(category) && input.consume_key(egui::Modifiers::NONE, egui::Key::W) {
            toggle_mode = true;
        }
        if category == ProbeCategory::EdgeAngle
            && input.consume_key(egui::Modifiers::NONE, egui::Key::R)
        {
            toggle_rotation = true;
        }
        if category == ProbeCategory::Calibrate
            && input.consume_key(egui::Modifiers::NONE, egui::Key::V)
        {
            cycle_calibration = true;
        }
        if category == ProbeCategory::Calibrate
            && input.consume_key(egui::Modifiers::CTRL, egui::Key::R)
        {
            reset_calibration_now = true;
        }
    });
    if toggle_mode {
        app.probe.params.probe_mode = match app.probe.params.probe_mode {
            ProbeMode::ApplyWcs => ProbeMode::MeasureOnly,
            ProbeMode::MeasureOnly => ProbeMode::ApplyWcs,
        };
        app.probe.error = None;
        app.remember_probe_params();
        app.toast(
            if app.probe.params.probe_mode == ProbeMode::ApplyWcs {
                "DIRECT WCO — Run writes the active work offset"
            } else {
                "POSITION — measure, then A applies the result"
            },
            if app.probe.params.probe_mode == ProbeMode::ApplyWcs {
                theme::WARNING
            } else {
                theme::ACCENT
            },
        );
    }
    if toggle_rotation {
        app.probe.params.wcs_rotation = !app.probe.params.wcs_rotation;
        app.probe.error = None;
        app.remember_probe_params();
        app.toast(
            format!(
                "Edge-angle requested rotation {} — every angle Run still clears old rotation (R0)",
                if app.probe.params.wcs_rotation {
                    "ON"
                } else {
                    "OFF"
                }
            ),
            theme::WARNING,
        );
    }
    if cycle_calibration {
        let next = next_calibration_axis(app.probe.params.square_cal_axis);
        set_calibration_axis(app, next);
        app.toast(
            format!("CALIBRATION AXIS {}", calibration_axis_label(next)),
            theme::ACCENT,
        );
    }
    if reset_calibration_now {
        reset_calibration(app);
    }
    toggle_mode || toggle_rotation || cycle_calibration || reset_calibration_now
}

fn handle_result_action_keys(app: &mut LincApp, ui: &mut Ui) -> bool {
    if !results_visible(app.probe.category) {
        return false;
    }
    // Ctrl+X is the X data reset here, but it is also the clipboard chord:
    // while a text editor owns the keyboard the editor gets it, never the reset.
    let cut_allowed =
        !app.probe.param_editing && app.probe.dro_edit.is_none() && !ui.ctx().text_edit_focused();
    let mut update = false;
    let mut reset = None;
    let mut apply = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::U) {
            update = true;
        }
        if input.consume_key(egui::Modifiers::CTRL, egui::Key::R) {
            reset = Some(ProbeDataReset::All);
        }
        if cut_allowed && input.consume_key(egui::Modifiers::CTRL, egui::Key::X) {
            reset = Some(ProbeDataReset::X);
        }
        if input.consume_key(egui::Modifiers::CTRL, egui::Key::Y) {
            reset = Some(ProbeDataReset::Y);
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::A) {
            apply = true;
        }
    });
    if update {
        update_probe_params(app);
    }
    if let Some(reset) = reset {
        reset_probe_data(app, reset);
    }
    if apply {
        super::probe::apply(app);
    }
    update || reset.is_some() || apply
}

pub fn handle_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() {
        return;
    }
    if handle_probe_dro_keys(app, ui) {
        return;
    }
    if app.probe.param_editing {
        handle_param_edit_keys(app, ui);
        return;
    }
    if ui.ctx().text_edit_focused() {
        return;
    }
    if app.probe.category == ProbeCategory::ProbeHelp {
        handle_help_keys(app, ui);
        return;
    }
    let focus_first = ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::P));
    if focus_first && parameter_editable(app.probe.category) {
        if let Some(index) = default_parameter(app.probe.category, app.probe.operation()) {
            app.probe.param_selected = index;
            app.probe.param_focused = true;
            app.probe.param_editing = false;
            app.probe.param_buffer.clear();
            app.probe.error = None;
        }
        return;
    }
    if handle_probe_mode_keys(app, ui) {
        return;
    }
    if handle_result_action_keys(app, ui) {
        return;
    }
    if handle_probe_dro_axis_keys(app, ui) {
        return;
    }
    if handle_param_nav_keys(app, ui) {
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
        select_routine(app, &operation);
    }
    if run_now && !cards.is_empty() {
        super::probe::run(app);
    }
}

/// Drop the selected routine and any open parameter or DRO editor. Every entry
/// into the page runs this, so a return from Files cannot leave a routine that
/// Enter would run.
pub fn clear_selection(app: &mut LincApp) {
    app.probe.operation.clear();
    app.probe.param_focused = false;
    app.probe.param_editing = false;
    app.probe.param_buffer.clear();
    app.probe.dro_edit = None;
    app.probe.error = None;
}

pub fn enter_first(app: &mut LincApp) {
    app.probe.category = ProbeCategory::OutsideCorners;
    clear_selection(app);
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
    clear_selection(app);
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
    fn parameter_indices_match_the_absolute_probe_basic_contract() {
        let params = ProbeParameters::default();
        assert_eq!(parameter_number(0), 3014);
        assert_eq!(parameter_number(22), 3036);
        assert_eq!(PARAM_LABELS[0], "PROBE TOOL");
        assert_eq!(PARAM_LABELS[11], "BOSS/POCKET DIAM");
        assert_eq!(param_text(&params, 2), "500.0");
        assert_eq!(param_text(&params, 3), "1000.0");
        assert_eq!(param_text(&params, 11), "50.0");
        assert_eq!(param_text(&params, 22), "AVG XY");
        assert!(!parameter_value_editable(0));
        assert!(!parameter_value_editable(18));
        assert!(!parameter_value_editable(22));
        for index in 1..=15 {
            assert!(parameter_value_editable(index), "#{}", 3014 + index);
        }
        for index in 19..=21 {
            assert!(parameter_value_editable(index), "#{}", 3014 + index);
        }
    }

    #[test]
    fn visible_feature_hints_are_editable_and_have_distinct_meanings() {
        for category in [
            ProbeCategory::BossAndPocket,
            ProbeCategory::RidgeAndValley,
            ProbeCategory::Calibrate,
        ] {
            for operation in cards_for(category)
                .iter()
                .filter_map(|card| linc_core::probe::operation(card.operation))
            {
                for index in hint_parameters(category, Some(operation)) {
                    assert!(parameter_value_editable(index));
                }
            }
        }
        assert!(PARAM_LABELS[12].contains("X WIDTH"));
        assert!(PARAM_LABELS[13].contains("Y WIDTH"));
        assert!(PARAM_LABELS[14].contains("X WIDTH"));
        assert!(PARAM_LABELS[15].contains("Y WIDTH"));
    }

    #[test]
    fn ctrl_p_defaults_to_the_visible_routine_hints() {
        let round = linc_core::probe::operation("round_boss").unwrap();
        let rect = linc_core::probe::operation("rect_boss").unwrap();
        let ridge_y = linc_core::probe::operation("ridge_y").unwrap();
        assert_eq!(
            default_parameter(ProbeCategory::BossAndPocket, None),
            Some(11)
        );
        assert_eq!(
            default_parameter(ProbeCategory::BossAndPocket, Some(round)),
            Some(11)
        );
        assert_eq!(
            default_parameter(ProbeCategory::BossAndPocket, Some(rect)),
            Some(12)
        );
        assert_eq!(
            default_parameter(ProbeCategory::RidgeAndValley, None),
            Some(14)
        );
        assert_eq!(
            default_parameter(ProbeCategory::RidgeAndValley, Some(ridge_y)),
            Some(15)
        );
        // The other tabs keep starting at the first common parameter.
        assert_eq!(
            default_parameter(ProbeCategory::OutsideCorners, None),
            Some(1)
        );
    }

    #[test]
    fn help_keyboard_navigation_clamps_and_topic_changes_reset_page() {
        assert_eq!(shifted_help_topic(0, -1), (0, 0));
        assert_eq!(shifted_help_topic(0, 1), (1, 0));
        assert_eq!(
            shifted_help_topic(HELP_TOPICS.len() - 1, 1),
            (HELP_TOPICS.len() - 1, 0)
        );
        assert_eq!(shifted_help_page(0, 0, -1), 0);
        assert_eq!(shifted_help_page(0, 0, 1), 1);
        assert_eq!(shifted_help_page(0, 1, 1), HELP_TOPICS[0].pages.len() - 1);
        let content = HELP_TOPICS
            .iter()
            .flat_map(|topic| topic.pages.iter())
            .flat_map(|page| page.iter())
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        for required in [
            "SINGLE TOUCH",
            "DIRECT WCO",
            "R0",
            "#5410",
            "#1001",
            "#1012",
            "#3036",
            "AVG XY",
        ] {
            assert!(content.contains(required), "help omits {required}");
        }
    }

    #[test]
    fn hidden_dro_and_result_pages_do_not_accept_page_local_axis_actions() {
        for category in ProbeCategory::ALL {
            let visible = !matches!(
                category,
                ProbeCategory::Calibrate | ProbeCategory::ProbeHelp
            );
            assert_eq!(dro_visible(category), visible);
            assert_eq!(results_visible(category), visible);
        }
    }

    #[test]
    fn calibration_axis_cycles_avg_xy_x_y() {
        assert_eq!(next_calibration_axis(0), 1);
        assert_eq!(next_calibration_axis(1), 2);
        assert_eq!(next_calibration_axis(2), 0);
        assert_eq!(next_calibration_axis(200), 0);
        assert_eq!(calibration_axis_label(0), "AVG XY");
        assert_eq!(calibration_axis_label(1), "X");
        assert_eq!(calibration_axis_label(2), "Y");
    }

    #[test]
    fn zero_slow_feed_is_valid_single_touch() {
        let mut params = ProbeParameters::default();
        params.slow_feed = 0.0;
        let operation = linc_core::probe::operation("surface_z").unwrap();
        assert!(params
            .validate_for_operation(true, params.probe_tool_number, operation)
            .is_ok());
    }

    #[test]
    fn six_routine_tabs_use_the_bottom_half_of_the_keypad() {
        for category in [ProbeCategory::BossAndPocket, ProbeCategory::RidgeAndValley] {
            let keys: Vec<&str> = cards_for(category).iter().map(|card| card.key).collect();
            assert_eq!(keys, ["4", "5", "6", "1", "2", "3"]);
        }
    }

    #[test]
    fn calibration_cards_number_from_the_bottom_row_up() {
        let cards = cards_for(ProbeCategory::Calibrate);
        let keys: Vec<&str> = cards.iter().map(|card| card.key).collect();
        assert_eq!(keys, ["3", "4", "1", "2"]);
        let operations: Vec<&str> = cards.iter().map(|card| card.operation).collect();
        assert_eq!(
            operations,
            [
                "cal_round_pocket",
                "cal_round_boss",
                "cal_square_pocket",
                "cal_square_boss"
            ]
        );
    }

    #[test]
    fn a_routine_card_never_shrinks_below_the_target_size() {
        // Three rows in a 1280x720 column are around 104 px; the clamp is what
        // holds the floor when a banner, a shorter window or a new row count
        // leaves less. Anything under the target clips instead of shrinking.
        for rows in 1..=4usize {
            for grid_h in [0.0, 40.0, 100.0, 144.0, 211.0, 396.0, 900.0] {
                let card = routine_card_height(grid_h, rows, 6.0);
                assert!(
                    (TARGET..=132.0).contains(&card),
                    "rows={rows} grid_h={grid_h}: {card}"
                );
            }
        }
        assert_eq!(routine_card_height(100.0, 3, 6.0), TARGET);
        assert_eq!(routine_card_height(396.0, 3, 6.0), 128.0);
    }

    #[test]
    fn the_parameter_grid_navigates_one_row_of_four() {
        assert_eq!(PARAM_COLUMNS, 4);
        assert_eq!(param_row_step(ProbeCategory::OutsideCorners), 4);
        assert_eq!(param_row_step(ProbeCategory::Calibrate), 1);
        // The grid is row-major over this list, so Down from any slot lands on
        // the field drawn directly below it.
        assert_eq!(COMMON_PARAMS.len(), 11);
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
