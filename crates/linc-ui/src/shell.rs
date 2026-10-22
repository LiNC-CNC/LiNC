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

/// Lines a PageUp/PageDown moves the selection in a read-only G-code view.
const LINE_PAGE: isize = 10;

pub struct LincApp {
    /// Publishes state for `linc-mcp`. `None` in tests and when the diagnostic
    /// socket could not be created.
    pub diag_bus: Option<std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>>,
    /// Backend sample stamp of the last published machine snapshot.
    diag_last_sample: u64,
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
    pub design_scale: f32,
    pub page_focus_requested: bool,
    pub modal: Option<Modal>,
    pub toasts: Vec<Toast>,
    pub quit_requested: bool,
    pub last_snapshot_ms: u64,
    /// Highest alarm id the operator has been told about. Controller errors used
    /// to appear only in the Machine page's message card, so a program that
    /// aborted looked like it had simply jumped to the end.
    pub alarm_id_seen: u64,
    /// Last jog speed handed to the HAL, so RAPID and the slider publish once
    /// instead of on every poll.
    pub jog_speed_published: Option<f64>,
    /// The work-zero cross, in machine coordinates. Held rather than recomputed
    /// each frame: while a program loads the snapshot can report no active
    /// system for a poll, and the cross would snap to the machine origin and
    /// back.
    pub graph_origin: [f64; 3],
    /// Largest remaining distance (absolute) seen for each axis since it last
    /// stopped, i.e. the length of the move in progress. The status only carries
    /// the remainder, so this is what turns it into a percentage.
    /// Set by `clear_trail`: the backend applies the clear on its next
    /// conversion, and until it does its snapshot still carries the old points.
    /// While set, those are replaced by the cleared tip so the trail cannot flash
    /// back for a frame.
    pub trail_cleared: bool,
    /// Tool tip at the moment of the clear, to tell a stale trail from new
    /// motion.
    pub trail_cleared_tip: Option<[f64; 3]>,
    pub move_distance: linc_core::model::AxisMap<f64>,
    /// 0..1 of the current move already travelled, per axis.
    pub move_progress: linc_core::model::AxisMap<f64>,
    /// Whether a modal was open last frame, so a newly opened one can put the
    /// keyboard on its default button.
    pub modal_open_last_frame: bool,
    /// Same, for the manual tool-change prompt, which is driven by the machine
    /// rather than by `self.modal`.
    pub tool_prompt_open_last_frame: bool,
    pub last_connection: Option<bool>,
    pub last_protect: Option<bool>,
    pub panel_offline_toast: bool,
    pub preview_ticks: u64,
    pub confirm_token: Option<linc_core::model::ToolChangeToken>,
    /// A program load is in flight; navigation waits for the backend verdict.
    pub pending_load: bool,
    /// Resolved `[keys]` bindings. The profile owns the keymap; the shell
    /// consumes it instead of hardcoding keys.
    pub keymap: crate::state::Keymap,
    /// Runtime UI preferences (CONFIGURATION.md §9). Machine identity, HAL
    /// pins, safety flags and the INI path are never stored here.
    pub preferences: linc_core::preferences::Preferences,
    /// Navigation entries the profile shows, in display order.
    pub nav: Vec<Page>,
    /// The finite steps from `[DISPLAY] INCREMENTS`, without the continuous
    /// `JOG` entry: continuous is selected by the panel's JOG mode.
    pub jog_increments: Vec<(String, f64)>,
    /// Index into `jog_increments`.
    pub jog_increment_index: usize,
    /// Fixed at 100: the feed override is what scales the jog now, so there is
    /// no second control to keep in step with it.
    pub jog_speed_percent: f64,
    preferences_dirty: bool,
    preferences_saved_at: std::time::Instant,
}

impl LincApp {
    /// Publish the latest state for the diagnostic server.
    ///
    /// A new backend sample is published at once; between samples the bus is
    /// refreshed at 5 Hz so `age_ms` stays small and a waiting agent can rely on
    /// it, without cloning the snapshot on every repaint.
    fn publish_diag(&mut self) {
        let Some(bus) = &self.diag_bus else { return };
        let Ok(mut bus) = bus.lock() else { return };
        let sample = self.snapshot.meta.captured_at_ms;
        let stale = linc_core::diag::now_ms() - bus.captured_at_ms() >= 200;
        if sample != self.diag_last_sample || stale {
            self.diag_last_sample = sample;
            bus.publish_machine(self.snapshot.clone());
        }
        bus.publish_ui(self.diag_ui_snapshot());
    }

    /// Answer a pending `diag.screenshot`.
    ///
    /// egui delivers the capture through a callback after the frame is painted,
    /// which is the only point where the rendered image exists: reading the
    /// window's frame buffer during layout returns the cleared buffer.
    fn capture_screenshot(&self, ctx: &egui::Context) {
        let Some(bus) = &self.diag_bus else { return };
        let requested = bus
            .lock()
            .map(|mut bus| bus.take_screenshot_request())
            .unwrap_or(false);
        if !requested {
            return;
        }
        let bus = bus.clone();
        ctx.request_screenshot(move |image| {
            let Some(screenshot) = png_from_color_image(&image) else {
                log::warn!("screenshot captured but could not be encoded");
                return;
            };
            if let Ok(mut bus) = bus.lock() {
                bus.put_screenshot(screenshot);
            }
        });
    }

    /// The UI-side state an agent needs; mirrors `linc_core::diag::UiSnapshot`.
    fn diag_ui_snapshot(&self) -> linc_core::diag::UiSnapshot {
        use linc_core::diag::UiSnapshot;
        let layout = match self.profile.variant {
            linc_core::model::MachineVariant::Lathe => "lathe",
            linc_core::model::MachineVariant::Mill => "mill",
        };
        // The controller mode is what the top bar shows; a panel mode that has
        // not decoded yet must not hide it.
        let mode = format!("{:?}", self.snapshot.task.mode);
        UiSnapshot {
            page: self.page.label().into(),
            layout: layout.into(),
            mode,
            program_view: match self.program.view {
                crate::state::ProgramView::Split => "SPLIT".into(),
                crate::state::ProgramView::Code => "CODE".into(),
            },
            view: self.program.graph_view.label().into(),
            zoom: self.program.graph_zoom,
            selected_line: (self.program.selected_line > 0).then_some(self.program.selected_line),
            current_line: {
                let line = self.snapshot.program.executing_line();
                (line > 0).then_some(line)
            },
            selected_axis: None,
            program_name: self.snapshot.program.display_name.clone(),
            program_dirty: self.program.editor.dirty,
            panel_online: self.snapshot.panel.connected,
            toasts: self.toasts.iter().map(|toast| toast.text.clone()).collect(),
        }
    }

    /// Attach the diagnostic bus the app server publishes into.
    pub fn set_diag_bus(
        &mut self,
        bus: std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>,
    ) {
        self.diag_bus = Some(bus);
    }

