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
use linc_core::mdi_file::SaveError;
use linc_core::model::{
    format_clock, now_ms, Alarm, AlarmSeverity, AlarmSource, AxisMap, MachinePower,
    MachineSnapshot, MachineVariant, PanelMode, PanelModeValidity, PositionSet, ProgramState,
    ToolChangeState, ToolChangeStatus,
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

/// Screen pixels the toolpath moves per arrow-key pan.
///
/// A few presses should cross the part, but one press must not jump it out of
/// the card; 40 px is about a short drag.
const VIEW_PAN_STEP: f32 = 40.0;

/// Zoom per key press, matching one wheel notch.
const VIEW_ZOOM_STEP: f32 = 1.25;

/// The clipboard shortcut `egui-winit` converts into `Event::Cut`. It is the
/// platform convention rather than a profile binding, and it is what the two
/// page-local consumers bind: the Probe page's X data reset and the Files
/// page's delete (`[keys].cut` defaults to the same chord).
const CUT_SHORTCUT: Binding = Binding::new(egui::Key::X, egui::Modifiers::CTRL);

/// One titled group of page-local keyboard commands shown in the sidebar.
/// An empty title means the group has no header (most pages).
struct ShortcutGroup {
    title: &'static str,
    items: Vec<(String, &'static str)>,
}

pub struct LincApp {
    /// Publishes state for `linc-mcp`. `None` in tests and when the diagnostic
    /// socket could not be created.
    pub diag_bus: Option<std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>>,
    /// Smoothed frame rate, shown in the top bar and published for diagnostics.
    pub fps: f32,
    /// Backend sample stamp of the last published machine snapshot.
    diag_last_sample: u64,
    pub profile: Profile,
    pub backend: Box<dyn UiBackend>,
    pub snapshot: MachineSnapshot,
    pub page: Page,
    pub last_non_mode_page: Page,
    pub files_return_page: Page,
    pub help_return_page: Page,
    /// Where Esc on the Diagnostics page goes back to: the page the operator
    /// left to look at it, not the program reset.
    pub diagnostics_return_page: Page,
    pub nav_focus: usize,
    pub focus_region: FocusRegion,
    pub last_mode: Option<PanelModeValidity>,
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
    pub diagnostics_tab: DiagnosticsTab,
    pub diagnostics_search: String,
    pub diagnostics_severity: Option<AlarmSeverity>,
    pub diagnostics_source: Option<AlarmSource>,
    pub diagnostics_clear_view: bool,
    pub ui_log: VecDeque<Alarm>,
    pub design_scale: f32,
    pub page_focus_requested: bool,
    pub modal: Option<Modal>,
    /// Full-screen controller log, opened with `[keys].messages`.
    pub messages_dialog: bool,
    /// Page-scroll requested by the dialog's keys this frame.
    pub messages_scroll: f32,
    /// Rising-edge detector, so follow-current is re-armed when a program
    /// starts but not again on every poll after the operator selects a line.
    pub program_was_running: bool,
    /// Geometry the Probe page painted, read by the 1280x720 layout tests.
    #[cfg(test)]
    pub probe_layout: screens::probe_view::ProbeLayout,
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
    /// MAIN's bottom row shows the following-error traces instead of the feed
    /// and servo cards. Toggled with the square key there.
    pub machine_following: bool,
    /// Following error per axis, newest last, sampled as each snapshot arrives.
    pub following_history: linc_core::model::AxisMap<std::collections::VecDeque<f64>>,
    /// Largest absolute following error per axis since the last program start.
    pub following_run_max: linc_core::model::AxisMap<f64>,
    /// Sum of absolute samples since the last program start, for the mean.
    pub following_run_abs_sum: linc_core::model::AxisMap<f64>,
    /// Samples behind that sum.
    pub following_run_samples: linc_core::model::AxisMap<u64>,
    /// Edge state: a program that starts clears the run's numbers.
    pub following_run_active: bool,
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
    /// Window size and scale of the last frame, for `linc_get_screen_model`.
    pub diag_viewport: linc_core::diag::ViewportInfo,
    /// Named shell rectangles from the last frame.
    pub diag_regions: Vec<linc_core::diag::ScreenRegion>,
    pub diag_frame_time_ms: f32,
    /// Lathe only: read the X position as a diameter (`2X`) instead of a
    /// radius. A display factor — the controller keeps working in radius, and
    /// no command is issued from it.
    pub lathe_diameter: bool,
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

    /// Apply the intents an agent queued through `diag.ui_action`.
    ///
    /// These land before the frame draws, so a page switch and the following
    /// screenshot show the same state.
    fn apply_diag_actions(&mut self) {
        let Some(bus) = &self.diag_bus else { return };
        let actions = bus
            .lock()
            .map(|mut bus| bus.take_ui_actions())
            .unwrap_or_default();
        for action in actions {
            match action {
                linc_core::diag::UiAction::Page { page } => {
                    if let Some(page) = page_from_name(&page) {
                        self.request_page(page);
                    } else {
                        self.toast(format!("unknown page {page}"), theme::WARNING);
                    }
                }
                linc_core::diag::UiAction::CycleView => self.cycle_toolpath_view(),
                linc_core::diag::UiAction::ShowBounds { visible } => {
                    self.graph_style.bounds_mode = if visible { 0 } else { 3 };
                    self.preferences.bounds_mode = Some(self.graph_style.bounds_mode);
                    self.preferences_dirty = true;
                }
                linc_core::diag::UiAction::CycleBounds => self.cycle_bounds(),
                linc_core::diag::UiAction::Fit => self.fit_graph(),
                linc_core::diag::UiAction::ClearTrail => self.clear_trail(),
                linc_core::diag::UiAction::SelectLine { line } => self.jump_to_line(line),
            }
        }
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
            fps: self.fps,
            toasts: self.toasts.iter().map(|toast| toast.text.clone()).collect(),
            viewport: self.diag_viewport.clone(),
            regions: self.diag_regions.clone(),
            frame_time_ms: self.diag_frame_time_ms,
        }
    }

    /// Record one shell rectangle for the screen model, in points.
    fn note_region(&mut self, name: &str, rect: egui::Rect) {
        self.diag_regions.push(linc_core::diag::ScreenRegion::new(
            name,
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
        ));
    }

    /// Attach the diagnostic bus the app server publishes into.
    pub fn set_diag_bus(&mut self, bus: std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>) {
        self.diag_bus = Some(bus);
    }

    pub fn new(profile: Profile, mut backend: Box<dyn UiBackend>, ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        theme::install_font_fallback(ctx);
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
            preview.trim_approach([0.0; 3]);
        }
        editor.set_saved_text(
            snapshot
                .program
                .content
                .as_deref()
                .cloned()
                .unwrap_or_default(),
        );
        // Runtime preferences are separate from the machine profile. They may
        // restore probe feeds/hints and UI state, never machine-owned identity
        // or measured calibration.
        let preferences = linc_core::preferences::Preferences::load(&profile.preferences_path);
        let mut probe_params = preferences
            .probe
            .clone()
            .unwrap_or_else(|| ProbeParameters::from_config(&profile.config.probe));
        probe_params.probe_tool_number = profile.config.probe.tool_number;
        probe_params.calibration_offset = snapshot.probe.calibration_offset.unwrap_or(0.0);
        let active_probe_wcs = snapshot
            .offsets
            .active_index
            .filter(|index| (1..=9).contains(index))
            .map(linc_core::model::wcs_name)
            .unwrap_or_else(|| "--".into());
        let mut graph_style = graph_style(&profile);
        graph_style.bounds_mode = preferences
            .bounds_mode
            .map(|mode| mode % 4)
            .unwrap_or(graph_style.bounds_mode);
        // A lathe-only view toggle; a mill profile never sees it and a file
        // written before the key existed reads as off.
        graph_style.lathe_mirror = preferences.lathe_mirror.unwrap_or(false);
        // The same for the REMAINING view.
        graph_style.remaining = preferences.remaining.unwrap_or(false);
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
            help_return_page: Page::Machine,
            diagnostics_return_page: Page::Machine,
            nav_focus: 0,
            focus_region: FocusRegion::Page,
            last_mode: Some(mode),
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
            probe: ProbeUi::new(probe_params, active_probe_wcs),
            mdi: MdiUi::restored(
                &preferences.mdi_history,
                profile.config.mdi.history_limit,
            ),
            help: HelpUi::default(),
            files: FilesUi::new(
                profile.machine_ini.program_prefix.clone(),
                profile.config.files.show_parent_above_root,
            ),
            homing_return_page: start_page,
            manual: ManualUi {
                view: if profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    linc_view::ViewMode::Iso3d
                },
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
            fps: 0.0,
            quit_requested: false,
            pending_load: false,
            keymap: crate::state::Keymap::from_config(&profile.config.keys),
            nav: nav_pages(&profile.config.ui.pages),
            jog_increments: crate::state::finite_increments(&profile.machine_ini.increments),
            jog_increment_index: preferences.jog_increment.unwrap_or(0),
            jog_speed_percent: 100.0,
            lathe_diameter: preferences.lathe_diameter.unwrap_or(false),
            diag_viewport: linc_core::diag::ViewportInfo::default(),
            diag_regions: Vec::new(),
            diag_frame_time_ms: 0.0,
            preferences,
            preferences_dirty: false,
            preferences_saved_at: std::time::Instant::now(),
            last_snapshot_ms: now_ms(),
            alarm_id_seen: 0,
            jog_speed_published: None,
            graph_origin: [0.0; 3],
            machine_following: false,
            following_history: linc_core::model::AxisMap::new(),
            following_run_max: linc_core::model::AxisMap::new(),
            following_run_abs_sum: linc_core::model::AxisMap::new(),
            following_run_samples: linc_core::model::AxisMap::new(),
            following_run_active: false,
            trail_cleared: false,
            trail_cleared_tip: None,
            move_distance: linc_core::model::AxisMap::new(),
            move_progress: linc_core::model::AxisMap::new(),
            modal_open_last_frame: false,
            tool_prompt_open_last_frame: false,
            messages_dialog: false,
            messages_scroll: 0.0,
            program_was_running: false,
            #[cfg(test)]
            probe_layout: screens::probe_view::ProbeLayout::default(),
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
            log::warn!("probe tool number is {configured}, expected {expected} (#3014)");
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
                snapshot.live_trail =
                    std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(tip, false)]);
            } else {
                self.trail_cleared = false;
            }
        }
        self.snapshot = snapshot;
        self.sample_following_error();
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
        // against. The backends bump `offsets.revision` on a write, but the
        // first snapshots arrive before the offset rows do and carry revision 0,
        // so a preview parsed with no offset is rebuilt once rows appear.
        //
        // The rows themselves are compared as well, because the controller moves
        // them without any HMI write: the startup publication issues its own
        // `G10 L2`, and a program may too. A preview left parsed against the
        // zero row it saw before the publication does not just draw the part in
        // the wrong place - `live_trail_shift` then shifts the tool marker by the
        // whole offset, which is the "toolhead sits at machine home" report.
        let rows = offsets_signature(&self.snapshot.offsets.offsets);
        let offsets_changed = self.snapshot.offsets.revision != self.program.preview_offsets_seen
            || rows != self.program.preview_offsets_rows;
        let offset_now_available =
            self.program.preview_offset.is_none() && !self.snapshot.offsets.offsets.is_empty();
        if offsets_changed || offset_now_available {
            self.refresh_preview();
        }
        // RAPID changes the jog ceiling, so the published speed follows it.
        // Compared against what we last sent, not against the snapshot: the
        // snapshot is rebuilt every poll, and comparing with it published on
        // every frame and starved the controller.
        let wanted = self.jog_speed_value();
        // A fault blocks new jog commands; keep the last published value
        // untouched so a slider change is published once the reset clears the
        // fault.
        if !self.snapshot.probe_trip_active()
            && self
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
            Some(index) if (1..=9).contains(&index) => linc_core::model::wcs_name(index),
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
        self.start_gate(PanelMode::Auto, true)
    }

    /// The MDI pane's "run the selected block" gate.
    ///
    /// Every check `run_from_line_block` makes except the AUTO-mode one and
    /// the loaded-program one: the pane starts a buffer block in MDI, and the
    /// issue path re-checks the mode before it sends anything.
    pub fn mdi_run_block(&self) -> Option<&'static str> {
        self.start_gate(PanelMode::Mdi, false)
    }

    /// Why a block may not start now, or `None` when it may.
    ///
    /// `mode` is the panel position the path needs; `needs_program` adds the
    /// loaded-program check the AUTO path has and the MDI buffer does not.
    fn start_gate(&self, mode: PanelMode, needs_program: bool) -> Option<&'static str> {
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

    /// MAIN shows the MDI pane while the panel sits in MDI.
    ///
    /// Keyed to the panel mode, not to page entry: returning to MAIN from
    /// another page while the panel still sits in MDI shows the pane again.
    pub fn mdi_pane_visible(&self) -> bool {
        self.page == Page::Machine && self.snapshot.panel.mode.mode() == Some(PanelMode::Mdi)
    }

    /// Why the MDI buffer may not be written to disk, or `None` when it may.
    ///
    /// The same policy `save_program` applies, minus the EDIT-mode one: a save
    /// needs an idle interpreter (`safety.save_requires_idle`), a live panel
    /// and no program protect, not the editor.
    pub fn mdi_save_block(&self) -> Option<&'static str> {
        if self.program_protected() {
            return Some("PROTECT ON — SAVE BLOCKED");
        }
        if self.panel_offline() {
            return Some("PANEL OFFLINE — SAVE BLOCKED");
        }
        if self.snapshot.task.running
            || self.snapshot.task.interp != linc_core::model::InterpState::Idle
        {
            return Some("not idle — save blocked");
        }
        None
    }

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
        self.modal = Some(crate::state::Modal::MdiSave(
            crate::state::MdiSaveDialog::default(),
        ));
    }

    /// Edit the open save dialog. A closed dialog is not an error: the frame
    /// that confirms a write clears it before these run.
    fn edit_mdi_save(&mut self, update: impl FnOnce(&mut crate::state::MdiSaveDialog)) {
        if let Some(crate::state::Modal::MdiSave(dialog)) = &mut self.modal {
            update(dialog);
        }
    }

    /// One step of the save dialog: write the buffer, or, when the name is
    /// taken, arm the overwrite step inside the same dialog.
    fn confirm_mdi_save(&mut self) {
        let Some(crate::state::Modal::MdiSave(dialog)) = self.modal.clone() else {
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
                self.modal = None;
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
        self.modal = Some(crate::state::Modal::ConfirmRunFromLine { line, block });
    }

    /// Run from the line selected in the G-code view, not from the controller's
    /// current line: after a program ends the controller sits on its last line,
    /// while the operator has picked the restart point in the editor.
    fn run_from_line_choice(&self) -> (u32, String) {
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
        if page == Page::Help && self.page != Page::Help {
            // Captured before the assignment below, which would otherwise make
            // the Help page its own return destination.
            self.help_return_page = if self.page.is_mode_driven() {
                self.last_non_mode_page
            } else {
                self.page
            };
        }
        if page == Page::Diagnostics && self.page != Page::Diagnostics {
            self.diagnostics_return_page = if self.page.is_mode_driven() {
                self.last_non_mode_page
            } else {
                self.page
            };
        }
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
            // Every entry path starts with nothing selected: Esc from Files
            // returns to the tab, not to a routine Enter would run. The tab
            // itself is preserved; only F8 selects the first one.
            screens::probe::clear_selection(self);
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
        if page == Page::Program && self.program.view == ProgramView::Code && self.editing_allowed()
        {
            self.program.focus_editor = true;
        }
        if page == Page::Machine {
            // Entering MAIN while the panel sits in MDI is also the pane
            // appearing, so it takes the keyboard like a mode entry does. It
            // only asks here: a click elsewhere on the page gives it up again
            // and the flag is one-shot.
            self.mdi.focus = self.snapshot.panel.mode.mode() == Some(PanelMode::Mdi);
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

    /// Keep the MDI buffer across restarts: the blocks are the operator's own
    /// machine commands, and recalling one is why the buffer exists.
    pub fn remember_mdi_history(&mut self) {
        self.preferences.mdi_history = self.mdi.block_texts();
        self.preferences_dirty = true;
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
    /// Ask for the preferences file to be written on the next save window.
    pub fn touch_preferences(&mut self) {
        self.preferences_dirty = true;
    }

    /// Persist operator-owned probe settings after a committed UI edit. Serde
    /// skips the profile-owned tool number and controller-owned calibration.
    pub fn remember_probe_params(&mut self) {
        let params = self.probe.params.clone();
        if self.preferences.probe.as_ref() != Some(&params) {
            self.preferences.probe = Some(params);
            self.preferences_dirty = true;
        }
    }

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

    /// End the session because the cabinet was switched off, not because the
    /// operator asked. Deliberately skips the idle guard, the discard prompt and
    /// the confirm dialog: the machine power is going away regardless, and a
    /// modal here would leave the HMI running while LinuxCNC is being shut down.
    /// This is the only path that may quit while a program runs.
    pub fn request_machine_off(&mut self) {
        log::info!("machine-on input dropped; ending the session");
        self.quit_requested = true;
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

    /// A dialog or the manual tool-change prompt owns the keyboard.
    ///
    /// Page-local key handlers must not move, edit or load the control
    /// underneath it: the dialog's button holds focus and takes Enter.
    pub fn overlay_open(&self) -> bool {
        self.modal.is_some()
            || self.messages_dialog
            || self.help.dialog.is_some()
            || self.snapshot.tool_change.status == linc_core::model::ToolChangeStatus::ManualPrompt
    }

    pub fn escape(&mut self) {
        if self.messages_dialog {
            self.messages_dialog = false;
            return;
        }
        if self.modal.is_some() {
            self.modal = None;
            return;
        }
        // The Help viewer owns Escape while it is open: closing it must not
        // fall through to the global program reset, which would stop a running
        // program behind the reader's back.
        if self.help.dialog.is_some() {
            self.help.close_dialog();
            return;
        }
        // The top-level Help page is a view, not the machine: Esc leaves it for
        // the page it came from. It must not fall through to the program reset;
        // the next Esc, on that page, is the one that stops a running program.
        if self.page == Page::Help {
            self.set_page(self.help_return_page);
            return;
        }
        // The Diagnostics page is a view too: Esc leaves it for the page it came
        // from, so an operator who opened it to look at a log does not stop the
        // program that is cutting behind it. The next Esc, on that page, is the
        // one that resets.
        if self.page == Page::Diagnostics {
            self.set_page(self.diagnostics_return_page);
            return;
        }
        // A latched probe trip is cleared by the controller reset alone. Esc
        // is that reset here, the same key that resets a running program.
        if self.snapshot.probe_trip_active() {
            self.clear_probe_trip();
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
        match self.page {
            Page::Files => self.set_page(self.files_return_page),
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

    pub fn sync(&mut self) {
        self.poll_backend();
        let mode = self.snapshot.panel.mode.clone();
        if self.last_mode.as_ref() != Some(&mode) {
            self.on_mode_change(mode.clone());
            self.last_mode = Some(mode);
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

    /// Enter a page that is coming back after its mode went away. The
    /// destination policy is unchanged (`last_non_mode_page` still decides),
    /// but a restored Probe page is an entry like any other: nothing may come
    /// back selected for Enter to run.
    fn restore_page(&mut self, page: Page) {
        if page == Page::Probe {
            screens::probe::clear_selection(self);
        }
        self.page = page;
    }

    fn on_mode_change(&mut self, mode: PanelModeValidity) {
        let Some(new_mode) = mode.mode() else {
            if self.page.is_mode_driven() {
                self.restore_page(self.last_non_mode_page);
            }
            return;
        };
        let prior_mode = self.last_mode.as_ref().and_then(PanelModeValidity::mode);
        if prior_mode.map(PanelMode::is_manual).unwrap_or(false)
            && !new_mode.is_manual()
            && self.page == Page::Manual
        {
            self.restore_page(self.last_non_mode_page);
        }
        if matches!(
            prior_mode,
            Some(PanelMode::Home) | Some(PanelMode::ProgHome)
        ) && !matches!(new_mode, PanelMode::Home | PanelMode::ProgHome)
            && self.page == Page::Homing
        {
            self.restore_page(self.last_non_mode_page);
        }
        match new_mode {
            PanelMode::Mdi => {
                // MDI is not a page: the panel position opens MAIN, whose
                // program card the mode swaps for the MDI pane.
                self.page = Page::Machine;
                if self.profile.config.mdi.auto_focus {
                    self.mdi.focus = true;
                }
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
                // Selecting AUTO brings up MAIN from any page: the run is
                // started from the machine page, whose toolpath and program
                // cards carry the active line. Leaving MDI swaps the pane back
                // for the normal read-only program card. This runs on the mode
                // change only, so navigating away afterwards is left alone.
                self.page = Page::Machine;
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
        // Re-arm the follow only when a run starts. Doing it on every poll
        // overrode a line the operator had just picked with Ctrl+G or the
        // arrows, so the selection snapped back to the running line.
        if self.snapshot.task.running && !self.program_was_running {
            self.program.follow_current = true;
        }
        self.program_was_running = self.snapshot.task.running;
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

    /// Copy the controller's measured #3032 value into the next parameter block.
    /// The field is read-only in the UI, so an open editor for another parameter
    /// keeps its own buffer and cannot be clobbered by this synchronization.
    pub(crate) fn sync_probe_calibration(&mut self) {
        let Some(offset) = self.snapshot.probe.calibration_offset else {
            return;
        };
        if self.probe.calibration_offset_seen == Some(offset) {
            return;
        }
        self.probe.params.calibration_offset = offset;
        self.probe.calibration_offset_seen = Some(offset);
    }

    fn sync_probe(&mut self) {
        // The backend's legacy selected_wcs is not an Apply target. Both the
        // display and Apply follow the controller's actual active work offset.
        self.probe.wcs = self
            .snapshot
            .offsets
            .active_index
            .filter(|index| (1..=9).contains(index))
            .map(linc_core::model::wcs_name)
            .unwrap_or_else(|| "--".into());
        self.sync_probe_calibration();
        if self.snapshot.probe.enabled {
            if let Some(result) = self.snapshot.probe.last_result.as_ref() {
                if self.probe.last_run.is_none() {
                    self.probe.last_run = Some(Instant::now());
                    self.probe.last_run_operation = Some(result.operation.clone());
                }
            }
        }
    }
    /// Keep a short trace of every axis's following error for MAIN's bottom row,
    /// and the run's worst and mean values for its header.
    ///
    /// Sampled as the snapshot arrives rather than per frame: the poll is the
    /// display's own rate, and a frame-driven trace would stretch and squeeze
    /// with the renderer's speed instead of with time. The worst and the mean
    /// start again with each program, so a fresh run is never reported against
    /// the last one; jogging and MDI between programs count towards them.
    fn sample_following_error(&mut self) {
        const SAMPLES: usize = 240;
        let running = self.snapshot.program.running;
        if running && !self.following_run_active {
            self.following_run_max = linc_core::model::AxisMap::new();
            self.following_run_abs_sum = linc_core::model::AxisMap::new();
            self.following_run_samples = linc_core::model::AxisMap::new();
        }
        self.following_run_active = running;
        let values: Vec<(String, f64)> = self
            .snapshot
            .machine
            .following_error
            .iter()
            .map(|(axis, value)| (axis.to_string(), *value))
            .collect();
        for (axis, value) in values {
            if self.following_history.get(&axis).is_none() {
                self.following_history
                    .insert(axis.clone(), std::collections::VecDeque::new());
            }
            if let Some(trace) = self.following_history.get_mut(&axis) {
                trace.push_back(value);
                while trace.len() > SAMPLES {
                    trace.pop_front();
                }
            }
            let worst = self.following_run_max.get(&axis).copied().unwrap_or(0.0);
            self.following_run_max.insert(axis.clone(), worst.max(value.abs()));
            let sum = self
                .following_run_abs_sum
                .get(&axis)
                .copied()
                .unwrap_or(0.0);
            self.following_run_abs_sum.insert(axis.clone(), sum + value.abs());
            let count = self.following_run_samples.get(&axis).copied().unwrap_or(0);
            self.following_run_samples.insert(axis, count + 1);
        }
    }

    /// The worst and mean absolute following error for `axis` since the last
    /// program start.
    pub fn following_error_summary(&self, axis: &str) -> (f64, f64) {
        let samples = self.following_run_samples.get(axis).copied().unwrap_or(0);
        if samples == 0 {
            return (0.0, 0.0);
        }
        let worst = self.following_run_max.get(axis).copied().unwrap_or(0.0);
        let sum = self
            .following_run_abs_sum
            .get(axis)
            .copied()
            .unwrap_or(0.0);
        (worst, sum / samples as f64)
    }

    pub fn active_offset_values(&self) -> Option<&linc_core::model::AxisMap<f64>> {
        active_offset(&self.snapshot)
    }

    /// The finite steps the INI declares.
    pub fn increments(&self) -> &[(String, f64)] {
        &self.jog_increments
    }

    pub fn jog_increment_index(&self) -> usize {
        self.jog_increment_index
            .min(self.increments().len().saturating_sub(1))
    }

    /// Selected increment in machine units; 0.0 means continuous jog.
    pub fn jog_increment_value(&self) -> f64 {
        crate::state::jog_increment(self.increments(), self.jog_increment_index()).1
    }

    pub fn jog_increment_label(&self) -> Option<String> {
        crate::state::jog_increment(self.increments(), self.jog_increment_index())
            .0
            .into()
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
        // The frame is the controller's offset, program or not. A jog screen that
        // has never loaded a program still has to place the tool at its machine
        // position: with preview_offset left at None, live_trail_shift collapses
        // to work - machine, and the marker is drawn on the tool's work
        // coordinate - machine home once the operator goes to WCS zero, and off
        // by the whole offset before that.
        let offset = self.preview_offset(&text);
        self.program.preview_offset = offset.clone();
        self.program.preview_offsets_seen = self.snapshot.offsets.revision;
        self.program.preview_offsets_rows = offsets_signature(&self.snapshot.offsets.offsets);
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
        let mut preview = parse_preview(&text, &axes, offset.as_ref(), self.tool_tip());
        // The first rapid is the approach from the live tool position to the
        // program start. Keep the program geometry; do not draw that connector.
        preview.trim_approach(self.tool_tip());
        let valid = preview.error.is_none();
        if valid {
            self.program.last_valid_preview = preview.clone();
        }
        self.program.preview = preview;
    }

    /// The work offset the preview is drawn with, in machine coordinates: the
    /// system the program selects, else the controller's active one.
    fn preview_offset(&self, text: &str) -> Option<linc_core::model::AxisMap<f64>> {
        let index = linc_core::gcode::program_wcs(text).or(self.snapshot.offsets.active_index)?;
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
    /// `Ctrl+B`: dimensions only -> + machine box -> + program box -> off.
    pub fn cycle_bounds(&mut self) {
        self.graph_style.bounds_mode = (self.graph_style.bounds_mode + 1) % 4;
        self.preferences.bounds_mode = Some(self.graph_style.bounds_mode);
        self.preferences_dirty = true;
        let label = match self.graph_style.bounds_mode {
            0 => "bounds: dimensions",
            1 => "bounds: + machine",
            2 => "bounds: + program box",
            _ => "bounds: off",
        };
        self.toast(label, theme::MUTED);
    }

    /// `Ctrl+M`: mirror the lathe's X about the Z centreline of the turning
    /// view. Screen only; the model keeps its coordinates.
    pub fn cycle_lathe_mirror(&mut self) {
        let on = !self.graph_style.lathe_mirror;
        self.graph_style.lathe_mirror = on;
        self.preferences.lathe_mirror = Some(on);
        self.preferences_dirty = true;
        // The pan was aimed at the picture the other way round, so re-frame:
        // a flipped part that keeps the old pan can slide off the card.
        self.program.fit_requested = true;
        self.manual.fit = true;
        self.toast(
            if on {
                "lathe X: mirrored"
            } else {
                "lathe X: not mirrored"
            },
            theme::MUTED,
        );
    }

    /// `Ctrl+P`: REMAINING. The live trail goes away and the part of the
    /// program path the machine has already run is eaten away, block by
    /// completed block. A view mode: no coordinate, offset or controller state
    /// changes, and the eaten blocks come back with the mode.
    pub fn cycle_remaining(&mut self) {
        let on = !self.graph_style.remaining;
        self.graph_style.remaining = on;
        self.preferences.remaining = Some(on);
        self.preferences_dirty = true;
        self.toast(
            if on {
                "remaining: path eaten behind the tool"
            } else {
                "remaining: off, full path"
            },
            theme::MUTED,
        );
    }

    /// `Ctrl+E`: read the lathe's X as a radius or a diameter. Presentation
    /// only: the X position the controller reports is a radius, and every
    /// command still carries that number.
    pub fn cycle_lathe_diameter(&mut self) {
        let diameter = !self.lathe_diameter;
        self.lathe_diameter = diameter;
        self.preferences.lathe_diameter = Some(diameter);
        self.preferences_dirty = true;
        self.toast(
            if diameter {
                "lathe X: diameter (2X)"
            } else {
                "lathe X: radius (X)"
            },
            theme::MUTED,
        );
    }

    /// Whether the X readouts show a diameter. A mill's X is a position, never
    /// a diameter, so the flag only lives on a lathe.
    pub fn reads_x_as_diameter(&self) -> bool {
        self.lathe_diameter && self.profile.variant == MachineVariant::Lathe
    }

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
            // No placeholder zoom or pan here: the fit recomputes both from the
            // card's rect, and writing the old defaults first only painted a
            // bogus "ZOOM 1.0x" in the card's readout for the frame before the
            // fit ran. That one-frame value was the whole visible effect of
            // Ctrl+0 on a view that was already fitted.
            self.program.fit_requested = true;
        }
    }

    /// Pan and zoom the toolpath on the page in front.
    ///
    /// The chords are hover-scoped, so the page decides which view state they
    /// move. A view the operator has moved stops auto-fitting: while it is on,
    /// the fit is recomputed every frame and would undo the press.
    fn pan_view(&mut self, pan: Vec2, zoom: f32) {
        let (zoom_out, pan_out, auto_fit) = match self.page {
            Page::Manual | Page::Homing => (
                &mut self.manual.zoom,
                &mut self.manual.pan,
                &mut self.manual.auto_fit,
            ),
            Page::Offsets => (
                &mut self.offsets.preview_zoom,
                &mut self.offsets.preview_pan,
                &mut self.offsets.preview_auto_fit,
            ),
            _ => (
                &mut self.program.graph_zoom,
                &mut self.program.graph_pan,
                &mut self.program.graph_auto_fit,
            ),
        };
        // Zoom about the card's centre, not the point the projection happens to
        // anchor on: the projection keeps the screen point at
        // `rect.center() + pan` fixed, so carrying the pan with the factor is
        // what keeps the part where the operator sees it instead of walking it
        // off the card. The arrow pan is a screen distance and comes after.
        let before = *zoom_out;
        let after = (before * zoom).clamp(linc_view::MIN_ZOOM, linc_view::MAX_ZOOM);
        *pan_out *= after / before;
        *zoom_out = after;
        *pan_out += pan;
        *auto_fit = false;
    }

    pub fn clear_trail(&mut self) {
        // View-only action. The backend is not asked to change machine state;
        // the live trail is re-seeded at the current tool tip in the local view.
        // The backend applies the clear on its next conversion, so the next few
        // snapshots still carry the old points; `trail_cleared` masks them until
        // either that clear lands or the tool moves.
        let tip = self.tool_tip();
        self.snapshot.live_trail =
            std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(tip, false)]);
        self.snapshot.tool_tip = Some(tip);
        self.trail_cleared = true;
        self.trail_cleared_tip = Some(tip);
        self.send(MachineCommand::ClearTrail);
        self.toast("live trail cleared", theme::MUTED);
    }

    pub fn focus_nav(&mut self) {
        self.focus_region = FocusRegion::Nav;
        self.nav_focus = self
            .nav
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
    }

    /// `egui-winit` turns the platform clipboard shortcut (Ctrl+X) into
    /// `Event::Cut`, not a key event, so the Probe page's X data reset never
    /// sees it. Hand the shortcut back to the Probe page as the key event it
    /// expects; leave the clipboard event alone while a text field can use it,
    /// and never synthesize the chord for any other page. Refresh (Ctrl+R) and
    /// redo (Ctrl+Y) never need this because they are not clipboard shortcuts.
    fn hand_cut_to_page(&mut self, ui: &egui::Ui) {
        // Only the Probe page binds Ctrl+X. Every other page keeps the
        // clipboard event, so Ctrl+X stays a text-editing chord there and the
        // Files page's delete is Backspace.
        if self.page != Page::Probe {
            return;
        }
        if ui.ctx().text_edit_focused() || self.overlay_open() {
            return;
        }
        ui.input_mut(|input| {
            if !input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Cut))
            {
                return;
            }
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Cut));
            input.events.push(egui::Event::Key {
                key: CUT_SHORTCUT.key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: CUT_SHORTCUT.modifiers,
            });
        });
    }

    pub fn handle_keyboard(&mut self, ui: &mut egui::Ui) {
        self.hand_cut_to_page(ui);
        // A modal messages dialog owns the keyboard: Enter/Escape close it and
        // the page keys scroll it. Nothing behind it may react to a key.
        if self.messages_dialog {
            let page = ui.ctx().content_rect().height() * 0.7;
            let mut close = false;
            let mut scroll = 0.0;
            ui.input_mut(|input| {
                if input.consume_key(self.keymap.back.modifiers, self.keymap.back.key)
                    || input.consume_key(self.keymap.activate.modifiers, self.keymap.activate.key)
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
            self.messages_scroll = scroll;
            if close {
                self.messages_dialog = false;
            }
            return;
        }
        // Every other dialog owns the keyboard too. Its focused buttons and
        // fields handle Enter, arrows and typing during `draw`; the shell only
        // keeps Escape for a dismissible dialog. Without this, F5-F8, Ctrl+D/H,
        // Ctrl+Q and the page keys could act on the page behind the dialog.
        if self.overlay_open() {
            let escape = ui.input_mut(|input| {
                input.consume_key(self.keymap.back.modifiers, self.keymap.back.key)
            });
            if escape && (self.modal.is_some() || self.help.dialog.is_some()) {
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
                    self.keymap.mdi_line_up.modifiers,
                    self.keymap.mdi_line_up.key,
                ) {
                    mdi_line_up = true;
                }
                if input.consume_key(
                    self.keymap.mdi_line_down.modifiers,
                    self.keymap.mdi_line_down.key,
                ) {
                    mdi_line_down = true;
                }
                if input.consume_key(
                    self.keymap.mdi_history_up.modifiers,
                    self.keymap.mdi_history_up.key,
                ) {
                    mdi_up = true;
                }
                if input.consume_key(
                    self.keymap.mdi_history_down.modifiers,
                    self.keymap.mdi_history_down.key,
                ) {
                    mdi_down = true;
                }
                if input.consume_key(
                    self.keymap.mdi_buffer_delete.modifiers,
                    self.keymap.mdi_buffer_delete.key,
                ) {
                    mdi_delete = true;
                }
                if input.consume_key(
                    self.keymap.program_run_from_line.modifiers,
                    self.keymap.program_run_from_line.key,
                ) {
                    mdi_run = true;
                }
                if input.consume_key(self.keymap.goto_line.modifiers, self.keymap.goto_line.key) {
                    mdi_goto = true;
                }
                if input.consume_key(self.keymap.save.modifiers, self.keymap.save.key) {
                    mdi_save = true;
                }
                // `Esc` keeps its global meanings (a dialog, a latched probe
                // trip, a running program reset). The pane only takes the
                // clear chord itself when the profile moved it off the back
                // key; otherwise that path clears the input after the global
                // checks have declined the key.
                if self.keymap.mdi_clear != self.keymap.back
                    && input.consume_key(self.keymap.mdi_clear.modifiers, self.keymap.mdi_clear.key)
                {
                    mdi_clear = true;
                }
                // Enter issues the block. The binding is configurable, so the
                // pane test comes first and the mode flag second: consume_key
                // swallows the key whatever the rest of the condition says.
                if self.profile.config.mdi.execute_on_enter
                    && input.consume_key(self.keymap.mdi_issue.modifiers, self.keymap.mdi_issue.key)
                {
                    mdi_issue = true;
                }
            }
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
            // Page-local: the square is CODE/SPLIT where there is a program
            // view and the following-error traces on MAIN. Shortcuts that mean
            // nothing on the page in front of the operator are not consumed, so
            // they cannot shadow another page's command.
            if matches!(self.page, Page::Program | Page::Machine)
                && input.consume_key(
                    self.keymap.program_cycle_view.modifiers,
                    self.keymap.program_cycle_view.key,
                )
            {
                if self.page == Page::Program {
                    cycle_view = true;
                } else {
                    machine_following = true;
                }
            }
            // Page-local to the pages that draw a toolpath.
            if matches!(
                self.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing
            ) {
                if input.consume_key(self.keymap.graph_fit.modifiers, self.keymap.graph_fit.key) {
                    graph_fit = true;
                }
                if input.consume_key(
                    self.keymap.graph_clear.modifiers,
                    self.keymap.graph_clear.key,
                ) {
                    graph_clear = true;
                }
                if input.consume_key(
                    self.keymap.program_run_from_line.modifiers,
                    self.keymap.program_run_from_line.key,
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
                self.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing | Page::Offsets
            );
            // Zoom is page-scoped, not hover-scoped: it collides with nothing a
            // text field uses, and requiring the pointer over the card made the
            // chord work only when the trackball happened to be there - the
            // reported sometimes. Ctrl+0 was always page-scoped, so the two agree
            // now.
            if view_page {
                let k = &self.keymap;
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
                let zoom_out =
                    input.consume_key(k.view_zoom_out.modifiers, k.view_zoom_out.key);
                if zoom_out {
                    view_zoom /= VIEW_ZOOM_STEP;
                }
                // Page-scoped like the zoom: Ctrl+P is no text field's chord and
                // no navigation key, so it works wherever a toolpath is drawn.
                if input.consume_key(k.remaining.modifiers, k.remaining.key) {
                    remaining = true;
                }
            }
            let view_hovered = view_page && toolpath_hovered;
            let view_pan_keys = view_hovered && !text_focused;
            if view_pan_keys {
                let k = &self.keymap;
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
            if input.consume_key(self.keymap.tools.modifiers, self.keymap.tools.key) {
                nav_page = Some(Page::Tools);
            }
            // The jog screens carry no button for the limit override, so the
            // chord is the only way to free a joint parked on a switch.
            if input.consume_key(
                self.keymap.limits_override.modifiers,
                self.keymap.limits_override.key,
            ) {
                self.send(linc_core::command::MachineCommand::ToggleLimitsOverride);
            }
            if input.consume_key(self.keymap.offsets.modifiers, self.keymap.offsets.key) {
                if self.page == Page::Probe {
                    if linc_core::probe::ProbeCategory::ALL.last().copied()
                        == Some(self.probe.category)
                    {
                        probe_exit = true;
                    } else {
                        probe_next = true;
                    }
                } else if self.page == Page::Offsets
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
            let editing_program = self.page == Page::Program && self.editing_allowed();
            if !editing_program
                && input.consume_key(
                    self.keymap.diagnostics.modifiers,
                    self.keymap.diagnostics.key,
                )
            {
                nav_page = Some(Page::Diagnostics);
            }
            if input.consume_key(self.keymap.help.modifiers, self.keymap.help.key) {
                nav_page = Some(Page::Help);
            }
            if !text_editing
                && !self.overlay_open()
                && input.consume_key(self.keymap.messages.modifiers, self.keymap.messages.key)
            {
                open_messages = true;
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
            // Page-local commands: test the page before `consume_key`, or the
            // key is swallowed on every other page (and in every text field).
            // This is the same rule the MDI binding below documents.
            if self.page == Page::Program
                && input.consume_key(self.keymap.save.modifiers, self.keymap.save.key)
            {
                save = true;
            }
            if self.page == Page::Help
                && input.consume_key(self.keymap.find.modifiers, self.keymap.find.key)
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
            if self.page == Page::Files
                && input.consume_key(self.keymap.refresh.modifiers, self.keymap.refresh.key)
            {
                refresh = true;
            }
            // Undo/redo belong to the focused G-code editor. Every other text
            // field keeps the chords for its own undo stack.
            if self.page == Page::Program && self.program.focus_editor {
                if input.consume_key(self.keymap.undo.modifiers, self.keymap.undo.key) {
                    undo = true;
                }
                if input.consume_key(self.keymap.redo.modifiers, self.keymap.redo.key) {
                    redo = true;
                }
            }
            // An edit or a page popup owns Escape: it cancels the edit (or
            // closes the selector) rather than leaving the page.
            let editing_in_place = (self.page == Page::Tools
                && (self.tools.editing || self.tools.kind_popup.is_some()))
                || (self.page == Page::Offsets
                    && (self.offsets.editing || self.offsets.dro_edit.is_some()))
                || (self.page == Page::Files && self.files.rename.is_some())
                || (self.page == Page::Probe
                    && (self.probe.param_editing
                        || self.probe.param_focused
                        || self.probe.dro_edit.is_some()))
                || self.help.dialog.is_some();
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
            if matches!(self.page, Page::Program | Page::Machine)
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
            if matches!(
                self.page,
                Page::Machine | Page::Program | Page::Manual | Page::Homing | Page::Offsets
            ) && input.consume_key(
                self.keymap.toggle_bounds.modifiers,
                self.keymap.toggle_bounds.key,
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
                    self.keymap.toggle_lathe_mirror.modifiers,
                    self.keymap.toggle_lathe_mirror.key,
                )
            {
                toggle_lathe_mirror = true;
            }
            if self.profile.variant == MachineVariant::Lathe
                && input.consume_key(
                    self.keymap.toggle_lathe_diameter.modifiers,
                    self.keymap.toggle_lathe_diameter.key,
                )
            {
                toggle_lathe_diameter = true;
            }
            // The tool table pages its own cursor with these keys, so they must
            // reach the page instead of being consumed for tab navigation.
            let page_keys_belong_to_page = matches!(self.page, Page::Tools | Page::Files)
                || self.help.dialog.is_some()
                || (self.page == Page::Probe
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
            if self.page == Page::Help
                && !text_focused
                && input.consume_key(
                    self.keymap.help_search.modifiers,
                    self.keymap.help_search.key,
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
        if goto_line && matches!(self.page, Page::Program | Page::Machine) {
            self.modal = Some(crate::state::Modal::GoToLine {
                buffer: String::new(),
                mdi: false,
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
            self.machine_following = !self.machine_following;
        }
        if cycle_view {
            self.cycle_program_view();
            let view = self.program.view;
            self.remember_program_view(view);
        }
        if probe_exit {
            nav_page = Some(Page::Offsets);
        }
        if let Some(page) = nav_page {
            self.request_page(page);
        }
        if (help_search || find) && self.page == Page::Help && !self.overlay_open() {
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
            self.focus_region = FocusRegion::Page;
            self.page_focus_requested = true;
        }
        if open_messages {
            self.messages_dialog = true;
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
            self.modal = Some(crate::state::Modal::GoToLine {
                buffer: String::new(),
                mdi: true,
            });
        }
        if mdi_save {
            self.request_mdi_save();
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
                let page = self
                    .nav
                    .get(self.nav_focus)
                    .copied()
                    .unwrap_or(Page::Machine);
                self.request_page(page);
            }
        }
    }

    pub fn page_up(&mut self) {
        match self.page {
            Page::Diagnostics => {
                let tabs = DiagnosticsTab::visible(&self.profile);
                let index = tabs
                    .iter()
                    .position(|tab| *tab == self.diagnostics_tab)
                    .unwrap_or(0);
                self.diagnostics_tab = tabs[(index + tabs.len() - 1) % tabs.len()];
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
                let tabs = DiagnosticsTab::visible(&self.profile);
                let index = tabs
                    .iter()
                    .position(|tab| *tab == self.diagnostics_tab)
                    .unwrap_or(0);
                self.diagnostics_tab = tabs[(index + 1) % tabs.len()];
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
        // A fresh model each frame: a region that disappears must not linger.
        self.diag_regions.clear();
        let screen = ui.available_rect_before_wrap();
        self.diag_viewport = linc_core::diag::ViewportInfo {
            width: screen.width(),
            height: screen.height(),
            pixels_per_point: ui.ctx().pixels_per_point(),
            fullscreen: ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false)),
        };
        self.diag_frame_time_ms = ui.ctx().input(|input| input.unstable_dt * 1000.0);
        ui.painter().rect_filled(ui.max_rect(), 0.0, theme::SURFACE);
        let top_bar = egui::containers::Panel::top("linc-top")
            .exact_size(theme::TOP_BAR_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| self.top_bar(ui));
        self.note_region("top-bar", top_bar.response.rect);
        self.messages_strip(ui);
        let bottom_bar = egui::containers::Panel::bottom("linc-bottom")
            .exact_size(theme::BOTTOM_BAR_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(16, 4)),
            )
            .show(ui, |ui| self.bottom_bar(ui));
        self.note_region("bottom-bar", bottom_bar.response.rect);
        let nav = egui::containers::Panel::left("linc-nav")
            .exact_size(theme::NAV_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.nav(ui));
        self.note_region("nav", nav.response.rect);
        let central = egui::containers::CentralPanel::default()
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
                        Page::Homing => screens::homing::show(self, ui),
                        Page::Manual => screens::manual::show(self, ui),
                    },
                );
            });
        self.note_region("content", central.response.rect);
        // Panels are laid out against the context's screen rect, which can be
        // a few points larger than the rect the app's `Ui` was given; report
        // the extent the shell actually drew, so the model is self-consistent.
        let right = self
            .diag_regions
            .iter()
            .map(|region| region.x + region.width)
            .fold(0.0f32, f32::max);
        let bottom = self
            .diag_regions
            .iter()
            .map(|region| region.y + region.height)
            .fold(0.0f32, f32::max);
        self.diag_viewport.width = self.diag_viewport.width.max(right);
        self.diag_viewport.height = self.diag_viewport.height.max(bottom);
        self.draw_modal(ui.ctx());
        self.draw_tool_change_prompt(ui.ctx());
        crate::screens::help::draw_dialog(self, ui.ctx());
        crate::screens::common::messages_dialog(self, ui.ctx());
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
            let (state_word, state_color) = program_state_word(&self.snapshot.program);
            metric_colored(ui, "STATE", state_word.into(), 13.0, state_color);
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
                // Render rate beside the clock: the first number to ask about
                // when the panel feels slow.
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("{:.0} FPS", self.fps))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(if self.fps < 30.0 {
                            theme::WARNING
                        } else {
                            theme::MUTED
                        }),
                );
                ui.add_space(14.0);
                let (messages, alarms) = self.log_counts();
                let alarm_color = if alarms > 0 {
                    theme::ERROR
                } else {
                    theme::MUTED
                };
                ui.label(
                    RichText::new(format!("ALARM {alarms}"))
                        .monospace()
                        .size(theme::font_size(12.0))
                        .color(alarm_color),
                );
                ui.label(
                    RichText::new(format!("MSG {messages}"))
                        .monospace()
                        .size(theme::font_size(12.0))
                        .color(if messages > 0 {
                            theme::FG
                        } else {
                            theme::MUTED
                        }),
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

    /// The log the operator sees: the controller's alarms plus the HMI's own
    /// notices, newest first. Diagnostics and the status bars share this list
    /// so one of them cannot report messages the other shows as zero.
    pub fn logs(&self) -> Vec<Alarm> {
        let mut logs: Vec<Alarm> = self.snapshot.alarms.iter().cloned().collect();
        logs.extend(self.ui_log.iter().cloned());
        logs.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
        logs
    }

    /// A program is loaded and not idle at the interpreter: running, paused, or
    /// waiting for a tool change. This is the state in which Esc resets the
    /// program and the sidebar offers the shortcut.
    pub fn program_active(&self) -> bool {
        self.snapshot.task.running
            || self.snapshot.program.paused
            // `Unknown` before the first status poll is not a running program,
            // so require a loaded program for the interpreter-state test.
            || (self.snapshot.program.line_count > 0
                && self.snapshot.task.interp != linc_core::model::InterpState::Idle)
    }

    /// Rows for the SERVO LOAD pane, in the order `[ui] servo_load` lists them.
    /// Entries whose axis or spindle is absent are skipped, so a shared profile
    /// can name an axis the machine does not have.
    pub fn servo_load_rows(&self) -> Vec<(String, f64)> {
        let mut rows = Vec::new();
        for entry in &self.profile.config.ui.servo_load {
            let label = entry.trim().to_ascii_uppercase();
            if label == "S" {
                if let Some(load) = self
                    .snapshot
                    .spindles
                    .first()
                    .and_then(|spindle| spindle.load)
                {
                    rows.push((label, load));
                }
            } else if let Some(load) = self.snapshot.machine.servo_load.get(&label) {
                rows.push((label, *load));
            }
        }
        rows
    }

    /// The newest `limit` entries of [`Self::logs`], for cards that show a few
    /// rows every frame: bounded work per frame instead of cloning the whole
    /// log. Each source is already newest-first, so only `limit` entries of
    /// each are copied before the merge.
    pub fn recent_logs(&self, limit: usize) -> Vec<Alarm> {
        let mut logs: Vec<Alarm> = self.ui_log.iter().take(limit).cloned().collect();
        logs.extend(self.snapshot.alarms.iter().take(limit).cloned());
        logs.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
        logs.truncate(limit);
        logs
    }

    /// `(messages, alarms)` over [`Self::logs`], split once by
    /// `AlarmSeverity::is_alarm`: a warning is an alarm in the top bar and in
    /// the diagnostics table alike.
    pub fn log_counts(&self) -> (usize, usize) {
        let mut alarms = 0;
        let mut messages = 0;
        for entry in self.snapshot.alarms.iter().chain(self.ui_log.iter()) {
            if entry.severity.is_alarm() {
                alarms += 1;
            } else {
                messages += 1;
            }
        }
        (messages, alarms)
    }

    /// The live fields the bottom bar shows on its left, in reading order:
    /// program progress, homing, limits, a pending manual tool change and the
    /// axis with the most left to go. Empty entries are omitted rather than
    /// drawn as placeholders, so a quiet machine gives a quiet row.
    fn status_fields(&self) -> Vec<(String, Color32)> {
        let mut fields: Vec<(String, Color32)> = Vec::new();
        let program = &self.snapshot.program;
        if program.line_count > 0 {
            let line = program.executing_line().max(1);
            let percent = (program.progress.clamp(0.0, 1.0) * 100.0).round() as i32;
            fields.push((
                format!("LN {line}/{} {percent}%", program.line_count),
                theme::MUTED,
            ));
        }
        if let Some(field) = homing_field(&self.snapshot.machine.homed) {
            fields.push(field);
        }
        // A held limit switch is never quiet: it blocks motion and says why.
        if !self.snapshot.machine.limits.is_empty() {
            fields.push((
                format!("LIMIT {}", self.snapshot.machine.limits.join(" ")),
                theme::ERROR,
            ));
        }
        if let Some(field) = tool_change_field(&self.snapshot.tool_change) {
            fields.push(field);
        }
        if !self.shows_coordinate_status() {
            if let Some((axis, dtg)) = moving_axis(&self.snapshot.positions.axes) {
                fields.push((format!("DTG {axis} {dtg:+.3}"), theme::MUTED));
            }
        }
        fields
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
        // The rows carry their own gap: the list spacing the sidebar inherits
        // is 8 px a row, which put a band of dead space around every entry.
        ui.spacing_mut().item_spacing.y = theme::NAV_ROW_GAP;
        for (index, page) in nav.iter().enumerate() {
            let Some((_, label, keycap)) = NAV_ITEMS.iter().find(|(entry, _, _)| entry == page)
            else {
                continue;
            };
            // TOUCH PROBE is a subpage of OFFSETS; keep the parent marked in
            // the sidebar instead of leaving no entry highlighted.
            let selected =
                *page == self.page || (*page == Page::Offsets && self.page == Page::Probe);
            let focused = self.focus_region == FocusRegion::Nav && self.nav_focus == index;
            let geometry = nav_row_geometry(*page);
            // The target every row keeps, whatever it paints inside it.
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(theme::NAV_WIDTH - 20.0, geometry.height),
                Sense::click(),
            );
            let pill =
                egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), geometry.pill));
            let painter = ui.painter();
            painter.rect_filled(
                pill,
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
                    pill.expand(1.0),
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
            if geometry.icon {
                draw_nav_icon(
                    painter,
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 8.0, rect.center().y - 9.0),
                        Vec2::splat(18.0),
                    ),
                    *page,
                    icon_color,
                );
            }
            // A row with an icon holds its label and keycap on two lines beside
            // it; a one-line row starts at the pill's own inset, with no icon
            // to clear.
            let label_x = rect.left() + if geometry.icon { 34.0 } else { 12.0 };
            // Only a row with an icon is a two-line row. The secondary rows
            // carry a [keys] hint too, and painting it under the label is what
            // kept DIAGNOSTICS and G-CODE HELP two lines tall.
            let two_line = geometry.icon && !keycap.is_empty();
            let label_y = if two_line {
                rect.center().y - 6.0
            } else {
                rect.center().y
            };
            painter.text(
                egui::pos2(label_x, label_y),
                Align2::LEFT_CENTER,
                *label,
                FontId::proportional(theme::font_size(11.0)),
                if selected {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    theme::FG
                },
            );
            if two_line {
                painter.text(
                    egui::pos2(label_x, rect.center().y + 7.0),
                    Align2::LEFT_CENTER,
                    *keycap,
                    FontId::monospace(theme::font_size(8.5)),
                    if selected {
                        Color32::from_rgb(0x0b, 0x17, 0x22)
                    } else {
                        theme::MUTED
                    },
                );
            } else if !keycap.is_empty() {
                // A one-line row carries its chord at the pill's right edge, so
                // label and hint share the row instead of stacking: the labels
                // are short enough (DIAG, G-CODE) for both to fit.
                painter.text(
                    egui::pos2(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
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
                ui.memory_mut(|memory| memory.stop_text_input());
                // A nav click is the operator leaving whichever text field had
                // the keyboard. Without this the G-code editor kept focus (the
                // panel keyboard has no Tab), so the toolpath's pan chords and
                // the page keys stayed dead after navigating, while the nav row
                // drew its focus border and looked ready for the arrows.
                self.nav_focus = index;
                self.focus_region = FocusRegion::Nav;
                self.request_page(*page);
            }
        }
        // Minimal: the list ends where the rows do, and the SHORTCUTS frame
        // takes the rest of the column.
        ui.spacing_mut().item_spacing.y = theme::NAV_ROW_GAP;
        ui.add_space(theme::NAV_BOTTOM_GAP);
        theme::separator(ui);
        ui.add_space(theme::NAV_BOTTOM_GAP);
        // Page-local keyboard commands live here: the bottom bar had one row and
        // truncated the list, and the sidebar has the height. The list takes the
        // sidebar's remainder, less the frame's own margins and header.
        //
        // The pane gives the list every pixel the sidebar has left and scrolls
        // inside when the list is longer than that; a list that fits keeps the
        // frame at its content height. `available_height` is read here, outside
        // the frame, where the column really bounds it.
        let header = ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    "SHORTCUTS".to_string(),
                    FontId::proportional(theme::font_size(9.0)),
                    theme::MUTED,
                )
                .size()
                .y
        });
        // The frame's 6 px margin and 1 px stroke on each side, the header line
        // above the list, and the air that keeps the frame off the footer.
        const FRAME_CHROME: f32 = 6.0 * 2.0 + 1.0 * 2.0;
        const FOOTER_PAD: f32 = 4.0;
        let list_height = (ui.available_height() - FRAME_CHROME - header - FOOTER_PAD).max(60.0);
        let shortcuts = self.key_hints();
        if !shortcuts.is_empty() {
            Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(4)
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    // The list can be longer than the sidebar: scroll it, and keep
                    // the safety note under it on screen.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new("SHORTCUTS")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("sidebar-shortcuts")
                        .max_height(list_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // Half a text line between entries: the rows are dense enough
                            // to read as a list, with air that does not waste the pane.
                            let hint_line = ui
                                .painter()
                                .layout_no_wrap(
                                    "Ag".to_string(),
                                    egui::FontId::monospace(theme::font_size(9.0)),
                                    theme::MUTED,
                                )
                                .size()
                                .y;
                            for group in shortcuts.iter() {
                                if !group.title.is_empty() {
                                    // Also before the first title: the pane header is not a
                                    // group, and the first one was sitting right under it.
                                    ui.add_space(hint_line * 0.5);
                                    ui.label(
                                        RichText::new(group.title)
                                            .size(theme::font_size(8.0))
                                            .strong()
                                            .color(theme::MUTED),
                                    );
                                    ui.add_space(hint_line * 0.35);
                                }
                                for (index, (key, action)) in group.items.iter().enumerate() {
                                    // One line per command, exactly; `ui.horizontal`
                                    // inherited the 44 px touch target and left large air
                                    // between two 9 pt rows.
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(ui.available_width(), hint_line),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(key.clone())
                                                    .monospace()
                                                    .size(theme::font_size(9.0))
                                                    .color(theme::C3),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(*action)
                                                            .size(theme::font_size(9.0))
                                                            .color(theme::MUTED),
                                                    );
                                                },
                                            );
                                        },
                                    );
                                    if index + 1 < group.items.len() {
                                        ui.add_space(hint_line * 0.5);
                                    }
                                }
                            }
                        });
                });
        }
    }

    /// Keyboard commands available on the current page, grouped and titled for
    /// the sidebar. Rendered as hints only — they are not buttons.
    fn key_hints(&self) -> Vec<ShortcutGroup> {
        let k = &self.keymap;
        // The square is the key glyph for the view toggles: F10 alone flips
        // SPLIT/CODE, Ctrl+F10 cycles the toolpath view. Both sit at the top of
        // the pane so the display commands are the first things in reach.
        let square = "\u{25a2}";
        // The lathe display toggles: the label names what the key does next, so
        // the pane reads as an action rather than as the current state.
        let lathe_hints: Vec<(String, &'static str)> =
            if self.profile.variant == MachineVariant::Lathe {
                vec![
                    (
                        k.toggle_lathe_mirror.label(),
                        if self.graph_style.lathe_mirror {
                            "UNMIRROR"
                        } else {
                            "MIRROR"
                        },
                    ),
                    (
                        k.toggle_lathe_diameter.label(),
                        if self.lathe_diameter {
                            "RADIUS"
                        } else {
                            "DIAMETER"
                        },
                    ),
                ]
            } else {
                Vec::new()
            };
        let mut groups: Vec<ShortcutGroup> = Vec::new();
        if self.program_active() {
            groups.push(ShortcutGroup {
                title: "PROGRAM CONTROL",
                items: vec![(k.back.label(), "RESET")],
            });
        }
        let mut hints: Vec<(String, &'static str)> = Vec::new();
        match self.page {
            Page::Program => {
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: [
                        vec![
                            (
                                square.to_string(),
                                if self.program.view == ProgramView::Split {
                                    "CODE"
                                } else {
                                    "SPLIT"
                                },
                            ),
                            (format!("Ctrl+{square}"), "VIEW"),
                            (k.graph_fit.label(), "FIT"),
                            (k.graph_clear.label(), "CLEAR TRAIL"),
                            (k.toggle_bounds.label(), "BOUNDS"),
                            (k.remaining.label(), "REMAINING"),
                            (k.view_pan_hint(), "PAN"),
                            (k.view_zoom_hint(), "ZOOM"),
                        ],
                        lathe_hints.clone(),
                    ]
                    .concat(),
                });
                groups.push(ShortcutGroup {
                    title: "PROGRAM",
                    items: vec![
                        ("FILE".into(), "BROWSER"),
                        (k.program_run_from_line.label(), "RUN LINE"),
                        (k.goto_line.label(), "GOTO"),
                        (k.messages.label(), "MESSAGES"),
                        (k.save.label(), "SAVE"),
                        (k.undo.label(), "UNDO"),
                        (k.redo.label(), "REDO"),
                    ],
                });
            }
            Page::Machine => {
                // MAIN shows the same toolpath, so it carries the same view
                // commands, plus the two program actions that do not need the
                // editor in front (run from line and go to line).
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: [
                        vec![
                            (format!("Ctrl+{square}"), "VIEW"),
                            (k.graph_fit.label(), "FIT"),
                            (k.graph_clear.label(), "CLEAR TRAIL"),
                            (k.toggle_bounds.label(), "BOUNDS"),
                            (k.remaining.label(), "REMAINING"),
                            // The square swaps the feed/servo pair below the
                            // graph for the following-error traces, and names
                            // what the next press brings back.
                            (
                                square.to_string(),
                                if self.machine_following {
                                    "FEED"
                                } else {
                                    "F.ERROR"
                                },
                            ),
                            (k.view_pan_hint(), "PAN"),
                            (k.view_zoom_hint(), "ZOOM"),
                        ],
                        lathe_hints.clone(),
                    ]
                    .concat(),
                });
                if self.mdi_pane_visible() {
                    // While the pane is up its keys replace the MAIN ones:
                    // Enter and the arrows act on the buffer, Ctrl+R runs the
                    // selected block instead of a program line.
                    groups.push(ShortcutGroup {
                        title: "MDI",
                        items: vec![
                            (k.mdi_issue.label(), "ISSUE"),
                            ("Up/Down".into(), "BUFFER"),
                            (k.goto_line.label(), "GOTO"),
                            (k.program_run_from_line.label(), "RUN LINE"),
                            (k.mdi_buffer_delete.label(), "DELETE"),
                            (k.save.label(), "SAVE"),
                            (k.back.label(), "CLEAR"),
                        ],
                    });
                } else {
                    groups.push(ShortcutGroup {
                        title: "PROGRAM",
                        items: vec![
                            ("FILE".into(), "BROWSER"),
                            (k.program_run_from_line.label(), "RUN LINE"),
                            (k.goto_line.label(), "GOTO"),
                            (k.messages.label(), "MESSAGES"),
                        ],
                    });
                }
            }
            Page::Files => {
                // The square flips the right pane between the program directory
                // and the programs opened most recently; its label names what
                // the key gives you, like CODE/SPLIT on the Program page.
                let right_pane = if self.files.recent_shown {
                    "NC_FILES"
                } else {
                    "RECENT"
                };
                // Enter is contextual: a folder opens, a USB file copies into
                // NC_FILES, an NC or recent file loads.
                let (entries, selected, on_parent) = if self.files.usb_focused {
                    (
                        &self.files.usb_entries,
                        self.files.usb_selected,
                        self.files.usb_parent_selected,
                    )
                } else {
                    (
                        &self.files.nc_entries,
                        self.files.nc_selected,
                        self.files.nc_parent_selected,
                    )
                };
                let recent_focus = !self.files.usb_focused && self.files.recent_shown;
                let enter = if recent_focus {
                    // A recent row is a file path: there is no folder to open
                    // and nothing to copy into.
                    "LOAD"
                } else if on_parent || entries.get(selected).is_some_and(|entry| entry.is_dir) {
                    "OPEN"
                } else if self.files.usb_focused {
                    "COPY"
                } else {
                    "LOAD"
                };
                groups.push(ShortcutGroup {
                    title: "BROWSE",
                    items: vec![
                        ("\u{2190}/\u{2192}".into(), "PANE"),
                        (square.to_string(), right_pane),
                        ("Enter".into(), enter),
                    ],
                });
                // Copy, rename and delete act on the focused pane's directory
                // row; the recent list is not one, so only NEW is offered there.
                let file_items: Vec<(String, &'static str)> = if recent_focus {
                    vec![("N".into(), "NEW")]
                } else {
                    vec![
                        ("C".into(), "COPY"),
                        ("R".into(), "RENAME"),
                        ("N".into(), "NEW"),
                        (format!("Bksp/{}", k.files_delete.label()), "DELETE"),
                    ]
                };
                groups.push(ShortcutGroup {
                    title: "FILE",
                    items: file_items,
                });
                groups.push(ShortcutGroup {
                    title: "MEDIA",
                    items: vec![(k.refresh.label(), "REFRESH"), ("E".into(), "EJECT")],
                });
            }
            Page::Help => {
                hints.push((
                    format!("{} / {}", k.help_search.label(), k.find.label()),
                    "SEARCH",
                ));
                hints.push(("Enter".into(), "READ"));
                hints.push(("Bksp".into(), "BACK"));
                hints.push(("PgUp/PgDn".into(), "PAGE"));
            }
            Page::Diagnostics => hints.push(("PgUp/PgDn".into(), "TABS")),
            Page::Manual | Page::Homing => {
                hints.push((k.graph_fit.label(), "FIT"));
                hints.push((format!("Ctrl+{square}"), "VIEW"));
                hints.push((k.toggle_bounds.label(), "BOUNDS"));
                hints.push((k.graph_clear.label(), "CLEAR TRAIL"));
                hints.push((k.remaining.label(), "REMAINING"));
                hints.push((k.view_pan_hint(), "PAN"));
                hints.push((k.view_zoom_hint(), "ZOOM"));
                hints.extend(lathe_hints.clone());
                hints.push(("Arrows".into(), "SELECT"));
                // Only the step modes take the digits: a continuous JOG has no
                // increment to pick, and HOME/PROG.HOME share this page family.
                if self.page == Page::Manual
                    && matches!(
                        self.snapshot.panel.mode.mode(),
                        Some(PanelMode::Inc | PanelMode::Mpg)
                    )
                    && !self.increments().is_empty()
                {
                    hints.push((format!("1-{}", self.increments().len()), "INCREMENT"));
                }
                hints.push(("X/Y/Z".into(), "ZERO AXIS"));
                hints.push(("A".into(), "ZERO ALL"));
            }
            Page::Offsets => {
                hints.push(("Arrows".into(), "SELECT"));
                hints.push(("A".into(), "ACTIVATE"));
                hints.push(("X/Y/Z".into(), "EDIT DRO"));
                hints.push(("OFFSET".into(), "PROBE"));
                hints.push((k.toggle_bounds.label(), "BOUNDS"));
                hints.push((k.remaining.label(), "REMAINING"));
                hints.push((k.view_pan_hint(), "PAN"));
                hints.push((k.view_zoom_hint(), "ZOOM"));
                hints.extend(lathe_hints.clone());
            }
            Page::Probe => {
                use linc_core::probe::ProbeCategory;

                match self.probe.category {
                    ProbeCategory::ProbeHelp => groups.push(ShortcutGroup {
                        title: "PROBE HELP",
                        items: vec![
                            ("Up/Down".into(), "TOPIC"),
                            ("Left/Right".into(), "PAGE"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ],
                    }),
                    ProbeCategory::Calibrate => groups.push(ShortcutGroup {
                        title: "CALIBRATION KEYS",
                        items: vec![
                            ("1-4".into(), "HIGHLIGHT"),
                            ("Enter".into(), "RUN CALIBRATION"),
                            ("V".into(), "AVG XY / X / Y"),
                            ("Ctrl+P".into(), "DIMENSIONS"),
                            ("Ctrl+R".into(), "RESET CALIBRATION"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ],
                    }),
                    category => {
                        let mut items = vec![
                            ("1-9".into(), "HIGHLIGHT"),
                            ("Enter".into(), "RUN PROBE"),
                            ("W".into(), "POSITION / DIRECT"),
                            ("A".into(), "APPLY RESULT"),
                            ("Ctrl+P".into(), "PARAMETERS"),
                        ];
                        if category == ProbeCategory::EdgeAngle {
                            items.push(("R".into(), "APPLY ROTATION"));
                        }
                        items.extend([
                            ("X/Y/Z".into(), "EDIT VISIBLE DRO"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ]);
                        groups.push(ShortcutGroup {
                            title: "PROBE KEYS",
                            items,
                        });
                        groups.push(ShortcutGroup {
                            title: "RESULT KEYS",
                            items: vec![
                                ("U".into(), "UPDATE PARAMS"),
                                ("Ctrl+R".into(), "RESET ALL DATA"),
                                ("Ctrl+X/Y".into(), "RESET X/Y DATA"),
                            ],
                        });
                    }
                }
            }
            Page::Tools => {
                hints.push(("Arrows".into(), "SELECT"));
                hints.push(("Enter".into(), "EDIT"));
                hints.push(("T".into(), "LOAD + G43"));
            }
        }
        if !hints.is_empty() {
            groups.push(ShortcutGroup {
                title: "",
                items: hints,
            });
        }
        groups
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
                        .color(if errors > 0 {
                            theme::ERROR
                        } else {
                            theme::MUTED
                        }),
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
                        // and read in full in the messages dialog (M).
                        let text: String = alarm.message.chars().take(64).collect();
                        ui.label(RichText::new(text).size(size).color(colour));
                        ui.add_space(12.0);
                    }
                });
            });
    }

    /// The centre field of the bottom bar: the work position of every axis,
    /// then the same axes with the distance still to go.
    ///
    /// Two groups rather than interleaved pairs: X/Y/Z in one block reads as the
    /// position, and the DTG block lines up under it axis for axis. Painted on
    /// the Program page it replaces the single moving-axis DTG field the bar
    /// otherwise carries, so the distance is never shown twice.
    fn coordinate_status_line(&self) -> String {
        let width = self.coordinate_field_width();
        let mut position = String::new();
        let mut dtg = String::new();
        for axis in self.axes() {
            let Some(entry) = self.snapshot.positions.axes.get(&axis) else {
                continue;
            };
            position.push_str(&format!("{axis}{:+width$.3} ", entry.work, width = width));
            dtg.push_str(&format!("{axis}{:+width$.3} ", entry.dtg, width = width));
        }
        format!("{}  DTG: {}", position.trim_end(), dtg.trim_end())
    }

    /// Characters in one coordinate field, sign and decimals included.
    ///
    /// Sized by the largest number that can appear - the machine envelope and
    /// the loaded program's extents - never by what is on screen: a field that
    /// widened when an axis crossed 10.000 made the whole line shuffle while the
    /// machine moved and the distance-to-go counted down.
    fn coordinate_field_width(&self) -> usize {
        const DECIMALS: usize = 3;
        let mut extent: f64 = 0.0;
        for bounds in [
            self.machine_bounds(),
            self.program.last_valid_preview.bounds,
        ]
        .into_iter()
        .flatten()
        {
            for axis in 0..3 {
                extent = extent.max(bounds.min[axis].abs()).max(bounds.max[axis].abs());
            }
        }
        // Sign, the integer digits of the extent, the point, the decimals.
        1 + format!("{:.*}", DECIMALS, extent).len()
    }

    /// True where the bar shows the full coordinate line, so the single
    /// moving-axis DTG field steps aside instead of repeating it.
    fn shows_coordinate_status(&self) -> bool {
        self.page == Page::Program
    }

    /// Bottom bar: keyboard commands on the left, read-only machine status on
    /// the right. It carries no buttons.
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let size = theme::font_size(11.0);
        ui.horizontal(|ui| {
            let field = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                ui.label(RichText::new(text).monospace().size(size).color(color));
            };
            // Left: what the machine is doing, in fields that appear only when
            // they have something to say. The bottom bar carries live machine
            // state alone; nothing here is an action.
            for (text, color) in self.status_fields() {
                field(ui, text, color);
                ui.add_space(16.0);
            }

            // The program's own numbers - the work position it is cutting to
            // and the distance left to get there - in the left-hand flow, after
            // the machine fields. Only on the Program page; the jog screens
            // carry the same figures in their own readout, and the bar is
            // read-only everywhere.
            if self.shows_coordinate_status() {
                let bar = ui.max_rect();
                let text = self.coordinate_status_line();
                let galley = ui.painter().layout_no_wrap(
                    text,
                    FontId::monospace(size),
                    theme::FG,
                );
                // Never let it run into the left or the right block: the bar is
                // 1280 px wide at the minimum and those two need about 600.
                if galley.size().x + 24.0 < bar.width() * 0.5 {
                    // Left-aligned in the flow, right after the machine fields:
                    // the operator reads the bar left to right, and a centred
                    // block floats away from the line it belongs to.
                    let pos = egui::pos2(
                        ui.cursor().min.x,
                        bar.center().y - galley.size().y / 2.0,
                    );
                    ui.painter().galley(pos, galley, theme::FG);
                }
            }

            // Right: the read-only machine state.
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
                let (machine_text, machine_color) = machine_status_field(
                    self.snapshot.machine.on,
                    self.snapshot.meta.stale,
                );
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
                    override_readout(
                        self.snapshot.feed.feed_override,
                        self.snapshot.feed.rapid_override,
                        self.snapshot.feed.spindle_override,
                    ),
                    theme::MUTED,
                );
                if self.snapshot.limits_overridden {
                    ui.add_space(16.0);
                    limits_override_indicator(ui);
                }
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
        if let Some(fault) = self.snapshot.probe_trip.clone() {
            let text = format!(
                "PROBE TRIPPED — {} — MOTION AND MDI BLOCKED. RESET TO CLEAR.",
                fault.message
            );
            if theme::banner_action(ui, theme::ERROR, &text, "RESET") {
                self.clear_probe_trip();
            }
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
        let mut save_name: Option<String> = None;
        let mut save_wrap: Option<bool> = None;
        let mut save_confirm = false;
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
                    if let Some(choice) = screens::files::overwrite_dialog(ui, focus_default) {
                        overwrite = Some(choice);
                    }
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
                    if focus_default {
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
                }
            });
        if let Some(text) = goto_buffer {
            let mdi = matches!(self.modal, Some(Modal::GoToLine { mdi: true, .. }));
            self.modal = Some(Modal::GoToLine { buffer: text, mdi });
        }
        if goto_accept {
            let target = match self.modal.as_ref() {
                Some(Modal::GoToLine { buffer, mdi }) => {
                    buffer.trim().parse::<usize>().ok().map(|line| (line, *mdi))
                }
                _ => None,
            };
            if let Some((line, mdi)) = target {
                self.modal = None;
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
        Page::Diagnostics => {
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
        Page::Files | Page::Probe | Page::Homing | Page::Manual => {
            painter.rect_stroke(rect.shrink(3.0), 2.0, s, StrokeKind::Inside);
        }
    }
}

impl eframe::App for LincApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::apply_design_zoom(ui.ctx(), self.design_scale);
        let dt = ui.ctx().input(|input| input.stable_dt) as f32;
        self.fps = fps_step(self.fps, dt);
        self.apply_diag_actions();
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

/// Exponential moving average of the frame rate, biased so the first samples
/// settle quickly without flickering.
pub fn fps_step(previous: f32, dt: f32) -> f32 {
    let dt = dt.clamp(1.0 / 240.0, 1.0);
    let sample = 1.0 / dt;
    if previous <= 0.0 {
        sample
    } else {
        previous * 0.9 + sample * 0.1
    }
}

/// Parse a page name as the diagnostic tools spell it. Accepts the navigation
/// label and the enum-ish spelling, so `G-CODE HELP` and `HELP` both work.
pub fn page_from_name(name: &str) -> Option<Page> {
    let wanted = name
        .trim()
        .to_ascii_uppercase()
        .replace(['-', '_', ' '], " ");
    for page in [
        Page::Machine,
        Page::Program,
        Page::Files,
        Page::Tools,
        Page::Offsets,
        Page::Diagnostics,
        Page::Help,
        Page::Probe,
        Page::Homing,
        Page::Manual,
    ] {
        let label = page.label().to_ascii_uppercase().replace(['-', '_'], " ");
        if label == wanted || format!("{page:?}").to_ascii_uppercase() == wanted {
            return Some(page);
        }
    }
    None
}

/// The bottom bar's machine field.
///
/// A machine that is off is a fault, not a quiet state: it is painted in the
/// error colour, so a bar full of green does not hide it. Stale data claims
/// neither, and asks.
fn machine_status_field(on: bool, stale: bool) -> (String, egui::Color32) {
    if stale {
        ("MACHINE ?".to_string(), theme::WARNING)
    } else if on {
        ("MACHINE ON".to_string(), theme::SUCCESS)
    } else {
        ("MACHINE OFF".to_string(), theme::ERROR)
    }
}

/// Blinking red in the status line: the limit switches are being ignored.
///
/// The jog screens have no override button any more, so this is the only sign
/// that the machine is free to drive through a switch. The text is painted on
/// both halves of the blink - only the colour alternates - so a capture or a
/// test always finds it.
pub fn limits_override_indicator(ui: &mut egui::Ui) {
    let bright = ((ui.input(|input| input.time) * 2.0) as i64).rem_euclid(2) == 0;
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(250));
    let colour = if bright {
        theme::ERROR
    } else {
        theme::ERROR.gamma_multiply(0.45)
    };
    ui.label(
        RichText::new("LIMITS OVERRIDE ON")
            .monospace()
            .size(theme::font_size(11.0))
            .color(colour),
    );
}

