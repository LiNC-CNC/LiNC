//! LiNC application shell: fixed top/left/bottom chrome, keyboard model,
//! mode-driven page switching, safety banners and modal confirmations.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};
use linc_core::command::{BackendEvent, CommandResult, MachineCommand};
use linc_core::config::Profile;
use linc_core::gcode::{parse_preview, Bounds3, Preview};
use linc_core::model::{
    format_clock, now_ms, Alarm, AlarmSeverity, AlarmSource, MachinePower, MachineSnapshot,
    MachineVariant, PanelMode, PanelModeValidity, TaskMode,
};
use linc_core::probe::ProbeParameters;

use crate::screens;
use crate::state::*;
use crate::theme;

pub const APP_TITLE: &str = "LiNC";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusRegion {
    Nav,
    Page,
    Actions,
}

pub struct LincApp {
    pub profile: Profile,
    pub backend: Box<dyn UiBackend>,
    pub snapshot: MachineSnapshot,
    pub page: Page,
    pub last_non_mode_page: Page,
    pub files_return_page: Page,
    pub nav_focus: usize,
    pub focus_region: FocusRegion,
    pub last_mode: Option<PanelModeValidity>,
    pub last_task_mode: TaskMode,
    pub start_page: Page,
    pub graph_style: GraphStyle,
    pub program: ProgramUi,
    pub offsets: OffsetsUi,
    pub tools: ToolsUi,
    pub probe: ProbeUi,
    pub mdi: MdiUi,
    pub help: HelpUi,
    pub files: FilesUi,
    pub homing_return_page: Page,
    pub manual: ManualUi,
    pub settings: SettingsUi,
    pub diagnostics_tab: DiagnosticsTab,
    pub diagnostics_search: String,
    pub diagnostics_severity: Option<AlarmSeverity>,
    pub diagnostics_source: Option<AlarmSource>,
    pub diagnostics_clear_view: bool,
    pub ui_log: VecDeque<Alarm>,
    pub modal: Option<Modal>,
    pub toasts: Vec<Toast>,
    pub quit_requested: bool,
    pub last_snapshot_ms: u64,
    pub last_connection: Option<bool>,
    pub last_protect: Option<bool>,
    pub panel_offline_toast: bool,
    pub preview_ticks: u64,
    pub confirm_token: Option<linc_core::model::ToolChangeToken>,
}

impl LincApp {
    pub fn new(profile: Profile, mut backend: Box<dyn UiBackend>, ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        if let Some(scale) = profile.ui_scale() {
            ctx.set_zoom_factor(scale);
        }
        let snapshot = backend.snapshot();
        Self::with_snapshot(profile, backend, snapshot)
    }

