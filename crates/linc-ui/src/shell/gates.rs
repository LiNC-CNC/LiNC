//! Safety gates and stop paths: what may start, what may be written, what
//! stops the machine, and the keys that reset.

use super::*;

/// The probe-trip banner's action button. The panel keyboard has no pointer, so
/// the button names the key that does the same thing: Esc resets the trip.
pub const PROBE_TRIP_ACTION: &str = "RESET (ESC)";

/// The probe-trip banner: what the probe hit and what the HMI did about it.
///
/// There are two answers, because there are two cases. Everything except one
/// switches the machine off, and for as long as the trip is latched it stays
/// off. The exception is a trip raised with the program paused *and* nothing
/// moving: the queue was held, nothing was aborted that had to be stopped, and
/// the operator is doing exactly that on purpose to prove the probe input is
/// wired. A paused program that was being jogged is not the exception - the
/// controller reports that jog as a non-probe move while the pause is still set
/// - so it keeps the switched-off wording. The banner says which one happened.
pub fn probe_trip_banner_text(fault: &linc_core::faults::ProbeTripFault) -> String {
    if fault.switches_machine_off() {
        format!(
            "PROBE TRIPPED — {} — MACHINE SWITCHED OFF. MOTION AND MDI BLOCKED. RESET (ESC) TO CLEAR.",
            fault.message
        )
    } else {
        format!(
            "PROBE TRIPPED — {} — PROGRAM PAUSED AND NOTHING MOVING, MACHINE LEFT ON. MOTION AND MDI BLOCKED. RESET (ESC) TO CLEAR.",
            fault.message
        )
    }
}