    pub fn new(profile: Profile, mut backend: Box<dyn UiBackend>, ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        theme::apply_design_zoom(ctx, profile.ui_scale().unwrap_or(1.0));
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
        // Runtime preferences are separate from the machine profile. They may
        // restore the last page and view, never machine data.
        let preferences = linc_core::preferences::Preferences::load(&profile.preferences_path);
        let remembered_page = preferences
            .last_page
            .as_deref()
            .map(page_from_config)
            .filter(|page| !page.is_mode_driven());
        let initial_page = if initial_page.is_mode_driven() {
            initial_page
        } else {
            remembered_page.unwrap_or(initial_page)
        };
        let program_view = preferences
            .program_view
            .as_deref()
            .and_then(|value| match value {
                "code" => Some(ProgramView::Code),
                "split" => Some(ProgramView::Split),
                _ => None,
            });
        let graph_view = preferences
            .toolpath_view
            .as_deref()
            .map(|value| view_from_config(value, profile.variant == MachineVariant::Lathe));
        let graph_zoom = preferences.toolpath_zoom;
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
                cursor_line: None,
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
            design_scale: configured_scale,
            page_focus_requested: true,
            modal: None,
            toasts: Vec::new(),
            diag_bus: None,
            diag_last_sample: 0,
            quit_requested: false,
            pending_load: false,
            keymap: crate::state::Keymap::from_config(&profile.config.keys),
            nav: nav_pages(&profile.config.ui.pages),
            jog_increments: crate::state::finite_increments(&profile.machine_ini.increments),
            jog_increment_index: preferences.jog_increment.unwrap_or(0),
            jog_speed_percent: 100.0,
            preferences,
            preferences_dirty: false,
            preferences_saved_at: std::time::Instant::now(),
            last_snapshot_ms: now_ms(),
            alarm_id_seen: 0,
            jog_speed_published: None,
            graph_origin: [0.0; 3],
            trail_cleared: false,
            trail_cleared_tip: None,
            move_distance: linc_core::model::AxisMap::new(),
            move_progress: linc_core::model::AxisMap::new(),
            modal_open_last_frame: false,
            tool_prompt_open_last_frame: false,
            last_connection: Some(snapshot.panel.connected),
            last_protect: Some(snapshot.panel.protect_effective()),
            panel_offline_toast: false,
            preview_ticks: 0,
            confirm_token: None,
            profile,
            backend,
        };
        // Restore the remembered view state. Only UI presentation is restored;
        // no machine data ever comes from preferences.
        if let Some(view) = graph_view {
            app.program.graph_view = view;
            app.manual.view = view;
        }
        if let Some(zoom) = graph_zoom.filter(|zoom| zoom.is_finite() && *zoom > 0.0) {
            app.program.graph_zoom = zoom;
            app.manual.zoom = zoom;
        }
        if let Some(view) = program_view {
            app.program.view = view;
        }
        // Publish the jog selections once at startup: the HAL nets for them
        // only exist after the GUI creates its component, and a pin that is
        // never written would leave jogging at zero speed.
        for (output, value) in [
            (
                linc_core::command::LincOutput::JogIncrement,
                linc_core::command::HalValue::Float(app.jog_increment_value()),
            ),
            (
                linc_core::command::LincOutput::JogSpeed,
                linc_core::command::HalValue::Float(app.jog_speed_output()),
            ),
        ] {
            app.send(linc_core::command::MachineCommand::SetHalOutput { output, value });
        }
        // Probe Basic's contract expects the probe tool in `#3014` to be 99.
        // A different value silently changes which tool the probe cycles assume
        // is mounted, so say so once at startup rather than at the first cycle.
        if app.profile.config.probe.enabled
            && app.profile.config.probe.tool_number != linc_core::probe::EXPECTED_PROBE_TOOL
        {
            let configured = app.profile.config.probe.tool_number;
            let expected = linc_core::probe::EXPECTED_PROBE_TOOL;
            log::warn!(
                "probe tool number is {configured}, expected {expected} (#3014)"
            );
            app.toast(
                format!("Probe tool is T{configured}, expected T{expected}"),
                theme::WARNING,
            );
            app.log_ui(
                AlarmSeverity::Warning,
                AlarmSource::Ui,
                "PROBE-TOOL",
                format!("probe tool is T{configured}, expected T{expected}"),
            );
        }
        // Seed the preference with the page we actually opened, so the file is
        // written even if the operator never navigates.
        let opening_page = app.page;
        app.remember_page(opening_page);
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

