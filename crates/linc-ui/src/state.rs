//! HMI-local state, backend client and screen models.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use linc_core::backend::BackendHandle;
use linc_core::command::{BackendEvent, MachineCommand, MachineError};
use linc_core::editor::Editor;
use linc_core::gcode::Preview;
use linc_core::help::HelpBundle;
use linc_core::model::MachineSnapshot;
use linc_core::probe::{ProbeCategory, ProbeMode, ProbeOperation, ProbeParameters};
use linc_view::{theme as view_theme, TrajectoryView, ViewMode};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Machine,
    Program,
    Offsets,
    Tools,
    Diagnostics,
    Help,
    Files,
    Probe,
    Mdi,
    Homing,
    Manual,
    Settings,
}

impl Page {
    pub const NAV: [Page; 6] = [
        Page::Machine,
        Page::Program,
        Page::Offsets,
        Page::Tools,
        Page::Diagnostics,
        Page::Help,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Page::Machine => "MACHINE",
            Page::Program => "PROGRAM",
            Page::Offsets => "OFFSETS",
            Page::Tools => "TOOLS",
            Page::Diagnostics => "DIAGNOSTICS",
            Page::Help => "G-CODE HELP",
            Page::Files => "FILES",
            Page::Probe => "TOUCH PROBE",
            Page::Mdi => "MDI",
            Page::Homing => "HOMING",
            Page::Manual => "MANUAL",
            Page::Settings => "SETTINGS",
        }
    }

    pub fn keycap(self) -> Option<&'static str> {
        match self {
            Page::Machine => Some("MAIN"),
            Page::Program => Some("FILE"),
            Page::Offsets => Some("OFFSET"),
            Page::Tools => Some("TOOL"),
            Page::Diagnostics => Some("WIN"),
            _ => None,
        }
    }

    pub fn is_mode_driven(self) -> bool {
        matches!(self, Page::Mdi | Page::Homing | Page::Manual)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramView {
    Split,
    Code,
}

impl ProgramView {
    pub fn label(self) -> &'static str {
        match self {
            Self::Split => "SPLIT",
            Self::Code => "CODE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticsTab {
    Overview,
    PanelLink,
    HalNml,
    Motion,
    Turret,
    Io,
    Logs,
}

impl DiagnosticsTab {
    pub const ALL: [DiagnosticsTab; 7] = [
        DiagnosticsTab::Overview,
        DiagnosticsTab::PanelLink,
        DiagnosticsTab::HalNml,
        DiagnosticsTab::Motion,
        DiagnosticsTab::Turret,
        DiagnosticsTab::Io,
        DiagnosticsTab::Logs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "OVERVIEW",
            Self::PanelLink => "PANEL LINK",
            Self::HalNml => "HAL / NML",
            Self::Motion => "MOTION",
            Self::Turret => "TURRET / LIVE TOOL",
            Self::Io => "I/O",
            Self::Logs => "LOGS",
        }
    }
}

pub struct Toast {
    pub text: String,
    pub color: egui::Color32,
    pub until: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingAction {
    Navigate(Page),
    OpenFiles,
    LoadProgram(PathBuf),
    Quit,
    NewFile,
    OpenRecent(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    ConfirmDiscard(PendingAction),
    ConfirmQuit,
    /// Start the loaded program at a line, with the skipped-context warning.
    ConfirmRunFromLine {
        line: u32,
        block: String,
    },
    /// Delete a file from one of the Files panes.
    ConfirmDelete {
        path: std::path::PathBuf,
        label: String,
        usb: bool,
    },
    ConfirmOverwrite(String),
    Error(String),
    Info(String),
}

pub trait UiBackend {
    fn snapshot(&mut self) -> MachineSnapshot;
    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError>;
    fn drain_events(&mut self) -> Vec<BackendEvent>;
}

impl UiBackend for BackendHandle {
    fn snapshot(&mut self) -> MachineSnapshot {
        BackendHandle::snapshot(self)
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        BackendHandle::send(self, command);
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        BackendHandle::drain_events(self)
    }
}

/// Synchronous backend for tests and the headless demo.
pub struct LocalBackend {
    pub backend: linc_core::sim::SimBackend,
}

impl LocalBackend {
    pub fn new(profile: linc_core::config::Profile, mode: linc_core::model::PanelMode) -> Self {
        Self {
            backend: linc_core::sim::SimBackend::new(profile, mode),
        }
    }
}

impl UiBackend for LocalBackend {
    fn snapshot(&mut self) -> MachineSnapshot {
        linc_core::backend::MachineBackend::snapshot(&mut self.backend)
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        linc_core::backend::MachineBackend::send(&mut self.backend, command)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

#[derive(Default)]
pub struct HelpUi {
    pub bundle: Option<std::sync::Arc<HelpBundle>>,
    pub query: String,
    pub focus_search: bool,
    pub selected_anchor: Option<String>,
    pub selected_row: usize,
    pub history: Vec<String>,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub scroll_to_anchor: bool,
}

impl HelpUi {
    pub fn ensure_loaded(&mut self) {
        if self.bundle.is_none() {
            match HelpBundle::load_bundled() {
                Ok(bundle) => self.bundle = Some(std::sync::Arc::new(bundle)),
                Err(error) => log::error!("cannot load help bundle: {error}"),
            }
        }
    }

    pub fn open(&mut self, anchor: &str) {
        if let Some(current) = self.selected_anchor.clone() {
            self.history.push(current);
        }
        self.selected_anchor = Some(anchor.to_string());
        self.scroll_to_anchor = true;
    }

    pub fn back(&mut self) -> bool {
        if let Some(previous) = self.history.pop() {
            self.selected_anchor = Some(previous);
            self.scroll_to_anchor = true;
            true
        } else {
            false
        }
    }

    pub fn texture(&mut self, ctx: &egui::Context, name: &str) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.textures.get(name) {
            return Some(texture.clone());
        }
        let bytes = crate::assets::image_bytes(name)?;
        let image = image::load_from_memory(bytes).ok()?.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        let texture = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
        self.textures.insert(name.to_string(), texture.clone());
        Some(texture)
    }
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountState {
    NoUsb,
    Mounting,
    Mounted,
    Error(String),
}

pub struct FilesUi {
    pub mount_state: MountState,
    pub usb_root: Option<PathBuf>,
    pub usb_label: String,
    pub usb_free: Option<u64>,
    pub usb_current: Option<PathBuf>,
    pub usb_entries: Vec<FileEntry>,
    pub usb_selected: usize,
    pub usb_focused: bool,
    pub nc_root: PathBuf,
    pub nc_current: PathBuf,
    pub nc_entries: Vec<FileEntry>,
    pub nc_selected: usize,
    pub status: String,
    pub error: Option<String>,
    pub copy: Option<CopyJob>,
    pub mount_job: Option<std::sync::mpsc::Receiver<Result<Option<String>, String>>>,
    pub last_mount_attempt: Instant,
    pub pending_overwrite: Option<(PathBuf, PathBuf, PathBuf)>,
    pub safe_to_remove: bool,
    /// Latest copy progress. `poll_copy` drains the channel every frame, so the
    /// value has to be latched here or the progress label never renders.
    pub copy_progress: Option<(u64, u64)>,
    pub last_refresh: Instant,
}

impl FilesUi {
    pub fn new(nc_root: PathBuf) -> Self {
        let mut ui = Self {
            mount_state: MountState::NoUsb,
            usb_root: None,
            usb_label: String::new(),
            usb_free: None,
            usb_current: None,
            usb_entries: Vec::new(),
            usb_selected: 0,
            usb_focused: false,
            nc_root: nc_root.clone(),
            nc_current: nc_root,
            nc_entries: Vec::new(),
            nc_selected: 0,
            status: String::new(),
            error: None,
            copy: None,
            mount_job: None,
            last_mount_attempt: Instant::now() - Duration::from_secs(10),
            pending_overwrite: None,
            safe_to_remove: false,
            copy_progress: None,
            last_refresh: Instant::now() - Duration::from_secs(10),
        };
        ui.refresh();
        ui
    }

    pub fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        if !self.nc_current.starts_with(&self.nc_root) {
            self.nc_current = self.nc_root.clone();
        }
        self.nc_entries = read_dir_entries(&self.nc_current);
        self.refresh_usb();
    }

    pub fn refresh_usb(&mut self) {
        match crate::files::discover_usb() {
            Ok(Some(usb)) => {
                self.usb_root = Some(usb.path.clone());
                self.usb_label = usb.label;
                self.usb_free = usb.free;
                self.mount_state = MountState::Mounted;
                // A freshly mounted device is in use again: the previous
                // eject's SAFE TO REMOVE must not carry over to it.
                self.safe_to_remove = false;
                self.usb_current = Some(usb.path.clone());
                self.usb_entries = read_dir_entries(&usb.path);
                self.usb_selected = self
                    .usb_selected
                    .min(self.usb_entries.len().saturating_sub(1));
                self.error = None;
            }
            Ok(None) => {
                self.mount_state = MountState::NoUsb;
                self.usb_root = None;
                self.usb_entries.clear();
                self.usb_selected = 0;
                self.usb_free = None;
                // Try udisks2 auto-mount in the background; a polkit prompt or
                // D-Bus timeout must never block the UI.
                if self.mount_job.is_none()
                    && self.last_mount_attempt.elapsed() > Duration::from_secs(5)
                {
                    self.last_mount_attempt = Instant::now();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::Builder::new()
                        .name("linc-udisks2-mount".into())
                        .spawn(move || {
                            let _ = tx.send(crate::udisks2::mount_first_removable());
                        })
                        .ok();
                    self.mount_job = Some(rx);
                    self.mount_state = MountState::Mounting;
                }
            }
            Err(error) => {
                self.mount_state = MountState::Error(error.clone());
                self.error = Some(error);
            }
        }
    }

    pub fn poll_mount(&mut self) {
        if self.mount_job.is_none()
            && self.usb_root.is_none()
            && self.last_mount_attempt.elapsed() > Duration::from_secs(5)
        {
            self.last_mount_attempt = Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("linc-udisks2-mount".into())
                .spawn(move || {
                    let _ = tx.send(crate::udisks2::mount_first_removable());
                })
                .ok();
            self.mount_job = Some(rx);
            self.mount_state = MountState::Mounting;
            return;
        }
        let Some(rx) = &self.mount_job else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(_path))) => {
                self.mount_job = None;
                self.mount_state = MountState::NoUsb;
                self.refresh_usb();
            }
            Ok(Ok(None)) => {
                self.mount_job = None;
                self.mount_state = MountState::NoUsb;
            }
            Ok(Err(error)) => {
                self.mount_job = None;
                self.mount_state = MountState::Error(error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.mount_job = None;
                self.mount_state = MountState::NoUsb;
            }
        }
    }

    pub fn nc_selected_path(&self) -> Option<&FileEntry> {
        self.nc_entries.get(self.nc_selected)
    }

    pub fn usb_selected_path(&self) -> Option<&FileEntry> {
        self.usb_entries.get(self.usb_selected)
    }
}

pub struct CopyJob {
    pub rx: std::sync::mpsc::Receiver<CopyProgress>,
    pub label: String,
    pub started: Instant,
}

#[derive(Clone, Debug)]
pub enum CopyProgress {
    Progress { copied: u64, total: u64 },
    Done,
    Error(String),
}

pub fn read_dir_entries(root: &std::path::Path) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    let Ok(read) = std::fs::read_dir(root) else {
        return entries;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && name != "." {
            // Dot files are hidden; folder navigation only.
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path);
        let is_symlink = metadata
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let followed = std::fs::metadata(&path);
        let is_dir = followed.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = followed.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = followed.ok().and_then(|m| m.modified().ok());
        entries.push(FileEntry {
            name,
            path,
            is_dir,
            is_symlink,
            size,
            modified,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

pub struct ProgramUi {
    pub editor: Editor,
    pub view: ProgramView,
    pub preview: Preview,
    pub last_valid_preview: Preview,
    pub preview_due: Option<Instant>,
    pub editor_revision_seen: u64,
    pub program_revision_seen: u64,
    pub loaded_path: Option<PathBuf>,
    pub search_needle: String,
    pub search_matches: Vec<usize>,
    pub search_index: usize,
    pub selected_line: usize,
    pub graph_zoom: f32,
    pub graph_pan: egui::Vec2,
    pub graph_view: ViewMode,
    pub fit_requested: bool,
    pub follow_current: bool,
    pub focus_editor: bool,
    pub code_scroll_to_line: Option<usize>,
    pub external_change: bool,
}

impl Default for ProgramUi {
    fn default() -> Self {
        Self {
            editor: Editor::new(),
            view: ProgramView::Split,
            preview: Preview::empty(),
            last_valid_preview: Preview::empty(),
            preview_due: None,
            editor_revision_seen: 0,
            program_revision_seen: 0,
            loaded_path: None,
            search_needle: String::new(),
            search_matches: Vec::new(),
            search_index: 0,
            selected_line: 1,
            graph_zoom: 1.0,
            graph_pan: egui::Vec2::ZERO,
            graph_view: ViewMode::Iso3d,
            fit_requested: true,
            follow_current: true,
            focus_editor: false,
            code_scroll_to_line: None,
            external_change: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OffsetDraft {
    pub values: HashMap<String, String>,
    pub rotation: String,
}

pub struct OffsetsUi {
    pub selected: u8,
    pub rows: HashMap<u8, OffsetDraft>,
    pub g92: Vec<String>,
    pub editing: bool,
    pub error: Option<String>,
    pub revision_seen: u64,
}

impl Default for OffsetsUi {
    fn default() -> Self {
        Self {
            selected: 1,
            rows: HashMap::new(),
            g92: Vec::new(),
            editing: false,
            error: None,
            revision_seen: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolDraft {
    pub description: String,
    pub diameter: String,
    pub length: String,
    pub nose_radius: String,
    pub orientation: String,
    pub pocket: String,
    pub axes: HashMap<String, String>,
}

impl ToolDraft {
    pub fn from_row(row: &linc_core::model::ToolRow) -> Self {
        Self {
            description: row.description.clone(),
            diameter: row
                .geometry
                .diameter
                .map(|v| v.to_string())
                .unwrap_or_default(),
            length: row
                .geometry
                .length
                .map(|v| v.to_string())
                .unwrap_or_default(),
            nose_radius: row
                .geometry
                .nose_radius
                .map(|v| v.to_string())
                .unwrap_or_default(),
            orientation: row
                .geometry
                .orientation
                .map(|v| v.to_string())
                .unwrap_or_default(),
            pocket: row.pocket.map(|v| v.to_string()).unwrap_or_default(),
            axes: ["X", "Z", "I", "J"]
                .into_iter()
                .map(|axis| {
                    (
                        axis.to_string(),
                        row.geometry
                            .values
                            .get(axis)
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect(),
        }
    }
}

pub struct ToolsUi {
    pub target: String,
    /// Selected cell: the table has its own cursor so the operator can move
    /// without a mouse.
    pub selected_row: usize,
    pub selected_column: usize,
    /// True while a cell is being edited; Enter commits, Escape cancels.
    pub editing: bool,
    pub edit_buffer: String,
    pub edit_focus: bool,
    pub drafts: HashMap<u16, ToolDraft>,
    pub error: Option<String>,
    pub revision_seen: u64,
    pub confirm_token: Option<linc_core::model::ToolChangeToken>,
}

impl Default for ToolsUi {
    fn default() -> Self {
        Self {
            target: "1".into(),
            selected_row: 0,
            selected_column: 0,
            editing: false,
            edit_buffer: String::new(),
            edit_focus: false,
            drafts: HashMap::new(),
            error: None,
            revision_seen: 0,
            confirm_token: None,
        }
    }
}

pub struct ProbeUi {
    pub category: ProbeCategory,
    pub operation: String,
    pub params: ProbeParameters,
    pub wcs: String,
    pub measure_only: bool,
    pub require_apply: bool,
    pub error: Option<String>,
    pub last_run: Option<Instant>,
    pub apply_target: u8,
}

impl ProbeUi {
    pub fn new(params: ProbeParameters, wcs: String) -> Self {
        let operation = linc_core::probe::operations_in(ProbeCategory::OutsideCorners)
            .first()
            .map(|operation| operation.id.to_string())
            .unwrap_or_default();
        Self {
            category: ProbeCategory::OutsideCorners,
            operation,
            params,
            wcs,
            measure_only: false,
            require_apply: true,
            error: None,
            last_run: None,
            apply_target: 1,
        }
    }

    pub fn operation(&self) -> Option<&'static ProbeOperation> {
        linc_core::probe::operation(&self.operation)
    }

    pub fn mdi(&self) -> Option<String> {
        self.operation()
            .map(|operation| self.params.mdi_text(operation))
    }
}

pub struct MdiUi {
    pub input: String,
    pub focus: bool,
    pub history: VecDeque<(String, u64)>,
    pub history_index: Option<usize>,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub quick_commands: Vec<String>,
    pub syntax_ok: bool,
    pub panel_mode: Option<linc_core::model::PanelMode>,
}

impl MdiUi {
    pub fn new(quick_commands: Vec<String>) -> Self {
        Self {
            input: String::new(),
            focus: true,
            history: VecDeque::new(),
            history_index: None,
            last_result: None,
            last_error: None,
            quick_commands,
            syntax_ok: true,
            panel_mode: None,
        }
    }

    pub fn push_history(&mut self, limit: usize, command: String) {
        self.history
            .push_front((command, linc_core::model::now_ms()));
        while self.history.len() > limit {
            self.history.pop_back();
        }
        self.history_index = None;
    }
}

pub struct ManualUi {
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub view: ViewMode,
    pub fit: bool,
}

impl Default for ManualUi {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            view: ViewMode::Iso3d,
            fit: true,
        }
    }
}

pub struct SettingsUi {
    pub density: f32,
    pub show_keyboard: bool,
    pub page: usize,
}

impl Default for SettingsUi {
    fn default() -> Self {
        Self {
            density: 1.0,
            show_keyboard: true,
            page: 0,
        }
    }
}

pub struct GraphStyle {
    pub show_grid: bool,
    pub show_live_trail: bool,
    pub show_program_bounds: bool,
    pub show_machine_bounds: bool,
    pub show_stock: bool,
    pub highlight_selected_line: bool,
    pub show_legend: bool,
    pub trail_max_points: usize,
    pub perspective: bool,
}

impl GraphStyle {
    pub fn apply<'a>(
        &self,
        view: &mut TrajectoryView<'a>,
        selected_line: Option<usize>,
        current_line: Option<usize>,
    ) {
        view.show_grid = self.show_grid;
        view.show_live_trail = self.show_live_trail;
        view.show_program_bounds = self.show_program_bounds;
        view.show_machine_bounds = self.show_machine_bounds;
        view.show_stock = self.show_stock;
        view.show_selected_line = self.highlight_selected_line;
        view.show_legend = self.show_legend;
        view.selected_line = selected_line;
        view.current_line = current_line;
    }
}

pub fn toast(text: impl Into<String>, color: egui::Color32) -> Toast {
    Toast {
        text: text.into(),
        color,
        until: Instant::now() + Duration::from_secs(5),
    }
}

pub fn default_probe_mode(measure_only: bool) -> ProbeMode {
    if measure_only {
        ProbeMode::MeasureOnly
    } else {
        ProbeMode::ApplyWcs
    }
}

pub fn page_accent(page: Page) -> egui::Color32 {
    match page {
        Page::Machine | Page::Program => theme::ACCENT,
        Page::Offsets | Page::Tools => theme::ACCENT,
        Page::Diagnostics => theme::C1,
        Page::Help => theme::C2,
        Page::Files => theme::C4,
        Page::Probe => theme::C6,
        Page::Mdi | Page::Homing | Page::Manual => theme::C3,
        Page::Settings => theme::MUTED,
    }
}

pub fn view_from_config(value: &str, lathe: bool) -> ViewMode {
    if lathe {
        return ViewMode::LatheXz;
    }
    match value {
        "xy" | "top" => ViewMode::TopXy,
        "xz" | "side" => ViewMode::SideXz,
        _ => ViewMode::Iso3d,
    }
}

pub fn graph_style(profile: &linc_core::config::Profile) -> GraphStyle {
    GraphStyle {
        show_grid: profile.config.toolpath.show_grid,
        show_live_trail: profile.config.toolpath.show_live_trail,
        show_program_bounds: profile.config.toolpath.show_program_bounds,
        show_machine_bounds: profile.config.toolpath.show_machine_bounds,
        show_stock: profile.config.toolpath.show_stock,
        highlight_selected_line: profile.config.toolpath.highlight_selected_line,
        // SPEC §6.2: the legend must identify the loaded program path and the
        // live trail as separate layers. It was hardcoded off, so the two
        // colours were never keyed anywhere in the UI.
        show_legend: true,
        trail_max_points: profile.config.toolpath.trail_max_points,
        perspective: profile.config.toolpath.perspective,
    }
}

pub fn selected_line_color() -> egui::Color32 {
    view_theme::ACCENT
}

// ---------------------------------------------------------------------------
// Configured keymap
//
// `[keys]` in the profile is the operator-facing key reference. It used to be
// display-only: the real bindings were hardcoded, so a profile that rebound a
// key loaded cleanly and silently did nothing (CONFIGURATION.md §1.7 exists to
// prevent exactly that). The keymap is now parsed once and drives the shell.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: egui::Key,
    pub modifiers: egui::Modifiers,
}

impl Binding {
    pub const fn new(key: egui::Key, modifiers: egui::Modifiers) -> Self {
        Self { key, modifiers }
    }


    /// Short display form, e.g. `F5`, `Ctrl+Q`, `Esc`, `/`. Used by the
    /// bottom bar's keyboard-command hints.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.modifiers.ctrl {
            out.push_str("Ctrl+");
        }
        if self.modifiers.alt {
            out.push_str("Alt+");
        }
        if self.modifiers.shift {
            out.push_str("Shift+");
        }
        out.push_str(key_name(self.key));
        out
    }

    /// Parse a binding such as `F5`, `Escape`, `Ctrl+Q`, `Ctrl+ArrowUp`, `/`.
    /// Returns `None` for an unparseable value; the caller keeps its default.
    pub fn parse(text: &str) -> Option<Self> {
        let mut modifiers = egui::Modifiers::NONE;
        let mut key_text = text.trim();
        loop {
            let lower = key_text.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("ctrl+") {
                modifiers.ctrl = true;
                modifiers.command = true;
                key_text = &key_text[key_text.len() - rest.len()..];
            } else if let Some(rest) = lower.strip_prefix("shift+") {
                modifiers.shift = true;
                key_text = &key_text[key_text.len() - rest.len()..];
            } else if let Some(rest) = lower.strip_prefix("alt+") {
                modifiers.alt = true;
                key_text = &key_text[key_text.len() - rest.len()..];
            } else {
                break;
            }
        }
        let key = parse_key(key_text.trim())?;
        Some(Self { key, modifiers })
    }
}

/// Inverse of [`parse_key`] for the hints in the bottom bar.
pub fn key_name(key: egui::Key) -> &'static str {
    use egui::Key;
    match key {
        Key::A => "A",
        Key::B => "B",
        Key::C => "C",
        Key::D => "D",
        Key::E => "E",
        Key::F => "F",
        Key::G => "G",
        Key::H => "H",
        Key::I => "I",
        Key::J => "J",
        Key::K => "K",
        Key::L => "L",
        Key::M => "M",
        Key::N => "N",
        Key::O => "O",
        Key::P => "P",
        Key::Q => "Q",
        Key::R => "R",
        Key::S => "S",
        Key::T => "T",
        Key::U => "U",
        Key::V => "V",
        Key::W => "W",
        Key::X => "X",
        Key::Y => "Y",
        Key::Z => "Z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::Escape => "Esc",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Bksp",
        Key::Delete => "Del",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        Key::Slash => "/",
        Key::Space => "Space",
        _ => "?",
    }
}

fn parse_key(text: &str) -> Option<egui::Key> {
    use egui::Key;
    let upper = text.trim().to_ascii_uppercase();
    Some(match upper.as_str() {
        "A" => Key::A,
        "B" => Key::B,
        "C" => Key::C,
        "D" => Key::D,
        "E" => Key::E,
        "F" => Key::F,
        "G" => Key::G,
        "H" => Key::H,
        "I" => Key::I,
        "J" => Key::J,
        "K" => Key::K,
        "L" => Key::L,
        "M" => Key::M,
        "N" => Key::N,
        "O" => Key::O,
        "P" => Key::P,
        "Q" => Key::Q,
        "R" => Key::R,
        "S" => Key::S,
        "T" => Key::T,
        "U" => Key::U,
        "V" => Key::V,
        "W" => Key::W,
        "X" => Key::X,
        "Y" => Key::Y,
        "Z" => Key::Z,
        "0" => Key::Num0,
        "1" => Key::Num1,
        "2" => Key::Num2,
        "3" => Key::Num3,
        "4" => Key::Num4,
        "5" => Key::Num5,
        "6" => Key::Num6,
        "7" => Key::Num7,
        "8" => Key::Num8,
        "9" => Key::Num9,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "ESC" | "ESCAPE" => Key::Escape,
        "ENTER" | "RETURN" => Key::Enter,
        "TAB" => Key::Tab,
        "BACKSPACE" => Key::Backspace,
        "DELETE" | "DEL" => Key::Delete,
        "UP" | "ARROWUP" => Key::ArrowUp,
        "DOWN" | "ARROWDOWN" => Key::ArrowDown,
        "LEFT" | "ARROWLEFT" => Key::ArrowLeft,
        "RIGHT" | "ARROWRIGHT" => Key::ArrowRight,
        "HOME" => Key::Home,
        "END" => Key::End,
        "PAGEUP" | "PGUP" => Key::PageUp,
        "PAGEDOWN" | "PGDN" => Key::PageDown,
        "/" | "SLASH" => Key::Slash,
        "SPACE" => Key::Space,
        _ => return None,
    })
}

/// Resolved key bindings for the shell. Every entry falls back to the spec
/// default when the profile value is missing or unparseable.
#[derive(Clone, Debug)]
pub struct Keymap {
    pub machine: Binding,
    pub program: Binding,
    /// Separate CODE/SPLIT toggle. Unlike the F-key navigation bindings it is
    /// listed in the hint row, so the Program screen does not need buttons.
    pub program_cycle_view: Binding,
    /// Page-local to Program: fit the toolpath and clear the live trail.
    pub graph_fit: Binding,
    pub graph_clear: Binding,
    /// Page-local to Program: start the loaded program at the current line.
    pub program_run_from_line: Binding,
    /// Page-local to Files: copy the selection between the panes.
    pub files_copy_to_usb: Binding,
    pub files_copy_to_nc: Binding,
    pub files_delete: Binding,
    pub tools: Binding,
    pub offsets: Binding,
    pub diagnostics: Binding,
    pub back: Binding,
    pub quit: Binding,
    pub activate: Binding,
    pub open: Binding,
    pub save: Binding,
    pub undo: Binding,
    pub redo: Binding,
    pub find: Binding,
    pub refresh: Binding,
    pub mdi_issue: Binding,
    pub mdi_history_up: Binding,
    pub mdi_history_down: Binding,
    pub help_search: Binding,
    /// Bindings from the profile that could not be parsed, reported in Settings.
    pub rejected: Vec<String>,
}

fn binding(value: &str, default: Binding, rejected: &mut Vec<String>) -> Binding {
    match Binding::parse(value) {
        Some(binding) => binding,
        None => {
            rejected.push(value.to_string());
            default
        }
    }
}

impl Keymap {
    pub fn from_config(keys: &linc_core::config::KeysConfig) -> Self {
        use egui::{Key, Modifiers};
        let mut rejected = Vec::new();
        let none = Modifiers::NONE;
        let ctrl = Modifiers::CTRL;
        let keymap = Self {
            machine: binding(&keys.machine, Binding::new(Key::F5, none), &mut rejected),
            program: binding(&keys.program, Binding::new(Key::F6, none), &mut rejected),
            program_cycle_view: binding(
                &keys.program_cycle_view,
                Binding::new(Key::F10, none),
                &mut rejected,
            ),
            graph_fit: binding(&keys.graph_fit, Binding::new(Key::Num0, ctrl), &mut rejected),
            graph_clear: binding(&keys.graph_clear, Binding::new(Key::L, ctrl), &mut rejected),
            program_run_from_line: binding(
                &keys.program_run_from_line,
                Binding::new(Key::R, ctrl),
                &mut rejected,
            ),
            files_copy_to_usb: binding(
                &keys.files_copy_to_usb,
                Binding::new(Key::U, ctrl),
                &mut rejected,
            ),
            files_copy_to_nc: binding(
                &keys.files_copy_to_nc,
                Binding::new(Key::N, ctrl),
                &mut rejected,
            ),
            files_delete: binding(
                &keys.files_delete,
                Binding::new(Key::Delete, none),
                &mut rejected,
            ),
            tools: binding(&keys.tools, Binding::new(Key::F7, none), &mut rejected),
            offsets: binding(&keys.offsets, Binding::new(Key::F8, none), &mut rejected),
            diagnostics: binding(
                &keys.diagnostics,
                Binding::new(Key::Escape, ctrl),
                &mut rejected,
            ),
            back: binding(&keys.back, Binding::new(Key::Escape, none), &mut rejected),
            quit: binding(&keys.quit, Binding::new(Key::Q, ctrl), &mut rejected),
            activate: binding(&keys.activate, Binding::new(Key::Enter, none), &mut rejected),
            open: binding(&keys.open, Binding::new(Key::O, ctrl), &mut rejected),
            save: binding(&keys.save, Binding::new(Key::S, ctrl), &mut rejected),
            undo: binding(&keys.undo, Binding::new(Key::Z, ctrl), &mut rejected),
            redo: binding(&keys.redo, Binding::new(Key::Y, ctrl), &mut rejected),
            find: binding(&keys.find, Binding::new(Key::F, ctrl), &mut rejected),
            refresh: binding(&keys.refresh, Binding::new(Key::R, ctrl), &mut rejected),
            mdi_issue: binding(&keys.mdi_issue, Binding::new(Key::Enter, none), &mut rejected),
            mdi_history_up: binding(
                &keys.mdi_history_up,
                Binding::new(Key::ArrowUp, none),
                &mut rejected,
            ),
            mdi_history_down: binding(
                &keys.mdi_history_down,
                Binding::new(Key::ArrowDown, none),
                &mut rejected,
            ),
            help_search: binding(&keys.help_search, Binding::new(Key::Slash, none), &mut rejected),
            rejected,
        };
        keymap
    }
}

/// The finite steps from `[DISPLAY] INCREMENTS`.
///
/// The list conventionally starts with `JOG` (0.0), which means continuous jog.
/// The panel's JOG mode already selects continuous, so offering a zero step in
/// INC mode would only produce a keypress that moves nothing. Continuous is a
/// mode, not a step.
pub fn finite_increments(increments: &[(String, f64)]) -> Vec<(String, f64)> {
    increments
        .iter()
        .filter(|(_, value)| *value > 0.0)
        .cloned()
        .collect()
}

/// Jog speed in machine units per second.
///
/// The INI's default linear velocity is the 100% reference, and the operator's
/// percentage scales it; the value published to `linc.jog-speed` is already
/// absolute, so the machine HAL needs no multiplier.
pub fn jog_speed(default_linear_velocity: f64, percent: f64) -> f64 {
    (default_linear_velocity * percent.clamp(0.0, 100.0) / 100.0).max(0.0)
}

/// Resolve a selected increment against the INI's list.
///
/// Returns the label and the value in machine units. An out-of-range index
/// clamps to the last entry, and an empty list yields continuous jog, so a
/// profile without `[DISPLAY] INCREMENTS` still works.
pub fn jog_increment(increments: &[(String, f64)], index: usize) -> (String, f64) {
    match increments.get(index) {
        Some((label, value)) => (label.clone(), *value),
        None => match increments.last() {
            Some((label, value)) => (label.clone(), *value),
            None => ("JOG".to_string(), 0.0),
        },
    }
}

#[cfg(test)]
mod jog_speed_tests {
    use super::jog_speed;

    #[test]
    fn scales_the_ini_default_by_the_percentage() {
        // 60 mm/s is the shipped default; 100% is unchanged, 50% is half.
        assert_eq!(jog_speed(60.0, 100.0), 60.0);
        assert_eq!(jog_speed(60.0, 50.0), 30.0);
        assert_eq!(jog_speed(60.0, 0.0), 0.0);
    }

    #[test]
    fn clamps_out_of_range_percentages() {
        assert_eq!(jog_speed(60.0, 150.0), 60.0);
        assert_eq!(jog_speed(60.0, -20.0), 0.0);
    }
}

#[cfg(test)]
mod jog_increment_tests {
    use super::{finite_increments, jog_increment};

    fn sample() -> Vec<(String, f64)> {
        vec![
            ("JOG".to_string(), 0.0),
            ("10mm".to_string(), 10.0),
            ("1mm".to_string(), 1.0),
            (".1mm".to_string(), 0.1),
            (".01mm".to_string(), 0.01),
        ]
    }

    #[test]
    fn continuous_is_not_offered_as_a_step() {
        let steps = finite_increments(&sample());
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0], ("10mm".to_string(), 10.0));
        assert!(steps.iter().all(|(_, value)| *value > 0.0));
    }

    #[test]
    fn resolves_every_ini_step_in_order() {
        let steps = finite_increments(&sample());
        assert_eq!(jog_increment(&steps, 0), ("10mm".to_string(), 10.0));
        assert_eq!(jog_increment(&steps, 1), ("1mm".to_string(), 1.0));
        assert_eq!(jog_increment(&steps, 3), (".01mm".to_string(), 0.01));
    }

    #[test]
    fn clamps_and_survives_a_missing_list() {
        let steps = finite_increments(&sample());
        assert_eq!(jog_increment(&steps, 99), (".01mm".to_string(), 0.01));
        assert_eq!(jog_increment(&[], 3), ("JOG".to_string(), 0.0));
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::*;

    #[test]
    fn parses_the_documented_binding_forms() {
        assert_eq!(
            Binding::parse("F5"),
            Some(Binding::new(egui::Key::F5, egui::Modifiers::NONE))
        );
        assert_eq!(
            Binding::parse("Escape"),
            Some(Binding::new(egui::Key::Escape, egui::Modifiers::NONE))
        );
        let ctrl_q = Binding::parse("Ctrl+Q").expect("ctrl+q");
        assert_eq!(ctrl_q.key, egui::Key::Q);
        assert!(ctrl_q.modifiers.ctrl);
        let ctrl_up = Binding::parse("Ctrl+ArrowUp").expect("ctrl+arrowup");
        assert_eq!(ctrl_up.key, egui::Key::ArrowUp);
        assert!(ctrl_up.modifiers.ctrl);
        assert_eq!(
            Binding::parse("/"),
            Some(Binding::new(egui::Key::Slash, egui::Modifiers::NONE))
        );
        assert!(Binding::parse("NotAKey").is_none());
    }

    #[test]
    fn a_rebound_profile_key_changes_the_binding() {
        // The whole point of `[keys]`: a profile that rebinds a key must
        // actually rebind it, not just change the on-screen reference.
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.tools = "F9".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.tools.key, egui::Key::F9);
        assert!(keymap.rejected.is_empty());
    }

    #[test]
    fn an_unparseable_binding_falls_back_and_is_reported() {
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.machine = "Hyper+Z".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.machine.key, egui::Key::F5);
        assert_eq!(keymap.rejected, vec!["Hyper+Z".to_string()]);
    }
}
