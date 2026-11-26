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

    /// Why this CYCLE START press must not start the machine, or `None` when it
    /// may.
    ///
    /// A paused program is resumed by the same key, so the run-only checks (idle
    /// interpreter) cannot apply to it; everything else - machine on, AUTO,
    /// homing, a live panel, and the program interlock - holds either way.
    pub(super) fn cycle_start_refusal(&self) -> Option<String> {
        if self.snapshot.program.paused {
            if self.snapshot.machine.estop {
                return Some("emergency stop is active".into());
            }
            if self.snapshot.task.power != linc_core::model::MachinePower::On {
                return Some("machine is not ON".into());
            }
            if self.snapshot.panel.mode.mode() != Some(PanelMode::Auto) {
                return Some("select AUTO on the panel first".into());
            }
            if self.panel_offline() || self.snapshot.meta.stale {
                return Some("panel state is not current".into());
            }
            return self.program_block_reason();
        }
        if let Some(reason) = self.start_gate(PanelMode::Auto, true) {
            return Some(reason.to_string());
        }
        self.program_block_reason()
    }

    /// Take the panel's CYCLE START key: decide, then either send it to the
    /// controller or say why not.
    ///
    /// Runs once a frame, after the snapshot is polled, on the rising edge of the
    /// key's bit. The press is not forwarded as a HAL edge: it becomes
    /// [`linc_core::command::MachineCommand::CycleStart`], so a press LiNC refuses
    /// never becomes a command at all and the controller answers with its own
    /// words when it turns one down.
    pub(super) fn service_cycle_start(&mut self) {
        let down = self
            .snapshot
            .panel
            .button(linc_core::model::panel_button::CYCLE_START);
        let pressed = down && !self.cycle_start_down;
        if down != self.cycle_start_down {
            // The key is momentary and the HMI reads it from a 20 Hz snapshot, so
            // a press can be seen, missed, or refused - and until now none of the
            // three left a trace outside the screen. One line per edge is all it
            // takes to tell those apart from the machine side.
            log::info!("cycle start key {}", if down { "down" } else { "up" });
        }
        self.cycle_start_down = down;
        if !pressed {
            return;
        }
        if let Some(reason) = self.cycle_start_refusal() {
            log::warn!("cycle start refused: {reason}");
            self.cycle_refusal = Some(reason.clone());
            self.toast(format!("CYCLE START REFUSED — {reason}"), theme::ERROR);
            return;
        }
        log::info!("cycle start sent to the controller");
        self.cycle_refusal = None;
        self.send(linc_core::command::MachineCommand::CycleStart);
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

    /// Whether the program the editor shows may be typed into.
    ///
    /// The loaded program keeps the controller's own rule: the panel in EDIT,
    /// an idle interpreter, PROTECT off and a live link. The prepared program
    /// is not the controller's: it is a file being written for the next cycle,
    /// so it needs the link and PROTECT off and nothing about the mode or the
    /// run. Undo, redo, the page's line commands and the editor's own
    /// interactivity all read this one answer.
    pub fn editing_allowed(&self) -> bool {
        if self.front_is_prepared() {
            self.prepared_editing_allowed()
        } else {
            self.loaded_editing_allowed()
        }
    }

    /// Whether the editor is showing the prepared document. `front` is only
    /// `Prepared` while there is one, but the pair is checked here so a stale
    /// front cannot unlock an empty buffer.
    pub(super) fn front_is_prepared(&self) -> bool {
        self.program.front == DocumentFront::Prepared && self.program.prepared.is_some()
    }

    /// The loaded program's editing gate: the controller's own rule.
    pub(super) fn loaded_editing_allowed(&self) -> bool {
        self.snapshot.program_editing_allowed() && !self.panel_offline()
    }

    /// The prepared program's editing gate: a live panel link with PROTECT off.
    /// The panel mode and the interpreter's state are the loaded program's
    /// business; the link itself is the same test the backend makes, so a stale
    /// or dropped link locks typing rather than letting it run into a refusal.
    fn prepared_editing_allowed(&self) -> bool {
        !self.panel_offline() && !self.program_protected()
    }

    /// The prepared document's write gate: everything typing needs, plus the
    /// panel mode the backend also checks. Typing does not need a known mode;
    /// a write does, because the backend refuses one.
    pub(super) fn prepared_write_allowed(&self) -> bool {
        self.prepared_editing_allowed() && self.snapshot.panel.mode.is_valid()
    }

    pub fn program_dirty(&self) -> bool {
        self.program.loaded().editor.dirty
    }

    /// Why `SAVE` is disabled for the document in front.
    pub(super) fn save_block_reason(&self) -> String {
        if self.front_is_prepared() {
            return self.prepared_save_block_reason();
        }
        self.loaded_save_block_reason()
    }

    fn prepared_save_block_reason(&self) -> String {
        if self.program_protected() {
            return "SAVE needs PROGRAM PROTECT off.".into();
        }
        if !self.snapshot.panel.mode.is_valid() {
            return "SAVE needs a known panel mode.".into();
        }
        "SAVE needs the panel link; it is offline.".into()
    }

    /// Why `SAVE` is disabled for the loaded program, in the order the operator
    /// can act on.
    ///
    /// The panel mode comes first because the controller refuses a save outside
    /// EDIT outright; the rest are the checks the backend repeats on every
    /// write. A dialog that greys the button without naming the reason looks
    /// like a bug rather than a gate.
    pub(super) fn loaded_save_block_reason(&self) -> String {
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

    /// Hand the keyboard back from an in-place field that has closed.
    ///
    /// egui keeps a text edit's focus after the widget stops being drawn, and
    /// `text_edit_focused` then answers true for the rest of the session, which
    /// makes every page key inert — the operator sees a dead HMI. The fields that
    /// open and close in place are listed here with the state that says they are
    /// on screen; when one is closed, the focus goes back to the page.
    pub(super) fn release_closed_edits(&mut self, ctx: &egui::Context) {
        let transient = [
            (crate::state::DRO_FIELD_ID, self.offsets.dro_edit.is_some()),
            ("linc-probe-dro", self.probe.dro_edit.is_some()),
            (
                "linc-probe-param",
                self.probe.param_editing || self.probe.param_focused,
            ),
        ];
        for (name, open) in transient {
            if open {
                continue;
            }
            let id = egui::Id::unique(name);
            if ctx.memory(|memory| memory.focused()) == Some(id) {
                ctx.memory_mut(|memory| memory.surrender_focus(id));
                self.restore_page_keyboard();
            }
        }
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
        // The prepared buffer is asked about first so that a dirty loaded
        // buffer cannot hide it: a single question about the loaded program
        // would end the session and drop the prepared text without one.
        if self
            .program
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.editor.dirty)
        {
            // Quitting drops the prepared buffer with the session, so it is
            // asked about like the loaded one. The question names the prepared
            // file; SAVE cannot apply to it from here.
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::QuitPrepared));
        } else if self.program.loaded().editor.dirty {
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
            // DISCARD on the prepared question drops that buffer, then the quit
            // ladder runs again: a dirty loaded buffer still gets its own
            // question before the session ends.
            PendingAction::QuitPrepared => {
                if self.program.prepared.take().is_some() {
                    self.program.front = DocumentFront::Loaded;
                }
                if self.program.loaded().editor.dirty {
                    self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::Quit));
                } else {
                    self.chrome.quit_requested = true;
                }
            }
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
            PendingAction::PrepareProgram(path) => self.open_prepared(path),
            PendingAction::ClosePrepared => self.close_prepared(),
            PendingAction::HandoffPrepared => self.handoff_prepared(),
        }
    }

    /// A dialog or the manual tool-change prompt owns the keyboard.
    ///
    /// Page-local key handlers must not move, edit or load the control
    /// underneath it: the dialog's button holds focus and takes Enter.
    pub fn overlay_open(&self) -> bool {
        // The safety notice is the first overlay of a fresh installation: it
        // owns the keyboard and the pointer until it is accepted or declined.
        self.safety_gate
            || self.overlay.modal.is_some()
            || self.overlay.messages_dialog
            || self.help.dialog.is_some()
            || self.tools.number.is_some()
            || self.tools.position.is_some()
            || self.manual.spindle_speed.is_some()
            || self.snapshot.tool_change.status == linc_core::model::ToolChangeStatus::ManualPrompt
    }

    /// Accept the safety notice and keep the session running.
    ///
    /// The acceptance is written immediately: a crash or a power cut on the
    /// first frame after the click must not ask the operator to consent again.
    pub fn accept_safety_notice(&mut self) {
        let at = linc_core::model::now_ms() / 1000;
        self.prefs.preferences.accept_safety_notice(at);
        self.prefs.dirty = true;
        self.save_preferences(true);
        self.safety_gate = false;
        log::info!(
            "safety notice version {} accepted",
            linc_core::preferences::SAFETY_NOTICE_VERSION
        );
    }

    /// Decline the safety notice: the session ends without accepting.
    pub fn decline_safety_notice(&mut self) {
        log::info!("safety notice declined; quitting");
        self.chrome.quit_requested = true;
    }

    pub fn escape(&mut self) {
        if self.tools.number.is_some() {
            self.tools.number = None;
            return;
        }
        // The manual screens' speed dialog takes its own Escape in the page, so
        // a refused entry keeps it open. This is the path for any other caller.
        if self.manual.spindle_speed.is_some() {
            self.manual.spindle_speed = None;
            return;
        }
        // The tool-position dialog takes its own Escape in the page, where a
        // refused entry keeps it open. This is the path for any other caller.
        if self.tools.position.is_some() {
            self.tools.position = None;
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
        // The tool table's search is a field too: Esc clears it while it holds
        // text, and only an empty line falls through to the page and then to the
        // program reset. The caret goes back to the field, because egui drops
        // the focus on Escape and clearing is not a dismissal.
        if self.nav.page == Page::Tools && !self.tools.query.is_empty() {
            self.tools.query.clear();
            self.tools.focus_search = true;
            return;
        }
        // The HAL browser's filter and the log search are fields, not pages:
        // Esc clears one while it holds text, and only an empty line falls
        // through to leaving the page. Clearing keeps the caret, so the next
        // keystroke narrows again instead of going nowhere.
        if self.nav.page == Page::Diagnostics
            && matches!(
                self.diagnostics.tab,
                DiagnosticsTab::HalNml | DiagnosticsTab::Logs
            )
            && !self.diagnostics.search().is_empty()
        {
            self.diagnostics.search_mut().clear();
            // egui drops the focus on Escape; the clear is not a dismissal, so
            // ask for the caret back on the next draw.
            self.diagnostics.focus_search = true;
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

    /// Clear the diagnostics view: the log rows and the counters that count
    /// them start again from now.
    ///
    /// This is a view command. It acknowledges nothing and resets nothing on
    /// the controller: a fault the machine still holds stays in the snapshot
    /// and comes back on the next entry the controller reports.
    pub fn clear_log_view(&mut self) {
        self.diagnostics.cleared_before_ms = linc_core::model::now_ms();
        self.diagnostics.log.clear();
        // The list starts at its top again: the rows the reader was looking at
        // are the rows they just cleared.
        self.diagnostics.logs_scroll = Some(0.0);
        self.toast("LOG VIEW CLEARED", theme::MUTED);
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

    /// Unload the program (`Ctrl+U` on the Program page).
    ///
    /// This is the operator's "done cutting" action, and it is also what releases
    /// the external-link interlock: with a program loaded the link is held down
    /// so a panel cycle start cannot run into WiFi jitter, and unloading lets it
    /// come back so the machine is reachable again. Refused while running -
    /// stop first - and while the buffer has unsaved edits, because unloading is
    /// not a way to discard them.
    pub fn unload_program(&mut self) {
        if self.snapshot.task.running {
            self.toast("PROGRAM RUNNING — STOP FIRST", theme::ERROR);
            return;
        }
        if self.snapshot.program.dirty {
            self.toast("UNSAVED CHANGES — SAVE OR REVERT FIRST", theme::WARNING);
            return;
        }
        if self.snapshot.program.path.is_none() {
            self.toast("NO PROGRAM LOADED", theme::MUTED);
            return;
        }
        self.send(linc_core::command::MachineCommand::UnloadProgram);
        // Nothing loaded means nothing to start: drop the interlock now rather
        // than when the controller's answer arrives.
        self.program.machine_check = None;
        self.sync_program_ok();
        // The command is queued, so the outcome is not known here: the controller
        // may refuse or keep the file, and this toast used to promise success
        // either way. The `NET` metric and the program name show what happened.
        self.toast("UNLOAD REQUESTED — WATCH NET", theme::WARNING);
    }

    pub fn program_editing_allowed(&self) -> bool {
        self.editing_allowed()
    }
}