    /// Send without polling the backend back.
    ///
    /// `send` polls, so anything called from `poll_backend` must use this or it
    /// recurses until the stack overflows.
    fn send_quiet(&mut self, command: MachineCommand) {
        if let Err(error) = self.backend.send(command) {
            self.log_ui(
                AlarmSeverity::Error,
                AlarmSource::Ui,
                "UI-CMD",
                error.to_string(),
            );
        }
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

    /// Turn each axis's remaining distance into a fraction of its current move.
    fn update_move_progress(&mut self) {
        let mut updates = Vec::new();
        for (axis, position) in self.snapshot.positions.axes.iter() {
            let total = self.move_distance.get(axis).copied().unwrap_or(0.0);
            let (fraction, total) = move_progress(position.dtg, total);
            updates.push((axis.to_string(), total, fraction));
        }
        for (axis, total, fraction) in updates {
            self.move_distance.insert(axis.clone(), total);
            self.move_progress.insert(axis, fraction);
        }
    }

    pub fn poll_backend(&mut self) {
        let mut snapshot = self.backend.snapshot();
        if self.trail_cleared {
            if cleared_trail_pending(&snapshot, self.trail_cleared_tip) {
                let tip = snapshot.tool_tip.unwrap_or([0.0; 3]);
                snapshot.live_trail = std::sync::Arc::new(vec![tip]);
            } else {
                self.trail_cleared = false;
            }
        }
        self.snapshot = snapshot;
        // Keep the last known work zero: loading a program can leave the
        // snapshot without an active system for a poll, and the cross must not
        // jump to the machine origin when that happens.
        if let Some(offset) = active_offset(&self.snapshot) {
            self.graph_origin = [
                offset.get("X").copied().unwrap_or(0.0),
                offset.get("Y").copied().unwrap_or(0.0),
                offset.get("Z").copied().unwrap_or(0.0),
            ];
        }
        self.update_move_progress();
        // A touch-off or a DRO entry moves the origin the preview is drawn
        // against. The backends bump `offsets.revision` on a write.
        if self.snapshot.offsets.revision != self.program.preview_offsets_seen {
            self.refresh_preview();
        }
        // RAPID changes the jog ceiling, so the published speed follows it.
        // Compared against what we last sent, not against the snapshot: the
        // snapshot is rebuilt every poll, and comparing with it published on
        // every frame and starved the controller.
        let wanted = self.jog_speed_value();
        if self
            .jog_speed_published
            .map(|value| (value - wanted).abs() > 1e-9)
            .unwrap_or(true)
        {
            self.jog_speed_published = Some(wanted);
            self.send_quiet(linc_core::command::MachineCommand::SetHalOutput {
                output: linc_core::command::LincOutput::JogSpeed,
                value: linc_core::command::HalValue::Float(wanted * 60.0),
            });
        }
        // Tell the operator about controller errors wherever they are. An
        // aborted program used to leave the console as the only witness.
        let newest = self
            .snapshot
            .alarms
            .iter()
            .map(|alarm| alarm.id)
            .max()
            .unwrap_or(0);
        if self.alarm_id_seen == 0 {
            // First poll: the controller may already hold a backlog. Show it in
            // the message strip, but do not fire a toast for every entry.
            self.alarm_id_seen = newest;
        } else if newest > self.alarm_id_seen {
            let fresh: Vec<linc_core::model::Alarm> = self
                .snapshot
                .alarms
                .iter()
                .filter(|alarm| alarm.id > self.alarm_id_seen)
                .cloned()
                .collect();
            for alarm in fresh {
                let colour = match alarm.severity {
                    linc_core::model::AlarmSeverity::Critical
                    | linc_core::model::AlarmSeverity::Error => theme::ERROR,
                    linc_core::model::AlarmSeverity::Warning => theme::WARNING,
                    linc_core::model::AlarmSeverity::Info => theme::MUTED,
                };
                self.toast(alarm.message, colour);
            }
            self.alarm_id_seen = newest;
        }
        let mut loaded = false;
        let mut load_failed = false;
        self.last_snapshot_ms = now_ms();
        let events = self.backend.drain_events();
        for event in events {
            match event {
                BackendEvent::Command(CommandResult {
                    command, result, ..
                }) => match result {
                    Ok(()) => {
                        if command == "Load program" {
                            loaded = true;
                        }
                        self.log_ui(
                            AlarmSeverity::Info,
                            AlarmSource::Ui,
                            "UI-OK",
                            format!("{command} accepted"),
                        );
                    }
                    Err(error) => {
                        if command == "Load program" {
                            load_failed = true;
                        }
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
        if self.pending_load {
            if loaded {
                self.commit_load();
            } else if load_failed {
                // Stay in Files so the operator sees the error next to the file.
                self.pending_load = false;
            }
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

    /// Why "run from line" is unavailable, or `None` when it may run.
    ///
    /// A line number is the one program-start input the panel cannot supply,
    /// so the HMI issues it; everything else that guards a program start is
    /// still checked here rather than assumed.
    pub fn run_from_line_block(&self) -> Option<&'static str> {
        if self.snapshot.program.line_count == 0 {
            return Some("no program is loaded");
        }
        if self.snapshot.task.running || self.snapshot.task.interp != linc_core::model::InterpState::Idle {
            return Some("abort the running program first");
        }
        if self.snapshot.machine.estop {
            return Some("emergency stop is active");
        }
        if self.snapshot.task.power != linc_core::model::MachinePower::On {
            return Some("machine is not ON");
        }
        if self.snapshot.panel.mode.mode() != Some(PanelMode::Auto) {
            return Some("select AUTO on the panel first");
        }
        if self.panel_offline() || self.snapshot.meta.stale {
            return Some("panel state is not current");
        }
        if self
            .axes()
            .iter()
            .any(|axis| !self.snapshot.machine.homed.get(axis).copied().unwrap_or(false))
        {
            return Some("home every axis first");
        }
        None
    }

    /// Ask to run from the current line: opens the confirmation, or explains
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
        let selected = (self.program.selected_line as isize + step).clamp(1, count as isize) as usize;
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
        let line = self.snapshot.program.current_line.max(1) as u32;
        let block = self
            .snapshot
            .program
            .content
            .as_deref()
            .and_then(|text| text.lines().nth(line as usize - 1))
            .unwrap_or_default()
            .trim()
            .to_string();
        self.modal = Some(crate::state::Modal::ConfirmRunFromLine { line, block });
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
        self.remember_page(page);
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
        self.page_focus_requested = true;
        if page == Page::Tools {
            // The table takes the keyboard as soon as the page opens: nothing
            // else on the page holds focus, and the cursor starts at the first
            // editable cell of the selected row.
            self.tools.editing = false;
            self.tools.edit_buffer.clear();
            if self.tools.selected_column == 0 {
                self.tools.selected_column = 1;
            }
        }
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
        // Do not navigate yet: SPEC §6.7 requires that a failed load stays in
        // Files and shows the error. The navigation is committed in
        // `poll_backend` when the backend confirms the load.
        self.pending_load = true;
    }

    /// A successful load always returns to Program in SPLIT, fitted and not
    /// started, overriding any remembered layout.
    fn commit_load(&mut self) {
        self.pending_load = false;
        if let Some(path) = self.snapshot.program.path.clone() {
            let text = path.to_string_lossy().to_string();
            self.preferences.remember_file(&text, 12);
            self.preferences_dirty = true;
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
        self.set_page(Page::Program);
    }

    /// Remember the last operator-selected, non-mode-driven page.
    fn remember_page(&mut self, page: Page) {
        if page.is_mode_driven() {
            return;
        }
        let key = page_key(page);
        if self.preferences.last_page.as_deref() != Some(key) {
            self.preferences.last_page = Some(key.to_string());
            self.preferences_dirty = true;
        }
    }

    fn remember_program_view(&mut self, view: ProgramView) {
        let key = match view {
            ProgramView::Split => "split",
            ProgramView::Code => "code",
        };
        if self.preferences.program_view.as_deref() != Some(key) {
            self.preferences.program_view = Some(key.to_string());
            self.preferences_dirty = true;
        }
    }

    /// Flush preferences at most every two seconds while running, so a crash
    /// cannot lose more than a couple of seconds of UI state.
    pub fn save_preferences(&mut self, force: bool) {
        if !self.preferences_dirty {
            return;
        }
        if !force && self.preferences_saved_at.elapsed() < Duration::from_secs(2) {
            return;
        }
        match self.preferences.save(&self.profile.preferences_path) {
            Ok(()) => {
                self.preferences_dirty = false;
                self.preferences_saved_at = std::time::Instant::now();
            }
            Err(error) => {
                log::warn!("cannot write preferences: {error}");
                self.preferences_dirty = false;
            }
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
            self.last_connection = Some(self.snapshot.panel.connected);
        }
        let protect = self.snapshot.panel.protect_effective();
        if self.last_protect != Some(protect) {
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
                // Selecting AUTO on the panel shows the program, as Probe
                // Basic does: the operator watches the active line and the
                // running code without hunting for the page. This runs on the
                // mode change only, so navigating away afterwards is left
                // alone.
                self.page = Page::Program;
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
        // Only while a program is actually running: otherwise a stale
        // follow flag dragged the view back to line 1 every frame and fought
        // the operator's own cursor movement.
        // Follow the line the tool is on. `current_line` is the interpreter's
        // read-ahead and would drag the pane to the end of the program while the
        // machine is still cutting the first move.
        let executed = self.snapshot.program.executing_line();
        if self.program.follow_current
            && self.snapshot.task.running
            && executed > 0
            && executed != self.program.selected_line
        {
            self.program.selected_line = executed;
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
        self.offsets.rows.clear();
        for offset in &self.snapshot.offsets.offsets {
            let mut draft = crate::state::OffsetDraft {
                values: std::collections::HashMap::new(),
                rotation: format!("{:.4}", offset.rotation.unwrap_or(0.0)),
            };
            for (axis, value) in offset.values.iter() {
                draft.values.insert(axis.to_string(), format!("{value:.4}"));
            }
            self.offsets.rows.insert(offset.index, draft);
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
        self.tools.drafts.clear();
        for row in &self.snapshot.tool_table.rows {
            self.tools
                .drafts
                .insert(row.tool, crate::state::ToolDraft::from_row(row));
        }
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

    /// The finite steps the INI declares.
    pub fn increments(&self) -> &[(String, f64)] {
        &self.jog_increments
    }

    pub fn jog_increment_index(&self) -> usize {
        self.jog_increment_index.min(self.increments().len().saturating_sub(1))
    }

    /// Selected increment in machine units; 0.0 means continuous jog.
    pub fn jog_increment_value(&self) -> f64 {
        crate::state::jog_increment(self.increments(), self.jog_increment_index()).1
    }

    pub fn jog_increment_label(&self) -> Option<String> {
        crate::state::jog_increment(self.increments(), self.jog_increment_index()).0.into()
    }

    /// Publish the selection to `linc.jog-increment` when it changes.
    pub fn set_jog_increment_index(&mut self, index: usize) {
        let index = index.min(self.increments().len().saturating_sub(1));
        if index == self.jog_increment_index {
            return;
        }
        self.jog_increment_index = index;
        self.preferences.jog_increment = Some(index);
        self.preferences_dirty = true;
        let value = self.jog_increment_value();
        self.send(linc_core::command::MachineCommand::SetHalOutput {
            output: linc_core::command::LincOutput::JogIncrement,
            value: linc_core::command::HalValue::Float(value),
        });
    }

    /// Jog speed in machine units per second.
    ///
    /// The ceiling is the profile's `[jog] max_speed` in units per minute,
    /// switched to `rapid_speed` while the panel's RAPID key is held, and
    /// scaled by the operator's percentage. The machine still clamps to its own
    /// MAX_VELOCITY.
    /// The value for `halui.axis.jog-speed`.
    ///
    /// This build reads the pin as machine units per **minute**: 50 written to
    /// it jogs at 0.83 mm/s, which is what the operator saw as "1 mm/s". The
    /// page keeps thinking in units per second (`jog_speed_value`), and the
    /// conversion happens here so there is one place to change.
    pub fn jog_speed_output(&self) -> f64 {
        self.jog_speed_value() * 60.0
    }

    pub fn jog_speed_value(&self) -> f64 {
        let ceiling = if self.snapshot.panel.rapid {
            self.profile.config.jog.rapid_speed
        } else {
            self.profile.config.jog.max_speed
        };
        // The feed override scales the jog, as probe_basic does with its
        // `mult2.jog-speed` (feed override * 100 -> jog speed percentage).
        let percent = self.jog_speed_percent * self.snapshot.panel.feed_override;
        crate::state::jog_speed(ceiling / 60.0, percent)
    }

    /// The tool tip in machine coordinates, matching the preview and the trail.
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
        // The plot is drawn in machine space with the work offset applied, so the
        // part sits where the work zero is on the machine. Which system: the one
        // the program selects (a `G54`/`G55` in its preamble is where it will
        // cut), falling back to the controller's active system for a program that
        // selects none.
        let offset = self.preview_offset(&text);
        let preview = parse_preview(&text, &axes, offset.as_ref(), self.tool_tip());
        let valid = preview.error.is_none();
        if valid {
            self.program.last_valid_preview = preview.clone();
        }
        self.program.preview = preview;
        self.program.preview_offsets_seen = self.snapshot.offsets.revision;
    }

    /// The work offset the preview is drawn with, in machine coordinates: the
    /// system the program selects, else the controller's active one.
    fn preview_offset(&self, text: &str) -> Option<linc_core::model::AxisMap<f64>> {
        let index = linc_core::gcode::program_wcs(text)
            .or(self.snapshot.offsets.active_index)?;
        self.snapshot
            .offsets
            .offsets
            .iter()
            .find(|row| row.index == index)
            .map(|row| row.values.clone())
            .or_else(|| self.active_offset().cloned())
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

    /// Cycle the toolpath view through ISO, X, Y and Z, refitting each time so
    /// the new projection frames the program instead of inheriting a zoom that
    /// only suited the old one.
    pub fn cycle_toolpath_view(&mut self) {
        let next = linc_view::cycle_view(self.program.graph_view, self.profile.variant);
        self.program.graph_view = next;
        self.program.fit_requested = true;
        // The manual and homing pages keep their own view state; keep it in step
        // so the key does the same thing wherever a toolpath is on screen.
        self.manual.view = next;
        self.manual.fit = true;
    }

    /// Fit the program toolpath: reset zoom/pan and ask the view to reframe.
    pub fn fit_graph(&mut self) {
        // The manual modes draw their own view state.
        if matches!(self.page, Page::Manual | Page::Homing) {
            self.manual.zoom = 1.0;
            self.manual.pan = Vec2::ZERO;
            self.manual.fit = true;
        } else {
            self.program.graph_zoom = 1.0;
            self.program.graph_pan = Vec2::ZERO;
            self.program.fit_requested = true;
        }
    }

    pub fn clear_trail(&mut self) {
        // View-only action. The backend is not asked to change machine state;
        // the live trail is re-seeded at the current tool tip in the local view.
        // The backend applies the clear on its next conversion, so the next few
        // snapshots still carry the old points; `trail_cleared` masks them until
        // either that clear lands or the tool moves.
        let tip = self.tool_tip();
        self.snapshot.live_trail = std::sync::Arc::new(vec![tip]);
        self.snapshot.tool_tip = Some(tip);
        self.trail_cleared = true;
        self.trail_cleared_tip = Some(tip);
        self.send(MachineCommand::ClearTrail);
        self.toast("live trail cleared", theme::MUTED);
    }

    pub fn focus_nav(&mut self) {
        self.focus_region = FocusRegion::Nav;
        self.nav_focus = self.nav.iter().position(|page| *page == self.page).unwrap_or(0);
    }

    pub fn handle_keyboard(&mut self, ui: &mut egui::Ui) {
        // A text field keeps Home, End and the page keys while it holds the
        // keyboard; they are cursor movement there and focus navigation here.
        // Add every text field that can hold focus to this list.
        let text_focused = ui.memory(|memory| {
            memory.has_focus(egui::Id::unique("linc-program-editor"))
                || memory.has_focus(egui::Id::unique("linc-tool-cell"))
                || memory.has_focus(egui::Id::unique("linc-offsets-dro"))
        });
        let mut nav_page = None;
        let mut escape = false;
        let mut quit = false;
        let mut request_files = false;
        let mut save = false;
        let mut cycle_view = false;
        let mut graph_fit = false;
        let mut graph_clear = false;
        let mut run_from_line = false;
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
        let mut duplicate_line = false;
        let mut delete_line = false;
        let mut move_line_up = false;
        let mut move_line_down = false;
        let mut line_step = 0isize;
        let mut goto_line = false;
        let mut cycle_toolpath_view = false;

        ui.input_mut(|input| {
            if input.consume_key(self.keymap.machine.modifiers, self.keymap.machine.key) {
                nav_page = Some(Page::Machine);
            }
            if input.consume_key(self.keymap.program.modifiers, self.keymap.program.key) {
                // The key is labelled FILE: it opens the program, and opens the
                // file browser when the program is already in front. CODE/SPLIT
                // is a separate command on the square key.
                if self.page == Page::Program {
                    request_files = true;
                } else {
                    nav_page = Some(Page::Program);
                }
            }
            // Page-local: CODE/SPLIT only exists where there is a program view.
            // Shortcuts that mean nothing on the page in front of the operator
            // are not consumed, so they cannot shadow another page's command.
            if self.page == Page::Program
                && input.consume_key(
                    self.keymap.program_cycle_view.modifiers,
                    self.keymap.program_cycle_view.key,
                )
            {
                cycle_view = true;
            }
            // Page-local to the pages that draw a toolpath.
            if matches!(self.page, Page::Program | Page::Manual | Page::Homing) {
                if input.consume_key(self.keymap.graph_fit.modifiers, self.keymap.graph_fit.key) {
                    graph_fit = true;
                }
                if input.consume_key(self.keymap.graph_clear.modifiers, self.keymap.graph_clear.key)
                {
                    graph_clear = true;
                }
                if input.consume_key(
                    self.keymap.program_run_from_line.modifiers,
                    self.keymap.program_run_from_line.key,
                ) {
                    run_from_line = true;
                }
            }
            if input.consume_key(self.keymap.tools.modifiers, self.keymap.tools.key) {
                nav_page = Some(Page::Tools);
            }
            if input.consume_key(self.keymap.offsets.modifiers, self.keymap.offsets.key) {
                nav_page = Some(Page::Offsets);
            }
            if input.consume_key(self.keymap.diagnostics.modifiers, self.keymap.diagnostics.key) {
                nav_page = Some(Page::Diagnostics);
            }
            if input.consume_key(self.keymap.quit.modifiers, self.keymap.quit.key) {
                quit = true;
            }
            // Not on Program: FILE is the way to the file browser there, and a
            // second key for the same destination only crowds the hint row.
            if self.page != Page::Program
                && input.consume_key(self.keymap.open.modifiers, self.keymap.open.key)
            {
                request_files = true;
            }
            if input.consume_key(self.keymap.save.modifiers, self.keymap.save.key) {
                save = true;
            }
            if input.consume_key(self.keymap.find.modifiers, self.keymap.find.key) {
                find = true;
            }
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
            if input.consume_key(self.keymap.refresh.modifiers, self.keymap.refresh.key) {
                refresh = true;
            }
            if input.consume_key(self.keymap.undo.modifiers, self.keymap.undo.key) {
                undo = true;
            }
            if input.consume_key(self.keymap.redo.modifiers, self.keymap.redo.key) {
                redo = true;
            }
            // An edit owns Escape: it cancels the edit rather than leaving the
            // page.
            let editing_in_place = (self.page == Page::Tools && self.tools.editing)
                || (self.page == Page::Offsets
                    && (self.offsets.editing || self.offsets.dro_edit.is_some()));
            if !editing_in_place
                && input.consume_key(self.keymap.back.modifiers, self.keymap.back.key)
            {
                escape = true;
            }
            // A read-only G-code pane has no text cursor, so the arrows and the
            // page keys move the selected line there; an editable pane leaves
            // them to the editor.
            if self.page == Page::Program
                && !self.editing_allowed()
                && self.focus_region == FocusRegion::Page
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
            if self.page == Page::Program
                && input.consume_key(self.keymap.goto_line.modifiers, self.keymap.goto_line.key)
            {
                goto_line = true;
            }
            // Every page with a toolpath: ISO -> X -> Y -> Z. F10 on its own
            // already toggles SPLIT/CODE on the Program page.
            if matches!(
                self.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing
            ) && input.consume_key(self.keymap.view_cycle.modifiers, self.keymap.view_cycle.key)
            {
                cycle_toolpath_view = true;
            }
            // The tool table pages its own cursor with these keys, so they must
            // reach the page instead of being consumed for tab navigation.
            let page_keys_belong_to_page = self.page == Page::Tools;
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
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft) {
                focus_left = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight) {
                focus_right = true;
            }
            if input.consume_key(self.keymap.help_search.modifiers, self.keymap.help_search.key) {
                help_search = true;
            }
            // The page test comes first: `consume_key` swallows the key whether
            // or not the rest of the condition holds, and this binding is plain
            // Enter, so on every other page it ate the key that opens a file.
            if self.page == Page::Mdi
                && self.profile.config.mdi.execute_on_enter
                && input.consume_key(self.keymap.mdi_issue.modifiers, self.keymap.mdi_issue.key)
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
        if cycle_toolpath_view {
            self.cycle_toolpath_view();
        }
        if goto_line && self.page == Page::Program {
            self.modal = Some(crate::state::Modal::GoToLine {
                buffer: String::new(),
            });
        }
        if line_step != 0 && self.page == Page::Program {
            self.step_selected_line(line_step);
        }
        if save && self.page == Page::Program {
            self.save_program();
        }
        if refresh && self.page == Page::Files {
            self.files.refresh();
        }
        if self.page == Page::Program {
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
        if run_from_line {
            self.request_run_from_line();
        }
        if graph_fit {
            self.fit_graph();
        }
        if graph_clear {
            self.clear_trail();
        }
        if cycle_view {
            self.cycle_program_view();
            let view = self.program.view;
            self.remember_program_view(view);
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
            self.page_focus_requested = true;
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
                    self.nav_focus = (self.nav_focus + 1).min(self.nav.len().saturating_sub(1));
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.nav_focus = self.nav_focus.saturating_sub(1);
                }
                if input.consume_key(self.keymap.activate.modifiers, self.keymap.activate.key) {
                    activate = true;
                }
            });
            if activate {
                let page = self.nav.get(self.nav_focus).copied().unwrap_or(Page::Machine);
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
        self.messages_strip(ui);
        egui::containers::Panel::bottom("linc-bottom")
            .exact_size(theme::BOTTOM_BAR_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 4)),
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
                // The page title is the stable keyboard focus stop. It lets
                // arrow keys enter the normal widget focus order without a
                // mouse or Tab.
                if self.page_focus_requested {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::unique("linc-page-focus"), true)
                    });
                    self.page_focus_requested = false;
                }
                self.banners(ui);
                // The central page must end above the fixed bottom action bar.
                let page_bottom = ui.ctx().input(|input| input.viewport_rect().bottom())
                    - theme::BOTTOM_BAR_HEIGHT;
                let page_height = (page_bottom - ui.cursor().min.y).max(120.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), page_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| match self.page {
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
                    },
                );
            });
        self.draw_modal(ui.ctx());
        self.draw_tool_change_prompt(ui.ctx());
        self.draw_toasts(ui.ctx());
        self.save_preferences(false);
        if self.quit_requested {
            self.save_preferences(true);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("LiNC")
                            .size(theme::font_size(16.0))
                            .strong()
                            .color(theme::FG),
                    );
                    if self.is_sim() {
                        ui.add_space(4.0);
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
                        ui.painter().rect_filled(rect, 3, theme::WARNING);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            "SIM",
                            FontId::monospace(theme::font_size(10.0)),
                            Color32::from_rgb(0x1a, 0x12, 0x04),
                        );
                    }
                    ui.label(
                        RichText::new(self.profile.config.machine.name.clone())
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("PANEL {}", self.snapshot.panel.mode.label()))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(if self.snapshot.panel.mode.is_valid() {
                                theme::ACCENT
                            } else {
                                theme::ERROR
                            }),
                    );
                    ui.label(
                        RichText::new(format!("TASK {}", self.snapshot.task.mode.label()))
                            .monospace()
                            .size(theme::font_size(9.0))
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
                        .size(theme::font_size(13.0))
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
                        .size(theme::font_size(12.0))
                        .color(alarm_color),
                );
                ui.label(
                    RichText::new(format!("MSG {}", self.snapshot.message_count()))
                        .monospace()
                        .size(theme::font_size(12.0))
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
        ui.label(
            RichText::new("NAVIGATION")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        ui.add_space(4.0);
        // Copied out: the loop body navigates, which borrows `self` mutably.
        let nav = self.nav.clone();
        for (index, page) in nav.iter().enumerate() {
            let Some((_, label, keycap)) = NAV_ITEMS.iter().find(|(entry, _, _)| entry == page)
            else {
                continue;
            };
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
            let icon_color = if selected {
                Color32::from_rgb(0x0b, 0x17, 0x22)
            } else {
                theme::MUTED
            };
            draw_nav_icon(
                painter,
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 8.0, rect.center().y - 9.0),
                    Vec2::splat(18.0),
                ),
                *page,
                icon_color,
            );
            painter.text(
                egui::pos2(rect.left() + 34.0, rect.center().y - 7.0),
                Align2::LEFT_CENTER,
                *label,
                FontId::proportional(theme::font_size(11.0)),
                if selected {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    theme::FG
                },
            );
            if !keycap.is_empty() {
                painter.text(
                    egui::pos2(rect.left() + 34.0, rect.center().y + 8.0),
                    Align2::LEFT_CENTER,
                    *keycap,
                    FontId::monospace(theme::font_size(8.5)),
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
        Frame::new()
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(4)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("FOCUS / NAVIGATION")
                        .size(theme::font_size(9.0))
                        .strong()
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("ARROWS MOVE")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("ENTER SELECT")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("ESC BACK")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("PGUP / PGDN")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("HOME / END")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("CTRL+LEFT NAV")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new("NO ON-SCREEN\nMACHINE CONTROLS")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    }

    /// Keyboard commands available on the current page, as `(key, action)`
    /// pairs. Rendered as hints only — they are not buttons.
    fn key_hints(&self) -> Vec<(String, &'static str)> {
        let k = &self.keymap;
        // Page-specific commands first: the row is truncated to the available
        // width, and the commands for the page in front of the operator matter
        // more than the global navigation keys.
        let mut hints: Vec<(String, &'static str)> = Vec::new();
        match self.page {
            Page::Program => {
                // The square is the key glyph (the toggle sits on an F-key,
                // which is implicit everywhere else); the label names the view
                // the press would switch to, so the row reads as an action.
                hints.push((
                    "\u{25a2}".to_string(),
                    if self.program.view == ProgramView::Split {
                        "CODE"
                    } else {
                        "SPLIT"
                    },
                ));
                hints.push((k.program_run_from_line.label(), "RUN LINE"));
                hints.push((k.graph_fit.label(), "FIT"));
                hints.push((k.graph_clear.label(), "CLEAR TRAIL"));
                hints.push((k.goto_line.label(), "GOTO"));
                hints.push((k.view_cycle.label(), "VIEW"));
                hints.push((k.save.label(), "SAVE"));
                hints.push((k.undo.label(), "UNDO"));
                hints.push((k.redo.label(), "REDO"));
            }
            Page::Files => {
                hints.push((k.files_copy_to_nc.label(), "COPY NC"));
                hints.push((k.files_copy_to_usb.label(), "COPY USB"));
                hints.push((k.refresh.label(), "REFRESH"));
                hints.push(("Enter".into(), "LOAD"));
                hints.push(("Bksp".into(), "UP"));
                hints.push((k.files_delete.label(), "DELETE"));
            }
            Page::Help => {
                hints.push((k.help_search.label(), "SEARCH"));
                hints.push(("Bksp".into(), "BACK"));
                hints.push(("PgUp/PgDn".into(), "PAGE"));
            }
            Page::Diagnostics => hints.push(("PgUp/PgDn".into(), "TABS")),
            Page::Mdi => {
                hints.push((k.mdi_issue.label(), "ISSUE"));
                hints.push((k.mdi_history_up.label(), "HISTORY"));
            }
            Page::Manual | Page::Homing => {
                hints.push((k.graph_fit.label(), "FIT"));
                hints.push((k.view_cycle.label(), "VIEW"));
                hints.push((k.graph_clear.label(), "CLEAR TRAIL"));
                hints.push(("Arrows".into(), "SELECT"));
                hints.push(("X/Y/Z".into(), "ZERO AXIS"));
                hints.push(("A".into(), "ZERO ALL"));
            }
            Page::Offsets | Page::Probe => hints.push(("Arrows".into(), "SELECT")),
            Page::Tools => hints.push(("Arrows".into(), "SELECT")),
            _ => {}
        }
        // Only page-specific commands are hinted. The physical navigation keys
        // (F5-F10), ESC and Ctrl+Q are implicit: they are printed on the panel
        // and in the left navigation, so repeating them here wastes the row.
        hints
    }

    /// Controller messages, one line above the hint row.
    ///
    /// The controller is the only thing that reports why a program stopped, and
    /// those messages used to live only on the Machine page. The newest few are
    /// shown on every page, newest first, coloured by severity; the row
    /// disappears when the controller has said nothing.
    fn messages_strip(&self, ui: &mut egui::Ui) {
        if self.snapshot.alarms.is_empty() {
            return;
        }
        let size = theme::font_size(9.5);
        let errors = self
            .snapshot
            .alarms
            .iter()
            .filter(|alarm| {
                matches!(
                    alarm.severity,
                    linc_core::model::AlarmSeverity::Error
                        | linc_core::model::AlarmSeverity::Critical
                )
            })
            .count();
        egui::containers::Panel::bottom("linc-messages")
            .exact_size(20.0)
            .frame(
                Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(16, 2)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if errors > 0 {
                            format!("MESSAGES · {errors} ERROR")
                        } else {
                            "MESSAGES".to_string()
                        })
                        .size(size)
                        .strong()
                        .color(if errors > 0 { theme::ERROR } else { theme::MUTED }),
                    );
                    ui.add_space(8.0);
                    for alarm in self.snapshot.alarms.iter().take(3) {
                        let colour = match alarm.severity {
                            linc_core::model::AlarmSeverity::Critical
                            | linc_core::model::AlarmSeverity::Error => theme::ERROR,
                            linc_core::model::AlarmSeverity::Warning => theme::WARNING,
                            linc_core::model::AlarmSeverity::Info => theme::MUTED,
                        };
                        // The strip is one row high: long messages are cut here
                        // and read in full in the Machine page's pane.
                        let text: String = alarm.message.chars().take(64).collect();
                        ui.label(RichText::new(text).size(size).color(colour));
                        ui.add_space(12.0);
                    }
                });
            });
    }

    /// Bottom bar: keyboard commands on the left, read-only machine status on
    /// the right. It carries no buttons.
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let size = theme::font_size(11.0);
        let hint_size = theme::font_size(9.5);
        ui.horizontal(|ui| {
            // Left: the keyboard commands available on this page, truncated to
            // the space the right-aligned status block leaves free.
            const STATUS_BUDGET: f32 = 500.0;
            let hint_char = hint_size * 0.62;
            let budget = (ui.available_width() - STATUS_BUDGET).max(140.0);
            let mut used = 0.0_f32;
            for (index, (key, action)) in self.key_hints().into_iter().enumerate() {
                let width = (key.chars().count() + action.chars().count() + 1) as f32
                    * hint_char
                    + 10.0;
                if used + width > budget {
                    break;
                }
                used += width;
                if index > 0 {
                    ui.add_space(10.0);
                }
                ui.label(
                    RichText::new(key)
                        .monospace()
                        .size(hint_size)
                        .strong()
                        .color(theme::C3),
                );
                ui.label(
                    RichText::new(action)
                        .monospace()
                        .size(hint_size)
                        .color(theme::MUTED),
                );
            }

            // Right: the read-only machine state.
            let field = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                ui.label(RichText::new(text).monospace().size(size).color(color));
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Never claim OK on stale data: the controller may be latched
                // while the last snapshot still says otherwise.
                let (estop_text, estop_color) = if self.snapshot.meta.stale {
                    ("E-STOP ?".to_string(), theme::WARNING)
                } else if self.snapshot.machine.estop {
                    ("E-STOP ACTIVE".to_string(), theme::ERROR)
                } else {
                    ("E-STOP OK".to_string(), theme::SUCCESS)
                };
                field(ui, estop_text, estop_color);
                ui.add_space(16.0);
                // Machine on/off sits immediately left of the E-stop state.
                // `halui.machine.is-on` is the authority; a stale snapshot
                // reports `?` rather than guessing.
                let (machine_text, machine_color) = if self.snapshot.meta.stale {
                    ("MACHINE ?".to_string(), theme::WARNING)
                } else if self.snapshot.machine.on {
                    ("MACHINE ON".to_string(), theme::SUCCESS)
                } else {
                    ("MACHINE OFF".to_string(), theme::MUTED)
                };
                field(ui, machine_text, machine_color);
                ui.add_space(16.0);
                // Protect is only worth a field when it is engaged: the off
                // state is the normal one and the row is space-limited.
                if self.program_protected() {
                    field(ui, "PROTECT ON".to_string(), theme::WARNING);
                    ui.add_space(16.0);
                }
                field(
                    ui,
                    format!(
                        "OVR F{:.0} S{:.0} R{:.0}",
                        self.snapshot.feed.feed_override * 100.0,
                        self.snapshot.feed.spindle_override * 100.0,
                        self.snapshot.feed.rapid_override * 100.0
                    ),
                    theme::MUTED,
                );
                ui.add_space(16.0);
                field(
                    ui,
                    self.snapshot.panel.mode.label().to_string(),
                    if self.snapshot.panel.mode.is_valid() {
                        theme::ACCENT
                    } else {
                        theme::ERROR
                    },
                );
            });
        });
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        // The E-stop is the most severe state and is never hidden behind a tab.
        if self.snapshot.machine.estop {
            theme::banner(
                ui,
                theme::ERROR,
                "EMERGENCY STOP ACTIVE — MOTION AND MACHINE ACTIONS BLOCKED",
            );
            ui.add_space(4.0);
        }
        if self.snapshot.limits_overridden {
            theme::banner(
                ui,
                theme::WARNING,
                "LIMITS OVERRIDE ACTIVE — LIMIT SWITCHES ARE IGNORED",
            );
            ui.add_space(4.0);
        }
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
    }

    /// The manual tool-change prompt, on every page.
    ///
    /// Probe Basic's equivalent is an application-modal dialog driven by the
    /// HAL request, so the operator sees it wherever they are and cannot close
    /// it without confirming. This is the same idea: it follows
    /// `tool.change_active` rather than a page, and has no dismiss action.
    fn draw_tool_change_prompt(&mut self, ctx: &egui::Context) {
        if self.snapshot.tool_change.status != linc_core::model::ToolChangeStatus::ManualPrompt {
            self.tool_prompt_open_last_frame = false;
            return;
        }
        let focus_default = !self.tool_prompt_open_last_frame;
        self.tool_prompt_open_last_frame = true;
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

    fn draw_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.clone() else {
            self.modal_open_last_frame = false;
            return;
        };
        // On the frame a dialog opens, put the keyboard on its default button so
        // Enter takes it and the arrow keys reach the others.
        let focus_default = !self.modal_open_last_frame;
        self.modal_open_last_frame = true;
        let mut close = false;
        let mut action: Option<PendingAction> = None;
        let mut save_and_continue = false;
        let mut start_line: Option<u32> = None;
        let mut delete: Option<(std::path::PathBuf, bool)> = None;
        let mut quit = false;
        let mut overwrite: Option<bool> = None;
        let mut goto_buffer: Option<String> = None;
        let mut goto_accept = false;
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
                    ui.label(RichText::new("UNSAVED CHANGES").size(theme::font_size(18.0)).strong().color(theme::FG));
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
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if focus_default {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
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
                        if focus_default {
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
                        if focus_default {
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
                        if focus_default {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Modal::GoToLine { buffer } => {
                    ui.label(
                        RichText::new("GO TO LINE")
                            .size(theme::font_size(18.0))
                            .strong()
                            .color(theme::FG),
                    );
                    ui.add_space(6.0);
                    ui.label(format!(
                        "Line number (1-{}):",
                        self.program.editor.line_count().max(1)
                    ));
                    ui.add_space(6.0);
                    let mut text = buffer.clone();
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .id(egui::Id::unique("linc-goto-line-input"))
                            .desired_width(120.0)
                            .hint_text("line"),
                    );
                    if focus_default {
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
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, "OVERWRITE", true).clicked() {
                            overwrite = Some(true);
                        }
                        let cancel = theme::action_button(ui, "CANCEL", true);
                        if focus_default {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            overwrite = Some(false);
                        }
                    });
                }
                Modal::Error(text) => {
                    ui.label(RichText::new("ERROR").size(theme::font_size(18.0)).strong().color(theme::ERROR));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    if theme::action_button(ui, "OK", true).clicked() {
                        close = true;
                    }
                }
                Modal::Info(text) => {
                    ui.label(RichText::new("NOTICE").size(theme::font_size(18.0)).strong().color(theme::FG));
                    ui.add_space(6.0);
                    ui.label(text);
                    ui.add_space(12.0);
                    if theme::action_button(ui, "OK", true).clicked() {
                        close = true;
                    }
                }
            });
        if let Some(text) = goto_buffer {
            self.modal = Some(Modal::GoToLine { buffer: text });
        }
        if goto_accept {
            let line = match self.modal.as_ref() {
                Some(Modal::GoToLine { buffer }) => buffer.trim().parse::<usize>().ok(),
                _ => None,
            };
            if let Some(line) = line {
                self.modal = None;
                self.jump_to_line(line);
            }
        }
        if close {
            self.modal = None;
        }
        if let Some((path, usb)) = delete {
            self.modal = None;
            self.delete_file(path, usb);
        }
        if let Some(line) = start_line {
            self.modal = None;
            self.send(linc_core::command::MachineCommand::RunFromLine { line });
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

fn draw_nav_icon(painter: &egui::Painter, rect: egui::Rect, page: Page, color: Color32) {
    let s = egui::Stroke::new(1.5, color);
    let c = rect.center();
    match page {
        Page::Machine => {
            painter.rect_stroke(rect.shrink(2.0), 3.0, s, StrokeKind::Inside);
            painter.circle_stroke(c, 3.0, s);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 2.0, c.y),
                    egui::pos2(rect.right() - 2.0, c.y),
                ],
                s,
            );
        }
        Page::Program => {
            painter.rect_stroke(
                rect.shrink2(Vec2::new(2.0, 1.0)),
                2.0,
                s,
                StrokeKind::Inside,
            );
            for offset in [-3.0, 0.0, 3.0] {
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 5.0, c.y + offset),
                        egui::pos2(rect.right() - 5.0, c.y + offset),
                    ],
                    s,
                );
            }
        }
        Page::Offsets => {
            painter.circle_stroke(c, 7.0, s);
            painter.line_segment(
                [
                    egui::pos2(c.x, rect.top() + 1.0),
                    egui::pos2(c.x, rect.top() + 5.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, rect.bottom() - 5.0),
                    egui::pos2(c.x, rect.bottom() - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 1.0, c.y),
                    egui::pos2(rect.left() + 5.0, c.y),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.right() - 5.0, c.y),
                    egui::pos2(rect.right() - 1.0, c.y),
                ],
                s,
            );
        }
        Page::Tools => {
            let points = [
                egui::pos2(rect.left() + 2.0, c.y),
                egui::pos2(rect.left() + 6.0, c.y),
                egui::pos2(rect.left() + 8.0, rect.top() + 5.0),
                egui::pos2(rect.left() + 11.0, rect.bottom() - 4.0),
                egui::pos2(rect.left() + 14.0, c.y),
                egui::pos2(rect.right() - 2.0, c.y),
            ];
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], s);
            }
        }
        Page::Diagnostics | Page::Settings => {
            painter.circle_stroke(c, 7.0, s);
            painter.circle_filled(c, 2.0, color);
        }
        Page::Help => {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(15.0),
                color,
            );
        }
        Page::Files | Page::Probe | Page::Mdi | Page::Homing | Page::Manual => {
            painter.rect_stroke(rect.shrink(3.0), 2.0, s, StrokeKind::Inside);
        }
    }
}

