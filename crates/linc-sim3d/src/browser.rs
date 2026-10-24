//! The file picker and the recent-setup list.
//!
//! The 3D window has to work headless, under Xvfb, in the same runs that
//! import STEP files for the test suite, so the picker is drawn with egui
//! rather than handed to a desktop portal: no extra dependency, no portal
//! service to be missing, and one code path in a capture and on the desktop.
//!
//! The listing, the extension filter and the recent store are pure functions
//! over paths, so the tests exercise them without a window.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const BROWSER_ID: &str = "linc-sim3d-browser";
const LIST_WIDTH: f32 = 560.0;
/// How many rows the list keeps visible. The modal never shrinks below this,
/// so a listing never collapses to one or two lines.
pub const VISIBLE_ROWS: usize = 10;

/// The height ten rows need, from the row height and the gap between rows.
pub fn list_extent(row_height: f32, spacing: f32) -> f32 {
    row_height * VISIBLE_ROWS as f32 + spacing * (VISIBLE_ROWS as f32 - 1.0)
}

/// What the picker is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A mesh to import as a workpiece.
    Step,
    /// A setup file: the config a workpiece list is loaded from and saved to.
    Setup,
}

impl Purpose {
    /// The extensions the listing keeps, lowercase.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Step => &["step", "stp", "stl"],
            Self::Setup => &["toml"],
        }
    }

    /// The window heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Step => "Import STEP file",
            Self::Setup => "Open setup file",
        }
    }

    /// The word on the accept button.
    pub fn accept(self) -> &'static str {
        match self {
            Self::Step => "Import",
            Self::Setup => "Open",
        }
    }

    /// One line naming what the picker takes.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Step => "STEP and STL meshes",
            Self::Setup => "simulator config files",
        }
    }
}

/// One row of the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// True when the path carries one of the purpose's extensions, ignoring case.
pub fn extension_matches(path: &Path, purpose: Purpose) -> bool {
    let Some(extension) = path.extension().map(|value| value.to_string_lossy()) else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    purpose
        .extensions()
        .iter()
        .any(|candidate| *candidate == extension)
}

/// Read one directory: every subdirectory, and the files the purpose accepts.
///
/// Directories come first, then files, each sorted case-insensitively by name.
/// Dot entries are skipped unless show_hidden asks for them. An entry that
/// cannot be read is skipped rather than failing the whole listing.
pub fn list_directory(
    dir: &Path,
    purpose: Purpose,
    show_hidden: bool,
) -> Result<Vec<Entry>, String> {
    let read =
        fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut entries = Vec::new();
    for item in read {
        let Ok(item) = item else { continue };
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = item.path();
        let is_dir = item.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if !is_dir && !extension_matches(&path, purpose) {
            continue;
        }
        entries.push(Entry { path, name, is_dir });
    }
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

/// The directory above, or None at the filesystem root and for a relative path
/// with no parent left to strip.
pub fn parent_of(dir: &Path) -> Option<PathBuf> {
    dir.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// The home directory, when HOME names one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Expand a typed path: a leading tilde is the home directory, a relative path
/// is taken from the browser's directory, and an empty string is no path.
pub fn resolve_typed(text: &str, dir: &Path) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let expanded = expand_path(text);
    Some(if expanded.is_absolute() {
        expanded
    } else {
        dir.join(expanded)
    })
}

/// Expand a leading tilde against HOME; everything else is used as typed.
pub fn expand_path(text: &str) -> PathBuf {
    let text = text.trim();
    if text == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(text)
}

/// The picker: a directory listing plus a typed-path escape hatch.
///
/// The browser stays open across frames. show() draws it and returns the file
/// the operator picked, at which point the caller acts and drops the browser.
#[derive(Debug, Clone)]
pub struct Browser {
    purpose: Purpose,
    dir: PathBuf,
    entries: Vec<Entry>,
    error: Option<String>,
    typed: String,
    show_hidden: bool,
    open: bool,
}

impl Browser {
    /// A browser over one directory, listed immediately.
    pub fn new(purpose: Purpose, dir: PathBuf) -> Self {
        let mut browser = Self {
            purpose,
            dir,
            entries: Vec::new(),
            error: None,
            typed: String::new(),
            show_hidden: false,
            open: true,
        };
        browser.refresh();
        browser
    }

    /// What the browser is picking.
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// The directory on show.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// True while the modal is up; the caller uses this to keep Escape from
    /// closing the window instead of the picker.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The rows of the current listing, for the panel and the tests.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Move to another directory and re-list it.
    pub fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.refresh();
    }

    fn refresh(&mut self) {
        match list_directory(&self.dir, self.purpose, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.error = Some(error);
            }
        }
    }

    /// Draw the modal for this frame; Some(path) is a pick.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        let mut picked: Option<PathBuf> = None;
        let mut next_dir: Option<PathBuf> = None;
        let mut relist = false;
        let mut close = false;

        let modal = egui::Modal::new(egui::Id::unique(BROWSER_ID)).show(ctx, |ui| {
            ui.set_width(LIST_WIDTH);
            ui.heading(self.purpose.title());
            ui.label(egui::RichText::new(self.purpose.hint()).small().weak());

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.dir != Path::new("/"), egui::Button::new("up"))
                    .clicked()
                {
                    next_dir = parent_of(&self.dir);
                }
                if ui.button("home").clicked() {
                    next_dir = home_dir();
                }
                ui.monospace(self.dir.display().to_string());
            });

            ui.horizontal(|ui| {
                ui.label("path");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.typed)
                        .desired_width(LIST_WIDTH - 130.0)
                        .hint_text("~/parts/block.step"),
                );
                let submitted =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if ui.button("go").clicked() || submitted {
                    picked = resolve_typed(&self.typed, &self.dir);
                }
            });

            // Ten rows are always on show: the list reserves their height and
            // the modal is sized by its content, so the picker never collapses
            // to a couple of lines.
            let row_height =
                ui.text_style_height(&egui::TextStyle::Body) + 2.0 * ui.spacing().button_padding.y;
            let min_list = list_extent(row_height, ui.spacing().item_spacing.y);
            let max_list = (ctx.content_rect().height() * 0.6).max(min_list);

            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(LIST_WIDTH - 20.0);
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(0xe6, 0x5c, 0x4c), error);
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .min_scrolled_height(min_list)
                    .max_height(max_list)
                    .id_salt("linc-sim3d-browser-list")
                    .show(ui, |ui| {
                        if self.entries.is_empty() && self.error.is_none() {
                            ui.label("nothing here the picker takes");
                        }
                        // A row is as wide as the list and its name sits at the
                        // left edge, so the whole line is a hit target and long
                        // names read like a file listing.
                        let row = egui::vec2(ui.available_width(), row_height);
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        for entry in &self.entries {
                            let label = if entry.is_dir {
                                format!("{}/", entry.name)
                            } else {
                                entry.name.clone()
                            };
                            let (rect, response) =
                                ui.allocate_exact_size(row, egui::Sense::click());
                            if ui.is_rect_visible(rect) {
                                if response.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        2.0,
                                        ui.visuals().widgets.hovered.weak_bg_fill,
                                    );
                                }
                                let color = if entry.is_dir {
                                    ui.visuals().hyperlink_color
                                } else {
                                    ui.visuals().text_color()
                                };
                                ui.painter().with_clip_rect(rect).text(
                                    rect.left_center() + egui::vec2(6.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    &label,
                                    font.clone(),
                                    color,
                                );
                            }
                            let response = response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(entry.path.display().to_string());
                            if response.clicked() {
                                if entry.is_dir {
                                    next_dir = Some(entry.path.clone());
                                } else {
                                    picked = Some(entry.path.clone());
                                }
                            }
                        }
                    });
            });

            ui.horizontal(|ui| {
                let mut show_hidden = self.show_hidden;
                if ui.checkbox(&mut show_hidden, "hidden").changed() {
                    self.show_hidden = show_hidden;
                    relist = true;
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{}: {}",
                        self.purpose.accept(),
                        self.purpose.hint()
                    ))
                    .small()
                    .weak(),
                );
                if ui.button("cancel").clicked() {
                    close = true;
                }
            });
        });

        if modal.backdrop_response.clicked()
            || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            close = true;
        }

        if relist {
            let dir = self.dir.clone();
            next_dir = Some(dir);
        }
        if let Some(dir) = next_dir {
            self.set_dir(dir);
        }
        if picked.is_some() {
            close = true;
        }
        if close {
            self.open = false;
        }
        picked
    }
}

