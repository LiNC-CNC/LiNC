//! Modal dialogs: confirmation, tool change, save-as, toasts, and the one-shot
//! focus that puts the keyboard on the safe button when a dialog opens.

use super::*;

impl LincApp {
    /// `Ctrl+S` on the pane: open the save dialog, or say why it cannot write.
    pub fn request_mdi_save(&mut self) {
        if let Some(reason) = self.mdi_save_block() {
            self.toast(reason, theme::ERROR);
            return;
        }
        if self.mdi.is_empty() {
            self.mdi.last_error = Some("the MDI buffer is empty".into());
            return;
        }
        self.overlay.modal = Some(crate::state::Modal::MdiSave(
            crate::state::MdiSaveDialog::default(),
        ));
    }

    /// Edit the open save dialog. A closed dialog is not an error: the frame
    /// that confirms a write clears it before these run.
    pub(super) fn edit_mdi_save(&mut self, update: impl FnOnce(&mut crate::state::MdiSaveDialog)) {
        if let Some(crate::state::Modal::MdiSave(dialog)) = &mut self.overlay.modal {
            update(dialog);
        }
    }

    /// One step of the save dialog: write the buffer, or, when the name is
    /// taken, arm the overwrite step inside the same dialog.
    pub(super) fn confirm_mdi_save(&mut self) {
        let Some(crate::state::Modal::MdiSave(dialog)) = self.overlay.modal.clone() else {
            return;
        };
        if let Some(reason) = self.mdi_save_block() {
            self.edit_mdi_save(|dialog| dialog.error = Some(reason.to_string()));
            return;
        }
        match screens::mdi::save(self, &dialog.name, dialog.wrap, dialog.confirm_overwrite) {
            Ok(path) => {
                // The buffer is kept: the blocks are still the session's
                // working list, and a save is not a reset.
                self.overlay.modal = None;
                self.mdi.last_error = None;
                self.toast(format!("saved {}", path.display()), theme::SUCCESS);
            }
            Err(SaveError::Exists(_)) if !dialog.confirm_overwrite => {
                self.edit_mdi_save(|dialog| {
                    dialog.confirm_overwrite = true;
                    dialog.error = None;
                });
            }
            Err(error) => {
                self.edit_mdi_save(|dialog| dialog.error = Some(error.to_string()));
            }
        }
    }

    /// The folder an untitled buffer is written into: the `NC_FILES` directory
    /// the file browser is showing, falling back to its root when that one has
    /// walked above the configured tree.
    pub(super) fn save_as_directory(&self) -> PathBuf {
        let root = self.profile.nc_files_root().to_path_buf();
        if self.files.nc_current.starts_with(&root) {
            self.files.nc_current.clone()
        } else {
            root
        }
    }

    /// The extension an unnamed program gets: the first configured G-code
    /// extension, so the file the operator just saved is one the Files page
    /// will load.
    pub(super) fn save_as_extension(&self) -> String {
        self.profile
            .config
            .files
            .load_extensions
            .first()
            .cloned()
            .unwrap_or_else(|| ".ngc".into())
    }

