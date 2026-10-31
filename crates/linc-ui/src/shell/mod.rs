//! LiNC application shell: fixed top/left/bottom chrome, keyboard model,
//! mode-driven page switching, safety banners and modal confirmations.
//!
//! `LincApp` holds the state; every other file here carries one concern's
//! `impl LincApp` block. Methods can live in any module of the crate, and a
//! submodule can reach `LincApp`'s private fields, so the split is a move of
//! text, not a change of shape.

use crate::screens;
use crate::state::*;
use crate::theme;
use egui::{
    Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};
use linc_core::command::{BackendEvent, CommandResult, MachineCommand};
use linc_core::config::Profile;
use linc_core::gcode::{parse_preview_full, Bounds3, Preview, Tools};
use linc_core::mdi_file::SaveError;
use linc_core::model::{
    format_clock, now_ms, Alarm, AlarmSeverity, AlarmSource, AxisMap, MachinePower,
    MachineSnapshot, MachineVariant, PanelMode, PanelModeValidity, PositionSet, ProgramState,
    ToolChangeState, ToolChangeStatus,
};
use linc_core::probe::ProbeParameters;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod chrome;
mod commands;
mod diag;
mod dialogs;
mod gates;
mod hints;
mod jog;
mod keyboard;
mod nav;
mod prefs;
mod program;
mod status;
mod sync;
mod view;

// Every submodule reaches its siblings through `use super::*`, which reads
// the names imported here. Rust cannot see that use from this file, so each
// glob is exempt from the unused-import lint.
#[allow(unused_imports)]
use chrome::*;
#[allow(unused_imports)]
use commands::*;
#[allow(unused_imports)]
use diag::*;
#[allow(unused_imports)]
use dialogs::*;
#[allow(unused_imports)]
use gates::*;
#[allow(unused_imports)]
use hints::*;
#[allow(unused_imports)]
use jog::*;
#[allow(unused_imports)]
use keyboard::*;
#[allow(unused_imports)]
use nav::*;
#[allow(unused_imports)]
use prefs::*;
#[allow(unused_imports)]
use program::*;
#[allow(unused_imports)]
use status::*;
#[allow(unused_imports)]
use sync::*;
#[allow(unused_imports)]
use view::*;

pub use commands::move_progress;
pub use gates::{probe_trip_banner_text, PROBE_TRIP_ACTION};
pub use keyboard::FocusRegion;
pub use nav::{mode_page, nav_row_geometry, page_from_name, NavRowGeometry, NAV_ITEMS};
pub use status::{
    active_offset, fps_step, limits_override_indicator, machine_bounds, override_readout,
    state_label,
};

pub const APP_TITLE: &str = "LiNC";

pub struct LincApp {
    /// Publishes state for `linc-mcp`. `None` in tests and when the diagnostic
    /// socket could not be created.
    pub diag_bus: Option<std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>>,
    /// Smoothed frame rate, shown in the top bar and published for diagnostics.
    pub fps: f32,
    pub profile: Profile,
    pub backend: Box<dyn UiBackend>,
    pub snapshot: MachineSnapshot,
    pub graph_style: GraphStyle,
    pub program: ProgramUi,
    pub offsets: OffsetsUi,
    pub tools: ToolsUi,
    pub probe: ProbeUi,
    pub mdi: MdiUi,
    pub help: HelpUi,
    pub files: FilesUi,
    pub manual: ManualUi,
    pub design_scale: f32,
    pub(super) nav: NavUi,
    pub(super) diagnostics: DiagnosticsUi,
    pub(super) overlay: OverlayUi,
    pub(super) jog: JogUi,
    pub(super) prefs: PrefsUi,
    pub(super) view: ViewState,
    pub(super) following: FollowError,
    pub(super) chrome: ChromeUi,
}