impl LincApp {
    /// Why a block may not start now, or `None` when it may.
    ///
    /// `mode` is the panel position the path needs; `needs_program` adds the
    /// loaded-program check the AUTO path has and the MDI buffer does not.
    pub(super) fn start_gate(&self, mode: PanelMode, needs_program: bool) -> Option<&'static str> {
        if needs_program && self.snapshot.program.line_count == 0 {
            return Some("no program is loaded");
        }
        if self.snapshot.task.running
            || self.snapshot.task.interp != linc_core::model::InterpState::Idle
        {
            return Some("abort the running program first");
        }
        if self.snapshot.machine.estop {
            return Some("emergency stop is active");
        }
        if self.snapshot.task.power != linc_core::model::MachinePower::On {
            return Some("machine is not ON");
        }
        if self.snapshot.panel.mode.mode() != Some(mode) {
            return Some(match mode {
                PanelMode::Mdi => "panel mode is not MDI",
                _ => "select AUTO on the panel first",
            });
        }
        if self.panel_offline() || self.snapshot.meta.stale {
            return Some("panel state is not current");
        }
        if self.axes().iter().any(|axis| {
            !self
                .snapshot
                .machine
                .homed
                .get(axis)
                .copied()
                .unwrap_or(false)
        }) {
            return Some("home every axis first");
        }
        None
    }

    pub fn program_protected(&self) -> bool {
        self.snapshot.panel.protect_effective()
    }

    pub fn panel_offline(&self) -> bool {
        !self.snapshot.panel.connected || self.snapshot.panel.communication_error
    }

    pub fn invalid_mode(&self) -> bool {
        matches!(
            self.snapshot.panel.mode,
            PanelModeValidity::Invalid(_) | PanelModeValidity::Unknown
        )
    }

    pub fn editing_allowed(&self) -> bool {
        self.snapshot.program_editing_allowed() && !self.panel_offline()
    }

    pub fn program_dirty(&self) -> bool {
        self.program.editor.dirty
    }

    /// Why `SAVE` is disabled, in the order the operator can act on.
    ///
    /// The panel mode comes first because the controller refuses a save outside
    /// EDIT outright; the rest are the checks the backend repeats on every
    /// write. A dialog that greys the button without naming the reason looks
    /// like a bug rather than a gate.
    pub(super) fn save_block_reason(&self) -> String {
        let mode = self.snapshot.panel.mode.mode();
        if mode != Some(PanelMode::Edit) {
            let name = mode.map(PanelMode::label).unwrap_or("UNKNOWN");
            return format!(
                "SAVE needs the panel in EDIT (it is in {name}); DISCARD drops the buffer, \
                 go back to EDIT to save it.",
            );
        }
        if self.program_protected() {
            return "SAVE needs PROGRAM PROTECT off.".into();
        }
        if self.panel_offline() {
            return "SAVE needs the panel link; it is offline.".into();
        }
        "SAVE needs an idle interpreter; the machine is running.".into()
    }

    /// Hand the keyboard back to the page a dialog interrupted.
    ///
    /// egui drops the focus together with the widget that held it, so a closed
    /// dialog left the session with no focus owner: the editor stopped taking
    /// keys and the page keys fell through. The G-code editor is the one field
    /// the shell can focus by name, so it takes the keyboard back on the
    /// Program page; everywhere else the page anchor is the focus stop.
    pub(super) fn restore_page_keyboard(&mut self) {
        if self.nav.page == Page::Program && self.editing_allowed() {
            self.program.focus_editor = true;
            return;
        }
        self.nav.page_focus_requested = true;
    }

    /// End the session because the cabinet was switched off, not because the
    /// operator asked. Deliberately skips the idle guard, the discard prompt and
    /// the confirm dialog: the machine power is going away regardless, and a
    /// modal here would leave the HMI running while LinuxCNC is being shut down.
    /// This is the only path that may quit while a program runs.
    ///
    /// The caller filters the probe-trip recovery toggle: a switch-off while
    /// the trip's machine-off level stands is not a shutdown and never gets
    /// here.
    pub fn request_machine_off(&mut self) {
        log::info!("machine-on input dropped; ending the session");
        self.chrome.quit_requested = true;
    }

    pub fn request_quit(&mut self) {
        if self.snapshot.task.running || self.snapshot.task.motion_active {
            self.toast(
                "cannot quit while a program is running or the machine is moving",
                theme::ERROR,
            );
            return;
        }
        if self.program.editor.dirty {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::Quit));
        } else if self.profile.config.safety.confirm_quit {
            self.overlay.modal = Some(Modal::ConfirmQuit);
        } else {
            self.chrome.quit_requested = true;
        }
    }

    pub fn perform_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::Navigate(page) => self.set_page(page),
            PendingAction::OpenFiles => self.open_files(),
            PendingAction::LoadProgram(path) => self.load_program(path),
            PendingAction::Quit => self.chrome.quit_requested = true,
            PendingAction::NewFile => {
                self.program = ProgramUi {
                    graph_view: if self.profile.variant == MachineVariant::Lathe {
                        linc_view::ViewMode::LatheXz
                    } else {
                        linc_view::ViewMode::Iso3d
                    },
                    ..Default::default()
                };
                self.nav.page = Page::Program;
            }
            PendingAction::OpenRecent(path) => self.load_program(path),
        }
    }

    /// A dialog or the manual tool-change prompt owns the keyboard.
    ///
    /// Page-local key handlers must not move, edit or load the control
    /// underneath it: the dialog's button holds focus and takes Enter.
    pub fn overlay_open(&self) -> bool {
        self.overlay.modal.is_some()
            || self.overlay.messages_dialog
            || self.help.dialog.is_some()
            || self.tools.number.is_some()
            || self.snapshot.tool_change.status == linc_core::model::ToolChangeStatus::ManualPrompt
    }

    pub fn escape(&mut self) {
        if self.tools.number.is_some() {
            self.tools.number = None;
            return;
        }
        if self.overlay.messages_dialog {
            self.overlay.messages_dialog = false;
            return;
        }
        if self.overlay.modal.is_some() {
            self.overlay.modal = None;
            return;
        }
        // The Help viewer owns Escape while it is open: closing it must not
        // fall through to the global program reset, which would stop a running
        // program behind the reader's back.
        if self.help.dialog.is_some() {
            self.help.close_dialog();
            return;
        }
        // A latched probe trip is cleared by the controller reset alone, and the
        // machine is switched off until it is. Esc is that reset here, the same
        // key that resets a running program, so it takes the key before the page
        // branches: the trip banner is on every page, and an operator who opened
        // HELP or DIAGNOSTICS to see what tripped the probe must be able to
        // reset the fault from there instead of navigating away first. A dialog,
        // the messages viewer and the manual tool-change prompt already took
        // Esc above, or held it in the shell's overlay branch.
        if self.snapshot.probe_trip_active() {
            self.clear_probe_trip();
            return;
        }
        // The top-level Help page is a view, not the machine: Esc leaves it for
        // the page it came from. It must not fall through to the program reset;
        // the next Esc, on that page, is the one that stops a running program.
        if self.nav.page == Page::Help {
            self.set_page(self.nav.help_return_page);
            return;
        }
        // The Diagnostics page is a view too: Esc leaves it for the page it came
        // from, so an operator who opened it to look at a log does not stop the
        // program that is cutting behind it. The next Esc, on that page, is the
        // one that resets.
        if self.nav.page == Page::Diagnostics {
            self.set_page(self.nav.diagnostics_return_page);
            return;
        }
        // K4's program reset on the keyboard: Esc stops the running program
        // wherever the operator is, before it means "go back".
        if self.program_active() {
            self.abort_program();
            return;
        }
        // The pane is keyed to the panel mode, so its Esc is too. It clears the
        // uncommitted input only, and only once the global meanings above (a
        // dialog, a probe trip, a running program) have had their turn.
        if self.mdi_pane_visible() && !self.mdi.input.is_empty() {
            self.mdi.clear_input();
            return;
        }
        match self.nav.page {
            Page::Files => self.set_page(self.nav.files_return_page),
            Page::Probe => self.set_page(Page::Offsets),
            Page::Program => {
                if self.program.focus_editor {
                    self.program.focus_editor = false;
                } else {
                    self.request_page(Page::Machine);
                }
            }
            Page::Diagnostics => self.set_page(Page::Machine),
            _ => self.set_page(Page::Machine),
        }
    }

    /// Clear a latched probe trip with the controller reset.
    ///
    /// `EMC_TASK_ABORT` is what clears the motion error LinuxCNC set when the
    /// probe tripped. The local fault clears only when that send succeeds; no
    /// timeout, poll or reconnect clears it.
    pub fn clear_probe_trip(&mut self) {
        if self.panel_offline() {
            self.toast("PANEL OFFLINE — CANNOT RESET", theme::ERROR);
            return;
        }
        self.send(linc_core::command::MachineCommand::AbortProgram);
        if self.snapshot.probe_trip_active() {
            self.toast("PROBE TRIP RESET NOT CONFIRMED", theme::ERROR);
        } else {
            self.toast("PROBE TRIP RESET", theme::WARNING);
        }
    }

    /// Stop and reset the running program, the same effect as the panel's K4.
    pub fn abort_program(&mut self) {
        if self.panel_offline() {
            self.toast("PANEL OFFLINE — CANNOT RESET", theme::ERROR);
            return;
        }
        self.send(linc_core::command::MachineCommand::AbortProgram);
        self.toast("PROGRAM RESET", theme::WARNING);
    }

    pub fn program_editing_allowed(&self) -> bool {
        self.editing_allowed()
    }
}
