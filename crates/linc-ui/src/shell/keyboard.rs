//! The keyboard model: `FocusRegion`, and the one handler that owns every
//! key the shell consumes.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusRegion {
    Nav,
    Page,
    Actions,
}

/// Lines a PageUp/PageDown moves the selection in a read-only G-code view.
pub(super) const LINE_PAGE: isize = 10;

/// The clipboard shortcut `egui-winit` converts into `Event::Cut`. It is the
/// platform convention rather than a profile binding, and it is what the two
/// page-local consumers bind: the Probe page's X data reset and the Files
/// page's delete (`[keys].cut` defaults to the same chord).
pub(super) const CUT_SHORTCUT: Binding = Binding::new(egui::Key::X, egui::Modifiers::CTRL);

impl LincApp {
    pub fn handle_keyboard(&mut self, ui: &mut egui::Ui) {
        self.hand_cut_to_page(ui);
        // A modal messages dialog owns the keyboard: Enter/Escape close it and
        // the page keys scroll it. Nothing behind it may react to a key.
        if self.overlay.messages_dialog {
            let page = ui.ctx().content_rect().height() * 0.7;
            let mut close = false;
            let mut scroll = 0.0;
            ui.input_mut(|input| {
                if input.consume_key(self.prefs.keymap.back.modifiers, self.prefs.keymap.back.key)
                    || input.consume_key(
                        self.prefs.keymap.activate.modifiers,
                        self.prefs.keymap.activate.key,
                    )
                {
                    close = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                    scroll += page;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                    scroll -= page;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    scroll += 40.0;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    scroll -= 40.0;
                }
            });
            self.overlay.messages_scroll = scroll;
            if close {
                self.overlay.messages_dialog = false;
            }
            return;
        }
        // Every other dialog owns the keyboard too. Its focused buttons and
        // fields handle Enter, arrows and typing during `draw`; the shell only
        // keeps Escape for a dismissible dialog. Without this, F5-F8, Ctrl+D/H,
        // Ctrl+Q and the page keys could act on the page behind the dialog.
        if self.overlay_open() {
            // The tool-number and tool-position dialogs take their own Enter
            // and Escape in the page, before the table draws: a refused entry
            // must keep the dialog open and report why, which a consumed Escape
            // could not do. Their fields have focus, so the shell has nothing
            // else to give them.
            if (self.tools.number.is_some() || self.tools.position.is_some())
                && self.overlay.modal.is_none()
                && self.help.dialog.is_none()
            {
                return;
            }
            let escape = ui.input_mut(|input| {
                input.consume_key(self.prefs.keymap.back.modifiers, self.prefs.keymap.back.key)
            });
            if escape && (self.overlay.modal.is_some() || self.help.dialog.is_some()) {
                self.escape();
            }
            return;
        }
        // A text field keeps Home, End and the page keys while it holds the
        // keyboard; they are cursor movement there and focus navigation here.
        // Add every text field that can hold focus to this list.
        let text_focused = ui.memory(|memory| {
            memory.has_focus(egui::Id::unique("linc-program-editor"))
                || memory.has_focus(egui::Id::unique("linc-tool-cell"))
                || memory.has_focus(egui::Id::unique("linc-tool-position"))
                || memory.has_focus(egui::Id::unique("linc-offset-cell"))
                || memory.has_focus(egui::Id::unique("linc-offsets-dro"))
                || memory.has_focus(egui::Id::unique("linc-file-rename"))
                || memory.has_focus(egui::Id::unique("linc-probe-param"))
                || memory.has_focus(egui::Id::unique("linc-probe-dro"))
                || memory.has_focus(egui::Id::unique("linc-help-search"))
                || memory.has_focus(egui::Id::unique("linc-goto-line-input"))
                || memory.has_focus(egui::Id::unique("linc-mdi-input"))
                || memory.has_focus(egui::Id::unique("linc-diagnostics-search"))
        });
        let mut nav_page = None;
        let mut mode_nav = false;
        let mut escape = false;
        let mut quit = false;
        let mut request_files = false;
        let mut save = false;
        let mut cycle_view = false;
        let mut graph_fit = false;
        let mut graph_clear = false;
        let mut remaining = false;
        let mut machine_following = false;
        let mut run_from_line = false;
        let mut mdi_issue = false;
        let mut help_search = false;
        let mut page_up = false;
        let mut page_down = false;
        let mut probe_enter = false;
        let mut probe_next = false;
        let mut probe_exit = false;
        let mut home = false;
        let mut end = false;
        let mut focus_left = false;
        let mut focus_right = false;
        let mut undo = false;
        let mut redo = false;
        let mut find = false;
        let mut refresh = false;
        let mut duplicate_line = false;
        let mut delete_line = false;
        let mut move_line_up = false;
        let mut move_line_down = false;
        let mut line_step = 0isize;
        let mut goto_line = false;
        let mut cycle_toolpath_view = false;
        let mut toggle_bounds = false;
        let mut toggle_lathe_mirror = false;
        let mut toggle_lathe_diameter = false;
        let mut view_pan = egui::Vec2::ZERO;
        let mut view_zoom = 1.0f32;
        let mut open_messages = false;
        let mut mdi_up = false;
        let mut mdi_down = false;
        let mut mdi_line_up = false;
        let mut mdi_line_down = false;
        let mut mdi_delete = false;
        let mut mdi_clear = false;
        let mut mdi_run = false;
        let mut mdi_goto = false;
        let mut mdi_save = false;
        let text_editing = ui.ctx().text_edit_focused();
        let toolpath_hovered = linc_view::toolpath_hovered(ui.ctx());
        let mdi_pane = self.mdi_pane_visible();

        ui.input_mut(|input| {
            // The MDI pane replaces the MAIN page's program card while the
            // panel sits in MDI, so its keys are guarded by the mode and the
            // page rather than by a Page variant. They come first: plain Enter
            // is the shell's activate key, the arrows navigate, and Ctrl+R is
            // run-from-line on MAIN, so the pane would lose every one of them.
            if mdi_pane {
                // The buffer line moves go first: they share the arrow keys
                // with recall, and a chord that arrived here must not be left
                // for the plain arrow to take. `consume_key` matches the
                // modifiers exactly, so the order is belt and braces.
                if input.consume_key(
                    self.prefs.keymap.mdi_line_up.modifiers,
                    self.prefs.keymap.mdi_line_up.key,
                ) {
                    mdi_line_up = true;
                }
                if input.consume_key(
                    self.prefs.keymap.mdi_line_down.modifiers,
                    self.prefs.keymap.mdi_line_down.key,
                ) {
                    mdi_line_down = true;
                }
                if input.consume_key(
                    self.prefs.keymap.mdi_history_up.modifiers,
                    self.prefs.keymap.mdi_history_up.key,
                ) {
                    mdi_up = true;
                }
                if input.consume_key(
                    self.prefs.keymap.mdi_history_down.modifiers,
                    self.prefs.keymap.mdi_history_down.key,
                ) {
                    mdi_down = true;
                }
                if input.consume_key(
                    self.prefs.keymap.mdi_buffer_delete.modifiers,
                    self.prefs.keymap.mdi_buffer_delete.key,
                ) {
                    mdi_delete = true;
                }
                if input.consume_key(
                    self.prefs.keymap.program_run_from_line.modifiers,
                    self.prefs.keymap.program_run_from_line.key,
                ) {
                    mdi_run = true;
                }
                if input.consume_key(
                    self.prefs.keymap.goto_line.modifiers,
                    self.prefs.keymap.goto_line.key,
                ) {
                    mdi_goto = true;
                }
                if input.consume_key(self.prefs.keymap.save.modifiers, self.prefs.keymap.save.key) {
                    mdi_save = true;
                }
                // `Esc` keeps its global meanings (a dialog, a latched probe
                // trip, a running program reset). The pane only takes the
                // clear chord itself when the profile moved it off the back
                // key; otherwise that path clears the input after the global
                // checks have declined the key.
                if self.prefs.keymap.mdi_clear != self.prefs.keymap.back
                    && input.consume_key(
                        self.prefs.keymap.mdi_clear.modifiers,
                        self.prefs.keymap.mdi_clear.key,
                    )
                {
                    mdi_clear = true;
                }
                // Enter issues the block. The binding is configurable, so the
                // pane test comes first and the mode flag second: consume_key
                // swallows the key whatever the rest of the condition says.
                if self.profile.config.mdi.execute_on_enter
                    && input.consume_key(
                        self.prefs.keymap.mdi_issue.modifiers,
                        self.prefs.keymap.mdi_issue.key,
                    )
                {
                    mdi_issue = true;
                }
            }
            if input.consume_key(
                self.prefs.keymap.machine.modifiers,
                self.prefs.keymap.machine.key,
            ) {
                mode_nav = true;
            }
            if input.consume_key(
                self.prefs.keymap.program.modifiers,
                self.prefs.keymap.program.key,
            ) {
                // The key is labelled FILE: it opens the program, and opens the
                // file browser when the program is already in front. CODE/SPLIT
                // is a separate command on the square key.
                if self.nav.page == Page::Program {
                    request_files = true;
                } else {
                    nav_page = Some(Page::Program);
                }
            }
            // Page-local: the square is CODE/SPLIT where there is a program
            // view and the following-error traces on MAIN. Shortcuts that mean
            // nothing on the page in front of the operator are not consumed, so
            // they cannot shadow another page's command.
            if matches!(self.nav.page, Page::Program | Page::Machine)
                && input.consume_key(
                    self.prefs.keymap.program_cycle_view.modifiers,
                    self.prefs.keymap.program_cycle_view.key,
                )
            {
                if self.nav.page == Page::Program {
                    cycle_view = true;
                } else {
                    machine_following = true;
                }
            }
            // Page-local to the pages that draw a toolpath.
            if matches!(
                self.nav.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing
            ) {
                if input.consume_key(
                    self.prefs.keymap.graph_fit.modifiers,
                    self.prefs.keymap.graph_fit.key,
                ) {
                    graph_fit = true;
                }
                if input.consume_key(
                    self.prefs.keymap.graph_clear.modifiers,
                    self.prefs.keymap.graph_clear.key,
                ) {
                    graph_clear = true;
                }
                if input.consume_key(
                    self.prefs.keymap.program_run_from_line.modifiers,
                    self.prefs.keymap.program_run_from_line.key,
                ) {
                    run_from_line = true;
                }
            }
            // Hover-scoped like the wheel: the view takes the pan and zoom
            // chords only where it is the thing under the pointer. Ctrl+Left/
            // Right stays region focus everywhere else, and the G-code editor
            // and MDI buffer keep Ctrl+Up/Down for their line moves.
            //
            // The two groups are gated separately. Ctrl+Plus/Minus collide with
            // nothing a text field uses, so the zoom works while the editor
            // holds the keyboard — that is where the operator is when they want
            // a closer look at the path. The pan chords can only be the view's
            // where no text field needs them: Ctrl+Left/Right is the editor's
            // word move and Ctrl+Up/Down its line move (and the MDI buffer's
            // block move). Clicking the toolpath itself hands it the keyboard,
            // so the pan is one click away instead of unreachable.
            let view_page = matches!(
                self.nav.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing | Page::Offsets
            );
            // Zoom is page-scoped, not hover-scoped: it collides with nothing a
            // text field uses, and requiring the pointer over the card made the
            // chord work only when the trackball happened to be there - the
            // reported sometimes. Ctrl+0 was always page-scoped, so the two agree
            // now.
            if view_page {
                let k = &self.prefs.keymap;
                // Ctrl+Plus is the numpad key; on a normal keyboard the plus is
                // Shift+equals, which egui reports as Equals. Accept both, or the
                // chord depends on which key the operator reaches for.
                let zoom_in = input.consume_key(k.view_zoom_in.modifiers, k.view_zoom_in.key)
                    || (k.view_zoom_in.key == egui::Key::Plus
                        && (input.consume_key(k.view_zoom_in.modifiers, egui::Key::Equals)
                            || input.consume_key(
                                k.view_zoom_in.modifiers | egui::Modifiers::SHIFT,
                                egui::Key::Equals,
                            )));
                if zoom_in {
                    view_zoom *= VIEW_ZOOM_STEP;
                }
                // egui reports the numpad key and the main-row key as Minus alike,
                // so unlike the plus there is nothing to alias here.
                let zoom_out = input.consume_key(k.view_zoom_out.modifiers, k.view_zoom_out.key);
                if zoom_out {
                    view_zoom /= VIEW_ZOOM_STEP;
                }
                // Page-scoped like the zoom: Ctrl+M is no text field's chord and
                // no navigation key, so it works wherever a toolpath is drawn.
                if input.consume_key(k.remaining.modifiers, k.remaining.key) {
                    remaining = true;
                }
            }
            let view_hovered = view_page && toolpath_hovered;
            let view_pan_keys = view_hovered && !text_focused;
            if view_pan_keys {
                let k = &self.prefs.keymap;
                if input.consume_key(k.view_pan_left.modifiers, k.view_pan_left.key) {
                    view_pan.x -= VIEW_PAN_STEP;
                }
                if input.consume_key(k.view_pan_right.modifiers, k.view_pan_right.key) {
                    view_pan.x += VIEW_PAN_STEP;
                }
                if input.consume_key(k.view_pan_up.modifiers, k.view_pan_up.key) {
                    view_pan.y -= VIEW_PAN_STEP;
                }
                if input.consume_key(k.view_pan_down.modifiers, k.view_pan_down.key) {
                    view_pan.y += VIEW_PAN_STEP;
                }
            }
            if input.consume_key(
                self.prefs.keymap.tools.modifiers,
                self.prefs.keymap.tools.key,
            ) {
                nav_page = Some(Page::Tools);
            }
            // The jog screens carry no button for the limit override, so the
            // chord is the only way to free a joint parked on a switch.
            if input.consume_key(
                self.prefs.keymap.limits_override.modifiers,
                self.prefs.keymap.limits_override.key,
            ) {
                self.send(linc_core::command::MachineCommand::ToggleLimitsOverride);
            }
            if input.consume_key(
                self.prefs.keymap.offsets.modifiers,
                self.prefs.keymap.offsets.key,
            ) {
                if self.nav.page == Page::Probe {
                    if linc_core::probe::ProbeCategory::ALL.last().copied()
                        == Some(self.probe.category)
                    {
                        probe_exit = true;
                    } else {
                        probe_next = true;
                    }
                } else if self.nav.page == Page::Offsets
                    && self.snapshot.probe.enabled
                    && self.profile.variant == linc_core::model::MachineVariant::Mill
                {
                    probe_enter = true;
                } else {
                    nav_page = Some(Page::Offsets);
                }
            }
            // Ctrl+D duplicates a line in the editable program; that page-local
            // command shadows the global Diagnostics key there.
            let editing_program = self.nav.page == Page::Program && self.editing_allowed();
            if !editing_program
                && input.consume_key(
                    self.prefs.keymap.diagnostics.modifiers,
                    self.prefs.keymap.diagnostics.key,
                )
            {
                nav_page = Some(Page::Diagnostics);
            }
            if input.consume_key(self.prefs.keymap.help.modifiers, self.prefs.keymap.help.key) {
                nav_page = Some(Page::Help);
            }
            if !text_editing
                && !self.overlay_open()
                && input.consume_key(
                    self.prefs.keymap.messages.modifiers,
                    self.prefs.keymap.messages.key,
                )
            {
                open_messages = true;
            }
            if input.consume_key(self.prefs.keymap.quit.modifiers, self.prefs.keymap.quit.key) {
                quit = true;
            }
            // Not on Program: FILE is the way to the file browser there, and a
            // second key for the same destination only crowds the hint row.
            if self.nav.page != Page::Program
                && input.consume_key(self.prefs.keymap.open.modifiers, self.prefs.keymap.open.key)
            {
                request_files = true;
            }
            // Page-local commands: test the page before `consume_key`, or the
            // key is swallowed on every other page (and in every text field).
            // This is the same rule the MDI binding below documents.
            if self.nav.page == Page::Program
                && input.consume_key(self.prefs.keymap.save.modifiers, self.prefs.keymap.save.key)
            {
                save = true;
            }
            if self.nav.page == Page::Help
                && input.consume_key(self.prefs.keymap.find.modifiers, self.prefs.keymap.find.key)
            {
                find = true;
            }
            if editing_program {
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::D) {
                    duplicate_line = true;
                }
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::K) {
                    delete_line = true;
                }
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowUp) {
                    move_line_up = true;
                }
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowDown) {
                    move_line_down = true;
                }
            }
            // Files owns Ctrl+R (refresh). Program and the toolpath pages bind
            // it to RUN FROM LINE above; Probe binds it to RESET DATA, so those
            // pages must leave the key alone rather than swallow it.
            if self.nav.page == Page::Files
                && input.consume_key(
                    self.prefs.keymap.refresh.modifiers,
                    self.prefs.keymap.refresh.key,
                )
            {
                refresh = true;
            }
            // Undo/redo belong to the focused G-code editor. Every other text
            // field keeps the chords for its own undo stack.
            if self.nav.page == Page::Program && self.program.focus_editor {
                if input.consume_key(self.prefs.keymap.undo.modifiers, self.prefs.keymap.undo.key) {
                    undo = true;
                }
                if input.consume_key(self.prefs.keymap.redo.modifiers, self.prefs.keymap.redo.key) {
                    redo = true;
                }
            }
            // An edit or a page popup owns Escape: it cancels the edit (or
            // closes the selector) rather than leaving the page.
            let editing_in_place = (self.nav.page == Page::Tools
                && (self.tools.editing || self.tools.kind_popup.is_some()))
                || (self.nav.page == Page::Offsets
                    && (self.offsets.editing || self.offsets.dro_edit.is_some()))
                || (self.nav.page == Page::Files && self.files.rename.is_some())
                || (self.nav.page == Page::Probe
                    && (self.probe.param_editing
                        || self.probe.param_focused
                        || self.probe.dro_edit.is_some()))
                || self.help.dialog.is_some();
            if !editing_in_place
                && input.consume_key(self.prefs.keymap.back.modifiers, self.prefs.keymap.back.key)
            {
                escape = true;
            }
            // A read-only G-code pane has no text cursor, so the arrows and the
            // page keys move the selected line there; an editable pane leaves
            // them to the editor.
            if self.nav.page == Page::Program
                && !self.editing_allowed()
                && self.nav.focus_region == FocusRegion::Page
            {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    line_step -= 1;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    line_step += 1;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                    line_step -= LINE_PAGE;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                    line_step += LINE_PAGE;
                }
            }
            if matches!(self.nav.page, Page::Program | Page::Machine)
                && input.consume_key(
                    self.prefs.keymap.goto_line.modifiers,
                    self.prefs.keymap.goto_line.key,
                )
            {
                goto_line = true;
            }
            // Every page with a toolpath: ISO -> X -> Y -> Z. F10 on its own
            // already toggles SPLIT/CODE on the Program page.
            if matches!(
                self.nav.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing
            ) && input.consume_key(
                self.prefs.keymap.view_cycle.modifiers,
                self.prefs.keymap.view_cycle.key,
            ) {
                cycle_toolpath_view = true;
            }
            if matches!(
                self.nav.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing | Page::Offsets
            ) && input.consume_key(
                self.prefs.keymap.toggle_bounds.modifiers,
                self.prefs.keymap.toggle_bounds.key,
            ) {
                toggle_bounds = true;
            }
            // The two lathe display toggles are global rather than page-local:
            // the operator flips them while looking at the DRO, and the same
            // keys work on every page that shows the turning view or its
            // readout. A mill has neither, so the profile's variant guards
            // them and the chords stay free for the mill keys.
            if self.profile.variant == MachineVariant::Lathe
                && input.consume_key(
                    self.prefs.keymap.toggle_lathe_mirror.modifiers,
                    self.prefs.keymap.toggle_lathe_mirror.key,
                )
            {
                toggle_lathe_mirror = true;
            }
            if self.profile.variant == MachineVariant::Lathe
                && input.consume_key(
                    self.prefs.keymap.toggle_lathe_diameter.modifiers,
                    self.prefs.keymap.toggle_lathe_diameter.key,
                )
            {
                toggle_lathe_diameter = true;
            }
            // The tool table pages its own cursor with these keys, so they must
            // reach the page instead of being consumed for tab navigation.
            let page_keys_belong_to_page = matches!(self.nav.page, Page::Tools | Page::Files)
                || self.help.dialog.is_some()
                || (self.nav.page == Page::Probe
                    && (self.probe.param_editing || self.probe.dro_edit.is_some()));
            if !text_focused
                && !page_keys_belong_to_page
                && input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp)
            {
                page_up = true;
            }
            if !text_focused
                && !page_keys_belong_to_page
                && input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown)
            {
                page_down = true;
            }
            // Home and End are the editor's line commands while a text field
            // has focus; taking them for focus navigation made them do nothing
            // in the G-code editor.
            if !text_focused && input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                home = true;
            }
            if !text_focused && input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                end = true;
            }
            // A hovered toolpath has taken these for panning above: the view
            // under the pointer wins over moving the focus ring.
            if !text_focused
                && !view_pan_keys
                && input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft)
            {
                focus_left = true;
            }
            if !text_focused
                && !view_pan_keys
                && input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight)
            {
                focus_right = true;
            }
            // The Help search owns `/`; on other pages the character must reach
            // the page or text field instead of being consumed for nothing.
            if self.nav.page == Page::Help
                && !text_focused
                && input.consume_key(
                    self.prefs.keymap.help_search.modifiers,
                    self.prefs.keymap.help_search.key,
                )
            {
                help_search = true;
            }
        });

        if quit {
            self.request_quit();
        }
        if request_files {
            if self.snapshot.task.running {
                self.toast("cannot open files while running", theme::ERROR);
            } else {
                self.request_files();
            }
        }
        if cycle_toolpath_view {
            self.cycle_toolpath_view();
        }
        if toggle_bounds {
            self.cycle_bounds();
        }
        if toggle_lathe_mirror {
            self.cycle_lathe_mirror();
        }
        if toggle_lathe_diameter {
            self.cycle_lathe_diameter();
        }
        if goto_line && matches!(self.nav.page, Page::Program | Page::Machine) {
            self.overlay.modal = Some(crate::state::Modal::GoToLine {
                buffer: String::new(),
                mdi: false,
            });
        }
        if line_step != 0 && self.nav.page == Page::Program {
            self.step_selected_line(line_step);
        }
        if save && self.nav.page == Page::Program {
            self.save_program();
        }
        if refresh && self.nav.page == Page::Files {
            self.files.refresh();
        }
        if self.nav.page == Page::Program {
            let selected = self.program.selected_line;
            if duplicate_line && self.editing_allowed() {
                if let Some(line) = self.program.editor.duplicate_line(selected) {
                    self.program.selected_line = line;
                    self.program.code_scroll_to_line = Some(line);
                    self.mark_editor_changed();
                }
            }
            if delete_line && self.editing_allowed() {
                if let Some(line) = self.program.editor.delete_line(selected) {
                    self.program.selected_line = line;
                    self.program.code_scroll_to_line = Some(line);
                    self.mark_editor_changed();
                }
            }
            if move_line_up && self.editing_allowed() {
                if let Some(line) = self.program.editor.move_line_up(selected) {
                    self.program.selected_line = line;
                    self.program.code_scroll_to_line = Some(line);
                    self.mark_editor_changed();
                }
            }
            if move_line_down && self.editing_allowed() {
                if let Some(line) = self.program.editor.move_line_down(selected) {
                    self.program.selected_line = line;
                    self.program.code_scroll_to_line = Some(line);
                    self.mark_editor_changed();
                }
            }
        }
        if undo
            && self.nav.page == Page::Program
            && self.program.focus_editor
            && self.program.editor.undo()
        {
            self.mark_editor_changed();
        }
        if redo
            && self.nav.page == Page::Program
            && self.program.focus_editor
            && self.program.editor.redo()
        {
            self.mark_editor_changed();
        }
        if run_from_line {
            self.request_run_from_line();
        }
        if graph_fit {
            self.fit_graph();
        }
        if view_pan != egui::Vec2::ZERO || view_zoom != 1.0 {
            self.pan_view(view_pan, view_zoom);
        }
        if graph_clear {
            self.clear_trail();
        }
        if remaining {
            self.cycle_remaining();
        }
        if machine_following {
            self.following.active = !self.following.active;
        }
        if cycle_view {
            self.cycle_program_view();
            let view = self.program.view;
            self.remember_program_view(view);
        }
        if probe_exit {
            nav_page = Some(Page::Offsets);
        }
        if mode_nav {
            self.request_mode_page();
        }
        if let Some(page) = nav_page {
            self.request_page(page);
        }
        if (help_search || find) && self.nav.page == Page::Help && !self.overlay_open() {
            self.help.focus_search = true;
        }
        if probe_enter {
            screens::probe::enter_first(self);
            self.set_page(Page::Probe);
        }
        if probe_next {
            screens::probe::category_step(self, true);
        }
        if page_up {
            self.page_up();
        }
        if page_down {
            self.page_down();
        }
        if home {
            self.first_item();
        }
        if end {
            self.last_item();
        }
        if focus_left {
            self.focus_nav();
        }
        if focus_right {
            self.nav.focus_region = FocusRegion::Page;
            self.nav.page_focus_requested = true;
        }
        if open_messages {
            self.overlay.messages_dialog = true;
        }
        if escape {
            self.escape();
        }
        if mdi_issue {
            screens::mdi::issue(self);
        }
        if mdi_line_up {
            self.mdi.move_selected(true);
        }
        if mdi_line_down {
            self.mdi.move_selected(false);
        }
        if mdi_up {
            self.mdi.recall_older();
        }
        if mdi_down {
            self.mdi.recall_newer();
        }
        if mdi_delete {
            self.mdi.delete_selected();
        }
        if mdi_clear {
            self.mdi.clear_input();
        }
        if mdi_run {
            screens::mdi::run_selected(self);
        }
        if mdi_goto {
            self.overlay.modal = Some(crate::state::Modal::GoToLine {
                buffer: String::new(),
                mdi: true,
            });
        }
        if mdi_save {
            self.request_mdi_save();
        }

        if self.nav.focus_region == FocusRegion::Nav {
            let mut activate = false;
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.nav.nav_focus =
                        (self.nav.nav_focus + 1).min(self.nav.pages.len().saturating_sub(1));
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.nav.nav_focus = self.nav.nav_focus.saturating_sub(1);
                }
                if input.consume_key(
                    self.prefs.keymap.activate.modifiers,
                    self.prefs.keymap.activate.key,
                ) {
                    activate = true;
                }
            });
            if activate {
                let page = self
                    .nav
                    .pages
                    .get(self.nav.nav_focus)
                    .copied()
                    .unwrap_or(Page::Machine);
                self.request_page(page);
            }
        }
    }

    pub fn page_up(&mut self) {
        match self.nav.page {
            Page::Diagnostics => {
                let tabs = DiagnosticsTab::visible(&self.profile);
                let index = tabs
                    .iter()
                    .position(|tab| *tab == self.diagnostics.tab)
                    .unwrap_or(0);
                self.diagnostics.tab = tabs[(index + tabs.len() - 1) % tabs.len()];
            }
            Page::Offsets => {
                self.offsets.selected = if self.offsets.selected <= 1 {
                    6
                } else {
                    self.offsets.selected - 1
                };
            }
            Page::Probe => screens::probe::category_step(self, false),
            _ => {}
        }
    }

    pub fn page_down(&mut self) {
        match self.nav.page {
            Page::Diagnostics => {
                let tabs = DiagnosticsTab::visible(&self.profile);
                let index = tabs
                    .iter()
                    .position(|tab| *tab == self.diagnostics.tab)
                    .unwrap_or(0);
                self.diagnostics.tab = tabs[(index + 1) % tabs.len()];
            }
            Page::Offsets => {
                self.offsets.selected = if self.offsets.selected >= 9 {
                    1
                } else {
                    self.offsets.selected + 1
                };
            }
            Page::Probe => screens::probe::category_step(self, true),
            _ => {}
        }
    }

    pub fn first_item(&mut self) {
        match self.nav.page {
            Page::Files => {
                self.files.nc_selected = 0;
                self.files.usb_selected = 0;
            }
            Page::Help => {
                self.help.selected_row = 0;
            }
            Page::Tools => self.tools.selected_row = 0,
            _ => {}
        }
    }

    pub fn last_item(&mut self) {
        match self.nav.page {
            Page::Files => {
                self.files.nc_selected = self.files.nc_entries.len().saturating_sub(1);
                self.files.usb_selected = self.files.usb_entries.len().saturating_sub(1);
            }
            Page::Tools => {
                self.tools.selected_row = self.snapshot.tool_table.rows.len().saturating_sub(1)
            }
            _ => {}
        }
    }
}