    /// Write the buffer to the name the save-as dialog collected.
    ///
    /// Every reason to refuse is reported inside the dialog, which is still on
    /// screen: closing it on a bad name would lose the text the operator typed
    /// and leave the save looking like it happened.
    pub(super) fn save_program_as(&mut self, name: &str) {
        let Some(Modal::SaveProgramAs(dialog)) = self.overlay.modal.clone() else {
            return;
        };
        let name = name.trim();
        let invalid = name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0');
        if invalid {
            self.edit_save_as(|dialog| dialog.error = Some("invalid file name".into()));
            return;
        }
        let extension = self.save_as_extension();
        let name = if std::path::Path::new(name).extension().is_some() {
            name.to_string()
        } else {
            format!("{name}{extension}")
        };
        let path = dialog.directory.join(&name);
        if path.exists() {
            self.edit_save_as(|dialog| {
                dialog.error = Some(format!("{name} already exists; choose another name"))
            });
            return;
        }
        if !path.starts_with(self.profile.nc_files_root()) {
            self.edit_save_as(|dialog| {
                dialog.error = Some("that folder is outside NC_FILES".into())
            });
            return;
        }
        let content = self.program.editor.text();
        let accepted = self.try_send(MachineCommand::SaveProgram {
            path: path.clone(),
            expected_revision: self.snapshot.program.revision,
            content: content.clone(),
        });
        if !accepted {
            // The backend's reason is already a toast; keep the dialog so the
            // typed name is not lost with it.
            return;
        }
        // The buffer now has a home: the next Ctrl+S writes it without asking.
        self.program.editor.path = Some(path);
        // Both backends write the file before answering, so an accepted save is
        // a save: the dirty marker must not survive it.
        self.program.editor.mark_saved();
        self.overlay.modal = None;
        self.toast(format!("saved {name}"), theme::SUCCESS);
    }

    /// Edit the open save-as dialog. A closed dialog is not an error: the frame
    /// that confirms the write clears it before this runs.
    pub(super) fn edit_save_as(
        &mut self,
        update: impl FnOnce(&mut crate::state::SaveProgramDialog),
    ) {
        if let Some(Modal::SaveProgramAs(dialog)) = &mut self.overlay.modal {
            update(dialog);
        }
    }

