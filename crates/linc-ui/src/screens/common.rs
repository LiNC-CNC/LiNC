//! Components shared by more than one screen.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Layout, RichText, Sense, Ui, Vec2};
use linc_core::gcode::{lex_line, Preview, TokenKind};
use linc_core::model::{
    format_clock, AxisMap, MachineSnapshot, MachineVariant, PanelMode, PositionSet,
};
use linc_view::{draw_trajectory, TrajectoryView, ViewMode};

use crate::shell::LincApp;
use crate::state::Page;
use crate::theme;

/// Space between a pane's left edge and the selection band's rim bar.
pub const SELECTION_EDGE_PADDING: f32 = 4.0;

/// Paint the selection band every list shares: a translucent accent field
/// with a solid bar on its left rim. One implementation, so a selected G-code
/// line reads the same in the editor and in the MAIN program pane.
pub fn selection_band(ui: &Ui, rect: egui::Rect) {
    let rect = rect.intersect(ui.clip_rect());
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
        theme::tint(theme::ACCENT, theme::SELECTION_ALPHA),
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(theme::SELECTION_BAR, rect.height())),
        egui::CornerRadius::same(1),
        theme::ACCENT,
    );
}

pub fn token_color(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::GCode => theme::C1,
        TokenKind::MCode => theme::C2,
        TokenKind::Axis => theme::C3,
        TokenKind::Number => theme::FG,
        TokenKind::FeedSpeed => theme::C4,
        TokenKind::Tool => theme::C5,
        TokenKind::Parameter => theme::C6,
        TokenKind::Variable => theme::C6,
        TokenKind::Comment => theme::COMMENT,
        TokenKind::ProgramNumber => theme::C6,
        TokenKind::LineNumber => theme::LINE_NUMBER,
        TokenKind::OWord => theme::C2,
        TokenKind::Other => theme::FG,
    }
}

pub fn token_ui(ui: &mut Ui, text: &str, size: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for token in lex_line(text) {
            let mut rich = RichText::new(token.display())
                .monospace()
                .size(theme::font_size(size))
                .color(token_color(token.kind));
            if token.kind == TokenKind::Comment {
                rich = rich.italics();
            }
            ui.label(rich);
        }
    });
}

/// Syntax-coloured layout job for one G-code line. Unlike the label row this
/// can wrap to the pane width and reports the height that wrap needs.
fn token_job(text: &str, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = FontId::monospace(theme::font_size(size));
    for (index, token) in lex_line(text).into_iter().enumerate() {
        job.append(
            &token.display(),
            if index == 0 { 0.0 } else { 3.0 },
            TextFormat {
                font_id: font.clone(),
                color: token_color(token.kind),
                italics: token.kind == TokenKind::Comment,
                ..Default::default()
            },
        );
    }
    job
}

/// Space under a cell that has a mnemonic: half a mnemonic line, a little more
/// than the spacing between the lines inside a cell. Enough to separate wrapped
/// rows without leaving a hole.
const ROW_GAP: f32 = 5.0;

/// Active G-code modal groups as cells of group label and active code.
///
/// The Machine dashboard and the Program side pane share the cells; the column
/// count changes with the width available, and `mnemonics` adds the third line
/// naming what the code means (`G21` → `MM`). The dashboard leaves it off: that
/// strip sits under sixteen lines of G-code and its two-line cells are what the
/// card is sized for.
pub fn modal_grid(app: &LincApp, ui: &mut Ui, columns: usize, mnemonics: bool, id_salt: &str) {
    let codes = app
        .snapshot
        .program
        .modal
        .iter()
        .filter(|(key, _)| key.starts_with('G'))
        .map(|(key, value)| {
            (
                modal_abbreviation(key),
                modal_text(key, value),
                modal_mnemonic(key),
            )
        })
        .collect::<Vec<_>>();
    if codes.is_empty() {
        ui.label(
            RichText::new("—")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        return;
    }
    let columns = columns.max(1);
    let gaps = (columns - 1) as f32 * 4.0;
    let column_width = ((ui.available_width() - gaps) / columns as f32).max(30.0);
    egui::Grid::new(id_salt)
        .num_columns(columns)
        .spacing([4.0, 1.0])
        .min_col_width(column_width)
        .show(ui, |ui| {
            for (index, (abbreviation, code, mnemonic)) in codes.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(*abbreviation)
                            .size(theme::font_size(6.0))
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new(code)
                            .monospace()
                            .size(theme::font_size(8.0))
                            .color(theme::FG),
                    );
                    if mnemonics && !mnemonic.is_empty() {
                        ui.label(
                            RichText::new(*mnemonic)
                                .size(theme::font_size(7.0))
                                .color(theme::COMMENT),
                        );
                        // The third line runs straight into the next row's group
                        // label otherwise, and the rows stop reading as rows.
                        ui.add_space(ROW_GAP);
                    }
                });
                if (index + 1) % columns == 0 {
                    ui.end_row();
                }
            }
        });
}

/// A MODAL pane of its own, for pages that have room for a side column.
/// Shared card shell: an exact rect, the same 4 px frame and tight spacing the
/// Machine page's toolpath card uses, and a plain title label.
pub fn shell_card<R>(ui: &mut Ui, rect: egui::Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    // Slightly larger than the rect: the frame's 1 px stroke sits on the rect's
    // edge, and clipping exactly there cut the bottom border off.
    child.set_clip_rect(rect.expand(1.5));
    let inner = Vec2::new(
        (rect.width() - 8.0).max(20.0),
        (rect.height() - 8.0).max(10.0),
    );
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(4))
        .show(&mut child, |ui| {
            ui.set_min_size(inner);
            ui.spacing_mut().interact_size.y = 14.0;
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner
}

/// Card title, matching MAIN: a plain label, not a section header.
pub fn card_title(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::font_size(10.0))
            .strong()
            .color(theme::MUTED),
    );
}

/// Toolpath status line: keys on the left, view and zoom on the right. Every
/// toolpath card draws it, so no card needs a legend over the drawing.
pub fn toolpath_header(ui: &mut Ui, view: &str, zoom: f32, remaining: bool) {
    ui.horizontal(|ui| {
        card_title(ui, "TOOLPATH");
        ui.add_space(8.0);
        ui.label(
            RichText::new("— CUT")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::ACCENT),
        );
        ui.label(
            RichText::new("-- RAPID")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new("— LIVE")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::SUCCESS),
        );
        let (marker, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        // The same inverted triangle as the live tool marker, so the key reads
        // as the tool rather than as another line style.
        let tip = marker.center_bottom() - egui::vec2(0.0, 1.0);
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                tip + egui::vec2(-4.0, -8.0),
                tip + egui::vec2(4.0, -8.0),
                tip,
            ],
            linc_view::theme::TOOL,
            egui::Stroke::new(1.0, linc_view::theme::TOOL),
        ));
        ui.label(
            RichText::new("TOOL")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::FG),
        );
        // The REMAINING view has no trail and eats the path behind the tool, so
        // the two keys above do not describe the card: say which mode is in force.
        if remaining {
            ui.add_space(8.0);
            ui.label(
                RichText::new("REMAINING")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .strong()
                    .color(linc_view::theme::TOOL),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("ZOOM {:.1}x", zoom))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(view)
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
}

/// MODAL pane drawn into an exact rect.
///
/// The card is forced to the rect's size rather than to its content: a frame
/// sizes to content, and the few pixels of difference were enough to leave the
/// column beside the editor ending on a different line.
pub fn modal_pane_in(app: &LincApp, ui: &mut Ui, columns: usize, id_salt: &str, rect: egui::Rect) {
    shell_card(ui, rect, |ui| {
        card_title(ui, "MODAL");
        modal_grid(app, ui, columns, true, id_salt);
    });
}

/// Height a MODAL pane needs for `codes` G-code cells at `columns` wide.
///
/// Measured from the card: 26 px of chrome (margins plus title) and 41 px per
/// row of cells — group label, code, mnemonic and half a line of clearance.
/// Derived rather than observed so it cannot lag a frame and leave the column
/// short.
pub fn modal_height(codes: usize, columns: usize) -> f32 {
    let rows = codes.max(1).div_ceil(columns.max(1));
    // 8 px frame, an 18 px title row, and 41 px per row of cells.
    26.0 + rows as f32 * 41.0
}

/// Height of the two-line MODAL band the dashboard draws inside its own program
/// card: the rule and label above the grid, then one 25 px row of two-line
/// cells per `columns` codes.
///
/// `modal_height` sizes a card of its own — its frame, its title and
/// three-line cells — and is much taller. Reserving that here left the band's
/// excess as dead space under it instead of giving it to the listing.
pub fn modal_strip_height(codes: usize, columns: usize) -> f32 {
    let rows = codes.max(1).div_ceil(columns.max(1));
    // The 1 px rule, the 9 pt MODAL label (13 px) and the two 2 px gaps between
    // them: 18 px, measured off the card at the theme's ramp.
    const CHROME: f32 = 18.0;
    // A two-line cell: the group label and the code.
    const ROW: f32 = 25.0;
    CHROME + rows as f32 * ROW
}

pub fn modal_text(key: &str, value: &str) -> String {
    if value.is_empty() {
        key.to_string()
    } else {
        format!("{key} {value}")
    }
}

pub fn axis_cards(app: &LincApp, ui: &mut Ui) {
    let axes = app.axes();
    let count = axes.len().min(5).max(1);
    let compact = count > 3;
    let gap = 8.0;
    let width =
        ((ui.available_width() - gap * (count.saturating_sub(1)) as f32) / count as f32).max(120.0);
    let height = if compact {
        theme::DRO_CARD_HEIGHT_COMPACT
    } else {
        theme::DRO_CARD_HEIGHT
    };
    let mut rects = Vec::new();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for axis in axes.iter().take(count) {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
            rects.push((axis.clone(), rect));
        }
    });
    for (axis, rect) in rects {
        paint_axis_card(app, ui, &axis, rect, compact);
    }
}

/// The card chrome shared by every axis readout.
pub fn paint_dro_frame(ui: &Ui, rect: egui::Rect, accent: bool) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6, theme::PANEL);
    painter.rect_stroke(
        rect,
        6,
        egui::Stroke::new(1.0, if accent { theme::ACCENT } else { theme::BORDER }),
        egui::StrokeKind::Inside,
    );
}

/// The X factor of the lathe's radius/diameter display: a radius reads `X`, a
/// diameter reads `2X`. `Z` (and every mill axis) is a position, never a
/// diameter, so it never scales — doubling it would put a second wrong number
/// on the screen. The controller keeps reporting a radius either way; this is
/// the display only.
pub fn dro_value(axis: &str, value: f64, diameter: bool) -> f64 {
    if diameter && axis == "X" {
        value * 2.0
    } else {
        value
    }
}

/// The axis name, with the `DIA` mark while the readout shows a diameter.
pub fn dro_axis_label(axis: &str, diameter: bool) -> String {
    if diameter && axis == "X" {
        format!("{axis} DIA")
    } else {
        axis.to_string()
    }
}

