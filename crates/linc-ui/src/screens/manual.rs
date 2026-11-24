//! Read-only Manual screen shared by JOG, INC and MPG panel modes.
//!
//! Layout base: the same live graph + large coordinate readout split as the
//! Homing (HOME / PROG.HOME) screen — see common::live_split. Under the
//! coordinates sit the INC step row (INC only) and the SERVO LOAD card from
//! MAIN, so the operator reads axis load without leaving the jog page.
//!
//! The page carries the same two faces as MAIN, chosen with the one `main_view`
//! the operator set there: the graph, or the coordinate matrix. The matrix is the
//! DRO, so the column drops its own WORK COORDINATES card; the toolpath moves to
//! the third column beside the matrix instead of going with it, because a jog is
//! read against the part it moves over. The step row and the servo load sit under
//! that toolpath.
//!
//! No mode context card and no limit-override button: the [keys] chord covers
//! the override and the status line shows when it is on.

use egui::Ui;
use linc_core::command::{MachineCommand, SpindleStep};
use linc_core::model::{MachineVariant, PanelMode};

use crate::screens::common::{self, LiveAxisRow};
use crate::screens::machine;
use crate::shell::LincApp;
use crate::state::ShortcutCommand;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // The numeric speed dialog owns the keyboard while it is open; the page keys
    // stand down behind it and the window is drawn last, over the page.
    spindle_speed_dialog_keys(app, ui);
    handle_spindle_keys(app, ui);
    handle_zero_keys(app, ui);
    handle_increment_keys(app, ui);
    // No mode banner: the panel's mode key and the navigation already say which
    // mode this is, and the row cost vertical space on a 720-line screen. The
    // mode itself still drives the context card below.
    let readout = coordinates_readout(app);
    // Read once, before the split: the closure borrows `app` mutably, and the
    // card's height comes from the same row count it draws.
    let servo_rows = app.servo_load_rows();
    // INC fills this column with its step row, so the servo card is for JOG and
    // MPG only. One secondary card under the DRO, not a stack.
    let show_servo = !increment_keys_active(app.snapshot.panel.mode.mode());
    // Read before the split borrows `app`: the same choice MAIN carries, drawn
    // in the matrix face. Its WORK COORDINATES card is the DRO the column
    // would otherwise repeat, so the column keeps only the cards it does not.
    let alternate = machine::alternate_active(app);
    // The matrix face reads three cards across its top row: WORK COORDINATES,
    // MACHINE COORDINATES and the live toolpath beside them. The split is sized
    // from that — the matrix column keeps two cells and the graph column is the
    // third — so the toolpath card is the size of the cell it sits next to and
    // its bottom edge is the top row's.
    let page_height = ui.available_height() - 6.0;
    let matrix_width = (((ui.available_width() - 2.0 * machine::MATRIX_GAP) / 3.0).max(160.0))
        * 2.0
        + machine::MATRIX_GAP;
    let toolpath_height = machine::matrix_cell(matrix_width, page_height).y;
    common::live_split(
        app,
        ui,
        "manual-live-right",
        alternate.then_some(matrix_width),
        |app, ui| {
            if alternate {
                machine::coordinate_matrix(app, ui, ui.available_width(), page_height);
            } else {
                common::live_graph_card(app, ui, page_height);
            }
        },
        |app, ui| {
            if alternate {
                // The toolpath the matrix would otherwise hide. It keeps the
                // graph face's own card, header and all, so the legend and the
                // view keys read the same on both faces.
                common::live_graph_card(app, ui, toolpath_height);
            } else {
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
            }
            // The step is a jog setting, so it sits under the coordinates it
            // moves, not above them: the DRO is what the operator reads first.
            // INC is the mode that uses the `[DISPLAY] INCREMENTS` step; JOG is
            // continuous and MPG picks its step on the handwheel, so for both
            // the row would be a value the machine is not reading.
            //
            // Both faces open this column with a card — the coordinates, or the
            // matrix face's toolpath — so the card under it takes the gap.
            if increment_keys_active(app.snapshot.panel.mode.mode()) {
                ui.add_space(8.0);
                common::increment_row(app, ui);
            }
            // The same card MAIN draws under the toolpath. It shares the
            // column with the coordinates, so both frames take the column's
            // width and their left and right edges line up without a second
            // margin. Absent on a profile with no `[ui] servo_load` list, and
            // absent in INC, where the step row is the card this column carries.
            if show_servo && !servo_rows.is_empty() {
                ui.add_space(8.0);
                common::servo_load_card(
                    &servo_rows,
                    ui,
                    common::servo_load_card_height(servo_rows.len()),
                );
            }
        },
    );
    draw_spindle_speed_dialog(app, ui);
}