    /// The manual tool-change prompt, on every page.
    ///
    /// Probe Basic's equivalent is an application-modal dialog driven by the
    /// HAL request, so the operator sees it wherever they are and cannot close
    /// it without confirming. This is the same idea: it follows
    /// `tool.change_active` rather than a page, and has no dismiss action.
    pub(super) fn draw_tool_change_prompt(&mut self, ctx: &egui::Context) {
        if self.snapshot.tool_change.status != linc_core::model::ToolChangeStatus::ManualPrompt {
            self.overlay.tool_prompt_open_last_frame = false;
            return;
        }
        let focus_default = !self.overlay.tool_prompt_open_last_frame;
        self.overlay.tool_prompt_open_last_frame = true;
        let state = self.snapshot.tool_change.clone();
        let remark = self
            .snapshot
            .tool_table
            .rows
            .iter()
            .find(|row| row.tool == state.prepared_tool)
            .map(|row| row.description.clone())
            .unwrap_or_default();
        let mut confirm = false;
        egui::Window::new("MANUAL TOOL CHANGE")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .max_width(560.0)
            .frame(
                Frame::new()
                    .fill(theme::PANEL_RAISED)
                    .stroke(Stroke::new(1.0, theme::WARNING))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("INSTALL T{}", state.prepared_tool))
                        .size(theme::font_size(20.0))
                        .strong()
                        .color(theme::FG),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("REMOVE T{}", state.current_tool))
                        .size(theme::font_size(12.0))
                        .color(theme::MUTED),
                );
                if !remark.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(remark).size(theme::font_size(13.0)));
                }
                ui.add_space(12.0);
                if state.confirmed {
                    ui.label(
                        RichText::new("CONFIRMED — WAITING FOR THE MACHINE")
                            .size(theme::font_size(12.0))
                            .strong()
                            .color(theme::SUCCESS),
                    );
                } else {
                    ui.label(
                        RichText::new("Change the tool by hand, then confirm. The program continues by itself.")
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                    let installed = theme::accent_button(
                        ui,
                        "TOOL INSTALLED",
                        self.snapshot.panel.connected && state.token.is_some(),
                    );
                    // It is the prompt's only action, so holding focus on it is
                    // harmless and survives a page underneath claiming it.
                    if focus_default || !installed.has_focus() {
                        installed.request_focus();
                    }
                    if installed.clicked() {
                        confirm = true;
                    }
                }
            });
        if confirm {
            if let Some(token) = state.token.clone() {
                self.send(MachineCommand::ConfirmManualToolChange { token });
            }
        }
    }

    pub(super) fn draw_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.overlay.modal.clone() else {
            self.overlay.modal_open_last_frame = None;
            return;
        };
        // On the frame a dialog opens the keyboard goes where the operator's
        // work is: a dialog with an input field puts the ring in that field,
        // and one without a field puts it on the safe button so a reflex Enter
        // never deletes, overwrites, quits or starts anything. There is no
        // click-to-focus on this panel and no Tab to walk in with. A different
        // dialog replacing this one on the same frame counts as opening.
        let first_frame = self.overlay.modal_open_last_frame != Some(modal.kind());
        self.overlay.modal_open_last_frame = Some(modal.kind());
        let mut close = false;
        let mut action: Option<PendingAction> = None;
        let mut save_and_continue = false;
        let mut start_line: Option<u32> = None;
        let mut delete: Option<(std::path::PathBuf, bool)> = None;
        let mut delete_tool: Option<u16> = None;
        let mut quit = false;
        let mut overwrite: Option<bool> = None;
        let mut goto_buffer: Option<String> = None;
        let mut goto_accept = false;
        let mut save_name: Option<String> = None;
        let mut save_wrap: Option<bool> = None;
        let mut save_confirm = false;
        let mut save_as_name: Option<String> = None;
        let mut save_as_confirm = false;
        let mut discard_on_leave = false;
        // A real modal, not a Window: the backdrop and modal layer stop the
        // page behind from receiving Enter/clicks while a dialog owns the
        // keyboard.
        egui::Modal::new(egui::Id::unique("linc-modal"))
            .frame(
                Frame::new()
                    .fill(theme::PANEL_RAISED)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                match &modal {
                Modal::ConfirmDiscard(pending) => {
                    ui.label(RichText::new("UNSAVED CHANGES").size(theme::font_size(18.0)).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label("The program buffer has unsaved changes. Leaving, loading another file or quitting will discard them.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "DISCARD", true).clicked() {
                            action = Some(pending.clone());
                        }
                        let saving = self.editing_allowed();
                        if theme::accent_button(ui, "SAVE", saving).clicked() {
                            save_and_continue = true;
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if first_frame {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                    // A greyed SAVE with no reason reads as a broken dialog. A
                    // save needs the panel in EDIT, PROTECT off, an idle
                    // interpreter and a live panel; say which one is missing.
                    if !self.editing_allowed() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(self.save_block_reason())
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                    }
                }
                Modal::ConfirmRunFromLine { line, block } => {
                    ui.label(
                        RichText::new("RUN FROM LINE")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                    ui.add_space(6.0);
                    ui.label(format!(
                        "Start at line {line} of the loaded program?"
                    ));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if block.is_empty() { "(empty line)" } else { block.as_str() })
                            .monospace()
                            .size(theme::font_size(12.0))
                            .color(theme::FG),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        "Lines before this one are skipped: work offsets, tool offsets, feed rate, \
                         spindle and coolant state are not restored. Motion starts from the current \
                         machine position.",
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "RUN FROM LINE", true).clicked() {
                            start_line = Some(*line);
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if first_frame {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmDelete { path, label, usb } => {
                    ui.label(
                        RichText::new("DELETE FILE")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::ERROR),
                    );
                    ui.add_space(6.0);
                    ui.label(format!("Delete {label}?"));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(path.display().to_string())
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label("This cannot be undone.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "DELETE", true).clicked() {
                            delete = Some((path.clone(), *usb));
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if first_frame {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmToolDelete { tool, description } => {
                    ui.label(
                        RichText::new("DELETE TOOL")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::ERROR),
                    );
                    ui.add_space(6.0);
                    ui.label(format!("Delete T{tool}?"));
                    if !description.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(description)
                                .monospace()
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                    }
                    ui.add_space(8.0);
                    ui.label("This removes the row from the tool table and cannot be undone.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "DELETE", true).clicked() {
                            delete_tool = Some(*tool);
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if first_frame {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmQuit => {
                    ui.label(RichText::new("QUIT LiNC?").size(theme::font_size(18.0)).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label("The HMI is not part of the hardware E-stop chain. Panel control remains with linc-panel.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "QUIT", true).clicked() {
                            quit = true;
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if first_frame {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::GoToLine { buffer, mdi } => {
                    ui.label(
                        RichText::new("GO TO LINE")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::FG),
                    );
                    ui.add_space(6.0);
                    // The same editor either way; only the list it selects in
                    // changes, so MAIN's Ctrl+G walks the MDI buffer while the
                    // Program page keeps walking the loaded program.
                    let (limit, what) = if *mdi {
                        (self.mdi.len().max(1), "Buffer line")
                    } else {
                        (self.program.editor.line_count().max(1), "Line number")
                    };
                    ui.label(format!("{what} (1-{limit}):"));
                    ui.add_space(6.0);
                    let mut text = buffer.clone();
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .id(egui::Id::unique("linc-goto-line-input"))
                            .desired_width(120.0)
                            .hint_text("line"),
                    );
                    // The dialog exists to collect a number, so the field takes
                    // the ring on the frame it opens: Enter accepts what was
                    // typed and CANCEL is one arrow away. An empty field accepts
                    // nothing, so Enter cannot jump the view by itself.
                    if first_frame {
                        field.request_focus();
                    }
                    if field.changed() {
                        // Digits only, so the field cannot hold something the
                        // jump would have to reject.
                        goto_buffer = Some(
                            text.chars()
                                .filter(char::is_ascii_digit)
                                .take(7)
                                .collect::<String>(),
                        );
                    }
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        goto_accept = true;
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::accent_button(ui, "GO", true).clicked() {
                            goto_accept = true;
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmOverwrite(name) => {
                    ui.label(RichText::new("OVERWRITE FILE?").size(theme::font_size(18.0)).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label(format!("{name} already exists in the destination. Replace it atomically?"));
                    ui.add_space(12.0);
                    if let Some(choice) = screens::files::overwrite_dialog(ui, first_frame) {
                        overwrite = Some(choice);
                    }
                }
                Modal::Error(text) => {
                    ui.label(RichText::new("ERROR").size(theme::font_size(18.0)).strong().color(theme::ERROR));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    let ok = theme::action_button(ui, "OK", true);
                    if first_frame {
                        ok.request_focus();
                    }
                    if ok.clicked() {
                        close = true;
                    }
                }
                Modal::Info(text) => {
                    ui.label(RichText::new("NOTICE").size(theme::font_size(18.0)).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    let ok = theme::action_button(ui, "OK", true);
                    if first_frame {
                        ok.request_focus();
                    }
                    if ok.clicked() {
                        close = true;
                    }
                }
                Modal::MdiSave(dialog) => {
                    ui.label(
                        RichText::new("SAVE MDI BUFFER")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::FG),
                    );
                    ui.add_space(6.0);
                    ui.label(format!(
                        "{} block(s) to {}:",
                        self.mdi.len(),
                        self.profile.machine_ini.program_prefix.display()
                    ));
                    ui.add_space(6.0);
                    let mut text = dialog.name.clone();
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .id(egui::Id::unique("linc-mdi-save-name"))
                            .desired_width(260.0)
                            .hint_text("part"),
                    );
                    // The dialog exists to name the file, so the field takes the
                    // ring on the frame it opens: Enter writes the typed name
                    // rather than taking a button. A dialog with no field focuses
                    // its safe button instead.
                    if first_frame {
                        field.request_focus();
                    }
                    if field.changed() {
                        save_name = Some(text);
                    }
                    let mut wrap = dialog.wrap;
                    if ui
                        .checkbox(&mut wrap, "ADD PRE/POSTAMBLE")
                        .changed()
                    {
                        save_wrap = Some(wrap);
                    }
                    ui.add_space(6.0);
                    if let Some(error) = &dialog.error {
                        ui.label(
                            RichText::new(error)
                                .size(theme::font_size(10.0))
                                .color(theme::ERROR),
                        );
                    }
                    if dialog.confirm_overwrite {
                        ui.label(
                            RichText::new(
                                "That name already exists. REPLACE overwrites it; CANCEL keeps their file.",
                            )
                            .size(theme::font_size(10.0))
                            .color(theme::WARNING),
                        );
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        // The overwrite step is this button, not a second
                        // dialog: the buffer and the typed name stay put.
                        let write = if dialog.confirm_overwrite {
                            theme::danger_button(ui, "REPLACE", true)
                        } else {
                            theme::accent_button(ui, "SAVE", true)
                        };
                        if write.clicked() {
                            save_confirm = true;
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                    if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                        save_confirm = true;
                    }
                }
                Modal::SaveProgramAs(dialog) => {
                    ui.label(
                        RichText::new("SAVE PROGRAM AS")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::FG),
                    );
                    ui.add_space(6.0);
                    ui.label("This buffer has no file yet. Name it under NC_FILES:");
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(dialog.directory.display().to_string())
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(6.0);
                    let mut text = dialog.name.clone();
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .id(egui::Id::unique("linc-save-as-name"))
                            .desired_width(280.0)
                            .hint_text("part.ngc"),
                    );
                    // The dialog exists to name the file, so the field takes the
                    // ring on the frame it opens: Enter writes the typed name
                    // rather than taking a button. A dialog with no field focuses
                    // its safe button instead.
                    if first_frame {
                        field.request_focus();
                    }
                    if field.changed() {
                        save_as_name = Some(text);
                    }
                    ui.add_space(6.0);
                    if let Some(error) = &dialog.error {
                        ui.label(
                            RichText::new(error)
                                .size(theme::font_size(10.0))
                                .color(theme::ERROR),
                        );
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let write = theme::accent_button(ui, "SAVE", true);
                        if write.clicked() {
                            save_as_confirm = true;
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                    if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                        save_as_confirm = true;
                    }
                }
                Modal::UnsavedOnLeave => {
                    ui.label(
                        RichText::new("UNSAVED PROGRAM")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                    ui.add_space(6.0);
                    // The panel left EDIT on its own. Say what that costs
                    // before the operator presses CYCLE START and nothing runs:
                    // AUTO runs the loaded file, never the editor buffer.
                    ui.label(
                        "The panel left EDIT with changes that were never saved. \
                         AUTO runs the loaded program, so this buffer will not start.",
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "A save needs the panel in EDIT. DISCARD drops the buffer; \
                             KEEP holds it until you go back to EDIT.",
                        )
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "DISCARD BUFFER", true).clicked() {
                            discard_on_leave = true;
                        }
                        let keep = theme::action_button(ui, "KEEP", true);
                        if first_frame {
                            keep.request_focus();
                        }
                        if keep.clicked() {
                            close = true;
                        }
                    });
                }
                }
            });
        if let Some(text) = goto_buffer {
            let mdi = matches!(self.overlay.modal, Some(Modal::GoToLine { mdi: true, .. }));
            self.overlay.modal = Some(Modal::GoToLine { buffer: text, mdi });
        }
        if goto_accept {
            let target = match self.overlay.modal.as_ref() {
                Some(Modal::GoToLine { buffer, mdi }) => {
                    buffer.trim().parse::<usize>().ok().map(|line| (line, *mdi))
                }
                _ => None,
            };
            if let Some((line, mdi)) = target {
                self.overlay.modal = None;
                self.go_to_line(line, mdi);
            }
        }
        if let Some(name) = save_name {
            // Typing a new name drops an armed overwrite: it belongs to the
            // name it was armed for.
            self.edit_mdi_save(|dialog| {
                dialog.name = name;
                dialog.confirm_overwrite = false;
                dialog.error = None;
            });
        }
        if let Some(wrap) = save_wrap {
            self.edit_mdi_save(|dialog| dialog.wrap = wrap);
        }
        if save_confirm {
            self.confirm_mdi_save();
        }
        if let Some(name) = save_as_name {
            self.edit_save_as(|dialog| {
                dialog.name = name;
                dialog.error = None;
            });
        }
        if save_as_confirm {
            let name = match &self.overlay.modal {
                Some(Modal::SaveProgramAs(dialog)) => dialog.name.clone(),
                _ => String::new(),
            };
            self.save_program_as(&name);
        }
        if discard_on_leave {
            self.overlay.modal = None;
            self.discard_editor_changes();
            self.toast("editor changes discarded", theme::WARNING);
        }
        if close {
            self.overlay.modal = None;
        }
        if let Some((path, usb)) = delete {
            self.overlay.modal = None;
            self.delete_file(path, usb);
        }
        if let Some(tool) = delete_tool {
            self.overlay.modal = None;
            self.send(linc_core::command::MachineCommand::DeleteToolTableRow {
                tool,
                expected_revision: self.snapshot.tool_table.revision,
            });
        }
        if let Some(line) = start_line {
            self.overlay.modal = None;
            self.send(linc_core::command::MachineCommand::RunFromLine { line });
        }
        if let Some(action) = action {
            self.overlay.modal = None;
            // DISCARD is the operator agreeing to lose the edits, so drop them
            // before the pending action runs. Carrying a dirty buffer through
            // the navigation brought the same prompt back on the next visit.
            self.discard_editor_changes();
            self.perform_pending(action);
        }
        if save_and_continue {
            let pending = match &modal {
                Modal::ConfirmDiscard(pending) => Some(pending.clone()),
                _ => None,
            };
            let before = self.program.editor.dirty;
            self.save_program();
            if !self.program.editor.dirty || !before {
                self.overlay.modal = None;
                if let Some(pending) = pending {
                    self.perform_pending(pending);
                }
            }
        }
        if quit {
            self.overlay.modal = None;
            self.chrome.quit_requested = true;
        }
        if let Some(overwrite) = overwrite {
            self.overlay.modal = None;
            screens::files::finish_overwrite(self, overwrite);
        }
    }

    pub(super) fn draw_toasts(&mut self, ctx: &egui::Context) {
        #[allow(deprecated)]
        let area = egui::Area::new(egui::Id::new("linc-toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-24.0, -90.0))
            .interactable(false);
        area.show(ctx, |ui| {
            for toast in self.overlay.toasts.iter().rev() {
                Frame::new()
                    .fill(theme::PANEL_RAISED)
                    .stroke(Stroke::new(1.0, toast.color))
                    .corner_radius(4)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&toast.text)
                                    .size(theme::font_size(12.0))
                                    .color(toast.color),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(4.0);
            }
        });
    }
}

/// Everything that sits in front of a page: the open modal, the messages
/// strip, toasts, and the one-shot flags that keep a dialog from looking dead.
pub(crate) struct OverlayUi {
    pub modal: Option<Modal>,
    /// Full-screen controller log, opened with `[keys].messages`.
    pub messages_dialog: bool,
    /// Page-scroll requested by the dialog's keys this frame.
    pub messages_scroll: f32,
    pub toasts: Vec<Toast>,
    /// Which dialog was open last frame, so a newly opened one can put the
    /// keyboard on its input field, or on its safe button when it has none.
    pub modal_open_last_frame: Option<&'static str>,
    /// Same, for the manual tool-change prompt, which is driven by the machine
    /// rather than by `self.overlay.modal`.
    pub tool_prompt_open_last_frame: bool,
    /// Whether any dialog or prompt was open on the previous frame. Closing the
    /// last one leaves the keyboard ownerless, so the shell hands it back to
    /// the page (see `restore_page_keyboard`).
    pub overlay_was_open: bool,
    pub confirm_token: Option<linc_core::model::ToolChangeToken>,
}
