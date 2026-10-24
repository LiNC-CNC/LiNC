//! The side panel: status, workpieces, placement, camera and jog controls.
//!
//! The panel is the only place in window mode that changes the model, and it
//! applies its changes through the frozen Action enum, so the engine thread
//! never sees a half-applied edit. The one thing Action cannot express is a
//! workpiece's visibility: that lives in the config and the model reads it on
//! load, so a toggle is stored in the config and the model is reloaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Color32, RichText, Sense, Ui};

use crate::browser::{self, Browser, Purpose, Recents};
use crate::config::{Sim3dConfig, WorkpieceEntry};
use crate::engine::{MockPositionSource, Positions};
use crate::model::{Action, CollisionTarget, PlacedWorkpiece, SharedState, Snapshot};
use crate::tools::{ActiveTool, CutterShape};
use crate::types::{Transform, P3};
use crate::viewer::Viewer;

const GREEN: Color32 = Color32::from_rgb(0x5c, 0xc8, 0x6c);
const RED: Color32 = Color32::from_rgb(0xe6, 0x5c, 0x4c);
const AMBER: Color32 = Color32::from_rgb(0xd8, 0xb0, 0x50);

/// Which position source the window samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// The linc-sim3d HAL component: a live LinuxCNC machine.
    Hal,
    /// The window itself, through the jog controls.
    Ui,
}

impl SourceKind {
    /// The word the panel shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hal => "HAL",
            Self::Ui => "UI jog",
        }
    }

    /// True when the window owns the position.
    pub fn is_ui(self) -> bool {
        matches!(self, Self::Ui)
    }
}

/// One workpiece as the panel needs it: identity, placement and size.
#[derive(Debug, Clone)]
pub struct WorkpieceRow {
    pub name: String,
    /// The resolved STEP path, which is how a row maps back to its config entry.
    pub source: PathBuf,
    pub transform: Transform,
    pub visible: bool,
    pub triangles: usize,
}

/// The config's workpiece list, built from the model's rows.
///
/// The rows are the truth. An entry the list already names keeps its name and
/// takes the row's placement and visibility; a row the list never named is
/// added under the part's label; an entry the model has no row for is dropped,
/// because that workpiece was removed. A path under base is stored relative to
/// it, so a saved setup reads the same beside its STEP files.
pub fn entries_from_rows(
    rows: &[WorkpieceRow],
    existing: &[WorkpieceEntry],
    base: &Path,
) -> Vec<WorkpieceEntry> {
    let base = canonical(base);
    rows.iter()
        .map(|row| {
            let source = canonical(&row.source);
            let name = existing
                .iter()
                .find(|entry| canonical(&resolve(&entry.path, &base)) == source)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| row.name.clone());
            WorkpieceEntry {
                name,
                path: relative_to(&source, &base).unwrap_or(source),
                translate_mm: row.transform.translation,
                rotate_deg: row.transform.rotation_deg,
                visible: row.visible,
            }
        })
        .collect()
}

/// The entries as a target file should store them: each path resolved against
/// this session's config and made relative to the target's directory when it
/// lies under it.
pub fn entries_for_file(config: &Sim3dConfig, target: &Path) -> Vec<WorkpieceEntry> {
    let base = canonical(target.parent().unwrap_or_else(|| Path::new(".")));
    config
        .workpieces
        .iter()
        .map(|entry| {
            let mut entry = entry.clone();
            let absolute = canonical(&config.resolve(&entry.path));
            entry.path = relative_to(&absolute, &base).unwrap_or(absolute);
            entry
        })
        .collect()
}

/// Write a workpiece list into a config file.
///
/// The file keeps its own machine, probe, tool, cutter and HAL sections: a
/// setup saved from another profile must not have this session's machine
/// written into it. A file that does not exist yet is written from fallback.
pub fn write_workpieces(
    target: &Path,
    workpieces: Vec<WorkpieceEntry>,
    fallback: &Sim3dConfig,
) -> Result<(), String> {
    let mut config = if target.is_file() {
        Sim3dConfig::load(target).map_err(|error| error.to_string())?
    } else {
        fallback.clone()
    };
    config.workpieces = workpieces;
    config.save(target).map_err(|error| error.to_string())
}

