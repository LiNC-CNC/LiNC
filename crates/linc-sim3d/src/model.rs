//! Workpieces, shared state and the contact queries.
//!
//! The model owns the machine body, the tool table and the imported
//! workpieces. The engine thread asks it two questions per tick - does the
//! probe touch, does the cutter collide - and the UI thread changes it through
//! Action values handed over in SharedState.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use thiserror::Error;

use crate::config::Sim3dConfig;
use crate::machine::MachineModel;
use crate::step::StepImporter;
use crate::tools::{self, ActiveTool, ProbeGeom};
use crate::types::{Contact, Sphere, Transform, TriMesh, P3};
use crate::MeshIndex;
use linc_core::ini::MachineIni;
use linc_core::tooltable::ToolTable;

/// What a cutter hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionTarget {
    /// Index into SimModel::workpieces().
    Workpiece(usize),
    /// The machine body, not a workpiece.
    Machine,
}

/// One probe query, as the engine publishes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeContact {
    /// Ball within trigger_slack_mm of a workpiece surface.
    pub touching: bool,
    /// Signed distance from the ball surface to the mesh.
    pub distance: f64,
    /// Closest point on the mesh, in machine mm.
    pub point: P3,
    /// Index of the closest workpiece, when there is one.
    pub workpiece: Option<usize>,
}

/// One cutter query, as the engine publishes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutterContact {
    /// The cutter or the machine envelope collided.
    pub hit: bool,
    /// Signed distance from the swept capsule to the nearest mesh.
    pub distance: f64,
    /// Collision point, in machine mm.
    pub point: P3,
    /// What was hit, when something was.
    pub target: Option<CollisionTarget>,
}

/// The published state of one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub tip: P3,
    pub probe_touching: bool,
    pub probe_point: Option<P3>,
    pub probe_workpiece: Option<usize>,
    pub collision: bool,
    pub collision_point: Option<P3>,
    pub collision_target: Option<CollisionTarget>,
    pub tool_number: u16,
    pub tick: u64,
    pub faults: Vec<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            tip: [0.0; 3],
            probe_touching: false,
            probe_point: None,
            probe_workpiece: None,
            collision: false,
            collision_point: None,
            collision_target: None,
            tool_number: 0,
            tick: 0,
            faults: Vec::new(),
        }
    }
}

impl Snapshot {
    /// The state before the first tick: no contact, no collision, no faults.
    pub fn idle() -> Self {
        Self::default()
    }
}

/// A change the UI asks the model to make.
#[derive(Debug, Clone)]
pub enum Action {
    AddWorkpiece { path: PathBuf, transform: Transform },
    RemoveWorkpiece { index: usize },
    SetTransform { index: usize, transform: Transform },
    Reload,
}

/// What the model can refuse to do.
#[derive(Debug, Error)]
pub enum ModelError {
    #[error("cannot read machine INI {path}: {cause}")]
    Ini { path: PathBuf, cause: String },
    #[error("cannot load simulator config {path}: {cause}")]
    Config { path: PathBuf, cause: String },
    #[error("cannot import {path}: {cause}")]
    Import { path: PathBuf, cause: String },
    #[error("workpiece index {index} is out of range (0..{len})")]
    Index { index: usize, len: usize },
}

/// An imported workpiece, placed in machine coordinates.
#[derive(Debug, Clone)]
pub struct Workpiece {
    pub name: String,
    /// The STEP file it came from.
    pub source: PathBuf,
    /// Its placement in machine coordinates.
    pub transform: Transform,
    /// The placed mesh, in machine mm; this is what the index and the UI use.
    pub mesh: TriMesh,
    /// BVH over mesh.
    pub index: MeshIndex,
    /// Draw it and test contact against it.
    pub visible: bool,
    /// The imported mesh before the placement, so a new transform is exact.
    pub raw: TriMesh,
}

/// The machine, its tools and the loaded workpieces.
#[derive(Debug)]
pub struct SimModel {
    config: Sim3dConfig,
    machine: MachineModel,
    tool_table: Option<ToolTable>,
    tool_warning: Option<String>,
    probe: ProbeGeom,
    importer: StepImporter,
    workpieces: Vec<Workpiece>,
    faults: Vec<String>,
}

impl SimModel {
    /// Build the model from the config: machine body, tool table and every
    /// configured workpiece.
    ///
    /// A STEP file that will not import is recorded as a fault and skipped, so
    /// one bad path never leaves the simulator without a machine.
    pub fn from_config(config: &Sim3dConfig) -> Result<Self, ModelError> {
        let ini_path = config.machine_ini_path();
        let ini = MachineIni::parse_file(&ini_path).map_err(|error| ModelError::Ini {
            path: ini_path.clone(),
            cause: error.to_string(),
        })?;
        let machine = MachineModel::from_ini(&ini, config);
        let load = tools::load_tool_table(&ini);
        let probe = match &load.table {
            Some(table) => tools::probe_from_table(table, config),
            None => tools::probe_without_table(config),
        };
        let mut model = Self {
            config: config.clone(),
            machine,
            tool_table: load.table,
            tool_warning: load.warning,
            probe,
            importer: StepImporter::discover(),
            workpieces: Vec::new(),
            faults: Vec::new(),
        };
        if let Some(warning) = &model.tool_warning {
            model.faults.push(warning.clone());
        }
        for entry in &config.workpieces {
            let path = config.workpiece_path(entry);
            let label = workpiece_label(&entry.name, &path);
            match model.load_workpiece(
                &path,
                entry.transform(),
                Some(entry.name.as_str()),
                entry.visible,
            ) {
                Ok(workpiece) => model.workpieces.push(workpiece),
                Err(error) => model
                    .faults
                    .push(format!("workpiece {label} skipped: {error}")),
            }
        }
        Ok(model)
    }

