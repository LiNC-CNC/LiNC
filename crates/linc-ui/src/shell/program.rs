//! The program editor's commands: load, save, line jumps, run-from-line, and
//! the dirty-buffer bookkeeping.

use std::path::Path;

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
        let doc = self.program.doc_mut();
        let count = doc.editor.line_count();
        if count == 0 {
            return;
        }
        let selected = (doc.selected_line as isize + step).clamp(1, count as isize) as usize;
        doc.selected_line = selected;
        doc.code_scroll_to_line = Some(selected);
        // Choosing a line is the operator taking over; the tool must not drag
        // the selection straight back on the next frame.
        self.program.follow_current = false;
    }

    /// Select a line in the G-code view: highlight it, scroll to it and, when
    /// the program is editable, put the text cursor there as well.
    pub fn jump_to_line(&mut self, line: usize) {
        let doc = self.program.doc_mut();
        let count = doc.editor.line_count().max(1);
        let line = line.clamp(1, count);
        doc.selected_line = line;
        doc.code_scroll_to_line = Some(line);
        doc.cursor_line = Some(line);
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
            .loaded()
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
        // LOAD of the file being prepared is the handoff: the buffer's text is
        // the version that opens, and the prepared document is consumed by it
        // instead of leaving two documents on the same file.
        if self.is_prepared_path(&path) {
            self.request_handoff_prepared();
            return;
        }
        if self.program.loaded().editor.dirty {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::LoadProgram(path)));
        } else {
            self.load_program(path);
        }
    }

    /// Whether `path` is the file the prepared document was read from.
    fn is_prepared_path(&self, path: &Path) -> bool {
        self.program
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.path.as_deref())
            .is_some_and(|prepared| crate::screens::files::same_file(prepared, path))
    }

    /// Read `path` into the prepared document, replacing the one already there
    /// only after the unsaved-changes question.
    pub fn request_prepare_program(&mut self, path: PathBuf) {
        if self.program.pending_handoff || self.program.pending_prepared_save {
            self.toast(
                "a write is in flight; try again when it lands",
                theme::WARNING,
            );
            return;
        }
        if self
            .program
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.editor.dirty)
        {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::PrepareProgram(path)));
        } else {
            self.open_prepared(path);
        }
    }

    /// Read `path` into the prepared document. This is not a load: the file is
    /// read here and the controller is never asked to open it, so the action
    /// works in every machine state. A read that fails reports why and leaves
    /// the prepared document as it was.
    pub fn open_prepared(&mut self, path: PathBuf) {
        // The same rules LOAD applies, so any caller of this public entry point
        // gets them: a configured G-code extension, a file under NC_FILES, and
        // never the program the controller holds.
        let extension = path
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or_default();
        if !self
            .profile
            .config
            .files
            .load_extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&extension))
        {
            self.toast(
                format!("not a configured G-code extension: {extension}"),
                theme::ERROR,
            );
            return;
        }
        if !path.starts_with(self.profile.nc_files_root()) {
            self.toast("file is outside NC_FILES", theme::ERROR);
            return;
        }
        if self
            .program
            .loaded()
            .path
            .as_deref()
            .is_some_and(|loaded| crate::screens::files::same_file(loaded, &path))
        {
            self.toast("the loaded program cannot be prepared", theme::WARNING);
            return;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.toast(format!("cannot prepare {name}: {error}"), theme::ERROR);
                return;
            }
        };
        let mut prepared = Document::default();
        prepared.editor.set_loaded(path.clone(), text);
        prepared.path = Some(path);
        // A prepared file starts at its top. Without this the shared editor id
        // would carry the previous document's cursor, and the focused editor
        // would feed that line back as the prepared document's place.
        prepared.cursor_line = Some(1);
        self.program.prepared = Some(prepared);
        self.program.front = DocumentFront::Prepared;
        self.refit_after_document_change();
        self.refresh_preview();
        self.set_page(Page::Program);
        // The operator prepared this file to edit it: the next keystroke
        // belongs to the editor, not to the page's navigation.
        if self.editing_allowed() {
            self.program.focus_editor = true;
        }
    }

    /// Close the prepared document, asking first when it has unsaved changes.
    pub fn request_close_prepared(&mut self) {
        if self.program.prepared.is_none() {
            return;
        }
        if self.program.pending_handoff || self.program.pending_prepared_save {
            self.toast(
                "a write is in flight; try again when it lands",
                theme::WARNING,
            );
            return;
        }
        if self
            .program
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.editor.dirty)
        {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::ClosePrepared));
        } else {
            self.close_prepared();
        }
    }

    /// Give the editor back to the loaded program. The prepared buffer is
    /// dropped here, which only a confirmed discard or a clean buffer reaches.
    pub fn close_prepared(&mut self) {
        if self.program.prepared.take().is_none() {
            return;
        }
        self.program.front = DocumentFront::Loaded;
        self.refit_after_document_change();
        // The loaded document's preview stayed current while the prepared file
        // was in front, so re-parsing it here would only reset the run's
        // remaining-time cursor.
    }

    /// Frame the graph on the document that has just come to the front: a new
    /// program's bounds are not the old one's.
    fn refit_after_document_change(&mut self) {
        self.program.graph_zoom = 1.0;
        self.program.graph_pan = Vec2::ZERO;
        self.program.fit_requested = true;
    }

    pub fn load_program(&mut self, path: PathBuf) {
        if self.is_prepared_path(&path) {
            self.handoff_prepared();
            return;
        }
        // The load gate and the cycle-start gate ask the same question from two
        // ends: a file the check refuses never reaches the interpreter, and
        // `cycle_start_refusal` refuses the press on the interlock it leaves
        // shut. The key reaches the controller only through
        // `MachineCommand::CycleStart`, so a refused press sends nothing. A file
        // the check refuses still opens in the editor - the operator has to be
        // able to fix it where it is shown.
        if let Some(refusal) = self.syntax_refusal(&path) {
            self.refuse_load(path, refusal);
            return;
        }
        self.send(MachineCommand::LoadProgram {
            path,
            expected_revision: None,
        });
        // Do not navigate yet: SPEC §6.7 requires that a failed load stays in
        // Files and shows the error. The navigation is committed in
        // `poll_backend` when the backend confirms the load.
        self.program.pending_load = true;
    }

    /// Ask the interpreter about text that has just become the loaded program,
    /// remember the answer and publish the gate.
    ///
    /// The verdict held for the *old* text says nothing about the new one, and a
    /// program typed into EDIT was never asked about at all: save is the only
    /// route by which a file the interpreter refuses could reach the panel with
    /// the interlock open. Both save paths - overwrite and save-as - end here.
    pub(super) fn verify_saved_text(&mut self, content: &str) {
        self.program.machine_check = self.controller_check(content);
        if let Some(check) = self
            .program
            .machine_check
            .as_ref()
            .filter(|check| check.outcome == linc_core::verify::CheckOutcome::Refused)
        {
            let reason = format!("SAVED — {}", check.summary());
            self.toast(reason, theme::ERROR);
        }
        self.sync_program_ok();
    }

    /// Ask the controller's own interpreter about `text`, when this profile asks
    /// it to. `None` means the question is off in the configuration, which is not
    /// the same answer as "no faults".
    ///
    /// `rs274` reads the file the way the machine's task will: same reader, same
    /// preparation, no axis moves. It is the only check here that knows what a
    /// line means in the context of the lines above it, so it runs on everything
    /// that can become the loaded program - a load, and a save, which is how a
    /// program typed straight into the editor becomes one.
    fn controller_check(&mut self, text: &str) -> Option<linc_core::verify::MachineCheck> {
        if !self.profile.config.program.verify_on_load {
            return None;
        }
        let timeout =
            std::time::Duration::from_millis(self.profile.config.program.verify_timeout_ms.max(1));
        Some(linc_core::verify::check_program(
            Some(self.profile.machine_ini_path.as_path()),
            Some(self.profile.machine_ini.tool_table.as_path()),
            text,
            timeout,
        ))
    }

    /// Why this file must not be loaded, if it must not.
    ///
    /// Two checks, cheapest first: the block-level lint always runs, and the
    /// controller's own interpreter joins it when `program.verify_on_load` is
    /// set. A machine without `rs274` installed answers `Unavailable`, which is
    /// not a refusal - the machine itself will still have the last word at run
    /// time.
    fn syntax_refusal(&mut self, path: &Path) -> Option<String> {
        if !self.profile.config.program.block_run_on_syntax_error {
            return None;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            // Unreadable is the backend's error to report, with its own words.
            return None;
        };
        if let Some(issue) = linc_core::lint::lint_program(&text).into_iter().next() {
            return Some(format!("NOT LOADED  LN {}: {}", issue.line, issue.message));
        }
        // The block checks are not the whole story: the preview's own fatal fault
        // is a semantic one (`arc missing valid I/J/K or R`, a polar word against
        // the wrong plane, a G76 in the wrong plane) that no single block decides.
        let axes = self.axes();
        let preview = linc_core::gcode::parse_preview(&text, &axes, None, self.tool_tip());
        if let Some(error) = &preview.error {
            return Some(format!("NOT LOADED  LN {}: {}", error.line, error.message));
        }
        let Some(check) = self.controller_check(&text) else {
            return None;
        };
        self.program.machine_check = Some(check.clone());
        match check.outcome {
            linc_core::verify::CheckOutcome::Refused => {
                let place = match check.line {
                    Some(line) => format!("LN {line}"),
                    None => check
                        .message
                        .as_deref()
                        .and_then(linc_core::verify::line_from_message)
                        .map_or_else(|| "MACHINE".to_string(), |line| format!("LN {line}")),
                };
                Some(format!(
                    "NOT LOADED  {place}: {}",
                    check
                        .message
                        .as_deref()
                        .unwrap_or("the interpreter refused it")
                ))
            }
            // A crash is not a verdict either way, and the one thing it must
            // never be read as is "the file is fine": the interpreter died
            // before it finished reading, so the load is refused and the
            // interlock stays shut.
            linc_core::verify::CheckOutcome::Crashed => Some(format!(
                "NOT LOADED  {}: the interpreter crashed before it read the program",
                check
                    .line
                    .map_or_else(|| "MACHINE".to_string(), |line| format!("LN {line}"))
            )),
            // Ok, Timeout and Unavailable all let the load through.
            _ => None,
        }
    }

    /// Show the refused file in the editor, mark its faults and say why the
    /// interpreter does not have it.
    fn refuse_load(&mut self, path: PathBuf, reason: String) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let loaded = self.program.loaded_mut();
        loaded.editor.set_loaded(path.clone(), text);
        loaded.path = Some(path);
        self.program.pending_load = false;
        self.refresh_loaded_preview();
        self.program.loaded_mut().issue_index = 0;
        if let Some(line) = self.program.loaded().issues.first().map(|issue| issue.line) {
            let loaded = self.program.loaded_mut();
            loaded.selected_line = line;
            loaded.cursor_line = Some(line);
            loaded.code_scroll_to_line = Some(line);
        }
        self.set_page(Page::Program);
        self.toast_for(reason, theme::ERROR, 12);
        self.sync_program_ok();
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
        self.finish_program_commit();
    }

    /// The editor-side half of a program change, shared by a load and a
    /// handoff: back to Program, fitted and at the first line.
    fn finish_program_commit(&mut self) {
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
        self.program.loaded_mut().selected_line = 1;
        self.program.loaded_mut().code_scroll_to_line = Some(1);
        self.program.completed_at = None;
        self.set_page(Page::Program);
        // The interpreter holds the file and the buffer is clean: the interlock
        // may open, and saying so here keeps it from waiting for the next edit.
        self.program.machine_check = None;
        self.refresh_loaded_preview();
    }

    /// Hand the prepared document to the controller, asking first when the
    /// loaded document still holds unsaved changes.
    pub fn request_handoff_prepared(&mut self) {
        if self.program.prepared.is_none() {
            return;
        }
        if self.program.loaded().editor.dirty {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::HandoffPrepared));
        } else {
            self.handoff_prepared();
        }
    }

    /// Send the handoff, if one is not already in flight.
    ///
    /// The backend writes the prepared buffer's text and then opens that file.
    /// The press is recorded as pending until the result arrives; the prepared
    /// buffer's dirty marker goes with the acceptance and comes back if the
    /// result is an error.
    pub fn handoff_prepared(&mut self) {
        if self.program.pending_handoff {
            return;
        }
        if self.program.pending_prepared_save {
            self.toast("the prepared save is still in flight", theme::WARNING);
            return;
        }
        let Some(prepared) = self.program.prepared.as_ref() else {
            return;
        };
        let Some(path) = prepared.path.clone() else {
            return;
        };
        let content = prepared.editor.text();
        let was_dirty = prepared.editor.dirty;
        self.program.pending_handoff = true;
        self.program.handoff_was_dirty = was_dirty;
        self.program.handoff_sent = Some((path.clone(), content.clone()));
        let command = MachineCommand::LoadPreparedProgram {
            path: path.clone(),
            content: content.clone(),
        };
        if self.try_send(command) {
            if self.program.pending_handoff {
                if self.handoff_landed(&path, &content) {
                    // A synchronous backend has already opened it; there is no
                    // result event to wait for.
                    self.commit_handoff();
                } else if let Some(prepared) = self.program.prepared.as_mut() {
                    prepared.editor.dirty = false;
                }
            }
        } else {
            // Refused before it reached the controller: no result event will
            // come, and the buffer keeps the marker it had.
            self.program.pending_handoff = false;
            self.program.handoff_sent = None;
        }
    }

    /// Whether the controller already reports the handed-off file as its
    /// program, which is how a synchronous backend confirms the handoff.
    fn handoff_landed(&self, path: &Path, content: &str) -> bool {
        let program = &self.snapshot.program;
        !program.running
            && program
                .path
                .as_deref()
                .is_some_and(|loaded| crate::screens::files::same_file(loaded, path))
            && program
                .content
                .as_ref()
                .is_some_and(|text| text.as_str() == content)
    }

    /// The backend accepted the handoff: the prepared document is now the
    /// loaded one, and the editor follows it.
    pub(super) fn commit_handoff(&mut self) {
        self.program.pending_handoff = false;
        let sent = self.program.handoff_sent.take();
        if let (Some((sent_path, sent_text)), Some(mut prepared)) =
            (sent.clone(), self.program.prepared.take())
        {
            if prepared
                .path
                .as_deref()
                .is_some_and(|path| crate::screens::files::same_file(path, &sent_path))
            {
                if prepared.editor.text() == sent_text {
                    prepared.editor.mark_saved();
                }
                self.program.loaded = prepared;
            } else {
                // The prepared document changed under the press; the controller
                // opened the sent file, so adopt the snapshot's text instead.
                self.program.prepared = Some(prepared);
            }
        }
        // Keep the controller's own spelling of the path when the snapshot
        // already names the opened file, the same path a load would carry.
        if let Some(path) = self.snapshot.program.path.clone() {
            if self
                .program
                .loaded()
                .path
                .as_deref()
                .is_some_and(|loaded| crate::screens::files::same_file(loaded, &path))
            {
                self.program.loaded_mut().path = Some(path);
            }
        }
        // The snapshot's revision is the newest the UI knows about. The loaded
        // document was just replaced on purpose, so it must not be mistaken for
        // an external change; the opened file's own revision arrives next.
        self.program.loaded_mut().program_revision_seen = self.snapshot.program.revision;
        if let Some(path) = self.program.loaded().path.clone() {
            let text = path.to_string_lossy().to_string();
            self.prefs.preferences.remember_file(&text, 12);
            self.prefs.dirty = true;
        }
        self.program.front = DocumentFront::Loaded;
        self.finish_program_commit();
    }

    /// The handoff failed: the prepared buffer keeps its text and gets its
    /// dirty marker back, and the loaded program is left as it was.
    pub(super) fn abandon_handoff(&mut self) {
        self.program.pending_handoff = false;
        let sent = self.program.handoff_sent.take();
        if let Some(prepared) = self.program.prepared.as_mut() {
            // Text typed after the press is not in the refusal's file, so the
            // marker stays even when the buffer was clean when it went out.
            let typed_since = sent
                .as_ref()
                .is_some_and(|(_, text)| prepared.editor.text() != *text);
            prepared.editor.dirty = self.program.handoff_was_dirty || typed_since;
        }
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

    /// `SAVE` / the save chord: the document in front.
    ///
    /// The prepared document has its own file-only write; the loaded program
    /// keeps the controller's save, including the save-as dialog when the
    /// buffer has no file yet.
    pub fn save_program(&mut self) {
        if self.front_is_prepared() {
            self.save_prepared_program();
        } else {
            self.save_loaded_program();
        }
    }

    /// `SAVE` on the prepared document: write its file and leave the
    /// controller alone.
    ///
    /// The write is the backend's file-only verb, so the loaded program, its
    /// running text and the interlock are untouched. The dirty marker goes on
    /// acceptance and comes back with an error result, the way the handoff's
    /// does; a synchronous backend has no result event, so the file itself is
    /// the confirmation.
    fn save_prepared_program(&mut self) {
        if self.program.pending_prepared_save {
            return;
        }
        if self.program.pending_handoff {
            self.toast("the handoff is still in flight", theme::WARNING);
            return;
        }
        let Some(prepared) = self.program.prepared.as_ref() else {
            return;
        };
        if !prepared.editor.dirty {
            self.toast("no changes to save", theme::MUTED);
            return;
        }
        if !self.prepared_write_allowed() {
            let reason = self.save_block_reason();
            self.toast(reason, theme::ERROR);
            return;
        }
        // A prepared document always has a path: it was read from a file, and
        // nothing creates one without. There is no save-as for it.
        let Some(path) = prepared.path.clone() else {
            return;
        };
        let content = prepared.editor.text();
        self.program.pending_prepared_save = true;
        self.program.prepared_save_was_dirty = true;
        self.program.prepared_save_sent = Some(content.clone());
        let command = MachineCommand::SaveProgramFile {
            path: path.clone(),
            content: content.clone(),
        };
        if !self.try_send(command) {
            // Refused before it reached the controller: no result event will
            // come, and the buffer keeps the marker it had.
            self.program.pending_prepared_save = false;
            return;
        }
        if !self.program.pending_prepared_save {
            // A result event arrived inside the send and has settled the
            // marker already.
            return;
        }
        // The write was accepted. The marker goes with the acceptance and an
        // error result puts it back. A synchronous backend sends no result
        // event, so its answer is the file itself; this runs once per operator
        // save, not per frame.
        if let Some(prepared) = self.program.prepared.as_mut() {
            prepared.editor.dirty = false;
        }
        if std::fs::read_to_string(&path).is_ok_and(|written| written == content) {
            self.commit_prepared_save();
        } else {
            self.toast("save queued", theme::ACCENT);
        }
    }

    /// The backend accepted the prepared write.
    pub(super) fn commit_prepared_save(&mut self) {
        self.program.pending_prepared_save = false;
        let sent = self.program.prepared_save_sent.take();
        if let (Some(sent), Some(prepared)) = (sent, self.program.prepared.as_mut()) {
            if prepared.editor.text() == sent {
                // The file now holds this text; the next edit is dirty against
                // it, and an undo back to it is clean again.
                prepared.editor.mark_saved();
            }
        }
        self.toast("program saved", theme::SUCCESS);
    }

    /// The prepared write failed: the buffer keeps its text and gets its dirty
    /// marker back.
    pub(super) fn abandon_prepared_save(&mut self) {
        self.program.pending_prepared_save = false;
        let sent = self.program.prepared_save_sent.take();
        if let Some(prepared) = self.program.prepared.as_mut() {
            let typed_since = sent
                .as_ref()
                .is_some_and(|text| prepared.editor.text() != *text);
            prepared.editor.dirty = self.program.prepared_save_was_dirty || typed_since;
        }
    }

    pub(super) fn save_loaded_program(&mut self) {
        if !self.program.loaded().editor.dirty {
            self.toast("no changes to save", theme::MUTED);
            return;
        }
        if !self.loaded_editing_allowed() {
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
            .loaded()
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
        let content = self.program.loaded().editor.text();
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
            self.program.loaded_mut().editor.mark_saved();
            self.toast("program saved", theme::SUCCESS);
        } else if !self.program_protected() {
            self.toast("save queued", theme::ACCENT);
        }
        self.verify_saved_text(&content);
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
                let loaded = self.program.loaded_mut();
                loaded
                    .editor
                    .set_loaded(path.clone().unwrap_or_default(), (*text).clone());
                loaded.path = path;
            }
            None => {
                // Nothing is loaded, so an empty buffer is what the controller
                // would run.
                let loaded = self.program.loaded_mut();
                loaded.editor = linc_core::editor::Editor::new();
                loaded.path = None;
            }
        }
        self.program.loaded_mut().program_revision_seen = self.snapshot.program.revision;
        let loaded = self.program.loaded_mut();
        loaded.selected_line = 1;
        loaded.cursor_line = Some(1);
        loaded.code_scroll_to_line = Some(1);
        self.program.completed_at = None;
        self.program.follow_current = true;
        self.refresh_loaded_preview();
    }

    pub fn mark_editor_changed(&mut self) {
        // The interpreter's verdict was about the loaded text before this
        // keystroke. Keeping it would leave a stale red line under a block that
        // is already fixed, and a stale "the machine refused it" beside a
        // repaired file. An edit to the prepared document says nothing about the
        // text the machine holds, so that verdict stands.
        if self.program.front == DocumentFront::Loaded {
            self.program.machine_check = None;
        }
        self.sync_program_ok();
        if !self.profile.config.program.live_preview {
            return;
        }
        self.program.doc_mut().preview_due = Some(
            Instant::now() + Duration::from_millis(self.profile.config.program.preview_debounce_ms),
        );
    }
}
