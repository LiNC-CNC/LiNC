//! Panel-only homing and PROG.HOME observation screen.
//!
//! Layout base: the same live graph + large coordinate readout + mode context
//! split as the Manual (JOG / INC / MPG) screen, so the two mode-driven
//! screens stay consistent. The HMI only observes; it never homes an axis.

use egui::{RichText, Ui};
use linc_core::homing::{axis_rows, AxisHomeStatus};
use linc_core::model::PanelMode;

use crate::screens::common::{self, LiveAxisRow};
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // The Homing page advertises the same X/Y/Z and A touch-off keys as the
    // Manual page, so it has to handle them too.
    crate::screens::manual::handle_zero_keys(app, ui);
    let mode = app.snapshot.panel.mode.mode();
    let program_zero = mode == Some(PanelMode::ProgHome);
    let on_screen = matches!(mode, Some(PanelMode::Home) | Some(PanelMode::ProgHome));
    // No page title: the mode key names the mode and the
    // context card explains what the keys do.
    if !on_screen {
        theme::banner(
            ui,
            theme::MUTED,
            "This screen opens automatically when the physical panel is in HOME or PROG.HOME.",
        );
        ui.add_space(4.0);
    }
    // No mode banner here either: the mode key shows the mode.

    // A machine that could not power up because a joint is on a limit switch
    // can only be freed from here or the jog page, so the override is offered
    // wherever the operator might be looking.
    common::limits_override_row(app, ui);
    let rows = axis_rows(&app.snapshot, &app.profile.machine_ini.axes);
    let readout = homing_readout(app, &rows);

    common::live_split(
        app,
        ui,
        "homing-live-right",
        |app, ui| {
            // Leave a gap so the card's bottom border is not flush with the
            // edge of the page.
            common::live_graph_card(app, ui, ui.available_height() - 6.0);
        },
        |app, ui| {
            common::live_coordinates_card(
                ui,
                "MACHINE POSITION",
                &format!("{} / MACHINE", app.snapshot.positions.units),
                &readout,
            );
            homing_context_card(app, ui, &rows);
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
                // Homing is about homed state and machine position; a
                // remaining distance to go is noise here.
                dtg: None,
                active: app
                    .snapshot
                    .panel
                    .jog
                    .active_axis
                    .as_deref()
                    .map(|name| name == row.name)
                    .unwrap_or(false),
                badge: Some((row.status.label().to_string(), status_color(row.status))),
                // No sequence/search/latch note: the DRO is a position readout,
                // and the homing parameters belong in the machine INI rather
                // than repeated under every axis.
                note: None,
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

