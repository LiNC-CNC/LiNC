//! Read-only Manual screen shared by JOG, INC and MPG panel modes.
//!
//! Layout base: the same live graph + large coordinate readout + mode context
//! split as the Homing (HOME / PROG.HOME) screen — see
//! [`common::live_split`].

use egui::{RichText, Ui};
use linc_core::model::{MachineVariant, PanelMode};

use crate::screens::common::{self, LiveAxisRow};
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    handle_zero_keys(app, ui);
    // No mode banner: the panel's mode key and the navigation already say which
    // mode this is, and the row cost vertical space on a 720-line screen. The
    // mode itself still drives the context card below.
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
            common::limits_override_row(app, ui);
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



/// `X`, `Y`, `Z` zero that axis and `A` zeroes all of them.
///
/// The Manual (JOG / INC / MPG) and Homing (HOME / PROG.HOME) pages share one
/// key row, so both mode families must accept these keys — gating on
/// `is_manual()` alone left them advertised on the Homing page and dead there.
/// AUTO, EDIT and MDI keep them inert on purpose.
///
/// Modes whose page advertises the `X`/`Y`/`Z` and `A` touch-off keys. AUTO,
/// EDIT and MDI keep them inert on purpose, and an unknown panel mode fails
/// closed.
fn zero_keys_active(mode: Option<PanelMode>) -> bool {
    matches!(
        mode,
        Some(
            PanelMode::Jog
                | PanelMode::Inc
                | PanelMode::Mpg
                | PanelMode::Home
                | PanelMode::ProgHome
        )
    )
}

/// The same gate as the on-screen touch-off row: writes must be allowed, which
/// covers machine ON, idle, homed, program protect and an E-stopped or offline
/// panel.
pub(crate) fn handle_zero_keys(app: &mut LincApp, ui: &mut Ui) {
    let zeroable = zero_keys_active(app.snapshot.panel.mode.mode());
    // Only a text field keeps these keys for itself. The page holds focus on an
    // anchor widget for arrow navigation, so any focus at all is too broad a
    // test and made the keys dead.
    let text_focused = ui.memory(|memory| {
        memory.has_focus(egui::Id::unique("linc-program-editor"))
            || memory.has_focus(egui::Id::unique("linc-tool-cell"))
    });
    if !zeroable || text_focused {
        return;
    }
    let axes = app.axes();
    let mut zero: Option<Vec<String>> = None;
    let mut toggle_limits = false;
    ui.input_mut(|input| {
        for (key, axis) in [(egui::Key::X, "X"), (egui::Key::Y, "Y"), (egui::Key::Z, "Z")] {
            if input.consume_key(egui::Modifiers::NONE, key) {
                if axes.iter().any(|name| name == axis) {
                    zero = Some(vec![axis.to_string()]);
                }
            }
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::A) {
            zero = Some(axes.clone());
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::O) {
            toggle_limits = true;
        }
    });
    if toggle_limits {
        app.send(linc_core::command::MachineCommand::ToggleLimitsOverride);
    }
    if let Some(axes) = zero {
        common::zero_axes(app, axes);
    }
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
        if matches!(
            mode,
            Some(linc_core::model::PanelMode::Jog)
                | Some(linc_core::model::PanelMode::Inc)
                | Some(linc_core::model::PanelMode::Mpg)
        ) {
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
                        format!("{value:.0} {}/min", app.snapshot.positions.units)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_off_keys_are_live_in_manual_and_homing_modes() {
        for mode in [
            PanelMode::Jog,
            PanelMode::Inc,
            PanelMode::Mpg,
            PanelMode::Home,
            PanelMode::ProgHome,
        ] {
            assert!(zero_keys_active(Some(mode)), "{mode:?}");
        }
        for mode in [PanelMode::Auto, PanelMode::Edit, PanelMode::Mdi] {
            assert!(!zero_keys_active(Some(mode)), "{mode:?}");
        }
        assert!(!zero_keys_active(None));
    }
}