/// Modes whose keys trim or set the spindle speed: the manual family only.
/// AUTO, EDIT and MDI keep the chords for themselves, and Homing shares the
/// page family without a spindle control.
fn spindle_keys_active(mode: Option<PanelMode>) -> bool {
    matches!(mode, Some(PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg))
}

/// `Ctrl+PgUp` / `Ctrl+PgDn` step the running spindle's programmed speed and
/// `Ctrl+S` opens the numeric entry.
///
/// The step is the same command Axis's `-`/`+` send. It goes through
/// `check_spindle_step`, the rule both backends apply too, so the key answers on
/// the frame it is pressed instead of waiting for a round trip that would refuse
/// it anyway.
pub(crate) fn handle_spindle_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() || !spindle_keys_active(app.snapshot.panel.mode.mode()) {
        return;
    }
    let text_focused = ui.memory(|memory| {
        memory.has_focus(egui::Id::unique("linc-program-editor"))
            || memory.has_focus(egui::Id::unique("linc-tool-cell"))
    });
    if text_focused || ui.ctx().text_edit_focused() {
        return;
    }
    let up = app.prefs.keymap.spindle_speed_up;
    let down = app.prefs.keymap.spindle_speed_down;
    let entry_key = app.prefs.keymap.spindle_speed;
    let mut step = None;
    let mut open_entry = false;
    ui.input_mut(|input| {
        if input.consume_key(up.modifiers, up.key) {
            step = Some(SpindleStep::Faster);
        }
        if input.consume_key(down.modifiers, down.key) {
            step = Some(SpindleStep::Slower);
        }
        if input.consume_key(entry_key.modifiers, entry_key.key) {
            open_entry = true;
        }
    });
    if open_entry {
        open_spindle_speed_dialog(app);
        return;
    }
    let Some(step) = step else {
        return;
    };
    let ini = &app.profile.machine_ini;
    match linc_core::command::check_spindle_step(
        &app.snapshot,
        step,
        ini.spindle_increment,
        ini.max_spindle_speed,
    ) {
        Ok(()) => app.send(MachineCommand::StepSpindleSpeed { step }),
        Err(error) => app.toast(error.to_string(), theme::WARNING),
    }
}

/// Open the numeric dialog on the speed the controller reports, so Enter alone
/// re-issues it and the first digit replaces it.
fn open_spindle_speed_dialog(app: &mut LincApp) {
    let current = app
        .snapshot
        .spindles
        .first()
        .map(|spindle| spindle.commanded_speed)
        .unwrap_or(0.0);
    app.manual.spindle_speed = Some(crate::state::SpindleSpeedEntry::new(current));
}

/// Close it and give the keyboard back: the field held it, and the panel has no
/// Tab to walk out with.
fn close_spindle_speed_dialog(app: &mut LincApp, ui: &mut Ui) {
    app.manual.spindle_speed = None;
    ui.memory_mut(|memory| memory.stop_text_input());
}

/// Enter sets the speed, Escape cancels, and a refused entry keeps the dialog
/// open with its text and the reason.
fn spindle_speed_dialog_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.manual.spindle_speed.is_none() {
        return;
    }
    let back = app.prefs.keymap.back;
    let activate = app.prefs.keymap.activate;
    let (confirm, cancel) = ui.input_mut(|input| {
        // Escape is the configured back key on every profile; both are taken so
        // a rebound back key does not leave the dialog with no way out.
        let cancel = input.consume_key(back.modifiers, back.key)
            || input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        let confirm = input.consume_key(activate.modifiers, activate.key);
        (confirm, cancel)
    });
    if cancel {
        close_spindle_speed_dialog(app, ui);
        return;
    }
    if !confirm {
        return;
    }
    let Some(mut entry) = app.manual.spindle_speed.take() else {
        return;
    };
    match confirm_spindle_speed(app, &entry.text) {
        Ok(()) => close_spindle_speed_dialog(app, ui),
        Err(reason) => {
            entry.error = Some(reason);
            app.manual.spindle_speed = Some(entry);
        }
    }
}

