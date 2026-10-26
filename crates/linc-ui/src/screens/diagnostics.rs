//! Read-only diagnostics: panel link, HAL/NML, motion, I/O, turret, logs.

use egui::{RichText, Ui};
use linc_core::model::{format_clock, format_elapsed, Alarm, AlarmSeverity, AlarmSource};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::DiagnosticsTab;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let tabs = DiagnosticsTab::visible(&app.profile);
    // A tab remembered or restored from an earlier profile can name one this
    // profile hides; nothing below may draw its body.
    if !tabs.contains(&app.diagnostics_tab) {
        app.diagnostics_tab = DiagnosticsTab::Overview;
    }
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "diagnostics-page-focus");
    if app.diagnostics_clear_view {
        app.diagnostics_clear_view = false;
    }
    ui.horizontal(|ui| {
        for tab in tabs {
            if common::page_tab(ui, tab.label(), app.diagnostics_tab == tab).clicked() {
                app.diagnostics_tab = tab;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("READ-ONLY")
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        });
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("diagnostics-body")
        .max_height(ui.available_height())
        .show(ui, |ui| match app.diagnostics_tab {
            DiagnosticsTab::Overview => overview(app, ui),
            DiagnosticsTab::PanelLink => panel_link(app, ui),
            DiagnosticsTab::HalNml => hal_nml(app, ui),
            DiagnosticsTab::Motion => motion(app, ui),
            DiagnosticsTab::Turret => turret(app, ui),
            DiagnosticsTab::Io => io(app, ui),
            DiagnosticsTab::Logs => logs(app, ui),
        });
}

/// `theme::card`'s frame margin, on each side.
const CARD_PAD: f32 = 12.0;

/// Draw a card pinned to `width`, and to `height` when one is given.
///
/// A frame takes its size from its content, so a card inside a column of fixed
/// height used to end on its last row and leave the column's remainder empty.
/// The body is laid out top-down here instead of inheriting the caller's layout,
/// and the frame content is given the exact rect the column reserved.
fn pinned_card(ui: &mut Ui, width: f32, height: Option<f32>, add: impl FnOnce(&mut Ui)) {
    theme::card(ui, |ui| {
        ui.set_width((width - 2.0 * CARD_PAD).max(0.0));
        if let Some(height) = height {
            ui.set_height((height - 2.0 * CARD_PAD).max(0.0));
        }
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), add);
    });
}

/// The width of one of `remaining` equal columns left in the row, so rounding
/// never accumulates and the last card still ends on the page margin.
fn even_width(ui: &Ui, remaining: f32, gap: f32) -> f32 {
    (ui.available_width() - gap * (remaining - 1.0)) / remaining
}

/// A card as wide as `width` whose height follows its rows.
fn width_pinned_card(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    pinned_card(ui, width, None, add);
}

/// A card that occupies exactly `width` x `height`.
fn sized_card(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) {
    pinned_card(ui, width, Some(height), add);
}