/// path taken relative to base, when it lies under it.
pub fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    path.strip_prefix(base)
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn resolve(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub use crate::browser::expand_path;

/// The cutter profile in one line, for the status block.
pub fn shape_label(shape: &CutterShape) -> String {
    match *shape {
        CutterShape::Flat { diameter } => format!("flat D{diameter:.2}"),
        CutterShape::Ball { diameter } => format!("ball D{diameter:.2}"),
        CutterShape::Bull {
            diameter,
            corner_radius,
        } => format!("bull D{diameter:.2} R{corner_radius:.2}"),
        CutterShape::Drill { diameter } => format!("drill D{diameter:.2}"),
        CutterShape::Chamfer { diameter } => format!("chamfer D{diameter:.2}"),
        CutterShape::Probe { tip_radius } => format!("probe ball R{tip_radius:.2}"),
    }
}

/// The collision target in one line.
pub fn target_label(target: Option<CollisionTarget>) -> String {
    match target {
        Some(CollisionTarget::Workpiece(index)) => format!("workpiece {index}"),
        Some(CollisionTarget::Machine) => "machine envelope".to_string(),
        None => "collision".to_string(),
    }
}

/// The tip in machine millimetres, for the readouts.
pub fn format_tip(tip: P3) -> String {
    format!("{:9.3} {:9.3} {:9.3} mm", tip[0], tip[1], tip[2])
}

fn axis_mut(positions: &mut Positions, axis: usize) -> &mut f64 {
    match axis {
        0 => &mut positions.x,
        1 => &mut positions.y,
        _ => &mut positions.z,
    }
}

/// A message from the last panel action.
struct Notice {
    error: bool,
    text: String,
}

/// The side panel and the state it owns.
pub struct Panel {
    config: Sim3dConfig,
    config_path: PathBuf,
    source: SourceKind,
    jog: MockPositionSource,
    jog_step: f64,
    tool_number: u16,
    add_path: String,
    selected: Option<usize>,
    notice: Option<Notice>,
    /// The picker, while one is open.
    browser: Option<Browser>,
    /// Where each picker starts: the directory it was last used in.
    step_dir: PathBuf,
    setup_dir: PathBuf,
    /// The setup files this machine has opened or saved, newest first.
    recents: Recents,
    /// Where the recent list is stored; None when there is no state
    /// directory to write it to.
    recents_path: Option<PathBuf>,
    /// The setup file the workpiece list was loaded from, when that is not
    /// the config this panel saves to.
    setup_source: Option<PathBuf>,
}

impl Panel {
    /// A panel over a config, with the position source it feeds in UI mode.
    ///
    /// The jog source is handed in rather than created here, so the engine
    /// thread and the panel share one cell.
    pub fn new(
        config: Sim3dConfig,
        config_path: PathBuf,
        source: SourceKind,
        jog: MockPositionSource,
    ) -> Self {
        // The window opens on the probe: the probe is what this simulator is
        // for, and tool 0 has no geometry to draw.
        let tool_number = config.probe.tool_number;
        let mut positions = jog.get();
        positions.tool_number = tool_number;
        jog.set(positions);
        let recents_path = browser::state_path();
        let mut recents = recents_path
            .as_deref()
            .map(Recents::load)
            .unwrap_or_default();
        recents.prune();
        // The session config is not recorded here: it would sit at the top of
        // every run and clicking it would reload what is already loaded. A save
        // or an open records it.
        let fallback = usable_dir(setup_dir(&config));
        let start = |purpose: Purpose| {
            recents
                .dir_for(purpose)
                .map(Path::to_path_buf)
                .filter(|dir| dir.is_dir())
                .unwrap_or_else(|| fallback.clone())
        };
        Self {
            config,
            config_path,
            source,
            jog,
            jog_step: 1.0,
            tool_number,
            add_path: String::new(),
            selected: None,
            notice: None,
            browser: None,
            step_dir: start(Purpose::Step),
            setup_dir: start(Purpose::Setup),
            recents,
            recents_path,
            setup_source: None,
        }
    }

    /// Draw the panel. Returns true when the model changed and the caller has
    /// to rebuild its scene.
    pub fn ui(&mut self, ui: &mut Ui, shared: &Arc<SharedState>, viewer: &mut Viewer) -> bool {
        let snapshot = shared.snapshot();
        let (tool, faults, rows) = {
            let model = shared.read_model();
            let tool = model.active_tool(snapshot.tool_number, 0.0);
            let rows = model
                .workpieces()
                .iter()
                .map(|workpiece| WorkpieceRow {
                    name: workpiece.name.clone(),
                    source: workpiece.source.clone(),
                    transform: workpiece.transform,
                    visible: workpiece.visible,
                    triangles: workpiece.mesh.triangle_count(),
                })
                .collect::<Vec<_>>();
            (tool, model.faults().to_vec(), rows)
        };

        let mut changed = false;
        egui::ScrollArea::vertical()
            .id_salt("linc-sim3d-panel")
            .show(ui, |ui| {
                self.status(ui, &snapshot, &tool, &faults);
                self.position_source(ui, &snapshot);
                changed |= self.workpieces(ui, shared, &rows, snapshot.tip);
                self.camera(ui, viewer, snapshot.tip);
                legend(ui);
                if let Some(notice) = &self.notice {
                    ui.separator();
                    let color = if notice.error { RED } else { GREEN };
                    ui.colored_label(color, &notice.text);
                }
            });
        changed |= self.browse(ui.ctx(), shared);
        changed
    }

    // ------------------------------------------------------------- status

    fn status(&self, ui: &mut Ui, snapshot: &Snapshot, tool: &ActiveTool, faults: &[String]) {
        ui.heading("Status");
        egui::Grid::new("linc-sim3d-status")
            .num_columns(2)
            .spacing([10.0, 2.0])
            .show(ui, |ui| {
                ui.label("tip");
                ui.monospace(format_tip(snapshot.tip));
                ui.end_row();

                ui.label("probe");
                if snapshot.probe_touching {
                    ui.colored_label(GREEN, "CONTACT");
                } else {
                    ui.label("clear");
                }
                ui.end_row();

                ui.label("collision");
                if snapshot.collision {
                    ui.colored_label(RED, target_label(snapshot.collision_target));
                } else {
                    ui.label("none");
                }
                ui.end_row();

                ui.label("tool");
                ui.monospace(format!(
                    "T{} {}  R{:.2}",
                    tool.number,
                    shape_label(&tool.shape),
                    tool.tip_radius
                ));
                ui.end_row();

                ui.label("sample");
                ui.monospace(format!("{:.1} ms", self.config.hal.sample_ms));
                ui.end_row();
            });
        for fault in faults {
            ui.colored_label(RED, format!("fault: {fault}"));
        }
    }

    // ---------------------------------------------------------- jog / source

    fn position_source(&mut self, ui: &mut Ui, snapshot: &Snapshot) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Position");
            let (color, text) = if self.source.is_ui() {
                (AMBER, "source: UI jog (no HAL)")
            } else {
                (GREEN, "source: HAL")
            };
            ui.colored_label(color, RichText::new(text).strong());
        });
        let enabled = self.source.is_ui();
        let mut positions = self.jog.get();
        let mut changed = false;
        egui::Grid::new("linc-sim3d-jog")
            .num_columns(4)
            .spacing([6.0, 2.0])
            .show(ui, |ui| {
                for (axis, name) in ["X", "Y", "Z"].into_iter().enumerate() {
                    ui.label(name);
                    if ui.add_enabled(enabled, egui::Button::new("-")).clicked() {
                        *axis_mut(&mut positions, axis) -= self.jog_step;
                        changed = true;
                    }
                    if ui.add_enabled(enabled, egui::Button::new("+")).clicked() {
                        *axis_mut(&mut positions, axis) += self.jog_step;
                        changed = true;
                    }
                    let value = axis_mut(&mut positions, axis);
                    if ui
                        .add_enabled(
                            enabled,
                            egui::DragValue::new(value).speed(1.0).suffix(" mm"),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.end_row();
                }
                ui.label("tool");
                if ui
                    .add_enabled(
                        enabled,
                        egui::DragValue::new(&mut self.tool_number)
                            .range(0..=999)
                            .speed(1.0),
                    )
                    .changed()
                {
                    positions.tool_number = self.tool_number;
                    changed = true;
                }
                ui.label(format!("(probe T{})", self.config.probe.tool_number));
                ui.end_row();
            });
        ui.horizontal(|ui| {
            ui.label("step");
            for step in [0.1, 1.0, 10.0] {
                ui.selectable_value(&mut self.jog_step, step, format!("{step}"));
            }
        });
        if !enabled {
            ui.label("HAL owns the position; jog from LinuxCNC.");
        }
        if changed {
            self.jog.set(positions);
        }
        ui.label(format!("sampled tip {}", format_tip(snapshot.tip)));
    }

    // -------------------------------------------------------- workpieces

    fn workpieces(
        &mut self,
        ui: &mut Ui,
        shared: &Arc<SharedState>,
        rows: &[WorkpieceRow],
        tip: P3,
    ) -> bool {
        ui.separator();
        ui.heading("Workpieces");
        if rows.is_empty() {
            ui.label("none loaded");
        }

        let mut changed = false;
        let mut remove: Option<usize> = None;
        let mut visibility: Option<(PathBuf, bool)> = None;
        for (index, row) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut visible = row.visible;
                if ui.checkbox(&mut visible, "").changed() {
                    visibility = Some((row.source.clone(), visible));
                }
                if ui
                    .selectable_label(self.selected == Some(index), &row.name)
                    .clicked()
                {
                    self.selected = Some(index);
                }
                ui.label(format!("{} tris", row.triangles));
                if ui.button("remove").clicked() {
                    remove = Some(index);
                }
            });
            ui.label(
                RichText::new(format!(
                    "  {}  t=({:.1}, {:.1}, {:.1}) rot=({:.1}, {:.1}, {:.1})",
                    row.source.display(),
                    row.transform.translation[0],
                    row.transform.translation[1],
                    row.transform.translation[2],
                    row.transform.rotation_deg[0],
                    row.transform.rotation_deg[1],
                    row.transform.rotation_deg[2],
                ))
                .small()
                .weak(),
            );
        }

        if let Some((source, visible)) = visibility {
            self.set_visible(shared, &source, visible);
            changed = true;
        }
        if let Some(index) = remove {
            changed |= self.apply(shared, Action::RemoveWorkpiece { index });
            self.selected = None;
        }

        ui.horizontal(|ui| {
            ui.label("STEP");
            ui.add(
                egui::TextEdit::singleline(&mut self.add_path)
                    .desired_width(150.0)
                    .hint_text("~/parts/block.step"),
            );
            if ui.button("Add").clicked() && !self.add_path.trim().is_empty() {
                let path = expand_path(&self.add_path);
                changed |= self.add_workpiece(shared, path);
            }
            if ui
                .button("browse...")
                .on_hover_text("pick a STEP or STL file")
                .clicked()
            {
                self.open_picker(Purpose::Step);
            }
        });

        if let Some(index) = self.selected.filter(|index| *index < rows.len()) {
            changed |= self.placement(ui, shared, index, &rows[index], tip);
        }

        ui.horizontal(|ui| {
            if ui
                .button("Save")
                .on_hover_text(format!(
                    "write the workpieces to {}",
                    self.save_target().display()
                ))
                .clicked()
            {
                self.save(rows);
            }
            if ui.button("Reload").clicked() {
                changed |= self.apply(shared, Action::Reload);
            }
            if ui
                .button("Open setup...")
                .on_hover_text("load another setup file's workpieces")
                .clicked()
            {
                self.open_picker(Purpose::Setup);
            }
        });
        ui.label(
            RichText::new(format!("config {}", self.config_path.display()))
                .small()
                .weak(),
        );
        ui.label(
            RichText::new(format!("save writes {}", self.save_target().display()))
                .small()
                .weak(),
        );
        if !self.recents.entries().is_empty() {
            let mut open: Option<PathBuf> = None;
            ui.collapsing("Recent setups", |ui| {
                for path in self.recents.entries() {
                    if ui
                        .selectable_label(false, recent_label(path))
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        open = Some(path.clone());
                    }
                }
            });
            if let Some(path) = open {
                changed |= self.open_setup(shared, &path);
            }
        }
        changed
    }

    fn placement(
        &mut self,
        ui: &mut Ui,
        shared: &Arc<SharedState>,
        index: usize,
        row: &WorkpieceRow,
        tip: P3,
    ) -> bool {
        let mut transform = row.transform;
        let mut edited = false;
        egui::Grid::new("linc-sim3d-placement")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                ui.label("translate mm");
                ui.horizontal(|ui| {
                    for (axis, prefix) in ["X ", "Y ", "Z "].into_iter().enumerate() {
                        if ui
                            .add(
                                egui::DragValue::new(&mut transform.translation[axis])
                                    .speed(0.5)
                                    .prefix(prefix),
                            )
                            .changed()
                        {
                            edited = true;
                        }
                    }
                });
                ui.end_row();

                ui.label("rotate deg");
                ui.horizontal(|ui| {
                    for (axis, prefix) in ["X ", "Y ", "Z "].into_iter().enumerate() {
                        if ui
                            .add(
                                egui::DragValue::new(&mut transform.rotation_deg[axis])
                                    .speed(0.5)
                                    .prefix(prefix),
                            )
                            .changed()
                        {
                            edited = true;
                        }
                    }
                });
                ui.end_row();
            });
        if ui.button("use current position as origin").clicked() {
            transform.translation = tip;
            edited = true;
        }
        if !edited {
            return false;
        }
        self.apply(shared, Action::SetTransform { index, transform })
    }

    /// Store a visibility change: the model takes visibility from the config,
    /// so the config is written and the model reloaded.
    fn set_visible(&mut self, shared: &Arc<SharedState>, source: &Path, visible: bool) {
        let Some(index) = self.find_entry(source) else {
            self.notice = Some(Notice {
                error: true,
                text: format!("{} is not in the config", source.display()),
            });
            return;
        };
        self.config.workpieces[index].visible = visible;
        let target = self.save_target();
        let entries = entries_for_file(&self.config, &target);
        let fallback = self.config.clone();
        if let Err(text) = write_workpieces(&target, entries, &fallback) {
            self.notice = Some(Notice {
                error: true,
                text: format!("cannot save the workpieces: {text}"),
            });
            return;
        }
        match shared.write_model().apply(Action::Reload) {
            Ok(()) => {
                self.notice = Some(Notice {
                    error: false,
                    text: format!("visibility stored in {} and reloaded", target.display()),
                });
            }
            Err(error) => {
                self.notice = Some(Notice {
                    error: true,
                    text: format!("cannot reload the model: {error}"),
                });
            }
        }
    }

    fn find_entry(&self, source: &Path) -> Option<usize> {
        self.config
            .workpieces
            .iter()
            .position(|entry| self.config.resolve(&entry.path) == source)
    }

    /// Write the workpiece state, every row and nothing else, into the setup
    /// file in use.
    fn save(&mut self, rows: &[WorkpieceRow]) {
        let target = self.save_target();
        // Two spellings of the same list: the panel keeps paths relative to this
        // session's config, while the file gets paths relative to itself.
        let entries = entries_from_rows(rows, &self.config.workpieces, &self.config.base_dir);
        self.config.workpieces = entries;
        let entries = entries_for_file(&self.config, &target);
        let count = entries.len();
        let fallback = self.config.clone();
        let saved = write_workpieces(&target, entries, &fallback);
        let ok = saved.is_ok();
        self.notice = Some(match saved {
            Ok(()) => Notice {
                error: false,
                text: format!("saved {count} workpiece(s) to {}", target.display()),
            },
            Err(text) => Notice {
                error: true,
                text: format!("cannot save the workpieces: {text}"),
            },
        });
        if ok {
            self.record_recent(&target);
        }
    }

    /// The file Save writes: the setup the workpieces came from, else the config
    /// this window was started on.
    fn save_target(&self) -> PathBuf {
        self.setup_source
            .clone()
            .unwrap_or_else(|| self.config_path.clone())
    }

    fn apply(&mut self, shared: &Arc<SharedState>, action: Action) -> bool {
        match shared.write_model().apply(action) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(Notice {
                    error: true,
                    text: error.to_string(),
                });
                false
            }
        }
    }

    // ------------------------------------------------------------ pickers

    /// True while a picker is up, so Escape closes it instead of the window.
    pub fn browser_open(&self) -> bool {
        self.browser.as_ref().is_some_and(Browser::is_open)
    }

    /// Open the picker for one purpose, where it was last used.
    fn open_picker(&mut self, purpose: Purpose) {
        let remembered = self
            .recents
            .dir_for(purpose)
            .map(Path::to_path_buf)
            .filter(|dir| dir.is_dir());
        let dir = remembered.unwrap_or_else(|| match purpose {
            Purpose::Step => self.step_dir.clone(),
            Purpose::Setup => self.setup_dir.clone(),
        });
        self.browser = Some(Browser::new(purpose, usable_dir(dir)));
    }

    /// Draw the open picker and act on its choice.
    fn browse(&mut self, ctx: &egui::Context, shared: &Arc<SharedState>) -> bool {
        let Some((purpose, picked, still_open, dir)) = self.browser.as_mut().map(|browser| {
            let purpose = browser.purpose();
            let picked = browser.show(ctx);
            (
                purpose,
                picked,
                browser.is_open(),
                browser.dir().to_path_buf(),
            )
        }) else {
            return false;
        };
        if !still_open {
            match purpose {
                Purpose::Step => self.step_dir = dir.clone(),
                Purpose::Setup => self.setup_dir = dir.clone(),
            }
            self.browser = None;
            if self.recents.remember_dir(purpose, &dir) {
                self.persist_recents();
            }
        }
        let Some(path) = picked else {
            return false;
        };
        match purpose {
            Purpose::Step => {
                self.add_path = path.display().to_string();
                self.add_workpiece(shared, path)
            }
            Purpose::Setup => self.open_setup(shared, &path),
        }
    }

    /// Import one STEP or STL file and select the new row.
    fn add_workpiece(&mut self, shared: &Arc<SharedState>, path: PathBuf) -> bool {
        if !self.apply(
            shared,
            Action::AddWorkpiece {
                path,
                transform: Transform::identity(),
            },
        ) {
            return false;
        }
        self.add_path.clear();
        self.selected = shared.read_model().workpieces().len().checked_sub(1);
        true
    }

    /// Load a setup file's workpiece list.
    ///
    /// Only the workpieces move. A setup saved from another profile carries
    /// its own INI, tool table and HAL section, and none of those can change
    /// under a running session: the machine body and the tools were built from
    /// this session's config, and the HAL component holds the pins it started
    /// with.
    fn open_setup(&mut self, shared: &Arc<SharedState>, path: &Path) -> bool {
        let config = match Sim3dConfig::load(path) {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some(Notice {
                    error: true,
                    text: format!("cannot open {}: {error}", path.display()),
                });
                return false;
            }
        };
        let source = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let entries: Vec<PlacedWorkpiece> = config
            .workpieces
            .iter()
            .map(|entry| PlacedWorkpiece {
                path: config.resolve(&entry.path),
                name: entry.name.clone(),
                transform: entry.transform(),
                visible: entry.visible,
            })
            .collect();
        let requested = entries.len();
        if let Err(error) = shared.write_model().apply(Action::LoadWorkpieces {
            entries: entries.clone(),
            source: Some(source.clone()),
        }) {
            self.notice = Some(Notice {
                error: true,
                text: error.to_string(),
            });
            return false;
        }
        // The panel keeps its own entry list so a save or a visibility toggle
        // still finds the row it started from. Entries stay relative when the
        // setup lives beside this config, and become absolute otherwise.
        let beside = config.base_dir == self.config.base_dir;
        self.config.workpieces = config
            .workpieces
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                if !beside {
                    entry.path = config.resolve(&entry.path);
                }
                entry
            })
            .collect();
        self.selected = None;
        self.setup_source = Some(source.clone());
        let loaded = shared.read_model().workpieces().len();
        self.record_recent(&source);
        self.notice = Some(Notice {
            error: false,
            text: format!(
                "loaded {loaded} of {requested} workpiece(s) from {}; the machine and the tools are unchanged",
                path.display()
            ),
        });
        true
    }

    /// Remember a setup file, newest first, and write the store out.
    fn record_recent(&mut self, path: &Path) {
        if self.recents.record(path) {
            self.persist_recents();
        }
    }

    fn persist_recents(&mut self) {
        let Some(store) = self.recents_path.clone() else {
            return;
        };
        if let Err(error) = self.recents.save(&store) {
            self.notice = Some(Notice {
                error: true,
                text: error,
            });
        }
    }

    // ------------------------------------------------------------- camera

    fn camera(&mut self, ui: &mut Ui, viewer: &mut Viewer, tip: P3) {
        ui.separator();
        ui.heading("Camera");
        ui.horizontal(|ui| {
            if ui.button("fit").clicked() {
                viewer.fit();
            }
            if ui
                .button("probe")
                .on_hover_text("centre on the live tip, close up")
                .clicked()
            {
                viewer.look_at(tip, 140.0);
            }
            if ui.button("iso").clicked() {
                preset(viewer, crate::viewer::Camera::iso());
            }
            if ui.button("top").clicked() {
                preset(viewer, crate::viewer::Camera::top());
            }
            if ui.button("front").clicked() {
                preset(viewer, crate::viewer::Camera::front());
            }
        });
        ui.label(
            RichText::new("drag orbits, middle/right drag pans, wheel zooms")
                .small()
                .weak(),
        );
    }
}

/// The directory a picker starts in when no better one is known.
fn setup_dir(config: &Sim3dConfig) -> PathBuf {
    if config.base_dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        config.base_dir.clone()
    }
}

/// A directory that exists, as an absolute path: the one asked for, else home,
/// else the root. A config loaded from a relative path has a relative base
/// directory, and a picker started there could not walk up out of it.
fn usable_dir(dir: PathBuf) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(&dir) {
        return resolved;
    }
    if dir.is_dir() {
        return dir;
    }
    browser::home_dir()
        .filter(|home| home.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// A recent setup as the panel lists it: file name, then its directory.
fn recent_label(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => format!("{name}   {}", parent.display()),
        None => name,
    }
}

fn preset(viewer: &mut Viewer, mut camera: crate::viewer::Camera) {
    camera.fit(&viewer.scene().bounds);
    viewer.camera = camera;
}

fn legend(ui: &mut Ui) {
    ui.separator();
    ui.heading("Legend");
    for (text, color) in crate::viewer::legend() {
        ui.horizontal(|ui| {
            swatch(ui, color);
            ui.label(text);
        });
    }
}

fn swatch(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}