/// What a confirmed number sends: an absolute speed for a spindle that is
/// already turning. The gate is the one the backends apply, so the dialog cannot
/// accept a number the machine would refuse on a rule it did not check.
fn confirm_spindle_speed(app: &mut LincApp, text: &str) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("type a spindle speed".into());
    }
    let rpm = trimmed
        .parse::<f64>()
        .map_err(|_| format!("{trimmed} is not a spindle speed"))?;
    let ceiling = app.profile.machine_ini.max_spindle_speed;
    linc_core::command::check_spindle_speed(&app.snapshot, rpm, ceiling)
        .map_err(|error| error.to_string())?;
    app.send(MachineCommand::SetSpindleSpeed { rpm });
    Ok(())
}

/// The dialog itself: the speed in force, the step and the ceiling, one line of
/// digits, and the reason the last confirm was refused.
fn draw_spindle_speed_dialog(app: &mut LincApp, ui: &mut Ui) {
    let Some(mut entry) = app.manual.spindle_speed.take() else {
        return;
    };
    let fresh = std::mem::replace(&mut entry.fresh, false);
    let error = entry.error.clone();
    let mut text = entry.text.clone();
    let cancel_key = app.prefs.keymap.back.label();
    let ini = &app.profile.machine_ini;
    let (now, step, ceiling) = app
        .snapshot
        .spindles
        .first()
        .map(|spindle| {
            (
                spindle.commanded_speed.abs(),
                ini.spindle_increment,
                ini.max_spindle_speed,
            )
        })
        .unwrap_or((0.0, ini.spindle_increment, ini.max_spindle_speed));
    let mut summary = format!("NOW {now:.0} RPM  ·  STEP {step:.0}");
    if ceiling > 0.0 {
        summary.push_str(&format!("  ·  MAX {ceiling:.0}"));
    }
    let id = egui::Id::unique("linc-spindle-speed");
    egui::Window::new("SPINDLE SPEED")
        .id(egui::Id::unique("linc-spindle-speed-dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(380.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                egui::RichText::new(&summary)
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(6.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(140.0)
                    .hint_text("rpm"),
            );
            if fresh {
                // No click-to-focus and no Tab on this keyboard: the field
                // takes it on the frame the dialog opens, with the whole value
                // selected, so Enter alone re-issues it.
                field.request_focus();
                if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                    state.store(ui.ctx(), id);
                }
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("ENTER SETS  ·  {cancel_key} CANCELS"))
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if let Some(message) = &error {
                ui.label(
                    egui::RichText::new(message)
                        .size(theme::font_size(10.0))
                        .color(theme::ERROR),
                );
            }
        });
    entry.text = text;
    app.manual.spindle_speed = Some(entry);
}

fn coordinates_readout(app: &LincApp) -> Vec<LiveAxisRow> {
    app.axes()
        .into_iter()
        .map(|axis| {
            let (work, machine, dtg) = common::axis_display(app, &axis).unwrap_or((0.0, 0.0, 0.0));
            LiveAxisRow {
                active: app
                    .snapshot
                    .panel
                    .jog
                    .active_axis
                    .as_deref()
                    .map(|name| name == axis)
                    .unwrap_or(false),
                work,
                machine,
                dtg: Some(dtg),
                name: axis,
                badge: None,
                note: None,
            }
        })
        .collect()
}