/// The top half of an axis readout: the axis name, the work value and the
/// dimmed machine value beside it.
///
/// The Offsets page draws the same card without the bottom band, so the two
/// readouts cannot drift apart; `show_work` is false while an inline edit field
/// replaces the value.
// The card chrome and the axis it shows travel together; splitting it would
// make every caller reassemble the same six fields.
#[allow(clippy::too_many_arguments)]
pub fn paint_dro_readout(
    ui: &Ui,
    rect: egui::Rect,
    axis: &str,
    work: f64,
    machine: f64,
    compact: bool,
    show_work: bool,
    // Lathe only: X reads as a diameter (`2X`) and carries the mark.
    diameter: bool,
) {
    let painter = ui.painter_at(rect);
    let work = dro_value(axis, work, diameter);
    let machine = dro_value(axis, machine, diameter);
    let layout = dro_line_layout(ui, rect.width(), axis, work, machine, compact, diameter);
    // One line: the axis name, the coordinate beside it and, when it still
    // fits, the machine coordinate at the right end. The name used to hold a
    // row of its own above the number — a whole text row spent on one
    // character, and the reason the cards were a third taller than they needed.
    let line_y = rect.top() + theme::DRO_LINE_Y;
    let font = FontId::monospace(layout.font_size);
    painter.text(
        egui::pos2(rect.left() + theme::DRO_INSET, line_y),
        egui::Align2::LEFT_CENTER,
        &layout.label,
        font.clone(),
        theme::C3,
    );
    if show_work {
        painter.text(
            egui::pos2(rect.left() + layout.value_x, line_y),
            egui::Align2::LEFT_CENTER,
            &layout.value,
            font,
            theme::FG,
        );
    }
    // The machine coordinate sits beside the work value: the operator reads the
    // work position first, and the controller's own coordinate is the reference
    // for offsets and limits, so it stays visible but dimmed — and it goes when
    // a long coordinate would otherwise run into it.
    if let Some(text) = &layout.machine {
        painter.text(
            egui::pos2(rect.right() - theme::DRO_INSET, line_y),
            egui::Align2::RIGHT_CENTER,
            text,
            FontId::monospace(theme::font_size(theme::DRO_SMALL_FONT)),
            theme::MUTED,
        );
    }
}

/// Where the pieces of one readout line land, and whether the machine
/// coordinate still fits beside them.
pub struct DroLineLayout {
    pub label: String,
    pub value: String,
    pub machine: Option<String>,
    /// Left edge of the work value, measured from the card's left edge: after
    /// the label, so the two never overlap.
    pub value_x: f32,
    /// The axis name is set at the coordinate's own size.
    pub font_size: f32,
}

/// Lay out one readout line from measured text.
///
/// The machine coordinate is the field that goes first: the axis name and the
/// work value are what the operator is reading, and the controller's own
/// coordinate is a reference. It is dropped whole — a truncated number is
/// worse than no number — as soon as it would touch the value.
#[allow(clippy::too_many_arguments)]
pub fn dro_line_layout(
    ui: &Ui,
    width: f32,
    axis: &str,
    work: f64,
    machine: f64,
    compact: bool,
    diameter: bool,
) -> DroLineLayout {
    let base = if compact {
        theme::DRO_FONT_COMPACT
    } else {
        theme::DRO_FONT
    };
    let label = dro_axis_label(axis, diameter);
    let value = theme::format_position(work, 3);
    let machine_text = format!("M {}", theme::format_position(machine, 3));
    let label_width = theme::measure(ui, &label, base);
    let value_width = theme::measure(ui, &value, base);
    let machine_width = theme::measure(ui, &machine_text, theme::DRO_SMALL_FONT);
    // Between the two insets, with a gap either side of the machine value.
    let available =
        width - theme::DRO_INSET * 2.0 - label_width - theme::DRO_LABEL_GAP - value_width;
    let value_x = theme::DRO_INSET + label_width + theme::DRO_LABEL_GAP;
    DroLineLayout {
        label,
        value,
        machine: (available >= machine_width + theme::DRO_LABEL_GAP).then_some(machine_text),
        value_x,
        font_size: theme::font_size(base),
    }
}

/// The bottom band of a jog-screen readout: distance to go and the move bar.
pub fn paint_dro_progress(ui: &Ui, rect: egui::Rect, dtg: f64, fraction: f64) {
    let painter = ui.painter_at(rect);
    painter.text(
        rect.left_bottom() + Vec2::new(14.0, -15.0),
        egui::Align2::LEFT_BOTTOM,
        format!("DTG {dtg:.3}"),
        FontId::monospace(theme::font_size(9.0)),
        theme::MUTED,
    );
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 14.0, rect.bottom() - 9.0),
        egui::pos2(rect.right() - 14.0, rect.bottom() - 5.0),
    );
    painter.rect_filled(track, 2, theme::BORDER);
    if fraction > 0.0 {
        let fill = egui::Rect::from_min_size(
            track.min,
            Vec2::new(track.width() * fraction as f32, track.height()),
        );
        painter.rect_filled(fill, 2, theme::ACCENT);
    }
}

fn paint_axis_card(app: &LincApp, ui: &Ui, axis: &str, rect: egui::Rect, compact: bool) {
    let (work, machine, dtg) = axis_display(app, axis).unwrap_or((0.0, 0.0, 0.0));
    let active = app
        .snapshot
        .panel
        .jog
        .active_axis
        .as_deref()
        .map(|name| name == axis)
        .unwrap_or(false);
    // Progress of the move in flight, not the position within the travel range:
    // the bar starts empty when the axis sets off, fills as the distance to go
    // shrinks, and sits full once it arrives (see `LincApp::move_progress`).
    let fraction = app.move_progress.get(axis).copied().unwrap_or(1.0);
    paint_dro_frame(ui, rect, active);
    paint_dro_readout(
        ui,
        rect,
        axis,
        work,
        machine,
        compact,
        true,
        app.reads_x_as_diameter(),
    );
    paint_dro_progress(ui, rect, dtg, fraction);
}

/// One load bar: the ruler runs green (0-80 %), yellow (80-100 %) and red
/// (100-120 %) behind an accent progress fill, with the percentage on the right.
/// One meter row: the label, the bar and the percentage on a single line.
pub const SERVO_METER_HEIGHT: f32 = 16.0;

/// Where the block of meters starts inside the card's free space.
///
/// The SERVO LOAD card is as tall as the feed block beside it, so a three-axis
/// machine has slack left over. Half of it above the meters and half below
/// centres them; stacked under the title they read as a gap at the card's
/// bottom instead of as a margin.
fn servo_centred_lead(free: f32, rows: usize) -> f32 {
    ((free - rows as f32 * SERVO_METER_HEIGHT) / 2.0).max(0.0)
}

fn servo_meter(ui: &mut Ui, label: &str, load: f64) {
    const SCALE: f32 = 120.0;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), SERVO_METER_HEIGHT),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let font = FontId::monospace(theme::font_size(10.0));
    painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        font.clone(),
        theme::FG,
    );
    painter.text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        format!("{load:.0}%"),
        font,
        theme::FG,
    );
    let bar = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 17.0, rect.center().y - 4.0),
        egui::pos2(rect.right() - 40.0, rect.center().y + 4.0),
    );
    if bar.width() < 12.0 {
        return;
    }
    let radius = 2;
    let zone = |from: f32, to: f32, color: Color32, left: bool, right: bool| {
        let x0 = bar.left() + bar.width() * (from / SCALE);
        let x1 = bar.left() + bar.width() * (to / SCALE);
        let rounding = egui::CornerRadius {
            nw: if left { radius } else { 0 },
            sw: if left { radius } else { 0 },
            ne: if right { radius } else { 0 },
            se: if right { radius } else { 0 },
        };
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, bar.top()), egui::pos2(x1, bar.bottom())),
            rounding,
            theme::tint(color, 100),
        );
    };
    zone(0.0, 80.0, theme::SUCCESS, true, false);
    zone(80.0, 100.0, theme::WARNING, false, false);
    zone(100.0, SCALE, theme::ERROR, false, true);
    let fill = ((load / SCALE as f64).clamp(0.0, 1.0)) as f32;
    if fill > 0.001 {
        painter.rect_filled(
            egui::Rect::from_min_max(
                bar.left_top(),
                egui::pos2(bar.left() + bar.width() * fill, bar.bottom()),
            ),
            egui::CornerRadius::same(radius),
            theme::ACCENT,
        );
    }
    painter.rect_stroke(
        bar,
        egui::CornerRadius::same(radius),
        egui::Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );
}

pub fn servo_load_card(rows: &[(String, f64)], ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        // The same content offset as the feed card, so both card frames end on
        // the same bottom line.
        ui.set_min_height((height - 24.0).max(0.0));
        ui.set_max_height((height - 24.0).max(0.0));
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = SERVO_METER_HEIGHT;
        ui.spacing_mut().item_spacing.y = 0.0;
        theme::section_header(ui, "SERVO LOAD");
        // Centre the block between the title and the card's bottom edge. The
        // card is as tall as the feed block beside it, so on a three-axis
        // machine the difference used to collect as one gap under the meters.
        let free = ui.max_rect().bottom() - ui.cursor().top();
        ui.add_space(servo_centred_lead(free, rows.len()));
        for (label, load) in rows {
            servo_meter(ui, label, *load);
        }
    });
}

/// One controller message: clock and severity on the first line, the text
/// wrapped over the full width below it.
fn message_row(ui: &mut Ui, alarm: &linc_core::model::Alarm) {
    let color = match alarm.severity {
        linc_core::model::AlarmSeverity::Critical | linc_core::model::AlarmSeverity::Error => {
            theme::ERROR
        }
        linc_core::model::AlarmSeverity::Warning => theme::WARNING,
        linc_core::model::AlarmSeverity::Info => theme::MUTED,
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format_clock(alarm.timestamp_ms))
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(alarm.severity.label())
                .monospace()
                .size(theme::font_size(9.0))
                .color(color),
        );
    });
    ui.add(
        egui::Label::new(
            RichText::new(&alarm.message)
                .size(theme::font_size(11.0))
                .color(if alarm.severity == linc_core::model::AlarmSeverity::Info {
                    theme::MUTED
                } else {
                    color
                }),
        )
        .wrap(),
    );
    ui.add_space(4.0);
}

/// Full-screen controller log, opened with `[keys].messages` and closed with
/// Enter, Escape or CLOSE. It owns the keyboard while it is up: the shell
/// consumes the close keys and the page keys before the page sees them.
pub fn messages_dialog(app: &mut LincApp, ctx: &egui::Context) {
    if !app.messages_dialog {
        return;
    }
    let screen = ctx.content_rect();
    let size = (screen.size() - Vec2::new(48.0, 48.0)).max(Vec2::new(640.0, 360.0));
    let logs = app.recent_logs(400);
    let (messages, alarms) = app.log_counts();
    let scroll = std::mem::take(&mut app.messages_scroll);
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("linc-messages-dialog"))
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(8)
                .inner_margin(16),
        )
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("MESSAGES")
                        .size(theme::font_size(22.0))
                        .strong()
                        .color(theme::FG),
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("ALARM {alarms} / MSG {messages}"))
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::action_button(ui, "CLOSE", true).clicked() {
                        close = true;
                    }
                });
            });
            theme::separator(ui);
            let list_height = (size.y - 90.0).max(120.0);
            egui::ScrollArea::vertical()
                .id_salt("linc-messages-scroll")
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if scroll != 0.0 {
                        ui.scroll_with_delta(Vec2::new(0.0, -scroll));
                    }
                    if logs.is_empty() {
                        ui.label(
                            RichText::new("NO MESSAGES")
                                .monospace()
                                .size(theme::font_size(12.0))
                                .color(theme::SUCCESS),
                        );
                        return;
                    }
                    let width = ui.available_width();
                    ui.set_max_width(width);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for alarm in &logs {
                        message_row(ui, alarm);
                        theme::separator(ui);
                    }
                });
        });
    if close || modal.should_close() {
        app.messages_dialog = false;
    }
}

fn modal_abbreviation(key: &str) -> &'static str {
    let code = key.trim().trim_start_matches('G').trim();
    if let Ok(number) = code.parse::<f32>() {
        if (54.0..=59.3).contains(&number) {
            return "WORK";
        }
        if (81.0..=89.0).contains(&number) {
            return "CYCLE";
        }
    }
    match code {
        "0" | "1" | "2" | "3" | "5" | "5.1" | "5.2" | "5.3" | "38.2" | "38.3" | "38.4" | "38.5" => {
            "MOTION"
        }
        "4" => "DWELL",
        "7" => "DIAM",
        "8" => "RAD",
        "9" => "STOP",
        "10" => "OFFSET",
        "15" | "16" => "POLAR",
        "17" | "18" | "19" => "PLANE",
        "20" | "21" => "UNITS",
        "28" | "30" => "HOME",
        "33" | "33.1" | "76" => "THREAD",
        "40" | "40.1" | "41" | "41.1" | "42" | "42.1" => "COMP",
        "43" | "43.1" | "49" => "LEN",
        "53" => "MACHINE",
        "54" | "55" | "56" | "57" | "58" | "59" | "54.1" | "55.1" | "56.1" | "57.1" | "58.1"
        | "59.1" | "59.2" | "59.3" => "WORK",
        "61" | "61.1" | "64" => "PATH",
        "73" | "74" => "CYCLE",
        "80" => "CANCEL",
        "90" | "91" => "DIST",
        "90.1" | "91.1" => "ARC",
        "92" | "92.1" | "92.2" | "92.3" => "OFFSET",
        "93" | "94" | "95" => "FEED",
        "96" | "97" => "SPIN",
        "98" | "99" => "RETURN",
        _ => "G-CODE",
    }
}