fn overview(app: &mut LincApp, ui: &mut Ui) {
    // The page's card rhythm: 12 px between cards across a row, 8 px between the
    // two cards of the right column.
    const COLUMN_GAP: f32 = 12.0;
    const GAP: f32 = 8.0;
    let available = ui.available_width();
    // Each card is as tall as its three lines and pins its own width: a fixed
    // height left a blank strip under the detail line of all four.
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        health_card(
            ui,
            even_width(ui, 4.0, COLUMN_GAP),
            "CONTROL",
            if app.snapshot.machine.estop {
                "E-STOP"
            } else {
                "HEALTHY"
            },
            app.snapshot.machine.estop,
            &format!(
                "{} / {}",
                app.snapshot.task.mode.label(),
                app.snapshot.task.interp.label()
            ),
        );
        health_card(
            ui,
            even_width(ui, 3.0, COLUMN_GAP),
            "PANEL LINK",
            if app.snapshot.panel.connected {
                "ONLINE"
            } else {
                "OFFLINE"
            },
            !app.snapshot.panel.connected,
            &format!(
                "{} {} / CRC {} / TO {}",
                app.snapshot.panel.capabilities.transport.to_uppercase(),
                app.snapshot.panel.link.port,
                app.snapshot.panel.link.crc_errors,
                app.snapshot.panel.link.timeouts
            ),
        );
        health_card(
            ui,
            even_width(ui, 2.0, COLUMN_GAP),
            "NML / COMMAND",
            if app.snapshot.machine.nml_command_connected {
                "CONNECTED"
            } else {
                "LOST"
            },
            !app.snapshot.machine.nml_command_connected,
            &format!(
                "STATUS {} ms / ECHO {}",
                app.snapshot.machine.status_age_ms, app.snapshot.machine.echo_serial
            ),
        );
        let following = max_following_error(app);
        let servo = max_servo_load(app);
        health_card(
            ui,
            even_width(ui, 1.0, COLUMN_GAP),
            "MOTION",
            if following > 50.0 {
                "ELEVATED"
            } else {
                "NOMINAL"
            },
            following > 50.0,
            &format!("FOLLOW {following:.1} µm / SERVO {servo:.0}%"),
        );
    });
    ui.add_space(6.0);
    // Two columns: the alarm table owns the left one outright, the summaries
    // stack in the right one. Each card is handed the rect its column reserves,
    // so the two columns end on the same line instead of on their content.
    // The row runs down to the status line, less a small pad, so the page has no
    // dead band under the cards at any window height.
    const ROW_PAD: f32 = 8.0;
    let row_height = (ui.available_height() - ROW_PAD).max(240.0);
    // The right column takes whatever the left one and the gap leave, so the row
    // ends on the page margin with the health row above.
    let left_width = available * 0.58;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(left_width, row_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                sized_card(ui, left_width, row_height, |ui| {
                    let room = ui.available_height();
                    let top = ui.cursor().top();
                    ui.horizontal(|ui| {
                        theme::section_header(ui, "ACTIVE ALARMS & MESSAGES");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let logs = app.logs();
                            let alarms = logs.iter().filter(|a| a.severity.is_alarm()).count();
                            let messages = logs.len().saturating_sub(alarms);
                            for (label, count) in [
                                ("MESSAGES", messages),
                                ("ALARMS", alarms),
                                ("ALL", logs.len()),
                            ] {
                                ui.label(
                                    RichText::new(format!("{label} {count}"))
                                        .monospace()
                                        .size(theme::font_size(8.0))
                                        .color(theme::MUTED),
                                );
                                ui.add_space(8.0);
                            }
                        });
                    });
                    // The whole card is the table now, so it scrolls through the
                    // log rather than cutting it at five rows.
                    let header = ui.cursor().top() - top;
                    let logs = app.logs();
                    egui::ScrollArea::vertical()
                        .id_salt("overview-alarms-scroll")
                        .max_height((room - header).max(60.0))
                        .show(ui, |ui| log_table(ui, "overview-alarms", &logs, usize::MAX));
                });
            },
        );
        // The layout already spaces the two columns; top the gap up to the page
        // rhythm without touching the spacing the card bodies are laid out with.
        ui.add_space((COLUMN_GAP - ui.spacing().item_spacing.x).max(0.0));
        let right_width = ui.available_width().max(300.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new(right_width, row_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                // Eight rows decide PANEL SUMMARY's height, and a frame cannot be
                // shorter than them: let it take its content height and hand
                // MOTION HEALTH the rest of the column, so the column ends on the
                // same line as the alarm card whatever the row pitch is.
                let top = ui.cursor().top();
                width_pinned_card(ui, right_width, |ui| {
                    compact_rows(ui);
                    theme::section_header(ui, "PANEL SUMMARY");
                    theme::kv_row(ui, "MODE", app.snapshot.panel.mode.label());
                    theme::kv_row(
                        ui,
                        "PROTECT",
                        if app.program_protected() {
                            "ON / LOCKED"
                        } else {
                            "OFF"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "E-STOP",
                        if app.snapshot.panel.estop {
                            "ACTIVE"
                        } else {
                            "OK"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "FEED OVR",
                        format!("{:.0}%", app.snapshot.panel.feed_override * 100.0),
                    );
                    theme::kv_row(
                        ui,
                        "RAPID OVR",
                        format!("{:.0}%", app.snapshot.panel.rapid_override * 100.0),
                    );
                    theme::kv_row(
                        ui,
                        "SPINDLE OVR",
                        format!("{:.0}%", app.snapshot.panel.spindle_override * 100.0),
                    );
                    theme::kv_row(
                        ui,
                        "STATUS DISPLAY",
                        status_display_text(app.snapshot.panel.sseg_status),
                    );
                    theme::kv_row(
                        ui,
                        "TOOLS",
                        format!(
                            "T{} / NEXT T{}",
                            app.snapshot.panel.sseg_tool, app.snapshot.panel.sseg_next_tool
                        ),
                    );
                });
                let used = ui.cursor().top() - top;
                sized_card(ui, right_width, (row_height - used).max(120.0), |ui| {
                    compact_rows(ui);
                    theme::section_header(ui, "MOTION HEALTH");
                    theme::kv_row(ui, "POWER", app.snapshot.task.power.label());
                    theme::kv_row(
                        ui,
                        "FOLLOWING ERROR",
                        format!("{:.3} µm", max_following_error(app)),
                    );
                    theme::kv_row(ui, "SERVO", format!("{:.0}%", max_servo_load(app)));
                    theme::kv_row(
                        ui,
                        "SPINDLE",
                        format!(
                            "{:.0} RPM / {:.0}%",
                            app.snapshot
                                .spindles
                                .first()
                                .map(|spindle| spindle.actual_speed)
                                .unwrap_or(0.0),
                            app.snapshot
                                .spindles
                                .first()
                                .and_then(|spindle| spindle.load)
                                .unwrap_or(0.0)
                        ),
                    );
                    theme::kv_row(ui, "HOMED", homed_summary(app));
                });
            },
        );
    });
}