impl eframe::App for LincApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::apply_design_zoom(ui.ctx(), self.design_scale);
        self.publish_diag();
        self.capture_screenshot(ui.ctx());
        if self.diag_bus.is_some() {
            // egui repaints on demand, so an idle window would stop publishing
            // and every diagnostic response would read as stale. Four times a
            // second is cheap and keeps `age_ms` honest.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }

        self.handle_keyboard(ui);
        self.draw(ui);
    }
}

/// Navigation entries in display order, with their label and keycap hint.
///
/// Which of these actually appear is `[ui] pages` in the profile, so a machine
/// can hide the pages it does not use (a machine with no changer can hide
/// TOOLS, for example) without removing the screen from the binary.
/// Ordered to match the keyboard: MAIN (F5), FILE (F6), TOOL (F7), OFFSET
/// (F8), WIN (F10). A page that has no function key follows the ones that do.
pub const NAV_ITEMS: [(Page, &str, &str); 7] = [
    (Page::Machine, "MACHINE", "MAIN"),
    (Page::Program, "PROGRAM", "FILE"),
    (Page::Tools, "TOOLS", "TOOL"),
    (Page::Offsets, "OFFSETS", "OFFSET"),
    (Page::Diagnostics, "DIAGNOSTICS", "WIN"),
    (Page::Help, "G-CODE HELP", "HELP"),
    (Page::Settings, "SETTINGS", ""),
];