/// What the active code in a modal group means, in one short word.
///
/// The cell's first line names the group and the second names the code, so
/// `UNITS G21` still left the operator to remember which of G20/G21 is which.
/// This is the third line: `MM`. Kept to seven characters — the MAIN card's
/// cells are about 40 px wide — and empty when the code has no short meaning,
/// in which case the cell keeps the two lines it had.
///
/// Meanings follow the LinuxCNC G-code reference: `G94` is feed per minute
/// whatever the program's units, so it says `PER MIN`, not `MM/MIN`.
fn modal_mnemonic(key: &str) -> &'static str {
    match key.trim().trim_start_matches('G').trim() {
        // Motion.
        "0" => "RAPID",
        "1" => "FEED",
        "2" => "CW",
        "3" => "CCW",
        "4" => "DWELL",
        "5" | "5.1" => "SPLINE",
        "5.2" | "5.3" => "NURBS",
        "38.2" | "38.3" | "38.4" | "38.5" => "PROBE",
        // Dimensional and plane modes.
        "7" => "X DIA",
        "8" => "X RAD",
        "10" => "SET",
        "15" => "OFF",
        "16" => "ON",
        "17" => "XY",
        "18" => "ZX",
        "19" => "YZ",
        "20" => "INCH",
        "21" => "MM",
        // Dwell, homing and threading.
        "28" => "HOME 1",
        "30" => "HOME 2",
        "33" | "33.1" | "76" => "THREAD",
        // Cutter and tool-length compensation.
        "40" | "40.1" | "49" => "OFF",
        "41" | "41.1" => "LEFT",
        "42" | "42.1" => "RIGHT",
        "43" => "APPLY",
        "43.1" => "DYNAMIC",
        // Machine coordinates and work offsets.
        "53" => "MOVE",
        "54" => "CS 1",
        "55" => "CS 2",
        "56" => "CS 3",
        "57" => "CS 4",
        "58" => "CS 5",
        "59" => "CS 6",
        "54.1" | "55.1" | "56.1" | "57.1" | "58.1" => "SAVED",
        "59.1" => "CS 7",
        "59.2" => "CS 8",
        "59.3" => "CS 9",
        // Path control.
        "61" | "61.1" => "EXACT",
        "64" => "BLEND",
        // Canned cycles.
        "73" => "CHIP",
        "74" => "TAP LH",
        "80" => "OFF",
        "81" => "DRILL",
        "82" => "DWELL",
        "83" => "PECK",
        "84" => "TAP",
        "85" | "86" | "88" | "89" => "BORE",
        "87" => "BACK",
        // Distance modes and offsets.
        "90" | "90.1" => "ABS",
        "91" | "91.1" => "INC",
        "92" => "SET",
        "92.1" | "92.2" => "OFF",
        "92.3" => "ON",
        // Feed and spindle modes.
        "93" => "INVERSE",
        "94" => "PER MIN",
        "95" => "PER REV",
        "96" => "CSS",
        "97" => "RPM",
        "98" => "INIT",
        "99" => "R PLANE",
        _ => "",
    }
}

fn field_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::font_size(8.5))
            .color(theme::MUTED),
    );
}

/// Height the card's progress line (rule, label, bar and status row) takes
/// off its bottom. The MODAL band is pinned directly above it.
const PROGRESS_HEIGHT: f32 = 46.0;

/// Rows of G-code that fit in `available` px.
///
/// The listing is the card's flexible part: the band and the progress line own
/// their heights, and the listing takes every whole row between the header and
/// the band. Whole rows only, so the remainder is a single row's worth at most
/// and never enough to open a gap under the band. No floor: a card whose band
/// and progress line take all of it lists nothing rather than painting under
/// them.
///
/// `pitch` is one row's height plus the gap the card's layout adds after
/// it; counting rows by the row's height alone is what let the last rows run
/// past the band.
/// First line of the listing window.
///
/// While a program runs, the line being cut sits a third of the way down: the
/// third above it is what has just gone, the two thirds below are what is
/// coming, which is the part the operator is reading ahead in. Centring it put
/// half the window behind the tool and left the running line out of the window
/// altogether once a few long lines wrapped above it.
///
/// Idle, the window follows the operator's selection instead, clamped so the
/// last page of the program still fills the window.
fn listing_first_line(focus: usize, window_len: usize, running: bool, total: usize) -> isize {
    let window_len = window_len.max(1);
    if running {
        // Negative is fine and intended: before the program has cut a full
        // third, the window pads above line 1 so the running line stays put
        // instead of jumping down the card as the first lines execute.
        focus as isize - (window_len / 3) as isize
    } else {
        let last_first = (total.saturating_sub(window_len) + 1).max(1) as isize;
        (focus.saturating_sub(window_len / 2).max(1) as isize).min(last_first)
    }
}

/// Top row for an *idle* listing: keep the highlighted line where it is and
/// scroll one line when it leaves the window.
///
/// Re-centring on every move made the list jump under the operator's eyes while
/// they walked a selection down a long program; a jump larger than the window
/// still centres, because scrolling to put the new line at the very edge is
/// what makes it hard to find.
fn listing_follow(previous: isize, focus: usize, window_len: usize, total: usize) -> isize {
    let window_len = window_len.max(1) as isize;
    let focus = focus.max(1) as isize;
    let last_first = (total as isize - window_len + 1).max(1);
    let mut first = previous.clamp(1, last_first);
    if focus < first || focus >= first + window_len {
        let distance = if focus < first {
            first - focus
        } else {
            focus - (first + window_len - 1)
        };
        first = if distance > window_len {
            (focus - window_len / 2).max(1)
        } else if focus < first {
            focus
        } else {
            focus - window_len + 1
        };
    }
    first.clamp(1, last_first)
}

fn listing_rows(available: f32, pitch: f32) -> usize {
    (available / pitch.max(1.0)).floor().max(0.0) as usize
}

pub fn program_card(app: &LincApp, ui: &mut Ui) {
    program_card_sized(app, ui, ui.available_height());
}

pub fn program_card_sized(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(0.0));
        ui.set_max_height((height - 24.0).max(0.0));
        ui.set_min_width(ui.available_width());
        ui.set_max_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 18.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        let card_width = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(card_width, (height - 24.0).max(0.0)),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::section_header(ui, "PROGRAM");
                theme::separator(ui);

                let content = app.snapshot.program.content.as_deref();
                let row_height = ui.spacing().interact_size.y.max(16.0);
                let codes = app
                    .snapshot
                    .program
                    .modal
                    .iter()
                    .filter(|(key, _)| key.starts_with('G'))
                    .count();
                // The card's bottom block is placed by rect, not by what the
                // listing happened to leave: the progress line owns the last
                // PROGRESS_HEIGHT pixels and the MODAL band its own derived
                // height directly above, so the band ends on the progress rule
                // and no dead space can open between them. The listing takes
                // every whole row between the header and the band, and paints
                // inside that rect: a taller band (a second row of codes, a
                // narrower card) takes rows off the listing instead of being
                // painted over.
                let area = ui.max_rect();
                let band = egui::Rect::from_min_max(
                    egui::pos2(
                        area.left(),
                        area.max.y - PROGRESS_HEIGHT - modal_strip_height(codes, 6),
                    ),
                    egui::pos2(area.right(), area.max.y - PROGRESS_HEIGHT),
                );
                let listing = egui::Rect::from_min_max(
                    egui::pos2(area.left(), ui.cursor().min.y),
                    egui::pos2(area.right(), band.top().max(ui.cursor().min.y)),
                );
                let pitch = row_height + ui.spacing().item_spacing.y;
                let window_len = listing_rows(listing.height(), pitch);
                let mut list_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(listing)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                // Whatever the rows turn out to measure, and whatever the
                // program carries, nothing of the listing reaches the band.
                list_ui.set_clip_rect(listing.intersect(ui.clip_rect()));
                if let Some(content) = content {
                    // Follow the line motion is cutting, not the interpreter's
                    // read-ahead, which reaches the end of the program first.
                    let current = app.snapshot.program.executing_line().max(1);
                    let total = app.snapshot.program.line_count.max(1);
                    let is_running = app.snapshot.task.running || app.snapshot.program.paused;
                    // While the machine runs, the pane follows the line being
                    // cut; when it is idle it follows the operator's selection,
                    // so Ctrl+G moves the window instead of being ignored.
                    let focus = if is_running {
                        current
                    } else {
                        app.program.selected_line.clamp(1, total)
                    };
                    let first = if is_running {
                        listing_first_line(focus, window_len, true, total)
                    } else {
                        listing_follow(
                            app.program.listing_first.get(),
                            focus,
                            window_len,
                            total,
                        )
                    };
                    // Remembered either way, so the handover when a run ends
                    // does not throw the window back to where it was idle.
                    app.program.listing_first.set(first);
                    let mut lines = content.lines().skip((first.max(1) - 1) as usize);
                    let number_font = FontId::monospace(theme::font_size(7.5));
                    let number_width = ui.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                "0000".to_string(),
                                number_font.clone(),
                                theme::MUTED,
                            )
                            .size()
                            .x
                    });
                    let text_lane =
                        (list_ui.available_width() - theme::SELECTION_BAR - number_width - 8.0)
                            .max(40.0);
                    for row in 0..window_len {
                        let line_number = first + row as isize;
                        let line = if line_number >= 1 { lines.next() } else { None };
                        let is_current = line_number == focus as isize;
                        let galley = line.map(|line| {
                            let mut job = token_job(line, 9.5);
                            job.wrap.max_width = text_lane;
                            ui.fonts_mut(|fonts| fonts.layout_job(job))
                        });
                        let height = galley
                            .as_ref()
                            .map_or(row_height, |galley| galley.size().y.max(row_height))
                            + ui.spacing().item_spacing.y;
                        // Whole rows inside the listing only: a row that would
                        // cross the band's top stays unpainted, so wrapped
                        // lines cost rows instead of reaching the band.
                        // A row that would cross the band stays unpainted, so
                        // wrapped lines cost rows instead of reaching the band -
                        // except the line being cut: that one is the reason the
                        // operator is looking, so it is painted and clipped.
                        if list_ui.cursor().min.y + height > listing.max.y + 0.5 && !is_current {
                            break;
                        }
                        let (rect, _) = list_ui.allocate_exact_size(
                            egui::vec2(list_ui.available_width(), height),
                            egui::Sense::hover(),
                        );
                        if is_current {
                            // The band goes down before the row's text, so the
                            // tokens are painted over it rather than tinted by
                            // it.
                            selection_band(&list_ui, rect);
                        }
                        let mut row_ui = list_ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("program_row", row))
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Min)),
                        );
                        row_ui.set_clip_rect(rect.intersect(list_ui.clip_rect()));
                        row_ui.spacing_mut().item_spacing.x = 4.0;
                        // Lane for the band's rim bar, so the line numbers start
                        // at the same x on every row.
                        let _ = row_ui.allocate_exact_size(
                            egui::vec2(theme::SELECTION_BAR, height),
                            egui::Sense::hover(),
                        );
                        let number = line
                            .map(|_| format!("{line_number:>4}"))
                            .unwrap_or_else(|| "    ".to_string());
                        row_ui.label(
                            RichText::new(number)
                                .monospace()
                                .size(theme::font_size(7.5))
                                .color(if is_current {
                                    theme::ACCENT
                                } else {
                                    theme::MUTED
                                }),
                        );
                        if let Some(galley) = galley {
                            row_ui.painter().galley(
                                egui::pos2(
                                    rect.left()
                                        + theme::SELECTION_BAR
                                        + number_width
                                        + 8.0,
                                    rect.top() + 1.0,
                                ),
                                galley,
                                theme::FG,
                            );
                        }
                    }
                } else {
                    for _ in 0..window_len {
                        if list_ui.cursor().min.y + row_height > listing.max.y + 0.5 {
                            break;
                        }
                        let _ = list_ui.allocate_exact_size(
                            egui::vec2(list_ui.available_width(), row_height),
                            egui::Sense::hover(),
                        );
                    }
                }
                // The band, pinned to its rect above the progress line.
                let mut band_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(band)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                band_ui.set_clip_rect(band.intersect(ui.clip_rect()));
                theme::separator(&mut band_ui);
                field_label(&mut band_ui, "MODAL");
                modal_grid(app, &mut band_ui, 6, false, "program_modal_fields");

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        let status = if app.snapshot.task.running {
                            "RUNNING"
                        } else if app.snapshot.program.paused {
                            "PAUSED"
                        } else {
                            "STOP"
                        };
                        ui.label(
                            RichText::new(format!(
                                "LN {}/{}  {:.0}%",
                                app.snapshot.program.executing_line(),
                                app.snapshot.program.line_count,
                                app.snapshot.program.progress * 100.0
                            ))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(status)
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(if app.snapshot.task.running {
                                        theme::SUCCESS
                                    } else {
                                        theme::MUTED
                                    }),
                            );
                        });
                    });
                    ui.add(
                        egui::ProgressBar::new(app.snapshot.program.progress.clamp(0.0, 1.0))
                            .desired_width(ui.available_width())
                            .desired_height(6.0)
                            .fill(theme::ACCENT)
                            .text(""),
                    );
                    field_label(ui, "PROGRESS");
                    theme::separator(ui);
                });
            },
        );
    });
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "True"
    } else {
        "False"
    }
}

