//! Components shared by more than one screen.

use egui::{Align, Color32, FontId, Layout, RichText, Sense, Ui, Vec2};
use linc_core::gcode::{lex_line, Preview, TokenKind};
use linc_core::model::{format_clock, AxisMap, MachineSnapshot, MachineVariant, PanelMode};
use linc_view::{draw_trajectory, TrajectoryView, ViewMode};

use crate::shell::LincApp;
use crate::state::Page;
use crate::theme;

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

fn token_line_ui(ui: &mut Ui, text: &str, size: f32) {
    ui.horizontal(|ui| {
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


/// Active G-code modal groups as abbreviated cells.
///
/// The Machine dashboard and the Program side pane use the same cells; only the
/// column count changes with the width available.
pub fn modal_grid(app: &LincApp, ui: &mut Ui, columns: usize, id_salt: &str) {
    let codes = app
        .snapshot
        .program
        .modal
        .iter()
        .filter(|(key, _)| key.starts_with('G'))
        .map(|(key, value)| (modal_abbreviation(key), modal_text(key, value)))
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
            for (index, (abbreviation, code)) in codes.iter().enumerate() {
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
        ui.painter().circle_stroke(
            marker.center(),
            4.0,
            egui::Stroke::new(1.5, linc_view::theme::TOOL),
        );
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
        modal_grid(app, ui, columns, id_salt);
    });
}

/// MODAL pane with an exact height, so the column that holds it ends where the

/// Height a MODAL pane needs for `codes` G-code cells at `columns` wide.
///
/// Measured from the card: 45 px of chrome (margins plus header) and 45 px per
/// row of cells. Derived rather than observed so it cannot lag a frame and
/// leave the column short.
pub fn modal_height(codes: usize, columns: usize) -> f32 {
    let rows = codes.max(1).div_ceil(columns.max(1));
    // 8 px frame, an 18 px title row, and 26 px per row of cells.
    26.0 + rows as f32 * 26.0
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

fn paint_axis_card(app: &LincApp, ui: &Ui, axis: &str, rect: egui::Rect, compact: bool) {
    let position = app.snapshot.positions.axes.get(axis);
    let work = position.map(|p| p.work).unwrap_or(0.0);
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
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6, theme::PANEL);
    painter.rect_stroke(
        rect,
        6,
        egui::Stroke::new(1.0, if active { theme::ACCENT } else { theme::BORDER }),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + Vec2::new(14.0, 10.0),
        egui::Align2::LEFT_TOP,
        axis,
        FontId::proportional(theme::font_size(13.0)),
        theme::C3,
    );
    painter.text(
        rect.left_top() + Vec2::new(14.0, 35.0),
        egui::Align2::LEFT_CENTER,
        crate::theme::format_position(work, 3),
        FontId::monospace(if compact { 19.0 } else { 23.0 }),
        theme::FG,
    );
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

pub fn health_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 21.0).max(0.0));
        ui.set_max_height((height - 21.0).max(0.0));
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 22.0;
        theme::section_header(ui, "SERVO LOAD");
        for axis in app.axes().iter().take(5) {
            let load = app
                .snapshot
                .machine
                .servo_load
                .get(axis)
                .copied()
                .unwrap_or(0.0);
            theme::meter(
                ui,
                axis,
                load / 100.0,
                format!("{load:.0}%"),
                theme::pressure_color(load / 100.0),
            );
        }
    });
}

