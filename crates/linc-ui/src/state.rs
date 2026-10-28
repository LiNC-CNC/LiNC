//! HMI-local state, backend client and screen models.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use linc_core::backend::BackendHandle;
use linc_core::command::{BackendEvent, MachineCommand, MachineError};
use linc_core::config::Profile;
use linc_core::editor::Editor;
use linc_core::gcode::Preview;
use linc_core::help::HelpBundle;
use linc_core::model::MachineSnapshot;
use linc_core::probe::{ProbeCategory, ProbeOperation, ProbeParameters};
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
    Homing,
    Manual,
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
            Page::Homing => "HOMING",
            Page::Manual => "MANUAL",
        }
    }

    pub fn keycap(self) -> Option<&'static str> {
        match self {
            Page::Machine => Some("MAIN"),
            Page::Program => Some("FILE"),
            Page::Offsets => Some("OFFSET"),
            Page::Tools => Some("TOOL"),
            Page::Diagnostics => Some("Ctrl+D"),
            Page::Help => Some("Ctrl+H"),
            _ => None,
        }
    }

    /// Pages the panel position opens and closes by itself. MAIN is not one of
    /// them: MDI shows the pane *on* MAIN, it does not own a page.
    pub fn is_mode_driven(self) -> bool {
        matches!(self, Page::Homing | Page::Manual)
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

    /// The tabs `profile` can show, in `ALL` order: the turret tab needs both
    /// the profile key and a changer that has stations to report.
    pub fn visible(profile: &Profile) -> Vec<DiagnosticsTab> {
        let turret = profile.config.diagnostics.turret_tab && profile.changer.uses_pockets();
        Self::ALL
            .into_iter()
            .filter(|tab| *tab != Self::Turret || turret)
            .collect()
    }

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
    /// Ask for a line number to jump to: a line of the loaded program, or,
    /// with `mdi`, a line of the MDI buffer.
    GoToLine {
        buffer: String,
        mdi: bool,
    },
    /// Write the MDI buffer to a new program file under `PROGRAM_PREFIX`.
    MdiSave(MdiSaveDialog),
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

/// The MDI pane's save dialog: a file name and the pre/postamble wrapper.
///
/// An existing name is confirmed here, not in a second dialog: the same window
/// turns its write button into REPLACE, so the buffer cannot be lost to a
/// cascade of prompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdiSaveDialog {
    pub name: String,
    /// Write `mdi.save_preamble` above the buffer and `save_postamble` below.
    pub wrap: bool,
    /// Set once the name collides: the dialog then writes over it.
    pub confirm_overwrite: bool,
    pub error: Option<String>,
}

impl Default for MdiSaveDialog {
    fn default() -> Self {
        Self {
            name: String::new(),
            // The profile's pre/postamble is what a saved block list needs to
            // run on its own, so the wrapper starts ticked.
            wrap: true,
            confirm_overwrite: false,
            error: None,
        }
    }
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

#[derive(Clone, Debug)]
pub struct HelpDialog {
    pub anchor: String,
    pub history: Vec<String>,
    pub position: usize,
    pub scroll_to_top: bool,
    /// Keyboard cursor on the RELATED chips at the bottom; `None` means the
    /// arrows scroll the document.
    pub related: Option<usize>,
    /// Set from the scroll area so Down at the bottom can enter the chips.
    pub at_bottom: bool,
}

#[derive(Default)]
pub struct HelpUi {
    pub bundle: Option<std::sync::Arc<HelpBundle>>,
    pub query: String,
    pub focus_search: bool,
    pub selected_anchor: Option<String>,
    pub selected_row: usize,
    /// Full-screen reference dialog opened from the table.
    pub dialog: Option<HelpDialog>,
    pub textures: HashMap<String, egui::TextureHandle>,
    /// Bring the selected table row into view on the next draw.
    pub scroll_to_selection: bool,
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

    pub fn open_dialog(&mut self, anchor: &str) {
        self.selected_anchor = Some(anchor.to_string());
        self.dialog = Some(HelpDialog {
            anchor: anchor.to_string(),
            history: vec![anchor.to_string()],
            position: 0,
            scroll_to_top: true,
            related: None,
            at_bottom: false,
        });
    }

    pub fn close_dialog(&mut self) {
        self.dialog = None;
    }

    pub fn dialog_link(&mut self, anchor: &str) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.history.truncate(dialog.position + 1);
        if dialog.history.last().map(String::as_str) != Some(anchor) {
            dialog.history.push(anchor.to_string());
            dialog.position += 1;
        }
        dialog.anchor = anchor.to_string();
        dialog.scroll_to_top = true;
        dialog.related = None;
        dialog.at_bottom = false;
        self.selected_anchor = Some(anchor.to_string());
    }