/// The recent setup files and the directories the pickers last used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recents {
    pub setups: Vec<PathBuf>,
    /// The directory the STEP picker closed in, so the next one starts there.
    pub step_dir: Option<PathBuf>,
    /// The directory the setup picker closed in.
    pub setup_dir: Option<PathBuf>,
}

/// How many setup files the panel lists.
pub const RECENT_LIMIT: usize = 8;

impl Recents {
    /// Read the store, or an empty list when it is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Write the store, creating its directory.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("cannot serialise the recent list: {error}"))?;
        fs::write(path, text + "\n")
            .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }

    /// Put a setup at the front, keeping one entry per file. True when the
    /// list changed.
    pub fn record(&mut self, path: &Path) -> bool {
        let path = absolute(path);
        if self.setups.first() == Some(&path) {
            return false;
        }
        self.setups.retain(|existing| absolute(existing) != path);
        self.setups.insert(0, path);
        self.setups.truncate(RECENT_LIMIT);
        true
    }

    /// Drop entries whose file has gone. True when the list changed.
    pub fn prune(&mut self) -> bool {
        let before = self.setups.len();
        self.setups.retain(|path| path.is_file());
        before != self.setups.len()
    }

    /// The list as the panel shows it, newest first.
    pub fn entries(&self) -> &[PathBuf] {
        &self.setups
    }

    /// Where a picker should start: the directory it last closed in.
    pub fn dir_for(&self, purpose: Purpose) -> Option<&Path> {
        match purpose {
            Purpose::Step => self.step_dir.as_deref(),
            Purpose::Setup => self.setup_dir.as_deref(),
        }
    }

    /// Remember the directory a picker closed in. True when the list changed.
    pub fn remember_dir(&mut self, purpose: Purpose, dir: &Path) -> bool {
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| absolute(dir));
        let slot = match purpose {
            Purpose::Step => &mut self.step_dir,
            Purpose::Setup => &mut self.setup_dir,
        };
        if slot.as_deref() == Some(dir.as_path()) {
            return false;
        }
        *slot = Some(dir);
        true
    }
}

/// The store's path: $XDG_STATE_HOME/linc/sim3d-recent.json, else
/// $HOME/.local/state/linc/sim3d-recent.json.
pub fn state_path() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(state).join("linc").join("sim3d-recent.json"));
    }
    home_dir().map(|home| {
        home.join(".local")
            .join("state")
            .join("linc")
            .join("sim3d-recent.json")
    })
}

/// A path with the working directory applied, so two spellings of one file
/// compare equal in the recent list.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}
