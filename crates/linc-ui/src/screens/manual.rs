//! Read-only Manual screen shared by JOG, INC and MPG panel modes.
//!
//! Layout base: the same live graph + large coordinate readout + mode context
//! split as the Homing (HOME / PROG.HOME) screen — see
//! [`common::live_split`].

use egui::{RichText, Ui};
use linc_core::model::MachineVariant;

use crate::screens::common::{self, LiveAxisRow};
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let mode = app
        .snapshot
        .panel
        .mode
        .mode()
        .filter(|mode| mode.is_manual());
    let mode_label = mode.map(|mode| mode.label()).unwrap_or("UNKNOWN");
    common::page_header(
        ui,
        "MANUAL",
        "JOG / INC / MPG share this read-only screen. Input stays on the physical panel.",
    );
    common::live_mode_header(
        ui,
        mode_label,
        mode.is_some(),
        "NO ON-SCREEN JOG OR MODE CONTROLS",
    );
    let readout = coordinates_readout(app);
    common::live_split(
        app,
        ui,
        "manual-live-right",
        |app, ui| {
            let height = (ui.available_height() - 20.0).max(320.0);
            common::live_graph_card(app, ui, height);
        },
        |app, ui| {
            common::live_coordinates_card(
                ui,
                "WORK COORDINATES",
                &format!(
                    "{} / {}{}",
                    app.active_wcs_label(),
                    app.snapshot.positions.units,
                    if app.profile.variant == MachineVariant::Lathe {
                        " / RADIUS"
                    } else {
                        ""
                    }
                ),
                &readout,
            );
            mode_context_card(app, ui);
            input_card(app, ui);
        },
    );
}

fn coordinates_readout(app: &LincApp) -> Vec<LiveAxisRow> {
    app.axes()
        .into_iter()
        .map(|axis| {
            let position = app.snapshot.positions.axes.get(&axis);
            LiveAxisRow {
                active: app
                    .snapshot
                    .panel
                    .jog
                    .active_axis
                    .as_deref()
                    .map(|name| name == axis)
                    .unwrap_or(false),
                work: position.map(|p| p.work).unwrap_or(0.0),
                machine: position.map(|p| p.machine).unwrap_or(0.0),
                dtg: position.map(|p| p.dtg).unwrap_or(0.0),
                name: axis,
                badge: None,
                note: None,
            }
        })
        .collect()
}

fn input_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "INPUT");
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
        theme::kv_row(
            ui,
            "PENDANT",
            if app.snapshot.pendant.connected {
                "WHB04B-6 / JOG ONLY"
            } else {
                "NOT CONNECTED"
            },
        );
        theme::kv_row(
            ui,
            "ACTIVE AXIS",
            app.snapshot
                .panel
                .jog
                .active_axis
                .clone()
                .unwrap_or_else(|| "—".into()),
        );
        ui.label(
            RichText::new("MPG encoder counts are not carried by the RS-485 link and are never fabricated.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}


fn mode_context_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "MODE CONTEXT");
        let mode = app.snapshot.panel.mode.mode();
        let active = app
            .snapshot
            .panel
            .jog
            .active_axis
            .clone()
            .unwrap_or_else(|| "—".into());
        theme::kv_row(ui, "ACTIVE AXIS", active);
        match mode {
            Some(linc_core::model::PanelMode::Jog) => {
                theme::kv_row(ui, "MODE", "CONTINUOUS");
                theme::kv_row(
                    ui,
                    "JOG SPEED",
                    format!(
                        "{:.0} {}",
                        app.snapshot.panel.jog.speed, app.snapshot.positions.units
                    ),
                );
                theme::kv_row(
                    ui,
                    "RAPID",
                    if app.snapshot.panel.jog.rapid {
                        "ON"
                    } else {
                        "OFF"
                    },
                );
            }
            Some(linc_core::model::PanelMode::Inc) => {
                theme::kv_row(ui, "MODE", "INCREMENT");
                theme::kv_row(
                    ui,
                    "INCREMENT",
                    app.snapshot
                        .panel
                        .jog
                        .increment
                        .map(|value| format!("{value} {}", app.snapshot.positions.units))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                );
                theme::kv_row(
                    ui,
                    "REMAINING STEP",
                    app.snapshot
                        .panel
                        .jog
                        .remaining_step
                        .map(|value| format!("{value:.3}"))
                        .unwrap_or_else(|| "—".into()),
                );
            }
            Some(linc_core::model::PanelMode::Mpg) => {
                theme::kv_row(ui, "MODE", "HANDWHEEL");
                theme::kv_row(
                    ui,
                    "SCALE",
                    app.snapshot
                        .panel
                        .jog
                        .handwheel_scale
                        .map(|value| format!("{value}"))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                );
                theme::kv_row(ui, "STEP MODE", app.snapshot.panel.jog.mode.clone());
            }
            _ => {
                theme::kv_row(ui, "MODE", "UNKNOWN");
            }
        }
        ui.label(
            RichText::new("When the panel mode or axis selection is unknown after reconnect, it stays UNKNOWN until a fresh state arrives.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}
