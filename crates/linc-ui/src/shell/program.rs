//! The program editor's commands: load, save, line jumps, run-from-line, and
//! the dirty-buffer bookkeeping.

use super::*;

impl LincApp {
    /// Accept the go-to-line dialog: a program line, or the MDI buffer line
    /// the pane points at.
    pub fn go_to_line(&mut self, line: usize, mdi: bool) {
        if mdi {
            self.mdi.goto(line);
        } else {
            self.jump_to_line(line);
        }
    }

    /// Ask to run from the selected line: opens the confirmation, or explains
    /// which condition blocks it. Silent failure would leave the shortcut
    /// looking broken.
    /// Delete a file from a Files pane, then refresh that pane.
    pub fn delete_file(&mut self, path: std::path::PathBuf, usb: bool) {
        // Validate against the pane's configured root, not just its current
        // directory, so a delete cannot escape it.
        let root = if usb {
            match self.files.usb_root.clone() {
                Some(root) => root,
                None => return,
            }
        } else {
            self.profile.nc_files_root().to_path_buf()
        };
        match crate::files::delete_file(&root, &path) {
            Ok(()) => {
                self.toast("file deleted", theme::SUCCESS);
                self.files.refresh();
            }
            Err(error) => self.toast(error.to_string(), theme::ERROR),
        }
    }

    /// Move the G-code selection by `step` lines, clamped to the buffer. Used by
    /// the arrows and page keys in a read-only view, where there is no text
    /// cursor to move.
    pub fn step_selected_line(&mut self, step: isize) {
        let count = self.program.editor.line_count();
        if count == 0 {
            return;
        }
        let selected =
            (self.program.selected_line as isize + step).clamp(1, count as isize) as usize;
        self.program.selected_line = selected;
        self.program.code_scroll_to_line = Some(selected);
        // Choosing a line is the operator taking over; the tool must not drag
        // the selection straight back on the next frame.
        self.program.follow_current = false;
    }

    /// Select a line in the G-code view: highlight it, scroll to it and, when
    /// the program is editable, put the text cursor there as well.
    pub fn jump_to_line(&mut self, line: usize) {
        let count = self.program.editor.line_count().max(1);
        let line = line.clamp(1, count);
        self.program.selected_line = line;
        self.program.code_scroll_to_line = Some(line);
        self.program.cursor_line = Some(line);
        self.program.follow_current = false;
        if self.editing_allowed() {
            self.program.focus_editor = true;
        }
    }

    pub fn request_run_from_line(&mut self) {
        if let Some(reason) = self.run_from_line_block() {
            self.toast(reason, theme::WARNING);
            return;
        }
        let (line, block) = self.run_from_line_choice();
        self.overlay.modal = Some(crate::state::Modal::ConfirmRunFromLine { line, block });
    }

    /// Run from the line selected in the G-code view, not from the controller's
    /// current line: after a program ends the controller sits on its last line,
    /// while the operator has picked the restart point in the editor.
    pub(super) fn run_from_line_choice(&self) -> (u32, String) {
        let line = self
            .program
            .selected_line
            .clamp(1, self.snapshot.program.line_count.max(1));
        let block = self
            .snapshot
            .program
            .content
            .as_deref()
            .and_then(|text| text.lines().nth(line - 1))
            .unwrap_or_default()
            .trim()
            .to_string();
        (line as u32, block)
    }