/// Bottom-bar override readout.
///
/// The order is the physical panel's — FEED, RAPID, SPINDLE — so the numbers
/// under the F, R and S keys read in the order the operator's hand finds them.
pub fn override_readout(feed: f64, rapid: f64, spindle: f64) -> String {
    format!(
        "OVR F{:.0} R{:.0} S{:.0}",
        feed * 100.0,
        rapid * 100.0,
        spindle * 100.0
    )
}

/// Navigation entries in display order, with their label and keycap hint.
///
/// Which of these actually appear is `[ui] pages` in the profile, so a machine
/// can hide the pages it does not use (a machine with no changer can hide
/// TOOLS, for example) without removing the screen from the binary.
/// Ordered to match the keyboard: MAIN (F5), FILE (F6), TOOL (F7), OFFSET
/// (F8). A page that has no function key follows the ones that do.
///
/// DIAGNOSTICS and G-CODE HELP are listed too: `configs/base.toml` names all
/// seven, so the operator can reach the diagnostics and help pages with the
/// arrows and Enter as well as with their `[keys]` chords (Ctrl+D, Ctrl+H).
/// They draw as one-line rows without an icon (see [`NAV_PRIMARY`]).
pub const NAV_ITEMS: [(Page, &str, &str); 6] = [
    (Page::Machine, "MACHINE", "MAIN"),
    (Page::Program, "PROGRAM", "FILE"),
    (Page::Tools, "TOOLS", "TOOL"),
    (Page::Offsets, "OFFSETS", "PROBE"),
    (Page::Diagnostics, "DIAG", "Ctrl+D"),
    (Page::Help, "G-CODE", "Ctrl+H"),
];