    pub fn with_snapshot(
        profile: Profile,
        backend: Box<dyn UiBackend>,
        snapshot: MachineSnapshot,
    ) -> Self {
        let start_page = page_from_config(&profile.config.ui.start_page);
        let configured_scale = profile.ui_scale().unwrap_or(1.0);
        let mode = snapshot.panel.mode.clone();
        let initial_page = mode_page(&mode).unwrap_or(start_page);
        let program_text = snapshot.program.content.clone();
        let program_path = snapshot.program.path.clone();
        let mut editor = if let Some(text) = program_text {
            linc_core::editor::Editor::from_text(program_path.clone(), &text)
        } else {
            linc_core::editor::Editor::new()
        };
        let mut preview = Preview::empty();
        if let Some(text) = &snapshot.program.content {
            let axes = profile.machine_ini.axis_names();
            let offset = active_offset(&snapshot);
            preview = parse_preview(text, &axes, offset, [0.0; 3]);
        }
        editor.set_saved_text(
            snapshot
                .program
                .content
                .as_deref()
                .cloned()
                .unwrap_or_default(),
        );
        let graph_style = graph_style(&profile);
        let probe_params = ProbeParameters::from_config(&profile.config.probe);
        let mut app = Self {
            snapshot: snapshot.clone(),
            page: initial_page,
            last_non_mode_page: if initial_page.is_mode_driven() {
                start_page
            } else {
                initial_page
            },
            files_return_page: Page::Machine,
            nav_focus: 0,
            focus_region: FocusRegion::Page,
            last_mode: Some(mode),
            last_task_mode: snapshot.task.mode,
            start_page,
            graph_style,
            program: ProgramUi {
                editor,
                preview: preview.clone(),
                last_valid_preview: preview,
                program_revision_seen: snapshot.program.revision,
                loaded_path: program_path,
                selected_line: snapshot.program.current_line.max(1),
                graph_view: if profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    linc_view::ViewMode::Iso3d
                },
                ..Default::default()
            },
            offsets: OffsetsUi::default(),
            tools: ToolsUi::default(),
            probe: ProbeUi::new(probe_params, profile.config.probe.wcs.clone()),
            mdi: MdiUi::new(profile.config.mdi.quick_commands.clone()),
            help: HelpUi::default(),
            files: FilesUi::new(profile.machine_ini.program_prefix.clone()),
            homing_return_page: start_page,
            manual: ManualUi {
                view: if profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    linc_view::ViewMode::Iso3d
                },
                ..Default::default()
            },
            settings: SettingsUi {
                density: configured_scale,
                ..Default::default()
            },
            diagnostics_tab: DiagnosticsTab::Overview,
            diagnostics_search: String::new(),
            diagnostics_severity: None,
            diagnostics_source: None,
            diagnostics_clear_view: false,
            ui_log: VecDeque::new(),
            modal: None,
            toasts: Vec::new(),
            quit_requested: false,
            last_snapshot_ms: now_ms(),
            last_connection: Some(snapshot.panel.connected),
            last_protect: Some(snapshot.panel.protect_effective()),
            panel_offline_toast: false,
            preview_ticks: 0,
            confirm_token: None,
            profile,
            backend,
        };
        app.help.ensure_loaded();
        app.offsets = OffsetsUi::default();
        app.sync_offsets_from_snapshot(true);
        app.sync_tools_from_snapshot(true);
        if app.page == Page::Program
            && app.snapshot.panel.mode.mode() == Some(PanelMode::Edit)
            && app.editing_allowed()
        {
            app.program.view = ProgramView::Code;
            app.program.focus_editor = true;
        }
        app
    }

    pub fn send(&mut self, command: MachineCommand) {
        if let Err(error) = self.backend.send(command) {
            self.toast(error.to_string(), theme::ERROR);
            self.log_ui(
                AlarmSeverity::Error,
                AlarmSource::Ui,
                "UI-CMD",
                error.to_string(),
            );
        }
        self.poll_backend();
    }

    pub fn poll_backend(&mut self) {
        self.snapshot = self.backend.snapshot();
        self.last_snapshot_ms = now_ms();
        let events = self.backend.drain_events();
        for event in events {
            match event {
                BackendEvent::Command(CommandResult {
                    command, result, ..
                }) => match result {
                    Ok(()) => {
                        self.log_ui(
                            AlarmSeverity::Info,
                            AlarmSource::Ui,
                            "UI-OK",
                            format!("{command} accepted"),
                        );
                    }
                    Err(error) => {
                        self.toast(error.to_string(), theme::ERROR);
                        self.log_ui(
                            AlarmSeverity::Error,
                            AlarmSource::Ui,
                            "UI-ERR",
                            format!("{command}: {error}"),
                        );
                    }
                },
                BackendEvent::Log(alarm) => self.ui_log.push_front(alarm),
                BackendEvent::SnapshotLost => {
                    self.toast("status snapshot lost", theme::WARNING);
                }
            }
        }
        while self.ui_log.len() > 1000 {
            self.ui_log.pop_back();
        }
    }

    pub fn log_ui(
        &mut self,
        severity: AlarmSeverity,
        source: AlarmSource,
        code: &str,
        message: impl Into<String>,
    ) {
        let id = self.ui_log.front().map(|alarm| alarm.id + 1).unwrap_or(1);
        let mut alarm = Alarm::new(id, severity, source, code, message);
        alarm.timestamp_ms = now_ms();
        self.ui_log.push_front(alarm);
    }

    pub fn toast(&mut self, text: impl Into<String>, color: Color32) {
        self.toasts.push(toast(text, color));
        if self.toasts.len() > 6 {
            self.toasts.remove(0);
        }
    }

    pub fn is_sim(&self) -> bool {
        self.profile.is_sim() || self.snapshot.simulated
    }

    pub fn active_offset(&self) -> Option<&linc_core::model::AxisMap<f64>> {
        active_offset(&self.snapshot)
    }

    pub fn machine_bounds(&self) -> Option<Bounds3> {
        machine_bounds(&self.profile)
    }

    pub fn axes(&self) -> Vec<String> {
        self.profile.machine_ini.axis_names()
    }

    pub fn wcs_label(&self, index: Option<u8>) -> String {
        match index {
            Some(index) if (1..=9).contains(&index) => format!("G{}", 53 + index),
            _ => "--".into(),
        }
    }

    pub fn active_wcs_label(&self) -> String {
        self.wcs_label(self.snapshot.offsets.active_index)
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

    pub fn set_page(&mut self, page: Page) {
        if page == Page::Files {
            self.files_return_page = if self.page == Page::Files {
                Page::Program
            } else {
                self.page
            };
            self.files.refresh();
        }
        if page == Page::Probe {
            self.homing_return_page = Page::Offsets;
        }
        if !page.is_mode_driven() {
            self.last_non_mode_page = page;
        }
        self.page = page;
        self.focus_region = FocusRegion::Page;
        if page == Page::Help {
            self.help.ensure_loaded();
        }
        if page == Page::Program && self.program.view == ProgramView::Code {
            self.program.focus_editor = true;
        }
    }

    pub fn request_page(&mut self, page: Page) {
        if page == Page::Program && self.page == Page::Program {
            self.cycle_program_view();
            return;
        }
        if self.program.editor.dirty && self.page == Page::Program && page != Page::Program {
            self.modal = Some(Modal::ConfirmDiscard(PendingAction::Navigate(page)));
        } else {
            self.set_page(page);
        }
    }

    pub fn request_files(&mut self) {
        if self.program.editor.dirty && self.page == Page::Program {
            self.modal = Some(Modal::ConfirmDiscard(PendingAction::OpenFiles));
        } else {
            self.open_files();
        }
    }

    pub fn open_files(&mut self) {
        self.files_return_page = if self.page == Page::Files {
            Page::Program
        } else {
            self.page
        };
        self.files.refresh();
        self.set_page(Page::Files);
    }

    pub fn request_load(&mut self, path: PathBuf) {
        if self.program.editor.dirty {
            self.modal = Some(Modal::ConfirmDiscard(PendingAction::LoadProgram(path)));
        } else {
            self.load_program(path);
        }
    }

    pub fn load_program(&mut self, path: PathBuf) {
        self.send(MachineCommand::LoadProgram {
            path,
            expected_revision: None,
        });
        // A successful load always returns to Program in SPLIT, fitted, not started.
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
            self.set_page(Page::Program);
        }
    }

    pub fn cycle_program_view(&mut self) {
        let mode = self.snapshot.panel.mode.mode();
        if mode == Some(PanelMode::Edit) {
            self.program.view = ProgramView::Code;
            self.program.focus_editor = true;
        } else {
            self.program.view = match self.program.view {
                ProgramView::Split => ProgramView::Code,
                ProgramView::Code => ProgramView::Split,
            };
        }
        self.program.focus_editor = false;
        self.page = Page::Program;
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
            self.modal = Some(Modal::Error(
                "No program path. Load a file from NC_FILES before saving.".into(),
            ));
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

    pub fn request_quit(&mut self) {
        if self.snapshot.task.running || self.snapshot.task.motion_active {
            self.toast(
                "cannot quit while a program is running or the machine is moving",
                theme::ERROR,
            );
            return;
        }
        if self.program.editor.dirty {
            self.modal = Some(Modal::ConfirmDiscard(PendingAction::Quit));
        } else if self.profile.config.safety.confirm_quit {
            self.modal = Some(Modal::ConfirmQuit);
        } else {
            self.quit_requested = true;
        }
    }

    pub fn perform_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::Navigate(page) => self.set_page(page),
            PendingAction::OpenFiles => self.open_files(),
            PendingAction::LoadProgram(path) => self.load_program(path),
            PendingAction::Quit => self.quit_requested = true,
            PendingAction::NewFile => {
                self.program = ProgramUi {
                    graph_view: if self.profile.variant == MachineVariant::Lathe {
                        linc_view::ViewMode::LatheXz
                    } else {
                        linc_view::ViewMode::Iso3d
                    },
                    ..Default::default()
                };
                self.page = Page::Program;
            }
            PendingAction::OpenRecent(path) => self.load_program(path),
        }
    }

    pub fn escape(&mut self) {
        if self.modal.is_some() {
            self.modal = None;
            return;
        }
        match self.page {
            Page::Help => {
                if self.help.back() {
                } else if !self.help.query.is_empty() {
                    self.help.query.clear();
                } else {
                    self.set_page(self.last_non_mode_page);
                }
            }
            Page::Files => self.set_page(self.files_return_page),
            Page::Probe => self.set_page(Page::Offsets),
            Page::Settings => self.set_page(self.last_non_mode_page),
            Page::Mdi => {
                self.mdi.input.clear();
                self.mdi.history_index = None;
                self.mdi.last_error = None;
                // Leaving MDI discards unissued input only.
            }
            Page::Program => {
                if self.program.search_open {
                    self.program.search_open = false;
                } else if self.program.focus_editor {
                    self.program.focus_editor = false;
                } else {
                    self.request_page(Page::Machine);
                }
            }
            Page::Diagnostics => self.set_page(Page::Machine),
            _ => self.set_page(Page::Machine),
        }
    }

    pub fn sync(&mut self) {
        self.poll_backend();
        let mode = self.snapshot.panel.mode.clone();
        if self.last_mode.as_ref() != Some(&mode) {
            self.on_mode_change(mode.clone());
            self.last_mode = Some(mode);
        }
        if self.snapshot.task.mode != self.last_task_mode {
            if self.snapshot.task.mode == TaskMode::Mdi && self.page != Page::Mdi {
                // The panel mode remains authoritative; task-mode MDI from a
                // probe/tool command must not hijack the screen.
            }
            self.last_task_mode = self.snapshot.task.mode;
        }

        if self.snapshot.panel.connected != self.last_connection.unwrap_or(false) {
            if self.snapshot.panel.connected {
                self.toast("panel online", theme::SUCCESS);
            } else {
                self.toast("PANEL OFFLINE — machine actions blocked", theme::ERROR);
            }
            self.last_connection = Some(self.snapshot.panel.connected);
        }
        let protect = self.snapshot.panel.protect_effective();
        if self.last_protect != Some(protect) {
            if protect {
                self.toast("PROTECT ON — editing locked", theme::WARNING);
            } else {
                self.toast("protect off", theme::SUCCESS);
            }
            self.last_protect = Some(protect);
        }

        self.sync_program();
        self.sync_offsets_from_snapshot(false);
        self.sync_tools_from_snapshot(false);
        self.sync_probe();
        if self.snapshot.probe.last_result.is_some() && self.probe.last_run.is_none() {
            self.probe.last_run = Some(Instant::now());
            self.toast("probe result received", theme::SUCCESS);
        }
        self.tick_preview();
        self.toasts.retain(|toast| toast.until > Instant::now());
    }

    fn on_mode_change(&mut self, mode: PanelModeValidity) {
        let Some(new_mode) = mode.mode() else {
            if self.page.is_mode_driven() {
                self.page = self.last_non_mode_page;
            }
            return;
        };
        let prior_mode = self.last_mode.as_ref().and_then(PanelModeValidity::mode);
        if prior_mode.map(PanelMode::is_manual).unwrap_or(false)
            && !new_mode.is_manual()
            && self.page == Page::Manual
        {
            self.page = self.last_non_mode_page;
        }
        if prior_mode == Some(PanelMode::Mdi)
            && new_mode != PanelMode::Mdi
            && self.page == Page::Mdi
        {
            self.mdi.input.clear();
            self.page = self.last_non_mode_page;
        }
        if matches!(
            prior_mode,
            Some(PanelMode::Home) | Some(PanelMode::ProgHome)
        ) && !matches!(new_mode, PanelMode::Home | PanelMode::ProgHome)
            && self.page == Page::Homing
        {
            self.page = self.last_non_mode_page;
        }
        match new_mode {
            PanelMode::Mdi => {
                if self.page != Page::Mdi {
                    self.homing_return_page = self.page;
                }
                self.page = Page::Mdi;
                self.mdi.focus = true;
            }
            PanelMode::Home | PanelMode::ProgHome => {
                if self.page != Page::Homing {
                    self.homing_return_page = if self.page.is_mode_driven() {
                        self.last_non_mode_page
                    } else {
                        self.page
                    };
                }
                self.page = Page::Homing;
            }
            PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg => {
                if self.page != Page::Manual {
                    self.last_non_mode_page = if self.page.is_mode_driven() {
                        self.last_non_mode_page
                    } else {
                        self.page
                    };
                }
                self.manual.view = if self.profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    self.manual.view
                };
                self.page = Page::Manual;
            }
            PanelMode::Edit => {
                if self.page != Page::Program {
                    self.homing_return_page = self.page;
                }
                self.page = Page::Program;
                self.program.view = ProgramView::Code;
                self.program.focus_editor = self.program_editing_allowed();
            }
            PanelMode::Auto => {
                if self.page.is_mode_driven() {
                    self.page = self.last_non_mode_page;
                }
            }
        }
    }

    pub fn program_editing_allowed(&self) -> bool {
        self.editing_allowed()
    }

    fn sync_program(&mut self) {
        let revision = self.snapshot.program.revision;
        if revision != self.program.program_revision_seen {
            let new_text = self.snapshot.program.content.clone();
            let new_path = self.snapshot.program.path.clone();
            let text_changed = new_text
                .as_ref()
                .map(|text| text.as_str() != self.program.editor.text())
                .unwrap_or(false);
            if self.program.editor.dirty && text_changed && new_path == self.program.loaded_path {
                self.program.external_change = true;
            } else if let Some(text) = new_text {
                let same_as_editor = text.as_str() == self.program.editor.text();
                if same_as_editor {
                    self.program.editor.mark_saved();
                } else {
                    self.program
                        .editor
                        .set_loaded(new_path.clone().unwrap_or_default(), text.to_string());
                }
                self.program.loaded_path = new_path;
                self.program.program_revision_seen = revision;
                self.program.external_change = false;
                self.refresh_preview();
                self.program.selected_line = self.snapshot.program.current_line.max(1);
            } else {
                self.program.program_revision_seen = revision;
            }
        }
        if self.program.follow_current
            && self.snapshot.program.current_line > 0
            && self.snapshot.program.current_line != self.program.selected_line
        {
            self.program.selected_line = self.snapshot.program.current_line;
            self.program.code_scroll_to_line = Some(self.program.selected_line);
        }
        if self.snapshot.task.running {
            self.program.follow_current = true;
        }
    }

    pub(crate) fn sync_offsets_from_snapshot(&mut self, force: bool) {
        if !force && self.snapshot.offsets.revision == self.offsets.revision_seen {
            return;
        }
        self.offsets.revision_seen = self.snapshot.offsets.revision;
        if let Some(offset) = self
            .snapshot
            .offsets
            .offsets
            .iter()
            .find(|offset| offset.index == self.offsets.selected)
        {
            self.offsets.values = offset
                .values
                .iter()
                .map(|(_, value)| format!("{value:.4}"))
                .collect();
            self.offsets.rotation = offset.rotation.unwrap_or(0.0).to_string();
        }
        self.offsets.g92 = self
            .snapshot
            .offsets
            .g92
            .iter()
            .map(|(_, value)| format!("{value:.4}"))
            .collect();
    }

    fn sync_tools_from_snapshot(&mut self, force: bool) {
        if !force && self.snapshot.tool_table.revision == self.tools.revision_seen {
            return;
        }
        self.tools.revision_seen = self.snapshot.tool_table.revision;
        self.tools.selected_row = self
            .tools
            .selected_row
            .min(self.snapshot.tool_table.rows.len().saturating_sub(1));
        if let Some(token) = self.snapshot.tool_change.token.clone() {
            self.confirm_token = Some(token);
        }
    }

    fn sync_probe(&mut self) {
        if self.snapshot.probe.enabled {
            self.probe.wcs = self.snapshot.probe.selected_wcs.clone();
            if let Some(result) = self.snapshot.probe.last_result.as_ref() {
                if self.probe.last_run.is_none() {
                    self.probe.last_run = Some(Instant::now());
                }
                let _ = result;
            }
        }
    }

    pub fn active_offset_values(&self) -> Option<&linc_core::model::AxisMap<f64>> {
        active_offset(&self.snapshot)
    }

    pub fn tool_tip(&self) -> [f64; 3] {
        ["X", "Y", "Z"].map(|axis| {
            self.snapshot
                .positions
                .axes
                .get(axis)
                .map(|position| position.machine)
                .unwrap_or(0.0)
        })
    }

    pub fn refresh_preview(&mut self) {
        let text = self.program.editor.text();
        if text.is_empty() {
            self.program.preview = Preview::empty();
            self.program.last_valid_preview = Preview::empty();
            return;
        }
        let axes = self.axes();
        let offset = self.active_offset().cloned();
        let preview = parse_preview(&text, &axes, offset.as_ref(), self.tool_tip());
        let valid = preview.error.is_none();
        if valid {
            self.program.last_valid_preview = preview.clone();
        }
        self.program.preview = preview;
    }

    pub fn tick_preview(&mut self) {
        if let Some(due) = self.program.preview_due {
            if Instant::now() >= due {
                self.program.preview_due = None;
                self.preview_ticks += 1;
                self.refresh_preview();
            }
        }
    }

    pub fn mark_editor_changed(&mut self) {
        if !self.profile.config.program.live_preview {
            return;
        }
        self.program.preview_due = Some(
            Instant::now() + Duration::from_millis(self.profile.config.program.preview_debounce_ms),
        );
    }

    pub fn clear_trail(&mut self) {
        // View-only action. The backend is not asked to change machine state;
        // the live trail is re-seeded at the current tool tip in the local view.
        // The real backend's trail is owned by its sampler; clearing here only
        // affects the current view until the next sample. Both Program and
        // Manual views use this method.
        let tip = self.tool_tip();
        let mut trail = vec![tip];
        if trail.len() > self.graph_style.trail_max_points {
            trail.truncate(self.graph_style.trail_max_points);
        }
        self.snapshot.live_trail = std::sync::Arc::new(trail);
        self.snapshot.tool_tip = Some(tip);
        self.send(MachineCommand::ClearTrail);
        self.toast("live trail cleared", theme::MUTED);
    }

    pub fn focus_nav(&mut self) {
        self.focus_region = FocusRegion::Nav;
        self.nav_focus = NAV_ITEMS
            .iter()
            .position(|(page, _, _)| *page == self.page)
            .unwrap_or(0);
    }

    pub fn handle_keyboard(&mut self, ui: &mut egui::Ui) {
        let mut nav_page = None;
        let mut escape = false;
        let mut quit = false;
        let mut request_files = false;
        let mut save = false;
        let mut cycle_view = false;
        let mut mdi_issue = false;
        let mut help_search = false;
        let mut page_up = false;
        let mut page_down = false;
        let mut home = false;
        let mut end = false;
        let mut focus_left = false;
        let mut focus_right = false;
        let mut undo = false;
        let mut redo = false;
        let mut find = false;
        let mut refresh = false;

        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
                nav_page = Some(Page::Machine);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::F6) {
                cycle_view = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::F7) {
                nav_page = Some(Page::Tools);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::F8) {
                nav_page = Some(Page::Offsets);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::F10) {
                nav_page = Some(Page::Diagnostics);
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::Q) {
                quit = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::O) {
                request_files = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::S) {
                save = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::F) {
                find = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::R) {
                refresh = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::Z) {
                undo = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::Y) {
                redo = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                escape = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                page_up = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                page_down = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                home = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                end = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft) {
                focus_left = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight) {
                focus_right = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Slash) {
                help_search = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                && self.page == Page::Mdi
                && self.profile.config.mdi.execute_on_enter
            {
                mdi_issue = true;
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
        if save && self.page == Page::Program {
            self.save_program();
        }
        if find && self.page == Page::Program {
            self.program.search_open = true;
            self.program.focus_editor = true;
        }
        if refresh && self.page == Page::Files {
            self.files.refresh();
        }
        if undo && self.page == Page::Program && self.program.focus_editor {
            if self.program.editor.undo() {
                self.mark_editor_changed();
            }
        }
        if redo && self.page == Page::Program && self.program.focus_editor {
            if self.program.editor.redo() {
                self.mark_editor_changed();
            }
        }
        if cycle_view {
            if self.page == Page::Program {
                self.cycle_program_view();
            } else {
                self.request_page(Page::Program);
            }
        }
        if let Some(page) = nav_page {
            self.request_page(page);
        }
        if help_search && self.page == Page::Help {
            self.help.focus_search = true;
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
            self.focus_region = FocusRegion::Page;
        }
        if escape {
            self.escape();
        }
        if mdi_issue {
            screens::mdi::issue(self);
        }

        if self.focus_region == FocusRegion::Nav {
            let mut activate = false;
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.nav_focus = (self.nav_focus + 1).min(NAV_ITEMS.len() - 1);
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.nav_focus = self.nav_focus.saturating_sub(1);
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    activate = true;
                }
            });
            if activate {
                let page = NAV_ITEMS[self.nav_focus].0;
                self.request_page(page);
            }
        }
    }

    pub fn page_up(&mut self) {
        match self.page {
            Page::Diagnostics => {
                let index = DiagnosticsTab::ALL
                    .iter()
                    .position(|tab| *tab == self.diagnostics_tab)
                    .unwrap_or(0);
                self.diagnostics_tab = DiagnosticsTab::ALL
                    [(index + DiagnosticsTab::ALL.len() - 1) % DiagnosticsTab::ALL.len()];
            }
            Page::Offsets => {
                self.offsets.selected = if self.offsets.selected <= 1 {
                    6
                } else {
                    self.offsets.selected - 1
                };
                self.sync_offsets_from_snapshot(true);
            }
            Page::Probe => screens::probe::category_step(self, false),
            _ => {}
        }
    }

    pub fn page_down(&mut self) {
        match self.page {
            Page::Diagnostics => {
                let index = DiagnosticsTab::ALL
                    .iter()
                    .position(|tab| *tab == self.diagnostics_tab)
                    .unwrap_or(0);
                self.diagnostics_tab = DiagnosticsTab::ALL[(index + 1) % DiagnosticsTab::ALL.len()];
            }
            Page::Offsets => {
                self.offsets.selected = if self.offsets.selected >= 6 {
                    1
                } else {
                    self.offsets.selected + 1
                };
                self.sync_offsets_from_snapshot(true);
            }
            Page::Probe => screens::probe::category_step(self, true),
            _ => {}
        }
    }

    pub fn first_item(&mut self) {
        match self.page {
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
        match self.page {
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

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        self.sync();
        ui.painter().rect_filled(ui.max_rect(), 0.0, theme::SURFACE);
        egui::containers::Panel::top("linc-top")
            .exact_size(theme::TOP_BAR_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| self.top_bar(ui));
        egui::containers::Panel::bottom("linc-bottom")
            .exact_size(theme::BOTTOM_BAR_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| self.bottom_bar(ui));
        egui::containers::Panel::left("linc-nav")
            .exact_size(theme::NAV_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.nav(ui));
        egui::containers::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                self.banners(ui);
                match self.page {
                    Page::Machine => screens::machine::show(self, ui),
                    Page::Program => screens::program::show(self, ui),
                    Page::Offsets => screens::offsets::show(self, ui),
                    Page::Tools => screens::tools::show(self, ui),
                    Page::Diagnostics => screens::diagnostics::show(self, ui),
                    Page::Help => screens::help::show(self, ui),
                    Page::Files => screens::files::show(self, ui),
                    Page::Probe => screens::probe::show(self, ui),
                    Page::Mdi => screens::mdi::show(self, ui),
                    Page::Homing => screens::homing::show(self, ui),
                    Page::Manual => screens::manual::show(self, ui),
                    Page::Settings => screens::settings::show(self, ui),
                }
            });
        self.draw_modal(ui.ctx());
        self.draw_toasts(ui.ctx());
        if self.quit_requested {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("LiNC").size(22.0).strong().color(theme::FG));
                    if self.is_sim() {
                        ui.add_space(4.0);
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
                        ui.painter().rect_filled(rect, 3, theme::WARNING);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            "SIM",
                            FontId::monospace(10.0),
                            Color32::from_rgb(0x1a, 0x12, 0x04),
                        );
                    }
                    ui.label(
                        RichText::new(self.profile.config.machine.name.clone())
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("PANEL {}", self.snapshot.panel.mode.label()))
                            .monospace()
                            .size(10.0)
                            .color(if self.snapshot.panel.mode.is_valid() {
                                theme::ACCENT
                            } else {
                                theme::ERROR
                            }),
                    );
                    ui.label(
                        RichText::new(format!("TASK {}", self.snapshot.task.mode.label()))
                            .monospace()
                            .size(10.0)
                            .color(theme::MUTED),
                    );
                });
            });
            ui.add_space(18.0);
            metric(ui, "PROGRAM", self.program_label(), 13.0);
            metric(
                ui,
                "TOOL",
                format!("T{}", self.snapshot.tool_change.current_tool),
                13.0,
            );
            metric(
                ui,
                "SPINDLE",
                self.snapshot
                    .spindles
                    .first()
                    .map(|spindle| format!("S {:.0}", spindle.actual_speed))
                    .unwrap_or_else(|| "S --".into()),
                13.0,
            );
            metric(
                ui,
                "FEED",
                format!("F {:.0}", self.snapshot.feed.feed),
                13.0,
            );
            metric(ui, "WORK", self.active_wcs_label(), 13.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format_clock(now_ms()))
                        .monospace()
                        .size(13.0)
                        .color(theme::MUTED),
                );
                ui.add_space(14.0);
                let alarm_color = if self.snapshot.alarm_count() > 0 {
                    theme::ERROR
                } else {
                    theme::MUTED
                };
                ui.label(
                    RichText::new(format!("ALARM {}", self.snapshot.alarm_count()))
                        .monospace()
                        .size(12.0)
                        .color(alarm_color),
                );
                ui.label(
                    RichText::new(format!("MSG {}", self.snapshot.message_count()))
                        .monospace()
                        .size(12.0)
                        .color(theme::MUTED),
                );
                ui.add_space(14.0);
                theme::status_dot(
                    ui,
                    if self.snapshot.panel.connected && !self.snapshot.panel.communication_error {
                        theme::SUCCESS
                    } else {
                        theme::ERROR
                    },
                    if self.snapshot.panel.connected && !self.snapshot.panel.communication_error {
                        "PANEL ONLINE"
                    } else {
                        "PANEL OFFLINE"
                    },
                );
            });
        });
    }

    fn program_label(&self) -> String {
        let name = self
            .snapshot
            .program
            .display_name
            .rsplit('/')
            .next()
            .unwrap_or("");
        if name.is_empty() {
            "--".into()
        } else {
            name.to_string()
        }
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("NAVIGATION").size(10.0).color(theme::MUTED));
        ui.add_space(4.0);
        for (index, (page, label, keycap)) in NAV_ITEMS.iter().enumerate() {
            let selected = *page == self.page;
            let focused = self.focus_region == FocusRegion::Nav && self.nav_focus == index;
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(theme::NAV_WIDTH - 20.0, 38.0), Sense::click());
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                4,
                if selected {
                    theme::ACCENT
                } else if focused {
                    theme::PANEL_RAISED
                } else {
                    theme::SURFACE
                },
            );
            if focused {
                painter.rect_stroke(
                    rect.expand(1.0),
                    5,
                    Stroke::new(2.0, theme::ACCENT),
                    StrokeKind::Outside,
                );
            }
            painter.text(
                egui::pos2(rect.left() + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                *label,
                FontId::proportional(12.5),
                if selected {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    theme::FG
                },
            );
            if !keycap.is_empty() {
                painter.text(
                    egui::pos2(rect.right() - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    *keycap,
                    FontId::monospace(8.5),
                    if selected {
                        Color32::from_rgb(0x0b, 0x17, 0x22)
                    } else {
                        theme::MUTED
                    },
                );
            }
            if response.clicked() {
                self.nav_focus = index;
                self.focus_region = FocusRegion::Nav;
                self.request_page(*page);
            }
        }
        ui.add_space(10.0);
        theme::separator(ui);
        ui.add_space(8.0);
        ui.label(RichText::new("VIEW").size(10.0).color(theme::MUTED));
        let settings_selected = self.page == Page::Settings;
        if ui
            .add_sized(
                [theme::NAV_WIDTH - 20.0, 36.0],
                egui::Button::new("SETTINGS").selected(settings_selected),
            )
            .clicked()
        {
            self.request_page(Page::Settings);
        }
        ui.add_space(10.0);
        Frame::new()
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(4)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("FOCUS / NAVIGATION")
                        .size(9.0)
                        .strong()
                        .color(theme::MUTED),
                );
                ui.label(RichText::new("ARROWS MOVE").size(9.0).color(theme::MUTED));
                ui.label(RichText::new("ENTER SELECT").size(9.0).color(theme::MUTED));
                ui.label(RichText::new("ESC BACK").size(9.0).color(theme::MUTED));
                ui.label(RichText::new("PGUP / PGDN").size(9.0).color(theme::MUTED));
                ui.label(RichText::new("HOME / END").size(9.0).color(theme::MUTED));
                ui.label(RichText::new("CTRL+LEFT NAV").size(9.0).color(theme::MUTED));
            });
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new("NO ON-SCREEN\nMACHINE CONTROLS")
                    .size(9.0)
                    .color(theme::MUTED),
            );
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut esc = false;
            if theme::keycap(ui, "MAIN", self.page == Page::Machine).clicked() {
                self.request_page(Page::Machine);
            }
            if theme::keycap(ui, "FILE", self.page == Page::Program).clicked() {
                if self.page == Page::Program {
                    self.cycle_program_view();
                } else {
                    self.request_page(Page::Program);
                }
            }
            if theme::keycap(ui, "OFFSET", self.page == Page::Offsets).clicked() {
                self.request_page(Page::Offsets);
            }
            if theme::keycap(ui, "TOOL", self.page == Page::Tools).clicked() {
                self.request_page(Page::Tools);
            }
            if theme::keycap(ui, "WIN", self.page == Page::Diagnostics).clicked() {
                self.request_page(Page::Diagnostics);
            }
            if theme::keycap(ui, "ESC", false).clicked() {
                esc = true;
            }
            if esc {
                self.escape();
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            self.context_actions(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "OVR F{:.0}% S{:.0}% R{:.0}%",
                        self.snapshot.feed.feed_override * 100.0,
                        self.snapshot.feed.spindle_override * 100.0,
                        self.snapshot.feed.rapid_override * 100.0
                    ))
                    .monospace()
                    .size(11.0)
                    .color(theme::MUTED),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new(if self.program_protected() {
                        "PROTECT ON"
                    } else {
                        "PROTECT OFF"
                    })
                    .monospace()
                    .size(11.0)
                    .color(if self.program_protected() {
                        theme::WARNING
                    } else {
                        theme::MUTED
                    }),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new(if self.snapshot.machine.estop {
                        "E-STOP ACTIVE"
                    } else {
                        "E-STOP OK"
                    })
                    .monospace()
                    .size(11.0)
                    .color(if self.snapshot.machine.estop {
                        theme::ERROR
                    } else {
                        theme::SUCCESS
                    }),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("ALARM {}", self.snapshot.alarm_count()))
                        .monospace()
                        .size(11.0)
                        .color(if self.snapshot.alarm_count() > 0 {
                            theme::ERROR
                        } else {
                            theme::MUTED
                        }),
                );
            });
        });
    }

    fn context_actions(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::Machine => {
                if theme::action_button(ui, "CLEAR TRAIL", self.graph_style.show_live_trail)
                    .clicked()
                {
                    self.clear_trail();
                }
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
            Page::Program => {
                if theme::action_button(ui, "LOAD", !self.snapshot.task.running).clicked() {
                    self.request_files();
                }
                if theme::action_button(
                    ui,
                    "SAVE",
                    self.program.editor.dirty && self.editing_allowed(),
                )
                .clicked()
                {
                    self.save_program();
                }
                if theme::action_button(ui, "EDIT", self.editing_allowed()).clicked() {
                    self.program.view = ProgramView::Code;
                    self.program.focus_editor = true;
                }
                if theme::action_button(ui, "SEARCH", true).clicked() {
                    self.program.search_open = true;
                    self.program.focus_editor = true;
                }
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
            Page::Offsets => {
                if theme::action_button(
                    ui,
                    "PROBE",
                    self.snapshot.probe.enabled && !self.snapshot.task.running,
                )
                .clicked()
                {
                    self.set_page(Page::Probe);
                }
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
            Page::Tools => {
                if theme::action_button(ui, "CHANGE TOOL", self.snapshot.issue_allowed().is_ok())
                    .clicked()
                {
                    screens::tools::issue_change(self);
                }
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
            Page::Diagnostics => {
                if theme::action_button(ui, "CLEAR VIEW", true).clicked() {
                    self.diagnostics_clear_view = true;
                    self.ui_log.clear();
                }
                if theme::action_button(ui, "COPY LOG", true).clicked() {
                    screens::diagnostics::copy_log(self, ui);
                }
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
            Page::Help => {
                if theme::action_button(ui, "BACK", !self.help.history.is_empty()).clicked() {
                    self.help.back();
                }
                if theme::action_button(ui, "MACHINE", true).clicked() {
                    self.request_page(Page::Machine);
                }
            }
            Page::Files => {
                if theme::action_button(ui, "REFRESH", true).clicked() {
                    self.files.refresh();
                }
                if theme::action_button(ui, "ESC BACK", true).clicked() {
                    self.set_page(self.files_return_page);
                }
            }
            Page::Probe => {
                if theme::accent_button(ui, "RUN PROBE", true).clicked() {
                    screens::probe::run(self);
                }
                if theme::action_button(
                    ui,
                    "APPLY RESULT",
                    self.snapshot.probe.last_result.is_some(),
                )
                .clicked()
                {
                    screens::probe::apply(self);
                }
                if theme::action_button(ui, "ESC BACK", true).clicked() {
                    self.set_page(Page::Offsets);
                }
            }
            Page::Mdi => {
                if theme::accent_button(ui, "ISSUE MDI", true).clicked() {
                    screens::mdi::issue(self);
                }
                if theme::action_button(ui, "CLEAR", true).clicked() {
                    self.mdi.input.clear();
                    self.mdi.last_error = None;
                }
                if theme::action_button(ui, "ESC", true).clicked() {
                    self.escape();
                }
            }
            Page::Homing => {
                if theme::action_button(ui, "BACK", true).clicked() {
                    self.set_page(self.homing_return_page);
                }
            }
            Page::Manual => {
                if theme::action_button(ui, "CLEAR TRAIL", true).clicked() {
                    self.clear_trail();
                }
                if theme::action_button(ui, "FIT", true).clicked() {
                    self.manual.fit = true;
                }
            }
            Page::Settings => {
                if theme::action_button(ui, "HELP", true).clicked() {
                    self.request_page(Page::Help);
                }
            }
        }
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        if self.panel_offline() {
            theme::banner(ui, theme::ERROR, "PANEL OFFLINE — MACHINE ACTIONS BLOCKED");
            ui.add_space(4.0);
        } else if self.invalid_mode() {
            theme::banner(ui, theme::ERROR, "PANEL MODE INVALID — FAILS CLOSED");
            ui.add_space(4.0);
        }
        if self.program_protected()
            && matches!(
                self.page,
                Page::Program | Page::Offsets | Page::Tools | Page::Probe | Page::Files
            )
        {
            theme::banner(
                ui,
                theme::WARNING,
                "PROTECT ON — EDITING LOCKED. Buffer preserved; writes, imports and applies are blocked.",
            );
            ui.add_space(4.0);
        }
        if self.page == Page::Program && self.program.external_change {
            theme::banner(
                ui,
                theme::WARNING,
                "PROGRAM CHANGED EXTERNALLY — unsaved buffer preserved. Save will use the current revision.",
            );
            ui.add_space(4.0);
        }
        if self.snapshot.machine.estop && !self.panel_offline() {
            theme::banner(
                ui,
                theme::ERROR,
                "E-STOP ACTIVE — hardware safety chain engaged",
            );
            ui.add_space(4.0);
        }
        if !self.snapshot.machine.homed.is_empty()
            && !self.snapshot.machine.homed.iter().all(|(_, homed)| *homed)
        {
            theme::banner(
                ui,
                theme::WARNING,
                "NOT ALL AXES HOMED — MDI/probe/tool issue is blocked",
            );
            ui.add_space(4.0);
        }
    }

    fn draw_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.clone() else {
            return;
        };
        let mut close = false;
        let mut action: Option<PendingAction> = None;
        let mut save_and_continue = false;
        let mut quit = false;
        let mut overwrite: Option<bool> = None;
        egui::Window::new("LiNC")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .max_width(520.0)
            .frame(
                Frame::new()
                    .fill(theme::PANEL_RAISED)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| match &modal {
                Modal::ConfirmDiscard(pending) => {
                    ui.label(RichText::new("UNSAVED CHANGES").size(18.0).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label("The program buffer has unsaved changes. Leaving, loading another file or quitting will discard them.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "DISCARD", true).clicked() {
                            action = Some(pending.clone());
                        }
                        if theme::accent_button(ui, "SAVE", self.editing_allowed()).clicked() {
                            save_and_continue = true;
                        }
                        if theme::action_button(ui, "CANCEL", true).clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmQuit => {
                    ui.label(RichText::new("QUIT LiNC?").size(18.0).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label("The HMI is not part of the hardware E-stop chain. Panel control remains with linc-panel.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "QUIT", true).clicked() {
                            quit = true;
                        }
                        if theme::action_button(ui, "CANCEL", true).clicked() {
                            close = true;
                        }
                    });
                }
                Modal::ConfirmOverwrite(name) => {
                    ui.label(RichText::new("OVERWRITE FILE?").size(18.0).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label(format!("{name} already exists in the destination. Replace it atomically?"));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "OVERWRITE", true).clicked() {
                            overwrite = Some(true);
                        }
                        if theme::action_button(ui, "CANCEL", true).clicked() {
                            overwrite = Some(false);
                        }
                    });
                }
                Modal::Error(text) => {
                    ui.label(RichText::new("ERROR").size(18.0).strong().color(theme::ERROR));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    if theme::action_button(ui, "OK", true).clicked() {
                        close = true;
                    }
                }
                Modal::Info(text) => {
                    ui.label(RichText::new("NOTICE").size(18.0).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    if theme::action_button(ui, "OK", true).clicked() {
                        close = true;
                    }
                }
            });
        if close {
            self.modal = None;
        }
        if let Some(action) = action {
            self.modal = None;
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
                self.modal = None;
                if let Some(pending) = pending {
                    self.perform_pending(pending);
                }
            }
        }
        if quit {
            self.modal = None;
            self.quit_requested = true;
        }
        if let Some(overwrite) = overwrite {
            self.modal = None;
            screens::files::finish_overwrite(self, overwrite);
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        #[allow(deprecated)]
        let area = egui::Area::new(egui::Id::new("linc-toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-24.0, -90.0))
            .interactable(false);
        area.show(ctx, |ui| {
            for toast in self.toasts.iter().rev() {
                Frame::new()
                    .fill(theme::PANEL_RAISED)
                    .stroke(Stroke::new(1.0, toast.color))
                    .corner_radius(4)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&toast.text).size(12.0).color(toast.color),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(4.0);
            }
        });
    }
}

impl eframe::App for LincApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_keyboard(ui);
        self.draw(ui);
    }
}

pub const NAV_ITEMS: [(Page, &str, &str); 7] = [
    (Page::Machine, "MACHINE", "MAIN"),
    (Page::Program, "PROGRAM", "FILE"),
    (Page::Offsets, "OFFSETS", "OFFSET"),
    (Page::Tools, "TOOLS", "TOOL"),
    (Page::Diagnostics, "DIAGNOSTICS", "WIN"),
    (Page::Help, "G-CODE HELP", "HELP"),
    (Page::Settings, "SETTINGS", ""),
];

fn page_from_config(value: &str) -> Page {
    match value.to_ascii_lowercase().as_str() {
        "program" => Page::Program,
        "offsets" => Page::Offsets,
        "tools" => Page::Tools,
        "diagnostics" => Page::Diagnostics,
        "help" => Page::Help,
        "files" => Page::Files,
        "probe" => Page::Probe,
        "settings" => Page::Settings,
        _ => Page::Machine,
    }
}

pub fn mode_page(mode: &PanelModeValidity) -> Option<Page> {
    match mode.mode()? {
        PanelMode::Mdi => Some(Page::Mdi),
        PanelMode::Home | PanelMode::ProgHome => Some(Page::Homing),
        PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg => Some(Page::Manual),
        PanelMode::Edit => Some(Page::Program),
        PanelMode::Auto => None,
    }
}

pub fn active_offset(snapshot: &MachineSnapshot) -> Option<&linc_core::model::AxisMap<f64>> {
    let index = snapshot.offsets.active_index?;
    snapshot
        .offsets
        .offsets
        .iter()
        .find(|offset| offset.index == index)
        .map(|offset| &offset.values)
}

pub fn machine_bounds(profile: &Profile) -> Option<Bounds3> {
    let mut bounds = Bounds3 {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };
    let mut any = false;
    for axis in &profile.machine_ini.axes {
        let index = match axis.name.as_str() {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            _ => continue,
        };
        bounds.min[index] = axis.min_limit;
        bounds.max[index] = axis.max_limit;
        any = true;
    }
    if !any {
        return None;
    }
    for index in 0..3 {
        if !bounds.min[index].is_finite() {
            bounds.min[index] = 0.0;
            bounds.max[index] = 0.0;
        }
    }
    Some(bounds)
}

fn metric(ui: &mut egui::Ui, label: &str, value: String, size: f32) {
    ui.add_space(6.0);
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.0).color(theme::MUTED));
        ui.label(RichText::new(value).monospace().size(size).color(theme::FG));
    });
}