    pub fn dialog_back(&mut self) -> bool {
        let Some(dialog) = self.dialog.as_mut() else {
            return false;
        };
        if dialog.position == 0 {
            return false;
        }
        dialog.position -= 1;
        dialog.anchor = dialog.history[dialog.position].clone();
        dialog.scroll_to_top = true;
        dialog.related = None;
        dialog.at_bottom = false;
        self.selected_anchor = Some(dialog.anchor.clone());
        true
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

/// An in-place file rename: the NAME cell becomes a text field with the
/// stem selected, like the tool table's description cell.
#[derive(Clone, Debug)]
pub struct FileRename {
    /// True for the USB list, false for NC_FILES.
    pub usb: bool,
    pub path: PathBuf,
    pub buffer: String,
    /// Characters to select when the field opens (the name without extension).
    pub select_len: usize,
    pub focus: bool,
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
    /// The `..` row is selected in that pane (it is above the entries).
    pub usb_parent_selected: bool,
    pub nc_parent_selected: bool,
    /// Show `..` in NC_FILES at its root; from `[files] show_parent_above_root`.
    pub allow_parent_above_root: bool,
    /// Rows a PageUp/PageDown moves, measured from the last drawn list.
    pub page_rows: usize,
    /// Bring the selected row into view on the next draw (keyboard moves).
    pub scroll_to_selection: bool,
    /// In-place rename in one of the lists, if any.
    pub rename: Option<FileRename>,
    pub nc_root: PathBuf,
    pub nc_current: PathBuf,
    pub nc_entries: Vec<FileEntry>,
    pub nc_selected: usize,
    /// The right pane shows the recent programs instead of the directory.
    /// Toggled with the square key, like CODE/SPLIT on the Program page.
    pub recent_shown: bool,
    pub recent_selected: usize,
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
    pub fn new(nc_root: PathBuf, allow_parent_above_root: bool) -> Self {
        let mut ui = Self {
            mount_state: MountState::NoUsb,
            usb_root: None,
            usb_label: String::new(),
            usb_free: None,
            usb_current: None,
            usb_entries: Vec::new(),
            usb_selected: 0,
            usb_focused: false,
            usb_parent_selected: false,
            nc_parent_selected: false,
            allow_parent_above_root,
            page_rows: 10,
            scroll_to_selection: false,
            rename: None,
            nc_root: nc_root.clone(),
            nc_current: nc_root,
            nc_entries: Vec::new(),
            nc_selected: 0,
            recent_shown: false,
            recent_selected: 0,
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
        if !self.nc_current.starts_with(&self.nc_root) && !self.allow_parent_above_root {
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
                // LINC_USB_DIR names the media explicitly (tests, a fixed
                // mount); never let udisks2 mount something else over it.
                if std::env::var_os("LINC_USB_DIR").is_none()
                    && self.mount_job.is_none()
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
        if std::env::var_os("LINC_USB_DIR").is_none()
            && self.mount_job.is_none()
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
    /// Line whose caret should be placed in the editor on the next frame. A jump
    /// can only move the highlight; the text cursor lives in `egui`'s edit state.
    pub cursor_line: Option<usize>,
    pub graph_zoom: f32,
    pub graph_pan: egui::Vec2,
    /// Orbit of the 3D toolpath: yaw about the vertical, pitch above the plane.
    pub graph_yaw: f32,
    pub graph_pitch: f32,
    pub graph_view: ViewMode,
    pub fit_requested: bool,
    /// Keep the toolpath framed on the program until the operator pans/zooms.
    pub graph_auto_fit: bool,
    /// Work offset the preview was parsed with, so corner labels can show work
    /// coordinates without guessing the active system.
    pub preview_offset: Option<linc_core::model::AxisMap<f64>>,
    pub follow_current: bool,
    /// Top row the program listing was drawn at. Kept so an idle listing can
    /// scroll one line at a time when the highlighted line leaves the window,
    /// instead of re-centring on every move.
    pub listing_first: std::cell::Cell<isize>,
    pub focus_editor: bool,
    pub code_scroll_to_line: Option<usize>,
    /// `snapshot.offsets.revision` the preview was drawn against, so a touch-off
    /// or DRO entry rebuilds it.
    pub preview_offsets_seen: u64,
    /// The offset rows the preview was parsed against. The controller changes
    /// them without an HMI write (startup publication, a program's own `G10`),
    /// and a stale frame shifts the tool marker by the whole offset.
    pub preview_offsets_rows: Vec<f64>,
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
            cursor_line: None,
            graph_zoom: 1.0,
            graph_pan: egui::Vec2::ZERO,
            graph_yaw: linc_view::DEFAULT_YAW,
            graph_pitch: linc_view::DEFAULT_PITCH,
            graph_view: ViewMode::Iso3d,
            fit_requested: true,
            graph_auto_fit: true,
            preview_offset: None,
            follow_current: true,
            listing_first: std::cell::Cell::new(1),
            focus_editor: false,
            code_scroll_to_line: None,
            preview_offsets_seen: 0,
            preview_offsets_rows: Vec::new(),
            external_change: false,
        }
    }
}

/// Evaluate a numeric entry, including simple arithmetic.
///
/// Operators are an operator convenience: entering `-25.15/2` or `10+2.5` is
/// quicker than working it out, and it means a copied expression from a drawing
/// can be typed straight in. Grammar: `+ - * /`, parentheses, unary minus.
pub fn eval_expression(text: &str) -> Option<f64> {
    let mut parser = ExpressionParser {
        bytes: text.trim().as_bytes(),
        position: 0,
    };
    let value = parser.sum()?;
    parser.skip_space();
    if parser.position == parser.bytes.len() && value.is_finite() {
        Some(value)
    } else {
        None
    }
}

struct ExpressionParser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl ExpressionParser<'_> {
    fn skip_space(&mut self) {
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.position += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.bytes.get(self.position).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.position += 1;
                    value += self.product()?;
                }
                Some(b'-') => {
                    self.position += 1;
                    value -= self.product()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.position += 1;
                    value *= self.unary()?;
                }
                Some(b'/') => {
                    self.position += 1;
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value /= divisor;
                }
                _ => return Some(value),
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(b'-') => {
                self.position += 1;
                Some(-self.unary()?)
            }
            Some(b'+') => {
                self.position += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek()? {
            b'(' => {
                self.position += 1;
                let value = self.sum()?;
                if self.peek()? != b')' {
                    return None;
                }
                self.position += 1;
                Some(value)
            }
            _ => {
                let start = self.position;
                while self
                    .bytes
                    .get(self.position)
                    .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
                {
                    self.position += 1;
                }
                if self.position == start {
                    return None;
                }
                std::str::from_utf8(&self.bytes[start..self.position])
                    .ok()?
                    .parse::<f64>()
                    .ok()
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OffsetDraft {
    pub values: HashMap<String, String>,
    pub rotation: String,
}

/// Inline editor for one axis of the Offsets page DRO.
#[derive(Clone, Debug, Default)]
pub struct DroEdit {
    pub axis: String,
    pub buffer: String,
    /// Put the keyboard in the field on the next frame.
    pub focus: bool,
}

pub struct OffsetsUi {
    pub selected: u8,
    /// Selected cell, the way the tool table tracks it: row is the index into
    /// the offsets list, column 0 is the WCS label and the rest are the value
    /// cells.
    pub selected_row: usize,
    pub selected_column: usize,
    pub rows: HashMap<u8, OffsetDraft>,
    pub g92: Vec<String>,
    pub editing: bool,
    pub edit_buffer: String,
    /// The cell editor was just opened: select its whole contents on the next
    /// frame, so typing replaces the value rather than appending to it.
    pub edit_focus: bool,
    pub error: Option<String>,
    pub revision_seen: u64,
    /// Axis whose DRO value is being typed, if any.
    pub dro_edit: Option<DroEdit>,
    /// When the page last drew: a gap means the operator just arrived, which is
    /// when the selection should jump to the active system.
    pub last_frame_ms: i64,
    /// The page's own toolpath view: the offset being edited is shown there, so
    /// it keeps separate zoom/pan from the Program page.
    pub preview_zoom: f32,
    pub preview_pan: egui::Vec2,
    pub preview_yaw: f32,
    pub preview_pitch: f32,
    pub preview_fit: bool,
    pub preview_auto_fit: bool,
}

impl Default for OffsetsUi {
    fn default() -> Self {
        Self {
            selected: 1,
            selected_row: 0,
            selected_column: 1,
            rows: HashMap::new(),
            g92: Vec::new(),
            edit_buffer: String::new(),
            editing: false,
            edit_focus: false,
            error: None,
            revision_seen: 0,
            dro_edit: None,
            last_frame_ms: 0,
            preview_zoom: 1.0,
            preview_pan: egui::Vec2::ZERO,
            preview_yaw: linc_view::DEFAULT_YAW,
            preview_pitch: linc_view::DEFAULT_PITCH,
            preview_fit: true,
            preview_auto_fit: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolDraft {
    /// Tool number, as text so the cell can be edited like any other.
    pub tool: String,
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
            tool: row.tool.to_string(),
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
    /// Scroll the table to the selected row on the next frame. Set only when the
    /// cursor moves by keyboard, so the view follows the cursor without fighting
    /// the operator's own scrolling.
    pub scroll_to_selected: bool,
    /// Row and anchor rect of the open TYPE selector, if any.
    pub kind_popup: Option<(usize, egui::Rect)>,
    /// Enter was pressed on a TYPE cell; the cell paints the selector with its
    /// own rect on the next frame.
    pub kind_popup_pending: bool,
    /// Highlighted choice in the open TYPE selector.
    pub kind_popup_index: usize,
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
            scroll_to_selected: false,
            kind_popup: None,
            kind_popup_pending: false,
            kind_popup_index: 0,
        }
    }
}

pub struct ProbeUi {
    pub category: ProbeCategory,
    pub operation: String,
    pub params: ProbeParameters,
    /// Display name of the controller's actual active WCS.
    pub wcs: String,
    pub error: Option<String>,
    pub last_run: Option<Instant>,
    /// Catalogue id issued by the most recent UI Run, including one still running.
    pub last_run_operation: Option<String>,
    /// Last controller calibration copied into the parameter block. This value is
    /// read-only in the UI and is never restored from preferences.
    pub calibration_offset_seen: Option<f64>,
    pub textures: HashMap<String, egui::TextureHandle>,
    /// Inline parameter editor. `param_selected` is the zero-based value index in
    /// the absolute #3014..=#3036 Probe Basic parameter contract.
    pub param_selected: usize,
    pub param_focused: bool,
    pub param_editing: bool,
    pub param_buffer: String,
    pub param_edit_focus: bool,
    /// Inline editor for the probe page DRO, shared shape with the Offsets DRO.
    pub dro_edit: Option<DroEdit>,
    pub help_topic: usize,
    pub help_page: usize,
}

impl ProbeUi {
    pub fn new(params: ProbeParameters, wcs: String) -> Self {
        let calibration_offset_seen = Some(params.calibration_offset);
        Self {
            category: ProbeCategory::OutsideCorners,
            operation: String::new(),
            params,
            wcs,
            error: None,
            last_run: None,
            last_run_operation: None,
            calibration_offset_seen,
            textures: HashMap::new(),
            param_selected: 1,
            param_focused: false,
            param_editing: false,
            param_buffer: String::new(),
            param_edit_focus: false,
            dro_edit: None,
            help_topic: 0,
            help_page: 0,
        }
    }

    pub fn operation(&self) -> Option<&'static ProbeOperation> {
        linc_core::probe::operation(&self.operation)
    }

    pub fn mdi(&self) -> Option<String> {
        self.operation()
            .map(|operation| self.params.mdi_text(operation))
    }

    pub fn texture(&mut self, ctx: &egui::Context, operation: &str) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.textures.get(operation) {
            return Some(texture.clone());
        }
        let bytes = crate::assets::probe_icon_bytes(&format!("{operation}.png"))?;
        let image = image::load_from_memory(bytes).ok()?.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        let texture = ctx.load_texture(
            format!("probe-icon-{operation}"),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(operation.to_string(), texture.clone());
        Some(texture)
    }
}

/// The MDI pane's working state.
///
/// It is keyed to the panel mode, not to a page, and lives for the app
/// session: leaving MDI keeps the buffer, and the only thing `Esc` clears is
/// the uncommitted input.
pub struct MdiUi {
    /// The block being typed, or one recalled from the buffer for editing.
    pub input: String,
    /// One-shot request for the input to take the keyboard on the next draw.
    pub focus: bool,
    /// One-shot request to put the text caret after the input. Recall replaces
    /// the text under the widget, whose own caret would otherwise stay where
    /// the previous block left it — at the start of the new one.
    pub caret_to_end: bool,
    /// The blocks committed this session, oldest first, in issue order.
    pub blocks: VecDeque<(String, u64)>,
    /// The buffer line the arrows, `Ctrl+R`, `Ctrl+Bksp` and `Ctrl+G` point at.
    pub selection: Option<usize>,
    /// The last block handed to the controller.
    pub last_result: Option<String>,
    /// Why the last issue or save did nothing; shown in the strip.
    pub last_error: Option<String>,
    /// The move the input block would make, cached: MAIN's toolpath draws it
    /// over the loaded program while the pane is up.
    pub overlay: Option<BlockOverlay>,
}

/// One block's move, parsed once per change of its inputs.
///
/// The toolpath repaints at the display rate, so the parse cannot run per
/// frame: "screens::mdi::overlay" keeps this and re-parses only when the input,
/// the machine position or the modal state behind it changes.
pub struct BlockOverlay {
    /// The trimmed block that was parsed.
    pub input: String,
    /// Where the block starts, in machine coordinates.
    pub start: [f64; 3],
    /// Fingerprint of the modal state and work offset it was parsed with.
    pub modal: u64,
    /// The parsed moves. Empty when the block is empty, too long or unparsable:
    /// the pane would refuse it, so the graph draws nothing for it.
    pub segments: Vec<linc_core::gcode::PathSegment>,
    /// Bumped on every recompute. The cache is otherwise invisible, and this is
    /// how a test holds the pane to one parse per change.
    pub generation: u64,
}

impl Default for MdiUi {
    fn default() -> Self {
        Self::new()
    }
}

impl MdiUi {
    /// The buffer restored from the preferences file, newest last, clamped to
    /// the profile's limit. The timestamps are not shown, so a restored block
    /// carries 0.
    pub fn restored(blocks: &[String], limit: usize) -> Self {
        let mut ui = Self::new();
        let keep = blocks.len().saturating_sub(limit.max(1));
        ui.blocks = blocks[keep..]
            .iter()
            .map(|block| (block.clone(), 0))
            .collect();
        ui
    }

    pub fn new() -> Self {
        Self {
            input: String::new(),
            // (restored above fills the buffer from the preferences file)
            // The pane asks for the keyboard the first time it appears.
            focus: true,
            caret_to_end: true,
            blocks: VecDeque::new(),
            selection: None,
            last_result: None,
            last_error: None,
            overlay: None,
        }
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The buffer line a key press acts on, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selection
            .and_then(|index| self.blocks.get(index))
            .map(|(block, _)| block.as_str())
    }

    /// The buffer as program lines, for `mdi_file::compose`.
    pub fn block_texts(&self) -> Vec<String> {
        self.blocks.iter().map(|(block, _)| block.clone()).collect()
    }

    /// Commit an issued block: a line recalled with `Up`/`Down` is
    /// overwritten in place, anything else appends. Past `limit` the oldest
    /// block goes, so the buffer stays a session-sized working list.
    pub fn commit(&mut self, limit: usize, block: String) {
        let at = linc_core::model::now_ms();
        match self.selection.filter(|index| *index < self.blocks.len()) {
            Some(index) => self.blocks[index] = (block, at),
            None => self.blocks.push_back((block, at)),
        }
        while self.blocks.len() > limit.max(1) {
            self.blocks.pop_front();
        }
        self.input.clear();
        self.selection = None;
        self.focus = true;
        self.caret_to_end = true;
    }

    /// `Up`: the block before the selected one, starting from the newest, and
    /// load it into the input. The buffer line itself stays put, so `Ctrl+R`
    /// still has a selected line to run while it is edited.
    pub fn recall_older(&mut self) -> bool {
        let Some(newest) = self.blocks.len().checked_sub(1) else {
            return false;
        };
        let index = self
            .selection
            .map_or(newest, |index| index.saturating_sub(1));
        self.select(index)
    }

    /// `Down`: the block after the selected one. Past the newest block the
    /// selection clears with the input, ready for a fresh block.
    pub fn recall_newer(&mut self) -> bool {
        let Some(index) = self.selection else {
            return false;
        };
        match index + 1 {
            next if next < self.blocks.len() => self.select(next),
            _ => {
                self.clear_input();
                true
            }
        }
    }

    /// `Ctrl+Bksp`: drop the selected buffer line. With no such line in the
    /// buffer yet it clears the uncommitted input instead.
    pub fn delete_selected(&mut self) -> Option<String> {
        let index = self.selection.filter(|index| *index < self.blocks.len());
        let Some(index) = index else {
            self.clear_input();
            return None;
        };
        let removed = self.blocks.remove(index).map(|(block, _)| block);
        // Keep the band on a line: the row that slid into this slot, else the
        // new last row.
        self.selection = if self.blocks.is_empty() {
            None
        } else {
            Some(index.min(self.blocks.len() - 1))
        };
        self.input.clear();
        self.last_error = None;
        self.caret_to_end = true;
        removed
    }

    /// `Ctrl+G`: select a buffer line by its 1-based number, bounded by the
    /// buffer, and load it for editing.
    pub fn goto(&mut self, line: usize) -> bool {
        if self.blocks.is_empty() {
            return false;
        }
        self.select(line.saturating_sub(1).min(self.blocks.len() - 1))
    }

    /// `Ctrl+Up` / `Ctrl+Down`: swap the selected block with its neighbour.
    ///
    /// The selection follows the moved block, so a second press keeps moving
    /// the same line. At the ends, and with no line selected, it does nothing:
    /// no wrap and no pop, and the input is left exactly as it is — the block
    /// being edited stays the block being edited.
    pub fn move_selected(&mut self, up: bool) -> bool {
        let Some(index) = self.selection.filter(|index| *index < self.blocks.len()) else {
            return false;
        };
        let target = if up {
            index.checked_sub(1)
        } else {
            (index + 1 < self.blocks.len()).then_some(index + 1)
        };
        let Some(target) = target else {
            return false;
        };
        self.blocks.swap(index, target);
        self.selection = Some(target);
        true
    }

    /// `Esc`: clear the uncommitted input and the pointer at a buffer line.
    /// Issued blocks are never cancelled.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.selection = None;
        self.last_error = None;
        self.focus = true;
        self.caret_to_end = true;
    }

    fn select(&mut self, index: usize) -> bool {
        let Some((block, _)) = self.blocks.get(index) else {
            return false;
        };
        self.input = block.clone();
        self.selection = Some(index);
        self.last_error = None;
        self.focus = true;
        self.caret_to_end = true;
        true
    }
}

pub struct ManualUi {
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub yaw: f32,
    pub pitch: f32,
    pub view: ViewMode,
    pub fit: bool,
    /// Keep the live toolpath framed until the operator pans/zooms.
    pub auto_fit: bool,
}

impl Default for ManualUi {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            yaw: linc_view::DEFAULT_YAW,
            pitch: linc_view::DEFAULT_PITCH,
            view: ViewMode::Iso3d,
            fit: true,
            auto_fit: true,
        }
    }
}

pub struct GraphStyle {
    pub show_grid: bool,
    pub show_live_trail: bool,
    /// REMAINING: no trail, and the completed part of the path is
    /// eaten. Remembered in the preferences file, not in the profile.
    pub remaining: bool,
    /// `Ctrl+B` cycle: 0 dimensions only, 1 + machine box, 2 + program box, 3 off.
    pub bounds_mode: u8,
    pub show_stock: bool,
    pub stock_alpha: u8,
    /// Lathe only: mirror X about the Z centreline of the turning view.
    pub lathe_mirror: bool,
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
        eaten_through: Option<usize>,
    ) {
        view.show_grid = self.show_grid;
        // REMAINING draws no trail at all: where the machine has been is the hole
        // it ate, and a green line over that hole would put the trail back.
        view.show_live_trail = self.show_live_trail && !self.remaining;
        view.remaining = self.remaining;
        view.eaten_through = eaten_through;
        let (dims, machine, box_at) = bounds_visibility(self.bounds_mode);
        view.show_program_bounds = dims;
        view.show_machine_bounds = machine;
        view.show_program_box = box_at;
        view.show_stock = self.show_stock;
        view.stock_alpha = self.stock_alpha;
        view.mirror_x = self.lathe_mirror;
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

pub fn page_accent(page: Page) -> egui::Color32 {
    match page {
        Page::Machine | Page::Program => theme::ACCENT,
        Page::Offsets | Page::Tools => theme::ACCENT,
        Page::Diagnostics => theme::C1,
        Page::Help => theme::C2,
        Page::Files => theme::C4,
        Page::Probe => theme::C6,
        Page::Homing | Page::Manual => theme::C3,
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
        remaining: false,
        bounds_mode: bounds_mode_from_config(
            profile.config.toolpath.show_program_bounds,
            profile.config.toolpath.show_machine_bounds,
        ),
        show_stock: profile.config.toolpath.show_stock,
        stock_alpha: profile.config.toolpath.stock_alpha,
        // Remembered per operator in the preferences file, not in the profile.
        lathe_mirror: false,
        highlight_selected_line: profile.config.toolpath.highlight_selected_line,
        // SPEC §6.2: the legend must identify the loaded program path and the
        // live trail as separate layers. It was hardcoded off, so the two
        // colours were never keyed anywhere in the UI.
        show_legend: true,
        trail_max_points: profile.config.toolpath.trail_max_points,
        perspective: profile.config.toolpath.perspective,
    }
}
/// How far the live trail must move to be drawn in the frame of the loaded
/// program's path.
///
/// The path is parsed at `program words + the work offset the program selects`,
/// so it is machine space. The live trail is the machine's own tool-tip
/// position, which the interpreter computes as
/// `work + work offset + G92 + tool length` — and the tool length `G43` applies
/// is the one term the preview has no way to know. The two frames therefore
/// differ by `preview offset + work - machine`, which is zero on a machine with
/// no tool length and no G92 in play.
///
/// The simulator tracks both frames (its motion and jog paths apply the active
/// work system), so the same arithmetic holds there and resolves to zero unless
/// a tool length is in play.
pub fn live_trail_shift(
    snapshot: &MachineSnapshot,
    preview_offset: Option<&linc_core::model::AxisMap<f64>>,
) -> [f64; 3] {
    ["X", "Y", "Z"].map(|axis| {
        let machine = snapshot
            .positions
            .axes
            .get(axis)
            .map(|position| position.machine)
            .unwrap_or(0.0);
        let work = snapshot
            .positions
            .axes
            .get(axis)
            .map(|position| position.work)
            .unwrap_or(0.0);
        let offset = preview_offset
            .and_then(|map| map.get(axis).copied())
            .unwrap_or(0.0);
        offset + work - machine
    })
}

/// What the `Ctrl+B` cycle shows: `(dimensions, machine box, program box)`.
pub fn bounds_visibility(mode: u8) -> (bool, bool, bool) {
    match mode % 4 {
        0 => (true, false, false),
        1 => (true, true, false),
        2 => (true, true, true),
        _ => (false, false, false),
    }
}

/// The cycle position a profile's toolpath flags imply, for a first run.
pub fn bounds_mode_from_config(show_program: bool, show_machine: bool) -> u8 {
    if !show_program && !show_machine {
        3
    } else if show_machine {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::{bounds_mode_from_config, bounds_visibility};

    #[test]
    fn the_cycle_goes_dimensions_machine_box_off() {
        assert_eq!(bounds_visibility(0), (true, false, false));
        assert_eq!(bounds_visibility(1), (true, true, false));
        assert_eq!(bounds_visibility(2), (true, true, true));
        assert_eq!(bounds_visibility(3), (false, false, false));
        assert_eq!(bounds_visibility(4), bounds_visibility(0), "wraps");
    }

    #[test]
    fn a_profile_flags_imply_a_first_run_mode() {
        assert_eq!(bounds_mode_from_config(false, false), 3);
        assert_eq!(bounds_mode_from_config(true, false), 0);
        assert_eq!(bounds_mode_from_config(true, true), 2);
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
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Equals => "=",
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
        "+" | "PLUS" => Key::Plus,
        "-" | "MINUS" => Key::Minus,
        "=" | "EQUALS" => Key::Equals,
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
    /// Pages with a toolpath: cycle the view ISO, X, Y, Z.
    pub view_cycle: Binding,
    /// Pages with a toolpath: show or hide the program bounding box.
    pub toggle_bounds: Binding,
    /// Pages with a toolpath: REMAINING. The live trail is not drawn and
    /// the program path is eaten block by completed block, so the card shows
    /// what is still to come instead of where the machine has been.
    pub remaining: Binding,
    /// Lathe only: mirror X about the Z centreline of the XZ/TURN view. The
    /// projection flips, so the part, the trail, the tool and the MDI overlay
    /// move together; no coordinate changes.
    pub toggle_lathe_mirror: Binding,
    /// Lathe only: read the X position as a radius (`X`) or a diameter (`2X`).
    pub toggle_lathe_diameter: Binding,
    /// Page-local to Program: fit the toolpath and clear the live trail.
    pub graph_fit: Binding,
    pub graph_clear: Binding,
    /// Pages with a toolpath: pan the view while the pointer is over it.
    pub view_pan_left: Binding,
    pub view_pan_right: Binding,
    pub view_pan_up: Binding,
    pub view_pan_down: Binding,
    /// Pages with a toolpath: zoom the view while the pointer is over it.
    pub view_zoom_in: Binding,
    pub view_zoom_out: Binding,
    /// Page-local to Program: ask for a line number and jump there.
    pub goto_line: Binding,
    /// Page-local to Program: start the loaded program at the current line.
    pub program_run_from_line: Binding,
    pub files_delete: Binding,
    pub tools: Binding,
    pub offsets: Binding,
    /// Toggle the limit-switch override. The jog screens carry no button for it.
    pub limits_override: Binding,
    pub diagnostics: Binding,
    pub help: Binding,
    /// Any page: open the full-screen messages dialog.
    pub messages: Binding,
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
    /// Page-local to the MDI pane: clear the uncommitted input. It shares
    /// Escape with `back` by default, and the pane leaves that chord to the
    /// shell then: Esc still resets a running program first.
    pub mdi_clear: Binding,
    pub mdi_history_up: Binding,
    pub mdi_history_down: Binding,
    /// Page-local to the MDI pane: swap the selected buffer block with its
    /// neighbour. Ctrl by default — the panel keyboard has no Shift key.
    pub mdi_line_up: Binding,
    pub mdi_line_down: Binding,
    /// Page-local to the MDI pane: drop the selected buffer line.
    pub mdi_buffer_delete: Binding,
    pub help_search: Binding,
    /// Bindings from the profile that could not be parsed. The default holds for
    /// each of them and `from_config` logs it; the list is what the tests read.
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

/// A buffer line-move binding, held to a chord that cannot be taken for recall.
///
/// `Up`/`Down` already recall a buffer line and the shell navigates with the
/// bare arrows, so a profile that rebinds the move onto one of them would
/// shadow recall with no sign of it. The chord is rejected the way an
/// unparsable one is: the default holds and the log names it.
fn line_move_binding(value: &str, default: Binding, rejected: &mut Vec<String>) -> Binding {
    let binding = binding(value, default, rejected);
    if binding.modifiers.is_none()
        && matches!(binding.key, egui::Key::ArrowUp | egui::Key::ArrowDown)
    {
        rejected.push(value.to_string());
        return default;
    }
    binding
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
            view_cycle: binding(
                &keys.view_cycle,
                Binding::new(Key::F10, ctrl),
                &mut rejected,
            ),
            toggle_bounds: binding(
                &keys.toggle_bounds,
                Binding::new(Key::B, ctrl),
                &mut rejected,
            ),
            remaining: binding(&keys.remaining, Binding::new(Key::M, ctrl), &mut rejected),
            toggle_lathe_mirror: binding(
                &keys.toggle_lathe_mirror,
                Binding::new(Key::J, ctrl),
                &mut rejected,
            ),
            toggle_lathe_diameter: binding(
                &keys.toggle_lathe_diameter,
                Binding::new(Key::E, ctrl),
                &mut rejected,
            ),
            program_cycle_view: binding(
                &keys.program_cycle_view,
                Binding::new(Key::F10, none),
                &mut rejected,
            ),
            graph_fit: binding(
                &keys.graph_fit,
                Binding::new(Key::Num0, ctrl),
                &mut rejected,
            ),
            graph_clear: binding(&keys.graph_clear, Binding::new(Key::L, ctrl), &mut rejected),
            view_pan_left: binding(
                &keys.view_pan_left,
                Binding::new(Key::ArrowLeft, ctrl),
                &mut rejected,
            ),
            view_pan_right: binding(
                &keys.view_pan_right,
                Binding::new(Key::ArrowRight, ctrl),
                &mut rejected,
            ),
            view_pan_up: binding(
                &keys.view_pan_up,
                Binding::new(Key::ArrowUp, ctrl),
                &mut rejected,
            ),
            view_pan_down: binding(
                &keys.view_pan_down,
                Binding::new(Key::ArrowDown, ctrl),
                &mut rejected,
            ),
            view_zoom_in: binding(
                &keys.view_zoom_in,
                Binding::new(Key::Plus, ctrl),
                &mut rejected,
            ),
            view_zoom_out: binding(
                &keys.view_zoom_out,
                Binding::new(Key::Minus, ctrl),
                &mut rejected,
            ),
            goto_line: binding(&keys.goto_line, Binding::new(Key::G, ctrl), &mut rejected),
            program_run_from_line: binding(
                &keys.program_run_from_line,
                Binding::new(Key::R, ctrl),
                &mut rejected,
            ),
            files_delete: binding(
                &keys.files_delete,
                Binding::new(Key::Delete, none),
                &mut rejected,
            ),
            tools: binding(&keys.tools, Binding::new(Key::F7, none), &mut rejected),
            offsets: binding(&keys.offsets, Binding::new(Key::F8, none), &mut rejected),
            limits_override: binding(
                &keys.limits_override,
                Binding::new(
                    Key::O,
                    Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                ),
                &mut rejected,
            ),
            diagnostics: binding(&keys.diagnostics, Binding::new(Key::D, ctrl), &mut rejected),
            help: binding(&keys.help, Binding::new(Key::H, ctrl), &mut rejected),
            messages: binding(&keys.messages, Binding::new(Key::M, none), &mut rejected),
            back: binding(&keys.back, Binding::new(Key::Escape, none), &mut rejected),
            quit: binding(&keys.quit, Binding::new(Key::Q, ctrl), &mut rejected),
            activate: binding(
                &keys.activate,
                Binding::new(Key::Enter, none),
                &mut rejected,
            ),
            open: binding(&keys.open, Binding::new(Key::O, ctrl), &mut rejected),
            save: binding(&keys.save, Binding::new(Key::S, ctrl), &mut rejected),
            undo: binding(&keys.undo, Binding::new(Key::Z, ctrl), &mut rejected),
            redo: binding(&keys.redo, Binding::new(Key::Y, ctrl), &mut rejected),
            find: binding(&keys.find, Binding::new(Key::F, ctrl), &mut rejected),
            refresh: binding(&keys.refresh, Binding::new(Key::R, ctrl), &mut rejected),
            mdi_issue: binding(
                &keys.mdi_issue,
                Binding::new(Key::Enter, none),
                &mut rejected,
            ),
            mdi_clear: binding(
                &keys.mdi_clear,
                Binding::new(Key::Escape, none),
                &mut rejected,
            ),
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
            mdi_line_up: line_move_binding(
                &keys.mdi_line_up,
                Binding::new(Key::ArrowUp, ctrl),
                &mut rejected,
            ),
            mdi_line_down: line_move_binding(
                &keys.mdi_line_down,
                Binding::new(Key::ArrowDown, ctrl),
                &mut rejected,
            ),
            mdi_buffer_delete: binding(
                &keys.mdi_buffer_delete,
                Binding::new(Key::Backspace, ctrl),
                &mut rejected,
            ),
            help_search: binding(
                &keys.help_search,
                Binding::new(Key::Slash, none),
                &mut rejected,
            ),
            rejected,
        };
        // Nothing on screen shows these any more (SETTINGS is gone), and a key
        // that silently does not work is a mystery at the machine: say it.
        for chord in &keymap.rejected {
            log::warn!(
                "[keys] {chord:?} is not a chord this shell understands; its default holds"
            );
        }
        keymap
    }

    /// `Ctrl+Arrows` for the toolpath hint row, built from the configured pan
    /// binding: a profile that rebinds the pan shows the new modifier.
    pub fn view_pan_hint(&self) -> String {
        let label = self.view_pan_left.label();
        let key = key_name(self.view_pan_left.key);
        format!("{}Arrows", label.strip_suffix(key).unwrap_or(""))
    }

    /// The two zoom keys under one modifier, e.g. `Ctrl+-/+`.
    pub fn view_zoom_hint(&self) -> String {
        let out = self.view_zoom_out.label();
        let out_key = key_name(self.view_zoom_out.key);
        let in_key = key_name(self.view_zoom_in.key);
        format!(
            "{}{}/{}",
            out.strip_suffix(out_key).unwrap_or(""),
            out_key,
            in_key
        )
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
/// Jog speed from the ceiling and a percentage.
///
/// The percentage can exceed 100: the feed override scales it, and on this
/// machine an override of 150 % is reachable. Negative percentages clamp to
/// zero, because a jog cannot run backwards.
pub fn jog_speed(ceiling_per_second: f64, percent: f64) -> f64 {
    (ceiling_per_second * percent.clamp(0.0, 150.0) / 100.0).max(0.0)
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
        assert_eq!(jog_speed(60.0, 150.0), 90.0);
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
        // The view zoom chords: both the named form and the bare symbol.
        let ctrl_plus = Binding::parse("Ctrl+Plus").expect("ctrl+plus");
        assert_eq!(ctrl_plus.key, egui::Key::Plus);
        assert!(ctrl_plus.modifiers.ctrl);
        assert_eq!(Binding::parse("Ctrl++"), Some(ctrl_plus));
        assert_eq!(
            Binding::parse("Ctrl+-"),
            Binding::parse("Ctrl+Minus"),
            "the symbol form is the named form"
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
    fn expressions_evaluate_and_reject_nonsense() {
        assert_eq!(eval_expression("10"), Some(10.0));
        assert_eq!(eval_expression("-25.15"), Some(-25.15));
        assert_eq!(eval_expression("10+2.5"), Some(12.5));
        assert_eq!(eval_expression("-25.15/2"), Some(-12.575));
        assert_eq!(eval_expression("(1+2)*3"), Some(9.0));
        assert_eq!(eval_expression("2*-3"), Some(-6.0));
        assert_eq!(eval_expression(" 4 - 6 "), Some(-2.0));
        // Division by zero, an unbalanced bracket and a trailing operator are
        // all rejected rather than producing a value.
        assert_eq!(eval_expression("1/0"), None);
        assert_eq!(eval_expression("(1+2"), None);
        assert_eq!(eval_expression("1+"), None);
        assert_eq!(eval_expression("abc"), None);
    }

    #[test]
    fn the_view_chords_default_to_ctrl_arrows_and_plus_minus() {
        let keymap = Keymap::from_config(&linc_core::config::KeysConfig::spec_default());
        assert!(keymap.rejected.is_empty());
        assert_eq!(keymap.view_pan_left.key, egui::Key::ArrowLeft);
        assert_eq!(keymap.view_pan_right.key, egui::Key::ArrowRight);
        assert_eq!(keymap.view_pan_up.key, egui::Key::ArrowUp);
        assert_eq!(keymap.view_pan_down.key, egui::Key::ArrowDown);
        for pan in [
            &keymap.view_pan_left,
            &keymap.view_pan_right,
            &keymap.view_pan_up,
            &keymap.view_pan_down,
        ] {
            assert!(pan.modifiers.ctrl);
        }
        assert_eq!(keymap.view_zoom_in.key, egui::Key::Plus);
        assert_eq!(keymap.view_zoom_out.key, egui::Key::Minus);
        assert_eq!(keymap.view_pan_hint(), "Ctrl+Arrows");
        assert_eq!(keymap.view_zoom_hint(), "Ctrl+-/+");
    }

    #[test]
    fn an_unparseable_binding_falls_back_and_is_reported() {
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.machine = "Hyper+Z".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.machine.key, egui::Key::F5);
        assert_eq!(keymap.rejected, vec!["Hyper+Z".to_string()]);
    }

    #[test]
    fn the_mdi_keys_survive_the_parser_and_a_bad_delete_chord_is_reported() {
        // The shipped default must parse: a rejected default would leave the
        // pane's delete key bound to nothing while Settings showed the text.
        let keys = linc_core::config::KeysConfig::spec_default();
        let keymap = Keymap::from_config(&keys);
        assert!(keymap.rejected.is_empty(), "{:?}", keymap.rejected);
        assert_eq!(keymap.mdi_issue.key, egui::Key::Enter);
        assert!(keymap.mdi_issue.modifiers.is_none());
        assert_eq!(keymap.mdi_clear.key, egui::Key::Escape);
        assert_eq!(keymap.mdi_history_up.key, egui::Key::ArrowUp);
        assert_eq!(keymap.mdi_history_down.key, egui::Key::ArrowDown);
        assert_eq!(keymap.mdi_buffer_delete.key, egui::Key::Backspace);
        assert!(keymap.mdi_buffer_delete.modifiers.ctrl);
        // Ctrl, not Shift: the panel keyboard has no Shift key. Plain Up/Down
        // stay with recall, so the move must keep its modifier.
        assert_eq!(keymap.mdi_line_up.key, egui::Key::ArrowUp);
        assert!(keymap.mdi_line_up.modifiers.ctrl);
        assert_eq!(keymap.mdi_line_down.key, egui::Key::ArrowDown);
        assert!(keymap.mdi_line_down.modifiers.ctrl);
        assert_ne!(keymap.mdi_line_up, keymap.mdi_history_up);
        assert_ne!(keymap.mdi_line_down, keymap.mdi_history_down);

        // An unparseable chord is rejected like every other binding: the
        // default stays and the profile's text is reported.
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.mdi_buffer_delete = "Ctrl+NoSuchKey".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.mdi_buffer_delete.key, egui::Key::Backspace);
        assert!(keymap.mdi_buffer_delete.modifiers.ctrl);
        assert_eq!(keymap.rejected, vec!["Ctrl+NoSuchKey".to_string()]);

        // A line move rebound onto a bare arrow would shadow recall: it is
        // rejected the same way, and the default holds.
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.mdi_line_up = "Up".to_string();
        keys.mdi_line_down = "Down".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.mdi_line_up.key, egui::Key::ArrowUp);
        assert!(keymap.mdi_line_up.modifiers.ctrl, "the default chord holds");
        assert!(keymap.mdi_line_down.modifiers.ctrl);
        assert_eq!(keymap.rejected, vec!["Up".to_string(), "Down".to_string()]);
    }

    #[test]
    fn moving_a_buffer_line_swaps_the_neighbours_and_carries_the_selection() {
        let mut mdi = MdiUi::new();
        for block in ["G0 X1", "G1 X2", "G1 X3"] {
            mdi.commit(8, block.to_string());
        }
        // No selection: nothing moves, nothing is issued.
        assert!(!mdi.move_selected(true));
        assert!(!mdi.move_selected(false));
        assert_eq!(mdi.block_texts(), ["G0 X1", "G1 X2", "G1 X3"]);

        mdi.recall_older();
        assert_eq!(mdi.input, "G1 X3");
        assert!(mdi.move_selected(true));
        assert_eq!(mdi.block_texts(), ["G0 X1", "G1 X3", "G1 X2"]);
        assert_eq!(mdi.selection, Some(1), "the selection follows the block");
        assert_eq!(mdi.input, "G1 X3", "the move leaves the input alone");
        assert_eq!(mdi.selected(), Some("G1 X3"));

        // At the top it stops: no wrap, no pop.
        assert!(mdi.move_selected(true));
        assert_eq!(mdi.block_texts(), ["G1 X3", "G0 X1", "G1 X2"]);
        assert_eq!(mdi.selection, Some(0));
        assert!(!mdi.move_selected(true));
        assert_eq!(mdi.selection, Some(0));
        assert_eq!(mdi.len(), 3);

        // And at the bottom.
        assert!(mdi.move_selected(false));
        assert!(mdi.move_selected(false));
        assert_eq!(mdi.block_texts(), ["G0 X1", "G1 X2", "G1 X3"]);
        assert_eq!(mdi.selection, Some(2));
        assert!(!mdi.move_selected(false));
        assert_eq!(mdi.selection, Some(2));
        assert_eq!(mdi.len(), 3);
    }
}

#[cfg(test)]
mod mdi_ui_tests {
    use super::MdiUi;

    /// The buffer comes back in the order it was issued, oldest first, and the
    /// profile's limit still applies to a restored one.
    #[test]
    fn the_buffer_is_restored_in_order_and_clamped() {
        assert!(MdiUi::restored(&[], 20).blocks.is_empty());
        let blocks: Vec<String> = ["G0 X0".to_string(), "M19 R0".to_string()]
            .into_iter()
            .collect::<Vec<_>>();
        let ui = MdiUi::restored(&blocks, 20);
        assert_eq!(ui.block_texts(), blocks);

        let long: Vec<String> = (0..5).map(|index| format!("G0 X{index}")).collect();
        let ui = MdiUi::restored(&long, 2);
        assert_eq!(ui.block_texts(), vec!["G0 X3".to_string(), "G0 X4".to_string()]);
    }
}

#[cfg(test)]
mod help_dialog_tests {
    use super::HelpUi;

    #[test]
    fn the_help_dialog_walks_back_and_stops() {
        let mut help = HelpUi::default();
        help.open_dialog("gcode:g0");
        assert_eq!(
            help.dialog.as_ref().map(|dialog| dialog.anchor.as_str()),
            Some("gcode:g0")
        );
        help.dialog_link("gcode:g90");
        assert_eq!(help.dialog.as_ref().unwrap().position, 1);
        assert!(help.dialog_back());
        assert_eq!(
            help.dialog.as_ref().map(|dialog| dialog.anchor.as_str()),
            Some("gcode:g0")
        );
        assert!(!help.dialog_back());
        help.close_dialog();
        assert!(help.dialog.is_none());
    }
}

#[cfg(test)]
mod probe_ui_tests {
    use super::ProbeUi;
    use linc_core::probe::{ProbeCategory, ProbeMode, ProbeParameters};

    #[test]
    fn probe_ui_starts_without_a_routine_and_with_empty_help_position() {
        let ui = ProbeUi::new(ProbeParameters::default(), "G54".into());
        assert_eq!(ui.category, ProbeCategory::OutsideCorners);
        assert!(ui.operation.is_empty());
        assert_eq!(ui.params.probe_mode, ProbeMode::MeasureOnly);
        assert_eq!(ui.help_topic, 0);
        assert_eq!(ui.help_page, 0);
        assert_eq!(ui.param_selected, 1);
        assert_eq!(ui.calibration_offset_seen, Some(0.0));
    }
}

#[cfg(test)]
mod page_key_tests {
    use super::Keymap;

    #[test]
    fn the_lathe_display_toggles_resolve_to_ctrl_j_and_ctrl_e() {
        let keymap = Keymap::from_config(&linc_core::config::KeysConfig::spec_default());
        assert!(keymap.rejected.is_empty(), "{:?}", keymap.rejected);
        assert_eq!(keymap.toggle_lathe_mirror.key, egui::Key::J);
        assert!(keymap.toggle_lathe_mirror.modifiers.ctrl);
        assert_eq!(keymap.toggle_lathe_diameter.key, egui::Key::E);
        assert!(keymap.toggle_lathe_diameter.modifiers.ctrl);
        // Both chords must be free: Ctrl+D is diagnostics, Ctrl+R run-from-line,
        // and no two bindings may share one chord.
        for (label, binding) in [
            ("mirror", keymap.toggle_lathe_mirror),
            ("diameter", keymap.toggle_lathe_diameter),
        ] {
            let clashes = [
                ("bounds", keymap.toggle_bounds),
                ("view", keymap.view_cycle),
                ("diagnostics", keymap.diagnostics),
                ("run-line", keymap.program_run_from_line),
                ("goto", keymap.goto_line),
                ("messages", keymap.messages),
            ];
            for (other, candidate) in clashes {
                assert!(
                    binding != candidate,
                    "{label} shares its chord with {other}"
                );
            }
        }
        // A profile can rebind them like any other key.
        let mut keys = linc_core::config::KeysConfig::spec_default();
        keys.toggle_lathe_mirror = "Ctrl+F11".to_string();
        let keymap = Keymap::from_config(&keys);
        assert_eq!(keymap.toggle_lathe_mirror.key, egui::Key::F11);
    }

    #[test]
    fn diagnostics_and_help_use_ctrl_d_and_ctrl_h() {
        let keymap = Keymap::from_config(&linc_core::config::KeysConfig::spec_default());
        assert_eq!(keymap.diagnostics.key, egui::Key::D);
        assert!(keymap.diagnostics.modifiers.ctrl);
        assert_eq!(keymap.help.key, egui::Key::H);
        assert!(keymap.help.modifiers.ctrl);
    }
}

#[cfg(test)]
mod trail_frame_tests {
    use super::live_trail_shift;
    use linc_core::model::{AxisMap, MachineSnapshot, MachineVariant, PositionSet};

    const AXES: [&str; 3] = ["X", "Y", "Z"];

    fn place(snapshot: &mut MachineSnapshot, axis: &str, work: f64, machine: f64) {
        snapshot.positions.axes.insert(
            axis.to_string(),
            PositionSet {
                work,
                machine,
                dtg: 0.0,
                velocity: 0.0,
            },
        );
    }

    /// The work offset of the G54 fixture: where the work zero sits on the
    /// machine.
    fn g54() -> AxisMap<f64> {
        AxisMap::from_entries([
            ("X".to_string(), -120.0),
            ("Y".to_string(), -80.0),
            ("Z".to_string(), 45.0),
        ])
    }

    #[test]
    fn the_tool_length_g43_applies_is_taken_back_out_of_the_trail() {
        // T5 of the machine tool table, which Engraving.nc selects with G43 H5.
        let tool = [0.0, 0.0, -67.128];
        // The cut is at program X16.864 Y3.422 Z-0.1. The machine's tool-tip
        // position carries the work offset and the tool length; the DRO's work
        // value, which the program path is parsed against, does not.
        let program = [16.864, 3.422, -0.1];
        let work = g54();
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        for (index, axis) in AXES.iter().enumerate() {
            let offset = work.get(axis).copied().unwrap_or(0.0);
            place(
                &mut snapshot,
                axis,
                program[index],
                program[index] + offset + tool[index],
            );
        }
        let shift = live_trail_shift(&snapshot, Some(&work));
        for index in 0..3 {
            assert!(
                (shift[index] + tool[index]).abs() < 1e-9,
                "axis {index}: shift {shift:?} must undo the tool length {tool:?}"
            );
            // The shifted trail lands on the machine point the program path is
            // drawn at: `program + the offset the preview was parsed with`.
            let drawn = snapshot.positions.axes.get(AXES[index]).unwrap().machine + shift[index];
            let path = program[index] + work.get(AXES[index]).copied().unwrap();
            assert!(
                (drawn - path).abs() < 1e-9,
                "axis {index}: {drawn} vs {path}"
            );
        }
    }

    #[test]
    fn a_machine_without_tool_length_or_g92_is_not_shifted() {
        let work = g54();
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        for axis in AXES {
            let offset = work.get(axis).copied().unwrap_or(0.0);
            // work = machine - offset, with no tool length and no G92.
            place(&mut snapshot, axis, -offset, 0.0);
        }
        assert_eq!(live_trail_shift(&snapshot, Some(&work)), [0.0; 3]);
    }

    #[test]
    fn the_simulator_is_left_as_it_is() {
        let work = g54();
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.simulated = true;
        for axis in AXES {
            // The simulator applies the work system to both fields, so its trail
            // and the preview already share a frame and no correction is due.
            let (offset, work_value) = (work.get(axis).copied().unwrap_or(0.0), 1.0);
            place(&mut snapshot, axis, work_value, offset + work_value);
        }
        assert_eq!(live_trail_shift(&snapshot, Some(&work)), [0.0; 3]);
    }
}