/// The navigation rows that carry an icon and a two-line label.
///
/// The four entries with a function key do: they are what the operator's hand
/// finds on the panel. DIAGNOSTICS and G-CODE HELP are reached by a chord or by
/// the arrows, so they draw as one-line rows without an icon — a third of the
/// attention, and in a six-row list the vertical space.
const NAV_PRIMARY: [Page; 4] = [Page::Machine, Page::Program, Page::Tools, Page::Offsets];

/// Geometry of one navigation row: the target the operator gets and the pill
/// painted inside it.
pub struct NavRowGeometry {
    /// Interaction height. Never below [`theme::MIN_TARGET`].
    pub height: f32,
    /// Painted pill. Shorter than `height`: what shrank is the padding, not
    /// the target.
    pub pill: f32,
    /// An icon beside a two-line label.
    pub icon: bool,
}

pub fn nav_row_geometry(page: Page) -> NavRowGeometry {
    if NAV_PRIMARY.contains(&page) {
        NavRowGeometry {
            height: theme::NAV_ROW_HEIGHT,
            pill: theme::NAV_PILL_PRIMARY,
            icon: true,
        }
    } else {
        NavRowGeometry {
            height: theme::NAV_ROW_HEIGHT,
            pill: theme::NAV_PILL_SECONDARY,
            icon: false,
        }
    }
}

/// Navigation entries to show, from `[ui] pages`.
///
/// An empty list means "all of them", so a profile that never mentions the key
/// still gets the full navigation.
fn nav_pages(configured: &[String]) -> Vec<Page> {
    if configured.is_empty() {
        return NAV_ITEMS.iter().map(|(page, _, _)| *page).collect();
    }
    configured.iter().map(|key| page_from_config(key)).collect()
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
        _ => Page::Machine,
    }
}

pub fn mode_page(mode: &PanelModeValidity) -> Option<Page> {
    match mode.mode()? {
        // MDI has no page of its own: it opens MAIN, where the mode swaps
        // the program card for the MDI pane.
        PanelMode::Mdi => Some(Page::Machine),
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

/// The numbers the preview's frame depends on, flattened in a stable order.
///
/// An offset row is a handful of floats; comparing them per poll is cheaper than
/// re-parsing the program to find out whether it needs re-parsing.
fn offsets_signature(rows: &[linc_core::model::WorkOffset]) -> Vec<f64> {
    let mut out = Vec::with_capacity(rows.len() * 3);
    for row in rows {
        for axis in ["X", "Y", "Z"] {
            out.push(row.values.get(axis).copied().unwrap_or(0.0));
        }
    }
    out
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
    metric_colored(ui, label, value, size, theme::FG);
}

/// A top-bar metric whose value carries a state colour: the label stays quiet,
/// the value is what has to be read at a glance.
fn metric_colored(ui: &mut egui::Ui, label: &str, value: String, size: f32, color: Color32) {
    ui.add_space(6.0);
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(RichText::new(value).monospace().size(size).color(color));
    });
}

