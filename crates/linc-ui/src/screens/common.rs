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
                        RichText::new(format!("{key} {value}"))
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
    let height = if compact { 76.0 } else { 96.0 };
    let mut rects = Vec::new();
    ui.horizontal_top(|ui| {
        for axis in axes.iter().take(count) {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
            rects.push((axis.clone(), rect));
            ui.add_space(gap);
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
    let homed = app
        .snapshot
        .machine
        .homed
        .get(axis)
        .copied()
        .unwrap_or(false);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6, theme::PANEL);
    painter.rect_stroke(
        rect,
        6,
        egui::Stroke::new(1.0, if active { theme::ACCENT } else { theme::BORDER }),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + Vec2::new(14.0, 12.0),
        egui::Align2::LEFT_TOP,
        axis,
        FontId::proportional(theme::font_size(15.0)),
        theme::C3,
    );
    if active {
        painter.text(
            rect.left_top() + Vec2::new(38.0, 14.0),
            egui::Align2::LEFT_TOP,
            "ACTIVE",
            FontId::monospace(theme::font_size(8.5)),
            theme::ACCENT,
        );
    }
    painter.text(
        rect.right_top() + Vec2::new(-14.0, 12.0),
        egui::Align2::RIGHT_TOP,
        if homed { "HOMED" } else { "NOT HOMED" },
        FontId::monospace(theme::font_size(9.0)),
        if homed {
            theme::SUCCESS
        } else {
            theme::WARNING
        },
    );
    painter.text(
        rect.left_top() + Vec2::new(14.0, 38.0),
        egui::Align2::LEFT_CENTER,
        crate::theme::format_position(work, 3),
        FontId::monospace(if compact { 22.0 } else { 30.0 }),
        theme::FG,
    );
    painter.text(
        rect.left_bottom() + Vec2::new(14.0, -12.0),
        egui::Align2::LEFT_BOTTOM,
        format!("M {machine:+.3}"),
        FontId::monospace(theme::font_size(9.5)),
        theme::MUTED,
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-14.0, -12.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("DTG {dtg:.3}"),
        FontId::monospace(theme::font_size(9.5)),
        theme::MUTED,
    );
}

pub fn health_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "MACHINE HEALTH");
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
        theme::separator(ui);
        theme::kv_row(
            ui,
            "COOLANT",
            if app.snapshot.machine.coolant_flood {
                "FLOOD"
            } else if app.snapshot.machine.coolant_mist {
                "MIST"
            } else {
                "OFF"
            },
        );
        theme::kv_row(
            ui,
            "LUBE",
            if app.snapshot.machine.lube {
                "ON"
            } else {
                "OFF"
            },
        );
        theme::kv_row(
            ui,
            "PANEL",
            if app.snapshot.panel.connected {
                "ONLINE"
            } else {
                "OFFLINE"
            },
        );
        theme::kv_row(
            ui,
            "PROTECT",
            if app.program_protected() { "ON" } else { "OFF" },
        );
    });
}

pub fn alarm_card(app: &LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height(height);
        ui.horizontal(|ui| {
            theme::section_header(ui, "ALARMS / MESSAGES");
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

pub fn program_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROGRAM");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(crate::theme::format_position(
                        app.snapshot.program.progress as f64 * 100.0,
                        0,
                    ))
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
                );
            });
        });
        let name = if app.snapshot.program.display_name.is_empty() {
            "NO PROGRAM".to_string()
        } else {
            app.snapshot.program.display_name.clone()
        };
        ui.label(
            RichText::new(name)
                .monospace()
                .size(theme::font_size(13.0))
                .color(theme::FG),
        );
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "LN {}/{}  {:.0}%",
                    app.snapshot.program.current_line,
                    app.snapshot.program.line_count,
                    app.snapshot.program.progress * 100.0
                ))
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
            );
            ui.label(
                RichText::new(if app.snapshot.task.running {
                    "RUN"
                } else {
                    "STOP"
                })
                .monospace()
                .size(theme::font_size(10.0))
                .color(if app.snapshot.task.running {
                    theme::SUCCESS
                } else {
                    theme::MUTED
                }),
            );
        });
        token_ui(ui, &truncate(&app.snapshot.program.current_block, 44), 12.0);
        if let Some(next) = &app.snapshot.program.next_line {
            ui.label(
                RichText::new("NEXT")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            token_ui(ui, &truncate(next, 44), 11.0);
        }
        if !app.snapshot.program.modal.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new("MODAL")
                        .size(theme::font_size(8.5))
                        .color(theme::MUTED),
                );
                for (key, value) in app.snapshot.program.modal.iter().take(6) {
                    ui.label(
                        RichText::new(format!("{key}{value}"))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::FG),
                    );
                }
            });
        }
    });
}

pub fn feed_spindle_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "FEED / SPINDLE LOAD");
        theme::meter(
            ui,
            "FEED OVR",
            app.snapshot.feed.feed_override / 2.0,
            format!("{:.0}%", app.snapshot.feed.feed_override * 100.0),
            theme::ACCENT,
        );
        ui.label(
            RichText::new(format!(
                "F {:.0} {} / MIN",
                app.snapshot.feed.feed, app.snapshot.feed.units
            ))
            .monospace()
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
        for spindle in app.snapshot.spindles.iter().take(2) {
            let load = spindle.load.unwrap_or(0.0);
            theme::meter(
                ui,
                &spindle.label(),
                load / 100.0,
                format!("{:.0}%", load),
                theme::pressure_color(load / 100.0),
            );
            ui.label(
                RichText::new(format!(
                    "S {:.0} RPM / OVR {:.0}%",
                    spindle.actual_speed,
                    spindle.override_factor * 100.0
                ))
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
            );
        }
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
                    .size(theme::font_size(24.0))
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