/// Compact a card's rows so each key/value line reads as one line.
///
/// `interact_size.y` sizes a `ui.horizontal` row and the minimum height of a
/// grid row, so at `MIN_TARGET` every row carries a blank line of 44 pt plus
/// `item_spacing.y`: eight rows overflowed a summary card, and the same pitch
/// spaced the diagnostics panes apart. The spacing is set on the card's own
/// `Ui`, so it does not leak to the rest of the page.
fn compact_rows(ui: &mut Ui) {
    ui.spacing_mut().interact_size.y = 14.0;
    ui.spacing_mut().item_spacing.y = 2.0;
}

fn health_card(ui: &mut Ui, width: f32, title: &str, value: &str, fault: bool, detail: &str) {
    width_pinned_card(ui, width, |ui| {
        theme::section_header(ui, title);
        ui.label(
            RichText::new(value)
                .size(theme::font_size(20.0))
                .strong()
                .color(if fault { theme::ERROR } else { theme::SUCCESS }),
        );
        ui.label(
            RichText::new(detail)
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

fn panel_link(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.5, 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "RS-485 / VIRTUAL LINK");
                    compact_rows(ui);
                    let link = &app.snapshot.panel.link;
                    theme::kv_row(
                        ui,
                        "TRANSPORT",
                        app.snapshot.panel.capabilities.transport.to_uppercase(),
                    );
                    theme::kv_row(
                        ui,
                        "PORT",
                        if link.port.is_empty() {
                            "—".into()
                        } else {
                            link.port.clone()
                        },
                    );
                    theme::kv_row(ui, "BAUD", link.baud.to_string());
                    theme::kv_row(ui, "CYCLE", format!("{:.1} ms", link.cycle_time_ms));
                    theme::kv_row(
                        ui,
                        "LAST RX",
                        link.last_rx_age_ms
                            .map(format_elapsed)
                            .unwrap_or_else(|| "—".into()),
                    );
                    theme::kv_row(ui, "RX BYTES", link.rx_bytes.to_string());
                    theme::kv_row(ui, "TX BYTES", link.tx_bytes.to_string());
                    theme::kv_row(ui, "CRC ERRORS", link.crc_errors.to_string());
                    theme::kv_row(ui, "TIMEOUTS", link.timeouts.to_string());
                    theme::kv_row(ui, "RECONNECTS", link.reconnects.to_string());
                    if !link.available {
                        ui.label(
                            RichText::new("link counters unavailable in this profile")
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                    }
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "CAPABILITIES");
                    compact_rows(ui);
                    let caps = &app.snapshot.panel.capabilities;
                    theme::kv_row(ui, "BUTTONS", caps.buttons.to_string());
                    theme::kv_row(ui, "LEDS", caps.leds.to_string());
                    theme::kv_row(ui, "DISPLAYS", caps.displays.to_string());
                    theme::kv_row(ui, "MODE ENCODING", caps.mode_encoding.clone());
                    theme::kv_row(ui, "OVERRIDE ENCODING", caps.override_encoding.clone());
                    theme::kv_row(
                        ui,
                        "PENDANT",
                        if caps.pendant_supported {
                            "WHB04B-6 JOG ONLY"
                        } else {
                            "NONE"
                        },
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.5 - 8.0).max(300.0), 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "MODE / OVERRIDES");
                    compact_rows(ui);
                    theme::kv_row(ui, "MODE", app.snapshot.panel.mode.label());
                    if let linc_core::model::PanelModeValidity::Invalid(active) =
                        &app.snapshot.panel.mode
                    {
                        ui.label(
                            RichText::new(format!(
                                "INVALID: {} active inputs — failing closed",
                                active
                                    .iter()
                                    .map(|mode| mode.label())
                                    .collect::<Vec<_>>()
                                    .join(" + ")
                            ))
                            .color(theme::ERROR),
                        );
                    }
                    theme::kv_row(
                        ui,
                        "FEED",
                        format!("{:.3}", app.snapshot.panel.feed_override),
                    );
                    theme::kv_row(
                        ui,
                        "RAPID",
                        format!("{:.3}", app.snapshot.panel.rapid_override),
                    );
                    theme::kv_row(
                        ui,
                        "SPINDLE",
                        format!("{:.3}", app.snapshot.panel.spindle_override),
                    );
                    theme::kv_row(
                        ui,
                        "PROGRAM PROTECT",
                        if app.snapshot.panel.protect_effective() {
                            "ON / UNKNOWN=ON"
                        } else {
                            "OFF"
                        },
                    );
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "RAW BUTTON / LED BITMAPS");
                    compact_rows(ui);
                    theme::kv_row(
                        ui,
                        "BUTTONS",
                        format!("0x{:016X}", app.snapshot.panel.buttons),
                    );
                    theme::kv_row(ui, "LEDS", format!("0x{:016X}", app.snapshot.panel.leds));
                    theme::kv_row(ui, "7SEG TOOL", app.snapshot.panel.sseg_tool.to_string());
                    theme::kv_row(
                        ui,
                        "7SEG NEXT",
                        app.snapshot.panel.sseg_next_tool.to_string(),
                    );
                    theme::kv_row(
                        ui,
                        "7SEG STATUS",
                        status_display_text(app.snapshot.panel.sseg_status),
                    );
                    theme::kv_row(ui, "HOME", bitmap(&app.snapshot.panel.home));
                    theme::kv_row(ui, "PROG.HOME", bitmap(&app.snapshot.panel.proghome));
                });
            },
        );
    });
}