    /// The machine body.
    pub fn machine(&self) -> &MachineModel {
        &self.machine
    }

    /// Envelope of the machine body.
    pub fn machine_bounds(&self) -> crate::types::Bounds {
        self.machine.machine_bounds()
    }

    /// The loaded workpieces, in config order.
    pub fn workpieces(&self) -> &[Workpiece] {
        &self.workpieces
    }

    /// The config the model was built from.
    pub fn config(&self) -> &Sim3dConfig {
        &self.config
    }

    /// Probe geometry, from the tool table and config.
    pub fn probe(&self) -> ProbeGeom {
        self.probe
    }

    /// The tool table, when one could be loaded.
    pub fn tool_table(&self) -> Option<&ToolTable> {
        self.tool_table.as_ref()
    }

    /// Problems worth showing to the operator: a bad tool table, a skipped
    /// workpiece.
    pub fn faults(&self) -> &[String] {
        &self.faults
    }

    /// Record another fault.
    pub fn record_fault(&mut self, message: impl Into<String>) {
        self.faults.push(message.into());
    }

    /// The tool in the spindle, with the offset the engine reports applied.
    pub fn active_tool(&self, tool_number: u16, tool_offset_z: f64) -> ActiveTool {
        tools::active_tool(
            tool_number,
            tool_offset_z,
            self.tool_table.as_ref(),
            &self.config,
        )
    }

    /// Probe the workpiece at a machine-coordinate tip.
    ///
    /// The tip is the centre of the probe ball. touching is true once the ball
    /// surface is within the configured trigger slack of the nearest visible
    /// workpiece.
    pub fn probe_contact(&self, tip: P3) -> ProbeContact {
        let sphere = Sphere {
            center: tip,
            radius: self.probe.tip_radius,
        };
        let mut best: Option<(usize, Contact)> = None;
        for (position, workpiece) in self.workpieces.iter().enumerate() {
            if !workpiece.visible || workpiece.index.triangle_count() == 0 {
                continue;
            }
            let Some(contact) = workpiece.index.nearest_sphere(&sphere) else {
                continue;
            };
            if best.is_none_or(|(_, current)| contact.distance < current.distance) {
                best = Some((position, contact));
            }
        }
        match best {
            Some((position, contact)) => ProbeContact {
                touching: contact.distance <= self.config.probe.trigger_slack_mm,
                distance: contact.distance,
                point: contact.point,
                workpiece: Some(position),
            },
            None => ProbeContact {
                touching: false,
                distance: f64::INFINITY,
                point: tip,
                workpiece: None,
            },
        }
    }

    /// Collide a cutter with the workpieces and the machine envelope.
    ///
    /// The probe is not a cutter: a probe tool always answers hit = false, so
    /// probing is never reported as a crash. distance is INFINITY when nothing
    /// was within reach.
    pub fn cutter_contact(&self, tip: P3, tool: &ActiveTool) -> CutterContact {
        if tool.is_probe() {
            return CutterContact {
                hit: false,
                distance: f64::INFINITY,
                point: tip,
                target: None,
            };
        }
        let (_, capsule) = tool.contact_shapes(tip);
        let mut best: Option<(usize, Contact)> = None;
        for (position, workpiece) in self.workpieces.iter().enumerate() {
            if !workpiece.visible || workpiece.index.triangle_count() == 0 {
                continue;
            }
            let Some(contact) = workpiece.index.nearest_capsule(&capsule) else {
                continue;
            };
            if best.is_none_or(|(_, current)| contact.distance < current.distance) {
                best = Some((position, contact));
            }
        }
        if let Some((position, contact)) = best.filter(|(_, contact)| contact.distance <= 0.0) {
            return CutterContact {
                hit: true,
                distance: contact.distance,
                point: contact.point,
                target: Some(CollisionTarget::Workpiece(position)),
            };
        }
        if self.machine.envelope_violated(tip, capsule.b) {
            return CutterContact {
                hit: true,
                distance: 0.0,
                point: tip,
                target: Some(CollisionTarget::Machine),
            };
        }
        match best {
            Some((_, contact)) => CutterContact {
                hit: false,
                distance: contact.distance,
                point: contact.point,
                target: None,
            },
            None => CutterContact {
                hit: false,
                distance: f64::INFINITY,
                point: tip,
                target: None,
            },
        }
    }