/// Small rotation arrow beside the spindle `S`: clockwise for forward,
/// counter-clockwise for reverse, and a dot when stopped. It is drawn as
/// vectors so it never depends on font glyph coverage.
fn spindle_direction_mark(ui: &Ui, rect: egui::Rect, center: egui::Pos2, direction: i8, on: bool) {
    let painter = ui.painter_at(rect);
    if !on || direction == 0 {
        painter.circle_filled(center, 2.0, theme::MUTED);
        return;
    }
    let color = if direction > 0 {
        theme::SUCCESS
    } else {
        theme::WARNING
    };
    let radius = 6.5;
    let start = 300.0_f32.to_radians();
    let sweep = 300.0_f32.to_radians();
    let steps = 20;
    let mut points = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let angle = start + sweep * step as f32 / steps as f32;
        points.push(center + Vec2::new(angle.cos(), angle.sin()) * radius);
    }
    let stroke = egui::Stroke::new(1.5, color);
    painter.add(egui::Shape::line(points, stroke));
    // The head sits at the clockwise end for forward, and at the other end
    // pointing the other way for reverse, so the same arc reads either way.
    let (head_angle, sign) = if direction > 0 {
        (start + sweep, 1.0)
    } else {
        (start, -1.0)
    };
    let tip = center + Vec2::new(head_angle.cos(), head_angle.sin()) * radius;
    let tangent = Vec2::new(-head_angle.sin(), head_angle.cos()) * sign;
    let back = tip - tangent * 4.2;
    let side = Vec2::new(-tangent.y, tangent.x) * 2.4;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side, back - side],
        color,
        egui::Stroke::NONE,
    ));
}

/// The two F rows: the programmed F word and its override, then the effective
/// rate (the word scaled by the override when it is enabled) and its flag.
fn feed_rows(feed: &linc_core::model::FeedState) -> [(String, String, String); 2] {
    let units = if feed.units.is_empty() {
        "mm"
    } else {
        feed.units.as_str()
    };
    let effective = if feed.feed_override_enabled {
        feed.feed * feed.feed_override
    } else {
        feed.feed
    };
    // G95 is feed per spindle revolution and G93 is inverse time; both are
    // fractional, and the lathe's usual 0.008 in/rev rounded to "0" when shown
    // as whole units per minute.
    let (unit, digits) = match feed.feed_mode.as_str() {
        "G95" => (format!("{units}/rev"), 3),
        "G93" => ("1/min".to_string(), 3),
        _ => (format!("{units}/min"), 0),
    };
    [
        (
            format!("{:.*}", digits, feed.feed),
            unit.clone(),
            format!("{:.0}%", feed.feed_override * 100.0),
        ),
        (
            format!("{:.*}", digits, effective),
            unit,
            format!("({})", yes_no(feed.feed_override_enabled)),
        ),
    ]
}

/// Feed and spindle block in the machine-HMI layout: a large `F` or `S` on the
/// left, two right-aligned value rows beside it. The first row is the
/// programmed rate and its override, the second the effective value and its
/// enable/on flag. The spindle block carries its rotation direction beside the
/// letter.
fn feed_speed_block(
    ui: &Ui,
    rect: egui::Rect,
    letter: &str,
    rows: &[(String, String, String); 2],
    spindle: Option<(i8, bool)>,
) {
    let painter = ui.painter_at(rect);
    // The lane leaves room beside the letter for the spindle's direction arrow.
    let lane = 42.0;
    let gap = 6.0;
    let detail_size = 9.0;
    let detail_font = FontId::monospace(theme::font_size(detail_size));
    let measure = |text: &str, font: &FontId| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_string(), font.clone(), theme::FG)
                .size()
                .x
        })
    };
    let units_width = rows
        .iter()
        .map(|(_, units, _)| measure(units, &detail_font))
        .fold(0.0f32, f32::max);
    let extra_width = rows
        .iter()
        .map(|(_, _, extra)| measure(extra, &detail_font))
        .fold(0.0f32, f32::max);
    // The feed/RPM values are the primary reading, so they get the larger face,
    // shrinking only as far as the narrower half requires. The units and the
    // override/flag stay in the smaller detail face.
    let mut value_size = 14.0;
    let value_width = loop {
        let font = FontId::monospace(theme::font_size(value_size));
        let width = rows
            .iter()
            .map(|(value, _, _)| measure(value, &font))
            .fold(0.0f32, f32::max);
        let total = lane + width + gap + units_width + gap + extra_width;
        if total <= rect.width() - 2.0 || value_size <= 10.0 {
            break width;
        }
        value_size -= 0.5;
    };
    painter.text(
        rect.left_center() + Vec2::new(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        letter,
        FontId::proportional(theme::font_size(32.0)),
        theme::SUCCESS,
    );
    if let Some((direction, on)) = spindle {
        spindle_direction_mark(
            ui,
            rect,
            egui::pos2(rect.left() + 30.0, rect.center().y - 12.0),
            direction,
            on,
        );
    }
    let value_font = FontId::monospace(theme::font_size(value_size));
    let value_right = rect.left() + lane + value_width;
    let units_left = value_right + gap;
    // The override/flag column starts right after the units instead of being
    // pushed to the far edge, which left a wide hole before `100%`.
    let extra_left = units_left + units_width + gap;
    for (index, (value, units, extra)) in rows.iter().enumerate() {
        let y = rect.center().y + (index as f32 - 0.5) * 24.0;
        painter.text(
            egui::pos2(value_right, y),
            egui::Align2::RIGHT_CENTER,
            value,
            value_font.clone(),
            theme::FG,
        );
        painter.text(
            egui::pos2(units_left, y),
            egui::Align2::LEFT_CENTER,
            units,
            detail_font.clone(),
            theme::SUCCESS,
        );
        painter.text(
            egui::pos2(extra_left, y),
            egui::Align2::LEFT_CENTER,
            extra,
            detail_font.clone(),
            theme::SUCCESS,
        );
    }
}

pub fn feed_speed_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(0.0));
        ui.set_max_height((height - 24.0).max(0.0));
        ui.set_min_width(ui.available_width());
        theme::section_header(ui, "FEED / SPINDLE");
        let width = ui.available_width();
        let body_height = (height - 52.0).max(48.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, body_height), Sense::hover());
        let half = rect.width() * 0.5;
        let left = egui::Rect::from_min_size(rect.min, Vec2::new(half - 6.0, body_height));
        let right = egui::Rect::from_min_size(
            egui::pos2(rect.center().x + 6.0, rect.top()),
            Vec2::new(half - 6.0, body_height),
        );
        ui.painter().line_segment(
            [
                egui::pos2(rect.center().x, rect.top() + 4.0),
                egui::pos2(rect.center().x, rect.bottom() - 4.0),
            ],
            egui::Stroke::new(1.0, theme::BORDER),
        );

        let feed_rows = feed_rows(&app.snapshot.feed);
        feed_speed_block(ui, left, "F", &feed_rows, None);

        let (commanded, actual, override_factor, on) = app
            .snapshot
            .spindles
            .first()
            .map(|spindle| {
                (
                    spindle.commanded_speed,
                    spindle.actual_speed,
                    spindle.override_factor,
                    spindle.on,
                )
            })
            .unwrap_or((0.0, 0.0, 1.0, false));
        let spindle_rows = [
            (
                format!("{commanded:.0}"),
                "RPM".to_string(),
                format!("{:.0}%", override_factor * 100.0),
            ),
            (
                format!("{actual:.0}"),
                "RPM".to_string(),
                format!("({})", yes_no(on)),
            ),
        ];
        let direction = app
            .snapshot
            .spindles
            .first()
            .map(|spindle| (spindle.direction, spindle.on))
            .unwrap_or((0, false));
        feed_speed_block(ui, right, "S", &spindle_rows, Some(direction));
    });
}

pub fn turret_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "POWER TURRET / LIVE TOOL");
        let Some(atc) = app.snapshot.atc.as_ref() else {
            ui.label(RichText::new("NO TURRET CONFIGURED").color(theme::MUTED));
            return;
        };
        ui.horizontal(|ui| {
            theme::label_value(
                ui,
                "STATION",
                atc.station
                    .map(|station| format!("T{:02}", station))
                    .unwrap_or_else(|| "—".into()),
                26.0,
            );
            ui.add_space(18.0);
            theme::label_value(
                ui,
                "CLAMPED",
                atc.clamped
                    .map(|clamped| if clamped { "CLAMPED" } else { "UNCLAMPED" })
                    .unwrap_or("UNKNOWN"),
                14.0,
            );
            ui.add_space(18.0);
            theme::label_value(
                ui,
                "INDEX",
                if atc.indexing { "INDEXING" } else { "READY" },
                14.0,
            );
        });
        ui.add_space(6.0);
        theme::kv_row(
            ui,
            "LIVE TOOL",
            atc.live_tool_rpm
                .map(|rpm| {
                    format!(
                        "{rpm:.0} RPM {}",
                        if atc.live_tool_direction >= 0 {
                            "CW"
                        } else {
                            "CCW"
                        }
                    )
                })
                .unwrap_or_else(|| "OFF".into()),
        );
        theme::kv_row(
            ui,
            "ORIENTATION",
            match atc.live_tool_axial {
                Some(true) => "AXIAL",
                Some(false) => "RADIAL",
                None => "UNKNOWN",
            },
        );
        if let Some(fault) = &atc.fault {
            ui.label(RichText::new(fault).color(theme::ERROR));
        }
    });
}