fn hal_nml(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.42, 500.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "NML CHANNELS");
                    compact_rows(ui);
                    theme::kv_row(
                        ui,
                        "COMMAND",
                        if app.snapshot.machine.nml_command_connected {
                            "CONNECTED"
                        } else {
                            "LOST"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "STATUS",
                        if app.snapshot.machine.nml_status_connected {
                            "CONNECTED"
                        } else {
                            "LOST"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "STATUS AGE",
                        format_elapsed(app.snapshot.machine.status_age_ms),
                    );
                    theme::kv_row(
                        ui,
                        "COMMAND AGE",
                        format_elapsed(app.snapshot.machine.command_age_ms),
                    );
                    theme::kv_row(
                        ui,
                        "ECHO SERIAL",
                        app.snapshot.machine.echo_serial.to_string(),
                    );
                    theme::kv_row(
                        ui,
                        "HAL THREAD",
                        if app.snapshot.machine.hal_thread_ok {
                            "OK"
                        } else {
                            "FAULT"
                        },
                    );
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "DECLARED linc.* OUTPUTS");
                    compact_rows(ui);
                    for output in linc_core::command::LincOutput::ALLOWED {
                        let value = match output {
                            linc_core::command::LincOutput::HmiAlive => {
                                app.snapshot.hal.hmi_alive.to_string()
                            }
                            linc_core::command::LincOutput::ToolChangeConfirm => {
                                app.snapshot.hal.tool_change_confirm.to_string()
                            }
                            linc_core::command::LincOutput::StatusDisplay => app
                                .snapshot
                                .hal
                                .status_display
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".into()),
                            linc_core::command::LincOutput::JogIncrement => app
                                .snapshot
                                .hal
                                .jog_increment
                                .map(|value| format!("{value}"))
                                .unwrap_or_else(|| "0 (continuous)".into()),
                            linc_core::command::LincOutput::JogSpeed => app
                                .snapshot
                                .hal
                                .jog_speed
                                .map(|value| format!("{value:.1}"))
                                .unwrap_or_else(|| "0".into()),
                        };
                        theme::kv_row(ui, output.pin_name(), value);
                    }
                    ui.label(
                        RichText::new("No generic HAL write path exists.")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.42 - 8.0).max(320.0), 500.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "HAL PIN BROWSER");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.diagnostics_search)
                            .id(egui::Id::unique("linc-diagnostics-search"))
                            .hint_text("filter pin")
                            .desired_width(220.0),
                    );
                    compact_rows(ui);
                    let filter = app.diagnostics_search.to_ascii_lowercase();
                    let pins = hal_pin_rows(app);
                    egui::ScrollArea::vertical()
                        .id_salt("hal-pins-scroll")
                        .max_height(390.0)
                        .show(ui, |ui| {
                            egui::Grid::new("hal-pins")
                                .num_columns(2)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (name, value) in pins {
                                        if !filter.is_empty()
                                            && !name.to_ascii_lowercase().contains(&filter)
                                        {
                                            continue;
                                        }
                                        ui.label(
                                            RichText::new(name)
                                                .monospace()
                                                .size(theme::font_size(10.0))
                                                .color(theme::C1),
                                        );
                                        ui.label(
                                            RichText::new(value)
                                                .monospace()
                                                .size(theme::font_size(10.0))
                                                .color(theme::FG),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                });
            },
        );
    });
}

