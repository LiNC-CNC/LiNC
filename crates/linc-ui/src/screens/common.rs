//! Components shared by more than one screen.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Layout, RichText, Sense, Ui, Vec2};
use linc_core::gcode::{lex_line, Preview, TokenKind};
use linc_core::model::{format_clock, AxisMap, MachineSnapshot, MachineVariant, PanelMode};
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
pub fn toolpath_header(ui: &mut Ui, view: &str, zoom: f32) {
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

/// MODAL pane with an exact height, so the column that holds it ends where the

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
    let height = if compact { 72.0 } else { 92.0 };
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
    painter.text(
        rect.left_top() + Vec2::new(14.0, 10.0),
        egui::Align2::LEFT_TOP,
        dro_axis_label(axis, diameter),
        FontId::proportional(theme::font_size(13.0)),
        theme::C3,
    );
    if show_work {
        painter.text(
            rect.left_top() + Vec2::new(14.0, 35.0),
            egui::Align2::LEFT_CENTER,
            crate::theme::format_position(work, 3),
            FontId::monospace(if compact { 19.0 } else { 23.0 }),
            theme::FG,
        );
    }
    // The machine coordinate sits beside the work value: the operator reads the
    // work position first, and the controller's own coordinate is the reference
    // for offsets and limits, so it stays visible but dimmed.
    painter.text(
        rect.right_top() + Vec2::new(-14.0, 35.0),
        egui::Align2::RIGHT_CENTER,
        format!("M {}", crate::theme::format_position(machine, 3)),
        FontId::monospace(theme::font_size(9.0)),
        theme::MUTED,
    );
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
    let position = app.snapshot.positions.axes.get(axis);
    let work = position.map(|p| p.work).unwrap_or(0.0);
    let machine = position.map(|p| p.machine).unwrap_or(0.0);
    let dtg = position.map(|p| p.dtg).unwrap_or(0.0);
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
fn servo_meter(ui: &mut Ui, label: &str, load: f64) {
    const SCALE: f32 = 120.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
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

pub fn servo_load_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        // The same content offset as the feed card, so both card frames end on
        // the same bottom line.
        ui.set_min_height((height - 24.0).max(0.0));
        ui.set_max_height((height - 24.0).max(0.0));
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 16.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        theme::section_header(ui, "SERVO LOAD");
        for (label, load) in app.servo_load_rows() {
            servo_meter(ui, &label, load);
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

/// Rows of G-code the PROGRAM card shows at `height`.
///
/// The card is a fixed height, so the listing takes what the rest of the card
/// leaves: the header and rule above it, the MODAL label, the strip (26 px per
/// row of two-line cells) and the progress line below it. Nothing is held back
/// for the strip beyond what it needs, so the surplus goes to the listing.
fn program_window_rows(height: f32, codes: usize, row_height: f32) -> usize {
    const HEADER: f32 = 26.0;
    const MODAL_LABEL: f32 = 12.0;
    const PROGRESS: f32 = 46.0;
    // Reserve the modal grid's real height, not one short line per row: its
    // cells are 41 px tall, and underestimating grew the strip down into the
    // progress line instead of taking rows from the listing above it.
    let strip = modal_height(codes, 6);
    let budget = (height - 24.0) - HEADER - MODAL_LABEL - strip - PROGRESS;
    (budget / (row_height + 2.0)).floor().max(6.0) as usize
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
                let window_len = program_window_rows(height, codes, row_height);
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
                    let half_window = (window_len / 2) as isize;
                    let first = if is_running {
                        focus as isize - half_window
                    } else {
                        let last_first = (total.saturating_sub(window_len) + 1).max(1) as isize;
                        (focus.saturating_sub(window_len / 2).max(1) as isize).min(last_first)
                    };
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
                        (ui.available_width() - theme::SELECTION_BAR - number_width - 8.0)
                            .max(40.0);
                    // Wrapped rows consume more than row_height, so stop before
                    // the fixed window would push the modal strip out of the card.
                    let listing_limit = window_len as f32 * (row_height + 2.0);
                    let mut used = 0.0f32;
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
                            + 2.0;
                        if row > 0 && used + height > listing_limit {
                            break;
                        }
                        used += height;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), height),
                            egui::Sense::hover(),
                        );
                        if is_current {
                            // The band goes down before the row's text, so the
                            // tokens are painted over it rather than tinted by
                            // it.
                            selection_band(ui, rect);
                        }
                        let mut row_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("program_row", row))
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Min)),
                        );
                        row_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
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
                        let _ = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::hover(),
                        );
                    }
                }
                theme::separator(ui);
                field_label(ui, "MODAL");
                modal_grid(app, ui, 6, false, "program_modal_fields");

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
    app.graph_style
        .apply(&mut view, selected_line, current_line);
    view.show_legend = view.show_legend && show_legend;
    let response = draw_trajectory(ui, &mut view);
    *zoom = view.zoom;
    *pan = view.pan;
    *yaw = view.yaw;
    *pitch = view.pitch;
    *fit = view.fit;
    *auto_fit = view.auto_fit;
    response
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
        toolpath_header(ui, app.manual.view.label(), app.manual.zoom);
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

/// Large work/machine/DTG readout, shared by the Manual and Homing screens.
/// Limits override, on one compact row above the touch-off card.
///
/// The controller refuses motion while a joint sits on a limit switch, and the
/// override is the only way to accept it — so it has to be reachable: it lives
/// next to the other machine actions rather than at the bottom of the page.
pub fn limits_override_row(app: &mut crate::shell::LincApp, ui: &mut Ui) {
    let overridden = app.snapshot.limits_overridden;
    // Blinking red while active: the state means the limit switches are being
    // ignored, so it must not be possible to miss. Repaints are requested for as
    // long as it is on, because egui only animates when asked.
    let blink_on = ((ui.input(|input| input.time) * 2.0) as i64) % 2 == 0;
    if overridden {
        ui.ctx().request_repaint();
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("LIMITS")
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        let label = if overridden {
            "OVERRIDE ON"
        } else {
            "OVERRIDE"
        };
        let clicked = if overridden && blink_on {
            theme::danger_button(ui, label, true).clicked()
        } else if overridden {
            theme::action_button(ui, label, true).clicked()
        } else {
            theme::action_button(ui, label, true).clicked()
        };
        if clicked {
            app.send(linc_core::command::MachineCommand::ToggleLimitsOverride);
        }
        ui.label(
            RichText::new("O")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        if overridden {
            ui.label(
                RichText::new("SWITCHES IGNORED")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::ERROR),
            );
        }
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
                    ui.label(
                        RichText::new(&row.name)
                            .monospace()
                            .size(theme::font_size(13.0))
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
    use super::{
        dro_axis_label, dro_value, feed_rows, modal_height, modal_mnemonic, program_window_rows,
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
    fn the_program_window_gives_its_surplus_to_the_listing() {
        // The MAIN card at a 720-high window.
        let height = 486.0;
        let row_height = 18.0;
        let rows = program_window_rows(height, 5, row_height);
        // The listing fills the card down to the progress line rather than
        // stopping at a round number of rows.
        let used = 26.0 + rows as f32 * (row_height + 2.0) + 12.0 + modal_height(5, 6) + 46.0;
        let inner = height - 24.0;
        assert!(used <= inner, "{used} > {inner}");
        assert!(
            used + row_height + 2.0 > inner,
            "another row would have fitted"
        );

        // A strip that wraps to two 41 px rows takes two listing rows back.
        assert_eq!(program_window_rows(height, 7, row_height), rows - 2);
    }
}