/// Navigation entries to show, from `[ui] pages`.
///
/// An empty list means "all of them", so a profile that never mentions the key
/// still gets the full navigation.
fn nav_pages(configured: &[String]) -> Vec<Page> {
    if configured.is_empty() {
        return NAV_ITEMS.iter().map(|(page, _, _)| *page).collect();
    }
    configured
        .iter()
        .map(|key| page_from_config(key))
        .collect()
}

/// Stable preferences key for a page. Mode-driven pages are never persisted.
fn page_key(page: Page) -> &'static str {
    match page {
        Page::Machine => "machine",
        Page::Program => "program",
        Page::Offsets => "offsets",
        Page::Tools => "tools",
        Page::Diagnostics => "diagnostics",
        Page::Help => "help",
        Page::Files => "files",
        Page::Probe => "probe",
        Page::Settings => "settings",
        Page::Mdi => "mdi",
        Page::Homing => "homing",
        Page::Manual => "manual",
    }
}

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

/// Fraction of the current move already travelled, from the remaining distance.
///
/// `remaining` is the magnitude of the DTG — the sign only says which way the
/// move goes — and `total` is the largest remainder seen since the axis last
/// stopped, i.e. the length of the move. The bar starts empty, fills as the
/// remainder shrinks, and sits full once the axis arrives. A move that turns out
/// to be longer than the one recorded resets the total, so the bar can never go
/// past empty.
///
/// Returns `(fraction, total)`, where `total` is what the caller should keep for
/// the next observation.
pub fn move_progress(remaining: f64, total: f64) -> (f64, f64) {
    const AT_REST: f64 = 1e-6;
    let remaining = remaining.abs();
    if remaining <= AT_REST {
        // Nothing left to travel: the move is complete and the next one starts
        // from a fresh total.
        return (1.0, 0.0);
    }
    let total = total.max(remaining);
    ((1.0 - remaining / total).clamp(0.0, 1.0), total)
}