pub fn trajectory(
    app: &LincApp,
    ui: &mut Ui,
    preview: &Preview,
    height: f32,
    view_mode: ViewMode,
    zoom: &mut f32,
    pan: &mut Vec2,
    yaw: &mut f32,
    pitch: &mut f32,
    fit: &mut bool,
    auto_fit: &mut bool,
    selected_line: Option<usize>,
    current_line: Option<usize>,
    // Legends name the colours; a thumbnail too small to read them omits it.
    show_legend: bool,
    // Frame the tool tip, trail and origin markers as well as the program.
    fit_all: bool,
    // Draw the machine-origin triad and the line to the work origin.
    origin_tether: bool,
    // Keep the machine envelope on regardless of the Ctrl+B cycle.
    machine_bounds_always: bool,
    // A second origin marker, in the current-line colour: the OFFSETS page
    // highlights the row being edited without moving the active cross.
    highlight: Option<([f64; 3], String)>,
) -> egui::Response {
    // Machine coordinates throughout: the part, the trail and the tool stay put
    // when the operator touches off, and the work-zero cross moves instead.
    let origin = app.graph_origin;
    let mut view = TrajectoryView::new(
        preview,
        app.snapshot.live_trail.as_slice(),
        app.profile.variant,
    );
    view.origin = origin;
    view.work_offset = app.program.preview_offset.as_ref().map(|map| {
        [
            map.get("X").copied().unwrap_or(0.0),
            map.get("Y").copied().unwrap_or(0.0),
            map.get("Z").copied().unwrap_or(0.0),
        ]
    });
    if let Some((point, label)) = highlight {
        view.highlight_origin = Some(point);
        view.highlight_label = Some(label);
    }
    view.view = view_mode;
    view.height = height;
    view.machine_bounds = app.machine_bounds();
    view.zoom = *zoom;
    view.pan = *pan;
    view.yaw = *yaw;
    view.pitch = *pitch;
    view.fit = *fit;
    view.auto_fit = *auto_fit;
    view.fit_all = fit_all;
    view.origin_tether = origin_tether;
    view.machine_bounds_always = machine_bounds_always;
    view.selected_line = selected_line;
    view.current_line = current_line;
    view.axis_labels = app.axes();
    view.tool_tip = Some(app.tool_tip());
    app.graph_style.apply(
        &mut view,
        selected_line,
        current_line,
        eaten_through(&app.graph_style, &app.snapshot.program),
    );
    // Derived from the live positions, so it absorbs the G43 tool length (which
    // LinuxCNC folds into the machine position but the program path does not) and
    // any G92 or WCS mismatch. Recomputed here rather than by the Program page,
    // so every card that draws a trail is correct in the frame it is drawn.
    view.live_trail_shift =
        crate::state::live_trail_shift(&app.snapshot, app.program.preview_offset.as_ref());
    view.show_legend = view.show_legend && show_legend;
    let response = draw_trajectory(ui, &mut view);
    // A click on the card hands it the keyboard. The shell's pan chords are
    // hover-scoped *and* behind "no text field is focused" — Ctrl+Left/Right is
    // the G-code editor's word move, Ctrl+Up/Down its line move — and the panel
    // keyboard has no Tab key to move focus with. An editor that took focus on
    // page entry therefore held the keyboard for the rest of the session and
    // the chords were dead with no way back short of a dialog. Clicking the
    // toolpath is the operator saying "look here instead", so it takes focus.
    if response.clicked() || response.double_clicked() || response.drag_started() {
        response.request_focus();
    }
    if preview.incomplete {
        preview_incomplete_marker(ui, response.rect, preview.incomplete_reason.as_deref());
    }
    *zoom = view.zoom;
    *pan = view.pan;
    *yaw = view.yaw;
    *pitch = view.pitch;
    *fit = view.fit;
    *auto_fit = view.auto_fit;
    response
}

/// The last block the REMAINING view may eat: the interpreter's, and only in that
/// mode.
///
/// The mode is the switch, not the line: a card that draws a program the machine
/// ran shows the whole path until the operator asks for the progress view.
fn eaten_through(
    style: &crate::state::GraphStyle,
    program: &linc_core::model::ProgramState,
) -> Option<usize> {
    if style.remaining {
        program.consumed_through()
    } else {
        None
    }
}

/// Say so when the preview cannot be trusted.
///
/// The parser is not the interpreter: a construct it cannot evaluate, a
/// subroutine call it cannot resolve or a budget it hit leaves a path that is
/// shorter than the program. Painting that silently is what made a missing unit
/// conversion look like a rendering bug, so the card carries the warning and the
/// reason on hover.
fn preview_incomplete_marker(ui: &Ui, rect: egui::Rect, reason: Option<&str>) {
    let text = "PREVIEW INCOMPLETE";
    let font = FontId::proportional(theme::font_size(9.0));
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::WARNING);
    let pos = egui::pos2(
        rect.right() - 12.0 - galley.size().x,
        rect.bottom() - 8.0 - galley.size().y,
    );
    ui.painter().galley(pos, galley.clone(), theme::WARNING);
    if let Some(reason) = reason {
        ui.interact(
            egui::Rect::from_min_size(pos, galley.size()),
            ui.scope_id().with("preview-incomplete"),
            Sense::hover(),
        )
        .on_hover_text(reason);
    }
}

pub fn clear_trail_button(app: &mut LincApp, ui: &mut Ui) {
    if theme::action_button(ui, "CLEAR TRAIL", true).clicked() {
        app.clear_trail();
    }
}

pub fn displayed_wcs(app: &LincApp) -> String {
    app.active_wcs_label()
}

pub fn work_positions(app: &LincApp) -> AxisMap<f64> {
    let mut map = AxisMap::new();
    for (axis, position) in app.snapshot.positions.axes.iter() {
        map.insert(axis.to_string(), position.work);
    }
    map
}

/// The position one DRO shows for one axis, per `[ui] dro_position`.
///
/// Returns `(work, machine, dtg)` in that order. `work` and `machine` are the
/// configured frame; `dtg` always stays the controller's distance to go.
/// `None` means the snapshot has no position entry for this axis at all, which
/// a backend can produce before its first poll.
pub fn axis_display(app: &LincApp, axis: &str) -> Option<(f64, f64, f64)> {
    let position = *app.snapshot.positions.axes.get(axis)?;
    Some(dro_frame(
        app.profile.config.ui.dro_position.as_str(),
        app.snapshot.machine.commanded.get(axis).copied(),
        position,
    ))
}

/// Apply the configured DRO position source to one axis's feedback position.
///
/// `"commanded"` shows the commanded machine coordinate — falling back to the
/// feedback machine position when a backend has not populated `machine.commanded`
/// yet, so the readout still shows numbers — and applies the same work offset
/// the feedback position carries: `work = machine + (actual_work - actual_machine)`.
/// `"actual"` returns the feedback position unchanged. Either way `dtg` stays
/// the controller's value.
fn dro_frame(
    source: &str,
    commanded_machine: Option<f64>,
    position: PositionSet,
) -> (f64, f64, f64) {
    match source {
        "commanded" => {
            let machine = commanded_machine.unwrap_or(position.machine);
            let work = machine + (position.work - position.machine);
            (work, machine, position.dtg)
        }
        _ => (position.work, position.machine, position.dtg),
    }
}

/// Keyboard focus stop with nothing drawn.
///
/// A page needs one focusable widget at the top so arrow-key navigation can
/// start there (egui only bootstraps focus from Tab). Pages that do not want a
/// visible title use this instead of [`page_header`].
pub fn focus_anchor(ui: &mut Ui, salt: &str) {
    let _ = focus_anchor_locked(ui, salt, false);
}

/// Like [`focus_anchor`], but returns the response. `lock_arrows` keeps the
/// arrow keys on the page instead of letting egui walk the focus into another
/// widget (the Help table uses Up/Down for its own selection).
pub fn focus_anchor_locked(ui: &mut Ui, salt: &str, lock_arrows: bool) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    let response = ui.interact(
        rect,
        ui.scope_id().with(salt),
        Sense::focusable_noninteractive(),
    );
    let requested = ui.ctx().data_mut(|data| {
        data.remove_temp::<bool>(egui::Id::unique("linc-page-focus"))
            .unwrap_or(false)
    });
    if requested {
        response.request_focus();
    }
    if lock_arrows {
        let _ = ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            )
        });
    }
    // No focus ring: the anchor exists only so arrow-key navigation has
    // somewhere to start, and a ring around an empty full-width strip reads as
    // a stray empty control. Real widgets still draw their own rings.
    response
}

pub fn mode_label(app: &LincApp) -> String {
    app.snapshot
        .panel
        .mode
        .mode()
        .map(|mode| mode.label().to_string())
        .unwrap_or_else(|| "UNKNOWN".into())
}

pub fn is_lathe(app: &LincApp) -> bool {
    app.profile.variant == MachineVariant::Lathe
}

pub fn panel_mode_is(app: &LincApp, mode: PanelMode) -> bool {
    app.snapshot.panel.mode.mode() == Some(mode)
}

pub fn nav_badge(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(
        RichText::new(text)
            .monospace()
            .size(theme::font_size(10.0))
            .color(color),
    );
}

pub fn file_size(size: u64) -> String {
    crate::files::format_size(size)
}

pub fn read_only_hint(ui: &mut Ui, reason: &str) {
    ui.label(
        RichText::new(reason)
            .size(theme::font_size(10.0))
            .color(theme::WARNING),
    );
}

pub fn snapshot_axes(snapshot: &MachineSnapshot) -> Vec<String> {
    snapshot.positions.axes.keys().map(str::to_string).collect()
}

pub fn spacer(ui: &mut Ui, height: f32) {
    ui.add_space(height);
    let _ = Sense::hover();
}

pub fn centered_notice(ui: &mut Ui, text: &str, color: Color32) {
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 120.0),
        Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.label(
                RichText::new(text)
                    .size(theme::font_size(14.0))
                    .color(color),
            );
        },
    );
}

pub fn small_meter_row(ui: &mut Ui, label: &str, fraction: f64, text: &str) {
    theme::meter(ui, label, fraction, text, theme::pressure_color(fraction));
}

pub fn page_tab(ui: &mut Ui, label: &str, active: bool) -> egui::Response {
    let response = ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(theme::font_size(11.0))
                .strong()
                .color(if active {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    theme::FG
                }),
        )
        .fill(if active { theme::ACCENT } else { theme::PANEL })
        .stroke(egui::Stroke::new(
            1.0,
            if active { theme::ACCENT } else { theme::BORDER },
        ))
        .min_size(Vec2::new(0.0, 34.0)),
    );
    theme::focus_ring(ui, &response);
    response
}

pub fn big_position(ui: &mut Ui, label: &str, value: f64, unit: &str) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::theme::format_position(value, 3))
                    .monospace()
                    .size(theme::font_size(30.0))
                    .color(theme::FG),
            );
            ui.label(
                RichText::new(unit)
                    .size(theme::font_size(11.0))
                    .color(theme::MUTED),
            );
        });
    });
}

pub fn goto_page(app: &mut LincApp, page: Page) {
    app.request_page(page);
}

pub fn mode_page_hint(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
    );
}

pub fn font_mono(size: f32) -> FontId {
    FontId::monospace(size)
}

// ---------------------------------------------------------------------------
// Shared "live mode" layout base
//
// The panel's JOG/INC/MPG positions (Manual screen) and its HOME/PROG.HOME
// positions (Homing screen) are both read-only, mode-driven observation
// screens: a dominant live graph beside a large coordinate readout and a mode
// context card. They deliberately share one layout base so the two screens do
// not drift apart.
// ---------------------------------------------------------------------------

/// One axis row in the live coordinate readout.
pub struct LiveAxisRow {
    pub name: String,
    pub work: f64,
    pub machine: f64,
    /// Distance to go. `None` hides the field: homing is about homed state and
    /// machine position, where a remaining distance means nothing.
    pub dtg: Option<f64>,
    /// Highlighted because the panel reports it as the selected jog axis.
    pub active: bool,
    /// Optional state chip, e.g. `HOMED` on the Homing screen.
    pub badge: Option<(String, egui::Color32)>,
    /// Optional second line, e.g. the INI sequence/search/latch detail.
    pub note: Option<String>,
}