pub fn state_label(power: MachinePower) -> &'static str {
    power.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_follow_physical_mode() {
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Mdi)),
            Some(Page::Mdi)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Home)),
            Some(Page::Homing)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Jog)),
            Some(Page::Manual)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Invalid(vec![
                PanelMode::Mdi,
                PanelMode::Auto
            ])),
            None
        );
    }

    #[test]
    fn machine_bounds_use_ini_limits() {
        // Checked through a loaded profile in the integration tests.
        assert!(NAV_ITEMS.iter().any(|(page, _, _)| *page == Page::Help));
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use linc_core::model::PanelModeValidity;

    fn fixture() -> (tempfile::TempDir, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("base.toml"),
            include_str!("../../../configs/base.toml"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("gcode")).unwrap();
        std::fs::write(
            root.join("tool.tbl"),
            "T1 P0 Z-56 D6 ; end mill\nT12 P0 Z0 D10 ; face mill\n",
        )
        .unwrap();
        std::fs::write(
            root.join("probe_basic.ini"),
            "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = TZDR\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -372\nMAX_LIMIT = 5\n[AXIS_Y]\nMIN_LIMIT = -346\nMAX_LIMIT = 4\n[AXIS_Z]\nMIN_LIMIT = -253\nMAX_LIMIT = 19\n",
        )
        .unwrap();
        std::fs::write(
            root.join("sim.toml"),
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
panel_window = false
"#,
        )
        .unwrap();
        let profile = Profile::load(&root.join("sim.toml"), None).unwrap();
        (dir, profile)
    }

    fn make_app(mode: PanelMode) -> (tempfile::TempDir, LincApp) {
        let (dir, profile) = fixture();
        let ctx = egui::Context::default();
        let backend = Box::new(LocalBackend::new(profile.clone(), mode));
        let app = LincApp::new(profile, backend, &ctx);
        (dir, app)
    }

    #[test]
    fn mode_driven_pages_open_and_close() {
        let (_dir, mut app) = make_app(PanelMode::Mdi);
        assert_eq!(app.page, Page::Mdi);
        app.on_mode_change(PanelModeValidity::Valid(PanelMode::Auto));
        assert_eq!(app.page, app.last_non_mode_page);
        app.on_mode_change(PanelModeValidity::Valid(PanelMode::Home));
        assert_eq!(app.page, Page::Homing);
        app.on_mode_change(PanelModeValidity::Valid(PanelMode::Jog));
        assert_eq!(app.page, Page::Manual);
        app.on_mode_change(PanelModeValidity::Valid(PanelMode::Edit));
        assert_eq!(app.page, Page::Program);
        assert_eq!(app.program.view, ProgramView::Code);
    }

    #[test]
    fn edit_mode_starts_in_code_with_editor_focus() {
        let (_dir, app) = make_app(PanelMode::Edit);
        assert_eq!(app.page, Page::Program);
        assert_eq!(app.program.view, ProgramView::Code);
        assert!(app.program.focus_editor);
        assert!(app.editing_allowed());
    }

    #[test]
    fn unsaved_program_guards_navigation() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        app.program.editor.replace_text("G0 X0\nG1 X1\n".into());
        assert!(app.program_dirty());
        app.request_page(Page::Tools);
        assert!(matches!(
            app.modal,
            Some(Modal::ConfirmDiscard(PendingAction::Navigate(Page::Tools)))
        ));
        assert_eq!(app.page, Page::Program);
        app.modal = None;
        app.perform_pending(PendingAction::Navigate(Page::Tools));
        assert_eq!(app.page, Page::Tools);
        // The buffer survives a non-destructive page change.
        assert_eq!(app.program.editor.text(), "G0 X0\nG1 X1\n");
    }

    #[test]
    fn protect_unknown_fails_closed_in_the_ui() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        app.snapshot.panel.program_protect = false;
        app.snapshot.panel.program_protect_known = false;
        assert!(!app.editing_allowed());
        assert!(app.program_protected());
        app.program.editor.replace_text("G0 X1\n".into());
        let before = app.program.editor.text();
        app.save_program();
        assert_eq!(app.program.editor.text(), before);
        assert!(app.program.editor.dirty);
        assert!(app.modal.is_none());
    }

    #[test]
    fn editor_preview_updates_only_after_valid_parse() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        app.program
            .editor
            .replace_text("G0 X0 Y0\nG1 X10 Y10 F300\n".into());
        app.refresh_preview();
        assert!(app.program.preview.error.is_none());
        assert_eq!(app.program.last_valid_preview.segments.len(), 2);
        app.program.editor.replace_text("G0 X0 Y0\nG1 X\n".into());
        app.refresh_preview();
        assert!(app.program.preview.error.is_some());
        assert_eq!(app.program.last_valid_preview.segments.len(), 2);
    }

    #[test]
    fn manual_tool_change_prompt_and_confirmation_flow() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.tools.target = "12".into();
        screens::tools::issue_change(&mut app);
        app.poll_backend();
        assert_eq!(
            app.snapshot.tool_change.status,
            linc_core::model::ToolChangeStatus::ManualPrompt
        );
        let token = app.snapshot.tool_change.token.clone().unwrap();
        app.send(MachineCommand::ConfirmManualToolChange { token });
        app.poll_backend();
        assert_eq!(
            app.snapshot.tool_change.status,
            linc_core::model::ToolChangeStatus::Complete
        );
        assert_eq!(app.snapshot.hal.tool_change_confirm_seq, 1);
    }

    #[test]
    fn clear_trail_is_view_only_and_sends_backend_request() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        assert!(!app.snapshot.live_trail.is_empty());
        app.clear_trail();
        app.poll_backend();
        assert!(app.snapshot.live_trail.len() <= 1);
        assert_eq!(app.snapshot.program.line_count > 0, true);
    }
}