pub fn alarm_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 21.0).max(0.0));
        ui.set_max_height((height - 21.0).max(0.0));
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::section_header(ui, "MESSAGES");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "ALARM {} / MSG {}",
                        app.snapshot.alarm_count(),
                        app.snapshot.message_count()
                    ))
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
                );
            });
        });
        if app.snapshot.alarms.is_empty() {
            ui.label(
                RichText::new("NO ACTIVE ALARMS")
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::SUCCESS),
            );
        } else {
            let width = ui.available_width();
            egui::ScrollArea::vertical()
                .id_salt("alarm-card")
                .max_height(height - 34.0)
                // Without this the scroll area sizes to its content, and a long
                // controller message widened the whole card and pushed the page
                // sideways.
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_max_width(width);
                    // Rows of one message sit tight: the default spacing plus an
                    // explicit gap made every entry look double-spaced.
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for alarm in app.snapshot.alarms.iter().take(20) {
                        let color = match alarm.severity {
                            linc_core::model::AlarmSeverity::Critical
                            | linc_core::model::AlarmSeverity::Error => theme::ERROR,
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
                        // The message gets the full card width and wraps, so it
                        // reads instead of running off the edge.
                        ui.add(
                            egui::Label::new(
                                RichText::new(&alarm.message)
                                    .size(theme::font_size(11.0))
                                    .color(
                                        if alarm.severity == linc_core::model::AlarmSeverity::Info {
                                            theme::MUTED
                                        } else {
                                            color
                                        },
                                    ),
                            )
                            .wrap(),
                        );
                        ui.add_space(3.0);
                    }
                });
        }
    });
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
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

fn field_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::font_size(8.5))
            .color(theme::MUTED),
    );
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
                let window_len = 16usize;
                let row_height = ui.spacing().interact_size.y.max(16.0);
                if let Some(content) = content {
                    // Follow the line motion is cutting, not the interpreter's
                    // read-ahead, which reaches the end of the program first.
                    let current = app.snapshot.program.executing_line().max(1);
                    let total = app.snapshot.program.line_count.max(1);
                    let is_running = app.snapshot.task.running || app.snapshot.program.paused;
                    let half_window = (window_len / 2) as isize;
                    let first = if is_running {
                        current as isize - half_window
                    } else {
                        let last_first = (total.saturating_sub(window_len) + 1).max(1) as isize;
                        (current.saturating_sub(window_len / 2).max(1) as isize).min(last_first)
                    };
                    let mut lines = content.lines().skip((first.max(1) - 1) as usize);
                    for row in 0..window_len {
                        let line_number = first + row as isize;
                        let line = if line_number >= 1 { lines.next() } else { None };
                        let is_current = line_number == current as isize;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::hover(),
                        );
                        let mut row_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("program_row", row))
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        row_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                        row_ui.spacing_mut().item_spacing.x = 4.0;
                        let (marker, _) =
                            row_ui.allocate_exact_size(egui::vec2(2.0, 12.0), egui::Sense::hover());
                        if is_current {
                            row_ui.painter().rect_filled(marker, 1.0, theme::ACCENT);
                        }
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
                        match line {
                            Some(line) => token_line_ui(&mut row_ui, &truncate(line, 26), 9.5),
                            None => {
                                row_ui.label(
                                    RichText::new(" ").monospace().size(theme::font_size(9.5)),
                                );
                            }
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
                modal_grid(app, ui, 6, "program_modal_fields");

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

pub fn feed_spindle_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(0.0));
        ui.set_max_height((height - 24.0).max(0.0));
        ui.set_min_width(ui.available_width());
        theme::section_header(ui, "FEED / SPINDLE LOAD");
        ui.horizontal_top(|ui| {
            let (load, speed, override_factor) = app
                .snapshot
                .spindles
                .first()
                .map(|spindle| {
                    (
                        spindle.load.unwrap_or(0.0) / 100.0,
                        spindle.actual_speed,
                        spindle.override_factor * 100.0,
                    )
                })
                .unwrap_or((0.0, 0.0, 0.0));
            donut_gauge(
                ui,
                "FEED OVR",
                app.snapshot.feed.feed_override,
                format!("{:.0}%", app.snapshot.feed.feed_override * 100.0),
                &format!(
                    "{:.0} {} / MIN",
                    app.snapshot.feed.feed, app.snapshot.feed.units
                ),
                theme::ACCENT,
            );
            ui.add_space(16.0);
            donut_gauge(
                ui,
                "SPINDLE LOAD",
                load,
                format!("{:.0}%", load * 100.0),
                &format!("{speed:.0} RPM / {override_factor:.0}%"),
                theme::pressure_color(load),
            );
        });
    });
}

