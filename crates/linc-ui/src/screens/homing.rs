//! Panel-only homing and PROG.HOME observation screen.
//!
//! Layout base: the same live graph + large coordinate readout + mode context
//! split as the Manual (JOG / INC / MPG) screen, so the two mode-driven
//! screens stay consistent. The HMI only observes; it never homes an axis.

use egui::{RichText, Ui};
use linc_core::homing::{axis_rows, AxisHomeStatus, MAPPINGS};
use linc_core::model::PanelMode;

use crate::screens::common::{self, LiveAxisRow};
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let mode = app.snapshot.panel.mode.mode();
    let program_zero = mode == Some(PanelMode::ProgHome);
    let on_screen = matches!(mode, Some(PanelMode::Home) | Some(PanelMode::ProgHome));
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
    if !on_screen {
        theme::banner(
            ui,
            theme::MUTED,
            "This screen opens automatically when the physical panel is in HOME or PROG.HOME.",
        );
        ui.add_space(4.0);
    }
    let mode_label = mode.map(|mode| mode.label()).unwrap_or("UNKNOWN");
    common::live_mode_header(
        ui,
        mode_label,
        on_screen,
        "NO ON-SCREEN HOME, UNHOME, JOG OR OVERRIDE CONTROLS",
    );

    let rows = axis_rows(&app.snapshot, &app.profile.machine_ini.axes);
    let readout = homing_readout(app, &rows);

    common::live_split(
        app,
        ui,
        "homing-live-right",
        |app, ui| {
            let height = (ui.available_height() - 20.0).max(320.0);
            common::live_graph_card(app, ui, height);
        },
        |app, ui| {
            common::live_coordinates_card(
                ui,
                "MACHINE POSITION",
                &format!("{} / MACHINE", app.snapshot.positions.units),
                &readout,
            );
            homing_context_card(app, ui, &rows);
            panel_mapping_card(ui);
            if program_zero {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PROGRAM ZERO");
                    ui.label(
                        RichText::new("RAPID sends G0 X0Y0Z0; X±/Y±/Z± send G0 X0 / Y0 / Z0 in the current WCS. The HMI does not intercept these panel actions.")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            }
        },
    );
}

/// Per-axis live readout with the homing state as the row badge and the INI
/// sequence/search/latch detail as the note.
fn homing_readout(app: &LincApp, rows: &[linc_core::homing::HomeAxisRow]) -> Vec<LiveAxisRow> {
    rows.iter()
        .map(|row| {
            let position = app.snapshot.positions.axes.get(&row.name);
            LiveAxisRow {
                name: row.name.clone(),
                work: position.map(|p| p.work).unwrap_or(row.position),
                machine: position.map(|p| p.machine).unwrap_or(row.position),
                dtg: position.map(|p| p.dtg).unwrap_or(0.0),
                active: app
                    .snapshot
                    .panel
                    .jog
                    .active_axis
                    .as_deref()
                    .map(|name| name == row.name)
                    .unwrap_or(false),
                badge: Some((row.status.label().to_string(), status_color(row.status))),
                note: Some(match row.status {
                    AxisHomeStatus::NotConfigured => "OPTIONAL / NOT CONFIGURED".to_string(),
                    _ => format!(
                        "SEQ {} / SEARCH {:.1} / LATCH {:.1} / INDEX {}",
                        row.sequence,
                        row.search_vel,
                        row.latch_vel,
                        if row.use_index { "YES" } else { "NO" }
                    ),
                }),
            }
        })
        .collect()
}

fn status_color(status: AxisHomeStatus) -> egui::Color32 {
    match status {
        AxisHomeStatus::Homed => theme::SUCCESS,
        AxisHomeStatus::Homing => theme::ACCENT,
        AxisHomeStatus::NotHomed => theme::WARNING,
        AxisHomeStatus::Unknown | AxisHomeStatus::NotConfigured => theme::MUTED,
    }
}

/// Mode context: what the panel is doing right now, plus the INI sequence.
fn homing_context_card(app: &LincApp, ui: &mut Ui, rows: &[linc_core::homing::HomeAxisRow]) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "MODE CONTEXT");
        let mode = app.snapshot.panel.mode.mode();
        let homed = rows
            .iter()
            .filter(|row| row.status == AxisHomeStatus::Homed)
            .count();
        let homing = rows
            .iter()
            .any(|row| row.status == AxisHomeStatus::Homing);
        theme::kv_row(
            ui,
            "PANEL MODE",
            match mode {
                Some(PanelMode::Home) => "HOME",
                Some(PanelMode::ProgHome) => "PROG.HOME",
                _ => "UNKNOWN",
            },
        );
        theme::kv_row(ui, "AXES HOMED", format!("{homed}/{}", rows.len()));
        theme::kv_row(
            ui,
            "SEQUENCE",
            if homing {
                "RUNNING".to_string()
            } else if homed == rows.len() && !rows.is_empty() {
                "COMPLETE".to_string()
            } else {
                "INCOMPLETE".to_string()
            },
        );
        theme::kv_row(
            ui,
            "HOME ORDER",
            app.profile
                .machine_ini
                .home_order()
                .iter()
                .map(|axis| format!("{}", axis.name))
                .collect::<Vec<_>>()
                .join(" → "),
        );
        theme::kv_row(
            ui,
            "NO_FORCE_HOMING",
            if app.snapshot.machine.force_homing {
                "PROGRAMS BLOCKED UNTIL HOMED"
            } else {
                "PROGRAMS MAY RUN UNHOMED"
            },
        );
        theme::kv_row(
            ui,
            "LIMITS",
            if app.snapshot.machine.limits.is_empty() {
                "OK".to_string()
            } else {
                format!("ACTIVE: {}", app.snapshot.machine.limits.join(", "))
            },
        );
        ui.label(
            RichText::new("HOME_SEQUENCE, search/latch/index and limits come from the INI. Optional axes are shown only when configured.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

fn panel_mapping_card(ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "PANEL MAPPING");
        egui::Grid::new("panel-mapping")
            .num_columns(3)
            .striped(true)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                for header in ["KEY", "HOME ACTION", "PROG.HOME MDI"] {
                    ui.label(
                        RichText::new(header)
                            .size(theme::font_size(8.5))
                            .strong()
                            .color(theme::MUTED),
                    );
                }
                ui.end_row();
                for mapping in MAPPINGS {
                    ui.label(
                        RichText::new(mapping.key.label())
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::C3),
                    );
                    ui.label(
                        RichText::new(mapping.home_action)
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::FG),
                    );
                    ui.label(
                        RichText::new(mapping.proghome_mdi)
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::SUCCESS),
                    );
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Individual home outputs are disabled while any axis is homing; gating is in the panel service/post-GUI HAL.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new("PROG.HOME is current-WCS zero, not machine home and not G53. X/Y/Z only; the panel issues MDI immediately.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        if ui.available_width() > 0.0 {
            ui.label(
                RichText::new("Lathe X is shown in the configured radius/diameter convention.")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        }
    });
}