/// Is this snapshot's trail still the one from before `Ctrl+L`?
///
/// The backend applies the clear on its next conversion. Until then its
/// snapshot carries the old points, and repainting them made the trail flash
/// away and come straight back. The applied clear leaves a single point; a moved
/// tool tip means the points are new and should be kept.
fn cleared_trail_pending(snapshot: &MachineSnapshot, cleared_tip: Option<[f64; 3]>) -> bool {
    let moved = match (snapshot.tool_tip, cleared_tip) {
        (Some(now), Some(before)) => {
            (now[0] - before[0]).abs() > 1e-4
                || (now[1] - before[1]).abs() > 1e-4
                || (now[2] - before[2]).abs() > 1e-4
        }
        _ => false,
    };
    !(snapshot.live_trail.len() <= 2 || moved)
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
        ui.label(
            RichText::new(label)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
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
[preferences]
file = "{root}/prefs/{{profile}}.preferences.toml"
"#.replace("{root}", &root.to_string_lossy()),
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
    fn a_touch_off_reaches_the_offset_table() {
        // The keys issue `G10 L20`; the table rebuilds from the snapshot's
        // revision. A constant revision left the table showing the old value
        // while the DRO moved, which read as "only the DRO changed".
        let (_dir, mut app) = make_app(PanelMode::Jog);
        app.poll_backend();
        let machine_x = app
            .snapshot
            .positions
            .axes
            .get("X")
            .map(|position| position.machine)
            .unwrap_or(0.0);
        crate::screens::common::zero_axes(&mut app, vec!["X".to_string()]);
        app.poll_backend();
        app.sync_offsets_from_snapshot(false);
        let row = app
            .offsets
            .rows
            .get(&1)
            .expect("G54 row present after a touch off");
        let value = row.values.get("X").cloned().unwrap_or_default();
        let parsed: f64 = value.parse().unwrap_or(f64::NAN);
        assert!(
            (parsed - machine_x).abs() < 1e-6,
            "G54 X should hold the machine position {machine_x}, got {value:?}"
        );
    }

    #[test]
    fn mode_driven_pages_open_and_close() {
        let (_dir, mut app) = make_app(PanelMode::Mdi);
        assert_eq!(app.page, Page::Mdi);
        app.on_mode_change(PanelModeValidity::Valid(PanelMode::Auto));
        assert_eq!(app.page, Page::Program, "AUTO shows the running program");
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
        // The simulator models an instantaneous handshake, so it completes at
        // once. The LinuxCNC backend holds the prompt until `iocontrol` drops
        // the request; that level is covered in linc-linuxcnc.
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
    #[test]
    fn jump_to_line_clamps_and_selects() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        app.program.editor.replace_text("L1\nL2\nL3\nL4\n".into());
        app.jump_to_line(3);
        assert_eq!(app.program.selected_line, 3);
        assert_eq!(app.program.code_scroll_to_line, Some(3));
        assert_eq!(app.program.cursor_line, Some(3));
        app.jump_to_line(0);
        assert_eq!(app.program.selected_line, 1);
        app.jump_to_line(9999);
        assert_eq!(
            app.program.selected_line,
            app.program.editor.line_count().max(1)
        );
    }

    #[test]
    fn stepping_the_selection_clamps_at_both_ends() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        let text: String = (1..=40).map(|n| format!("L{n}\n")).collect();
        app.program.editor.replace_text(text);
        let count = app.program.editor.line_count();
        app.program.selected_line = 1;
        app.step_selected_line(-1);
        assert_eq!(app.program.selected_line, 1);
        app.step_selected_line(LINE_PAGE);
        assert_eq!(
            app.program.selected_line,
            (1 + LINE_PAGE as usize).min(count)
        );
        app.step_selected_line(10_000);
        assert_eq!(app.program.selected_line, count);
        assert_eq!(app.program.code_scroll_to_line, Some(count));
    }

    #[test]
    fn goto_line_is_bound_to_ctrl_g() {
        let (_dir, app) = make_app(PanelMode::Edit);
        assert!(app.keymap.goto_line.modifiers.ctrl);
        assert_eq!(app.keymap.goto_line.key, egui::Key::G);
    }

    #[test]
    fn dro_entry_sets_the_position_it_was_given() {
        let (_dir, mut app) = make_app(PanelMode::Jog);
        app.poll_backend();
        app.offsets.dro_edit = Some(crate::state::DroEdit {
            axis: "X".into(),
            buffer: "10+2.5".into(),
            focus: false,
        });
        assert!(crate::screens::offsets::commit_dro_edit(&mut app));
        app.poll_backend();
        let work = app
            .snapshot
            .positions
            .axes
            .get("X")
            .map(|position| position.work)
            .unwrap_or(f64::NAN);
        assert!((work - 12.5).abs() < 1e-6, "DRO X shows {work}, expected 12.5");
    }

    #[test]
    fn a_stale_trail_is_masked_until_the_backend_clears_it() {
        let (_dir, app) = make_app(PanelMode::Jog);
        let mut snapshot = app.snapshot.clone();
        snapshot.tool_tip = Some([1.0, 2.0, 3.0]);
        snapshot.live_trail = std::sync::Arc::new(vec![[0.0; 3]; 5]);
        let tip = Some([1.0, 2.0, 3.0]);
        assert!(cleared_trail_pending(&snapshot, tip), "old points masked");

        // The backend applied the clear: one point left.
        snapshot.live_trail = std::sync::Arc::new(vec![[1.0, 2.0, 3.0]]);
        assert!(!cleared_trail_pending(&snapshot, tip), "clear accepted");

        // The tool moved: the points are new, not stale.
        snapshot.live_trail = std::sync::Arc::new(vec![[0.0; 3]; 5]);
        snapshot.tool_tip = Some([5.0, 2.0, 3.0]);
        assert!(!cleared_trail_pending(&snapshot, tip), "new motion kept");
    }
    #[test]
    fn the_preview_uses_the_offset_the_program_selects() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        for row in app.snapshot.offsets.offsets.iter_mut() {
            if row.index == 1 {
                row.values.insert("X", -54.35);
                row.values.insert("Y", -49.3);
            }
            if row.index == 2 {
                row.values.insert("X", -100.0);
                row.values.insert("Y", -50.0);
            }
        }
        app.snapshot.offsets.active_index = Some(1);

        // The program selects G54: drawn at the G54 origin, not machine zero.
        app.program
            .editor
            .replace_text("G21 G90 G54\nG0 X0 Y0\n".into());
        app.refresh_preview();
        let end = app
            .program
            .preview
            .segments
            .last()
            .map(|segment| segment.end)
            .unwrap_or_default();
        assert!((end[0] + 54.35).abs() < 1e-6, "G54 X {}", end[0]);
        assert!((end[1] + 49.3).abs() < 1e-6, "G54 Y {}", end[1]);

        // A program that selects G55 is drawn at G55, though G54 is active.
        app.program
            .editor
            .replace_text("G21 G90 G55\nG0 X0 Y0\n".into());
        app.refresh_preview();
        let end = app
            .program
            .preview
            .segments
            .last()
            .map(|segment| segment.end)
            .unwrap_or_default();
        assert!((end[0] + 100.0).abs() < 1e-6, "G55 X {}", end[0]);

        // With no selection the active system is the fallback.
        app.program.editor.replace_text("G21 G90\nG0 X0 Y0\n".into());
        app.refresh_preview();
        let end = app
            .program
            .preview
            .segments
            .last()
            .map(|segment| segment.end)
            .unwrap_or_default();
        assert!((end[0] + 54.35).abs() < 1e-6, "active X {}", end[0]);
    }
    #[test]
    fn the_view_cycle_key_steps_iso_x_y_z_and_refits() {
        let (_dir, mut app) = make_app(PanelMode::Jog);
        assert!(app.keymap.view_cycle.modifiers.ctrl);
        assert_eq!(app.keymap.view_cycle.key, egui::Key::F10);
        let order = [
            linc_view::ViewMode::SideYz,
            linc_view::ViewMode::SideXz,
            linc_view::ViewMode::TopXy,
            linc_view::ViewMode::Iso3d,
        ];
        for expected in order {
            app.program.fit_requested = false;
            app.manual.fit = false;
            app.cycle_toolpath_view();
            assert_eq!(app.program.graph_view, expected, "{expected:?}");
            assert_eq!(app.manual.view, expected, "{expected:?}");
            assert!(app.program.fit_requested, "each view refits: {expected:?}");
            assert!(app.manual.fit, "each view refits: {expected:?}");
        }
    }
}