/// The program state word for the top bar.
///
/// Paused is the state that needs attention - the machine is waiting with a
/// line in hand - so it is the only one that earns a warning colour. Idle and
/// running are both normal and stay quiet.
fn program_state_word(program: &ProgramState) -> (&'static str, Color32) {
    if program.paused {
        ("PAUSED", theme::WARNING)
    } else if program.running {
        ("RUNNING", theme::SUCCESS)
    } else {
        ("IDLE", theme::MUTED)
    }
}

/// Homing progress for the status line.
///
/// Moves fail closed until every axis is homed, so a partly homed machine is a
/// warning that names the axes it already trusts; the fully homed case is the
/// normal one and stays muted.
fn homing_field(homed: &AxisMap<bool>) -> Option<(String, Color32)> {
    if homed.is_empty() {
        return None;
    }
    let total = homed.len();
    let done: Vec<&str> = homed
        .iter()
        .filter(|(_, is_homed)| **is_homed)
        .map(|(axis, _)| axis)
        .collect();
    if done.len() == total {
        Some(("HOMED".into(), theme::MUTED))
    } else if done.is_empty() {
        Some(("NOT HOMED".into(), theme::WARNING))
    } else {
        Some((format!("HOMED {}", done.join(" ")), theme::WARNING))
    }
}

/// The axis with the most left to go, for the status line's DTG field.
///
/// A settled machine has no field; during a move or a jog the largest distance
/// wins, which is the axis the operator is watching.
fn moving_axis(axes: &AxisMap<PositionSet>) -> Option<(&str, f64)> {
    axes.iter()
        .filter(|(_, position)| position.dtg.abs() > 0.001)
        .max_by(|a, b| a.1.dtg.abs().total_cmp(&b.1.dtg.abs()))
        .map(|(axis, position)| (axis, position.dtg))
}

/// The field for a manual tool change that is waiting on the operator.
fn tool_change_field(tool_change: &ToolChangeState) -> Option<(String, Color32)> {
    if tool_change.status != ToolChangeStatus::ManualPrompt {
        return None;
    }
    let target = tool_change.target_tool.unwrap_or(tool_change.current_tool);
    Some((format!("TC T{target} PENDING"), theme::WARNING))
}