/// Modes whose page shows the jog increment row and takes its numbers.
///
/// JOG runs continuously and ignores the step; MPG selects its step on the
/// handwheel, not from the `[DISPLAY] INCREMENTS` list. AUTO, EDIT and MDI are
/// not the manual family at all. An unknown panel mode fails closed like the
/// zero keys.
fn increment_keys_active(mode: Option<PanelMode>) -> bool {
    matches!(mode, Some(PanelMode::Inc))
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
/// Modes whose page advertises the zeroing letters, `X`/`Y`/`Z` and `A`.
///
/// JOG, INC and MPG are deliberately absent: in those modes the letters open the
/// active WCS for editing on MAIN's coordinate matrix, the way they do on the
/// OFFSETS page, and zeroing is left to the on-screen touch-off row. An unknown
/// panel mode fails closed.
fn zero_letter_keys_active(mode: Option<PanelMode>) -> bool {
    matches!(mode, Some(PanelMode::Home | PanelMode::ProgHome))
}

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
    let mut open_recovery = false;
    // A SHORTCUTS row click runs the same zeroing code its key does. The
    // gates above still win: a dialog, a text field or an inactive mode.
    let clicked = app.take_shortcut_click();
    let letters = zero_letter_keys_active(app.snapshot.panel.mode.mode());
    ui.input_mut(|input| {
        // JOG, INC and MPG spend the letters on the WCS edit instead — including
        // `A`, which used to zero every axis from here; see
        // `zero_letter_keys_active`. The recovery key stays.
        if letters {
            for (key, axis) in [
                (egui::Key::X, "X"),
                (egui::Key::Y, "Y"),
                (egui::Key::Z, "Z"),
            ] {
                if input.consume_key(egui::Modifiers::NONE, key)
                    && axes.iter().any(|name| name == axis)
                {
                    zero = Some(vec![axis.to_string()]);
                }
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::A) {
                zero = Some(axes.clone());
            }
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::O) {
            open_recovery = true;
        }
    });
    if open_recovery {
        app.open_limit_recovery();
    }
    if clicked == Some(ShortcutCommand::ManualZeroAll) && zero.is_none() {
        zero = Some(axes);
    }
    if let Some(axes) = zero {
        common::zero_axes(app, axes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers are the selection, and only the mode that uses a
    /// `[DISPLAY] INCREMENTS` step takes them: a continuous JOG has no
    /// increment to pick, MPG selects its step on the handwheel, and the other
    /// pages keep their digits for themselves.
    #[test]
    fn increment_keys_belong_to_the_step_modes() {
        assert!(increment_keys_active(Some(PanelMode::Inc)));
        for mode in [
            PanelMode::Jog,
            PanelMode::Mpg,
            PanelMode::Auto,
            PanelMode::Edit,
            PanelMode::Mdi,
        ] {
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

    /// X/Y/Z and A stop zeroing where the matrix can carry the letters into the
    /// WCS edit. HOME and PROG.HOME have no matrix, so their letters keep the
    /// touch-off.
    #[test]
    fn the_zero_letters_are_home_only() {
        for mode in [PanelMode::Home, PanelMode::ProgHome] {
            assert!(zero_letter_keys_active(Some(mode)), "{mode:?}");
        }
        for mode in [
            PanelMode::Jog,
            PanelMode::Inc,
            PanelMode::Mpg,
            PanelMode::Auto,
            PanelMode::Edit,
            PanelMode::Mdi,
        ] {
            assert!(!zero_letter_keys_active(Some(mode)), "{mode:?}");
        }
        assert!(!zero_letter_keys_active(None));
    }

    /// The spindle keys belong to the manual family only: AUTO, EDIT and MDI
    /// keep them, and Homing shares the page family without a spindle control.
    #[test]
    fn the_spindle_keys_are_manual_modes_only() {
        for mode in [PanelMode::Jog, PanelMode::Inc, PanelMode::Mpg] {
            assert!(spindle_keys_active(Some(mode)), "{mode:?}");
        }
        for mode in [
            PanelMode::Auto,
            PanelMode::Edit,
            PanelMode::Mdi,
            PanelMode::Home,
            PanelMode::ProgHome,
        ] {
            assert!(!spindle_keys_active(Some(mode)), "{mode:?}");
        }
        assert!(!spindle_keys_active(None));
    }

    /// The dialog opens on the speed the controller reports, and on nothing when
    /// the spindle is stopped so the first digit is the whole value.
    #[test]
    fn the_spindle_speed_dialog_seeds_on_the_commanded_speed() {
        let entry = crate::state::SpindleSpeedEntry::new(4185.4);
        assert_eq!(entry.text, "4185");
        assert!(entry.fresh);
        assert!(entry.error.is_none());
        assert_eq!(crate::state::SpindleSpeedEntry::new(0.0).text, "");
    }
}