    pub fn request_load(&mut self, path: PathBuf) {
        if self.program.editor.dirty {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::LoadProgram(path)));
        } else {
            self.load_program(path);
        }
    }

    pub fn load_program(&mut self, path: PathBuf) {
        self.send(MachineCommand::LoadProgram {
            path,
            expected_revision: None,
        });
        // Do not navigate yet: SPEC §6.7 requires that a failed load stays in
        // Files and shows the error. The navigation is committed in
        // `poll_backend` when the backend confirms the load.
        self.program.pending_load = true;
    }

    /// A successful load always returns to Program in SPLIT, fitted and not
    /// started, overriding any remembered layout.
    pub(super) fn commit_load(&mut self) {
        self.program.pending_load = false;
        if let Some(path) = self.snapshot.program.path.clone() {
            let text = path.to_string_lossy().to_string();
            self.prefs.preferences.remember_file(&text, 12);
            self.prefs.dirty = true;
        }
        // Reset the view only when the machine is idle; the navigation itself
        // must always happen, or a load confirmed during a busy interpreter
        // leaves the operator in Files looking at the file they just loaded.
        if self.snapshot.task.interp == linc_core::model::InterpState::Idle {
            self.program.view = ProgramView::Split;
            self.program.graph_view = if self.profile.variant == MachineVariant::Lathe {
                linc_view::ViewMode::LatheXz
            } else {
                linc_view::ViewMode::Iso3d
            };
            self.program.graph_zoom = 1.0;
            self.program.graph_pan = Vec2::ZERO;
            self.program.follow_current = true;
            self.program.focus_editor = false;
        }
        // A new program starts at its first line: the old selection is a line
        // of a file that is no longer loaded, and leaving it highlighted made
        // the graph and the gutter point at the wrong block.
        self.program.selected_line = 1;
        self.program.code_scroll_to_line = Some(1);
        self.set_page(Page::Program);
    }

    pub fn cycle_program_view(&mut self) {
        // A plain toggle. EDIT mode selects CODE once on the way in (see
        // `on_mode_change`), but the operator can still switch views while
        // editing; forcing CODE here made the key look dead in EDIT.
        self.program.view = match self.program.view {
            ProgramView::Split => ProgramView::Code,
            ProgramView::Code => ProgramView::Split,
        };
        self.program.focus_editor = false;
        self.nav.page = Page::Program;
    }

    pub fn save_program(&mut self) {
        if !self.program.editor.dirty {
            self.toast("no changes to save", theme::MUTED);
            return;
        }
        if !self.editing_allowed() {
            let reason = if self.program_protected() {
                "PROTECT ON — EDITING LOCKED"
            } else if self.panel_offline() {
                "PANEL OFFLINE — SAVE BLOCKED"
            } else {
                "not idle in EDIT mode — save blocked"
            };
            self.toast(reason, theme::ERROR);
            return;
        }
        let Some(path) = self
            .program
            .editor
            .path
            .clone()
            .or_else(|| self.snapshot.program.path.clone())
        else {
            // Nothing loaded and nothing named: ask for a name instead of
            // failing. A program typed straight into EDIT is the common way to
            // start one, and refusing the save threw the work away.
            let directory = self.save_as_directory();
            self.overlay.modal = Some(Modal::SaveProgramAs(crate::state::SaveProgramDialog {
                name: String::new(),
                directory,
                error: None,
            }));
            return;
        };
        let content = self.program.editor.text();
        self.send(MachineCommand::SaveProgram {
            path,
            expected_revision: self.snapshot.program.revision,
            content: content.clone(),
        });
        // Local backends update immediately; asynchronous backends confirm via
        // the next snapshot.
        if self
            .snapshot
            .program
            .content
            .as_ref()
            .map(|current| current.as_str() == content)
            .unwrap_or(false)
        {
            self.program.editor.mark_saved();
            self.toast("program saved", theme::SUCCESS);
        } else if !self.program_protected() {
            self.toast("save queued", theme::ACCENT);
        }
    }

    /// Drop the editor buffer's unsaved changes: the loaded program is the
    /// truth again.
    ///
    /// `DISCARD` in the unsaved-changes dialog means this, whatever the
    /// operator was on their way to do. Navigating away while keeping the
    /// buffer dirty made the same prompt come back on every trip to Files, with
    /// nothing discarded.
    pub(super) fn discard_editor_changes(&mut self) {
        match self.snapshot.program.content.clone() {
            Some(text) => {
                let path = self.snapshot.program.path.clone();
                self.program
                    .editor
                    .set_loaded(path.clone().unwrap_or_default(), (*text).clone());
                self.program.loaded_path = path;
            }
            None => {
                // Nothing is loaded, so an empty buffer is what the controller
                // would run.
                self.program.editor = linc_core::editor::Editor::new();
                self.program.loaded_path = None;
            }
        }
        self.program.program_revision_seen = self.snapshot.program.revision;
        self.program.selected_line = 1;
        self.program.cursor_line = Some(1);
        self.program.code_scroll_to_line = Some(1);
        self.program.follow_current = true;
        self.refresh_preview();
    }

    pub fn mark_editor_changed(&mut self) {
        if !self.profile.config.program.live_preview {
            return;
        }
        self.program.preview_due = Some(
            Instant::now() + Duration::from_millis(self.profile.config.program.preview_debounce_ms),
        );
    }
}
