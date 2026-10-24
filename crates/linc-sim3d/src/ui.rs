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

use crate::config::Sim3dConfig;
use crate::engine::{MockPositionSource, Positions};
use crate::model::{Action, CollisionTarget, SharedState, Snapshot};
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

/// Write the model's placement and visibility back into the config entries.
///
/// Entries are matched by resolved source path, so a part that failed to import
/// keeps the entry it came from. Entries the model has no row for are left
/// alone. Returns how many entries were updated.
pub fn sync_config(config: &mut Sim3dConfig, rows: &[WorkpieceRow]) -> usize {
    let resolved: Vec<(usize, PathBuf)> = config
        .workpieces
        .iter()
        .enumerate()
        .map(|(index, entry)| (index, config.resolve(&entry.path)))
        .collect();
    let mut updated = 0;
    for row in rows {
        let Some((index, _)) = resolved.iter().find(|(_, path)| *path == row.source) else {
            continue;
        };
        let entry = &mut config.workpieces[*index];
        entry.translate_mm = row.transform.translation;
        entry.rotate_deg = row.transform.rotation_deg;
        entry.visible = row.visible;
        updated += 1;
    }
    updated
}

/// Expand a typed path: a leading ~ is the home directory, everything else is
/// used as typed and resolves against the working directory.
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

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
                    .desired_width(190.0)
                    .hint_text("~/parts/block.step"),
            );
            if ui.button("Add").clicked() && !self.add_path.trim().is_empty() {
                let path = expand_path(&self.add_path);
                if self.apply(
                    shared,
                    Action::AddWorkpiece {
                        path,
                        transform: Transform::identity(),
                    },
                ) {
                    self.selected = Some(rows.len());
                    self.add_path.clear();
                    changed = true;
                }
            }
        });

        if let Some(index) = self.selected.filter(|index| *index < rows.len()) {
            changed |= self.placement(ui, shared, index, &rows[index], tip);
        }

        ui.horizontal(|ui| {
            if ui.button("Save config").clicked() {
                self.save(rows);
            }
            if ui.button("Reload").clicked() {
                changed |= self.apply(shared, Action::Reload);
            }
        });
        ui.label(
            RichText::new(format!("config {}", self.config_path.display()))
                .small()
                .weak(),
        );
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
        if let Err(error) = self.config.save(&self.config_path) {
            self.notice = Some(Notice {
                error: true,
                text: format!("cannot save the config: {error}"),
            });
            return;
        }
        match shared.write_model().apply(Action::Reload) {
            Ok(()) => {
                self.notice = Some(Notice {
                    error: false,
                    text: format!(
                        "visibility stored in {} and reloaded",
                        self.config_path.display()
                    ),
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

    fn save(&mut self, rows: &[WorkpieceRow]) {
        let updated = sync_config(&mut self.config, rows);
        self.notice = Some(match self.config.save(&self.config_path) {
            Ok(()) => Notice {
                error: false,
                text: format!(
                    "saved {updated} workpiece row(s) to {}",
                    self.config_path.display()
                ),
            },
            Err(error) => Notice {
                error: true,
                text: format!("cannot save the config: {error}"),
            },
        });
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
