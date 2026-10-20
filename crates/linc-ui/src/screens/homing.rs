//! Panel-only homing and PROG.HOME observation screen.

use egui::{RichText, Ui, Vec2};
use linc_core::homing::{axis_rows, AxisHomeStatus, MAPPINGS};
use linc_core::model::PanelMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let mode = app.snapshot.panel.mode.mode();
    let program_zero = mode == Some(PanelMode::ProgHome);
    let title = if program_zero {
        "PROGRAM ZERO"
    } else {
        "HOMING"
    };
    common::page_header(
        ui,
        title,
        if program_zero {
            "PROG.HOME moves X/Y/Z to zero in the current WCS. Panel keys only; the HMI never issues home."
        } else {
            "HOME mode is panel-only. The HMI observes sequence and state; it never homes an axis."
        },
    );
    if !matches!(mode, Some(PanelMode::Home) | Some(PanelMode::ProgHome)) {
        theme::banner(
            ui,
            theme::MUTED,
            "This screen opens automatically when the physical panel is in HOME or PROG.HOME.",
        );
        ui.add_space(4.0);
    }
    let rows = axis_rows(&app.snapshot, &app.profile.machine_ini.axes);
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new((available * 0.58).max(520.0), 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::section_header(ui, "AXIS SEQUENCE STATE");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let all = !app.snapshot.machine.homed.is_empty()
                                && app.snapshot.machine.homed.iter().all(|(_, homed)| *homed);
                            ui.label(
                                RichText::new(if all { "ALL AXES HOMED" } else { "HOMING INCOMPLETE" })
                                    .monospace()
                                    .size(10.0)
                                    .color(if all { theme::SUCCESS } else { theme::WARNING }),
                            );
                        });
                    });
                    egui::Grid::new("home-axis-state")
                        .num_columns(7)
                        .striped(true)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            for header in ["AXIS", "STATUS", "SEQ", "SEARCH", "LATCH", "INDEX", "POS"] {
                                ui.label(RichText::new(header).size(9.0).strong().color(theme::MUTED));
                            }
                            ui.end_row();
                            for row in &rows {
                                let color = match row.status {
                                    AxisHomeStatus::Homed => theme::SUCCESS,
                                    AxisHomeStatus::Homing => theme::ACCENT,
                                    AxisHomeStatus::NotHomed => theme::WARNING,
                                    AxisHomeStatus::Unknown => theme::MUTED,
                                    AxisHomeStatus::NotConfigured => theme::MUTED,
                                };
                                ui.label(RichText::new(&row.name).monospace().size(14.0).color(theme::C3));
                                ui.label(RichText::new(row.status.label()).monospace().size(11.0).color(color));
                                ui.label(RichText::new(row.sequence.to_string()).monospace().size(11.0));
                                ui.label(RichText::new(format!("{:.1}", row.search_vel)).monospace().size(11.0));
                                ui.label(RichText::new(format!("{:.1}", row.latch_vel)).monospace().size(11.0));
                                ui.label(RichText::new(if row.use_index { "YES" } else { "NO" }).monospace().size(11.0));
                                ui.label(RichText::new(format!("{:+.3}", row.position)).monospace().size(11.0));
                                ui.end_row();
                            }
                        });
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "HOME SEQUENCE / INI");
                    theme::kv_row(ui, "ORDER", app.profile.machine_ini.home_order().iter().map(|axis| format!("{}", axis.name)).collect::<Vec<_>>().join(" → "));
                    theme::kv_row(ui, "NO_FORCE_HOMING", if app.snapshot.machine.force_homing { "PROGRAMS BLOCKED UNTIL HOMED" } else { "PROGRAMS MAY RUN UNHOMED" });
                    theme::kv_row(ui, "HOMED", format!("{}/{}", rows.iter().filter(|row| row.status == AxisHomeStatus::Homed).count(), rows.len()));
                    ui.label(
                        RichText::new("HOME_SEQUENCE and search/latch/index values come from the INI; optional axes are shown only when configured.")
                            .size(9.0)
                            .color(theme::MUTED),
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available - (available * 0.58).max(520.0) - 8.0).max(340.0), 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PANEL MAPPING");
                    egui::Grid::new("panel-mapping")
                        .num_columns(5)
                        .striped(true)
                        .spacing([8.0, 3.0])
                        .show(ui, |ui| {
                            for header in ["KEY", "HOME PIN", "HOME ACTION", "PROG.HOME PIN", "PROG.HOME MDI"] {
                                ui.label(RichText::new(header).size(8.5).strong().color(theme::MUTED));
                            }
                            ui.end_row();
                            for mapping in MAPPINGS {
                                ui.label(RichText::new(mapping.key.label()).monospace().size(10.0).color(theme::C3));
                                ui.label(RichText::new(mapping.home_pin).monospace().size(8.0).color(theme::MUTED));
                                ui.label(RichText::new(mapping.home_action).monospace().size(9.0).color(theme::FG));
                                ui.label(RichText::new(mapping.proghome_pin).monospace().size(8.0).color(theme::MUTED));
                                ui.label(RichText::new(mapping.proghome_mdi).monospace().size(9.0).color(theme::SUCCESS));
                                ui.end_row();
                            }
                        });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Individual home outputs are disabled while any axis is homing; gating is in the panel service/post-GUI HAL.")
                            .size(9.0)
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("PROG.HOME is current-WCS zero, not machine home and not G53. X/Y/Z only; the panel issues MDI immediately.")
                            .size(9.0)
                            .color(theme::MUTED),
                    );
                });
            },
        );
    });
    ui.add_space(6.0);
    if program_zero {
        theme::card(ui, |ui| {
            theme::section_header(ui, "PROGRAM ZERO");
            ui.label(
                RichText::new("RAPID sends G0 X0Y0Z0; X±/Y±/Z± send G0 X0 / Y0 / Z0 in the current WCS. The HMI does not intercept these panel actions.")
                    .size(10.0)
                    .color(theme::MUTED),
            );
        });
    }
}