impl LincApp {
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
            preview = parse_preview_full(
                text,
                &axes,
                offset,
                [0.0; 3],
                &profile.subroutines(),
                &Tools::from_rows(&snapshot.tool_table.rows),
            );
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
                pending_load: false,
                program_was_running: false,
                ..Default::default()
            },
            offsets: OffsetsUi::default(),
            tools: ToolsUi::default(),
            probe: ProbeUi::new(probe_params, active_probe_wcs),
            mdi: MdiUi::restored(&preferences.mdi_history, profile.config.mdi.history_limit),
            help: HelpUi::default(),
            files: FilesUi::new(
                profile.machine_ini.program_prefix.clone(),
                profile.config.files.show_parent_above_root,
            ),
            manual: ManualUi {
                view: if profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    linc_view::ViewMode::Iso3d
                },
                ..Default::default()
            },
            design_scale: configured_scale,
            diag_bus: None,
            fps: 0.0,
            nav: NavUi {
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
                homing_return_page: start_page,
                page_focus_requested: true,
                pages: nav_pages(&profile.config.ui.pages),
            },
            diagnostics: DiagnosticsUi {
                tab: DiagnosticsTab::Overview,
                search: String::new(),
                severity: None,
                source: None,
                clear_view: false,
                log: VecDeque::new(),
                last_sample: 0,
                viewport: linc_core::diag::ViewportInfo::default(),
                regions: Vec::new(),
                frame_time_ms: 0.0,
            },
            overlay: OverlayUi {
                modal: None,
                toasts: Vec::new(),
                modal_open_last_frame: None,
                tool_prompt_open_last_frame: false,
                overlay_was_open: false,
                messages_dialog: false,
                messages_scroll: 0.0,
                confirm_token: None,
            },
            jog: JogUi {
                increments: crate::state::finite_increments(&profile.machine_ini.increments),
                index: preferences.jog_increment.unwrap_or(0),
                speed_percent: 100.0,
                speed_published: None,
            },
            view: ViewState {
                origin: [0.0; 3],
                trail_cleared: false,
                trail_cleared_tip: None,
                move_distance: linc_core::model::AxisMap::new(),
                move_progress: linc_core::model::AxisMap::new(),
                preview_ticks: 0,
            },
            following: FollowError {
                active: false,
                history: linc_core::model::AxisMap::new(),
                run_max: linc_core::model::AxisMap::new(),
                run_abs_sum: linc_core::model::AxisMap::new(),
                run_samples: linc_core::model::AxisMap::new(),
                run_active: false,
            },
            chrome: ChromeUi {
                quit_requested: false,
                lathe_diameter: preferences.lathe_diameter.unwrap_or(false),
                last_snapshot_ms: now_ms(),
                alarm_id_seen: 0,
                last_connection: Some(snapshot.panel.connected),
                last_protect: Some(snapshot.panel.protect_effective()),
            },
            prefs: PrefsUi {
                keymap: crate::state::Keymap::from_config(&profile.config.keys),
                dirty: false,
                saved_at: std::time::Instant::now(),
                preferences,
            },
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
        let opening_page = app.nav.page;
        app.remember_page(opening_page);
        app.help.ensure_loaded();
        app.offsets = OffsetsUi::default();
        app.sync_offsets_from_snapshot(true);
        app.sync_tools_from_snapshot(true);
        if app.nav.page == Page::Program
            && app.snapshot.panel.mode.mode() == Some(PanelMode::Edit)
            && app.editing_allowed()
        {
            app.program.view = ProgramView::Code;
            app.program.focus_editor = true;
        }
        app
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        self.sync();
        // A fresh model each frame: a region that disappears must not linger.
        self.diagnostics.regions.clear();
        let screen = ui.available_rect_before_wrap();
        self.diagnostics.viewport = linc_core::diag::ViewportInfo {
            width: screen.width(),
            height: screen.height(),
            pixels_per_point: ui.ctx().pixels_per_point(),
            fullscreen: ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false)),
        };
        self.diagnostics.frame_time_ms = ui.ctx().input(|input| input.unstable_dt * 1000.0);
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
            .show(ui, |ui| self.draw_nav(ui));
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
                // mouse or Tab. While a dialog is open the keyboard belongs to
                // it, so the request stays pending until the dialog closes:
                // taking it here left a freshly opened dialog with no focused
                // button at all.
                if self.nav.page_focus_requested && !self.overlay_open() {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::unique("linc-page-focus"), true)
                    });
                    self.nav.page_focus_requested = false;
                }
                self.banners(ui);
                // The central page must end above the fixed bottom action bar.
                let page_bottom = ui.ctx().input(|input| input.viewport_rect().bottom())
                    - theme::BOTTOM_BAR_HEIGHT;
                let page_height = (page_bottom - ui.cursor().min.y).max(120.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), page_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| match self.nav.page {
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
            .diagnostics
            .regions
            .iter()
            .map(|region| region.x + region.width)
            .fold(0.0f32, f32::max);
        let bottom = self
            .diagnostics
            .regions
            .iter()
            .map(|region| region.y + region.height)
            .fold(0.0f32, f32::max);
        self.diagnostics.viewport.width = self.diagnostics.viewport.width.max(right);
        self.diagnostics.viewport.height = self.diagnostics.viewport.height.max(bottom);
        self.draw_modal(ui.ctx());
        self.draw_tool_change_prompt(ui.ctx());
        crate::screens::help::draw_dialog(self, ui.ctx());
        crate::screens::common::messages_dialog(self, ui.ctx());
        // Every dialog is drawn by now, so the set of overlays is final for
        // this frame. egui drops the focus together with the widget that held
        // it, and none of the pages asks for it again: without this the
        // keyboard went nowhere after a dialog closed. Restore it on the frame
        // after the last overlay has gone.
        let overlay_open = self.overlay_open();
        if self.overlay.overlay_was_open && !overlay_open {
            self.restore_page_keyboard();
        }
        self.overlay.overlay_was_open = overlay_open;
        self.draw_toasts(ui.ctx());
        self.save_preferences(false);
        if self.chrome.quit_requested {
            self.save_preferences(true);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for LincApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::apply_design_zoom(ui.ctx(), self.design_scale);
        let dt = ui.ctx().input(|input| input.stable_dt);
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

#[cfg(test)]
mod ui_tests;