fn motion(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.55, 440.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "AXIS MOTION");
                    compact_rows(ui);
                    egui::Grid::new("motion-grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            for header in ["AXIS", "COMMANDED", "ACTUAL", "FOLLOW", "SERVO"] {
                                ui.label(
                                    RichText::new(header)
                                        .size(theme::font_size(9.0))
                                        .strong()
                                        .color(theme::MUTED),
                                );
                            }
                            ui.end_row();
                            for axis in app.axes() {
                                ui.label(RichText::new(&axis).monospace().color(theme::C3));
                                ui.label(
                                    RichText::new(format!(
                                        "{:+.3}",
                                        app.snapshot
                                            .machine
                                            .commanded
                                            .get(&axis)
                                            .copied()
                                            .unwrap_or(0.0)
                                    ))
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{:+.3}",
                                        app.snapshot
                                            .machine
                                            .actual
                                            .get(&axis)
                                            .copied()
                                            .unwrap_or(0.0)
                                    ))
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{:.4}",
                                        app.snapshot
                                            .machine
                                            .following_error
                                            .get(&axis)
                                            .copied()
                                            .unwrap_or(0.0)
                                    ))
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                let load = app
                                    .snapshot
                                    .machine
                                    .servo_load
                                    .get(&axis)
                                    .copied()
                                    .unwrap_or(0.0);
                                ui.label(
                                    RichText::new(format!("{load:.0}%"))
                                        .monospace()
                                        .size(theme::font_size(10.0))
                                        .color(theme::pressure_color(load / 100.0)),
                                );
                                ui.end_row();
                            }
                        });
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.55 - 8.0).max(300.0), 440.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "SPINDLES");
                    compact_rows(ui);
                    for spindle in &app.snapshot.spindles {
                        theme::kv_row(
                            ui,
                            &spindle.label(),
                            format!(
                                "{:.0} RPM / {:.0}%",
                                spindle.actual_speed,
                                spindle.load.unwrap_or(0.0)
                            ),
                        );
                    }
                    theme::kv_row(
                        ui,
                        "FORCE HOMING",
                        if app.snapshot.machine.force_homing {
                            "PROGRAMS BLOCKED UNTIL HOMED"
                        } else {
                            "NO FORCE"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "LIMITS",
                        if app.snapshot.machine.limits.is_empty() {
                            "OK".into()
                        } else {
                            app.snapshot.machine.limits.join(", ")
                        },
                    );
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "HOMING STATE");
                    compact_rows(ui);
                    for axis in app.axes() {
                        let homed = app
                            .snapshot
                            .machine
                            .homed
                            .get(&axis)
                            .copied()
                            .unwrap_or(false);
                        let homing = app
                            .snapshot
                            .machine
                            .homing
                            .get(&axis)
                            .copied()
                            .unwrap_or(false);
                        theme::kv_row(
                            ui,
                            &axis,
                            if homing {
                                "HOMING".to_string()
                            } else if homed {
                                "HOMED".to_string()
                            } else {
                                "NOT HOMED".to_string()
                            },
                        );
                    }
                });
            },
        );
    });
}

fn turret(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "TURRET / LIVE TOOL");
        compact_rows(ui);
        let Some(atc) = app.snapshot.atc.as_ref() else {
            ui.label(
                RichText::new("No turret/ATC configured for this profile.").color(theme::MUTED),
            );
            return;
        };
        theme::kv_row(ui, "STATIONS", atc.pockets.to_string());
        theme::kv_row(
            ui,
            "STATION",
            atc.station
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "CURRENT POCKET",
            atc.current_pocket
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "PREPARED",
            atc.prepared_pocket
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "CLAMP",
            atc.clamped
                .map(|v| if v { "CLAMPED" } else { "UNCLAMPED" })
                .unwrap_or("UNKNOWN"),
        );
        theme::kv_row(ui, "INDEX", if atc.indexing { "INDEXING" } else { "READY" });
        theme::kv_row(
            ui,
            "LIVE TOOL RPM",
            atc.live_tool_rpm
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "OFF".into()),
        );
        theme::kv_row(
            ui,
            "LIVE TOOL DIR",
            if atc.live_tool_direction >= 0 {
                "CW"
            } else {
                "CCW"
            },
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

fn io(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.5, 450.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "MACHINE I/O");
                    compact_rows(ui);
                    theme::kv_row(ui, "FLOOD", on_off(app.snapshot.machine.coolant_flood));
                    theme::kv_row(ui, "MIST", on_off(app.snapshot.machine.coolant_mist));
                    theme::kv_row(ui, "LUBE", on_off(app.snapshot.machine.lube));
                    theme::kv_row(
                        ui,
                        "LUBE LEVEL",
                        if app.snapshot.machine.lube_level_ok {
                            "OK"
                        } else {
                            "LOW"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "SAFE DOOR",
                        if app.snapshot.machine.door_closed {
                            "CLOSED"
                        } else {
                            "OPEN"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "TOOL CHANGE",
                        on_off(app.snapshot.machine.tool_change_active),
                    );
                    theme::kv_row(
                        ui,
                        "E-STOP",
                        if app.snapshot.machine.estop {
                            "ACTIVE"
                        } else {
                            "OK"
                        },
                    );
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PANEL LEDs");
                    compact_rows(ui);
                    ui.label(
                        RichText::new(format!("0x{:016X}", app.snapshot.panel.leds))
                            .monospace()
                            .size(theme::font_size(14.0)),
                    );
                    ui.label(
                        RichText::new(led_names(app.snapshot.panel.leds))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(6.0);
                    theme::section_header(ui, "PANEL BUTTONS");
                    ui.label(
                        RichText::new(format!("0x{:016X}", app.snapshot.panel.buttons))
                            .monospace()
                            .size(theme::font_size(14.0)),
                    );
                    ui.label(
                        RichText::new(button_names(app.snapshot.panel.buttons))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.5 - 8.0).max(300.0), 450.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "HAL PIN BROWSER");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.diagnostics_search)
                            .id(egui::Id::unique("linc-diagnostics-search"))
                            .hint_text("filter pin")
                            .desired_width(220.0),
                    );
                    compact_rows(ui);
                    let filter = app.diagnostics_search.to_ascii_lowercase();
                    egui::ScrollArea::vertical()
                        .id_salt("io-hal-pins-scroll")
                        .max_height(370.0)
                        .show(ui, |ui| {
                            egui::Grid::new("io-hal-pins")
                                .num_columns(2)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (name, value) in hal_pin_rows(app) {
                                        if !filter.is_empty()
                                            && !name.to_ascii_lowercase().contains(&filter)
                                        {
                                            continue;
                                        }
                                        ui.label(
                                            RichText::new(name)
                                                .monospace()
                                                .size(theme::font_size(10.0))
                                                .color(theme::C1),
                                        );
                                        ui.label(
                                            RichText::new(value)
                                                .monospace()
                                                .size(theme::font_size(10.0)),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                });
            },
        );
    });
}