/// Two-column live split: dominant graph on the left, scrollable readout on
/// the right. `salt` keeps the scroll state separate per screen. Both panes
/// receive `app` as an argument, so the caller never needs two live borrows.
pub fn live_split<R1, R2>(
    app: &mut LincApp,
    ui: &mut Ui,
    salt: &str,
    left: impl FnOnce(&mut LincApp, &mut Ui) -> R1,
    right: impl FnOnce(&mut LincApp, &mut Ui) -> R2,
) {
    let available = ui.available_size();
    let graph_width = (available.x * 0.68).max(560.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                left(app, ui);
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - graph_width - 8.0).max(320.0), available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(salt)
                    .max_height(available.y - 10.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        right(app, ui);
                    });
            },
        );
    });
}

/// Live toolpath card for the manual modes.
///
/// `FIT` and `CLEAR TRAIL` are keyboard commands, so the header carries no
/// buttons.
pub fn live_graph_card(app: &mut LincApp, ui: &mut Ui, card_height: f32) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), card_height.max(160.0)),
        egui::Sense::hover(),
    );
    shell_card(ui, rect, |ui| {
        let body_top = ui.cursor().top();
        toolpath_header(ui, app.manual.view.label(), app.manual.zoom, app.graph_style.remaining);
        let header_height = ui.cursor().top() - body_top;
        let graph_height = (rect.height() - 8.0 - header_height - 2.0).max(60.0);
        let preview = app.program.last_valid_preview.clone();
        let current_line = app.snapshot.program.highlight_line();
        let view = app.manual.view;
        let mut zoom = app.manual.zoom;
        let mut pan = app.manual.pan;
        let mut yaw = app.manual.yaw;
        let mut pitch = app.manual.pitch;
        let mut fit = app.manual.fit;
        let mut auto_fit = app.manual.auto_fit;
        trajectory(
            app,
            ui,
            &preview,
            graph_height,
            view,
            &mut zoom,
            &mut pan,
            &mut yaw,
            &mut pitch,
            &mut fit,
            &mut auto_fit,
            None,
            current_line,
            false,
            false,
            false,
            false,
            None,
        );
        app.manual.zoom = zoom;
        app.manual.pan = pan;
        app.manual.yaw = yaw;
        app.manual.pitch = pitch;
        app.manual.fit = fit;
        app.manual.auto_fit = auto_fit;
    });
}


/// Touch-off card shared by every live mode.
///
/// Sets the current position as the zero of the active work offset for one axis
/// or all of them. The work is done by `G10 L20`, so LinuxCNC calculates the
/// offset from the position it holds rather than the HMI deriving it from a
/// snapshot that may already be a cycle old.
pub fn zero_point_card(app: &mut crate::shell::LincApp, ui: &mut Ui) {
    let index = app.snapshot.offsets.active_index;
    let axes: Vec<String> = app.axes();
    // `writes_allowed` already covers machine ON, idle, not running, program
    // protect and an E-stopped or offline panel.
    let allowed = app.snapshot.writes_allowed() && index.is_some();
    let mut chosen: Option<Vec<String>> = None;
    // One compact row so the control stays above the fold next to the DRO.
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("SET ZERO")
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        for axis in &axes {
            let response = ui
                .add_enabled(
                    allowed,
                    egui::Button::new(RichText::new(format!("{axis}0")).monospace()),
                )
                .on_disabled_hover_text("machine must be ON, homed and idle");
            if response.clicked() {
                chosen = Some(vec![axis.clone()]);
            }
        }
        let response = ui
            .add_enabled(
                allowed,
                egui::Button::new(RichText::new("ALL0").monospace().strong()),
            )
            .on_disabled_hover_text("machine must be ON, homed and idle");
        if response.clicked() {
            chosen = Some(axes.clone());
        }
        ui.label(
            RichText::new(app.active_wcs_label())
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::ACCENT),
        );
    });
    if let Some(axes) = chosen {
        zero_axes(app, axes);
    }
}

/// Touch off: make the current position read zero in the active work offset.
///
/// This is `G10 L20`, so the controller computes the offset from the position it
/// holds — the stored G54 value changes and the DRO reads zero. Shared by the
/// axis keys and any on-screen control, so both go through the same gate.
pub fn zero_axes(app: &mut crate::shell::LincApp, axes: Vec<String>) {
    if !app.snapshot.writes_allowed() {
        app.toast("machine must be ON, homed and idle", theme::WARNING);
        return;
    }
    let Some(index) = app.snapshot.offsets.active_index else {
        app.toast("no active work offset", theme::WARNING);
        return;
    };
    app.send(linc_core::command::MachineCommand::ZeroWorkOffset { index, axes });
}

/// Room above each chip for its digit: the disc is 14 px across, plus air.
const INCREMENT_DISC_ROW: f32 = 20.0;

/// MAIN's bottom row when the square key is on: every axis's following error in
/// one lane, one colour each, on a scale they share.
///
/// Following error is the distance between where the controller is and where the
/// position loop wants it. One lane makes the axes comparable at a glance, and a
/// trace shows a sticky axis or a loose servo while it happens. The header
/// carries the numbers in three labelled groups - the live value, the largest
/// magnitude in the window, the mean magnitude - so the picture answers "how
/// bad, and how often" without reading the curve.
pub fn following_error_card(app: &LincApp, ui: &mut Ui, height: f32) {
    // This card's own frame: the header carries the numbers, so the air above it
    // is the one padding it cannot use. Four pixels instead of the shared
    // twelve.
    const TOP: f32 = 2.0;
    const BOTTOM: f32 = 2.0;
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: TOP as i8,
            bottom: BOTTOM as i8,
        })
        .show(ui, |ui| {
            let body = (height - TOP - BOTTOM).max(0.0);
            ui.set_min_height(body);
            ui.set_max_height(body);
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let axes = app.axes();
            let traces: Vec<(Vec<f64>, egui::Color32)> = axes
                .iter()
                .enumerate()
                .map(|(index, axis)| {
                    (
                        app.following_history
                            .get(axis)
                            .map(|samples| samples.iter().copied().collect())
                            .unwrap_or_default(),
                        trace_color(index),
                    )
                })
                .collect();
            // A horizontal row is as tall as `interact_size.y`, which the theme
            // sets to a 44 px touch target. This row is one line of text, so
            // give it the line's own height and hand the rest to the lane.
            let line = header_line_height(ui);
            ui.spacing_mut().interact_size.y = line;
            let header_top = ui.cursor().top();
            ui.horizontal(|ui| {
                // Same size as the numbers beside it: the header is one line,
                // and a 12 pt title would set the row's height for it.
                ui.label(
                    RichText::new("FOLLOWING ERROR")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .strong()
                        .color(theme::MUTED),
                );
                ui.add_space(10.0);
                // One label per group, the axes inside it in their own colours:
                // those colours are the lane's key. The live values need no
                // label - they sit right beside the title.
                for (index, axis) in axes.iter().enumerate() {
                    let now = app
                        .snapshot
                        .machine
                        .following_error
                        .get(axis)
                        .copied()
                        .unwrap_or(0.0);
                    axis_number(ui, axis, format!("{now:+0.3}"), trace_color(index));
                }
                ui.add_space(8.0);
                group_label(ui, "\u{1f5d6}");
                for (index, axis) in axes.iter().enumerate() {
                    let (worst, _) = app.following_error_summary(axis);
                    axis_number(ui, axis, format!("{worst:.3}"), trace_color(index));
                }
                ui.add_space(8.0);
                diameter_glyph(ui);
                for (index, axis) in axes.iter().enumerate() {
                    let (_, mean) = app.following_error_summary(axis);
                    axis_number(ui, axis, format!("{mean:.3}"), trace_color(index));
                }

            });
            // The lane takes everything the header leaves: one lane, maximised,
            // not a stack of rows.
            let used = ui.cursor().top() - header_top;
            let lane = (body - used).max(24.0);
            let series: Vec<(&[f64], egui::Color32)> = traces
                .iter()
                .map(|(values, color)| (values.as_slice(), *color))
                .collect();
            linc_view::draw_traces(ui, &series, lane);
        });
}

/// The diameter sign, drawn rather than typed: U+2300 is in none of egui's
/// bundled fonts (Hack, Ubuntu-Light, the emoji faces), so the character would
/// arrive on screen as an empty box. A circle with a slash is the same mark and
/// depends on no font at all.
fn diameter_glyph(ui: &mut Ui) {
    let size = theme::font_size(9.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center = rect.center();
    let radius = size * 0.36;
    let stroke = egui::Stroke::new(1.1, theme::MUTED);
    ui.painter().circle_stroke(center, radius, stroke);
    ui.painter().line_segment(
        [
            center + egui::vec2(-radius, radius),
            center + egui::vec2(radius, -radius),
        ],
        stroke,
    );
    ui.add_space(5.0);
}

/// The height one line of the header text needs.
///
/// A `ui.horizontal` row is as tall as `interact_size.y` - the theme sets it
/// to `MIN_TARGET`, a 44 px touch target - so a row of labels would claim a
/// third of the strip. The row is given the text height instead.
fn header_line_height(ui: &Ui) -> f32 {
    ui.fonts_mut(|fonts| fonts.row_height(&egui::FontId::monospace(theme::font_size(9.0))))
}

fn group_label(ui: &mut Ui, label: &str) {
    ui.label(
        RichText::new(label)
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    ui.add_space(5.0);
}

/// `X+0.012`: the axis letter carries the identity, the colour ties it to its
/// trace.
fn axis_number(ui: &mut Ui, axis: &str, value: String, color: egui::Color32) {
    ui.label(
        RichText::new(format!("{axis}{value}"))
            .monospace()
            .size(theme::font_size(9.0))
            .color(color),
    );
    ui.add_space(4.0);
}

/// One colour per axis, in the order the machine lists them.
fn trace_color(index: usize) -> egui::Color32 {
    match index % 3 {
        0 => linc_view::theme::ACCENT,
        1 => linc_view::theme::SUCCESS,
        _ => linc_view::theme::WARNING,
    }
}

/// The jog increment the operator picked from `[DISPLAY] INCREMENTS`.
///
/// The panel has one keypad and no spare keys, so a step is picked by its
/// number: the chips carry the same numbers the keyboard takes, and the
/// selected one is the fill the rest of the HMI uses for "this is the value".
pub fn increment_row(app: &mut LincApp, ui: &mut Ui) {
    let steps = app.increments().to_vec();
    if steps.is_empty() {
        return;
    }
    let selected = app.jog_increment_index();
    // One width for every chip and every disc: the operator aims at a column,
    // and a row of buttons sized by their text reads as a ragged sentence.
    let chip_width = steps
        .iter()
        .map(|(label, _)| theme::measure(ui, label, 10.0))
        .fold(0.0_f32, f32::max)
        + 28.0;
    let mut pick = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "INCREMENT");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("1-{}", steps.len()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        ui.horizontal_wrapped(|ui| {
            for (index, (label, _)) in steps.iter().enumerate() {
                // The digit is the key, so it sits above its chip in a disc,
                // centred on the column: the button carries the value alone.
                ui.vertical(|ui| {
                    let (disc, _) = ui.allocate_exact_size(
                        Vec2::new(chip_width, INCREMENT_DISC_ROW),
                        Sense::hover(),
                    );
                    let centre = disc.center();
                    let ink = if index == selected {
                        theme::ACCENT
                    } else {
                        theme::MUTED
                    };
                    ui.painter()
                        .circle_stroke(centre, 7.0, egui::Stroke::new(1.0, ink));
                    ui.painter().text(
                        centre,
                        egui::Align2::CENTER_CENTER,
                        format!("{}", index + 1),
                        FontId::monospace(theme::font_size(9.0)),
                        ink,
                    );
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if theme::chip_sized(ui, label.clone(), index == selected, chip_width)
                        .clicked()
                    {
                        pick = Some(index);
                    }
                });
                ui.add_space(6.0);
            }
        });
    });
    if let Some(index) = pick {
        app.set_jog_increment_index(index);
    }
}

