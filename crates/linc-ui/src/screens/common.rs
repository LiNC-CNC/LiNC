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

pub fn current_next_cards(app: &LincApp, ui: &mut Ui) {
    ui.columns(2, |columns| {
        theme::card(&mut columns[0], |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("CURRENT")
                        .size(theme::font_size(10.0))
                        .strong()
                        .color(theme::ACCENT),
                );
                ui.label(
                    RichText::new(format!("LN {}", app.snapshot.program.current_line))
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
            });
            let line = if app.snapshot.program.current_block.is_empty() {
                app.program
                    .editor
                    .line(app.snapshot.program.current_line.max(1))
            } else {
                app.snapshot.program.current_block.clone()
            };
            token_ui(ui, &line, 13.0);
        });
        theme::card(&mut columns[1], |ui| {
            ui.label(
                RichText::new("NEXT")
                    .size(theme::font_size(10.0))
                    .strong()
                    .color(theme::MUTED),
            );
            let line = app.snapshot.program.next_line.clone().unwrap_or_default();
            if line.is_empty() {
                ui.label(RichText::new("—").monospace().color(theme::MUTED));
            } else {
                token_ui(ui, &line, 13.0);
            }
        });
    });
}

pub fn modal_text(key: &str, value: &str) -> String {
    if value.is_empty() {
        key.to_string()
    } else {
        format!("{key} {value}")
    }
}

pub fn modal_summary(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.label(
            RichText::new("MODAL")
                .size(theme::font_size(10.0))
                .strong()
                .color(theme::MUTED),
        );
        if app.snapshot.program.modal.is_empty() {
            ui.label(RichText::new("—").monospace().color(theme::MUTED));
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for (key, value) in &app.snapshot.program.modal {
                    ui.label(
                        RichText::new(modal_text(key, value))
                            .monospace()
                            .size(theme::font_size(11.0))
                            .color(theme::FG),
                    );
                }
            });
        }
    });
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
    let range = app
        .profile
        .machine_ini
        .axes
        .iter()
        .find(|axis_config| axis_config.name == axis);
    let fraction = range
        .map(|axis_config| {
            let span = (axis_config.max_limit - axis_config.min_limit)
                .abs()
                .max(0.001);
            ((machine - axis_config.min_limit) / span).clamp(0.0, 1.0)
        })
        .unwrap_or(0.0);
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
            egui::ScrollArea::vertical()
                .id_salt("alarm-card")
                .max_height(height - 34.0)
                .show(ui, |ui| {
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
                            ui.label(
                                RichText::new(&alarm.message)
                                    .size(theme::font_size(11.0))
                                    .color(
                                        if alarm.severity == linc_core::model::AlarmSeverity::Info {
                                            theme::MUTED
                                        } else {
                                            color
                                        },
                                    ),
                            );
                        });
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
                    let current = app.snapshot.program.current_line.max(1);
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
                } else {
                    let modal_column_width = ((ui.available_width() - 4.0 * 5.0) / 6.0).max(30.0);
                    egui::Grid::new("program_modal_fields")
                        .num_columns(6)
                        .spacing([4.0, 1.0])
                        .min_col_width(modal_column_width)
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
                                if (index + 1) % 6 == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                }

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
                                app.snapshot.program.current_line,
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
) -> egui::Response {
    let mut view = TrajectoryView::new(
        preview,
        app.snapshot.live_trail.as_slice(),
        app.profile.variant,
    );
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
    if response.has_focus() {
        theme::focus_ring(ui, &response);
    }
}

pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let title_response = ui.label(
                RichText::new(title)
                    .size(theme::font_size(14.0))
                    .strong()
                    .color(theme::FG),
            );
            let title_focus = ui.interact(
                title_response.rect,
                ui.scope_id().with("page-title-focus"),
                Sense::focusable_noninteractive(),
            );
            let requested = ui.ctx().data_mut(|data| {
                data.remove_temp::<bool>(egui::Id::unique("linc-page-focus"))
                    .unwrap_or(false)
            });
            if requested {
                title_focus.request_focus();
            }
            if title_focus.has_focus() {
                theme::focus_ring(ui, &title_focus);
            }
            // A page with nothing to say gets no subtitle line at all; drawing
            // an empty label still costs a row of vertical space.
            if !subtitle.is_empty() {
                ui.label(
                    RichText::new(subtitle)
                        .size(theme::font_size(11.0))
                        .color(theme::MUTED),
                );
            }
        });
    });
    ui.add_space(4.0);
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
    pub dtg: f64,
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

/// `PANEL MODE: <mode>` banner plus the read-only reminder, shared by the
/// Manual and Homing screens.
pub fn live_mode_header(
    ui: &mut Ui,
    mode_label: &str,
    mode_known: bool,
    reminder: &str,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("PANEL MODE: {mode_label}"))
                .monospace()
                .size(theme::font_size(14.0))
                .color(if mode_known {
                    theme::ACCENT
                } else {
                    theme::WARNING
                }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(reminder)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        });
    });
    ui.add_space(4.0);
}

/// Dominant live graph card with the shared view controls. Uses the single
/// live-view state on `app.manual`, so the view is preserved when the operator
/// moves between the Manual and Homing positions.
pub fn live_graph_card(app: &mut LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "TOOLPATH / LIVE POSITION");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::action_button(ui, "CLEAR TRAIL", app.graph_style.show_live_trail)
                    .clicked()
                {
                    app.clear_trail();
                }
                if theme::action_button(ui, "FIT", true).clicked() {
                    app.manual.fit = true;
                }
                let label = app.manual.view.label();
                if theme::action_button(ui, label, true).clicked() {
                    app.manual.view = if app.profile.variant == MachineVariant::Lathe {
                        ViewMode::LatheXz
                    } else {
                        match app.manual.view {
                            ViewMode::Iso3d => ViewMode::TopXy,
                            ViewMode::TopXy => ViewMode::SideXz,
                            _ => ViewMode::Iso3d,
                        }
                    };
                }
                if app.snapshot.machine.estop {
                    ui.label(RichText::new("E-STOP").color(theme::ERROR));
                }
            });
        });
        let preview = app.program.last_valid_preview.clone();
        let current_line = app.snapshot.program.current_line;
        let view = app.manual.view;
        let mut zoom = app.manual.zoom;
        let mut pan = app.manual.pan;
        let mut fit = app.manual.fit;
        trajectory(
            app,
            ui,
            &preview,
            height,
            view,
            &mut zoom,
            &mut pan,
            &mut fit,
            None,
            Some(current_line),
        );
        app.manual.zoom = zoom;
        app.manual.pan = pan;
        app.manual.fit = fit;
    });
}

/// Large work/machine/DTG readout, shared by the Manual and Homing screens.
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
    if let (Some(axes), Some(index)) = (chosen, index) {
        app.send(linc_core::command::MachineCommand::ZeroWorkOffset { index, axes });
    }
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
                        ui.label(
                            RichText::new(format!("DTG {:.3}", row.dtg))
                                .monospace()
                                .size(theme::font_size(9.0))
                                .color(theme::MUTED),
                        );
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
                        ui.label(
                            RichText::new("WORK / MACHINE / DTG")
                                .size(theme::font_size(8.0))
                                .color(theme::MUTED),
                        );
                    });
                });
            });
        }
    });
}