fn logs(app: &mut LincApp, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("SEARCH")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.add(
            egui::TextEdit::singleline(&mut app.diagnostics_search)
                .id(egui::Id::unique("linc-diagnostics-search"))
                .hint_text("message, code, source")
                .desired_width(240.0),
        );
        ui.add_space(6.0);
        for severity in [
            AlarmSeverity::Info,
            AlarmSeverity::Warning,
            AlarmSeverity::Error,
            AlarmSeverity::Critical,
        ] {
            let active = app.diagnostics_severity == Some(severity);
            if common::page_tab(ui, severity.label(), active).clicked() {
                app.diagnostics_severity = if active { None } else { Some(severity) };
            }
        }
        ui.add_space(6.0);
        for source in [
            AlarmSource::Panel,
            AlarmSource::Program,
            AlarmSource::Probe,
            AlarmSource::Tool,
            AlarmSource::Hal,
            AlarmSource::Nml,
            AlarmSource::Motion,
            AlarmSource::Io,
        ] {
            let active = app.diagnostics_source == Some(source);
            if common::page_tab(ui, source.label(), active).clicked() {
                app.diagnostics_source = if active { None } else { Some(source) };
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} EVENTS", app.logs().len()))
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        });
    });
    ui.add_space(4.0);
    theme::card(ui, |ui| {
        let logs: Vec<Alarm> = app
            .logs()
            .into_iter()
            .filter(|alarm| {
                app.diagnostics_severity
                    .map(|severity| alarm.severity == severity)
                    .unwrap_or(true)
                    && app
                        .diagnostics_source
                        .map(|source| alarm.source == source)
                        .unwrap_or(true)
                    && {
                        let query = app.diagnostics_search.to_ascii_lowercase();
                        query.is_empty()
                            || alarm.message.to_ascii_lowercase().contains(&query)
                            || alarm.code.to_ascii_lowercase().contains(&query)
                            || alarm.source.label().to_ascii_lowercase().contains(&query)
                    }
            })
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("log-scroll")
            .max_height(480.0)
            .show(ui, |ui| log_table(ui, "log-grid", &logs, usize::MAX));
    });
}

pub fn copy_log(app: &mut LincApp, ui: &mut Ui) {
    let logs = app.logs();
    let text = logs
        .iter()
        .map(|alarm| {
            format!(
                "{} {} {} {} {}",
                format_clock(alarm.timestamp_ms),
                alarm.severity.label(),
                alarm.source.label(),
                alarm.code,
                alarm.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ui.ctx().copy_text(text);
    app.toast("log copied to clipboard", theme::SUCCESS);
}

/// The alarm table.
///
/// Not an `egui::Grid`: a grid row cannot size itself around a wrapped cell, so
/// a long message spilled into the row below. Each entry is one horizontal line
/// whose last cell is a width-bounded scope, so the message wraps inside its
/// column and the row grows to fit it, with the metadata aligned to the top.
fn log_table(ui: &mut Ui, _id: &str, logs: &[Alarm], limit: usize) {
    const TIME: f32 = 66.0;
    const SEVERITY: f32 = 74.0;
    const SOURCE: f32 = 56.0;
    const CODE: f32 = 64.0;
    const GAP: f32 = 12.0;
    let tight = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 2.0;
    log_row(ui, [TIME, SEVERITY, SOURCE, CODE], GAP, None);
    for alarm in logs.iter().take(limit) {
        log_row(ui, [TIME, SEVERITY, SOURCE, CODE], GAP, Some(alarm));
        ui.add_space(3.0);
    }
    ui.spacing_mut().item_spacing.y = tight;
}

fn log_column(ui: &mut Ui, width: f32, text: &str, colour: egui::Color32, strong: bool) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            let mut rich = RichText::new(text)
                .monospace()
                .size(theme::font_size(9.0))
                .color(colour);
            if strong {
                rich = rich.strong();
            }
            ui.add(egui::Label::new(rich).wrap());
        },
    );
}