pub fn live_coordinates_card(ui: &mut Ui, header: &str, units_label: &str, rows: &[LiveAxisRow]) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, header);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(units_label)
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
            });
        });
        for row in rows {
            let frame = egui::Frame::new()
                .fill(if row.active {
                    theme::ACCENT_DARK
                } else {
                    theme::PANEL
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if row.active {
                        theme::ACCENT
                    } else {
                        theme::BORDER
                    },
                ))
                .corner_radius(3)
                .inner_margin(egui::Margin::symmetric(8, 4));
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    // The axis name is set at the coordinate's own size and
                    // on its line: one character does not need a row of its
                    // own, and the pair reads as one number.
                    ui.label(
                        RichText::new(&row.name)
                            .monospace()
                            .size(theme::font_size(22.0))
                            .strong()
                            .color(theme::C3),
                    );
                    if row.active {
                        ui.label(
                            RichText::new("ACTIVE")
                                .size(theme::font_size(8.0))
                                .color(theme::ACCENT),
                        );
                    }
                    if let Some((text, color)) = &row.badge {
                        ui.label(
                            RichText::new(text)
                                .monospace()
                                .size(theme::font_size(9.0))
                                .color(*color),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(dtg) = row.dtg {
                            ui.label(
                                RichText::new(format!("DTG {dtg:.3}"))
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(theme::MUTED),
                            );
                        }
                        ui.label(
                            RichText::new(theme::format_position(row.work, 3))
                                .monospace()
                                .size(theme::font_size(22.0))
                                .color(theme::FG),
                        );
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("MACH {:+.3}", row.machine))
                            .monospace()
                            .size(theme::font_size(8.5))
                            .color(theme::MUTED),
                    );
                    if let Some(note) = &row.note {
                        ui.label(
                            RichText::new(note)
                                .monospace()
                                .size(theme::font_size(8.0))
                                .color(theme::MUTED),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let legend = if row.dtg.is_some() {
                            "WORK / MACHINE / DTG"
                        } else {
                            "WORK / MACHINE"
                        };
                        ui.label(
                            RichText::new(legend)
                                .size(theme::font_size(8.0))
                                .color(theme::MUTED),
                        );
                    });
                });
            });
        }
    });
}

#[cfg(test)]
mod tests {
    /// The running line sits a third of the way down the listing: a third of
    /// past lines above it, two thirds of upcoming ones below.
    /// An idle listing scrolls only when the highlighted line leaves it: one
    /// line at a time, and a jump bigger than the window still centres.
    #[test]
    fn the_idle_listing_follows_the_highlighted_line_when_it_leaves() {
        // Inside the window: nothing moves, even at the edges.
        assert_eq!(super::listing_follow(1, 5, 18, 37), 1);
        assert_eq!(super::listing_follow(1, 18, 18, 37), 1);
        // One past the bottom: one line of scroll.
        assert_eq!(super::listing_follow(1, 19, 18, 37), 2);
        // Above the window: the window moves up to it.
        assert_eq!(super::listing_follow(20, 10, 18, 37), 10);
        // A jump larger than the window centres instead of pinning the line to
        // the edge...
        assert_eq!(super::listing_follow(1, 60, 18, 100), 51);
        // ...and never past the last full window, nor above line 1.
        assert_eq!(super::listing_follow(1, 37, 18, 37), 20);
        assert_eq!(super::listing_follow(40, 37, 18, 37), 20);
        assert_eq!(super::listing_follow(0, 1, 18, 37), 1);
    }

    #[test]
    fn the_running_line_keeps_its_place_a_third_down_the_listing() {
        for (focus, window) in [(1usize, 9usize), (2, 9), (5, 9), (40, 12), (1, 3), (7, 3)] {
            let first = super::listing_first_line(focus, window, true, 100);
            let slot = focus as isize - first;
            assert!(
                slot >= 0 && slot < window as isize,
                "focus {focus} in a {window}-row window lands at slot {slot} (first {first})"
            );
            assert_eq!(
                slot,
                (window / 3) as isize,
                "focus {focus} sits a third down a {window}-row window"
            );
        }
    }

    /// At the very start the window pads above line 1 instead of letting the
    /// running line slide to the top of the card, and the padding never grows
    /// past the third.
    #[test]
    fn the_first_lines_of_a_program_pad_above_instead_of_climbing() {
        assert_eq!(super::listing_first_line(1, 9, true, 100), -2);
        assert_eq!(super::listing_first_line(4, 9, true, 100), 1);
        // Idle, the window follows the selection and fills at the end.
        assert_eq!(super::listing_first_line(1, 9, false, 100), 1);
        assert_eq!(super::listing_first_line(100, 9, false, 100), 92);
    }

    /// At the very start the window pads above line 1 instead of letting the
    /// running line slide to the top of the card, and the padding never grows
    /// past the third.
    use super::{
        dro_axis_label, dro_value, feed_rows, listing_rows, modal_height, modal_mnemonic,
        modal_strip_height,
    };
    use linc_core::model::FeedState;

    #[test]
    fn the_lathe_reads_x_in_radius_or_diameter() {
        // Radius is the controller's own number; diameter is the same number
        // doubled, and it changes nothing else.
        assert_eq!(dro_value("X", 1.25, false), 1.25);
        assert_eq!(dro_value("X", 1.25, true), 2.5);
        // Z is a length along the bed: the factor belongs to the lathe's X.
        assert_eq!(dro_value("Z", 1.25, true), 1.25);
        // The card says which one is on.
        assert_eq!(dro_axis_label("X", false), "X");
        assert_eq!(dro_axis_label("X", true), "X DIA");
        assert_eq!(dro_axis_label("Z", true), "Z");
    }

    /// The commanded frame shows the commanded machine coordinate and applies
    /// the same work offset the feedback position carries; the fallback keeps
    /// numbers on screen while a backend has not populated `machine.commanded`.
    #[test]
    fn the_dro_command_source_derives_work_from_the_same_offset() {
        // Feedback machine 100, work 12: the active offset is -88.
        let feedback = linc_core::model::PositionSet {
            work: 12.0,
            machine: 100.0,
            dtg: -3.5,
            velocity: 0.0,
        };
        // Commanded lags the feedback by 0.25 mm; the work offset is -88, so
        // the commanded work value is 99.75 - 88 = 11.75.
        let (work, machine, dtg) = super::dro_frame("commanded", Some(99.75), feedback);
        assert!((work - 11.75).abs() < 1e-9);
        assert!((machine - 99.75).abs() < 1e-9);
        assert!((dtg + 3.5).abs() < 1e-9);

        // No commanded entry yet: fall back to the feedback machine position,
        // so the readout shows numbers instead of blanks.
        let (work, machine, dtg) = super::dro_frame("commanded", None, feedback);
        assert!((work - 12.0).abs() < 1e-9);
        assert!((machine - 100.0).abs() < 1e-9);
        assert!((dtg + 3.5).abs() < 1e-9);

        // Actual returns the feedback position unchanged.
        let (work, machine, dtg) = super::dro_frame("actual", Some(99.75), feedback);
        assert!((work - 12.0).abs() < 1e-9);
        assert!((machine - 100.0).abs() < 1e-9);
        assert!((dtg + 3.5).abs() < 1e-9);
    }

    #[test]
    fn feed_rows_show_the_programmed_and_effective_rate() {
        let mut feed = FeedState::default();
        feed.feed = 700.0;
        feed.feed_override = 0.5;
        feed.feed_override_enabled = true;
        feed.units = "mm".into();
        let rows = feed_rows(&feed);
        assert_eq!(rows[0].0, "700");
        assert_eq!(rows[0].1, "mm/min");
        assert_eq!(rows[0].2, "50%");
        assert_eq!(rows[1].0, "350");
        assert_eq!(rows[1].2, "(True)");

        // A disabled override means the programmed rate is what runs.
        feed.feed_override_enabled = false;
        assert_eq!(feed_rows(&feed)[1].0, "700");

        // A lathe's G95 feed is per revolution, not per minute: 0.008 must
        // neither round to "0" nor claim in/min.
        let mut lathe = FeedState::default();
        lathe.feed = 0.008;
        lathe.feed_override = 0.5;
        lathe.feed_override_enabled = true;
        lathe.feed_mode = "G95".into();
        lathe.units = "in".into();
        let rows = feed_rows(&lathe);
        assert_eq!(rows[0], ("0.008".into(), "in/rev".into(), "50%".into()));
        assert_eq!(rows[1].0, "0.004");
        assert_eq!(rows[1].1, "in/rev");

        // G93 is inverse time, in 1/min.
        lathe.feed_mode = "G93".into();
        lathe.feed = 1.5;
        assert_eq!(feed_rows(&lathe)[0].1, "1/min");
    }

    #[test]
    fn the_modal_mnemonic_says_what_the_code_means() {
        assert_eq!(modal_mnemonic("G21"), "MM");
        assert_eq!(modal_mnemonic("G20"), "INCH");
        assert_eq!(modal_mnemonic("G90"), "ABS");
        assert_eq!(modal_mnemonic("G91"), "INC");
        assert_eq!(modal_mnemonic("G17"), "XY");
        assert_eq!(modal_mnemonic("G54"), "CS 1");
        assert_eq!(modal_mnemonic("G59.3"), "CS 9");
        assert_eq!(modal_mnemonic("G83"), "PECK");
        // Words, not the group's own jargon: G21 is the units group, but the
        // line has to say which unit.
        assert_ne!(modal_mnemonic("G21"), "UNITS");
    }

    #[test]
    fn a_code_outside_the_table_gets_no_mnemonic_line() {
        // Better a two-line cell than an invented meaning.
        assert_eq!(modal_mnemonic("G999"), "");
        assert_eq!(modal_mnemonic("M3"), "");
    }

    #[test]
    fn every_mnemonic_stays_short_enough_for_a_column() {
        // The SPLIT pane's eight columns are about 37 px wide, so a mnemonic
        // has to stay around seven characters or it widens its column past the
        // card.
        let codes = [
            "G0", "G1", "G2", "G3", "G4", "G5", "G5.1", "G5.2", "G38.2", "G7", "G8", "G10", "G15",
            "G16", "G17", "G18", "G19", "G20", "G21", "G28", "G30", "G33", "G40", "G41", "G42",
            "G43", "G43.1", "G49", "G53", "G54", "G54.1", "G59.3", "G61", "G64", "G73", "G74",
            "G76", "G80", "G81", "G82", "G83", "G84", "G85", "G87", "G90", "G91", "G92", "G93",
            "G94", "G95", "G96", "G97", "G98", "G99",
        ];
        for key in codes {
            let mnemonic = modal_mnemonic(key);
            assert!(!mnemonic.is_empty(), "{key} has no mnemonic");
            assert!(
                mnemonic.chars().count() <= 7,
                "{key}: {mnemonic} is wider than a cell"
            );
        }
    }

    #[test]
    fn the_listing_takes_every_row_the_band_leaves() {
        // The band and the progress line own their pixels; the listing takes
        // the whole rows between the card's header and the band, so what it
        // cannot use is under one row and never a gap under the band. The pitch
        // is one row plus the gap the layout adds under it.
        let pitch = 20.0;
        for available in [130.0, 361.0, 380.0, 500.0] {
            let rows = listing_rows(available, pitch);
            let used = rows as f32 * pitch;
            assert!(
                used <= available + 1e-3,
                "{available} fits {rows} rows, which need {used}"
            );
            assert!(
                used + pitch > available,
                "another row would have fitted in {available}"
            );
        }
        // A card whose band and progress line take everything lists nothing:
        // rows reserved there are the ones the band used to be painted over.
        assert_eq!(listing_rows(0.0, pitch), 0);
        assert_eq!(listing_rows(19.0, pitch), 0);
        assert_eq!(listing_rows(20.0, pitch), 1);
    }

    #[test]
    fn the_in_card_band_is_shorter_than_a_modal_card() {
        // The dashboard's cells are two lines; `modal_height` sizes a card with
        // a title and three-line cells. Reserving the card's height for the band
        // is what left the dead space under it.
        for codes in [0, 5, 6, 7, 13] {
            let band = modal_strip_height(codes, 6);
            assert!(band < modal_height(codes, 6), "{codes}");
        }
        // One row of cells is 25 px, not 41: a fifth code wraps to a second row.
        assert!((modal_strip_height(7, 6) - modal_strip_height(6, 6) - 25.0).abs() < 1e-6);
        assert_eq!(modal_strip_height(0, 6), modal_strip_height(1, 6));
    }

