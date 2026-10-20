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
                if let Some(content) = content {
                    let current = app.snapshot.program.current_line.max(1);
                    let total = app.snapshot.program.line_count.max(1);
                    let window_len = 16usize;
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
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let (marker, _) =
                                ui.allocate_exact_size(egui::vec2(2.0, 12.0), egui::Sense::hover());
                            if is_current {
                                ui.painter().rect_filled(marker, 1.0, theme::ACCENT);
                            }
                            let number = line
                                .map(|_| format!("{line_number:>4}"))
                                .unwrap_or_else(|| "    ".to_string());
                            ui.label(
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
                                Some(line) => token_line_ui(ui, &truncate(line, 26), 9.5),
                                None => {
                                    ui.label(
                                        RichText::new(" ").monospace().size(theme::font_size(9.5)),
                                    );
                                }
                            }
                        });
                    }
                } else {
                    field_label(ui, "CURRENT");
                    let current = app.snapshot.program.current_block.trim();
                    if current.is_empty() {
                        ui.label(
                            RichText::new("—")
                                .monospace()
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                    } else {
                        ui.horizontal(|ui| {
                            let (marker, _) =
                                ui.allocate_exact_size(egui::vec2(2.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(marker, 1.0, theme::ACCENT);
                            token_ui(ui, &truncate(current, 44), 11.0);
                        });
                    }
                    field_label(ui, "NEXT");
                    match app.snapshot.program.next_line.as_deref() {
                        Some(next) if !next.trim().is_empty() => {
                            token_ui(ui, &truncate(next, 44), 10.0);
                        }
                        _ => {
                            ui.label(
                                RichText::new("—")
                                    .monospace()
                                    .size(theme::font_size(10.0))
                                    .color(theme::MUTED),
                            );
                        }
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
            ui.label(
                RichText::new(subtitle)
                    .size(theme::font_size(11.0))
                    .color(theme::MUTED),
            );
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