fn log_row(ui: &mut Ui, widths: [f32; 4], gap: f32, alarm: Option<&Alarm>) {
    let header = ["TIME", "SEVERITY", "SOURCE", "CODE"];
    ui.horizontal_top(|ui| {
        for (index, width) in widths.into_iter().enumerate() {
            let (text, colour, strong) = match alarm {
                None => (header[index].to_string(), theme::MUTED, true),
                Some(alarm) => {
                    let text = match index {
                        0 => format_clock(alarm.timestamp_ms),
                        1 => alarm.severity.label().to_string(),
                        2 => alarm.source.label().to_string(),
                        _ => {
                            if alarm.code.is_empty() {
                                "—".to_string()
                            } else {
                                alarm.code.clone()
                            }
                        }
                    };
                    let colour = if index == 1 {
                        severity_color(alarm.severity)
                    } else {
                        theme::MUTED
                    };
                    (text, colour, false)
                }
            };
            log_column(ui, width, &text, colour, strong);
        }
        ui.add_space(gap);
        // Whatever is left of the row is the message column.
        let rest = ui.available_width().max(80.0);
        ui.scope(|ui| {
            ui.set_max_width(rest);
            match alarm {
                None => {
                    ui.label(
                        RichText::new("MESSAGE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                }
                Some(alarm) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&alarm.message)
                                .size(theme::font_size(10.0))
                                .color(if alarm.severity == AlarmSeverity::Info {
                                    theme::MUTED
                                } else {
                                    severity_color(alarm.severity)
                                }),
                        )
                        .wrap(),
                    );
                }
            }
        });
    });
}

fn severity_color(severity: AlarmSeverity) -> egui::Color32 {
    match severity {
        AlarmSeverity::Info => theme::MUTED,
        AlarmSeverity::Warning => theme::WARNING,
        AlarmSeverity::Error | AlarmSeverity::Critical => theme::ERROR,
    }
}

fn status_display_text(value: i32) -> String {
    if (54..=59).contains(&value) {
        value.to_string()
    } else {
        "--".into()
    }
}

fn bitmap(values: &[bool; 4]) -> String {
    values
        .iter()
        .map(|value| if *value { "1" } else { "0" })
        .collect()
}

fn max_following_error(app: &LincApp) -> f64 {
    app.snapshot
        .machine
        .following_error
        .iter()
        .map(|(_, value)| value.abs())
        .fold(0.0, f64::max)
}

fn max_servo_load(app: &LincApp) -> f64 {
    app.snapshot
        .machine
        .servo_load
        .iter()
        .map(|(_, value)| *value)
        .fold(0.0, f64::max)
}

fn homed_summary(app: &LincApp) -> String {
    if app.snapshot.machine.homed.is_empty() {
        return "UNKNOWN".into();
    }
    let homed = app
        .snapshot
        .machine
        .homed
        .iter()
        .filter(|(_, value)| **value)
        .count();
    format!("{}/{}", homed, app.snapshot.machine.homed.len())
}

fn on_off(value: bool) -> &'static str {
    if value {
        "ON"
    } else {
        "OFF"
    }
}

fn hal_pin_rows(app: &LincApp) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let pins = &app.profile.config.hal.pins;
    rows.push((
        pins.panel_connected.clone(),
        app.snapshot.panel.connected.to_string(),
    ));
    rows.push((
        pins.panel_error.clone(),
        app.snapshot.panel.communication_error.to_string(),
    ));
    rows.push((
        pins.program_protect.clone(),
        app.snapshot.panel.program_protect.to_string(),
    ));
    rows.push((
        pins.feed_override.clone(),
        format!("{:.3}", app.snapshot.panel.feed_override),
    ));
    rows.push((
        pins.rapid_override.clone(),
        format!("{:.3}", app.snapshot.panel.rapid_override),
    ));
    rows.push((
        pins.spindle_override.clone(),
        format!("{:.3}", app.snapshot.panel.spindle_override),
    ));
    rows.push((
        "linc.hmi-alive".into(),
        app.snapshot.hal.hmi_alive.to_string(),
    ));
    rows.push((
        "linc.tool-change-confirm".into(),
        app.snapshot.hal.tool_change_confirm.to_string(),
    ));
    rows.push((
        "linc.status-display".into(),
        app.snapshot
            .hal
            .status_display
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into()),
    ));
    for (name, mode) in [
        ("cnc_panel.mode_edit", "EDIT"),
        ("cnc_panel.mode_auto", "AUTO"),
        ("cnc_panel.mode_mdi", "MDI"),
        ("cnc_panel.mode_jog", "JOG"),
        ("cnc_panel.mode_inc", "INC"),
        ("cnc_panel.mode_mpg", "MPG"),
        ("cnc_panel.mode_home", "HOME"),
        ("cnc_panel.mode_proghome", "PROG.HOME"),
    ] {
        let active = app.snapshot.panel.mode.label() == mode;
        rows.push((name.into(), active.to_string()));
    }
    rows.push((
        "cnc_panel.sseg_status".into(),
        app.snapshot.panel.sseg_status.to_string(),
    ));
    rows.push((
        "cnc_panel.sseg_magtool".into(),
        app.snapshot.panel.sseg_tool.to_string(),
    ));
    rows.push((
        "cnc_panel.sseg_spdtool".into(),
        app.snapshot.panel.sseg_next_tool.to_string(),
    ));
    for axis in app.axes() {
        let position = app.snapshot.positions.axes.get(&axis);
        rows.push((
            format!("axis.{}.position", axis.to_ascii_lowercase()),
            format!("{:.4}", position.map(|p| p.work).unwrap_or(0.0)),
        ));
    }
    rows.push((
        "spindle.0.speed".into(),
        format!(
            "{:.1}",
            app.snapshot
                .spindles
                .first()
                .map(|s| s.actual_speed)
                .unwrap_or(0.0)
        ),
    ));
    rows
}