pub fn state_label(power: MachinePower) -> &'static str {
    power.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_follow_physical_mode() {
        // MDI opens MAIN, whose program card the mode swaps for the pane.
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Mdi)),
            Some(Page::Machine)
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
    fn the_program_state_word_reads_the_controller() {
        let mut program = ProgramState::default();
        assert_eq!(program_state_word(&program).0, "IDLE");
        program.running = true;
        assert_eq!(program_state_word(&program).0, "RUNNING");
        program.paused = true;
        assert_eq!(program_state_word(&program).0, "PAUSED");
    }

    #[test]
    fn homing_names_the_axes_it_cannot_trust() {
        let mut homed = AxisMap::new();
        assert!(homing_field(&homed).is_none(), "no axes, no field");
        homed.insert("X", false);
        homed.insert("Y", false);
        assert_eq!(homing_field(&homed).unwrap().0, "NOT HOMED");
        homed.insert("X", true);
        assert_eq!(homing_field(&homed).unwrap().0, "HOMED X");
        homed.insert("Y", true);
        assert_eq!(homing_field(&homed).unwrap().0, "HOMED");
    }

    #[test]
    fn the_status_line_names_the_axis_that_is_moving() {
        let mut axes = AxisMap::new();
        axes.insert(
            "X",
            PositionSet {
                dtg: 0.0,
                ..PositionSet::default()
            },
        );
        axes.insert(
            "Y",
            PositionSet {
                dtg: 0.0,
                ..PositionSet::default()
            },
        );
        assert!(moving_axis(&axes).is_none(), "a settled machine is quiet");
        axes.insert(
            "Y",
            PositionSet {
                dtg: -12.34,
                ..PositionSet::default()
            },
        );
        assert_eq!(moving_axis(&axes), Some(("Y", -12.34)));
        axes.insert(
            "X",
            PositionSet {
                dtg: 40.0,
                ..PositionSet::default()
            },
        );
        assert_eq!(moving_axis(&axes).map(|(axis, _)| axis), Some("X"));
    }

    #[test]
    fn a_pending_tool_change_is_named() {
        let mut tool_change = ToolChangeState::default();
        assert!(tool_change_field(&tool_change).is_none());
        tool_change.status = ToolChangeStatus::ManualPrompt;
        tool_change.target_tool = Some(7);
        assert_eq!(tool_change_field(&tool_change).unwrap().0, "TC T7 PENDING");
        // Without a target the current tool is the best name available.
        tool_change.target_tool = None;
        tool_change.current_tool = 3;
        assert_eq!(tool_change_field(&tool_change).unwrap().0, "TC T3 PENDING");
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use linc_core::model::{PanelModeValidity, ToolChangerKind};

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
"#
            .replace("{root}", &root.to_string_lossy()),
        )
        .unwrap();
        let profile = Profile::load(&root.join("sim.toml"), None).unwrap();
        (dir, profile)
    }

    /// A real profile (base.toml merged with a sim profile) with the two
    /// fields the Diagnostics turret tab is decided by, overridden.
    fn diagnostics_profile(turret_tab: bool, changer: ToolChangerKind) -> Profile {
        let (_dir, mut profile) = fixture();
        profile.config.diagnostics.turret_tab = turret_tab;
        profile.changer = changer;
        profile
    }

    #[test]
    fn diagnostics_turret_tab_needs_the_profile_key_and_stations() {
        let has_turret =
            |profile: &Profile| DiagnosticsTab::visible(profile).contains(&DiagnosticsTab::Turret);
        assert!(!has_turret(&diagnostics_profile(
            false,
            ToolChangerKind::Manual
        )));
        assert!(!has_turret(&diagnostics_profile(
            true,
            ToolChangerKind::Manual
        )));
        assert!(has_turret(&diagnostics_profile(true, ToolChangerKind::Atc)));
        assert!(has_turret(&diagnostics_profile(
            true,
            ToolChangerKind::Turret
        )));
        assert!(!has_turret(&diagnostics_profile(
            false,
            ToolChangerKind::Turret
        )));
        // Visible, the list is the whole strip in its usual order; hidden, it
        // is the same list minus the turret entry.
        assert_eq!(
            DiagnosticsTab::visible(&diagnostics_profile(true, ToolChangerKind::Turret)),
            DiagnosticsTab::ALL.to_vec()
        );
        let hidden = DiagnosticsTab::visible(&diagnostics_profile(false, ToolChangerKind::Atc));
        assert_eq!(hidden.len(), DiagnosticsTab::ALL.len() - 1);
        assert!(hidden.iter().all(|tab| DiagnosticsTab::ALL.contains(tab)));
    }

    #[test]
    fn diagnostics_navigation_and_body_skip_a_hidden_turret_tab() {
        let mut harness = ProbeHarness::ready(PanelMode::Auto, false, true, MachinePower::On);
        harness.app.profile.config.diagnostics.turret_tab = false;
        harness.app.profile.changer = ToolChangerKind::Turret;
        harness.app.set_page(Page::Diagnostics);

        harness.app.diagnostics_tab = DiagnosticsTab::Motion;
        harness.app.page_down();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Io);
        harness.app.page_up();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Motion);
        harness.app.page_up();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::HalNml);

        // Wrapping stays inside the visible list.
        harness.app.diagnostics_tab = DiagnosticsTab::Logs;
        harness.app.page_down();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Overview);
        harness.app.page_up();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Logs);

        // A remembered or restored turret tab never reaches the body.
        harness.app.diagnostics_tab = DiagnosticsTab::Turret;
        let output = harness.frame_full(Vec::new());
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Overview);
        let texts = painted_text(&output);
        assert!(!texts.iter().any(|(text, _)| text == "TURRET / LIVE TOOL"));
        assert!(texts.iter().any(|(text, _)| text == "OVERVIEW"));

        // Visible, the same tab is kept, drawn and navigated like any other.
        harness.app.profile.config.diagnostics.turret_tab = true;
        harness.app.diagnostics_tab = DiagnosticsTab::Turret;
        let output = harness.frame_full(Vec::new());
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Turret);
        assert!(painted_text(&output)
            .iter()
            .any(|(text, _)| text == "TURRET / LIVE TOOL"));
        harness.app.page_down();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Io);
        harness.app.page_up();
        assert_eq!(harness.app.diagnostics_tab, DiagnosticsTab::Turret);
    }

    fn make_app(mode: PanelMode) -> (tempfile::TempDir, LincApp) {
        let (dir, profile) = fixture();
        let ctx = egui::Context::default();
        let backend = Box::new(LocalBackend::new(profile.clone(), mode));
        let app = LincApp::new(profile, backend, &ctx);
        (dir, app)
    }

    /// The sidebar SHORTCUTS frame: the only 4 px-corner pane in the nav
    /// column. Its rect, so a test can see how tall the pane came out.
    fn shortcuts_frame(output: &egui::FullOutput) -> Option<egui::Rect> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(rect) => {
                    if rect.fill == theme::PANEL && rect.corner_radius.nw == 4 {
                        out.push(rect.rect);
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut rects);
        }
        rects
            .into_iter()
            .find(|rect| rect.left() < theme::NAV_WIDTH)
    }

    /// The bottom bar reads the overrides in the physical panel's order:
    /// FEED, RAPID, SPINDLE. The old format printed the spindle value in the
    /// RAPID slot and the rapid value in the SPINDLE slot.
    /// MACHINE OFF is a fault, not a quiet state: the bar paints it in the error
    /// colour, and stale data asks instead of guessing.
    #[test]
    fn the_machine_field_is_red_when_the_machine_is_off() {
        assert_eq!(
            machine_status_field(false, false),
            ("MACHINE OFF".to_string(), theme::ERROR)
        );
        assert_eq!(
            machine_status_field(true, false),
            ("MACHINE ON".to_string(), theme::SUCCESS)
        );
        assert_eq!(
            machine_status_field(false, true),
            ("MACHINE ?".to_string(), theme::WARNING)
        );
    }

    /// The header's worst and mean are the run's, and a new program starts
    /// them again: a fresh run must not be judged against the last one.
    #[test]
    fn the_following_error_summary_belongs_to_the_run() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        let feed = |harness: &mut ProbeHarness, value: f64| {
            harness
                .app
                .snapshot
                .machine
                .following_error
                .insert("X", value);
            harness.app.sample_following_error();
        };

        // Start from a cleared run: the harness has already sampled its own
        // snapshots while building the app.
        harness.app.following_run_max = linc_core::model::AxisMap::new();
        harness.app.following_run_abs_sum = linc_core::model::AxisMap::new();
        harness.app.following_run_samples = linc_core::model::AxisMap::new();
        harness.app.following_run_active = false;

        // No program yet: jogging still counts towards the numbers.
        feed(&mut harness, -0.002);
        feed(&mut harness, 0.004);
        let (worst, mean) = harness.app.following_error_summary("X");
        assert!((worst - 0.004).abs() < 1e-12, "worst {worst}");
        assert!((mean - 0.003).abs() < 1e-12, "mean {mean}");

        // A program starts: the numbers begin again.
        harness.app.snapshot.program.running = true;
        feed(&mut harness, 0.010);
        let (worst, mean) = harness.app.following_error_summary("X");
        assert!((worst - 0.010).abs() < 1e-12, "worst {worst}");
        assert!((mean - 0.010).abs() < 1e-12, "mean {mean}");

        // Still the same run: the numbers accumulate.
        feed(&mut harness, -0.006);
        let (worst, mean) = harness.app.following_error_summary("X");
        assert!((worst - 0.010).abs() < 1e-12, "worst {worst}");
        assert!((mean - 0.008).abs() < 1e-12, "mean {mean}");

        // The run ends and another starts: everything is cleared, so a quiet
        // new program does not inherit the old one's worst.
        harness.app.snapshot.program.running = false;
        feed(&mut harness, 0.001);
        harness.app.snapshot.program.running = true;
        feed(&mut harness, 0.001);
        let (worst, mean) = harness.app.following_error_summary("X");
        assert!((worst - 0.001).abs() < 1e-12, "worst {worst}");
        assert!((mean - 0.001).abs() < 1e-12, "mean {mean}");
    }

    /// The square key on MAIN swaps the feed/servo cards for the following-error
    /// traces, and swaps them back.
    #[test]
    fn the_square_key_shows_the_following_error_traces_on_main() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        let texts = |harness: &mut ProbeHarness| -> Vec<String> {
            painted_text(&harness.frame_full(Vec::new()))
                .into_iter()
                .map(|(text, _)| text)
                .collect()
        };
        let cards = texts(&mut harness);
        assert!(
            cards.iter().any(|text| text == "SERVO LOAD"),
            "the feed/servo pair is in front: {cards:?}"
        );

        harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
        assert!(harness.app.machine_following, "F10 shows the traces");
        let cards = texts(&mut harness);
        assert!(
            cards.iter().any(|text| text == "FOLLOWING ERROR"),
            "the traces replace both cards: {cards:?}"
        );
        assert!(
            !cards.iter().any(|text| text == "SERVO LOAD"),
            "and the servo card is gone: {cards:?}"
        );
        // Three labelled groups - value, worst, mean - one number per axis in
        // each of them.
        // The live values have no label and the mean's is a drawn diameter mark,
        // so the square glyph is the one typed label here; the per-axis count
        // below covers all three groups.
        for label in ["🗖"] {
            assert!(
                cards.iter().any(|text| text == label),
                "{label} is labelled once: {cards:?}"
            );
        }
        for axis in ["X", "Y", "Z"] {
            let numbers = cards
                .iter()
                .filter(|text| text.starts_with(axis) && text.len() > 1)
                .count();
            assert!(
                numbers >= 3,
                "{axis} appears in all three groups: {cards:?}"
            );
        }

        harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
        assert!(!harness.app.machine_following, "and back to the cards");
    }

    /// REMAINING changes what the card means - no trail, and the path eaten behind
    /// the tool - so the card says so in its own header, not only in the
    /// shortcut list.
    #[test]
    fn remaining_is_announced_in_the_toolpath_header() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        let header = |harness: &mut ProbeHarness| -> Vec<(String, egui::Rect)> {
            painted_text(&harness.frame_full(Vec::new()))
                .into_iter()
                .filter(|(_, rect)| rect.left() > theme::NAV_WIDTH && rect.center().y < 220.0)
                .collect()
        };
        assert!(
            !header(&mut harness).iter().any(|(text, _)| text == "REMAINING"),
            "no indicator with the mode off"
        );
        harness.app.graph_style.remaining = true;
        assert!(
            header(&mut harness).iter().any(|(text, _)| text == "REMAINING"),
            "the card header says REMAINING while it is on"
        );
    }

    /// The square key flips the right pane between the program directory and
    /// the recently opened programs, newest first, and leaves the left pane
    /// alone.
    #[test]
    fn the_square_key_flips_the_right_pane_to_recent_files() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness.app.set_page(Page::Files);
        harness.app.files.usb_focused = false;
        harness.app.preferences.recent_files = vec![
            "/nc/newest.ngc".to_string(),
            "/nc/older.ngc".to_string(),
        ];
        let texts = |harness: &mut ProbeHarness| -> Vec<String> {
            painted_text(&harness.frame_full(Vec::new()))
                .into_iter()
                .map(|(text, _)| text)
                .collect()
        };
        let listing = texts(&mut harness);
        assert!(listing.iter().any(|text| text == "NC_FILES"), "{listing:?}");
        assert!(
            !listing.iter().any(|text| text == "newest.ngc"),
            "the directory is in front: {listing:?}"
        );

        harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
        assert!(harness.app.files.recent_shown, "F10 flips the right pane");
        assert!(
            !harness.app.files.usb_focused,
            "and puts the arrows in that pane"
        );
        let listing = texts(&mut harness);
        assert!(listing.iter().any(|text| text == "RECENT"), "{listing:?}");
        let newest = listing
            .iter()
            .position(|text| text == "newest.ngc")
            .expect("the newest program is listed");
        let older = listing
            .iter()
            .position(|text| text == "older.ngc")
            .expect("the older program is listed");
        assert!(newest < older, "most recent first: {listing:?}");

        harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
        assert!(!harness.app.files.recent_shown, "and back to the directory");
    }

    /// The graph's frame is the controller's offset even when no program is
    /// loaded. The empty-text branch used to return before setting it, so a jog
    /// screen that had never opened a program kept `preview_offset = None`, the
    /// marker shift collapsed to `work - machine`, and the toolhead was drawn on
    /// its work coordinate: machine home at WCS zero, off by the whole offset
    /// before that.
    #[test]
    fn a_jog_screen_without_a_program_still_frames_the_offset() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.program.editor.replace_text(String::new());
        harness.app.snapshot.offsets.active_index = Some(1);
        if let Some(row) = harness
            .app
            .snapshot
            .offsets
            .offsets
            .iter_mut()
            .find(|row| row.index == 1)
        {
            row.values.insert("X", -156.381);
            row.values.insert("Y", -67.696);
            row.values.insert("Z", -89.944);
        }
        if let Some(position) = harness.app.snapshot.positions.axes.get_mut("X") {
            position.machine = -156.381;
            position.work = 0.0;
        }
        harness.app.refresh_preview();
        let offset = harness
            .app
            .program
            .preview_offset
            .clone()
            .expect("the controller's offset is the frame, program or not");
        assert_eq!(offset.get("X").copied(), Some(-156.381));
        assert!(harness.app.program.preview.segments.is_empty(), "no program");
        // The marker lands where the work reading already is: its shift is zero,
        // not the whole offset.
        let shift = crate::state::live_trail_shift(&harness.app.snapshot, Some(&offset));
        assert!(shift[0].abs() < 1e-9, "marker shift {shift:?}");
    }

    /// The signature is the numbers the frame depends on, in a stable order:
    /// the same rows must compare equal however the map was built, and a single
    /// moved value must compare different.
    #[test]
    fn the_offset_signature_sees_every_row_and_axis() {
        let rows = |x: f64| {
            (1..=9u8)
                .map(|index| linc_core::model::WorkOffset {
                    index,
                    values: linc_core::model::AxisMap::from_entries([
                        ("X".to_string(), x + index as f64),
                        ("Y".to_string(), -(index as f64)),
                        ("Z".to_string(), 0.0),
                    ]),
                    rotation: None,
                })
                .collect::<Vec<_>>()
        };
        let base = super::offsets_signature(&rows(0.0));
        assert_eq!(base.len(), 27, "nine rows of three axes");
        assert_eq!(base, super::offsets_signature(&rows(0.0)));
        let mut moved = rows(0.0);
        moved[0].values.insert("X", -156.381);
        assert_ne!(
            base,
            super::offsets_signature(&moved),
            "a published G54 must rebuild the preview"
        );
    }

    /// The coordinate line keeps its columns while the numbers move. A field
    /// sized by the value on screen widened the moment an axis crossed 10.000,
    /// which shuffled every field to its right - and the machine moves while the
    /// operator is reading them.
    #[test]
    fn the_coordinate_line_keeps_its_columns_as_the_numbers_move() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Program);
        let line_with = |harness: &mut ProbeHarness, work: f64, dtg: f64| {
            for axis in ["X", "Y", "Z"] {
                if let Some(entry) = harness.app.snapshot.positions.axes.get_mut(axis) {
                    entry.work = if axis == "X" { work } else { 0.0 };
                    entry.dtg = if axis == "X" { dtg } else { 0.0 };
                }
            }
            harness.app.coordinate_status_line()
        };
        let small = line_with(&mut harness, 1.0, 0.5);
        let large = line_with(&mut harness, -123.456, -300.0);
        assert_eq!(
            small.len(),
            large.len(),
            "same columns for both: {small:?} vs {large:?}"
        );
        assert!(large.contains("-123.456"), "{large}");
        assert!(
            small.contains("+1.000"),
            "the small value is padded, not left ragged: {small}"
        );
    }

    /// The centre of the bottom bar carries every axis position, then the same
    /// axes with their distance to go, and the single moving-axis field the bar
    /// otherwise shows stands down while it does.
    #[test]
    fn the_bottom_bar_groups_the_positions_then_their_dtg() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Program);
        assert!(harness.app.shows_coordinate_status());
        let line = harness.app.coordinate_status_line();
        let (position, dtg) = line.split_once("  DTG: ").expect("two groups");
        for axis in ["X", "Y", "Z"] {
            assert!(position.contains(axis), "{axis} in the position group: {line}");
            assert!(dtg.contains(axis), "{axis} in the DTG group: {line}");
        }
        assert_eq!(position.matches('X').count(), 1, "{line}");
        assert_eq!(dtg.matches('Z').count(), 1, "{line}");

        harness.frame(&[]);
        let texts = painted_text(&harness.frame_full(Vec::new()));
        let painted = texts
            .iter()
            .find(|(text, rect)| rect.center().y > 690.0 && text == &line)
            .map(|(_, rect)| *rect);
        let Some(rect) = painted else {
            panic!(
                "the bar paints {line}, got {:?}",
                texts
                    .iter()
                    .filter(|(_, r)| r.center().y > 690.0)
                    .map(|(t, _)| t.as_str())
                    .collect::<Vec<_>>()
            );
        };
        // Left-aligned in the flow: it starts after the machine fields (LN, HOMED)
        // rather than floating in the middle of the bar.
        assert!(
            rect.left() > 60.0 && rect.left() < 320.0,
            "the readout starts after the left-hand fields, at {} of a 1280 px bar",
            rect.left()
        );
        for (text, field) in &texts {
            if (text.starts_with("LN ") || text == "HOMED") && field.center().y > 690.0 {
                assert!(
                    field.right() < rect.left(),
                    "{text} ends before the readout begins: {} vs {}",
                    field.right(),
                    rect.left()
                );
            }
        }
        assert!(
            !texts
                .iter()
                .any(|(text, rect)| text.starts_with("DTG ") && rect.center().y > 690.0),
            "the single moving-axis DTG field stands down"
        );

        harness.app.set_page(Page::Machine);
        assert!(!harness.app.shows_coordinate_status());
    }

    /// The zoom chords must not depend on where the trackball happens to be:
    /// Ctrl+0 was always page-scoped while Ctrl+Plus/Minus were hover-scoped,
    /// which is why they worked only sometimes.
    #[test]
    fn the_zoom_chords_work_wherever_the_pointer_is_and_either_plus_key() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Program);
        harness.app.program.graph_auto_fit = false;
        harness.frame(&[]);

        // Pointer nowhere near the toolpath: the chord still zooms.
        let before = harness.app.program.graph_zoom;
        harness.frame_with(&[(egui::Key::Plus, ctrl())]);
        assert!(
            harness.app.program.graph_zoom > before,
            "Ctrl+Plus zooms without hovering the card: {} -> {}",
            before,
            harness.app.program.graph_zoom
        );

        // The main-row plus is Shift+equals, which egui reports as Equals.
        let before = harness.app.program.graph_zoom;
        harness.frame_with(&[(
            egui::Key::Equals,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        )]);
        assert!(
            harness.app.program.graph_zoom > before,
            "Shift+equals zooms in too: {} -> {}",
            before,
            harness.app.program.graph_zoom
        );

        // And it reaches past the old 40 ceiling the view used to stop at.
        harness.app.program.graph_zoom = 70.0;
        harness.frame_with(&[(egui::Key::Plus, ctrl())]);
        assert!(
            harness.app.program.graph_zoom > 70.0 && harness.app.program.graph_zoom <= 80.0,
            "70 zooms on to {} and stops at 80",
            harness.app.program.graph_zoom
        );
    }

    /// The jog and homing screens no longer carry a MODE CONTEXT pane.
    #[test]
    fn the_jog_screens_have_no_mode_context_pane() {
        for page in [Page::Manual, Page::Homing] {
            let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
            harness.app.set_page(page);
            harness.frame(&[]);
            let texts = painted_text(&harness.frame_full(Vec::new()));
            assert!(
                !texts.iter().any(|(text, _)| text == "MODE CONTEXT"),
                "{page:?} still paints a mode context pane"
            );
        }
    }

    #[test]
    fn the_override_readout_follows_the_panel_order() {
        assert_eq!(override_readout(1.0, 0.5, 0.25), "OVR F100 R50 S25");
        assert_eq!(override_readout(0.0, 2.0, 1.0), "OVR F0 R200 S100");
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let texts = painted_text(&output);
        assert!(
            texts.iter().any(|(text, _)| text == "OVR F100 R100 S100"),
            "the bar paints the panel order, got {:?}",
            texts
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            !texts.iter().any(|(text, _)| text == "OVR F100 S100 R100"),
            "the old FEED, SPINDLE, RAPID order is gone"
        );
    }

    /// The rows are as tight as a legible label allows. A short pill inside a
    /// tall row is what read as padding before, so the row is now only as tall
    /// as the pill plus a hair, and the operator asked for that space back.
    #[test]
    fn the_navigation_rows_are_tight_and_the_pill_fills_them() {
        for (page, _, _) in NAV_ITEMS {
            let geometry = nav_row_geometry(page);
            assert_eq!(
                geometry.height,
                theme::NAV_ROW_HEIGHT,
                "{page:?} keeps the tight row height"
            );
            assert!(
                geometry.height - geometry.pill <= 2.0,
                "{page:?} leaves at most one pixel above and below the pill: {} of {}",
                geometry.pill,
                geometry.height
            );
        }
        for page in [Page::Machine, Page::Program, Page::Tools, Page::Offsets] {
            assert!(nav_row_geometry(page).icon, "{page:?} keeps its icon");
            assert_eq!(nav_row_geometry(page).pill, theme::NAV_PILL_PRIMARY);
        }
        for page in [Page::Diagnostics, Page::Help] {
            assert!(!nav_row_geometry(page).icon, "{page:?} is a one-line row");
            assert_eq!(nav_row_geometry(page).pill, theme::NAV_PILL_SECONDARY);
        }
        assert_eq!(theme::NAV_ROW_GAP, 2.0);
        // Seven rows now cost less than seven 44 px targets did.
        assert!(theme::NAV_ROW_HEIGHT < theme::MIN_TARGET);
    }

    /// Measured at 1280x720, from the frame's own shapes: six rows of 28 px with
    /// a 2 px gap, 26/24 px pills. DIAGNOSTICS and G-CODE HELP are one-line rows
    /// whose labels start where the pill does, with no icon and no keycap under
    /// them; SETTINGS has no row at all and is reached with its chord.
    #[test]
    fn the_navigation_is_compact_and_lists_six_pages() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let texts = painted_text(&output);
        let nav = region(&harness.app, "nav");
        let in_nav = |text: &str| {
            texts
                .iter()
                .find(|(painted, rect)| painted == text && rect.left() < theme::NAV_WIDTH)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("{text} is a navigation row"))
        };
        for label in [
            "MACHINE",
            "PROGRAM",
            "TOOLS",
            "OFFSETS",
            "DIAG",
            "G-CODE",
        ] {
            in_nav(label);
        }
        assert!(
            !texts
                .iter()
                .any(|(text, rect)| text == "SETTINGS" && rect.left() < theme::NAV_WIDTH),
            "SETTINGS has no navigation row"
        );
        // A one-line row keeps its chord on the label's own line, at the pill's
        // right edge, instead of stacking it underneath.
        for (label, keycap) in [("DIAG", "Ctrl+D"), ("G-CODE", "Ctrl+H")] {
            let label_rect = in_nav(label);
            let keycap_rect = in_nav(keycap);
            assert!(
                (keycap_rect.center().y - label_rect.center().y).abs() < 1.5,
                "{keycap} shares the {label} row: {keycap_rect:?} vs {label_rect:?}"
            );
            assert!(
                keycap_rect.left() > label_rect.right(),
                "{keycap} sits to the right of {label}"
            );
        }
        // Six pills: four that carry an icon and a keycap, two one-liners.
        let mut heights: Vec<f32> = painted_rects(&output)
            .into_iter()
            .filter(|rect| {
                rect.left() >= nav.left() && rect.right() <= nav.right() && rect.width() > 100.0
            })
            .map(|rect| rect.height())
            .filter(|height| *height > 10.0 && *height < 40.0)
            .collect();
        heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(heights.len(), 6, "six painted rows, got {heights:?}");
        // One list: every row paints the same pill height, so the list reads as a
        // stack instead of alternating tall and short blocks. What differs between
        // the kinds is the icon and whether a keycap is drawn beside the label.
        assert_eq!(
            heights
                .iter()
                .filter(|height| (**height - theme::NAV_PILL_PRIMARY).abs() < 0.5)
                .count(),
            6,
            "{heights:?}"
        );
        assert_eq!(
            theme::NAV_PILL_SECONDARY, theme::NAV_PILL_PRIMARY,
            "a one-line row paints the same pill as the others"
        );
        // Pitch: the target plus the gap, from the labels that sit in them.
        let pitch = in_nav("PROGRAM").center().y - in_nav("MACHINE").center().y;
        assert!(
            (pitch - (nav_row_geometry(Page::Machine).height + theme::NAV_ROW_GAP)).abs() < 0.6,
            "row pitch {pitch}"
        );
        // A one-line row starts at the pill's inset: no icon column to clear.
        assert!(
            in_nav("DIAG").left() < in_nav("MACHINE").left() - 15.0,
            "DIAG is a one-line row: {} vs {}",
            in_nav("DIAG").left(),
            in_nav("MACHINE").left()
        );
        // The list ends a few pixels above the SHORTCUTS frame, not a band of air.
        let shortcuts = in_nav_top(&texts, "SHORTCUTS");
        // From the end of the last row's target to the SHORTCUTS frame's own
        // margin: the two NAV_BOTTOM_GAPs and the separator's lines, not a band
        // of air. It was 35 px before the list was compacted.
        let last_target_bottom =
            in_nav("G-CODE").center().y + nav_row_geometry(Page::Help).height * 0.5;
        assert!(
            shortcuts - last_target_bottom < 20.0,
            "bottom padding {}",
            shortcuts - last_target_bottom
        );
        // Ctrl+P is REMAINING now, not a page: the view changes, the page does not.
        let page = harness.app.page;
        harness.frame_with(&[(egui::Key::P, ctrl())]);
        assert!(harness.app.graph_style.remaining, "Ctrl+P turns REMAINING on");
        assert_eq!(harness.app.page, page, "a view mode, not a page");
        // The pages behind the rows still answer their own chords.
        harness.frame_with(&[(egui::Key::D, ctrl())]);
        assert_eq!(harness.app.page, Page::Diagnostics);
        harness.frame_with(&[(egui::Key::H, ctrl())]);
        assert_eq!(harness.app.page, Page::Help);
    }

    /// INC and MPG show the steps and take their numbers; JOG is continuous and
    /// gets neither, so its digits stay free and no wrong value is on screen.
    #[test]
    fn the_increment_row_and_its_numbers_belong_to_the_step_modes() {
        let steps = || {
            vec![
                ("10mm".to_string(), 10.0),
                ("1mm".to_string(), 1.0),
                (".1mm".to_string(), 0.1),
                (".01mm".to_string(), 0.01),
            ]
        };

        let mut harness = ProbeHarness::new(PanelMode::Inc, false, true);
        harness.app.jog_increments = steps();
        harness.app.jog_increment_index = 0;
        harness.app.set_page(Page::Manual);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let texts: Vec<String> = painted_text(&output).into_iter().map(|(text, _)| text).collect();
        for label in ["10mm", "1mm", ".1mm", ".01mm"] {
            assert!(
                texts.iter().any(|text| text == label),
                "every step is offered: {label} missing from {texts:?}"
            );
        }
        // The number is the key and sits outside the chip, so the button keeps
        // the value alone.
        assert!(
            texts.iter().filter(|text| ("1"..="4").contains(&text.as_str())).count() >= 4,
            "the four digits are painted next to their steps: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "1-4"),
            "the row says which numbers it takes: {texts:?}"
        );

        harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
        assert_eq!(harness.app.jog_increment_index, 2, "3 picks the third step");
        assert_eq!(harness.app.jog_increment_value(), 0.1);
        harness.frame_with(&[(egui::Key::Num9, egui::Modifiers::NONE)]);
        assert_eq!(
            harness.app.jog_increment_index, 2,
            "a digit past the last step is inert"
        );

        // JOG: no row, and the digits do nothing.
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.jog_increments = steps();
        harness.app.jog_increment_index = 0;
        harness.app.set_page(Page::Manual);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let texts: Vec<String> = painted_text(&output).into_iter().map(|(text, _)| text).collect();
        assert!(
            !texts.iter().any(|text| text == "10mm") && !texts.iter().any(|text| text == "INCREMENT"),
            "a continuous jog shows no increment: {texts:?}"
        );
        harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
        assert_eq!(harness.app.jog_increment_index, 0, "and takes no digits");
    }

    /// The style, not the page: REMAINING is what stops the trail being drawn,
    /// and the eaten line reaches the view as it is.
    #[test]
    fn remaining_takes_the_live_trail_out_of_the_style() {
        let (_dir, profile) = fixture();
        let mut style = crate::state::graph_style(&profile);
        style.show_live_trail = true;
        style.remaining = true;
        let preview = linc_core::gcode::Preview::empty();
        let mut view = linc_view::TrajectoryView::new(&preview, &[], profile.variant);
        style.apply(&mut view, None, None, Some(2));
        assert!(!view.show_live_trail, "REMAINING draws no trail");
        assert!(view.remaining, "the mode reaches the view");
        assert_eq!(view.eaten_through, Some(2), "and so does the eaten line");

        style.remaining = false;
        style.apply(&mut view, None, None, None);
        assert!(view.show_live_trail, "the trail comes back with the mode off");
        assert_eq!(view.eaten_through, None);
    }

    fn in_nav_top(texts: &[(String, egui::Rect)], label: &str) -> f32 {
        texts
            .iter()
            .find(|(text, rect)| text == label && rect.left() < theme::NAV_WIDTH)
            .map(|(_, rect)| rect.top())
            .unwrap_or_else(|| panic!("{label} is painted in the sidebar"))
    }

    /// The arrows walk the navigation over all six rows, and Enter opens the
    /// page the highlighted row names.
    #[test]
    fn the_arrow_keys_walk_all_six_navigation_rows() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        harness.app.focus_nav();
        harness.frame(&[]);
        assert_eq!(harness.app.nav.len(), 6, "six rows to walk");
        assert_eq!(harness.app.nav_focus, 0);
        for expected in 1..6 {
            harness.frame(&[egui::Key::ArrowDown]);
            assert_eq!(
                harness.app.nav_focus, expected,
                "ArrowDown reaches row {expected}"
            );
            assert_eq!(harness.app.focus_region, FocusRegion::Nav);
        }
        harness.frame(&[egui::Key::ArrowDown]);
        assert_eq!(
            harness.app.nav_focus, 5,
            "the last row is the end of the list"
        );
        harness.app.nav_focus = 5;
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(harness.app.page, Page::Help, "Enter opens G-CODE HELP");
        harness.app.focus_nav();
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.nav_focus, 4, "ArrowUp walks back");
    }

    /// A shell region's rect, from the frame's own report to the diagnostic
    /// bus: the tests read the footer and the navigation column from it rather
    /// than re-deriving them.
    fn region(app: &LincApp, name: &str) -> egui::Rect {
        let found = app
            .diag_regions
            .iter()
            .find(|region| region.name == name)
            .unwrap_or_else(|| panic!("{name} is reported"));
        egui::Rect::from_min_size(
            egui::pos2(found.x, found.y),
            egui::vec2(found.width, found.height),
        )
    }

    /// The SHORTCUTS pane sizes to its content while that fits, and takes the
    /// sidebar's remainder — down to the footer, less a little air — once the
    /// list is longer, scrolling inside itself.
    #[test]
    fn the_shortcuts_pane_sizes_to_content_then_to_the_footer() {
        // Diagnostics has one hint: the pane ends well above the footer.
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Diagnostics);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let nav = region(&harness.app, "nav");
        let pane = shortcuts_frame(&output).expect("the shortcuts pane is drawn");
        assert!(
            pane.bottom() < nav.bottom() - 40.0,
            "a short list keeps the pane at its content height: {pane:?} in {nav:?}"
        );

        // A profile that lists every navigation page still fits at 1280x720:
        // after the rows were compacted the Program page's list stays
        // content-sized instead of running to the footer. It must never pass
        // the navigation column's content edge.
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.nav = nav_pages(
            &[
                "machine",
                "program",
                "tools",
                "offsets",
                "diagnostics",
                "help",
            ]
            .map(str::to_string),
        );
        harness.app.set_page(Page::Program);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let nav = region(&harness.app, "nav");
        // The column's own 8 px bottom margin and the 4 px that keep the pane
        // off the footer.
        let pane = shortcuts_frame(&output).expect("the shortcuts pane is drawn");
        assert!(
            pane.bottom() <= nav.bottom() - 8.0 - 4.0 + 1.5,
            "the pane never passes the navigation column: {pane:?} in {nav:?}"
        );
    }

    /// Ctrl+Plus / Ctrl+Minus belong to the toolpath under the pointer. They do
    /// not rescale the shell: `[ui] scale` is the only source of that.
    #[test]
    fn ctrl_plus_and_minus_leave_the_ui_scale_alone() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.frame(&[]);
        let before = harness.ctx.zoom_factor();
        harness.frame_with(&[(egui::Key::Plus, ctrl())]);
        assert!(
            (harness.ctx.zoom_factor() - before).abs() < 0.005,
            "Ctrl+Plus rescaled the shell: {before} -> {}",
            harness.ctx.zoom_factor()
        );
        harness.frame_with(&[(egui::Key::Minus, ctrl())]);
        assert!(
            (harness.ctx.zoom_factor() - before).abs() < 0.005,
            "Ctrl+Minus rescaled the shell: {before} -> {}",
            harness.ctx.zoom_factor()
        );
    }

    /// MAIN ends on one line: the program card on the right and the two cards
    /// under the toolpath stop at the footer's top edge, and the MODAL band is
    /// pinned above the progress line with no dead space under it.
    #[test]
    fn the_main_page_cards_end_on_the_page_bottom_edge() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let footer = region(&harness.app, "bottom-bar");
        let cards = painted_cards(&output);
        // The right column's program card: the tall card past the middle.
        let program = cards
            .iter()
            .find(|rect| rect.left() > 900.0 && rect.height() > 300.0)
            .expect("the program card is drawn");
        assert!(
            (footer.top() - program.bottom()).abs() <= 3.0,
            "the program card ends at the page's edge: {program:?} vs {footer:?}"
        );
        // The two cards under the toolpath end on the same edge.
        let bottom_row: Vec<_> = cards
            .iter()
            .filter(|rect| rect.left() < 900.0 && rect.top() > 500.0)
            .collect();
        assert_eq!(
            bottom_row.len(),
            2,
            "feed/spindle and servo: {bottom_row:?}"
        );
        for rect in bottom_row {
            // The bottom row is a fixed height from `bottom_cards_height`, so it
            // lands within a couple of pixels of the same edge rather than on
            // it; the program card above is placed from the footer itself.
            assert!(
                (footer.top() - rect.bottom()).abs() <= 6.0,
                "every card ends at the page's edge: {rect:?} vs {footer:?}"
            );
        }
        // No dead space under the band: its cells end on the progress rule, and
        // the listing runs down to the band rather than stopping a row short.
        let texts = painted_text(&output);
        let modal = texts
            .iter()
            .find(|(text, _)| text == "MODAL")
            .expect("the MODAL band is drawn");
        let progress = texts
            .iter()
            .find(|(text, _)| text == "PROGRESS")
            .expect("the progress line is drawn");
        let band_bottom = texts
            .iter()
            .filter(|(_, rect)| rect.top() > modal.1.top() && rect.bottom() < progress.1.top())
            .map(|(_, rect)| rect.bottom())
            .fold(0.0f32, f32::max);
        assert!(
            progress.1.top() - band_bottom < 12.0,
            "the band ends on the progress rule: band {band_bottom}, progress {}",
            progress.1.top()
        );
        // The last listing line sits within one row of the band's rule.
        let listing_bottom = texts
            .iter()
            .filter(|(_, rect)| {
                rect.left() > program.left()
                    && rect.top() > program.top()
                    && rect.bottom() < modal.1.top()
            })
            .map(|(_, rect)| rect.bottom())
            .fold(0.0f32, f32::max);
        assert!(
            modal.1.top() - listing_bottom < 30.0,
            "the listing takes the space down to the band: {listing_bottom} vs {}",
            modal.1.top()
        );
    }

    #[test]
    fn machine_bounds_follow_the_ini_and_classify_the_toolpath() {
        let (_dir, profile) = fixture();
        let bounds = machine_bounds(&profile).expect("the INI has X/Y/Z limits");
        assert_eq!(bounds.min, [-372.0, -346.0, -253.0]);
        assert_eq!(bounds.max, [5.0, 4.0, 19.0]);
        // The same box the toolpath painter receives. A move from X 0 to X 10
        // crosses MAX_LIMIT 5 halfway along, so the red starts halfway.
        assert_eq!(
            linc_view::travel::classify_segment([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], &bounds),
            linc_view::travel::TravelClass::Straddling {
                enter: 0.0,
                exit: 0.5,
            }
        );
    }

    #[test]
    fn a_dialog_owns_the_keyboard_in_front_of_the_page() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.app.set_page(Page::Program);
        harness.app.modal = Some(Modal::Info("test".into()));

        harness.frame(&[egui::Key::F7]);
        assert_eq!(
            harness.app.page,
            Page::Program,
            "F7 must not navigate behind the dialog"
        );

        harness.frame(&[egui::Key::Escape]);
        assert!(harness.app.modal.is_none(), "Escape dismisses the dialog");
    }

    #[test]
    fn enter_in_the_save_dialog_does_not_act_on_the_page_behind() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.files_page_with_one_file("behind.ngc");
        harness.frame(&[]);
        harness.app.modal = Some(Modal::MdiSave(crate::state::MdiSaveDialog {
            name: "part".into(),
            wrap: false,
            ..Default::default()
        }));
        harness.sent.borrow_mut().clear();
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(
            harness.app.page,
            Page::Files,
            "Enter must not open the selected file behind the save dialog"
        );
        assert!(
            !harness
                .sent
                .borrow()
                .iter()
                .any(|command| matches!(command, MachineCommand::LoadProgram { .. })),
            "the dialog leaked Enter into the FILE page: {:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn zero_keys_do_not_fire_behind_a_dialog() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.frame(&[]);
        assert_eq!(harness.app.page, Page::Manual);

        harness.sent.borrow_mut().clear();
        harness.app.modal = Some(Modal::Info("test".into()));
        harness.frame(&[egui::Key::A]);
        assert!(
            !harness.sent.borrow().iter().any(|command| matches!(
                command,
                MachineCommand::ZeroWorkOffset { .. } | MachineCommand::SetWorkPosition { .. }
            )),
            "A must not zero axes behind a dialog: {:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn a_activates_the_selected_work_offset_only_on_the_offsets_page() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.app.set_page(Page::Offsets);
        harness.app.snapshot.offsets.active_index = Some(1);
        // G55. Keep the page's entry auto-select from resetting the row to the
        // active system if the test thread is descheduled for >200 ms before
        // the first frame. A future stamp makes that gap negative.
        harness.app.offsets.selected_row = 1;
        harness.app.offsets.last_frame_ms = linc_core::diag::now_ms() + 60_000;
        harness.frame(&[egui::Key::A]);
        assert!(
            harness
                .sent
                .borrow()
                .iter()
                .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { index: 2 })),
            "A on OFFSETS must activate the selected G55 row: {:?}",
            harness.sent_debug()
        );
        assert_eq!(harness.app.snapshot.offsets.active_index, Some(2));

        // An in-place edit owns the keyboard first.
        harness.sent.borrow_mut().clear();
        harness.app.offsets.selected_row = 1;
        harness.app.offsets.editing = true;
        harness.frame(&[egui::Key::A]);
        assert!(
            !harness
                .sent
                .borrow()
                .iter()
                .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { .. })),
            "A must not activate while a cell edit is open: {:?}",
            harness.sent_debug()
        );
        harness.app.offsets.editing = false;

        // The key is page-local: it must not reach the Offsets command from
        // another page.
        harness.sent.borrow_mut().clear();
        harness.app.set_page(Page::Machine);
        harness.frame(&[egui::Key::A]);
        assert!(
            !harness
                .sent
                .borrow()
                .iter()
                .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { .. })),
            "A must not leak off OFFSETS: {:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn probe_preferences_restore_only_operator_owned_values() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.probe.params.slow_feed = 0.0;
        app.probe.params.fast_feed = 234.5;
        app.probe.params.probe_mode = linc_core::probe::ProbeMode::ApplyWcs;
        app.probe.params.wcs_rotation = true;
        app.probe.params.x_hint_boss_pocket = 31.25;
        app.remember_probe_params();
        app.save_preferences(true);

        let saved = linc_core::preferences::Preferences::load(&app.profile.preferences_path);
        let saved_probe = saved.probe.as_ref().unwrap();
        assert_eq!(saved_probe.fast_feed, 234.5);
        assert_eq!(saved_probe.slow_feed, 0.0);
        // Serde defaults prove these machine/controller facts were not written.
        assert_eq!(saved_probe.probe_tool_number, 99);
        assert_eq!(saved_probe.calibration_offset, 0.0);

        app.profile.config.probe.tool_number = 42;
        app.snapshot.probe.calibration_offset = Some(0.375);
        app.probe.params.fast_feed = 999.0;
        let profile = app.profile.clone();
        let snapshot = app.snapshot.clone();
        let backend = Box::new(LocalBackend::new(profile.clone(), PanelMode::Auto));
        let app = LincApp::with_snapshot(profile, backend, snapshot);
        assert_eq!(app.probe.params.fast_feed, 234.5);
        assert_eq!(app.probe.params.slow_feed, 0.0);
        assert_eq!(app.probe.params.probe_tool_number, 42);
        assert_eq!(app.probe.params.calibration_offset, 0.375);
        assert_eq!(
            app.probe.params.probe_mode,
            linc_core::probe::ProbeMode::ApplyWcs
        );
        assert!(app.probe.params.wcs_rotation);
        assert_eq!(app.probe.params.x_hint_boss_pocket, 31.25);
    }

    #[test]
    fn probe_sync_uses_active_wcs_and_new_calibration_without_touching_edit_buffer() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.snapshot.offsets.active_index = Some(2);
        app.snapshot.probe.selected_wcs = "G54".into();
        app.snapshot.probe.calibration_offset = Some(0.125);
        app.probe.param_selected = 1;
        app.probe.param_editing = true;
        app.probe.param_buffer = "77.5".into();
        app.sync_probe();
        assert_eq!(app.probe.wcs, "G55");
        assert_eq!(app.probe.params.calibration_offset, 0.125);
        assert!(app.probe.param_editing);
        assert_eq!(app.probe.param_buffer, "77.5");

        app.snapshot.probe.calibration_offset = Some(0.25);
        app.sync_probe();
        assert_eq!(app.probe.params.calibration_offset, 0.25);
        assert_eq!(app.probe.param_buffer, "77.5");
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
        assert_eq!(app.page, Page::Machine, "MDI opens the MAIN page");
        assert!(app.mdi_pane_visible());
        switch_mode(&mut app, PanelMode::Auto);
        assert_eq!(app.page, Page::Machine, "AUTO brings up MAIN");
        assert!(
            !app.mdi_pane_visible(),
            "and the normal program card takes the pane's slot"
        );
        // From anywhere else AUTO lands on MAIN as well.
        switch_mode(&mut app, PanelMode::Mdi);
        app.set_page(Page::Files);
        switch_mode(&mut app, PanelMode::Auto);
        assert_eq!(app.page, Page::Machine, "AUTO brings up MAIN");
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
    fn a_manual_line_pick_survives_the_running_follow() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.snapshot.task.running = true;
        app.program.follow_current = false;
        app.program_was_running = false;
        app.sync_program();
        assert!(app.program.follow_current, "a run start re-arms follow");
        // Ctrl+G or the arrows picked a line: the next poll must not drag it
        // back to the executing one.
        app.program.follow_current = false;
        app.sync_program();
        assert!(!app.program.follow_current, "the operator's pick is kept");
        // The next run re-arms it again.
        app.snapshot.task.running = false;
        app.sync_program();
        app.snapshot.task.running = true;
        app.sync_program();
        assert!(app.program.follow_current);
    }

    #[test]
    fn escape_leaves_the_help_page_before_it_resets_a_running_program() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.poll_backend();
        app.snapshot.task.running = true;
        app.snapshot.task.interp = linc_core::model::InterpState::Reading;
        app.help.query = "G71".into();
        app.set_page(Page::Help);
        let back = app.help_return_page;
        assert_eq!(back, Page::Machine, "Help remembers the page it came from");
        app.escape();
        assert_eq!(app.page, back, "Esc leaves the Help page for the main view");
        assert!(
            !app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "leaving the Help page must not reset the program"
        );
        app.escape();
        assert!(
            app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "the next Esc on the main view is the program reset"
        );
    }

    #[test]
    fn escape_closes_the_help_dialog_before_resetting_a_running_program() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.poll_backend();
        app.snapshot.task.running = true;
        app.snapshot.task.interp = linc_core::model::InterpState::Reading;
        app.help.open_dialog("g0");
        app.escape();
        assert!(app.help.dialog.is_none(), "Escape closes the Help viewer");
        assert!(
            !app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "the running program behind the Help viewer must not be reset"
        );
    }

    #[test]
    fn escape_leaves_the_diagnostics_page_before_it_resets_a_running_program() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.poll_backend();
        app.set_page(Page::Program);
        app.snapshot.task.running = true;
        app.snapshot.task.interp = linc_core::model::InterpState::Reading;
        app.set_page(Page::Diagnostics);
        app.escape();
        assert_eq!(
            app.page,
            Page::Program,
            "Esc leaves DIAGNOSTICS for the page it came from"
        );
        assert!(
            !app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "looking at a log must not stop the program"
        );
        // The next Esc, on that page, is the one that resets it.
        app.escape();
        assert!(
            app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "Esc still resets once the operator is back on the page"
        );
    }

    #[test]
    fn escape_stops_a_running_program() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.poll_backend();
        app.snapshot.task.running = true;
        app.snapshot.task.interp = linc_core::model::InterpState::Reading;
        app.escape();
        app.poll_backend();
        assert!(!app.snapshot.task.running);
        assert_eq!(
            app.snapshot.task.interp,
            linc_core::model::InterpState::Idle
        );
        assert!(
            app.toasts.iter().any(|toast| toast.text == "PROGRAM RESET"),
            "the reset is acknowledged: {:?}",
            app.toasts
                .iter()
                .map(|toast| &toast.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_sidebar_offers_reset_while_a_program_is_active() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.snapshot.task.running = true;
        let shows_reset = |app: &LincApp| {
            app.key_hints().iter().any(|group| {
                group
                    .items
                    .iter()
                    .any(|(key, label)| key == "Esc" && *label == "RESET")
            })
        };
        assert!(shows_reset(&app));
        app.snapshot.task.running = false;
        app.snapshot.task.interp = linc_core::model::InterpState::Idle;
        app.snapshot.program.paused = false;
        assert!(!shows_reset(&app));
    }

    #[test]
    fn servo_load_rows_follow_the_profile_and_the_machine() {
        let (_dir, mut app) = make_app(PanelMode::Auto);
        app.profile.config.ui.servo_load = vec!["X".into(), "S".into(), "A".into()];
        app.snapshot.machine.servo_load.insert("X", 12.0);
        app.snapshot.machine.servo_load.insert("A", 4.0);
        app.snapshot.spindles.clear();
        assert_eq!(
            app.servo_load_rows(),
            vec![("X".to_string(), 12.0), ("A".to_string(), 4.0)],
            "S is skipped when the machine has no spindle load"
        );
        app.snapshot.spindles.push(linc_core::model::SpindleState {
            index: 0,
            name: "MAIN SPINDLE".into(),
            commanded_speed: 4200.0,
            actual_speed: 4185.0,
            load: Some(38.0),
            override_factor: 1.0,
            direction: 1,
            on: true,
            live_tool: false,
            live_tool_axial: false,
            live_tool_max_rpm: 0.0,
        });
        assert_eq!(
            app.servo_load_rows(),
            vec![
                ("X".to_string(), 12.0),
                ("S".to_string(), 38.0),
                ("A".to_string(), 4.0)
            ]
        );
        app.profile.config.ui.servo_load.clear();
        assert!(
            app.servo_load_rows().is_empty(),
            "an empty list hides the pane"
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
        assert!(
            (work - 12.5).abs() < 1e-6,
            "DRO X shows {work}, expected 12.5"
        );
    }

    #[test]
    fn the_fps_average_follows_the_frame_time() {
        assert!((fps_step(0.0, 1.0 / 60.0) - 60.0).abs() < 0.01);
        let mut fps = 60.0;
        for _ in 0..40 {
            fps = fps_step(fps, 1.0 / 30.0);
        }
        assert!((fps - 30.0).abs() < 0.5, "{fps}");
        // A wild frame time (a stall) cannot push the reading below zero or to
        // infinity; the clamp keeps it in a sane range.
        assert!(fps_step(60.0, 0.0) > 60.0);
        assert!(fps_step(60.0, 10.0) < 60.0);
    }

    #[test]
    fn page_names_parse_the_way_the_diagnostic_tools_spell_them() {
        assert_eq!(page_from_name("MACHINE"), Some(Page::Machine));
        assert_eq!(page_from_name("program"), Some(Page::Program));
        assert_eq!(page_from_name("G-CODE HELP"), Some(Page::Help));
        assert_eq!(page_from_name("HELP"), Some(Page::Help));
        assert_eq!(page_from_name("Diagnostics"), Some(Page::Diagnostics));
        assert_eq!(page_from_name("NOPE"), None);
    }

    #[test]
    fn a_stale_trail_is_masked_until_the_backend_clears_it() {
        let (_dir, app) = make_app(PanelMode::Jog);
        let mut snapshot = app.snapshot.clone();
        snapshot.tool_tip = Some([1.0, 2.0, 3.0]);
        snapshot.live_trail =
            std::sync::Arc::new(vec![linc_core::model::TrailPoint::new([0.0; 3], false); 5]);
        let tip = Some([1.0, 2.0, 3.0]);
        assert!(cleared_trail_pending(&snapshot, tip), "old points masked");

        // The backend applied the clear: one point left.
        snapshot.live_trail = std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(
            [1.0, 2.0, 3.0],
            false,
        )]);
        assert!(!cleared_trail_pending(&snapshot, tip), "clear accepted");

        // The tool moved: the points are new, not stale.
        snapshot.live_trail =
            std::sync::Arc::new(vec![linc_core::model::TrailPoint::new([0.0; 3], false); 5]);
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
        app.program
            .editor
            .replace_text("G21 G90\nG0 X0 Y0\n".into());
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
    #[test]
    fn run_from_line_uses_the_selected_line_not_the_controller_line() {
        let (_dir, mut app) = make_app(PanelMode::Edit);
        app.snapshot.program.content =
            Some(std::sync::Arc::new("G0 X0\nG1 X10\nG1 X20\n".to_string()));
        app.snapshot.program.line_count = 3;
        app.snapshot.program.current_line = 3;
        app.program.selected_line = 2;
        let (line, block) = app.run_from_line_choice();
        assert_eq!(line, 2);
        assert_eq!(block, "G1 X10");
    }

    use linc_core::probe::ProbeCategory;

    /// The one wording both calibration routines share under PROTECT.
    const CALIBRATION_PROTECT: &str = "PROTECT ON — calibration Run writes controller #3032 and the spindle reference parameters #3007/#3010";

    fn probe_raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events,
            ..Default::default()
        }
    }

    /// \`true\` when \`inner\` lies inside \`outer\` up to a rounding pixel.
    fn inside(outer: egui::Rect, inner: egui::Rect) -> bool {
        inner.min.x >= outer.min.x - 0.5
            && inner.min.y >= outer.min.y - 0.5
            && inner.max.x <= outer.max.x + 0.5
            && inner.max.y <= outer.max.y + 0.5
    }

    /// One probe page host: a 1280x720 headless window, a simulator machine in
    /// a known state, and a backend that records what the UI actually sent.
    struct ProbeHarness {
        _dir: tempfile::TempDir,
        ctx: egui::Context,
        app: LincApp,
        sent: std::rc::Rc<std::cell::RefCell<Vec<MachineCommand>>>,
    }

    impl ProbeHarness {
        fn new(mode: PanelMode, protect: bool, connected: bool) -> Self {
            Self::ready(mode, protect, connected, MachinePower::On)
        }

        fn ready(mode: PanelMode, protect: bool, connected: bool, power: MachinePower) -> Self {
            let (dir, profile) = fixture();
            let ctx = egui::Context::default();
            let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut sim = linc_core::sim::SimBackend::new(profile.clone(), mode);
            sim.set_power(power);
            sim.set_program_protect(protect, true);
            sim.set_panel_connected(connected);
            let backend = Box::new(RecordingBackend {
                inner: sim,
                sent: sent.clone(),
            });
            // Build the app inside a real 1280x720 pass: \`LincApp::new\` applies
            // the design zoom, and outside a pass egui reports no viewport.
            let mut app_slot = None;
            let mut profile_slot = Some(profile);
            let mut backend_slot = Some(backend);
            let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
                let profile = profile_slot.take().expect("one pass");
                let backend = backend_slot.take().expect("one pass");
                app_slot = Some(LincApp::new(profile, backend, ui.ctx()));
            });
            output.textures_delta.clear();
            let mut app = app_slot.unwrap();
            // The probe tool is in the spindle, so a blocked Run reports the
            // write policy rather than a tool mismatch.
            app.probe.params.probe_tool_number = app.snapshot.tool_change.current_tool;
            Self {
                _dir: dir,
                ctx,
                app,
                sent,
            }
        }

        /// Paint one frame at the 1280x720 baseline, with optional keys.
        fn frame(&mut self, keys: &[egui::Key]) -> Vec<egui::OutputCommand> {
            let keys: Vec<_> = keys
                .iter()
                .map(|key| (*key, egui::Modifiers::NONE))
                .collect();
            self.frame_with(&keys)
        }

        fn frame_with(
            &mut self,
            keys: &[(egui::Key, egui::Modifiers)],
        ) -> Vec<egui::OutputCommand> {
            let events = keys
                .iter()
                .map(|(key, modifiers)| egui::Event::Key {
                    key: *key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: *modifiers,
                })
                .collect();
            self.frame_events(events)
        }

        /// Paint one frame with raw events and return what egui asked the
        /// platform to do (clipboard writes prove a TextEdit consumed the cut).
        fn frame_events(&mut self, events: Vec<egui::Event>) -> Vec<egui::OutputCommand> {
            let mut output = self.frame_full(events);
            std::mem::take(&mut output.platform_output.commands)
        }

        /// Paint one frame and keep the whole output: a test that checks what
        /// reached the screen reads the shapes.
        fn frame_full(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame_input(probe_raw_input(events))
        }

        /// Paint one frame from a raw input, so a test can change the window
        /// size between frames the way a resize does.
        fn frame_input(&mut self, input: egui::RawInput) -> egui::FullOutput {
            let app = &mut self.app;
            let mut output = self.ctx.run_ui(input, |ui| {
                // The same prologue as \`eframe::App::ui\`, minus the frame.
                theme::apply_design_zoom(ui.ctx(), app.design_scale);
                app.handle_keyboard(ui);
                app.draw(ui);
            });
            // A headless frame has no painter to upload the textures to; the
            // deltas must be cleared, not dropped.
            output.textures_delta.clear();
            output
        }

        /// Draw the MDI pane alone into `slot`, and report the card frames and
        /// every text it painted. Calling the pane directly keeps the slot a
        /// known number instead of the page's computed column.
        fn draw_mdi_pane(
            &mut self,
            slot: egui::Rect,
        ) -> (Vec<egui::Rect>, Vec<(String, egui::Rect)>) {
            let app = &mut self.app;
            let mut output = self.ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
                theme::apply_design_zoom(ui.ctx(), app.design_scale);
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(slot)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                crate::screens::mdi::pane(app, &mut child, slot.height());
            });
            output.textures_delta.clear();
            (painted_cards(&output), painted_text(&output))
        }

        /// Open the page on \`category\` with a routine selected, bypassing the
        /// entry reset so a test can act on a selection.
        fn enter_probe(&mut self, category: ProbeCategory, operation: &str) {
            self.app.set_page(Page::Probe);
            self.app.probe.category = category;
            self.app.probe.operation = operation.to_string();
            self.app.probe.error = None;
        }

        /// Click at an absolute page position (press and release in one frame).
        fn click_at(&mut self, position: egui::Pos2) -> Vec<egui::OutputCommand> {
            self.frame_events(vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        }

        /// Put one file in NC_FILES, show the Files page and select it.
        fn files_page_with_one_file(&mut self, name: &str) -> std::path::PathBuf {
            // Keep the removable-media poll out of the test; the NC pane is enough.
            self.app.files.usb_root = Some(self.app.files.nc_root.clone());
            let path = self.app.files.nc_root.join(name);
            std::fs::write(&path, "G0 X0\n").unwrap();
            self.app.set_page(Page::Files);
            self.app.files.nc_current = self.app.files.nc_root.clone();
            self.app.files.nc_entries = crate::state::read_dir_entries(&self.app.files.nc_root);
            self.app.files.nc_selected = self
                .app
                .files
                .nc_entries
                .iter()
                .position(|entry| entry.path == path)
                .expect("test file is listed");
            self.app.files.nc_parent_selected = false;
            path
        }

        fn mdi_sent(&self) -> Vec<String> {
            self.sent
                .borrow()
                .iter()
                .filter_map(|command| match command {
                    MachineCommand::IssueMdi { block, .. } => Some(block.clone()),
                    _ => None,
                })
                .collect()
        }

        /// Every command that writes probe or controller state.
        fn probe_writes(&self) -> Vec<String> {
            self.sent
                .borrow()
                .iter()
                .filter(|command| {
                    matches!(
                        command,
                        MachineCommand::IssueMdi { .. }
                            | MachineCommand::ResetProbeCalibration
                            | MachineCommand::ApplyProbeResult { .. }
                    )
                })
                .map(|command| format!("{command:?}"))
                .collect()
        }

        fn sent_debug(&self) -> Vec<String> {
            self.sent
                .borrow()
                .iter()
                .map(|c| format!("{c:?}"))
                .collect()
        }
    }

    struct RecordingBackend {
        inner: linc_core::sim::SimBackend,
        sent: std::rc::Rc<std::cell::RefCell<Vec<MachineCommand>>>,
    }

    impl UiBackend for RecordingBackend {
        fn snapshot(&mut self) -> MachineSnapshot {
            linc_core::backend::MachineBackend::snapshot(&mut self.inner)
        }

        fn send(
            &mut self,
            command: MachineCommand,
        ) -> Result<(), linc_core::command::MachineError> {
            self.sent.borrow_mut().push(command.clone());
            linc_core::backend::MachineBackend::send(&mut self.inner, command)
        }

        fn drain_events(&mut self) -> Vec<BackendEvent> {
            Vec::new()
        }
    }

    // PR-03: PROTECT gates what an action writes, not which mode it runs in.

    #[test]
    fn protect_refuses_a_calibration_run_before_issuing_anything() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
        harness.frame(&[]);
        // The refusal is painted before Enter: a calibration Run rewrites #3032.
        let reason = harness
            .app
            .probe_layout
            .run_reason_text
            .clone()
            .unwrap_or_default();
        assert_eq!(reason, format!("RUN UNAVAILABLE: {CALIBRATION_PROTECT}"));

        screens::probe::run(&mut harness.app);
        assert_eq!(
            harness.app.probe.error.as_deref(),
            Some(CALIBRATION_PROTECT)
        );
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
        assert!(harness.app.probe.last_run_operation.is_none());

        // With PROTECT off the same Run issues exactly one block.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
        harness.frame(&[]);
        screens::probe::run(&mut harness.app);
        assert_eq!(harness.app.probe.error, None);
        let sent = harness.mdi_sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].contains("o<probe_cal_round_boss> call"),
            "{}",
            sent[0]
        );
    }

    #[test]
    fn protection_covers_the_spindle_nose_calibration_routine() {
        // spindle_nose is category SurfaceZ but the catalogue marks it
        // calibration: it writes #3010 and #3007, so PROTECT must treat it as a
        // calibration Run even in POSITION mode.
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "spindle_nose");
        harness.app.probe.params.probe_mode = linc_core::probe::ProbeMode::MeasureOnly;
        harness.frame(&[]);
        let reason = harness
            .app
            .probe_layout
            .run_reason_text
            .clone()
            .unwrap_or_default();
        assert_eq!(reason, format!("RUN UNAVAILABLE: {CALIBRATION_PROTECT}"));

        screens::probe::run(&mut harness.app);
        assert_eq!(
            harness.app.probe.error.as_deref(),
            Some(CALIBRATION_PROTECT)
        );
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
        assert!(harness.app.probe.last_run_operation.is_none());

        // PROTECT off: the same selection runs, still as a POSITION run.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "spindle_nose");
        harness.frame(&[]);
        screens::probe::run(&mut harness.app);
        assert_eq!(harness.app.probe.error, None);
        let sent = harness.mdi_sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].contains("o<probe_spindle_nose> call"),
            "{}",
            sent[0]
        );
    }

    #[test]
    fn an_unclassifiable_routine_block_is_refused_even_with_protect_off() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        // A routine call with no typed parameter block: the mode it inherits
        // from the interpreter cannot be trusted, so it fails closed rather than
        // passing as a measurement.
        crate::screens::probe_view::issue_probe_mdi(
            &mut harness.app,
            "o<probe_z_minus_wco> call".into(),
            "test",
        );
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("cannot be classified"), "{error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn protect_refuses_the_parameter_update_and_every_data_reset() {
        for (key, modifiers, label) in [
            (egui::Key::U, egui::Modifiers::NONE, "U UPDATE PARAMS"),
            (egui::Key::R, egui::Modifiers::CTRL, "Ctrl+R RESET ALL"),
            (egui::Key::X, egui::Modifiers::CTRL, "Ctrl+X RESET X"),
            (egui::Key::Y, egui::Modifiers::CTRL, "Ctrl+Y RESET Y"),
        ] {
            let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
            harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
            harness.frame_with(&[(key, modifiers)]);
            let error = harness.app.probe.error.clone().unwrap_or_default();
            assert!(error.contains("PROTECT ON"), "{label}: {error}");
            assert!(
                harness.probe_writes().is_empty(),
                "{label} issued {:?}",
                harness.sent_debug()
            );

            // PROTECT off: the same key issues one MDI block and no more.
            let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
            harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
            harness.frame_with(&[(key, modifiers)]);
            assert_eq!(harness.app.probe.error, None, "{label}");
            assert_eq!(harness.mdi_sent().len(), 1, "{label}");
        }
    }

    #[test]
    fn the_cut_shortcut_reaches_the_probe_x_data_reset() {
        // `egui-winit` never delivers Ctrl+X as a key event: the page receives
        // `Event::Cut`. This is the event the keyboard X reset really sees.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame_events(vec![egui::Event::Cut]);
        let sent = harness.mdi_sent();
        assert_eq!(sent.len(), 1, "{:?}", harness.sent_debug());
        assert!(sent[0].contains("o<x_data_reset> call"), "{}", sent[0]);
        assert_eq!(harness.app.probe.error, None);

        // The same shortcut under PROTECT still takes the write policy.
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame_events(vec![egui::Event::Cut]);
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("PROTECT ON"), "{error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn the_cut_hand_off_is_scoped_to_the_probe_page() {
        // No page other than Probe may receive a synthetic cut chord: the
        // clipboard event belongs to text editing everywhere else.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        let mut observed = Vec::new();
        for page in [Page::Machine, Page::Files, Page::Offsets, Page::Probe] {
            harness.app.set_page(page);
            let ctx = harness.ctx.clone();
            let app = &mut harness.app;
            let mut output = ctx.run_ui(probe_raw_input(vec![egui::Event::Cut]), |ui| {
                app.hand_cut_to_page(ui);
                let cut = ui.input(|input| {
                    input
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::Cut))
                });
                let key = ui.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(event, egui::Event::Key { key: egui::Key::X, modifiers, .. } if modifiers.ctrl)
                    })
                });
                observed.push((page, cut, key));
            });
            output.textures_delta.clear();
        }
        assert_eq!(
            observed,
            vec![
                (Page::Machine, true, false),
                (Page::Files, true, false),
                (Page::Offsets, true, false),
                (Page::Probe, false, true),
            ]
        );
    }

    #[test]
    fn the_cut_shortcut_never_resets_x_while_the_parameter_editor_is_open() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        // Ctrl+P selects the first parameter, Enter opens the inline editor.
        harness.frame_with(&[(egui::Key::P, egui::Modifiers::CTRL)]);
        harness.frame(&[egui::Key::Enter]);
        // One more frame: the editor takes focus and selects its whole value.
        harness.frame(&[]);
        assert!(harness.app.probe.param_editing, "the editor is open");
        let buffer = harness.app.probe.param_buffer.clone();
        assert!(!buffer.is_empty(), "the editor holds the value");

        let commands = harness.frame_events(vec![egui::Event::Cut]);

        assert!(
            harness.probe_writes().is_empty(),
            "Ctrl+X must not reset X data while the editor is open: {:?}",
            harness.sent_debug()
        );
        assert!(harness.app.probe.param_editing, "the editor stays open");
        assert_eq!(harness.app.probe.error, None);
        // The editor kept the chord: it cut its text to the clipboard.
        assert!(
            commands.iter().any(
                |command| matches!(command, egui::OutputCommand::CopyText(text) if *text == buffer)
            ),
            "the editor must still cut its text, got {commands:?}"
        );
        assert!(
            harness.app.probe.param_buffer.is_empty(),
            "the cut emptied the editor"
        );
    }

    #[test]
    fn the_cut_shortcut_still_cuts_text_in_the_gcode_editor() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\n".into());
        harness.app.set_page(Page::Program);
        harness.frame(&[]);

        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        let mut state = egui::text_edit::TextEditState::load(&harness.ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(2),
            )));
        state.store(&harness.ctx, id);

        let commands = harness.frame_events(vec![egui::Event::Cut]);

        assert!(
            commands.iter().any(
                |command| matches!(command, egui::OutputCommand::CopyText(text) if text == "G0")
            ),
            "the editor must cut the selection, got {commands:?}"
        );
        assert_eq!(harness.app.program.editor.text(), " X1\nG1 X2\n");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn a_hovered_toolpath_takes_the_pan_and_zoom_chords() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness.app.set_page(Page::Machine);
        // The widget records hover at the end of a frame; the shell reads it at
        // the top of the next one, which is the frame under test here.
        harness.ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
        });
        // Let the first frame auto-fit, so the pan under test is the key's own.
        harness.frame(&[]);
        let before = harness.app.program.graph_pan;
        harness.ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
        });
        harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
        assert!(
            (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
            "Ctrl+Left pans the view left by one step, {:?} -> {:?}",
            before,
            harness.app.program.graph_pan
        );
        assert_eq!(
            harness.app.focus_region,
            FocusRegion::Page,
            "the hovered view keeps Ctrl+Left from moving the focus ring"
        );
        assert!(
            !harness.app.program.graph_auto_fit,
            "a moved view stops fitting"
        );

        let zoom_before = harness.app.program.graph_zoom;
        let pan_before = harness.app.program.graph_pan;
        harness.ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
        });
        harness.frame_with(&[(egui::Key::Plus, ctrl())]);
        assert!(
            (harness.app.program.graph_zoom - zoom_before * VIEW_ZOOM_STEP).abs() < 1e-6,
            "Ctrl+Plus zooms in one step, got {}",
            harness.app.program.graph_zoom
        );
        assert!(
            (harness.app.program.graph_pan - pan_before * VIEW_ZOOM_STEP)
                .length()
                < 1e-3,
            "the pan carries the zoom so the card centre stays put, {:?} -> {:?}",
            pan_before,
            harness.app.program.graph_pan
        );
    }

    /// The CODE layout's card is a thumbnail, not a different view: it used to
    /// force TOP, so an oversized editor took the operator's perspective away
    /// and nothing but the view cycle brought it back.
    #[test]
    fn the_code_thumbnail_draws_the_operator_view_and_keeps_its_angles() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X0 Y0 Z0\nG1 X20 Y10 Z-40\n".into());
        harness.app.refresh_preview();
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Code;
        harness.app.program.graph_view = linc_view::ViewMode::Iso3d;
        harness.app.program.graph_yaw = 0.7;
        harness.app.program.graph_pitch = 0.42;
        harness.app.program.fit_requested = true;
        harness.frame(&[]);
        let iso_zoom = harness.app.program.graph_zoom;
        assert!(
            (harness.app.program.graph_yaw - 0.7).abs() < 1e-6
                && (harness.app.program.graph_pitch - 0.42).abs() < 1e-6,
            "drawing the thumbnail kept the operator's angles, got {} / {}",
            harness.app.program.graph_yaw,
            harness.app.program.graph_pitch
        );
        assert_eq!(harness.app.program.graph_view, linc_view::ViewMode::Iso3d);

        // The view the card is given is the view it fits for: the same part
        // fitted in TOP takes a different zoom, because the Z extent has no
        // screen length there.
        harness.app.program.graph_view = linc_view::ViewMode::TopXy;
        harness.app.program.fit_requested = true;
        harness.frame(&[]);
        let top_zoom = harness.app.program.graph_zoom;
        assert!(
            (iso_zoom - top_zoom).abs() > 1e-3,
            "the card fits for the view it is given, ISO {iso_zoom} vs TOP {top_zoom}"
        );
    }

    /// A card whose rect changes re-fits: the fit belongs to the rect it was
    /// computed for.
    #[test]
    fn the_code_thumbnail_refits_when_the_card_changes_size() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X0 Y0 Z0\nG1 X20 Y10 Z-40\n".into());
        harness.app.refresh_preview();
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Code;
        harness.app.program.graph_view = linc_view::ViewMode::Iso3d;
        harness.frame(&[]);
        // The operator's own zoom: nothing will re-frame it until the card
        // changes size.
        harness.app.program.graph_auto_fit = false;
        harness.app.program.graph_zoom = 6.0;
        harness.frame(&[]);
        assert!(
            (harness.app.program.graph_zoom - 6.0).abs() < 1e-6,
            "an untouched card keeps the operator's zoom"
        );

        let mut input = probe_raw_input(Vec::new());
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1024.0, 640.0),
        ));
        harness.frame_input(input);
        assert!(
            (harness.app.program.graph_zoom - 6.0).abs() > 0.1,
            "a resized card re-fits its contents, got {}",
            harness.app.program.graph_zoom
        );
    }

    /// The readout is one line: the axis name at the coordinate's own size, the
    /// coordinate beside it, the dimmed machine value at the right end. The name
    /// used to sit on a row of its own above the number.
    #[test]
    fn the_dro_puts_the_axis_name_on_the_coordinate_line() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let texts = painted_text(&output);
        let same_line = |from: egui::Rect, prefix: &str, right_of: f32| {
            texts
                .iter()
                .filter(|(text, rect)| {
                    text.starts_with(prefix)
                        && rect.left() >= right_of
                        && (rect.center().y - from.center().y).abs() < 2.0
                })
                .map(|(_, rect)| *rect)
                .min_by(|a, b| a.left().partial_cmp(&b.left()).unwrap())
        };
        for axis in ["X", "Y", "Z"] {
            // The axis names are painted in more than one place on MAIN; the
            // readout line is the one with the coordinate beside it.
            let (label, value) = texts
                .iter()
                .filter(|(text, _)| text == axis)
                .find_map(|(_, rect)| {
                    same_line(*rect, "+", rect.right()).map(|value| (*rect, value))
                })
                .unwrap_or_else(|| panic!("{axis} paints its coordinate beside its name"));
            assert!(
                (label.height() - value.height()).abs() < 0.6,
                "{axis}: the name is set at the coordinate's size, {} vs {}",
                label.height(),
                value.height()
            );
            assert!(
                value.left() >= label.right() - 0.5,
                "{axis}: the name and the coordinate do not overlap"
            );
            let machine = same_line(label, "M ", value.right())
                .unwrap_or_else(|| panic!("{axis} keeps its machine coordinate"));
            assert!(
                machine.left() >= value.right() - 0.5,
                "{axis}: the machine coordinate clears the value, {} vs {}",
                machine.left(),
                value.right()
            );
        }
    }

    /// A long coordinate on a narrow card drops the machine value whole rather
    /// than letting the two touch; the name and the value always have room.
    #[test]
    fn a_long_coordinate_drops_the_machine_value_instead_of_overlapping() {
        let ctx = egui::Context::default();
        let mut layouts = None;
        let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
            layouts = Some((
                crate::screens::common::dro_line_layout(
                    ui, 400.0, "X", -1234.567, -12.5, false, false,
                ),
                crate::screens::common::dro_line_layout(
                    ui, 180.0, "X", -1234.567, -12.5, true, true,
                ),
            ));
        });
        output.textures_delta.clear();
        let (wide, narrow) = layouts.expect("one pass");
        assert!(
            wide.machine.is_some(),
            "a wide card keeps the machine coordinate"
        );
        assert!(
            wide.value_x > theme::DRO_INSET,
            "the value starts after the name"
        );
        assert!(
            narrow.machine.is_none(),
            "a narrow card drops the machine coordinate, got {:?}",
            narrow.machine
        );
        assert_eq!(wide.value, "-1234.567");
        assert_eq!(narrow.value, "-1234.567");
        assert_eq!(
            narrow.label, "X DIA",
            "the diameter mark keeps its full label"
        );
        assert_eq!(wide.font_size, theme::font_size(theme::DRO_FONT));
        assert_eq!(narrow.font_size, theme::font_size(theme::DRO_FONT_COMPACT));
    }

    /// The readout is a third shorter, and the space goes to the toolpath: the
    /// graph card starts where the DRO row ends, so the page does not keep an
    /// empty gap.
    #[test]
    fn the_dro_cards_lost_the_second_row_and_the_toolpath_grew() {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(Page::Machine);
        harness.frame(&[]);
        let output = harness.frame_full(Vec::new());
        let rects = painted_rects(&output);
        // Each card paints its fill and its border as two rects at one place.
        let mut dro: Vec<egui::Rect> = rects
            .iter()
            .copied()
            .filter(|rect| {
                (rect.height() - theme::DRO_CARD_HEIGHT).abs() < 0.5 && rect.width() > 150.0
            })
            .collect();
        dro.dedup();
        assert_eq!(dro.len(), 3, "one card per mill axis, got {dro:?}");
        let dro_bottom = dro
            .iter()
            .fold(f32::MIN, |low, rect| low.max(rect.bottom()));
        // The toolpath card is the wide frame under the readout row.
        let graph = rects
            .iter()
            .copied()
            .filter(|rect| rect.width() > 400.0 && rect.top() >= dro_bottom)
            .min_by(|a, b| a.top().partial_cmp(&b.top()).unwrap())
            .expect("the toolpath card is under the readout");
        assert!(
            graph.top() > dro_bottom && graph.top() < dro_bottom + 16.0,
            "the toolpath starts right under the readout row, not below a gap: {} vs {}",
            graph.top(),
            dro_bottom
        );
        assert!(
            graph.height() > 200.0,
            "the toolpath took the space the readout gave up, got {}",
            graph.height()
        );
    }

    /// The centre of a nav row, from the label the sidebar paints for it.
    fn nav_row_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        painted_text(output)
            .into_iter()
            .find(|(text, rect)| text == label && rect.left() < theme::NAV_WIDTH)
            .map(|(_, rect)| rect.center())
            .unwrap_or_else(|| panic!("{label} is painted in the navigation"))
    }

    /// A point inside the toolpath card, below the line that titles it.
    fn toolpath_card_point(output: &egui::FullOutput) -> egui::Pos2 {
        let (_, rect) = painted_text(output)
            .into_iter()
            .find(|(text, _)| text == "TOOLPATH" || text == "PATH")
            .expect("the toolpath card paints its title");
        rect.center() + egui::vec2(0.0, 40.0)
    }

    fn press_at(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// The card's ZOOM readout, as painted in one frame.
    fn zoom_readout(output: &egui::FullOutput) -> Option<String> {
        painted_text(output)
            .into_iter()
            .map(|(text, _)| text)
            .find(|text| text.starts_with("ZOOM "))
    }

    /// Ctrl+Plus and Ctrl+Minus are the view's even while the G-code editor
    /// holds the keyboard: the editor binds neither chord, and that is where the
    /// operator is when they want a closer look at the path. Before this, the
    /// whole pan/zoom block sat behind "no text field has focus", and the
    /// editor takes focus as soon as the Program page opens, so both chords did
    /// nothing at all.
    #[test]
    fn the_zoom_chords_work_while_the_gcode_editor_holds_the_keyboard() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Split;
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.frame_events(Vec::new());
        assert!(
            harness.ctx.memory(|memory| memory.has_focus(id)),
            "the G-code editor holds the keyboard"
        );
        assert!(
            linc_view::toolpath_hovered(&harness.ctx),
            "the pointer is over the toolpath card"
        );

        let before = harness.app.program.graph_zoom;
        harness.frame_with(&[(egui::Key::Plus, ctrl())]);
        assert!(
            (harness.app.program.graph_zoom - before * VIEW_ZOOM_STEP).abs() < 1e-4,
            "Ctrl+Plus zooms one step while the editor has the keyboard, {} -> {}",
            before,
            harness.app.program.graph_zoom
        );
        let before = harness.app.program.graph_zoom;
        harness.frame_with(&[(egui::Key::Minus, ctrl())]);
        assert!(
            (harness.app.program.graph_zoom - before / VIEW_ZOOM_STEP).abs() < 1e-4,
            "Ctrl+Minus zooms one step out, {} -> {}",
            before,
            harness.app.program.graph_zoom
        );
        assert!(
            !harness.app.program.graph_auto_fit,
            "a zoomed view stops refitting, so the step stays on screen"
        );
    }

    /// Ctrl+0 refits, and the card's readout shows the zoom the card is drawn
    /// at. It used to show a placeholder 1.0x for the frame the fit request was
    /// made in — the whole visible effect of the key on an already-fitted view.
    #[test]
    fn ctrl_zero_refits_the_toolpath_and_the_zoom_readout_never_shows_a_placeholder() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Split;
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        // A view the operator has moved: the fit has something to do.
        harness.app.program.graph_auto_fit = false;
        harness.app.program.graph_zoom = 4.0;
        let output = harness.frame_full(Vec::new());
        assert_eq!(
            zoom_readout(&output).as_deref(),
            Some("ZOOM 4.0x"),
            "the readout shows the zoom the card is drawn at"
        );

        let output = harness.frame_full(vec![egui::Event::Key {
            key: egui::Key::Num0,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: ctrl(),
        }]);
        let fitted = harness.app.program.graph_zoom;
        assert!(
            (fitted - 4.0).abs() > 0.1,
            "Ctrl+0 refits the card instead of leaving it at the operator's zoom"
        );
        assert!(
            harness.app.program.graph_auto_fit,
            "the fit re-arms the follow-the-program framing"
        );
        assert_eq!(
            zoom_readout(&output).as_deref(),
            Some("ZOOM 4.0x"),
            "the fit frame still reports the zoom that was on screen; the old code wrote 1.0 here"
        );
        let expected = format!("ZOOM {fitted:.1}x");
        for _ in 0..2 {
            let output = harness.frame_full(Vec::new());
            assert_eq!(
                zoom_readout(&output).as_deref(),
                Some(expected.as_str()),
                "the fitted zoom is what the readout shows from then on"
            );
        }
    }

    /// The panel keyboard has no Tab key, so a click is how the keyboard leaves
    /// the G-code editor. Clicking the toolpath hands it the card, and the pan
    /// chords work from there.
    #[test]
    fn clicking_the_toolpath_hands_it_the_keyboard_and_the_pan_chords_work() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Split;
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        assert!(
            harness.ctx.memory(|memory| memory.has_focus(id)),
            "the editor starts with the keyboard"
        );

        harness.frame_events(vec![press_at(card, true)]);
        harness.frame_events(vec![press_at(card, false)]);
        assert!(
            !harness.ctx.memory(|memory| memory.has_focus(id)),
            "the click took the keyboard away from the editor"
        );

        let before = harness.app.program.graph_pan;
        harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
        assert!(
            (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
            "Ctrl+Left pans the toolpath once the card holds the keyboard, {:?} -> {:?}",
            before,
            harness.app.program.graph_pan
        );
    }

    /// The line move keeps every chord it had: with the editor focused,
    /// Ctrl+Up/Down moves the selected block and Ctrl+Left/Right moves the
    /// caret's word — none of them may pan the toolpath.
    #[test]
    fn a_focused_editor_keeps_ctrl_up_down_and_ctrl_left_right() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Split;
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.frame_events(Vec::new());
        assert!(
            linc_view::toolpath_hovered(&harness.ctx),
            "pointer on the card"
        );

        let pan = harness.app.program.graph_pan;
        harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
        assert_eq!(
            harness.app.program.graph_pan, pan,
            "Ctrl+Left belongs to the editor while it holds the keyboard"
        );
        assert_eq!(
            harness.app.focus_region,
            FocusRegion::Page,
            "and it does not move the focus ring either"
        );

        // A read-only pane has no line move, so use an editable one: the shell
        // only takes the chord when the page allows an edit.
        // Line 2 moves up past line 1: the line move answers Ctrl+Up only for a
        // line that has one above it.
        harness.app.program.selected_line = 2;
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        assert_eq!(
            harness.app.program.graph_pan, pan,
            "Ctrl+Up moves the selected line, not the view"
        );
        assert_eq!(
            harness.app.program.editor.text(),
            "G1 X2\nG0 X1\nG1 X3\n",
            "the selected block moved up, so the chord reached the editor"
        );
    }

    /// A nav click is the operator leaving the text field behind, so the row
    /// that draws its focus border also takes the keyboard back.
    ///
    /// The row of the page already in front is the case that matters: it
    /// navigates nowhere, so nothing else would have released the editor, and
    /// the pan chords stayed dead while the row drew its focus border. The
    /// panel keyboard has no Tab key to move focus with.
    #[test]
    fn a_navigation_click_releases_the_keyboard_from_the_gcode_editor() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
        harness.app.set_page(Page::Program);
        harness.app.program.view = ProgramView::Split;
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        assert!(harness.ctx.memory(|memory| memory.has_focus(id)));

        let row = nav_row_center(&output, "PROGRAM");
        harness.frame_events(vec![egui::Event::PointerMoved(row)]);
        harness.frame_events(vec![press_at(row, true)]);
        harness.frame_events(vec![press_at(row, false)]);
        assert_eq!(
            harness.app.page,
            Page::Program,
            "the same page, so nothing navigated"
        );
        assert!(
            !harness.ctx.memory(|memory| memory.has_focus(id)),
            "the nav click left the editor holding the keyboard"
        );
        assert_eq!(harness.app.focus_region, FocusRegion::Nav);

        // Back over the toolpath — the row cycled SPLIT to CODE, so it is the
        // thumbnail beside the editor now — the pan chord is live again.
        let output = harness.frame_full(Vec::new());
        let card = toolpath_card_point(&output);
        harness.frame_events(vec![egui::Event::PointerMoved(card)]);
        let before = harness.app.program.graph_pan;
        harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
        assert!(
            (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
            "Ctrl+Left pans after the nav click, {:?} -> {:?}",
            before,
            harness.app.program.graph_pan
        );
    }

    #[test]
    fn ctrl_left_still_moves_region_focus_away_from_the_toolpath() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness.app.set_page(Page::Machine);
        harness.ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), false);
        });
        harness.frame(&[]);
        let before = harness.app.program.graph_pan;
        harness.frame_with(&[(egui::Key::ArrowLeft, egui::Modifiers::CTRL)]);
        assert_eq!(harness.app.focus_region, FocusRegion::Nav);
        assert_eq!(
            harness.app.program.graph_pan, before,
            "no pan without the pointer on the toolpath"
        );
    }

    #[test]
    fn ctrl_arrow_stays_in_a_focused_text_editor() {
        let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
        harness
            .app
            .program
            .editor
            .replace_text("G0 X1\nG1 X2\n".into());
        harness.app.set_page(Page::Program);
        let id = egui::Id::unique("linc-program-editor");
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.frame_with(&[(egui::Key::ArrowLeft, egui::Modifiers::CTRL)]);
        assert_eq!(
            harness.app.focus_region,
            FocusRegion::Page,
            "Ctrl+Left must stay in the editor instead of moving focus to the nav"
        );
    }

    #[test]
    fn backspace_deletes_a_file_on_the_files_page() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        let path = harness.files_page_with_one_file("backspace-delete-test.ngc");

        harness.frame(&[egui::Key::Backspace]);

        match harness.app.modal {
            Some(crate::state::Modal::ConfirmDelete {
                path: deleted,
                usb: false,
                ..
            }) => assert_eq!(deleted, path),
            ref other => panic!("expected a delete confirmation, got {other:?}"),
        }
        assert!(path.exists(), "nothing is deleted before confirmation");
    }

    #[test]
    fn the_cut_shortcut_does_nothing_on_the_files_page() {
        // Ctrl+X is reserved for text editing: the Files page deletes with
        // Backspace and must not react to the clipboard chord at all.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        let path = harness.files_page_with_one_file("cut-does-nothing.ngc");

        let commands = harness.frame_events(vec![egui::Event::Cut]);

        assert!(
            harness.app.modal.is_none(),
            "Ctrl+X must not open a dialog: {:?}",
            harness.app.modal
        );
        assert!(path.exists());
        assert!(harness.app.files.rename.is_none());
        // Nothing consumed the Cut event, so egui asked for no clipboard write.
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::CopyText(_))),
            "{commands:?}"
        );
        // And the reserved chord never became a synthetic key.
        assert_eq!(harness.app.probe.error, None);
    }

    #[test]
    fn position_measurement_still_runs_with_protect_on() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame(&[]);
        // POSITION: the Run preamble assigns probe parameters, which is setup,
        // not a calibration or result write, so PROTECT must not block it.
        assert!(harness.app.probe_layout.run_reason_text.is_none());
        screens::probe::run(&mut harness.app);
        assert_eq!(harness.app.probe.error, None);
        let sent = harness.mdi_sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].contains("o<probe_back_left_top_corner> call"),
            "{}",
            sent[0]
        );
    }

    #[test]
    fn direct_wco_and_edge_angle_runs_keep_their_protect_gates() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.app.probe.params.probe_mode = linc_core::probe::ProbeMode::ApplyWcs;
        harness.app.snapshot.offsets.active_index = Some(1);
        harness.frame(&[]);
        screens::probe::run(&mut harness.app);
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("direct WCO"), "{error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );

        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
        harness.app.snapshot.offsets.active_index = Some(1);
        harness.frame(&[]);
        screens::probe::run(&mut harness.app);
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("edge-angle R0 reset"), "{error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn protect_keeps_the_calibration_reset_closed() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
        harness.frame_with(&[(egui::Key::R, egui::Modifiers::CTRL)]);
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("PROTECT"), "{error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    // PR-06: every page entry starts with nothing selected.

    #[test]
    fn files_return_clears_the_probe_selection_but_keeps_the_tab() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
        harness.app.probe.param_focused = true;
        harness.app.probe.param_editing = true;
        harness.app.probe.param_buffer = "12".into();
        harness.app.probe.dro_edit = Some(crate::state::DroEdit {
            axis: "X".into(),
            buffer: "1.0".into(),
            focus: false,
        });

        harness.app.request_files();
        assert_eq!(harness.app.page, Page::Files);
        harness.app.escape();

        assert_eq!(harness.app.page, Page::Probe);
        assert_eq!(
            harness.app.probe.category,
            ProbeCategory::EdgeAngle,
            "the tab the operator left is kept"
        );
        assert!(harness.app.probe.operation.is_empty());
        assert!(!harness.app.probe.param_focused);
        assert!(!harness.app.probe.param_editing);
        assert!(harness.app.probe.dro_edit.is_none());

        // Enter now has nothing to run.
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(
            harness.app.probe.error.as_deref(),
            Some("select an operation"),
            "{:?}",
            harness.sent_debug()
        );
        assert!(
            harness.probe_writes().is_empty(),
            "{:?}",
            harness.sent_debug()
        );
    }

    #[test]
    fn f8_opens_the_first_probe_tab_with_no_routine_selected() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.app.set_page(Page::Offsets);
        harness.app.probe.category = ProbeCategory::InsideCorners;
        harness.app.probe.operation = "rect_pocket_center_start".into();

        harness.frame(&[egui::Key::F8]);
        assert_eq!(harness.app.page, Page::Probe);
        assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
        assert!(harness.app.probe.operation.is_empty());
    }

    #[test]
    fn paging_probe_tabs_clears_the_selection() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");

        harness.frame(&[egui::Key::PageDown]);
        assert_eq!(harness.app.probe.category, ProbeCategory::InsideCorners);
        assert!(harness.app.probe.operation.is_empty());

        harness.app.probe.operation = "surface_z".into();
        harness.frame(&[egui::Key::PageUp]);
        assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
        assert!(harness.app.probe.operation.is_empty());
    }

    #[test]
    fn escape_returns_to_offsets_and_reentry_never_restores_a_selection() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");

        harness.frame(&[egui::Key::Escape]);
        assert_eq!(harness.app.page, Page::Offsets);

        harness.frame(&[egui::Key::F8]);
        assert_eq!(harness.app.page, Page::Probe);
        assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
        assert!(harness.app.probe.operation.is_empty());
    }

    // PR-06: a page restored after its mode went away is still an entry.

    #[test]
    fn restoring_the_probe_page_after_mode_loss_clears_the_selection() {
        // MDI is not in this list: it opens MAIN, which is not a mode-driven
        // page, so a mode loss there has no page to restore.
        for (mode, mode_page) in [
            (PanelMode::Jog, Page::Manual),
            (PanelMode::Home, Page::Homing),
        ] {
            for loss in [
                PanelModeValidity::Invalid(vec![PanelMode::Auto, mode]),
                PanelModeValidity::Unknown,
            ] {
                let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
                harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
                harness.app.probe.param_focused = true;
                harness.app.probe.param_editing = true;
                harness.app.probe.param_buffer = "12".into();
                harness.app.probe.dro_edit = Some(crate::state::DroEdit {
                    axis: "X".into(),
                    buffer: "1.0".into(),
                    focus: false,
                });

                // The panel switches to a mode-driven page.
                harness.app.last_mode = Some(PanelModeValidity::Valid(PanelMode::Auto));
                harness.app.on_mode_change(PanelModeValidity::Valid(mode));
                assert_eq!(harness.app.page, mode_page, "{mode:?}");
                assert_eq!(harness.app.last_non_mode_page, Page::Probe, "{mode:?}");

                // ...and the mode goes away: Probe comes back through the entry
                // logic, with the tab kept and nothing selected.
                harness.app.on_mode_change(loss.clone());
                assert_eq!(harness.app.page, Page::Probe, "{mode:?}/{loss:?}");
                assert_eq!(
                    harness.app.probe.category,
                    ProbeCategory::EdgeAngle,
                    "{mode:?}: the tab is kept"
                );
                assert!(harness.app.probe.operation.is_empty(), "{mode:?}");
                assert!(!harness.app.probe.param_focused, "{mode:?}");
                assert!(!harness.app.probe.param_editing, "{mode:?}");
                assert!(harness.app.probe.dro_edit.is_none(), "{mode:?}");

                // Enter has nothing to run.
                harness.frame(&[egui::Key::Enter]);
                assert_eq!(
                    harness.app.probe.error.as_deref(),
                    Some("select an operation"),
                    "{mode:?}"
                );
                assert!(
                    harness.probe_writes().is_empty(),
                    "{mode:?}/{loss:?}: {:?}",
                    harness.sent_debug()
                );
            }
        }
    }

    #[test]
    fn a_valid_mode_takes_its_own_page_so_normal_transitions_never_restore_probe() {
        // Every valid mode drives a page of its own, so the normal transitions
        // (MDI -> AUTO, MANUAL -> EDIT, HOME -> JOG) end on that page and never
        // restore a Probe page. Only a mode LOSS restores, and only that path
        // has to clear the Probe selection.
        // MDI opens MAIN, which is not mode-driven: it never has a page to
        // restore, so only the mode-driven positions are in this table.
        for (from, to, destination) in [
            (PanelMode::Jog, PanelMode::Edit, Page::Program),
            (PanelMode::Home, PanelMode::Jog, Page::Manual),
        ] {
            let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
            harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");
            harness.app.probe.param_focused = true;

            harness.app.last_mode = Some(PanelModeValidity::Valid(PanelMode::Auto));
            harness.app.on_mode_change(PanelModeValidity::Valid(from));
            assert!(harness.app.page.is_mode_driven(), "{from:?}");

            // `sync` records the new mode before it calls back in.
            harness.app.last_mode = Some(PanelModeValidity::Valid(from));
            harness.app.on_mode_change(PanelModeValidity::Valid(to));
            assert_eq!(harness.app.page, destination, "{from:?} -> {to:?}");
        }
    }

    // PR-09 and PR-10: the 1280x720 page keeps its Run context and its targets.

    /// Every clickable kind the Probe page painted, as the page recorded it.
    fn probe_targets(
        layout: &crate::screens::probe_view::ProbeLayout,
    ) -> Vec<(String, egui::Rect)> {
        let mut targets = Vec::new();
        for (kind, slot) in [
            ("dro", layout.dro_target),
            ("parameter", layout.parameter_target),
            ("hint field", layout.hint_target),
            ("calibration card", layout.calibrate_card),
            ("calibration action", layout.calibrate_action),
        ] {
            if let Some(rect) = slot {
                targets.push((kind.to_string(), rect));
            }
        }
        for (operation, rect) in &layout.routine_cards {
            targets.push((format!("card {operation}"), *rect));
        }
        targets
    }

    #[test]
    fn every_probe_pointer_target_meets_the_44px_minimum() {
        for (state, protect, power) in [
            ("banner-free", false, MachinePower::On),
            ("protect", true, MachinePower::On),
            ("estop", true, MachinePower::Estop),
        ] {
            for (category, operation) in [
                (ProbeCategory::OutsideCorners, "back_left_top_corner"),
                (ProbeCategory::InsideCorners, "back_left_inside_corner"),
                (ProbeCategory::BossAndPocket, "round_boss"),
                (ProbeCategory::RidgeAndValley, "ridge_x"),
                (ProbeCategory::EdgeAngle, "top_back_edge_angle"),
                (ProbeCategory::Calibrate, "cal_round_boss"),
            ] {
                let mut harness = ProbeHarness::ready(PanelMode::Auto, protect, true, power);
                harness.enter_probe(category, operation);
                harness.frame(&[]);
                let layout = harness.app.probe_layout.clone();

                let targets = probe_targets(&layout);
                assert!(
                    !targets.is_empty(),
                    "{state}/{category:?}: the page recorded no targets"
                );
                for (kind, rect) in targets {
                    assert!(
                        rect.width() >= theme::MIN_TARGET && rect.height() >= theme::MIN_TARGET,
                        "{state}/{category:?}: {kind} is {:?}",
                        rect.size()
                    );
                }

                if category == ProbeCategory::Calibrate {
                    assert!(
                        layout.calibrate_card.is_some(),
                        "{state}: the calibration cards are targets"
                    );
                    assert!(
                        layout.calibrate_action.is_some(),
                        "{state}: the calibration action is a target"
                    );
                    continue;
                }
                assert_eq!(
                    layout.routine_cards.len(),
                    crate::screens::probe_view::card_count(category),
                    "{state}/{category:?}: every card must be measured"
                );
                if matches!(
                    category,
                    ProbeCategory::BossAndPocket | ProbeCategory::RidgeAndValley
                ) {
                    assert!(
                        layout.hint_target.is_some(),
                        "{state}/{category:?}: the hint field is a target"
                    );
                }
            }
        }
    }

    #[test]
    fn a_pointer_pick_updates_the_run_context_and_hints_in_the_same_frame() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");
        harness.frame(&[]);
        assert_eq!(
            harness.app.probe_layout.hints_drawn.as_deref(),
            Some(&[11usize][..]),
            "the round boss shows only the boss/pocket diameter hint"
        );

        let target = harness
            .app
            .probe_layout
            .routine_cards
            .iter()
            .find(|(operation, _)| operation == "rect_boss")
            .map(|(_, rect)| rect.center())
            .expect("the rect boss card was recorded");

        harness.click_at(target);

        assert_eq!(harness.app.probe.operation, "rect_boss");
        let layout = harness.app.probe_layout.clone();
        let name = linc_core::probe::operation("rect_boss").unwrap().name;
        let action = layout.run_action_text.clone().unwrap_or_default();
        assert!(
            action.contains(name),
            "the Run context must name the picked routine in the same frame: {action}"
        );
        assert_eq!(
            layout.hints_drawn.as_deref(),
            Some(&[12usize, 13][..]),
            "the hint row must follow the picked routine in the same frame"
        );
    }

    #[test]
    fn every_parameter_name_fits_inside_its_target() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame(&[]);
        let cell = harness
            .app
            .probe_layout
            .parameter_target
            .expect("parameter field drawn")
            .width();
        let mut labels: Vec<&str> = crate::screens::probe_view::COMMON_PARAMS
            .iter()
            .map(|index| crate::screens::probe_view::PARAM_LABELS[*index])
            .collect();
        labels.push("STYLUS DIAM #5410");

        let mut widths = Vec::new();
        let ctx = harness.ctx.clone();
        let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
            for label in &labels {
                let width = ui.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            (*label).to_string(),
                            egui::FontId::monospace(9.0),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                });
                widths.push((label, width));
            }
        });
        output.textures_delta.clear();

        for (label, width) in widths {
            assert!(
                width <= cell - 4.0,
                "{label} needs {width:.1} px of a {cell:.1} px field"
            );
        }
    }

    #[test]
    fn the_action_and_reason_lines_fit_at_1280x720_with_banners() {
        // PROTECT, E-stop (two banners) and banner-free all have to show the
        // action, and the reason when Run is refused, inside the page.
        for (label, protect, power, connected) in [
            ("protect", true, MachinePower::On, true),
            ("estop", true, MachinePower::Estop, true),
            ("offline", true, MachinePower::On, false),
            ("plain", false, MachinePower::On, true),
        ] {
            for (category, operation) in [
                (ProbeCategory::OutsideCorners, "back_left_top_corner"),
                (ProbeCategory::EdgeAngle, "top_back_edge_angle"),
                (ProbeCategory::BossAndPocket, "round_boss"),
                (ProbeCategory::Calibrate, "cal_round_boss"),
            ] {
                let mut harness = ProbeHarness::ready(PanelMode::Auto, protect, connected, power);
                harness.enter_probe(category, operation);
                harness.frame(&[]);
                let layout = harness.app.probe_layout.clone();
                let page = layout.page;
                let action = layout
                    .run_action
                    .unwrap_or_else(|| panic!("{label}/{category:?}: no action line"));
                assert!(
                    inside(page, action),
                    "{label}/{category:?}: {action:?} vs {page:?}"
                );
                // A POSITION measurement is available under PROTECT; the
                // Edge Angle R0 reset and a calibration #3032 write are not.
                let blocked = !connected
                    || !matches!(power, MachinePower::On)
                    || (protect
                        && matches!(
                            category,
                            ProbeCategory::EdgeAngle | ProbeCategory::Calibrate
                        ));
                match (blocked, layout.run_reason) {
                    (true, Some(reason)) => {
                        let text = layout.run_reason_text.clone().unwrap_or_default();
                        assert!(text.contains("RUN UNAVAILABLE"), "{label}: {text}");
                        assert!(
                            inside(page, reason),
                            "{label}/{category:?}: reason {reason:?} vs {page:?}"
                        );
                        assert!(
                            action.bottom() <= reason.top(),
                            "{label}/{category:?}: action and reason overlap"
                        );
                    }
                    (true, None) => {
                        panic!("{label}/{category:?}: a blocked Run must say why")
                    }
                    (false, Some(reason)) => {
                        panic!("{label}/{category:?}: an available Run shows {reason:?}")
                    }
                    (false, None) => {}
                }
                if category == ProbeCategory::Calibrate {
                    // The Calibrate tab has its own panel instead of the
                    // routine columns.
                    let panel = layout.calibrate_panel.expect("calibration panel drawn");
                    assert!(
                        panel.bottom() <= page.bottom() + 0.5,
                        "{label}/{category:?}: panel {panel:?} vs {page:?}"
                    );
                    continue;
                }
                let grid = layout.routine_grid.expect("grid drawn");
                assert!(inside(page, grid), "{label}/{category:?}: grid {grid:?}");
                let left = layout.left_column.expect("left column drawn");
                assert!(
                    left.bottom() <= page.bottom() + 0.5,
                    "{label}/{category:?}: left column {left:?} vs {page:?}"
                );
            }
        }
    }

    // The MDI pane: the panel position replaces MAIN's program card.

    fn mdi_harness() -> ProbeHarness {
        ProbeHarness::new(PanelMode::Mdi, false, true)
    }

    fn blocks(app: &LincApp) -> Vec<String> {
        app.mdi
            .blocks
            .iter()
            .map(|(block, _)| block.clone())
            .collect()
    }

    fn history_limit(app: &LincApp) -> usize {
        app.profile.config.mdi.history_limit
    }

    use linc_core::gcode::SegmentKind;

    /// Turn the panel switch the way a frame sees it: the snapshot reports the
    /// new position and the mode handler runs against it, as `sync` does.
    fn switch_mode(app: &mut LincApp, mode: PanelMode) {
        app.snapshot.panel.mode = PanelModeValidity::Valid(mode);
        app.on_mode_change(PanelModeValidity::Valid(mode));
    }

    /// The card frames the frame painted: a rounded rect with the border
    /// colour, which is what `theme::card` and `common::shell_card` draw.
    fn painted_cards(output: &egui::FullOutput) -> Vec<egui::Rect> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(rect) => {
                    if rect.stroke.color == theme::BORDER && rect.corner_radius.nw == 6 {
                        out.push(rect.rect);
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut rects);
        }
        rects
    }

    /// Every rect the frame painted, for measuring what a card or a row is.
    fn painted_rects(output: &egui::FullOutput) -> Vec<egui::Rect> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(rect) => out.push(rect.rect),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut rects);
        }
        rects
    }

    /// Every text the frame painted, with the rect it landed in.
    fn painted_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => out.push((
                    text.galley.text().to_string(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        texts
    }

    /// Type one character per frame, the way the panel delivers them.
    fn type_text(harness: &mut ProbeHarness, text: &str) {
        for character in text.chars() {
            harness.frame_events(vec![egui::Event::Text(character.to_string())]);
        }
    }

    /// Where the input's text cursor sits, in characters.
    fn mdi_caret(harness: &ProbeHarness) -> usize {
        let id = egui::Id::unique("linc-mdi-input");
        egui::text_edit::TextEditState::load(&harness.ctx, id)
            .and_then(|state| state.cursor.char_range())
            .map(|range| range.primary.index.0)
            .unwrap_or(0)
    }

    /// Select a character range of the MDI input, the way a drag would.
    fn select_mdi_chars(harness: &ProbeHarness, start: usize, end: usize) {
        let id = egui::Id::unique("linc-mdi-input");
        let mut state = egui::text_edit::TextEditState::load(&harness.ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(end),
            )));
        state.store(&harness.ctx, id);
    }

    fn mdi_input_focused(harness: &ProbeHarness) -> bool {
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-mdi-input")))
    }

    /// What egui-winit delivers on a non-Mac keyboard: Ctrl also sets
    /// command, and a binding parsed from "Ctrl+..." requires it.
    fn ctrl() -> egui::Modifiers {
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND
    }

    /// The pane reserves the MODAL band at the bottom of its card and still
    /// fills the slot it was given: the column beside it ends on the same line.
    #[test]
    fn the_mdi_pane_draws_the_modal_band_and_fills_its_slot() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let slot = egui::Rect::from_min_size(egui::pos2(920.0, 90.0), egui::vec2(352.0, 560.0));
        let (cards, texts) = harness.draw_mdi_pane(slot);
        // The pane's own card fills the slot it was given, to the pixel: the
        // column beside it ends on the same line.
        let card = cards
            .iter()
            .find(|rect| (rect.width() - slot.width()).abs() < 0.5)
            .expect("the pane's card is drawn at the slot's width");
        assert!(
            (card.height() - slot.height()).abs() < 0.5,
            "the card fills its slot: {} of {}",
            card.height(),
            slot.height()
        );
        assert!((card.min.y - slot.min.y).abs() < 0.5);
        assert!(texts.iter().any(|(text, _)| text.contains("BLOCKS")));
        // An idle strip draws nothing: the issue keys are already on the rail.
        assert!(
            !texts.iter().any(|(text, _)| text.starts_with("ENTER ISSUES")),
            "the idle strip must not repeat the shortcuts rail"
        );
        harness.app.mdi.last_result = Some("G1 X10".into());
        let (_, texts) = harness.draw_mdi_pane(slot);
        let strip = texts
            .iter()
            .find(|(text, _)| text.starts_with("ISSUED"))
            .expect("the result strip is drawn");
        let modal = texts
            .iter()
            .find(|(text, _)| text == "MODAL")
            .expect("the MODAL band is drawn");
        // Inside the card, at its bottom, and under the strip.
        assert!(
            modal.1.min.y > strip.1.max.y,
            "the band sits below the strip"
        );
        assert!(modal.1.min.x >= slot.left() + 12.0 - 0.5);
        assert!(modal.1.max.x <= slot.right() - 12.0 + 0.5);
        assert!(modal.1.max.y <= slot.bottom() - 12.0 + 0.5);
        assert!(
            modal.1.min.y > slot.center().y,
            "the band takes the bottom of the card, not the middle"
        );
    }

    #[test]
    fn a_block_that_leaves_the_machine_envelope_is_refused() {
        // The pane lights the frame red from the same segments; the gate is what
        // stops Enter from sending a move the machine cannot make.
        let mut harness = mdi_harness();
        harness.app.mdi.input = "G0 X1000".into();
        screens::mdi::issue(&mut harness.app);
        let error = harness.app.mdi.last_error.clone().unwrap_or_default();
        assert!(error.contains("travel limit"), "{error}");
        assert!(harness.mdi_sent().is_empty(), "{:?}", harness.sent_debug());

        // The same axis, inside the envelope, still goes out.
        harness.app.mdi.input = "G0 X-100".into();
        harness.app.mdi.last_error = None;
        screens::mdi::issue(&mut harness.app);
        assert_eq!(harness.app.mdi.last_error, None);
        assert_eq!(harness.mdi_sent(), vec!["G0 X-100".to_string()]);
    }

    #[test]
    fn typing_a_block_inserts_the_space_before_each_value_word() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        harness.frame(&[]);
        assert!(mdi_input_focused(&harness), "the pane holds the keyboard");

        type_text(&mut harness, "G1X10");
        assert_eq!(harness.app.mdi.input, "G1 X10");
        assert_eq!(mdi_caret(&harness), 6, "the caret ends after the character");

        // The same rule from a value word, twice in one block.
        let mut second = mdi_harness();
        second.frame(&[]);
        second.frame(&[]);
        type_text(&mut second, "X10Y5");
        assert_eq!(second.app.mdi.input, "X10 Y5");
        assert_eq!(mdi_caret(&second), 6);

        // A block that is already spaced is left exactly as typed.
        let mut third = mdi_harness();
        third.frame(&[]);
        third.frame(&[]);
        type_text(&mut third, "M3 S1200");
        assert_eq!(third.app.mdi.input, "M3 S1200");
    }

    #[test]
    fn a_leading_value_letter_and_a_spaced_one_get_no_extra_space() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        harness.frame(&[]);
        type_text(&mut harness, "X10");
        assert_eq!(
            harness.app.mdi.input, "X10",
            "nothing before the first word"
        );

        // A letter after an existing space is not touched, and a second space is
        // never typed for it.
        type_text(&mut harness, " Y5");
        assert_eq!(harness.app.mdi.input, "X10 Y5");
    }

    #[test]
    fn a_value_letter_inside_a_comment_or_an_expression_is_left_alone() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        harness.frame(&[]);
        type_text(&mut harness, "G1(aX");
        assert_eq!(harness.app.mdi.input, "G1(aX", "a comment is prose");

        let mut brackets = mdi_harness();
        brackets.frame(&[]);
        brackets.frame(&[]);
        type_text(&mut brackets, "G1[#1X");
        assert_eq!(
            brackets.app.mdi.input, "G1[#1X",
            "an expression is not words"
        );

        // Closed again, the rule applies once more.
        type_text(&mut brackets, "]Y");
        assert_eq!(brackets.app.mdi.input, "G1[#1X] Y");
    }

    #[test]
    fn a_recalled_block_is_not_rewritten_until_a_key_is_typed() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G1X10".into());
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.input, "G1X10");
        harness.frame(&[]);
        harness.frame(&[]);
        assert_eq!(
            harness.app.mdi.input, "G1X10",
            "recall keeps the block as it was issued"
        );
        // The first typed character applies the rule at the caret.
        type_text(&mut harness, "Y");
        assert_eq!(harness.app.mdi.input, "G1X10 Y");
        assert_eq!(mdi_caret(&harness), 7);
    }

    #[test]
    fn a_selection_is_replaced_with_the_space_in_front_of_the_word() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        harness.frame(&[]);
        harness.app.mdi.input = "G1X10".into();
        harness.frame(&[]);
        // Replace the `X10` the way Enter-in-a-cell selects a value.
        select_mdi_chars(&harness, 2, 5);
        type_text(&mut harness, "Y");
        assert_eq!(harness.app.mdi.input, "G1 Y");
        assert_eq!(mdi_caret(&harness), 4, "the caret ends after the character");
    }

    #[test]
    fn auto_brings_up_main_from_anywhere() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        assert_eq!(harness.app.page, Page::Machine, "MDI opens MAIN");
        switch_mode(&mut harness.app, PanelMode::Auto);
        assert_eq!(harness.app.page, Page::Machine, "AUTO brings up MAIN");
        assert!(
            !harness.app.mdi_pane_visible(),
            "the pane goes with the mode, so MAIN shows the program card"
        );

        // From any other page too.
        switch_mode(&mut harness.app, PanelMode::Edit);
        harness.app.set_page(Page::Files);
        switch_mode(&mut harness.app, PanelMode::Auto);
        assert_eq!(harness.app.page, Page::Machine, "AUTO brings up MAIN");

        // The other positions keep their mapping.
        harness.app.set_page(Page::Files);
        switch_mode(&mut harness.app, PanelMode::Edit);
        assert_eq!(harness.app.page, Page::Program);
        assert_eq!(harness.app.program.view, ProgramView::Code);
        switch_mode(&mut harness.app, PanelMode::Jog);
        assert_eq!(harness.app.page, Page::Manual);
        switch_mode(&mut harness.app, PanelMode::Home);
        assert_eq!(harness.app.page, Page::Homing);
        switch_mode(&mut harness.app, PanelMode::Mdi);
        assert_eq!(harness.app.page, Page::Machine);
        // AUTO still has no page of its own, so the boot path is unchanged.
        assert!(mode_page(&PanelModeValidity::Valid(PanelMode::Auto)).is_none());
    }

    #[test]
    fn a_block_previews_in_the_machines_distance_mode() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        // No work offset: the block's numbers are the machine's in this test.
        harness.app.program.preview_offset = None;
        // Away from zero, or an absolute move and a relative one would land on
        // the same number and the test would prove nothing.
        if let Some(x) = harness.app.snapshot.positions.axes.get_mut("X") {
            x.machine = -100.0;
            x.work = -100.0;
        }
        harness.app.mdi.input = "G0 X-10".into();
        let absolute = crate::screens::mdi::overlay(&mut harness.app).to_vec();
        let end = absolute.last().expect("a move").end[0];
        assert!((end + 10.0).abs() < 1e-6, "G90 lands on X-10: {end}");

        // The machine sits in G91: the same block is a 10 mm move from where the
        // tool is, not a jump across the part.
        harness.app.snapshot.program.modal = vec![("G91".into(), String::new())];
        let start = harness.app.tool_tip();
        assert!((start[0] + 100.0).abs() < 1e-6, "the tool is at {start:?}");
        let relative = crate::screens::mdi::overlay(&mut harness.app).to_vec();
        let end = relative.last().expect("a move").end[0];
        assert!(
            (end - (start[0] - 10.0)).abs() < 1e-6,
            "G91 moves 10 mm from {}: {end}",
            start[0]
        );
    }

    #[test]
    fn the_overlay_starts_at_the_tool_and_ends_where_the_block_says() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        // A known frame: no work offset, so the block's numbers are the
        // machine's in this test.
        harness.app.program.preview_offset = None;
        let start = harness.app.tool_tip();
        harness.app.mdi.input = "G1 X10".into();
        let segments = crate::screens::mdi::overlay(&mut harness.app).to_vec();
        assert_eq!(segments.len(), 1, "one block, one move");
        assert_eq!(segments[0].start, start, "the move starts at the tool");
        assert_eq!(segments[0].end, [10.0, start[1], start[2]]);
        assert_eq!(
            segments[0].kind,
            SegmentKind::Feed,
            "a fast move is the cut colour, a solid line"
        );

        // A rapid is the other layer: the renderer dashes it.
        harness.app.mdi.input = "G0 X10".into();
        let rapid = crate::screens::mdi::overlay(&mut harness.app).to_vec();
        assert_eq!(rapid[0].kind, SegmentKind::Rapid);
        assert_ne!(rapid[0].kind, segments[0].kind);
        assert_eq!(rapid[0].start, start);
        assert_eq!(rapid[0].end, [10.0, start[1], start[2]]);

        // Nothing to draw for a block the pane would refuse.
        for block in ["", "   ", "G1 X"] {
            harness.app.mdi.input = block.into();
            assert!(
                crate::screens::mdi::overlay(&mut harness.app).is_empty(),
                "{block:?} draws nothing"
            );
        }
    }

    #[test]
    fn the_overlay_is_parsed_once_per_change() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        harness.app.mdi.input = "G1 X10".into();
        crate::screens::mdi::overlay(&mut harness.app);
        let first = harness.app.mdi.overlay.as_ref().expect("cached").generation;
        // Every frame of an unchanged pane keeps the same parse.
        crate::screens::mdi::overlay(&mut harness.app);
        crate::screens::mdi::overlay(&mut harness.app);
        assert_eq!(
            harness.app.mdi.overlay.as_ref().map(|c| c.generation),
            Some(first)
        );
        // A typed character, a moved machine or a changed modal state re-parses.
        harness.app.mdi.input.push('5');
        crate::screens::mdi::overlay(&mut harness.app);
        let typed = harness.app.mdi.overlay.as_ref().expect("cached").generation;
        assert!(typed > first, "the input changed");
        harness.app.mdi.input.pop();
        harness.app.program.preview_offset = Some(linc_core::model::AxisMap::new());
        crate::screens::mdi::overlay(&mut harness.app);
        let modal = harness.app.mdi.overlay.as_ref().expect("cached").generation;
        assert!(modal > typed, "the modal state changed");
    }

    #[test]
    fn ctrl_up_and_down_move_the_selected_block_and_carry_the_selection() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        for block in ["G0 X1", "G1 X2", "G1 X3"] {
            harness.app.mdi.commit(limit, block.into());
        }
        let sent = harness.mdi_sent().len();
        // No selection: the chords do nothing at all.
        harness.app.mdi.selection = None;
        harness.app.mdi.input.clear();
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
        assert_eq!(
            blocks(&harness.app),
            ["G0 X1", "G1 X2", "G1 X3"],
            "nothing moves without a selection"
        );

        // Recall the last line, then move it up: the selection follows.
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.selection, Some(2));
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X3", "G1 X2"]);
        assert_eq!(harness.app.mdi.selection, Some(1));
        assert_eq!(
            harness.app.mdi.input, "G1 X3",
            "the move leaves the input alone"
        );
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G1 X3", "G0 X1", "G1 X2"]);
        assert_eq!(harness.app.mdi.selection, Some(0));
        // At the top: no wrap, no pop.
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G1 X3", "G0 X1", "G1 X2"]);
        assert_eq!(harness.app.mdi.selection, Some(0));
        // Back down to the last line and stop there.
        harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
        harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X2", "G1 X3"]);
        assert_eq!(harness.app.mdi.selection, Some(2));
        harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
        assert_eq!(harness.app.mdi.selection, Some(2));
        assert_eq!(harness.app.mdi.len(), 3);

        // The moves never issue anything and never open a dialog.
        assert_eq!(harness.mdi_sent().len(), sent);
        assert!(harness.app.modal.is_none());
        // Plain arrows still recall.
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.selection, Some(1));
        assert_eq!(harness.app.mdi.input, "G1 X2");
    }

    #[test]
    fn the_moved_block_is_the_one_run_delete_and_save_act_on() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        for block in ["G0 X1", "G1 X2", "G1 X3"] {
            harness.app.mdi.commit(limit, block.into());
        }
        harness.app.mdi.input.clear();
        harness.app.mdi.selection = Some(2);
        harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X3", "G1 X2"]);
        assert_eq!(harness.app.mdi.selection, Some(1));

        // Ctrl+R runs the moved block, not the one that was in that slot.
        harness.frame_with(&[(egui::Key::R, ctrl())]);
        assert_eq!(harness.mdi_sent().last().map(String::as_str), Some("G1 X3"));

        // Ctrl+S writes the buffer in the order the operator sees.
        let dir = harness._dir.path().to_path_buf();
        let path = crate::screens::mdi::save_buffer(&harness.app, &dir, "order", false, false)
            .expect("the buffer saves");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "G0 X1\nG1 X3\nG1 X2\n"
        );

        // Ctrl+Bksp deletes the moved line.
        harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
        assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X2"]);
    }

    #[test]
    fn the_mdi_pane_owns_the_program_slot_on_main_and_outlives_the_page() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        assert_eq!(harness.app.page, Page::Machine, "MDI opens MAIN");
        assert!(harness.app.mdi_pane_visible());

        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X0".into());

        // Another page restores the program card and keeps the buffer.
        harness.frame(&[egui::Key::F6]);
        assert_eq!(harness.app.page, Page::Program);
        assert!(!harness.app.mdi_pane_visible());
        harness.frame(&[egui::Key::F5]);
        assert!(harness.app.mdi_pane_visible(), "MAIN shows the pane again");

        // So does leaving the mode and coming back.
        harness
            .app
            .on_mode_change(PanelModeValidity::Valid(PanelMode::Jog));
        assert!(!harness.app.mdi_pane_visible());
        harness
            .app
            .on_mode_change(PanelModeValidity::Valid(PanelMode::Mdi));
        assert_eq!(harness.app.page, Page::Machine);
        assert!(harness.app.mdi_pane_visible());
        assert_eq!(blocks(&harness.app), vec!["G0 X0".to_string()]);
        // auto_focus: the pane takes the keyboard when it appears, so a block
        // can be typed without reaching for the mouse.
        harness.frame(&[]);
        assert!(
            harness
                .ctx
                .memory(|memory| memory.has_focus(egui::Id::unique("linc-mdi-input"))),
            "the input holds the keyboard"
        );

        // Esc clears only the uncommitted input.
        harness.app.mdi.input = "G0 Z5".into();
        harness.frame(&[egui::Key::Escape]);
        assert!(harness.app.mdi.input.is_empty());
        assert_eq!(harness.app.mdi.len(), 1, "Esc never clears the buffer");
    }

    #[test]
    fn enter_issues_a_block_and_a_recalled_line_is_overwritten_in_place() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);

        harness.app.mdi.input = "G0 X1".into();
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(harness.mdi_sent(), vec!["G0 X1".to_string()]);
        assert_eq!(blocks(&harness.app), vec!["G0 X1".to_string()]);
        assert!(harness.app.mdi.input.is_empty(), "the input is clear again");

        harness.app.mdi.input = "G1 Z-1 F100".into();
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(harness.mdi_sent().len(), 2);
        assert_eq!(harness.app.mdi.len(), 2, "a typed block appends");

        // Recall the first block, edit it, and Enter overwrites that line.
        harness.frame(&[egui::Key::ArrowUp]);
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.selection, Some(0));
        harness.app.mdi.input = "G0 X5".into();
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(
            blocks(&harness.app),
            vec!["G0 X5".to_string(), "G1 Z-1 F100".to_string()]
        );
        assert_eq!(harness.mdi_sent().last().map(String::as_str), Some("G0 X5"));

        // A block that does not parse is not committed and nothing is sent.
        let sent = harness.mdi_sent().len();
        harness.app.mdi.input = "G1 X".into();
        harness.frame(&[egui::Key::Enter]);
        assert_eq!(harness.mdi_sent().len(), sent);
        assert_eq!(harness.app.mdi.len(), 2);
        assert!(harness.app.mdi.last_error.is_some(), "the strip says why");
        let _ = limit;
    }

    #[test]
    fn up_and_down_recall_a_buffer_line_and_keep_the_selection_on_it() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X1".into());
        harness.app.mdi.commit(limit, "G1 X2".into());

        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.input, "G1 X2", "Up starts at the newest");
        assert_eq!(harness.app.mdi.selection, Some(1));
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.input, "G0 X1");
        assert_eq!(harness.app.mdi.selection, Some(0));

        harness.frame(&[egui::Key::ArrowDown]);
        assert_eq!(harness.app.mdi.input, "G1 X2");
        assert_eq!(harness.app.mdi.selection, Some(1));
        harness.frame(&[egui::Key::ArrowDown]);
        assert_eq!(harness.app.mdi.selection, None, "past the newest block");
        assert!(harness.app.mdi.input.is_empty());

        // Recall is in place: both lines stay in the listing.
        assert_eq!(harness.app.mdi.len(), 2);
        assert!(harness.mdi_sent().is_empty(), "recall issues nothing");
    }

    #[test]
    fn ctrl_backspace_deletes_the_selected_block_and_otherwise_clears_the_input() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);

        // No buffer line selected: the chord clears the whole input, so the
        // text edit never got it (it would have deleted one word).
        harness.app.mdi.input = "G0 X1 Y2".into();
        harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
        assert!(harness.app.mdi.input.is_empty());
        assert!(harness.mdi_sent().is_empty());

        harness.app.mdi.commit(limit, "G0 X1".into());
        harness.app.mdi.commit(limit, "G1 X2".into());
        harness.frame(&[egui::Key::ArrowUp]);
        assert_eq!(harness.app.mdi.selection, Some(1));
        harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
        assert_eq!(blocks(&harness.app), vec!["G0 X1".to_string()]);
        assert_eq!(
            harness.app.mdi.selection,
            Some(0),
            "the band stays on a line"
        );
        assert!(harness.mdi_sent().is_empty(), "deleting issues nothing");
    }

    #[test]
    fn ctrl_r_issues_the_selected_block_with_no_dialog() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X3".into());
        harness.app.mdi.selection = Some(0);

        harness.frame_with(&[(egui::Key::R, ctrl())]);
        assert_eq!(harness.mdi_sent(), vec!["G0 X3".to_string()]);
        assert!(harness.app.modal.is_none(), "Ctrl+R confirms nothing");
        assert_eq!(harness.app.mdi.len(), 1, "the block is already buffered");
    }

    #[test]
    fn a_blocked_run_puts_the_reason_in_the_strip_and_sends_nothing() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X3".into());
        harness.app.mdi.selection = Some(0);

        // Axes unhomed: the run-from-line gate, reported without a dialog.
        harness.app.snapshot.machine.homed.insert("X", false);
        harness.frame_with(&[(egui::Key::R, ctrl())]);
        assert!(harness.mdi_sent().is_empty(), "{:?}", harness.sent_debug());
        assert_eq!(
            harness.app.mdi.last_error.as_deref(),
            Some("home every axis first")
        );
        assert!(harness.app.modal.is_none());

        // The panel not in MDI blocks it the same way.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X3".into());
        harness.app.mdi.selection = Some(0);
        screens::mdi::run_selected(&mut harness.app);
        assert!(harness.mdi_sent().is_empty());
        assert_eq!(
            harness.app.mdi.last_error.as_deref(),
            Some("panel mode is not MDI")
        );
    }

    #[test]
    fn ctrl_g_selects_a_buffer_line_and_leaves_the_program_goto_alone() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X1".into());
        harness.app.mdi.commit(limit, "G1 X2".into());

        harness.frame_with(&[(egui::Key::G, ctrl())]);
        assert!(
            matches!(harness.app.modal, Some(Modal::GoToLine { mdi: true, .. })),
            "Ctrl+G on the pane targets the buffer"
        );
        // Accepting closes the dialog first, the way the modal handler does.
        harness.app.modal = None;
        harness.app.go_to_line(99, true);
        assert_eq!(harness.app.mdi.selection, Some(1), "bounded by the buffer");
        assert_eq!(harness.app.mdi.input, "G1 X2");

        // The Program page keeps its own go-to-line.
        harness.app.set_page(Page::Program);
        harness.frame_with(&[(egui::Key::G, ctrl())]);
        assert!(matches!(
            harness.app.modal,
            Some(Modal::GoToLine { mdi: false, .. })
        ));
        harness.app.go_to_line(1, false);
        assert_eq!(harness.app.program.selected_line, 1);
        assert_eq!(
            harness.app.mdi.selection,
            Some(1),
            "the buffer selection is untouched"
        );
    }

    #[test]
    fn ctrl_s_writes_the_buffer_through_mdi_file() {
        let mut harness = mdi_harness();
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X0 Y0".into());
        harness.app.mdi.commit(limit, "G1 Z-1 F100".into());
        let prefix = harness.app.profile.machine_ini.program_prefix.clone();

        harness.frame_with(&[(egui::Key::S, ctrl())]);
        assert!(
            matches!(harness.app.modal, Some(Modal::MdiSave(_))),
            "Ctrl+S opens the dialog"
        );
        // The wrapper starts ticked, so the saved program carries the
        // profile's preamble and postamble.
        harness
            .app
            .edit_mdi_save(|dialog| dialog.name = "part".into());
        harness.app.confirm_mdi_save();
        assert!(harness.app.modal.is_none());
        let wrapped = std::fs::read_to_string(prefix.join("part.ngc")).unwrap();
        let mut lines = wrapped.lines();
        assert_eq!(lines.next(), Some("G17"));
        assert_eq!(lines.last(), Some("M30"));
        assert!(wrapped.contains("G0 X0 Y0\nG1 Z-1 F100\n"));
        assert_eq!(harness.app.mdi.len(), 2, "the buffer is kept after a save");

        // Unticked: the blocks alone.
        harness.app.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
            name: "plain".into(),
            wrap: false,
            ..Default::default()
        }));
        harness.app.confirm_mdi_save();
        assert_eq!(
            std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
            "G0 X0 Y0\nG1 Z-1 F100\n"
        );

        // An existing name needs the confirm inside the dialog first.
        harness.app.mdi.commit(limit, "G0 Z5".into());
        harness.app.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
            name: "plain".into(),
            wrap: false,
            ..Default::default()
        }));
        harness.app.confirm_mdi_save();
        assert!(
            matches!(
                harness.app.modal,
                Some(Modal::MdiSave(crate::state::MdiSaveDialog {
                    confirm_overwrite: true,
                    ..
                }))
            ),
            "the same dialog asks for the overwrite"
        );
        assert_eq!(
            std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
            "G0 X0 Y0\nG1 Z-1 F100\n",
            "nothing is written before the confirm"
        );
        harness.app.confirm_mdi_save();
        assert!(harness.app.modal.is_none());
        assert_eq!(
            std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
            "G0 X0 Y0\nG1 Z-1 F100\nG0 Z5\n"
        );
    }

    #[test]
    fn protect_refuses_the_mdi_save_before_it_opens_the_dialog() {
        let mut harness = ProbeHarness::new(PanelMode::Mdi, true, true);
        harness.frame(&[]);
        let limit = history_limit(&harness.app);
        harness.app.mdi.commit(limit, "G0 X0".into());
        let prefix = harness.app.profile.machine_ini.program_prefix.clone();

        harness.frame_with(&[(egui::Key::S, ctrl())]);
        assert!(harness.app.modal.is_none(), "protect keeps the dialog shut");
        assert!(!prefix.join("part.ngc").exists());

        // A protect that turns on while the dialog is open still refuses.
        harness.app.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
            name: "part".into(),
            ..Default::default()
        }));
        harness.app.confirm_mdi_save();
        assert!(
            harness.app.modal.is_some(),
            "the dialog stays open with the reason"
        );
        assert!(!prefix.join("part.ngc").exists());
    }

    #[test]
    fn the_parent_row_is_selectable_and_enter_opens_the_parent() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        // Keep the removable-media poll out of the test; the NC pane is enough.
        harness.app.files.usb_root = Some(harness.app.files.nc_root.clone());
        let root = harness.app.files.nc_root.clone();
        let part = root.join("part");
        std::fs::create_dir_all(&part).unwrap();
        std::fs::write(part.join("a.ngc"), "G0 X0\n").unwrap();
        harness.app.set_page(Page::Files);
        harness.app.files.nc_current = part.clone();
        harness.app.files.nc_entries = crate::state::read_dir_entries(&part);
        harness.app.files.nc_selected = 0;
        harness.app.files.nc_parent_selected = false;

        harness.frame(&[]);
        harness.frame(&[egui::Key::ArrowUp]);
        assert!(
            harness.app.files.nc_parent_selected,
            "Up from the first entry selects the .. row inside a directory"
        );
        assert_eq!(harness.app.files.nc_selected, 0);

        harness.frame(&[egui::Key::Enter]);
        assert_eq!(
            harness.app.files.nc_current, root,
            "Enter on the .. row opens the parent directory"
        );
        assert!(!harness.app.files.nc_parent_selected);
    }

    #[test]
    fn nc_files_has_no_parent_row_at_its_root_by_default() {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.files_page_with_one_file("part.ngc");
        assert!(
            !harness.app.files.allow_parent_above_root,
            "[files] show_parent_above_root is off in base.toml"
        );
        harness.frame(&[]);
        harness.frame(&[egui::Key::ArrowUp]);
        assert!(
            !harness.app.files.nc_parent_selected,
            "NC_FILES stops at PROGRAM_PREFIX without the profile option"
        );
        assert_eq!(harness.app.files.nc_current, harness.app.files.nc_root);
    }

    #[test]
    fn history_limit_drops_the_oldest_block() {
        let mut mdi = MdiUi::new();
        mdi.commit(2, "G0 X1".into());
        mdi.commit(2, "G1 X2".into());
        mdi.commit(2, "G2 X3".into());
        let texts: Vec<String> = mdi.blocks.iter().map(|(block, _)| block.clone()).collect();
        assert_eq!(texts, vec!["G1 X2".to_string(), "G2 X3".to_string()]);

        // Committing over a selected line rewrites it instead of growing.
        mdi.selection = Some(1);
        mdi.commit(2, "G3 X4".into());
        assert_eq!(mdi.len(), 2);
        assert_eq!(mdi.blocks[1].0, "G3 X4");
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
