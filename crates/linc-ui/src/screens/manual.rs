//! Read-only Manual screen shared by JOG, INC and MPG panel modes.
//!
//! Layout base: the same live graph + large coordinate readout split as the
//! Homing (HOME / PROG.HOME) screen — see common::live_split.
//!
//! No mode context card and no limit-override button: the [keys] chord covers
//! the override and the status line shows when it is on.

use egui::Ui;
use linc_core::model::{MachineVariant, PanelMode};

use crate::screens::common::{self, LiveAxisRow};
use crate::shell::LincApp;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    handle_zero_keys(app, ui);
    handle_increment_keys(app, ui);
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
            // The step is a jog setting, so it sits under the coordinates it
            // moves, not above them: the DRO is what the operator reads first.
            // INC and MPG are the modes that use a step; JOG is continuous, so
            // the row would be a value the machine is not reading.
            if increment_keys_active(app.snapshot.panel.mode.mode()) {
                ui.add_space(8.0);
                common::increment_row(app, ui);
            }
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

/// Modes whose page shows the jog increment row and takes its numbers.
///
/// JOG runs continuously and ignores the step; AUTO, EDIT and MDI are not the
/// manual family at all. An unknown panel mode fails closed like the zero keys.
fn increment_keys_active(mode: Option<PanelMode>) -> bool {
    matches!(mode, Some(PanelMode::Inc | PanelMode::Mpg))
}

/// `1`..`9` pick the `[DISPLAY] INCREMENTS` step of that number.
///
/// The panel keyboard has one keypad and no spare chords, so the number is the
/// selection: the same digits the row paints next to each step. Overflow keys
/// are ignored, so a profile with four steps leaves 5-9 inert.
pub(crate) fn handle_increment_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() || !increment_keys_active(app.snapshot.panel.mode.mode()) {
        return;
    }
    let text_focused = ui.memory(|memory| {
        memory.has_focus(egui::Id::unique("linc-program-editor"))
            || memory.has_focus(egui::Id::unique("linc-tool-cell"))
    });
    if text_focused || ui.ctx().text_edit_focused() {
        return;
    }
    let steps = app.increments().len();
    let mut pick = None;
    ui.input_mut(|input| {
        for (digit, key) in [
            (1, egui::Key::Num1),
            (2, egui::Key::Num2),
            (3, egui::Key::Num3),
            (4, egui::Key::Num4),
            (5, egui::Key::Num5),
            (6, egui::Key::Num6),
            (7, egui::Key::Num7),
            (8, egui::Key::Num8),
            (9, egui::Key::Num9),
        ] {
            if input.consume_key(egui::Modifiers::NONE, key) && digit <= steps {
                pick = Some(digit - 1);
            }
        }
    });
    if let Some(index) = pick {
        app.set_jog_increment_index(index);
    }
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
    // A dialog or the manual tool-change prompt owns the keyboard. Without
    // this, `A` could zero every axis behind the prompt.
    if app.overlay_open() {
        return;
    }
    let zeroable = zero_keys_active(app.snapshot.panel.mode.mode());
    // Only a text field keeps these keys for itself. The page holds focus on an
    // anchor widget for arrow navigation, so any focus at all is too broad a
    // test and made the keys dead.
    let text_focused = ui.memory(|memory| {
        memory.has_focus(egui::Id::unique("linc-program-editor"))
            || memory.has_focus(egui::Id::unique("linc-tool-cell"))
    });
    if !zeroable || text_focused || ui.ctx().text_edit_focused() {
        return;
    }
    let axes = app.axes();
    let mut zero: Option<Vec<String>> = None;
    let mut toggle_limits = false;
    ui.input_mut(|input| {
        for (key, axis) in [
            (egui::Key::X, "X"),
            (egui::Key::Y, "Y"),
            (egui::Key::Z, "Z"),
        ] {
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


#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers are the selection, and only the modes that use a step take
    /// them: a continuous JOG has no increment to pick, and the other pages
    /// keep their digits for themselves.
    #[test]
    fn increment_keys_belong_to_the_step_modes() {
        for mode in [PanelMode::Inc, PanelMode::Mpg] {
            assert!(increment_keys_active(Some(mode)), "{mode:?}");
        }
        for mode in [PanelMode::Jog, PanelMode::Auto, PanelMode::Edit, PanelMode::Mdi] {
            assert!(!increment_keys_active(Some(mode)), "{mode:?}");
        }
        assert!(!increment_keys_active(None));
    }

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