fn led_names(bits: u64) -> String {
    let mut names = Vec::new();
    for (name, bit) in linc_panel_led_names() {
        if bits & (1 << bit) != 0 {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        "—".into()
    } else {
        names.join(" ")
    }
}

fn button_names(bits: u64) -> String {
    let mut names = Vec::new();
    for (name, bit) in linc_panel_button_names() {
        if bits & (1 << bit) != 0 {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        "—".into()
    } else {
        names.join(" ")
    }
}

fn linc_panel_led_names() -> &'static [(&'static str, u32)] {
    &[
        ("rapid", 0),
        ("mlk", 1),
        ("flush", 2),
        ("spd_sp", 3),
        ("k2", 4),
        ("c", 5),
        ("cycle_off", 6),
        ("jog_xn", 8),
        ("bdt", 9),
        ("spd_cts", 10),
        ("spd_cw", 11),
        ("arm_man", 12),
        ("b", 13),
        ("cycle_on", 14),
        ("jog_yp", 16),
        ("sbk", 17),
        ("coolant", 18),
        ("mag_ccw", 19),
        ("pot_dn", 20),
        ("a", 21),
        ("ready", 22),
        ("jog_zp", 24),
        ("mpg_sim", 25),
        ("pt_la", 26),
        ("mag_cw", 27),
        ("chuck", 28),
        ("z", 29),
        ("run", 30),
        ("jog_an", 32),
        ("jog_ap", 33),
        ("air_blow", 34),
        ("spd_ori", 35),
        ("chip_ccw", 36),
        ("y", 37),
        ("airblow_alarm", 38),
        ("jog_bp", 40),
        ("jog_zn", 41),
        ("worklamp", 42),
        ("k1", 43),
        ("chip_sp", 44),
        ("x", 45),
        ("oil_alarm", 46),
        ("safe_door", 48),
        ("jog_yn", 49),
        ("drn", 50),
        ("corridor", 51),
        ("chip_cw", 52),
        ("k4", 53),
        ("gear_alarm", 54),
        ("jog_bn", 56),
        ("jog_xp", 57),
        ("opt", 58),
        ("lub", 59),
        ("spd_ccw", 60),
        ("k3", 61),
        ("mc_alarm", 62),
    ]
}

fn linc_panel_button_names() -> &'static [(&'static str, u32)] {
    &[
        ("program_protect", 0),
        ("estop", 7),
        ("cycle_start", 14),
        ("cycle_stop", 15),
        ("rapid", 63),
        ("mlk", 55),
        ("flush", 47),
        ("spd_sp", 39),
        ("k2", 34),
        ("jog_xn", 62),
        ("bdt", 54),
        ("spd_cts", 46),
        ("spd_cw", 38),
        ("arm_man", 30),
        ("jog_yp", 61),
        ("sbk", 53),
        ("coolant", 45),
        ("mag_ccw", 37),
        ("pot_dn", 29),
        ("jog_zp", 60),
        ("mpg_sim", 52),
        ("pt_la", 44),
        ("mag_cw", 36),
        ("chuck", 28),
        ("jog_an", 59),
        ("jog_ap", 51),
        ("air_blow", 43),
        ("spd_ori", 35),
        ("chip_ccw", 27),
        ("jog_bp", 58),
        ("jog_zn", 50),
        ("worklamp", 42),
        ("chip_sp", 26),
        ("safe_door", 57),
        ("jog_yn", 49),
        ("drn", 41),
        ("corridor", 33),
        ("chip_cw", 25),
        ("k4", 17),
        ("jog_bn", 56),
        ("jog_xp", 48),
        ("opt", 40),
        ("lub", 32),
        ("spd_ccw", 24),
        ("k3", 16),
        ("k1", 31),
    ]
}