    /// The meter block sits in the middle of the SERVO LOAD card's free space:
    /// as much margin above it as below it. A three-axis machine in a card as
    /// tall as the feed block beside it left all the slack at the bottom.
    #[test]
    fn the_servo_meters_are_centred_in_their_card() {
        // 76 px of free space under the title, three 16 px rows: 14 px above
        // them and the same 14 px below.
        let lead = super::servo_centred_lead(76.0, 3);
        assert!((lead - 14.0).abs() < 1e-6, "lead {lead}");
        assert!(
            (2.0 * lead + 3.0 * super::SERVO_METER_HEIGHT - 76.0).abs() < 1e-6,
            "the margin is split evenly"
        );
        // Exactly enough room, or less than the rows need: no lead at all, and
        // never a negative one.
        assert_eq!(super::servo_centred_lead(48.0, 3), 0.0);
        assert_eq!(super::servo_centred_lead(20.0, 3), 0.0);
    }

    /// The meters sit in the middle of the card's body: as much room above the
    /// block as below it, between the title and the body's bottom edge. Drawn
    /// into a slot of known size, so the two margins are measured and not
    /// inferred from the code that was meant to produce them.
    #[test]
    fn the_servo_card_centres_its_meters() {
        let rows: Vec<(String, f64)> = ["X", "Y", "Z"]
            .iter()
            .map(|axis| (axis.to_string(), 20.0))
            .collect();
        let ctx = egui::Context::default();
        let slot = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(300.0, 100.0));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(400.0, 300.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(slot)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            super::servo_load_card(&rows, &mut child, slot.height());
        });
        output.textures_delta.clear();

        let mut title_bottom = None;
        let mut block: Option<(f32, f32)> = None;
        let mut card = egui::Rect::NOTHING;
        for clipped in &output.shapes {
            match &clipped.shape {
                egui::Shape::Text(text) => {
                    let source = text.galley.job.text.as_str();
                    let top = text.pos.y;
                    let bottom = top + text.galley.size().y;
                    if source.starts_with("SERVO LOAD") {
                        title_bottom = Some(bottom);
                    } else if matches!(source, "X" | "Y" | "Z") {
                        block = Some(match block {
                            Some((first, _)) => (first.min(top), bottom),
                            None => (top, bottom),
                        });
                    }
                }
                egui::Shape::Rect(rect) if rect.rect.height() > 40.0 => {
                    if rect.rect.height() > card.height() {
                        card = rect.rect;
                    }
                }
                _ => {}
            }
        }
        let (block_top, block_bottom) = block.expect("three meter labels");
        // The card's 12 px inner margin is part of the frame, not of the body
        // the block is centred in.
        let body_bottom = card.bottom() - 12.0;
        let above = block_top - title_bottom.expect("the title");
        let below = body_bottom - block_bottom;
        assert!(
            (above - below).abs() <= 2.0,
            "above {above}, below {below}: the block is not centred in {card:?}"
        );
    }

    /// A header row is one line of text, not a touch target: the theme's
    /// `MIN_TARGET` would make the row 44 px tall and take a third of the
    /// following-error strip away from its lane.
    #[test]
    fn the_following_error_header_is_one_line() {
        let ctx = egui::Context::default();
        crate::theme::apply_design_zoom(&ctx, 1.0);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(1280.0, 720.0),
        ));
        let mut output = ctx.run_ui(input, |ui| {
            let line = super::header_line_height(ui);
            assert!(
                line < crate::theme::MIN_TARGET / 2.0,
                "the header row is one line ({line}), not a touch target"
            );
            assert!(line > 6.0, "and it is still a readable line ({line})");
        });
        output.textures_delta.clear();
    }
    /// The view eats only when the operator asked for it: the interpreter's
    /// completed line is the progress, the mode is the switch. A card that drew
    /// the running program kept its whole path until the mode was turned on.
    #[test]
    fn remaining_gates_what_the_interpreter_has_finished() {
        let (_dir, profile) = program_fixture();
        let mut style = crate::state::graph_style(&profile);
        let mut program = linc_core::model::ProgramState::default();
        program.line_count = 9;
        program.running = true;
        program.motion_line = 5;

        style.remaining = false;
        assert_eq!(
            super::eaten_through(&style, &program),
            None,
            "the mode is off, so nothing is eaten"
        );

        style.remaining = true;
        assert_eq!(
            super::eaten_through(&style, &program),
            Some(4),
            "the four blocks before the one being cut"
        );

        // Idle: the interpreter is not cutting anything, so there is no mouth
        // to eat with, however the mode is set.
        program.running = false;
        program.motion_line = 0;
        program.current_line = 0;
        assert_eq!(super::eaten_through(&style, &program), None);
    }

    /// A profile the card can be drawn against, with its preferences kept in
    /// the temp dir so no test reads another's remembered page.
    fn program_fixture() -> (tempfile::TempDir, linc_core::config::Profile) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("base.toml"),
            include_str!("../../../../configs/base.toml"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("gcode")).unwrap();
        std::fs::write(root.join("tool.tbl"), "T1 P0 Z-56 D6 ; end mill\n").unwrap();
        std::fs::write(
            root.join("probe_basic.ini"),
            "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = TZDR\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -372\nMAX_LIMIT = 5\n[AXIS_Y]\nMIN_LIMIT = -346\nMAX_LIMIT = 4\n[AXIS_Z]\nMIN_LIMIT = -253\nMAX_LIMIT = 19\n",
        )
        .unwrap();
        std::fs::write(
            root.join("sim.toml"),
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
panel_window = false
[preferences]
file = "{root}/prefs/{{profile}}.preferences.toml"
"#
            .replace("{root}", &root.to_string_lossy()),
        )
        .unwrap();
        let profile = linc_core::config::Profile::load(&root.join("sim.toml"), None).unwrap();
        (dir, profile)
    }

    /// What one frame of the MAIN program card painted, split by owner. The
    /// band is the clip its own label painted under, which is the rect the card
    /// placed it in; a listing row is found by the text the fixture program
    /// carries, so the assertions do not reuse the arithmetic they test.
    #[derive(Default)]
    struct CardPaint {
        band: Option<egui::Rect>,
        listing: Vec<(egui::Rect, egui::Rect)>,
        progress: Option<egui::Rect>,
        card: Option<egui::Rect>,
    }

    fn walk_card_paint(shape: &egui::Shape, clip: egui::Rect, paint: &mut CardPaint) {
        match shape {
            egui::Shape::Text(text) => {
                let painted = egui::Rect::from_min_size(text.pos, text.galley.size());
                let string = text.galley.text().to_string();
                if string.contains("LISTROW") {
                    paint.listing.push((painted, clip));
                } else if string == "MODAL" {
                    paint.band = Some(clip);
                } else if string == "PROGRESS" {
                    paint.progress = Some(painted);
                }
            }
            egui::Shape::Rect(rect) => {
                let frame =
                    rect.stroke.color == crate::theme::BORDER && rect.corner_radius.nw == 6;
                let bigger = paint.card.map_or(true, |card| rect.rect.area() > card.area());
                if frame && bigger {
                    paint.card = Some(rect.rect);
                }
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk_card_paint(shape, clip, paint);
                }
            }
            _ => {}
        }
    }

    /// Draw the program card in a 360 px slot with `codes` modal G codes
    /// and report what it painted.
    fn paint_program_card(codes: usize, height: f32) -> CardPaint {
        let (_dir, profile) = program_fixture();
        let ctx = egui::Context::default();
        let raw = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..Default::default()
        };
        let mut app = None;
        let mut output = ctx.run_ui(raw(), |ui| {
            let backend = Box::new(crate::state::LocalBackend::new(
                profile.clone(),
                linc_core::model::PanelMode::Auto,
            ));
            app = Some(crate::shell::LincApp::new(profile.clone(), backend, ui.ctx()));
        });
        output.textures_delta.clear();
        let mut app = app.unwrap();
        let text: String = (1..=400).map(|index| format!("LISTROW{index:04}\n")).collect();
        app.snapshot.program.content = Some(std::sync::Arc::new(text));
        app.snapshot.program.line_count = 400;
        app.snapshot.program.current_line = 1;
        app.snapshot.program.modal = (0..codes)
            .map(|index| (format!("G{}", index + 1), "1.0".to_string()))
            .collect();
        let slot = egui::Rect::from_min_size(
            egui::Pos2::new(400.0, 20.0),
            egui::vec2(360.0, height),
        );
        let mut out = ctx.run_ui(raw(), |ui| {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(slot)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            super::program_card_sized(&app, &mut child, height);
        });
        out.textures_delta.clear();
        let mut paint = CardPaint::default();
        for clipped in &out.shapes {
            walk_card_paint(&clipped.shape, clipped.clip_rect, &mut paint);
        }
        paint
    }

    fn band_top(paint: &CardPaint) -> f32 {
        paint.band.expect("the band always paints").top()
    }

    /// The defect: a band taller than the space the listing had left (a second
    /// row of codes, a narrower card) let the listing paint inside the band.
    #[test]
    fn the_listing_never_paints_inside_the_modal_band() {
        for codes in [0usize, 3, 7, 13] {
            for height in [200.0f32, 260.0, 340.0, 420.0, 600.0] {
                let paint = paint_program_card(codes, height);
                let band = paint.band.expect("the band always paints");
                for (row, clip) in &paint.listing {
                    assert!(
                        !row.intersects(band),
                        "codes {codes} height {height}: row {row:?} is painted in the band {band:?}"
                    );
                    assert!(
                        clip.max.y <= band.top() + 0.5,
                        "codes {codes} height {height}: row clip {clip:?} crosses the band top {}",
                        band.top()
                    );
                }
            }
        }
    }

    /// A taller band costs the listing whole rows, and a roomy card still gives
    /// it every row above the band.
    #[test]
    fn a_taller_band_takes_whole_rows_off_the_listing() {
        let small = paint_program_card(3, 420.0);
        let tall = paint_program_card(13, 420.0);
        assert!(!small.listing.is_empty(), "the roomy card lists rows");
        assert!(
            band_top(&tall) < band_top(&small),
            "13 codes need three rows of cells, 3 codes need one"
        );
        assert!(
            tall.listing.len() < small.listing.len(),
            "the taller band kept {} rows against {}",
            tall.listing.len(),
            small.listing.len()
        );
        let top = small
            .listing
            .iter()
            .map(|(row, _)| row.min.y)
            .fold(f32::INFINITY, f32::min);
        let bottom = small
            .listing
            .iter()
            .map(|(row, _)| row.max.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let pitch = (bottom - top) / (small.listing.len() - 1) as f32;
        assert!(
            band_top(&small) - bottom < pitch,
            "another row would have fitted in {:.1} px",
            band_top(&small) - bottom
        );
    }

    /// Nothing else on the page moves: the progress line keeps the card's bottom
    /// edge, stays under the band, and the card's own rect does not follow the
    /// band's height.
    #[test]
    fn the_progress_line_keeps_the_cards_bottom_edge() {
        let mut cards = Vec::new();
        for codes in [3usize, 13] {
            let paint = paint_program_card(codes, 600.0);
            let card = paint.card.expect("the card frame paints");
            // The slot ends on the page's bottom edge at 620; the frame's stroke
            // sits on it.
            assert!((card.max.y - 620.0).abs() <= 2.5, "the card ends at {card:?}");
            let progress = paint.progress.expect("the progress line paints");
            assert!(
                progress.min.y > band_top(&paint),
                "the progress line stays under the band"
            );
            assert!(progress.max.y < card.max.y - 1.0, "and inside the card");
            cards.push(card);
        }
        assert!(
            (cards[0].max.y - cards[1].max.y).abs() < 0.5,
            "the taller band moved the card: {cards:?}"
        );
    }
}