/// Encode an egui capture as a PNG.
fn png_from_color_image(image: &egui::ColorImage) -> Option<linc_core::diag::Screenshot> {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_array());
    }
    let image = image::RgbaImage::from_raw(width as u32, height as u32, bytes)?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(linc_core::diag::Screenshot {
        png,
        width: width as u32,
        height: height as u32,
        at_ms: linc_core::diag::now_ms(),
    })
}

#[cfg(test)]
mod move_progress_tests {
    use super::move_progress;

    #[test]
    fn a_move_fills_the_bar_from_empty_to_full() {
        let (fraction, total) = move_progress(-100.0, 0.0);
        assert_eq!(fraction, 0.0);
        assert_eq!(total, 100.0);

        let (fraction, total) = move_progress(-50.0, total);
        assert!((fraction - 0.5).abs() < 1e-9, "{fraction}");

        let (fraction, total) = move_progress(-0.0, total);
        assert_eq!(fraction, 1.0);
        assert_eq!(total, 0.0);
    }

    #[test]
    fn the_sign_of_the_remaining_distance_is_ignored() {
        let (plus, _) = move_progress(100.0, 0.0);
        let (minus, _) = move_progress(-100.0, 0.0);
        assert_eq!(plus, 0.0);
        assert_eq!(minus, 0.0);

        let (plus, _) = move_progress(40.0, 100.0);
        let (minus, _) = move_progress(-40.0, 100.0);
        assert!((plus - 0.6).abs() < 1e-9, "{plus}");
        assert_eq!(plus, minus);
    }

    #[test]
    fn a_longer_move_resets_the_total_instead_of_going_negative() {
        let (fraction, total) = move_progress(-200.0, 50.0);
        assert_eq!(fraction, 0.0);
        assert_eq!(total, 200.0);
    }

    #[test]
    fn an_idle_axis_shows_a_full_bar() {
        let (fraction, total) = move_progress(0.0, 0.0);
        assert_eq!(fraction, 1.0);
        assert_eq!(total, 0.0);
    }
}