fn donut_gauge(
    ui: &mut Ui,
    label: &str,
    value: f64,
    text: String,
    detail: &str,
    color: egui::Color32,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(102.0, 104.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let diameter = 66.0;
    let center = egui::pos2(rect.center().x, rect.top() + diameter * 0.5);
    let radius = diameter * 0.5 - 5.0;
    painter.circle_stroke(center, radius, egui::Stroke::new(6.0, theme::BORDER));
    let fraction = value.clamp(0.0, 1.0) as f32;
    if fraction > 0.001 {
        let steps = (fraction * 48.0).ceil() as usize;
        for step in 0..steps {
            let a0 = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * step as f32 / 48.0;
            let a1 =
                -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * (step + 1) as f32 / 48.0;
            painter.line_segment(
                [
                    center + Vec2::new(a0.cos(), a0.sin()) * radius,
                    center + Vec2::new(a1.cos(), a1.sin()) * radius,
                ],
                egui::Stroke::new(6.0, color),
            );
        }
    }
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        text,
        FontId::monospace(theme::font_size(11.0)),
        theme::FG,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.top() + diameter + 4.0),
        egui::Align2::CENTER_TOP,
        label,
        FontId::proportional(theme::font_size(9.0)),
        theme::MUTED,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.top() + diameter + 18.0),
        egui::Align2::CENTER_TOP,
        detail,
        FontId::monospace(theme::font_size(8.5)),
        theme::MUTED,
    );
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

pub fn positions_table(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "POSITION");
        egui::Grid::new("positions-table")
            .num_columns(4)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label(
                    RichText::new("AXIS")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("WORK")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("MACHINE")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("DTG")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.end_row();
                for axis in app.axes() {
                    let position = app.snapshot.positions.axes.get(&axis);
                    ui.label(RichText::new(&axis).monospace().color(theme::C3));
                    ui.label(
                        RichText::new(format!("{:+.3}", position.map(|p| p.work).unwrap_or(0.0)))
                            .monospace()
                            .color(theme::FG),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{:+.3}",
                            position.map(|p| p.machine).unwrap_or(0.0)
                        ))
                        .monospace()
                        .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new(format!("{:.3}", position.map(|p| p.dtg).unwrap_or(0.0)))
                            .monospace()
                            .color(theme::MUTED),
                    );
                    ui.end_row();
                }
            });
    });
}

/// Draw the loaded program path + live trail.
pub fn trajectory(
    app: &LincApp,
    ui: &mut Ui,
    preview: &Preview,
    height: f32,
    view_mode: ViewMode,
    zoom: &mut f32,
    pan: &mut Vec2,
    fit: &mut bool,
    selected_line: Option<usize>,
    current_line: Option<usize>,
    // Legends name the colours; a thumbnail too small to read them omits it.
    show_legend: bool,
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
    view.view = view_mode;
    view.height = height;
    view.machine_bounds = app.machine_bounds();
    view.zoom = *zoom;
    view.pan = *pan;
    view.fit = *fit;
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
    *fit = view.fit;
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
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    let response = ui.interact(rect, ui.scope_id().with(salt), Sense::focusable_noninteractive());
    let requested = ui.ctx().data_mut(|data| {
        data.remove_temp::<bool>(egui::Id::unique("linc-page-focus"))
            .unwrap_or(false)
    });
    if requested {
        response.request_focus();
    }
    // No focus ring: the anchor exists only so arrow-key navigation has
    // somewhere to start, and a ring around an empty full-width strip reads as
    // a stray empty control. Real widgets still draw their own rings.
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
        let current_line = app.snapshot.program.executing_line();
        let view = app.manual.view;
        let mut zoom = app.manual.zoom;
        let mut pan = app.manual.pan;
        let mut fit = app.manual.fit;
        trajectory(
            app,
            ui,
            &preview,
            graph_height,
            view,
            &mut zoom,
            &mut pan,
            &mut fit,
            None,
            Some(current_line),
            false,
        );
        app.manual.zoom = zoom;
        app.manual.pan = pan;
        app.manual.fit = fit;
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
        let label = if overridden { "OVERRIDE ON" } else { "OVERRIDE" };
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

pub fn live_coordinates_card(
    ui: &mut Ui,
    header: &str,
    units_label: &str,
    rows: &[LiveAxisRow],
) {
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
                    if row.active { theme::ACCENT } else { theme::BORDER },
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
