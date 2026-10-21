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
    // No page title: `live_mode_header` already names the mode, and a heading
    // above it only costs vertical space.
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
            // Leave a gap so the card's bottom border is not flush with the
            // edge of the page.
            common::live_graph_card(app, ui, ui.available_height() - 6.0);
        },
        |app, ui| {
            common::zero_point_card(app, ui);
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
                dtg: Some(position.map(|p| p.dtg).unwrap_or(0.0)),
                name: axis,
                badge: None,
                note: None,
            }
        })
        .collect()
}



fn mode_context_card(app: &mut LincApp, ui: &mut Ui) {
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
        // Jog speed is a percentage of the INI's default linear velocity, so
        // the operator can slow jogging without editing the machine config.
        if matches!(
            mode,
            Some(linc_core::model::PanelMode::Jog)
                | Some(linc_core::model::PanelMode::Inc)
                | Some(linc_core::model::PanelMode::Mpg)
        ) {
            let mut percent = app.jog_speed_percent;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("JOG SPEED")
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.add(
                    egui::Slider::new(&mut percent, 0.0..=100.0)
                        .suffix(" %")
                        .fixed_decimals(0),
                );
            });
            if (percent - app.jog_speed_percent).abs() > f64::EPSILON {
                app.set_jog_speed_percent(percent);
            }
            theme::kv_row(
                ui,
                "FEED",
                format!(
                    "{:.1} {}/s",
                    app.jog_speed_value(),
                    app.snapshot.positions.units
                ),
            );
            // What the controller was actually given. If this is zero or far
            // below FEED, the write never arrived and the jog is slow for a
            // reason outside this page.
            theme::kv_row(
                ui,
                "SENT",
                match app.snapshot.hal.jog_speed {
                    Some(value) => {
                        format!("{value:.1} {}/s", app.snapshot.positions.units)
                    }
                    None => "—".to_string(),
                },
            );
        }
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
                // The steps come from [DISPLAY] INCREMENTS in the machine INI,
                // so the HMI offers exactly the values LinuxCNC was configured
                // with instead of a hardcoded list. The first entry is the
                // continuous "JOG" step.
                theme::kv_row(ui, "STEP", app.jog_increment_label().unwrap_or_else(|| "JOG".into()));
                let increments: Vec<(String, f64)> = app.increments().to_vec();
                let selected = app.jog_increment_index();
                let mut chosen = None;
                ui.horizontal_wrapped(|ui| {
                    for (index, (label, _)) in increments.iter().enumerate() {
                        if theme::chip(ui, label.clone(), index == selected).clicked() {
                            chosen = Some(index);
                        }
                    }
                });
                if let Some(index) = chosen {
                    app.set_jog_increment_index(index);
                }
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