    /// Apply one action; a failed import leaves the model unchanged.
    pub fn apply(&mut self, action: Action) -> Result<(), ModelError> {
        match action {
            Action::AddWorkpiece { path, transform } => {
                let workpiece = self.load_workpiece(&path, transform, None, true)?;
                self.workpieces.push(workpiece);
            }
            Action::RemoveWorkpiece { index } => {
                let len = self.workpieces.len();
                if index >= len {
                    return Err(ModelError::Index { index, len });
                }
                self.workpieces.remove(index);
            }
            Action::SetTransform { index, transform } => {
                let len = self.workpieces.len();
                let Some(workpiece) = self.workpieces.get_mut(index) else {
                    return Err(ModelError::Index { index, len });
                };
                let mesh = workpiece.raw.transformed(&transform);
                workpiece.transform = transform;
                workpiece.index = MeshIndex::build(&mesh);
                workpiece.mesh = mesh;
            }
            Action::Reload => self.reload()?,
        }
        Ok(())
    }

    /// Import and place one workpiece; the placed mesh is what gets queried.
    fn load_workpiece(
        &self,
        path: &Path,
        transform: Transform,
        name: Option<&str>,
        visible: bool,
    ) -> Result<Workpiece, ModelError> {
        let raw = self
            .importer
            .import(path)
            .map_err(|error| ModelError::Import {
                path: path.to_path_buf(),
                cause: error.to_string(),
            })?;
        let mesh = raw.transformed(&transform);
        let index = MeshIndex::build(&mesh);
        Ok(Workpiece {
            name: workpiece_label(name.unwrap_or(""), path),
            source: path.to_path_buf(),
            transform,
            mesh,
            index,
            visible,
            raw,
        })
    }

    /// Reread the config file and re-import every workpiece it names.
    ///
    /// Without a config file (a model built from an in-memory config) the
    /// recorded entries are re-imported instead, which still picks up changed
    /// STEP files.
    fn reload(&mut self) -> Result<(), ModelError> {
        let config = match self.config.source_path.clone() {
            Some(path) => Sim3dConfig::load(&path).map_err(|error| ModelError::Config {
                path: path.clone(),
                cause: error.to_string(),
            })?,
            None => self.config.clone(),
        };
        let mut workpieces = Vec::new();
        let mut faults: Vec<String> = self.tool_warning.iter().cloned().collect();
        for entry in &config.workpieces {
            let path = config.workpiece_path(entry);
            let label = workpiece_label(&entry.name, &path);
            match self.load_workpiece(
                &path,
                entry.transform(),
                Some(entry.name.as_str()),
                entry.visible,
            ) {
                Ok(workpiece) => workpieces.push(workpiece),
                Err(error) => faults.push(format!("workpiece {label} skipped: {error}")),
            }
        }
        self.config = config;
        self.workpieces = workpieces;
        self.faults = faults;
        Ok(())
    }
}

/// The display name: the configured one, or the STEP file stem.
fn workpiece_label(name: &str, path: &Path) -> String {
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "workpiece".to_string())
}

/// The model and the snapshot, shared by the engine thread and the UI thread.
#[derive(Debug)]
pub struct SharedState {
    pub model: RwLock<SimModel>,
    pub snapshot: RwLock<Snapshot>,
    pub actions: Mutex<Vec<Action>>,
}

impl SharedState {
    /// A model at rest, with an idle snapshot and no queued actions.
    pub fn new(model: SimModel) -> Self {
        Self {
            model: RwLock::new(model),
            snapshot: RwLock::new(Snapshot::idle()),
            actions: Mutex::new(Vec::new()),
        }
    }

    /// Read access to the model; a poisoned lock still hands out the model.
    pub fn read_model(&self) -> RwLockReadGuard<'_, SimModel> {
        self.model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Write access to the model.
    pub fn write_model(&self) -> RwLockWriteGuard<'_, SimModel> {
        self.model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The last published snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Publish a snapshot.
    pub fn set_snapshot(&self, snapshot: Snapshot) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    }

    /// Queue one action for the engine thread.
    pub fn push(&self, action: Action) {
        self.actions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(action);
    }

    /// Take every queued action.
    pub fn take_actions(&self) -> Vec<Action> {
        let mut actions = self
            .actions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_has_no_contact() {
        let snapshot = Snapshot::idle();
        assert_eq!(snapshot, Snapshot::default());
        assert_eq!(snapshot.tip, [0.0; 3]);
        assert!(!snapshot.probe_touching);
        assert!(!snapshot.collision);
        assert!(snapshot.probe_point.is_none());
        assert!(snapshot.collision_target.is_none());
        assert!(snapshot.faults.is_empty());
        assert_eq!(snapshot.tick, 0);
    }

    #[test]
    fn workpiece_label_falls_back_to_the_file_stem() {
        assert_eq!(
            workpiece_label("vise", Path::new("/parts/block.step")),
            "vise"
        );
        assert_eq!(
            workpiece_label("  ", Path::new("/parts/block.step")),
            "block"
        );
    }
}
