//! PLC: ladder editor and live monitor for the linc-plc daemon.
//!
//! The daemon speaks its JSON protocol over a Unix socket and the client call
//! blocks, so the page owns a worker thread: the UI thread posts requests and
//! drains replies once per frame. A missing or stopped daemon therefore leaves
//! the page fully usable for editing and local validation.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align, Align2, Color32, FontId, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_client::Client;
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::model::{FbdCell, FbdGrid, FbdLine};
use linc_plc_core::protocol::{
    Request, Response, RunState, StateSnapshot, TrendData, PROTOCOL_VERSION,
};
use linc_plc_core::WordValue;
use linc_plc_core::{
    validate, Coil, CoilKind, Diagnostic, IoDirection, Logic, Pou, PouLanguage, Project, Rung,
    Section, Severity, VarValue,
};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

/// Ladder box: every contact, constant and coil is this hit target, which is
/// wider than the 44 px minimum so a variable name stays readable.
const BOX: Vec2 = Vec2::new(72.0, 44.0);
/// Horizontal gap between two boxes in series, and the wire drawn in it.
const RAIL_GAP: f32 = 14.0;
/// Vertical gap between two branches of a parallel block.
const BRANCH_GAP: f32 = 8.0;
/// Room a parallel block keeps on each side for its vertical rail.
const PARALLEL_INSET: f32 = 16.0;
/// How often a connected page asks for a snapshot, and how often it retries a
/// daemon that is not there.
const POLL_CONNECTED: Duration = Duration::from_millis(120);
const POLL_OFFLINE: Duration = Duration::from_millis(900);
/// Force lifetime requested from the daemon. The page never holds a force for
/// ever: a lost HMI must not leave a pin pinned.
/// A simulation force holds until it is released or the data is reset, as the ZX
/// simulation's toggles do. It must never expire while the operator navigates.
const FORCE_TTL_MS: u32 = 0;
/// What the operator can do when the profile leaves the PLC off. The page stays
/// usable offline, so this is a hint, not an error.
pub const DISABLED_HINT: &str = "set [plc] enabled = true or run start-sim.sh --plc";

/// One selectable node: the section, the rung, and the child indices that lead
/// from the rung's logic root to the node. An empty path is the rung itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NodePath {
    pub section: usize,
    pub rung: usize,
    pub path: Vec<usize>,
}

impl NodePath {
    fn root(section: usize, rung: usize) -> Self {
        Self {
            section,
            rung,
            path: Vec::new(),
        }
    }

    fn child(&self, index: usize) -> Self {
        let mut path = self.path.clone();
        path.push(index);
        Self {
            section: self.section,
            rung: self.rung,
            path,
        }
    }

    fn parent(&self) -> Option<Self> {
        let mut path = self.path.clone();
        path.pop()?;
        Some(Self {
            section: self.section,
            rung: self.rung,
            path,
        })
    }

    fn label(&self) -> String {
        if self.path.is_empty() {
            format!("RUNG S{} R{}", self.section + 1, self.rung + 1)
        } else {
            let steps = self
                .path
                .iter()
                .map(|index| format!("[{}]", index + 1))
                .collect::<Vec<_>>()
                .join("");
            format!("S{} R{} {steps}", self.section + 1, self.rung + 1)
        }
    }
}

/// What a click on the page asked for. Nothing mutates the project while a
/// card is being drawn: the cards only record the intent, and the page applies
/// them after the last card so no borrow is held across an edit.
impl Action {
    /// Does this action change what a Save writes?
    pub fn edits_project(&self) -> bool {
        !matches!(
            self,
            Self::Select(_)
                | Self::Validate
                | Self::Download
                | Self::Start
                | Self::Stop
                | Self::ClearForces
                | Self::Force(_, _)
                | Self::Arm(_)
                | Self::Save
        )
    }
}

#[derive(Debug)]
pub enum Action {
    Select(NodePath),
    ToggleContact(NodePath),
    ToggleConstant(NodePath),
    SetVariable(String),
    SetCoilVariable(String),
    SetCoilKind(CoilKind),
    SetComment(String),
    WrapSeries,
    WrapParallel,
    Delete,
    AddRung,
    AddSection,
    MoveUp,
    MoveDown,
    Connect,
    New,
    Open,
    Save,
    Validate,
    Download,
    Start,
    /// Load the project, then start it: the daemon refuses a bare Start unless
    /// it is Loaded, so the manual's Run is the pair.
    RunProject,
    Stop,
    ClearForces,
    Force(String, bool),
    /// Pin an operand to the watch list, or unpin it.
    ToggleWatch(String),
    /// Release one operand's force.
    Unforce(String),
    /// Set the comment of a declared symbol.
    SetSymbolComment(String, String),
    /// Rename a symbol and every cell or coil that names it.
    RenameSymbol {
        from: String,
        to: String,
    },
    /// Add an output symbol.
    AddSymbol(String),
    /// Add an input symbol, which the DEMO ladder needs as much as outputs.
    AddInput(String),
    /// Add an internal variable: writable, bound to no HAL pin.
    AddVar(String),
    /// Remove a declared symbol.
    RemoveSymbol(String),
    /// Patch a block cell's instance and preset in place.
    ///
    /// A whole-cell write from the box window raced the cell it had read when it
    /// opened, so a placed `TOF0` could be replaced by the older cell and vanish
    /// from the saved file. This writes only the two fields the window owns.
    SetBlockField {
        section: usize,
        rung: usize,
        position: usize,
        instance: Option<String>,
        preset: Option<String>,
    },
    /// Insert a row directly after `row`, without consulting the tree selection.
    AddRungAfter {
        section: usize,
        row: usize,
    },
    /// Open a POU from the list by index.
    OpenPou(usize),
    /// Add a POU in the given language and open it.
    AddPou(PouLanguage),
    /// Set an existing POU's language, used by `L` on an empty POU.
    SetPouLanguage {
        index: usize,
        language: PouLanguage,
    },
    /// Remove a ladder row.
    DeleteRung {
        section: usize,
        rung: usize,
    },
    /// Set a coil's operand by name.
    SetCoilVar {
        section: usize,
        rung: usize,
        var: String,
    },
    /// Write a row's comment, which is what a comment row holds.
    SetRungComment {
        section: usize,
        rung: usize,
        comment: String,
    },
    /// Insert a comment row after this one.
    AddCommentRow {
        section: usize,
        rung: usize,
    },
    /// Replace the function-block body of the current POU.
    SetFbd(Vec<FbdLine>),
    /// Replace the drawn function-block grid of the current POU.
    SetFbdGrid(linc_plc_core::model::FbdGrid),
    /// Rename a POU.
    RenamePou {
        index: usize,
        name: String,
    },
    /// Copy a POU under a new name.
    DuplicatePou {
        index: usize,
        name: String,
    },
    /// Delete a POU. MAIN is refused.
    DeletePou {
        index: usize,
    },
    /// Remove a cell and close the row up to the left.
    DeleteCell {
        section: usize,
        rung: usize,
        position: usize,
    },
    /// Add an empty row after this one, for a branch to hang on.
    AddRowAfter {
        section: usize,
        rung: usize,
    },
    /// Insert a cell in front of the cursor, moving the rest of the row right.
    InsertCell {
        section: usize,
        rung: usize,
        position: usize,
        cell: Cell,
    },
    /// Write one grid cell of a rung.
    SetCell {
        section: usize,
        rung: usize,
        position: usize,
        cell: Cell,
    },
    /// Write the coil position of a grid rung.
    SetCoil {
        section: usize,
        rung: usize,
        coil: Coil,
    },
    Arm(String),
}

// ---------------------------------------------------------------------------
// Project helpers: pure, so the editor rules are testable without a daemon.
// ---------------------------------------------------------------------------

/// The variable a new contact starts on: the first input the project declares,
/// then the first internal variable, and only then a placeholder the validator
/// will flag.
pub fn default_input_var(project: &Project) -> String {
    project
        .io
        .iter()
        .find(|point| matches!(&point.direction, IoDirection::In))
        .map(|point| point.name.clone())
        .or_else(|| project.vars.first().map(|var| var.name.clone()))
        .unwrap_or_else(|| "X0".to_string())
}

/// The variable a new coil starts on.
pub fn default_output_var(project: &Project) -> String {
    project
        .io
        .iter()
        .find(|point| matches!(&point.direction, IoDirection::Out))
        .map(|point| point.name.clone())
        .or_else(|| project.vars.first().map(|var| var.name.clone()))
        .unwrap_or_else(|| "Y0".to_string())
}

fn rung_at<'a>(project: &'a Project, node: &NodePath) -> Option<&'a Rung> {
    project.sections.get(node.section)?.rungs.get(node.rung)
}

fn rung_at_mut<'a>(project: &'a mut Project, node: &NodePath) -> Option<&'a mut Rung> {
    project
        .sections
        .get_mut(node.section)?
        .rungs
        .get_mut(node.rung)
}

fn walk<'a>(mut logic: &'a Logic, path: &[usize]) -> Option<&'a Logic> {
    for &index in path {
        let items = match logic {
            Logic::Series { items } | Logic::Parallel { items } => items,
            _ => return None,
        };
        logic = items.get(index)?;
    }
    Some(logic)
}

fn walk_mut<'a>(logic: &'a mut Logic, path: &[usize]) -> Option<&'a mut Logic> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(logic);
    };
    let items = match logic {
        Logic::Series { items } | Logic::Parallel { items } => items,
        _ => return None,
    };
    walk_mut(items.get_mut(index)?, rest)
}

/// The logic a selection points at.
pub fn logic_at<'a>(project: &'a Project, node: &NodePath) -> Option<&'a Logic> {
    walk(&rung_at(project, node)?.logic, &node.path)
}

/// The same node, for editing.
pub fn logic_at_mut<'a>(project: &'a mut Project, node: &NodePath) -> Option<&'a mut Logic> {
    let rung = project
        .sections
        .get_mut(node.section)?
        .rungs
        .get_mut(node.rung)?;
    walk_mut(&mut rung.logic, &node.path)
}

/// Wrap the selected node in a series with one new contact, so the operator can
/// keep adding conditions to the left of what they had.
pub fn wrap_series(project: &mut Project, node: &NodePath) -> bool {
    if matches!(logic_at(project, node), Some(Logic::Series { .. })) {
        return false;
    }
    let var = default_input_var(project);
    let Some(slot) = logic_at_mut(project, node) else {
        return false;
    };
    let inner = slot.clone();
    *slot = Logic::Series {
        items: vec![
            inner,
            Logic::Contact {
                var,
                negated: false,
            },
        ],
    };
    true
}

/// Wrap the selected node in a parallel block with one new branch.
pub fn wrap_parallel(project: &mut Project, node: &NodePath) -> bool {
    if matches!(logic_at(project, node), Some(Logic::Parallel { .. })) {
        return false;
    }
    let var = default_input_var(project);
    let Some(slot) = logic_at_mut(project, node) else {
        return false;
    };
    let inner = slot.clone();
    *slot = Logic::Parallel {
        items: vec![
            inner,
            Logic::Contact {
                var,
                negated: false,
            },
        ],
    };
    true
}

/// Flip a contact between normally open and normally closed.
pub fn toggle_negated(project: &mut Project, node: &NodePath) -> bool {
    match logic_at_mut(project, node) {
        Some(Logic::Contact { negated, .. }) => {
            *negated = !*negated;
            true
        }
        _ => false,
    }
}

/// Flip a constant between always-false and always-true.
pub fn toggle_constant(project: &mut Project, node: &NodePath) -> bool {
    match logic_at_mut(project, node) {
        Some(Logic::Const { value }) => {
            *value = !*value;
            true
        }
        _ => false,
    }
}

/// Remove the selected node. A rung root removes the whole rung; a node inside
/// a series or parallel leaves its parent, and a parent left holding one item
/// collapses into that item instead of keeping a one-branch block.
pub fn delete_node(project: &mut Project, node: &NodePath) -> bool {
    let Some(parent_path) = node.parent() else {
        let Some(section) = project.sections.get_mut(node.section) else {
            return false;
        };
        if node.rung >= section.rungs.len() {
            return false;
        }
        section.rungs.remove(node.rung);
        return true;
    };
    let Some(index) = node.path.last().copied() else {
        return false;
    };
    let Some(parent) = logic_at_mut(project, &parent_path) else {
        return false;
    };
    let collapsed = {
        let items = match parent {
            Logic::Series { items } | Logic::Parallel { items } => items,
            _ => return false,
        };
        if index >= items.len() {
            return false;
        }
        items.remove(index);
        if items.len() == 1 {
            Some(items.remove(0))
        } else {
            None
        }
    };
    if let Some(only) = collapsed {
        *parent = only;
    }
    true
}

/// Append (or insert after the given rung) a contact driving a new coil.
pub fn add_rung(project: &mut Project, section: usize, after: Option<usize>) -> Option<NodePath> {
    let input = default_input_var(project);
    let output = default_output_var(project);
    let target = project.sections.get_mut(section)?;
    let at = match after {
        Some(index) if index < target.rungs.len() => index + 1,
        _ => target.rungs.len(),
    };
    target.rungs.insert(
        at,
        Rung {
            cells: Vec::new(),
            comment: String::new(),
            logic: Logic::Contact {
                var: input,
                negated: false,
            },
            coil: Coil {
                var: output,
                kind: CoilKind::Normal,
            },
        },
    );
    Some(NodePath::root(section, at))
}

/// Add an empty section and return its index.
pub fn add_section(project: &mut Project) -> usize {
    let name = format!("SECTION {}", project.sections.len() + 1);
    project.sections.push(Section {
        name,
        rungs: Vec::new(),
    });
    project.sections.len() - 1
}

/// Swap a rung with its neighbour. Returns the new index, or None at an end.
pub fn move_rung(
    project: &mut Project,
    section: usize,
    rung: usize,
    delta: isize,
) -> Option<usize> {
    let rungs = &mut project.sections.get_mut(section)?.rungs;
    let target = rung as isize + delta;
    if target < 0 || target as usize >= rungs.len() {
        return None;
    }
    rungs.swap(rung, target as usize);
    Some(target as usize)
}

/// Coil glyph, as it is drawn in the ladder and listed by the property panel.
pub fn coil_label(kind: CoilKind) -> &'static str {
    match kind {
        CoilKind::Normal => "( )",
        CoilKind::Negated => "(/)",
        CoilKind::Set => "(S)",
        CoilKind::Reset => "(R)",
    }
}

fn coil_index(kind: &CoilKind) -> usize {
    match kind {
        CoilKind::Normal => 0,
        CoilKind::Negated => 1,
        CoilKind::Set => 2,
        CoilKind::Reset => 3,
    }
}

pub fn run_state_label(state: RunState) -> (&'static str, Color32) {
    match state {
        RunState::Idle => ("IDLE", theme::MUTED),
        RunState::Loaded => ("LOADED", theme::C3),
        RunState::Running => ("RUNNING", theme::SUCCESS),
        RunState::Fault => ("FAULT", theme::ERROR),
    }
}

/// Height a logic block needs, given the parallel branches below it.
pub fn logic_height(logic: &Logic) -> f32 {
    match logic {
        Logic::Series { items } => items
            .iter()
            .map(logic_height)
            .fold(BOX.y, |height: f32, item: f32| height.max(item)),
        Logic::Parallel { items } => {
            let mut height = 0.0;
            let mut count = 0;
            for item in items {
                if count > 0 {
                    height += BRANCH_GAP;
                }
                height += logic_height(item);
                count += 1;
            }
            height.max(BOX.y)
        }
        _ => BOX.y,
    }
}

/// Width a logic block needs.
pub fn logic_width(logic: &Logic) -> f32 {
    match logic {
        Logic::Series { items } => {
            let mut width = 0.0;
            let mut count = 0;
            for item in items {
                if count > 0 {
                    width += RAIL_GAP;
                }
                width += logic_width(item);
                count += 1;
            }
            width.max(BOX.x)
        }
        Logic::Parallel { items } => {
            items
                .iter()
                .map(logic_width)
                .fold(0.0_f32, f32::max)
                .max(BOX.x)
                + 2.0 * PARALLEL_INSET
        }
        _ => BOX.x,
    }
}

/// Serialise a project the way the file on disk is written.
pub fn project_to_json(project: &Project) -> Result<String, String> {
    // Serialise a canonical copy: a file written from `pous` alone lost the ladder
    // when `pous` was empty and `sections` held MAIN.
    let mut canonical = project.clone();
    let pous = canonical.all_pous();
    canonical.set_pous(pous);
    canonical.normalise();
    serde_json::to_string_pretty(&canonical).map_err(|error| error.to_string())
}

pub fn project_from_json(text: &str) -> Result<Project, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}

/// The project the page opens on start: the one the profile names, else the
/// simulation's ladder, else the built-in example. Loading it is what makes an
/// edit survive a restart, which is the difference between a demo and a toy.
pub fn startup_path() -> PathBuf {
    if let Some(path) = std::env::var_os("LINC_PLC_PROJECT") {
        return PathBuf::from(path);
    }
    let candidate = PathBuf::from("configs/plc-sim.json");
    if candidate.exists() {
        candidate
    } else {
        default_project_path()
    }
}

fn startup_project() -> Project {
    let path = startup_path();
    let loaded = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| project_from_json(&text).ok());
    match loaded {
        Some(mut project) => {
            project.normalise();
            project
        }
        None => {
            let mut project = Project::example();
            project.normalise();
            project
        }
    }
}

/// Where the page keeps its project when nothing else says otherwise.
pub fn project_path_from(xdg: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    let base = xdg
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|path| path.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("linc").join("plc.json")
}

pub fn default_project_path() -> PathBuf {
    project_path_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

// ---------------------------------------------------------------------------
// Worker link
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Hello,
    Trend,
    Validate,
    Load,
    Start,
    Stop,
    Step,
    ResetData,
    Snapshot,
    Force,
    ClearForces,
}

enum Job {
    Request { tag: Tag, request: Request },
}

enum Event {
    Online,
    Offline(String),
    Reply {
        tag: Tag,
        result: Result<Response, String>,
    },
}

struct Link {
    jobs: Sender<Job>,
    events: Receiver<Event>,
}

/// Connect on demand and answer one request at a time. The thread ends when the
/// page drops its sender, which is what CONNECT does when the path changes.
fn spawn_link(path: PathBuf) -> Link {
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
    let (events_tx, events_rx) = mpsc::channel::<Event>();
    let _ = thread::Builder::new()
        .name("linc-plc-link".into())
        .spawn(move || worker(jobs_rx, events_tx, path));
    Link {
        jobs: jobs_tx,
        events: events_rx,
    }
}

fn worker(jobs: Receiver<Job>, events: Sender<Event>, path: PathBuf) {
    let mut client: Option<Client> = None;
    while let Ok(job) = jobs.recv() {
        let Job::Request { tag, request } = job;
        if client.is_none() {
            match Client::connect(path.as_path()) {
                Ok(connected) => {
                    client = Some(connected);
                    let _ = events.send(Event::Online);
                }
                Err(error) => {
                    let _ = events.send(Event::Offline(error.to_string()));
                    continue;
                }
            }
        }
        let Some(connected) = client.as_mut() else {
            continue;
        };
        match connected.call(&request) {
            Ok(response) => {
                let _ = events.send(Event::Reply {
                    tag,
                    result: Ok(response),
                });
            }
            Err(error) => {
                // The socket is unusable: drop it so the next request dials
                // again instead of writing into a dead stream.
                client = None;
                let _ = events.send(Event::Offline(error.to_string()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Page state
// ---------------------------------------------------------------------------

/// One compare cell (`G`) being edited: the condition and both operands are in
/// one window, and the focus says which of the three the keys are for.
#[derive(Debug, Clone)]
pub struct CompareEdit {
    pub section: usize,
    pub rung: usize,
    pub position: usize,
    pub op: String,
    pub left: String,
    pub right: String,
    /// 0 condition, 1 operand A, 2 operand B.
    pub focus: u8,
    /// Why the last Enter refused an operand.
    pub error: Option<String>,
}

/// One compute cell (`M`) being edited: the assignment's operation, its
/// destination and both operands, in one window.
#[derive(Debug, Clone)]
pub struct ComputeEdit {
    pub section: usize,
    pub rung: usize,
    pub position: usize,
    pub op: String,
    pub target: String,
    pub left: String,
    pub right: String,
    /// 0 operation, 1 destination, 2 operand A, 3 operand B.
    pub focus: u8,
    /// Why the last Enter refused a value.
    pub error: Option<String>,
}

/// A ring of boolean samples for the trend page: one lane per symbol, oldest
/// sample first. Sampling happens once per drawn frame, not per scan, so a
/// change shorter than a frame can be missed; the scan itself is unaffected.
#[derive(Debug, Clone, Default)]
pub struct TrendBuffer {
    pub channels: Vec<String>,
    pub lanes: Vec<Vec<bool>>,
    /// Block members (`ET`/`CV`) as analog lanes, up to four.
    pub word_channels: Vec<String>,
    pub word_lanes: Vec<Vec<i16>>,
    pub samples: usize,
}

impl TrendBuffer {
    pub const DEPTH: usize = 512;
    pub const CHANNELS: usize = 8;

    pub const WORD_CHANNELS: usize = 4;

    pub fn sample(&mut self, vars: &[VarValue], words: &[WordValue], watch: &[String]) {
        let names = if watch.is_empty() {
            vars.iter()
                .take(Self::CHANNELS)
                .map(|var| var.name.clone())
                .collect::<Vec<_>>()
        } else {
            watch
                .iter()
                .filter(|name| words.iter().all(|word| &word.name != *name))
                .take(Self::CHANNELS)
                .cloned()
                .collect::<Vec<_>>()
        };
        if names != self.channels {
            self.channels = names.clone();
            self.lanes = vec![Vec::new(); self.channels.len()];
        }
        let mut values = Vec::with_capacity(names.len());
        for name in &names {
            values.push(
                vars.iter()
                    .find(|var| &var.name == name)
                    .map(|var| var.value)
                    .unwrap_or(false),
            );
        }
        for (lane, value) in self.lanes.iter_mut().zip(values.iter()) {
            lane.push(*value);
            if lane.len() > Self::DEPTH {
                lane.remove(0);
            }
        }
        let word_names = if watch.is_empty() {
            words
                .iter()
                .filter(|word| word.name.contains('.'))
                .take(Self::WORD_CHANNELS)
                .map(|word| word.name.clone())
                .collect::<Vec<_>>()
        } else {
            watch
                .iter()
                .filter(|name| words.iter().any(|word| &word.name == *name))
                .take(Self::WORD_CHANNELS)
                .cloned()
                .collect::<Vec<_>>()
        };
        if word_names != self.word_channels {
            self.word_channels = word_names;
            self.word_lanes = vec![Vec::new(); self.word_channels.len()];
        }
        let members = self
            .word_channels
            .clone()
            .into_iter()
            .filter_map(|name| words.iter().find(|word| word.name == name).cloned());
        for (lane, word) in self.word_lanes.iter_mut().zip(members) {
            lane.push(word.value);
            if lane.len() > Self::DEPTH {
                lane.remove(0);
            }
        }
        self.samples = self.samples.saturating_add(1);
    }
}

/// The POU being renamed or copied, with the name typed so far.
#[derive(Debug, Clone)]
pub struct PouEdit {
    pub index: usize,
    pub duplicate: bool,
    pub name: String,
}

fn pous_len(project: &Project) -> usize {
    project.all_pous().len().saturating_sub(1)
}

pub struct PlcUi {
    pub project: Project,
    /// Project file, edited in the status card.
    pub path_text: String,
    /// Daemon socket, edited in the status card.
    pub socket_text: String,
    pub selection: Option<NodePath>,
    /// Buffers the property panel edits; they follow the selection.
    var_text: String,
    coil_text: String,
    comment_text: String,
    buffer_key: Option<NodePath>,
    link: Option<Link>,
    pub online: bool,
    /// Start the ladder once the daemon answers: `--plc` asks for a running
    /// service, not one that waits for START. A STOP from the operator is not
    /// undone, because this fires once.
    pub autostart: bool,
    autostarted: bool,
    /// Effective service state from the profile (or `LINC_PLC`). False means no
    /// daemon was started at all, which is not the same as a daemon gone away.
    pub enabled: bool,
    pub banner: String,
    pub state: RunState,
    pub daemon_project: String,
    pub hash: String,
    pub scan_count: u64,
    pub scan_time_us: u32,
    /// Scan, cycle and lateness metrics from the last snapshot.
    pub timing: linc_plc_core::timing::TimingSnapshot,
    pub overruns: u64,
    /// Daemon heartbeat health, not a pin level: true only while the ladder is
    /// running and not faulted. The pin values are in `vars`.
    pub alive: bool,
    pub fault: Option<String>,
    pub vars: Vec<VarValue>,
    pub forced: Vec<String>,
    pub armed: Option<String>,
    pub local_diags: Vec<Diagnostic>,
    pub daemon_diags: Vec<Diagnostic>,
    pub last_error: Option<String>,
    pub message: String,
    /// Which of the six pages is showing.
    pub page: PlcPage,
    /// Index into `project.all_pous()`.
    pub pou: usize,
    /// The editor's File > Close was chosen; the shell closes the overlay.
    pub close_requested: bool,
    /// Cell cursor in the ladder grid: section, rung, position (9 = coil).
    pub cell_cursor: Option<(usize, usize, usize)>,
    /// The element picker is open on this cell.
    pub picker: Option<(usize, usize, usize)>,
    /// The row the element picker's cursor sits on; arrows move it, Enter takes it.
    pub picker_cursor: usize,
    /// The operand list is open for the cell a kind was just chosen for.
    pub operand_pick: Option<(usize, usize, usize, Cell)>,
    /// Filter typed into the operand list; Enter takes the row under its cursor.
    pub operand_filter: String,
    /// The operand list's cursor, an index into the filtered rows.
    pub operand_cursor: usize,
    /// The coil operand is being picked for this row.
    pub coil_pick: Option<(usize, usize)>,
    /// A compare cell's condition and operands are being edited in one window.
    pub compare_edit: Option<CompareEdit>,
    /// A compute cell's operation, destination and operands are being edited.
    pub compute_edit: Option<ComputeEdit>,
    /// Set when the selection moved by a key, so the list scrolls to it once.
    pub scroll_to_pou: bool,
    /// Set when the cell cursor moved, so the ladder scrolls it into view once.
    pub scroll_to_cursor: bool,
    /// Enter or a double click on a row asks for the editor window.
    pub open_editor: bool,
    /// SAVE asks the shell for the save dialog; the file name is chosen there.
    pub save_requested: bool,
    /// Ctrl+Shift+S asks where to write a copy.
    pub save_as_requested: bool,
    /// A dialog owns the keys; the list behind it must not take them.
    pub keys_blocked: bool,
    /// The box the dialog is editing, and the two typed fields it holds.
    pub block_fields: Option<(usize, usize, usize)>,
    /// Which of the block type's four instances the window has selected.
    pub block_instance_cursor: usize,
    /// Where the window's keys go: the instance list or the typed preset.
    pub block_focus_preset: bool,
    /// The preset being typed, and why the last Enter refused it.
    pub block_preset_text: String,
    pub block_preset_error: Option<String>,
    /// Project snapshots for undo, oldest first, and the redo arm of the pair.
    pub undo_stack: Vec<Vec<Pou>>,
    pub redo_stack: Vec<Vec<Pou>>,
    /// The cell copied with Ctrl+C.
    pub cell_clipboard: Option<Cell>,
    /// Cells copied from a marked block, one row per rung, pasted at the cursor.
    pub cell_block: Vec<Vec<Cell>>,
    /// Mark start for a span copy: section, rung, position.
    pub cell_mark: Option<(usize, usize, usize)>,
    /// The operand whose cross references are shown.
    pub xref: Option<String>,
    /// A timer or counter box is being edited at this cell.
    pub block_edit: Option<(usize, usize, usize)>,
    /// The block at this grid cell is having its instance and preset edited.
    pub fbd_block_edit: Option<(usize, usize)>,
    /// The next FBD value chosen is negated (`!`).
    pub fbd_negate: bool,
    /// Cursor on the FBD grid: row and column.
    pub fbd_cursor: Option<(usize, usize)>,
    /// The grid element menu is open at this cell.
    pub fbd_grid_pick: Option<(usize, usize)>,
    /// An input or output operand is being picked for this cell.
    pub fbd_operand_pick: Option<(usize, usize, bool)>,
    /// A POU is being renamed or copied.
    pub pou_edit: Option<PouEdit>,
    /// The trend window in milliseconds: 4 s, 20 s or 100 s.
    /// The frame the workbench was opened on: Escape and a close request are
    /// ignored on that frame, because the click or chord that opened it can arrive
    /// with a stale Escape in the same input batch and shut it instantly.
    pub opened_frame: u64,
    /// True while one of the workbench menus is open: the menu owns the keyboard
    /// then, so the editors must not also take arrows, Enter or Escape.
    pub menu_open: bool,
    /// The simulation's selected input byte: 0 or 1, as the manual's `B` picks.
    pub sim_byte: u8,
    /// TEST RUN started the ladder on entry and must stop it on the way out.
    sim_running: bool,
    /// Input bits held only for drawing the ladder: the machine and the daemon
    /// never see them. This is the editor's preview, not TEST RUN.
    pub preview: Vec<(String, bool)>,
    pub trend_window_ms: u32,
    /// A property text field is being typed into, so the ladder keys stand down.
    /// Cleared by Escape, by a click on the canvas, or by leaving the field.
    pub text_editing: bool,
    /// Show operands by IEC address instead of symbol name.
    pub view_addresses: bool,
    /// The name typed into the symbols table's new row.
    pub symbol_new: String,
    /// Symbols table cursor, its column, and the row being renamed.
    pub symbol_cursor: usize,
    pub symbol_column: usize,
    pub symbol_edit_row: usize,
    pub symbol_edit: Option<String>,
    /// The comment row being typed and its buffer: comments are the table's
    /// fourth column, edited in place like the name.
    pub symbol_comment_row: usize,
    pub symbol_comment_edit: Option<String>,
    /// The comment row being typed into, with its text.
    pub comment_row: Option<(usize, usize)>,
    pub comment_buffer: String,
    /// Boolean traces for the trend page.
    pub trend: TrendBuffer,
    /// Per-scan window from the daemon, when one is connected.
    pub trend_remote: TrendData,
    /// The word bank and block members from the last snapshot.
    pub words: Vec<WordValue>,
    /// Buffer for the IL editor; synced when the selected POU changes.
    /// IL line editor: the line under the cursor and the text typed into it.
    pub il_cursor: usize,
    pub il_buffer: String,
    pub il_message: Option<String>,
    il_for: Option<usize>,
    next_poll: Instant,
}

impl Default for PlcUi {
    fn default() -> Self {
        Self {
            project: startup_project(),
            path_text: startup_path().display().to_string(),
            socket_text: linc_plc_client::default_socket_path().display().to_string(),
            selection: Some(NodePath::root(0, 0)),
            var_text: String::new(),
            coil_text: String::new(),
            comment_text: String::new(),
            buffer_key: None,
            link: None,
            online: false,
            enabled: false,
            banner: String::new(),
            state: RunState::Idle,
            daemon_project: String::new(),
            hash: String::new(),
            scan_count: 0,
            scan_time_us: 0,
            timing: linc_plc_core::timing::TimingSnapshot::default(),
            overruns: 0,
            alive: false,
            fault: None,
            vars: Vec::new(),
            forced: Vec::new(),
            armed: None,
            local_diags: Vec::new(),
            daemon_diags: Vec::new(),
            last_error: None,
            message: String::new(),
            page: PlcPage::PouList,
            pou: 0,
            close_requested: false,
            cell_cursor: None,
            picker: None,
            picker_cursor: 0,
            operand_pick: None,
            operand_filter: String::new(),
            operand_cursor: 0,
            coil_pick: None,
            compare_edit: None,
            compute_edit: None,
            scroll_to_pou: false,
            scroll_to_cursor: false,
            open_editor: false,
            save_requested: false,
            save_as_requested: false,
            keys_blocked: false,
            block_fields: None,
            block_instance_cursor: 0,
            block_focus_preset: false,
            block_preset_text: String::new(),
            block_preset_error: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cell_clipboard: None,
            cell_block: Vec::new(),
            cell_mark: None,
            xref: None,
            block_edit: None,
            fbd_block_edit: None,
            fbd_negate: false,
            fbd_cursor: None,
            fbd_grid_pick: None,
            fbd_operand_pick: None,
            pou_edit: None,
            opened_frame: 0,
            menu_open: false,
            sim_byte: 0,
            sim_running: false,
            preview: Vec::new(),
            trend_window_ms: 20_000,
            autostart: false,
            autostarted: false,
            text_editing: false,
            view_addresses: false,
            symbol_new: String::new(),
            symbol_cursor: 0,
            symbol_column: 0,
            symbol_edit_row: 0,
            symbol_edit: None,
            symbol_comment_row: 0,
            symbol_comment_edit: None,
            comment_row: None,
            comment_buffer: String::new(),
            trend: TrendBuffer::default(),
            trend_remote: TrendData::default(),
            words: Vec::new(),
            il_cursor: 0,
            il_buffer: String::new(),
            il_message: None,
            il_for: None,
            next_poll: Instant::now(),
        }
    }
}

impl PlcUi {
    /// The selected POU, cloned so the caller can hold a ladder body while the
    /// page keeps mutating the service state.
    /// Remember the project before an edit. Fifty steps is enough to undo a
    /// session's worth of work without holding every intermediate file.
    pub fn snapshot(&mut self) {
        self.undo_stack.push(self.project.all_pous());
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo_stack.pop() {
            self.redo_stack.push(self.project.all_pous());
            self.project.set_pous(previous);
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.project.all_pous());
            self.project.set_pous(next);
        }
    }

    /// The label to draw for an operand: the name, or its IEC address when
    /// View > Addresses is on. Words and block members keep their spelling.
    pub fn operand_label(&self, name: &str) -> String {
        if !self.view_addresses {
            return name.to_string();
        }
        if name.contains('.') || name.starts_with("MW") {
            return name.to_string();
        }
        let inputs = self
            .project
            .io
            .iter()
            .filter(|point| point.direction == IoDirection::In);
        for (index, point) in inputs.enumerate() {
            if point.name == name {
                return format!("%IX{}.{}", index / 8, index % 8);
            }
        }
        let outputs = self
            .project
            .io
            .iter()
            .filter(|point| point.direction == IoDirection::Out);
        for (index, point) in outputs.enumerate() {
            if point.name == name {
                return format!("%QX{}.{}", index / 8, index % 8);
            }
        }
        for (index, var) in self.project.vars.iter().enumerate() {
            if var.name == name {
                return format!("%MX{}.{}", index / 8, index % 8);
            }
        }
        name.to_string()
    }

    /// The comment declared for a symbol, if it has one.
    pub fn symbol_comment(&self, name: &str) -> Option<&str> {
        self.project
            .io
            .iter()
            .find(|point| point.name == name)
            .map(|point| point.comment.as_str())
            .or_else(|| {
                self.project
                    .vars
                    .iter()
                    .find(|var| var.name == name)
                    .map(|var| var.comment.as_str())
            })
            .filter(|comment| !comment.is_empty())
    }

    pub fn current_pou(&self) -> Option<Pou> {
        self.project.all_pous().into_iter().nth(self.pou)
    }

    pub fn current_pou_sections(&self) -> Vec<linc_plc_core::model::Section> {
        self.current_pou()
            .map(|pou| pou.sections)
            .unwrap_or_default()
    }

    pub fn pou_count(&self) -> usize {
        self.project.all_pous().len()
    }

    /// A new POU in `language`, selected and opened for editing. ZX-PLC's
    /// POU > New asks for the name and language; the name is generated here.
    pub fn add_pou(&mut self, language: PouLanguage) {
        let mut pous = self.project.all_pous();
        let name = format!("POU{}", pous.len() + 1);
        pous.push(Pou::new(name, language));
        self.pou = pous.len() - 1;
        self.project.set_pous(pous);
        self.page = PlcPage::Edit;
        self.selection = None;
    }

    pub fn set_language(&mut self, language: PouLanguage) {
        let mut pous = self.project.all_pous();
        if let Some(pou) = pous.get_mut(self.pou) {
            pou.language = language;
        }
        self.project.set_pous(pous);
    }

    /// Load the line under the cursor when the POU changed.
    pub fn sync_il_line(&mut self) {
        if self.il_for != Some(self.pou) {
            self.il_cursor = 0;
            self.il_buffer = self.il_lines().first().cloned().unwrap_or_default();
            self.il_message = None;
            self.il_for = Some(self.pou);
        }
    }

    pub fn il_lines(&self) -> Vec<String> {
        self.current_pou().map(|pou| pou.il).unwrap_or_default()
    }

    /// Write the line buffer into the program, keeping the cursor on it.
    pub fn il_store(&mut self, lines: Vec<String>) {
        self.snapshot();
        let mut pous = self.project.all_pous();
        if let Some(pou) = pous.get_mut(self.pou) {
            pou.il = lines;
            pou.language = PouLanguage::Il;
        }
        self.project.set_pous(pous);
    }

    /// Write the line buffer into `lines` (and into the program when asked).
    pub fn il_store_buffer(&mut self, lines: &mut Vec<String>, keep: bool) {
        if self.il_cursor < lines.len() {
            lines[self.il_cursor] = self.il_buffer.clone();
        } else if self.il_cursor == lines.len() {
            lines.push(self.il_buffer.clone());
        }
        if keep {
            self.il_store(lines.clone());
        }
    }

    /// Write a whole line list, used when a line is removed.
    pub fn il_store_lines(&mut self, lines: Vec<String>) {
        self.il_store(lines);
    }

    /// Check the edited line on its own, so an error keeps the cursor here.
    pub fn il_check(&self, line: &str) -> Option<String> {
        let mut lines = self.il_lines();
        if self.il_cursor < lines.len() {
            lines[self.il_cursor] = line.to_string();
        }
        let mut project = self.project.clone();
        let mut pous = project.all_pous();
        let Some(pou) = pous.get_mut(self.pou) else {
            return None;
        };
        pou.il = lines;
        pou.language = PouLanguage::Il;
        project.set_pous(pous);
        validate(&project)
            .into_iter()
            .find(|item| {
                item.severity == Severity::Error
                    && item
                        .path
                        .starts_with(&format!("pous[{}].il[{}]", self.pou, self.il_cursor))
            })
            .map(|item| item.message)
    }
    fn poll(&mut self) {
        if self.link.is_none() {
            self.dial();
        }
        let mut events = Vec::new();
        let mut dropped = false;
        if let Some(link) = &self.link {
            loop {
                match link.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        dropped = true;
                        break;
                    }
                }
            }
        }
        if dropped {
            self.link = None;
            self.online = false;
            self.alive = false;
        }
        for event in events {
            self.apply_event(event);
        }
        if self.autostart && !self.autostarted && self.link.is_some() && self.online {
            self.autostarted = true;
            self.send(
                Tag::Load,
                Request::Load {
                    project: self.project.clone(),
                },
            );
            self.send(Tag::Start, Request::Start);
            self.message = "STARTED WITH THE SERVICE".to_string();
        }
        let now = Instant::now();
        if self.link.is_some() && now >= self.next_poll {
            self.next_poll = now
                + if self.online {
                    POLL_CONNECTED
                } else {
                    POLL_OFFLINE
                };
            self.send(Tag::Snapshot, Request::Snapshot);
            // The trend page needs the daemon's per-scan window, not the frame
            // sampling the page can do on its own.
            if self.page == PlcPage::Trend || !self.trend_remote.samples.is_empty() {
                self.send(
                    Tag::Trend,
                    Request::Trend {
                        window_ms: self.trend_window_ms,
                    },
                );
            }
        }
    }

    fn dial(&mut self) {
        if self.socket_text.trim().is_empty() {
            self.socket_text = linc_plc_client::default_socket_path().display().to_string();
        }
        let path = PathBuf::from(self.socket_text.trim());
        self.link = Some(spawn_link(path));
        self.next_poll = Instant::now();
        self.send(
            Tag::Hello,
            Request::Hello {
                protocol: PROTOCOL_VERSION,
            },
        );
    }

    fn send(&mut self, tag: Tag, request: Request) {
        if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
            eprintln!("send {tag:?} link={}", self.link.is_some());
        }
        let Some(link) = &self.link else {
            return;
        };
        if link.jobs.send(Job::Request { tag, request }).is_err() {
            self.link = None;
            self.online = false;
            self.last_error = Some("PLC worker stopped".to_string());
        }
    }

    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Online => {
                self.online = true;
                self.last_error = None;
            }
            Event::Offline(error) => {
                self.online = false;
                self.alive = false;
                self.last_error = Some(error);
            }
            Event::Reply { tag, result } => match result {
                Err(error) => {
                    self.last_error = Some(error);
                }
                Ok(response) => self.apply_response(tag, response),
            },
        }
    }

    fn apply_response(&mut self, tag: Tag, response: Response) {
        if std::env::var_os("LINC_PLC_SNAP_DEBUG").is_some() {
            match (&tag, &response) {
                (
                    Tag::Hello,
                    Response::Hello {
                        protocol, schema, ..
                    },
                ) => {
                    eprintln!(
                        "snap hello protocol={protocol} schema={schema} want={PROTOCOL_VERSION}"
                    );
                }
                (tag, Response::State(state)) => {
                    eprintln!(
                        "snap state tag={tag:?} run={:?} scan={} vars={} words={}",
                        state.state,
                        state.scan_count,
                        state.vars.len(),
                        state.words.len()
                    );
                }
                (tag, Response::Error { code, message }) => {
                    eprintln!("snap error tag={tag:?} {code}: {message}");
                }
                _ => {}
            }
        }
        if let (Tag::Trend, Response::Trend(data)) = (tag, &response) {
            self.trend_remote = data.clone();
            return;
        }
        match (tag, response) {
            (
                Tag::Hello,
                Response::Hello {
                    protocol,
                    schema,
                    daemon,
                    hal,
                },
            ) => {
                self.banner = format!(
                    "{daemon} · schema {schema} · {}",
                    if hal { "HAL OUTPUTS" } else { "NO HAL" }
                );
                if protocol != PROTOCOL_VERSION {
                    self.last_error = Some(format!(
                        "daemon speaks protocol {protocol}, this HMI speaks {PROTOCOL_VERSION}"
                    ));
                } else {
                    self.message = "CONNECTED".to_string();
                }
            }
            (Tag::Validate, Response::Validated { ok, diagnostics }) => {
                self.daemon_diags = diagnostics;
                self.message = if ok {
                    "DAEMON SAYS VALID".to_string()
                } else {
                    "DAEMON SAYS INVALID".to_string()
                };
            }
            (Tag::Load, Response::Loaded { hash, diagnostics }) => {
                self.hash = hash.clone();
                self.daemon_diags = diagnostics;
                self.message = if hash.is_empty() {
                    "DOWNLOADED".to_string()
                } else {
                    format!("DOWNLOADED {hash}")
                };
            }
            (_, Response::State(snapshot)) => self.adopt(snapshot),
            (Tag::Start, _) => self.message = "START SENT".to_string(),
            (Tag::Stop, _) => self.message = "STOP SENT".to_string(),
            (_, Response::Error { code, message }) => {
                self.last_error = Some(format!("{code}: {message}"));
            }
            (_, Response::Bye) => {
                self.online = false;
                self.message = "DAEMON SAID GOODBYE".to_string();
            }
            (_, other) => {
                self.last_error = Some(format!("unexpected PLC reply: {other:?}"));
            }
        }
    }

    fn adopt(&mut self, snapshot: StateSnapshot) {
        self.state = snapshot.state;
        self.daemon_project = snapshot.project;
        self.hash = snapshot.hash;
        self.scan_count = snapshot.scan_count;
        self.scan_time_us = snapshot.scan_time_us;
        self.timing = snapshot.timing;
        self.overruns = snapshot.overruns;
        self.alive = snapshot.alive;
        self.fault = snapshot.fault;
        self.vars = snapshot.vars;
        self.words = snapshot.words;
        // One sample per received snapshot, so the traces follow the scan.
        self.trend
            .sample(&self.vars, &self.words, &self.project.watch);
        self.forced = snapshot.forced;
    }

    pub fn live_value(&self, name: &str) -> Option<bool> {
        self.vars
            .iter()
            .find(|value| value.name == name)
            .map(|value| value.value)
    }

    /// The live value of a bit operand, if the snapshot carries it.
    pub fn bit_value(&self, name: &str) -> Option<bool> {
        if let Some((_, value)) = self.preview.iter().find(|(held, _)| held == name) {
            return Some(*value);
        }
        self.vars
            .iter()
            .find(|var| var.name == name)
            .map(|var| var.value)
    }

    /// The live value of a word operand (`MW3`, `TON0.ET`, `CTU1.CV`).
    pub fn word_value(&self, name: &str) -> Option<i16> {
        self.words
            .iter()
            .find(|word| word.name == name)
            .map(|word| word.value)
    }

    pub fn is_forced(&self, name: &str) -> bool {
        self.forced.iter().any(|forced| forced == name)
    }

    fn selected_section(&self) -> usize {
        self.selection
            .as_ref()
            .map(|node| node.section)
            .unwrap_or(0)
    }

    fn sync_buffers(&mut self) {
        let selection = self.selection.clone();
        self.buffer_key = selection.clone();
        let Some(node) = selection else {
            self.var_text.clear();
            self.coil_text.clear();
            self.comment_text.clear();
            return;
        };
        self.var_text = match logic_at(&self.project, &node) {
            Some(Logic::Contact { var, .. }) => var.clone(),
            _ => String::new(),
        };
        self.coil_text = rung_at(&self.project, &node)
            .map(|rung| rung.coil.var.clone())
            .unwrap_or_default();
        self.comment_text = rung_at(&self.project, &node)
            .map(|rung| rung.comment.clone())
            .unwrap_or_default();
    }

    fn normalise_selection(&mut self) {
        let Some(node) = self.selection.clone() else {
            return;
        };
        let Some(section) = self.project.sections.get(node.section) else {
            self.selection = None;
            self.sync_buffers();
            return;
        };
        if node.rung >= section.rungs.len() {
            self.selection = None;
            self.sync_buffers();
            return;
        }
        if logic_at(&self.project, &node).is_none() {
            self.selection = Some(NodePath::root(node.section, node.rung));
        }
        self.sync_buffers();
    }

    pub fn apply(&mut self, action: Action) {
        if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
            eprintln!("apply {:?}", action);
        }
        // Everything that changes the program is undoable; selection, runtime
        // control and forces are not part of the project.
        if action.edits_project() {
            self.snapshot();
        }
        match action {
            Action::Select(node) => {
                self.selection = Some(node);
                self.sync_buffers();
            }
            Action::ToggleContact(node) => {
                toggle_negated(&mut self.project, &node);
                self.sync_buffers();
            }
            Action::ToggleConstant(node) => {
                toggle_constant(&mut self.project, &node);
            }
            Action::SetVariable(text) => {
                if let Some(node) = self.selection.clone() {
                    if let Some(Logic::Contact { var, .. }) = logic_at_mut(&mut self.project, &node)
                    {
                        *var = text.trim().to_string();
                    }
                }
            }
            Action::SetCoilVariable(text) => {
                if let Some(node) = self.selection.clone() {
                    if let Some(rung) = rung_at_mut(&mut self.project, &node) {
                        rung.coil.var = text.trim().to_string();
                    }
                }
            }
            Action::SetCoilKind(kind) => {
                if let Some(node) = self.selection.clone() {
                    if let Some(rung) = rung_at_mut(&mut self.project, &node) {
                        rung.coil.kind = kind;
                    }
                }
            }
            Action::SetComment(text) => {
                if let Some(node) = self.selection.clone() {
                    if let Some(rung) = rung_at_mut(&mut self.project, &node) {
                        rung.comment = text;
                    }
                }
            }
            Action::WrapSeries => {
                if let Some(node) = self.selection.clone() {
                    wrap_series(&mut self.project, &node);
                    self.normalise_selection();
                }
            }
            Action::WrapParallel => {
                if let Some(node) = self.selection.clone() {
                    wrap_parallel(&mut self.project, &node);
                    self.normalise_selection();
                }
            }
            Action::Delete => self.delete_selection(),
            Action::OpenPou(index) => {
                let count = self.project.all_pous().len();
                if index >= count {
                    // A stale index must not create a POU that does not exist.
                    self.pou = count.saturating_sub(1);
                    return;
                }
                self.pou = index;
                self.page = PlcPage::Edit;
                self.selection = None;
                self.il_for = None;
            }
            Action::AddPou(language) => self.add_pou(language),
            Action::SetPouLanguage { index, language } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(index) {
                    pou.language = language;
                }
                self.project.set_pous(pous);
            }
            Action::AddRungAfter { section, row } => {
                let input = default_input_var(&self.project);
                let output = default_output_var(&self.project);
                let mut pous = self.project.all_pous();
                let mut made = None;
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(target) = pou.sections.get_mut(section) {
                        let at = (row + 1).min(target.rungs.len());
                        target.rungs.insert(
                            at,
                            Rung {
                                cells: Vec::new(),
                                comment: String::new(),
                                logic: Logic::Contact {
                                    var: input,
                                    negated: false,
                                },
                                coil: Coil::new(output, CoilKind::Normal),
                            },
                        );
                        made = Some(at);
                    }
                }
                self.project.set_pous(pous);
                if let Some(at) = made {
                    self.selection = Some(NodePath::root(section, at));
                }
                self.normalise_selection();
            }
            Action::DeleteRung { section, rung } => {
                // A row directly under a Link is that link's parallel branch;
                // deleting it would leave the link joined to an unrelated row.
                let mut pous = self.project.all_pous();
                let mut refused = false;
                if let Some(pou) = pous.get_mut(self.pou) {
                    let linked = rung > 0
                        && pou
                            .sections
                            .get(section)
                            .and_then(|target| target.rungs.get(rung - 1))
                            .map(|above| above.cells.iter().any(|cell| matches!(cell, Cell::Link)))
                            .unwrap_or(false);
                    if linked {
                        refused = true;
                    } else if let Some(target) = pou.sections.get_mut(section) {
                        if rung < target.rungs.len() {
                            target.rungs.remove(rung);
                        }
                    }
                }
                if !refused {
                    self.project.set_pous(pous);
                    self.normalise_selection();
                }
            }
            Action::AddRung => {
                // The POU list is canonical: the compiler reads it. `set_pous`
                // folds a single MAIN ladder back into the legacy `sections`, so
                // both representations and the compiler see the new row.
                let after = self.selection.as_ref().map(|node| node.rung);
                let section = self.selected_section();
                let input = default_input_var(&self.project);
                let output = default_output_var(&self.project);
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(target) = pou.sections.get_mut(section) {
                        let at = match after {
                            Some(index) if index < target.rungs.len() => index + 1,
                            _ => target.rungs.len(),
                        };
                        target.rungs.insert(
                            at,
                            Rung {
                                cells: Vec::new(),
                                comment: String::new(),
                                logic: Logic::Contact {
                                    var: input,
                                    negated: false,
                                },
                                coil: Coil::new(output, CoilKind::Normal),
                            },
                        );
                        self.selection = Some(NodePath::root(section, at));
                    }
                }
                self.project.set_pous(pous);
                self.normalise_selection();
            }
            Action::AddSection => {
                let index = add_section(&mut self.project);
                self.selection = Some(NodePath::root(index, 0));
                self.normalise_selection();
            }
            Action::MoveUp => self.move_selected(-1),
            Action::MoveDown => self.move_selected(1),
            Action::Connect => {
                self.link = None;
                self.online = false;
                self.next_poll = Instant::now();
                self.dial();
            }
            Action::New => {
                if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
                    eprintln!("apply Action::New");
                }
                // A new project starts empty: the manual's File > New gives an empty
                // MAIN, and the demo is a separate concern.
                self.project = Project::new("untitled");
                // A new project has exactly one POU: the cursor index must not
                // survive from the previous project, or the page shows a POU that
                // does not exist (blank header, empty body).
                self.pou = 0;
                self.il_for = None;
                self.il_cursor = 0;
                self.il_buffer.clear();
                self.selection = Some(NodePath::root(0, 0));
                self.daemon_diags.clear();
                self.message = "NEW PROJECT".to_string();
                self.normalise_selection();
            }
            Action::Open => {
                if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
                    eprintln!("apply Action::Open path={}", self.path_text);
                }
                self.open()
            }
            Action::Save => self.save(),
            Action::Validate => self.validate(),
            Action::Download => self.download(),
            Action::Start => {
                if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
                    eprintln!("apply Start online={} state={:?}", self.online, self.state);
                }
                // A bare Start is refused unless the daemon is Loaded, so a start
                // from the manual always loads first, in this order on the same
                // connection.
                self.download();
                self.send(Tag::Start, Request::Start);
                self.message = "LOAD + START SENT".to_string();
            }
            Action::RunProject => {
                self.download();
                self.send(Tag::Start, Request::Start);
                self.message = "LOAD + START SENT".to_string();
            }
            Action::Stop => {
                self.send(Tag::Stop, Request::Stop);
                self.message = "STOP SENT".to_string();
            }
            Action::Unforce(name) => {
                self.send(Tag::Force, Request::Unforce { name });
            }
            Action::ClearForces => {
                self.send(Tag::ClearForces, Request::ClearForces);
                self.message = "CLEAR FORCES SENT".to_string();
            }
            Action::SetRungComment {
                section,
                rung,
                comment,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        row.comment = comment;
                    }
                }
                self.project.set_pous(pous);
            }
            Action::AddCommentRow { section, rung } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(section) = pou.sections.get_mut(section) {
                        if rung < section.rungs.len() {
                            let mut row = Rung::new(
                                Logic::Const { value: true },
                                Coil::new(String::new(), CoilKind::Normal),
                            );
                            row.cells = vec![Cell::Wire; POSITIONS];
                            row.comment = "// ".to_string();
                            section.rungs.insert(rung + 1, row);
                        }
                    }
                }
                self.project.set_pous(pous);
                self.comment_row = Some((section, rung + 1));
                self.comment_buffer = String::new();
            }
            Action::SetBlockField {
                section,
                rung,
                position,
                instance,
                preset,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(cell) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                        .and_then(|row| row.cells.get_mut(position))
                    {
                        if let Cell::Block {
                            instance: current_instance,
                            preset: current_preset,
                            ..
                        } = cell
                        {
                            if let Some(name) = instance {
                                *current_instance = name;
                            }
                            if let Some(value) = preset {
                                *current_preset = value;
                            }
                        }
                    }
                }
                self.project.set_pous(pous);
            }
            Action::SetCoilVar { section, rung, var } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        row.coil.var = var;
                    }
                }
                self.project.set_pous(pous);
            }
            Action::AddInput(name) => {
                if !self.project.io.iter().any(|point| point.name == name)
                    && !self.project.vars.iter().any(|var| var.name == name)
                {
                    self.project
                        .io
                        .push(linc_plc_core::IoPoint::new(name, IoDirection::In));
                }
            }
            Action::AddSymbol(name) => {
                if !self.project.io.iter().any(|point| point.name == name)
                    && !self.project.vars.iter().any(|var| var.name == name)
                {
                    self.project
                        .io
                        .push(linc_plc_core::IoPoint::new(name, IoDirection::Out));
                }
            }
            Action::AddVar(name) => {
                if !self.project.io.iter().any(|point| point.name == name)
                    && !self.project.vars.iter().any(|var| var.name == name)
                {
                    self.project.vars.push(linc_plc_core::model::VarDecl {
                        name,
                        kind: linc_plc_core::model::VarKind::Bit,
                        retain: false,
                        comment: String::new(),
                    });
                }
            }
            Action::RemoveSymbol(name) => {
                self.project.io.retain(|point| point.name != name);
                self.project.vars.retain(|var| var.name != name);
                self.project.watch.retain(|operand| operand != &name);
            }
            Action::SetSymbolComment(name, comment) => {
                for point in self.project.io.iter_mut() {
                    if point.name == name {
                        point.comment = comment.clone();
                    }
                }
                for var in self.project.vars.iter_mut() {
                    if var.name == name {
                        var.comment = comment.clone();
                    }
                }
            }
            Action::RenameSymbol { from, to } => {
                let to = to.trim().to_string();
                if !to.is_empty() && to != from {
                    // The symbol and every reference to it move together; a table
                    // that renamed only the row would leave the ladder pointing at
                    // a name that no longer exists.
                    for point in self.project.io.iter_mut() {
                        if point.name == from {
                            point.name = to.clone();
                        }
                    }
                    for var in self.project.vars.iter_mut() {
                        if var.name == from {
                            var.name = to.clone();
                        }
                    }
                    for operand in self.project.watch.iter_mut() {
                        if *operand == from {
                            *operand = to.clone();
                        }
                    }
                    let mut pous = self.project.all_pous();
                    for pou in pous.iter_mut() {
                        for section in pou.sections.iter_mut() {
                            for rung in section.rungs.iter_mut() {
                                if rung.coil.var == from {
                                    rung.coil.var = to.clone();
                                }
                                for cell in rung.cells.iter_mut() {
                                    match cell {
                                        Cell::Contact { var, .. } | Cell::Edge { var, .. }
                                            if *var == from =>
                                        {
                                            *var = to.clone();
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    self.project.set_pous(pous);
                }
            }
            Action::AddRowAfter { section, rung } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(section) = pou.sections.get_mut(section) {
                        if rung < section.rungs.len() {
                            let mut row = Rung::new(
                                Logic::Const { value: true },
                                Coil::new(String::new(), CoilKind::Normal),
                            );
                            row.cells = vec![Cell::Wire; POSITIONS];
                            section.rungs.insert(rung + 1, row);
                        }
                    }
                }
                self.project.set_pous(pous);
            }
            Action::DeleteCell {
                section,
                rung,
                position,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        let migrated = if row.cells.is_empty() {
                            linc_plc_core::grid::migrate(&row.logic)
                        } else {
                            Some(row.cells.clone())
                        };
                        if let Some(mut cells) = migrated {
                            cells.resize(POSITIONS, Cell::Wire);
                            if position < POSITIONS {
                                cells.remove(position);
                                cells.push(Cell::Wire);
                                row.cells = cells;
                            }
                        }
                    }
                }
                self.project.set_pous(pous);
            }
            Action::InsertCell {
                section,
                rung,
                position,
                cell,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        let migrated = if row.cells.is_empty() {
                            linc_plc_core::grid::migrate(&row.logic)
                        } else {
                            Some(row.cells.clone())
                        };
                        if let Some(mut cells) = migrated {
                            cells.resize(POSITIONS, Cell::Wire);
                            if position < POSITIONS {
                                cells.pop();
                                cells.insert(position, cell);
                                row.cells = cells;
                            }
                        }
                    }
                }
                self.project.set_pous(pous);
            }
            Action::RenamePou { index, name } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(index) {
                    pou.name = name;
                }
                self.project.set_pous(pous);
            }
            Action::DuplicatePou { index, name } => {
                let mut pous = self.project.all_pous();
                if let Some(source) = pous.get(index).cloned() {
                    let mut copy = source;
                    copy.name = name;
                    pous.insert(index + 1, copy);
                }
                self.project.set_pous(pous);
            }
            Action::DeletePou { index } => {
                let mut pous = self.project.all_pous();
                if pous
                    .get(index)
                    .map(|pou| !pou.name.eq_ignore_ascii_case("MAIN"))
                    .unwrap_or(false)
                {
                    pous.remove(index);
                    self.project.set_pous(pous);
                    self.pou = self.pou.min(pous_len(&self.project));
                }
            }
            Action::SetFbdGrid(grid) => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    pou.fbd_grid = Some(grid);
                }
                self.project.set_pous(pous);
            }
            Action::SetFbd(lines) => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    pou.fbd = lines;
                }
                self.project.set_pous(pous);
            }
            Action::ToggleWatch(name) => {
                if let Some(index) = self
                    .project
                    .watch
                    .iter()
                    .position(|operand| operand.eq_ignore_ascii_case(&name))
                {
                    self.project.watch.remove(index);
                } else if self.project.watch.len() < 16 {
                    self.project.watch.push(name);
                }
            }
            Action::SetCell {
                section,
                rung,
                position,
                cell,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        // A version-2 rung converts to cells only when it is a
                        // flat series; a parallel branch keeps its tree instead
                        // of being silently flattened into different logic.
                        let migrated = if row.cells.is_empty() {
                            linc_plc_core::grid::migrate(&row.logic)
                        } else {
                            Some(row.cells.clone())
                        };
                        if let Some(mut cells) = migrated {
                            if position < POSITIONS {
                                cells.resize(POSITIONS, Cell::Wire);
                                // ZX asks for a box's instance and preset as it
                                // is placed, so the window opens from the write:
                                // opening it from the editor ran a frame before
                                // the cell existed and was then cleared.
                                if matches!(cell, Cell::Block { .. }) {
                                    self.block_edit = Some((section, rung, position));
                                }
                                cells[position] = cell;
                                row.cells = cells;
                            }
                        }
                    }
                }
                self.project.set_pous(pous);
            }
            Action::SetCoil {
                section,
                rung,
                coil,
            } => {
                let mut pous = self.project.all_pous();
                if let Some(pou) = pous.get_mut(self.pou) {
                    if let Some(row) = pou
                        .sections
                        .get_mut(section)
                        .and_then(|section| section.rungs.get_mut(rung))
                    {
                        row.coil = coil;
                    }
                }
                self.project.set_pous(pous);
            }
            Action::Force(name, value) => {
                self.send(
                    Tag::Force,
                    Request::Force {
                        name,
                        value,
                        ttl_ms: FORCE_TTL_MS,
                    },
                );
            }
            Action::Arm(name) => {
                self.armed = if self.armed.as_deref() == Some(name.as_str()) {
                    None
                } else {
                    Some(name)
                };
            }
        }
    }

    fn move_selected(&mut self, delta: isize) {
        if let Some(node) = self.selection.clone() {
            if let Some(rung) = move_rung(&mut self.project, node.section, node.rung, delta) {
                self.selection = Some(NodePath::root(node.section, rung));
            }
        }
        self.normalise_selection();
    }

    fn delete_selection(&mut self) {
        let Some(node) = self.selection.clone() else {
            return;
        };
        if delete_node(&mut self.project, &node) {
            let last = self
                .project
                .sections
                .get(node.section)
                .and_then(|section| section.rungs.len().checked_sub(1));
            self.selection = node
                .parent()
                .or_else(|| last.map(|rung| NodePath::root(node.section, rung)));
        }
        self.normalise_selection();
    }

    pub fn open(&mut self) {
        match std::fs::read_to_string(self.path_text.trim()) {
            Ok(text) => match project_from_json(&text) {
                Ok(mut project) => {
                    // Lay any tree rung out as cells on the way in, so the
                    // editor has one model from the first frame.
                    project.normalise();
                    self.project = project;
                    // The opened project decides the POU set: any index from the
                    // previous one is stale.
                    self.pou = 0;
                    self.il_for = None;
                    self.il_cursor = 0;
                    self.il_buffer.clear();
                    self.selection = Some(NodePath::root(0, 0));
                    self.daemon_diags.clear();
                    self.local_diags.clear();
                    self.message = "OPENED".to_string();
                    self.normalise_selection();
                }
                Err(error) => self.last_error = Some(format!("OPEN: {error}")),
            },
            Err(error) => self.last_error = Some(format!("OPEN: {error}")),
        }
    }

    pub fn save(&mut self) {
        if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
            eprintln!(
                "save: pous={} all_pous={} path={} says={:?}",
                self.project.pous.len(),
                self.project.all_pous().len(),
                self.path_text,
                self.project
                    .all_pous()
                    .iter()
                    .map(|pou| (
                        pou.name.clone(),
                        format!("{:?}", pou.language),
                        pou.il.len()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        // Canonical before writing: the daemon's Load and the file must agree with
        // what the editor shows.
        let pous = self.project.all_pous();
        self.project.set_pous(pous);
        self.project.normalise();
        match project_to_json(&self.project) {
            Ok(text) => match std::fs::write(self.path_text.trim(), text) {
                Ok(()) => self.message = "SAVED".to_string(),
                Err(error) => self.last_error = Some(format!("SAVE: {error}")),
            },
            Err(error) => self.last_error = Some(format!("SAVE: {error}")),
        }
    }

    pub fn validate(&mut self) {
        // Local validation always runs: it is the same library the daemon
        // links, so the page stays truthful with no daemon at all.
        self.local_diags = validate(&self.project);
        if self.online {
            self.send(
                Tag::Validate,
                Request::Validate {
                    project: self.project.clone(),
                },
            );
            self.message = "VALIDATE SENT".to_string();
        } else {
            self.message = "VALIDATED OFFLINE".to_string();
        }
    }

    pub fn download(&mut self) {
        self.validate();
        // The daemon stores what it loads; the file keeps the operator's copy
        // beside it so a restart finds the same project.
        self.save();
        self.send(
            Tag::Load,
            Request::Load {
                project: self.project.clone(),
            },
        );
        self.message = if !self.online {
            "SAVED — NO DAEMON".to_string()
        } else if matches!(self.state, RunState::Running) {
            // Load leaves the daemon in Loaded with every output false.
            "LOADED — LADDER STOPPED, PRESS START".to_string()
        } else {
            "LOAD SENT".to_string()
        };
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// The page the PLC application shows, as ZX-PLC's VIEW menu chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlcPage {
    PouList,
    Edit,
    Symbols,
    Sim,
    Watch,
    Trend,
    Messages,
    Service,
}

/// The page's status bar height, below the content.
const STATUS_BAR_HEIGHT: f32 = 22.0;

/// The PLC page is the launcher. ZX-PLC is a keyboard application, so the
/// workbench itself is a popped-out overlay that owns every key; the page keeps
/// the project, the service and the way in.

/// True when `key` was pressed with Ctrl and Shift held, consumed once. `ctrl_key`
/// matches modifiers exactly, so a shifted chord needs its own spelling.
fn ctrl_shift_key(ui: &Ui, key: egui::Key) -> bool {
    ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, key)
            || input.consume_key(
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                key,
            )
    })
}

/// True when `key` was pressed with Ctrl held, consumed so it is applied once.
fn ctrl_key(ui: &Ui, key: egui::Key) -> bool {
    // Consume, so one press is handled once: reading the event list let the same
    // chord be applied twice and added two rows per Ctrl+N. egui-winit reports
    // Ctrl as CTRL|COMMAND on Linux, so both spellings are consumed.
    ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::CTRL, key)
            || input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::COMMAND, key)
    })
}

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
        ui.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } = event
                {
                    eprintln!("raw key {key:?} pressed={pressed} mods={modifiers:?}");
                }
            }
        });
    }
    app.plc.poll();
    // A dialog owns the keyboard, including focus: the page's anchor took it
    // back every frame, so the save prompt's name field could not be typed in.
    if app.overlay.modal.is_none() {
        common::focus_anchor(ui, "plc-page-focus");
    }

    let mut actions: Vec<Action> = Vec::new();
    let _open_editor = false;
    let new_project = false;
    let mut open_frame: Option<u64> = None;
    // Ctrl+E opens the workbench from the keyboard, so the acceptance does not
    // depend on the pointer landing inside the button.
    // The service page is still drawn behind the workbench, so its keys must not
    // fire while the workbench owns the keyboard: plain `N` there opened the NEW
    // PROJECT? dialog over the IL editor and swallowed the next Enter.
    let workbench_open = app.overlay.plc_editor;
    // The mode switch needs keys: the buttons sit on the service pane below the
    // list, and the panel keyboard has no Tab to reach them with.
    if !workbench_open {
        if app.overlay.modal.is_none() && ctrl_shift_key(ui, egui::Key::Enter) {
            actions.push(Action::Stop);
        } else if app.overlay.modal.is_none() && ctrl_key(ui, egui::Key::Enter) {
            actions.push(Action::Start);
        }
    }
    // The rail advertises Ctrl+S on this screen, so the screen answers it: the
    // program list's Save asks for the same dialog the editor does.
    if !workbench_open {
        if ctrl_shift_key(ui, egui::Key::S) {
            app.plc.save_as_requested = true;
        } else if ctrl_key(ui, egui::Key::S) {
            app.plc.save_requested = true;
        }
    }
    if !workbench_open
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::E))
    {
        open_frame = Some(ui.ctx().cumulative_frame_nr());
    }
    // Anything on top of the list — a dialog or the editor window — owns Enter,
    // the arrows and the letters. Without this the list ate the Enter that should
    // have confirmed the save dialog, and every arrow aimed at the ladder.
    let dialog_open = app.overlay.modal.is_some() || workbench_open || app.plc.pou_edit.is_some();
    let plc = &mut app.plc;
    plc.keys_blocked = dialog_open;
    // The list is this screen's page, but only while it is the page in front:
    // the editor overlay draws the symbols, simulation, watch and trend pages,
    // and a list drawn behind it used to reset the page to the list every frame,
    // which made every page chord look dead.
    if !workbench_open {
        plc.page = PlcPage::PouList;
    }

    let width = ui.available_width();
    // The program list sits over the service pane: the list is what the operator
    // opens a body from, and the service pane is the machine it talks to. Side by
    // side, the service fields were squeezed to half the screen and truncated.
    let list_height = (ui.available_height() * 0.45).clamp(180.0, 300.0);
    ui.allocate_ui_with_layout(
        vec2(width, list_height),
        egui::Layout::top_down(Align::Min),
        |ui| pou_list_page(plc, ui, list_height, &mut actions),
    );
    ui.add_space(6.0);
    let pane_height = (ui.available_height() - 4.0).max(190.0);
    card(ui, vec2(width, pane_height), |ui, _| {
        status_card(plc, ui, &mut actions)
    });

    // The list's rename and duplicate windows belong to this screen: drawing
    // them only in the workbench left the list's copies invisible and its keys
    // blocked for as long as the state stayed set.
    pou_edit_dialog(plc, ui.ctx(), &mut actions);

    for action in actions {
        plc.apply(action);
    }
    // Enter on a row opens the body in the editor window. The action above has
    // already selected the POU and moved the page to Edit.
    let open_from_list = plc.open_editor;
    plc.open_editor = false;
    if open_from_list {
        open_frame = Some(ui.ctx().cumulative_frame_nr());
    }
    if app.plc.save_requested {
        app.plc.save_requested = false;
        app.request_plc_save();
    }
    if app.plc.save_as_requested {
        app.plc.save_as_requested = false;
        app.request_plc_save_as();
    }
    if new_project {
        app.plc.apply(Action::New);
    }
    if let Some(frame) = open_frame {
        app.overlay.plc_editor = true;
        app.plc.opened_frame = frame;
        // The editor window edits a program; the program list stays on this screen.
        app.plc.page = PlcPage::Edit;
        app.plc.picker = None;
        app.plc.operand_pick = None;
        app.plc.coil_pick = None;
    }
}

/// The popped-out workbench: menu bar, pages, status bar, keyboard owner.
/// Whether Escape should leave the workbench. A picker or block window inside it
/// owns Escape first, so closing the editor while one is open would strand that
/// state with no window and make the editor deaf.
pub fn escape_closes_editor(plc: &PlcUi) -> bool {
    plc.picker.is_none()
        && plc.operand_pick.is_none()
        && plc.coil_pick.is_none()
        && plc.block_edit.is_none()
        && plc.compare_edit.is_none()
        && plc.compute_edit.is_none()
        && plc.xref.is_none()
}

/// The PLC workbench's navigation strip, built with the main GUI's row metrics:
/// a fixed row height, a 2 px gap inside a group and a wider gap before each group
/// heading. Loose `ui.horizontal` spacing is exactly what made it read as a list of
/// floating labels.
/// Row metrics of the shortcut rail. Every gap is explicit: the rail zeroes egui's
/// own `item_spacing.y`, which otherwise added the default vertical spacing on top of
/// `NAV_ROW_GAP` and left empty-looking lines between rows.
pub const RAIL_INNER_GAP: f32 = theme::NAV_ROW_GAP;
pub const RAIL_GROUP_GAP: f32 = 8.0;

enum RailEntry {
    Heading(&'static str),
    Row {
        text: &'static str,
        key: &'static str,
        active: bool,
    },
}

/// The rail's rows as data, so they can be laid out in one column or two.
fn rail_entries(plc: &PlcUi) -> Vec<RailEntry> {
    use RailEntry::{Heading, Row};
    let row = |text: &'static str, key: &'static str, active: bool| Row { text, key, active };
    let mut entries = vec![
        Heading("PAGES"),
        row("Ladder", "Ctrl+1", plc.page == PlcPage::Edit),
        row("Symbols", "Ctrl+2", plc.page == PlcPage::Symbols),
        row("Test run", "Ctrl+3", plc.page == PlcPage::Sim),
        row("Watch", "Ctrl+4", plc.page == PlcPage::Watch),
        row("Trend", "Ctrl+5", plc.page == PlcPage::Trend),
    ];
    match plc.page {
        // The list has its own keys; the editor's element letters do not work
        // here, and listing them made the pane a lie.
        PlcPage::PouList => {
            entries.push(Heading("PROGRAM LIST"));
            entries.push(row("Open", "Enter", false));
            entries.push(row("New POU", "N", false));
            entries.push(row("Rename", "R", false));
            entries.push(row("Duplicate", "D", false));
            entries.push(row("Delete POU", "Bksp", false));
            entries.push(Heading("RUN"));
            entries.push(row("Start", "Ctrl+Enter", false));
            entries.push(row("Stop", "Ctrl+Shift+Enter", false));
            entries.push(Heading("SAVE"));
            entries.push(row("Save project", "Ctrl+S", false));
        }
        PlcPage::Edit => {
            entries.push(Heading("ELEMENTS"));
            entries.push(row("Contact", "C", false));
            entries.push(row("Negated contact", "X", false));
            entries.push(row("Rising / falling edge", "P N", false));
            entries.push(row("Wire / link", "W V", false));
            entries.push(row("Timer / counter", "T F U K D", false));
            entries.push(row("Compare / compute", "G M", false));
            entries.push(row("Insert before", "I", false));
            entries.push(row("Delete cell", "Bksp", false));
            entries.push(Heading("COIL"));
            entries.push(row("Normal / set / reset", "C S R", false));
            entries.push(Heading("RUNG"));
            entries.push(row("New row", "Ctrl+N", false));
            entries.push(row("Insert row", "Ctrl+Enter", false));
            entries.push(row("Delete row", "Ctrl+Bksp", false));
            entries.push(Heading("PREVIEW"));
            entries.push(row("Hold a contact", "Space", false));
            entries.push(row("Drawing only", "no HAL", false));
            entries.push(Heading("TOOLS"));
            entries.push(row("Cross references", "Ctrl+X", false));
            entries.push(row("Undo / redo", "Ctrl+Z Y", false));
            entries.push(Heading("SAVE"));
            entries.push(row("Save project", "Ctrl+S", false));
            entries.push(row("Save as", "Ctrl+Shift+S", false));
            entries.push(row("Close editor", "Esc", false));
        }
        // TEST RUN runs itself: entering loads and starts, leaving stops. There
        // is nothing to save here either; the ladder's own mode switch belongs
        // to the screen that owns the list.
        PlcPage::Sim => {
            entries.push(Heading("TEST RUN"));
            entries.push(row("Loads and starts", "on open", false));
            entries.push(row("Stops", "on leave", false));
            entries.push(Heading("LADDER"));
            entries.push(row("Step one scan", "S", false));
            entries.push(row("Reset data", "E", false));
        }
        // Every other page keeps the mode switch and the save in reach.
        _ => {
            entries.push(Heading("RUN"));
            entries.push(row("Start", "Ctrl+Enter", false));
            entries.push(row("Stop", "Ctrl+Shift+Enter", false));
            entries.push(Heading("SAVE"));
            entries.push(row("Save project", "Ctrl+S", false));
        }
    }
    entries
}

/// The height one entry asks for, including its gap.
fn rail_entry_height(entry: &RailEntry) -> f32 {
    match entry {
        RailEntry::Heading(_) => RAIL_GROUP_GAP + theme::NAV_ROW_HEIGHT_SECONDARY,
        RailEntry::Row { .. } => RAIL_INNER_GAP + theme::NAV_ROW_HEIGHT_SECONDARY,
    }
}

/// Paint entries from the top, with a group heading opening each new group.
fn draw_rail_entries(ui: &mut Ui, entries: &[RailEntry]) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut first = true;
    for entry in entries {
        match entry {
            RailEntry::Heading(text) => {
                if !first {
                    ui.add_space(RAIL_GROUP_GAP);
                }
                first = false;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), theme::NAV_ROW_HEIGHT_SECONDARY),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    egui::pos2(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    *text,
                    FontId::proportional(theme::font_size(8.0)),
                    theme::C3,
                );
            }
            RailEntry::Row { text, key, active } => {
                first = false;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), theme::NAV_ROW_HEIGHT_SECONDARY),
                    egui::Sense::hover(),
                );
                if *active {
                    ui.painter().rect_filled(rect, 2.0, theme::PANEL_RAISED);
                }
                ui.painter().text(
                    egui::pos2(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    *text,
                    FontId::proportional(theme::font_size(9.5)),
                    if *active { theme::FG } else { theme::MUTED },
                );
                ui.painter().text(
                    egui::pos2(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    *key,
                    FontId::monospace(theme::font_size(8.5)),
                    theme::ACCENT,
                );
                ui.add_space(RAIL_INNER_GAP);
            }
        }
    }
}

/// Split at a group heading nearest the halfway mark, so each column starts on a
/// heading instead of cutting a group in two.
fn rail_split(entries: &[RailEntry], total: f32) -> usize {
    let mut best = entries.len() / 2;
    let mut best_distance = f32::MAX;
    let mut running = 0.0;
    for (index, entry) in entries.iter().enumerate() {
        if matches!(entry, RailEntry::Heading(_)) && index > 0 {
            let distance = (running - total / 2.0).abs();
            if distance < best_distance {
                best_distance = distance;
                best = index;
            }
        }
        running += rail_entry_height(entry);
    }
    best
}

/// Height the whole rail asks for in one column; the editor widens the pane when
/// the viewport is shorter than this, so the second column has room for its label
/// and chord.
pub fn rail_total_height(plc: &PlcUi) -> f32 {
    rail_entries(plc).iter().map(rail_entry_height).sum()
}

fn shortcut_nav(plc: &PlcUi, ui: &mut Ui) {
    let entries = rail_entries(plc);
    let needed: f32 = entries.iter().map(rail_entry_height).sum();
    // A rail that cannot fit its rows in one column continues beside itself: the
    // groups stay in reading order, split on a heading so neither column starts
    // halfway through one.
    let columns = if needed > ui.available_height() && ui.available_width() >= 320.0 {
        2
    } else {
        1
    };
    if columns == 1 {
        draw_rail_entries(ui, &entries);
        return;
    }
    let split = rail_split(&entries, needed);
    let column_width = (ui.available_width() - 6.0) / 2.0;
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for chunk in [&entries[..split], &entries[split..]] {
            ui.allocate_ui_with_layout(
                egui::vec2(column_width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| draw_rail_entries(ui, chunk),
            );
        }
    });
}

pub fn editor(app: &mut LincApp, ui: &mut Ui) {
    // The workbench is a program editor, not a shell: the menu and the program
    // list belong to the PLC screen behind it, and a page left over from the
    // screen must not turn this window into a second copy of them.
    if app.plc.page == PlcPage::PouList {
        app.plc.page = PlcPage::Edit;
    }
    app.plc.menu_open = false;
    // Escape closes the editor only when nothing is open inside it. A picker or a
    // block window owns Escape first: consuming it here left the picker's state
    // set with its window gone, and the editor deaf to every later key.
    // The workbench draws on the frame after the one that opened it, so the guard
    // covers the opening frame and the first drawn frame.
    let opened_this_frame = ui.ctx().cumulative_frame_nr() <= app.plc.opened_frame + 1;
    if !opened_this_frame
        && escape_closes_editor(&app.plc)
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        && !ui.ctx().text_edit_focused()
    {
        app.overlay.plc_editor = false;
        // The preview is an editor aid: it must not outlive the window and leave
        // the screen showing held inputs.
        app.plc.preview.clear();
        return;
    }

    app.plc.poll();
    // `text_editing` is owned explicitly: a click on a visible field claims the
    // keyboard, Escape, a click on the grid, a page entry and the field losing
    // focus all give it back. It is deliberately not derived from "some TextEdit
    // has focus", because egui can hand focus to a field on a plain key press and
    // then the grid went deaf without the operator ever clicking a field.
    // No field may hold focus unless the operator claimed one by clicking it.
    // A TextEdit that keeps focus on this page consumes Enter and chords such as
    // Ctrl+Enter before the grid's handlers see them.
    // The grid's own anchor is the exception: it holds the focus so egui's focus
    // navigation never eats the arrow keys, and surrendering it here left the
    // cell cursor unable to move vertically at all.
    let anchor_id = ui.scope_id().with("plc-editor-focus");
    // A shell dialog owns the focus: surrendering anything that is not the grid
    // anchor took the ring away from the save prompt's name field every frame.
    if !app.plc.text_editing && app.overlay.modal.is_none() {
        ui.ctx().memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                if id != anchor_id {
                    memory.surrender_focus(id);
                }
            }
        });
    }
    if app.plc.text_editing {
        let still_focused = text_field_focused(ui.ctx());
        // Pages that own text fields: the ladder's property panel and the symbol
        // editor. Anything else means the keyboard owner really did change page.
        let page_lost_owner = !matches!(app.plc.page, PlcPage::Edit | PlcPage::Symbols);
        if page_lost_owner || !still_focused {
            app.plc.text_editing = false;
        }
    }
    if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
        // Every frame, not only key frames: a click changes the cursor without a
        // key, and a log written only on keys made the cursor look one input
        // behind, which is how a correct driver loop kept pressing arrows.
        {
            eprintln!(
                "plc keys: page={:?} text_editing={} focus={:?} picker={} operand_pick={} coil_pick={} compute={} block={} compare={} cursor={:?}",
                app.plc.page,
                app.plc.text_editing,
                ui.ctx().memory(|memory| memory.focused()),
                app.plc.picker.is_some(),
                app.plc.operand_pick.is_some(),
                app.plc.coil_pick.is_some(),
                app.plc.compute_edit.is_some(),
                app.plc.block_edit.is_some(),
                app.plc.compare_edit.is_some(),
                app.plc.cell_cursor,
            );
        }
    }
    // Locked: the grid owns the arrows, and an unlocked anchor let egui walk the
    // focus away on a plain Left/Right, so the cursor never moved. The anchor
    // also has to hold the focus: the lock filter is only consulted for the
    // focused widget, so without this egui spent the arrows on focus navigation
    // and the cell cursor never moved at all.
    if !app.plc.menu_open && app.overlay.modal.is_none() {
        let anchor = common::focus_anchor_locked(ui, "plc-editor-focus", true);
        if !app.plc.text_editing && !anchor.has_focus() {
            anchor.request_focus();
        }
    }

    // The menu bar does not take clicks in this build, so the pages and the
    // two actions the workflow needs every minute have keys as well.
    // A shell dialog owns every key; the page keys wait until it closes.
    let dialog_open = app.overlay.modal.is_some();
    let (
        to_ladder,
        to_symbols,
        to_sim,
        to_watch,
        to_trend,
        add_rung,
        save,
        save_as,
        new_project,
        add_row,
        delete_row,
    ) = if dialog_open {
        (
            false, false, false, false, false, false, false, false, false, false, false,
        )
    } else {
        (
            ctrl_key(ui, egui::Key::Num1),
            ctrl_key(ui, egui::Key::Num2),
            ctrl_key(ui, egui::Key::Num3),
            ctrl_key(ui, egui::Key::Num4),
            ctrl_key(ui, egui::Key::Num5),
            ctrl_key(ui, egui::Key::N),
            ctrl_key(ui, egui::Key::S),
            ctrl_shift_key(ui, egui::Key::S),
            // Ctrl+Shift+N is the keyboard path to File > New: the menu popup is not
            // reachable by keyboard in this build, and a new project must be.
            ctrl_shift_key(ui, egui::Key::N),
            // The manual's SS+ENTER substitute, plus Ctrl+J for terminals that
            // cannot send Ctrl+Enter.
            ctrl_key(ui, egui::Key::Enter) || ctrl_key(ui, egui::Key::J),
            // The panel keyboard has no Delete key: Bksp is the delete it has.
            ctrl_key(ui, egui::Key::Backspace),
        )
    };
    if to_ladder {
        app.plc.page = PlcPage::Edit;
    }
    if to_symbols {
        app.plc.page = PlcPage::Symbols;
    }
    if to_sim {
        app.plc.page = PlcPage::Sim;
    }
    if to_watch {
        app.plc.page = PlcPage::Watch;
    }
    if to_trend {
        app.plc.page = PlcPage::Trend;
    }
    // The simulation page owns its keys as the manual describes them, and the
    // input and run keys keep working on the watch and trend pages so a program can
    // be tried while its signals are compared in time.
    let sim_keys =
        !dialog_open && matches!(app.plc.page, PlcPage::Sim | PlcPage::Watch | PlcPage::Trend);
    if sim_keys {
        let (step, reset, watch, trend) = ui.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::S),
                input.consume_key(egui::Modifiers::NONE, egui::Key::E),
                input.consume_key(egui::Modifiers::NONE, egui::Key::W),
                input.consume_key(egui::Modifiers::NONE, egui::Key::G),
            )
        });
        if step {
            app.plc.send(Tag::Step, Request::Step);
        }
        if reset {
            app.plc.send(Tag::ResetData, Request::ResetData);
        }
        if watch {
            app.plc.page = PlcPage::Watch;
        }
        if trend {
            app.plc.page = PlcPage::Trend;
        }
    }
    if new_project {
        app.plc.apply(Action::New);
    }
    let ladder_page = app.plc.page == PlcPage::Edit;
    if add_rung && ladder_page {
        let (section, row) = app
            .plc
            .cell_cursor
            .map(|cursor| (cursor.0, cursor.1))
            .unwrap_or((0, 0));
        app.plc.apply(Action::AddRungAfter { section, row });
        // The new row is where the next key belongs: without moving the cursor,
        // repeated presses inserted at the same place and the ladder looked as
        // if nothing had happened.
        app.plc.cell_cursor = Some((section, row + 1, 0));
    }
    if delete_row && ladder_page {
        if let Some((section, rung)) = app.plc.cell_cursor.map(|cursor| (cursor.0, cursor.1)) {
            app.plc.apply(Action::DeleteRung { section, rung });
            app.plc.cell_cursor = Some((section, rung.saturating_sub(1), 0));
        }
    }
    if add_row && ladder_page {
        // Explicit row, not the tree selection: `AddRung` reads the selection,
        // which is not the cell cursor and inserted the row in the wrong place.
        let (section, row) = app
            .plc
            .cell_cursor
            .map(|cursor| (cursor.0, cursor.1))
            .unwrap_or((0, 0));
        app.plc.apply(Action::AddRungAfter { section, row });
        app.plc.cell_cursor = Some((section, row + 1, 0));
    }
    if save {
        app.request_plc_save();
    }
    if save_as {
        app.request_plc_save_as();
    }
    // TEST RUN is the ladder against the machine: opening the page loads and
    // starts it, leaving stops it. Nothing on the page can leave it running.
    if app.plc.page == PlcPage::Sim && !app.plc.sim_running {
        app.plc.sim_running = true;
        // Load what is on screen and run it. No save: testing an edit must not
        // rewrite the project file behind the operator's back.
        app.plc.validate();
        let project = app.plc.project.clone();
        app.plc.send(Tag::Load, Request::Load { project });
        app.plc.send(Tag::Start, Request::Start);
        app.plc.message = "TEST RUN".to_string();
    } else if app.plc.page != PlcPage::Sim && app.plc.sim_running {
        app.plc.sim_running = false;
        app.plc.apply(Action::Stop);
    }
    let mut actions: Vec<Action> = Vec::new();
    // A shell dialog (the save prompt, for one) owns the keyboard completely:
    // the grid behind it must not take the Enter that confirms the dialog.
    let shell_modal = app.overlay.modal.is_some();
    // Pickers first: they own the keyboard while they are open, and a text field
    // drawn later in the frame would consume Enter before the picker saw it.
    let modal_owned = if shell_modal {
        true
    } else {
        crate::screens::plc_grid::pickers(&mut app.plc, ui.ctx(), &mut actions)
    };

    // A window on top owns the letters: without this a preset's `m` or `s` also
    // ran the grid's commands, and Backspace deleted the cell under the box.
    if !app.plc.menu_open && !modal_owned {
        crate::screens::plc_grid::editor_keys(app, ui, &mut actions);
        il_keys(&mut app.plc, ui, &mut actions);
        fbd_keys(&mut app.plc, ui, &mut actions);
    }

    {
        let plc = &mut app.plc;

        let height = (ui.available_height() - STATUS_BAR_HEIGHT - 4.0).max(160.0);
        // A fixed column for the shortcuts and the rest for the page: without
        // allocating both sizes the page took the whole width and drew over the
        // strip.
        ui.horizontal_top(|ui| {
            // Two columns need room for the longest label beside its chord
            // ("Normal / set / reset" with "C S R"), so the pane widens with them.
            let nav_width = if rail_total_height(plc) > height {
                408.0
            } else {
                196.0
            };
            ui.allocate_ui_with_layout(
                egui::vec2(nav_width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| shortcut_nav(plc, ui),
            );
            ui.separator();
            let page_width = (ui.available_width() - 8.0).max(320.0);
            ui.allocate_ui_with_layout(
                egui::vec2(page_width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("plc-page")
                        .max_width(page_width)
                        .auto_shrink([false, false])
                        .show(ui, |ui| page_body(plc, ui, height, &mut actions));
                },
            );
        });
        status_bar(plc, ui);
    }

    // The other modal windows last: egui paints in call order, so a window drawn
    // before the page ends up underneath the page's cards.
    let windows_plc = &mut app.plc;
    fbd_grid_menu(windows_plc, ui.ctx(), &mut actions);
    fbd_operand_menu(windows_plc, ui.ctx(), &mut actions);

    for action in actions {
        app.plc.apply(action);
    }
    if app.plc.close_requested {
        app.plc.close_requested = false;
        // A close requested on the opening frame is the stale one that shut the
        // workbench before it could paint.
        if !opened_this_frame {
            app.overlay.plc_editor = false;
            app.plc.preview.clear();
        }
    }
}

/// ZX-PLC's menu bar: FILE, POU, EDIT, VIEW, RUN, HELP, with the current POU and
/// its language at the right, as the ZX screen shows `DEMO_LD (L10)`.
fn page_body(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    match plc.page {
        PlcPage::PouList => pou_list_page(plc, ui, height, actions),
        PlcPage::Edit => edit_page(plc, ui, height, actions),
        PlcPage::Symbols => symbols_page(plc, ui, height, actions),
        PlcPage::Sim => sim_page(plc, ui, height, actions),
        PlcPage::Watch => watch_page(plc, ui, height, actions),
        PlcPage::Trend => trend_page(plc, ui),
        PlcPage::Messages => {
            let _ = height;
            diagnostics_card(plc, ui, 400.0)
        }
        PlcPage::Service => card(ui, vec2(ui.available_width(), height), |ui, _| {
            status_card(plc, ui, actions)
        }),
    }
}

/// The program list keys read like the tool table's: arrows move the selection,
/// Page keys move a screenful, Enter opens, Backspace deletes. `N` adds a ladder
/// POU, `R` renames and `U` duplicates the selected one. MAIN is never deleted,
/// and the list is the only place a POU is created: the editor window edits the
/// one it was opened on.
fn pou_list_keys(plc: &mut PlcUi, ui: &Ui, actions: &mut Vec<Action>) {
    // Only the drawn list owns these keys. The screen sets the page before it
    // draws the list, so a stale `Edit` left behind by the closed editor cannot
    // make the list deaf, and a direct call from anywhere else stays inert.
    if plc.page != PlcPage::PouList || plc.text_editing || plc.keys_blocked {
        return;
    }
    let pous = plc.project.all_pous();
    if pous.is_empty() {
        return;
    }
    let mut selected = plc.pou.min(pous.len() - 1);
    let mut redraw = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            selected = selected.saturating_sub(1);
            redraw = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            selected = (selected + 1).min(pous.len() - 1);
            redraw = true;
        }
        // Page keys move by a screenful, as in every other list.
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
            selected = selected.saturating_sub(POU_PAGE as usize);
            redraw = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
            selected = (selected + POU_PAGE as usize).min(pous.len() - 1);
            redraw = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            actions.push(Action::OpenPou(selected));
            plc.open_editor = true;
            redraw = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::N) {
            actions.push(Action::AddPou(PouLanguage::Ld));
            redraw = true;
        }
        // Rename and duplicate ask for the name; pushing the action with the
        // current name renamed the POU to itself, which is why R looked dead.
        if input.consume_key(egui::Modifiers::NONE, egui::Key::R) {
            plc.pou_edit = Some(PouEdit {
                index: selected,
                duplicate: false,
                name: pous[selected].name.clone(),
            });
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::D) {
            plc.pou_edit = Some(PouEdit {
                index: selected,
                duplicate: true,
                // Eight characters is the name cap; `_2` keeps it readable.
                name: format!(
                    "{}_2",
                    &pous[selected].name[..pous[selected].name.len().min(6)]
                ),
            });
        }
        // Backspace deletes, as in the tool table. MAIN stays: the project needs
        // one POU to open into, and index 0 is the one every caller assumes.
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace) && selected > 0 {
            actions.push(Action::DeletePou { index: selected });
            redraw = true;
        }
    });
    if redraw {
        plc.pou = selected;
        plc.scroll_to_pou = true;
    }
}

/// Rows moved by one Page key, as in the tool table.
const POU_PAGE: isize = 8;

/// The program list, styled and driven like the tool table: a card, a heading,
/// a header row, one selected band with an accent bar, stripes, Page keys and a
/// footer that names the keys. The selection is the open POU.
fn pou_list_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    pou_list_keys(plc, ui, actions);
    let pous = plc.project.all_pous();
    let selected = plc.pou.min(pous.len().saturating_sub(1));
    let mut open: Option<usize> = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "PROGRAM LIST");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} POU", pous.len()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        ui.label(
            RichText::new("LADDER ONLY")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.add_space(4.0);
        if pous.is_empty() {
            ui.label(
                RichText::new("no POUs in this project")
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
            return;
        }
        let name_width = 150.0;
        let language_width = 76.0;
        let size_width = 44.0;
        let gap = 16.0;
        let comment_width =
            (ui.available_width() - name_width - language_width - size_width - gap * 3.0).max(80.0);
        let columns = [
            ("POU NAME", name_width),
            ("LANGUAGE", language_width),
            ("SIZE", size_width),
            ("COMMENT", comment_width),
        ];
        // Header: one line of uppercase headings and a rule under it, the same
        // weight as the tool table's header.
        let (header_rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 18.0), egui::Sense::hover());
        let painter = ui.painter().clone();
        let mut x = header_rect.left();
        for (heading, width) in columns {
            painter.text(
                egui::pos2(x, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                heading,
                egui::FontId::monospace(theme::font_size(9.0)),
                theme::MUTED,
            );
            x += width + gap;
        }
        painter.hline(
            header_rect.x_range(),
            header_rect.bottom(),
            egui::Stroke::new(1.0, theme::BORDER),
        );
        let row_height = 22.0;
        let list_height = (height - 96.0).max(72.0);
        egui::ScrollArea::vertical()
            .id_salt("plc-pou-scroll")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, pous.len(), |ui, range| {
                for index in range {
                    let pou = &pous[index];
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_height - 2.0),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter();
                    if index % 2 == 1 {
                        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::SURFACE);
                    }
                    if index == selected {
                        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::PANEL_RAISED);
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(theme::SELECTION_BAR, rect.height()),
                            ),
                            egui::CornerRadius::ZERO,
                            theme::ACCENT,
                        );
                        if plc.scroll_to_pou {
                            ui.scroll_to_rect(rect, Some(egui::Align::Center));
                            plc.scroll_to_pou = false;
                        }
                    }
                    let size = match pou.language {
                        PouLanguage::Ld => {
                            pou.sections.iter().map(|s| s.rungs.len()).sum::<usize>()
                        }
                        PouLanguage::Il => pou.il.len(),
                        PouLanguage::Fbd => pou.fbd.len(),
                    };
                    let text = |x: f32, value: &str, colour: egui::Color32, monospace: bool| {
                        painter.text(
                            egui::pos2(x, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            value,
                            if monospace {
                                egui::FontId::monospace(theme::font_size(10.0))
                            } else {
                                egui::FontId::proportional(theme::font_size(10.0))
                            },
                            colour,
                        );
                    };
                    let mut x = rect.left() + theme::SELECTION_BAR + 4.0;
                    text(x, &pou.name, theme::FG, true);
                    x = rect.left() + name_width + gap;
                    text(x, pou.language.label(), theme::C3, true);
                    x = rect.left() + name_width + language_width + gap * 2.0;
                    text(x, &size.to_string(), theme::MUTED, true);
                    x = rect.left() + name_width + language_width + size_width + gap * 3.0;
                    painter.with_clip_rect(egui::Rect::from_min_max(
                        egui::pos2(x, rect.top()),
                        egui::pos2(rect.right(), rect.bottom()),
                    ));
                    text(x, &pou.comment, theme::MUTED, false);
                    if response.clicked() {
                        plc.pou = index;
                        plc.scroll_to_pou = true;
                    }
                    if response.double_clicked() {
                        open = Some(index);
                    }
                }
            });
        ui.add_space(4.0);
        ui.label(
            RichText::new("ENTER OPEN · N NEW · R RENAME · D DUPLICATE · BKSP DELETE")
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
    if let Some(index) = open {
        // A double click opens the same body the Enter key does.
        plc.pou = index;
        actions.push(Action::OpenPou(index));
        plc.open_editor = true;
    }
}

/// The EDIT page: the selected POU in its own language, with the property
/// panel on the right. ZX-PLC opens a picker on Enter; the panel is the modern
/// form of the same thing, and it edits the same selection.
fn edit_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    let language = plc
        .current_pou()
        .map(|pou| pou.language)
        .unwrap_or(PouLanguage::Ld);
    ui.horizontal(|ui| {
        if let Some(pou) = plc.current_pou() {
            ui.label(
                RichText::new(format!("{} · {}", pou.name, language.label()))
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::FG),
            );
        }
    });
    if !plc.preview.is_empty() {
        ui.label(
            RichText::new(
                "PREVIEW · input bits held for the drawing only, not sent to the machine",
            )
            .size(theme::font_size(9.0))
            .color(theme::WARNING),
        );
    }
    if language == PouLanguage::Fbd {
        fbd_box_editor(plc, ui.ctx(), actions);
    }
    ui.add_space(2.0);
    // The editor owns the pane: the properties card and its POU LIST button are
    // gone, so the ladder, IL text and FBD grid use the whole width.
    let left_width = ui.available_width().max(220.0);
    let body_height = (height - 32.0).max(120.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(left_width, body_height),
            egui::Layout::top_down(Align::Min),
            |ui| match language {
                PouLanguage::Ld => {
                    // Clipped to its column: without a bounded scroll area the
                    // ladder drew past it and under the properties card.
                    let sections = plc.current_pou_sections();
                    egui::ScrollArea::both()
                        .id_salt("plc-ladder-clip")
                        .max_width(left_width)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            // Keep the cell cursor on screen: the page is taller
                            // than the window on a long program, and a cursor
                            // that walked off the fold could not be seen while
                            // keys moved it.
                            let follow = plc.scroll_to_cursor;
                            plc.scroll_to_cursor = false;
                            ladder_body(plc, &sections, ui, body_height, actions, follow);
                        });
                }
                PouLanguage::Il => il_editor(plc, ui, body_height),
                PouLanguage::Fbd => fbd_editor(plc, ui, body_height, actions),
            },
        );
    });
}

/// The operand a line refers to and its live bit value, if the snapshot has one.
/// A `CAL TON0(...)` line names the instance, so the block's `Q` is the bit.
fn il_operand_value(plc: &PlcUi, line: &str) -> (String, Option<bool>) {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") {
        return (String::new(), None);
    }
    let mut words = trimmed.split_whitespace();
    let _mnemonic = words.next();
    let rest = words.next().unwrap_or("").trim();
    if rest.is_empty() {
        return (String::new(), None);
    }
    let operand = match rest.find('(') {
        Some(open) => format!("{}.Q", &rest[..open]),
        None => rest.to_string(),
    };
    let value = plc.bit_value(&operand);
    (operand, value)
}

/// The right-hand column of an IL line: `TRUE`/`FALSE` for a bit, the value for a
/// word, and the running `ET` for a timer call.
fn il_display_value(plc: &PlcUi, operand: &str) -> Option<String> {
    if operand.is_empty() {
        return None;
    }
    if let Some(value) = plc.bit_value(operand) {
        return Some(if value {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        });
    }
    if let Some(value) = plc.word_value(operand) {
        return Some(value.to_string());
    }
    // `CAL TON0(...)`: the instance's ET, which is what a running timer shows.
    if let Some(instance) = operand.strip_suffix(".Q") {
        if let Some(et) = plc.word_value(&format!("{instance}.ET")) {
            return Some(format!("ET {et}"));
        }
    }
    None
}

/// The IL editor: numbered source lines over the POU's `il` body. The compiler
/// that turns them into bytecode is the next slice.
fn il_editor(plc: &mut PlcUi, ui: &mut Ui, height: f32) {
    plc.sync_il_line();
    let lines = plc.il_lines();
    ui.label(
        RichText::new(
            "SPACE finishes an instruction · ENTER checks the line · Ctrl+Enter inserts below · DELETE removes an empty line",
        )
        .monospace()
        .size(theme::font_size(8.5))
        .color(theme::MUTED),
    );
    ui.add_space(2.0);
    egui::ScrollArea::vertical()
        .id_salt("plc-il")
        .max_height((height - 8.0).max(120.0))
        .show(ui, |ui| {
            for (index, line) in lines.iter().enumerate() {
                let active = index == plc.il_cursor;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:>3}", index + 1))
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::LINE_NUMBER),
                    );
                    let text = if active {
                        format!("{}█", plc.il_buffer)
                    } else {
                        line.clone()
                    };
                    // The operand of a line is live: green when its bit is true,
                    // and timers also show their running ET on the right.
                    let (operand, live) = il_operand_value(plc, line);
                    let colour = if line.trim_start().starts_with("//") {
                        theme::COMMENT
                    } else if live == Some(true) {
                        theme::C4
                    } else if active {
                        theme::ACCENT
                    } else {
                        theme::FG
                    };
                    let response = ui.add(
                        egui::Label::new(
                            RichText::new(if text.is_empty() {
                                " ".to_string()
                            } else {
                                text
                            })
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(colour),
                        )
                        .sense(Sense::click()),
                    );
                    if response.clicked() {
                        plc.il_cursor = index;
                        plc.il_buffer = line.clone();
                        plc.il_message = None;
                    }
                    if let Some(value) = il_display_value(plc, &operand) {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(value)
                                    .monospace()
                                    .size(theme::font_size(8.5))
                                    .color(theme::MUTED),
                            );
                        });
                    }
                });
            }
            if lines.is_empty() {
                ui.label(
                    RichText::new("empty POU — type the first instruction")
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
            }
        });
    if let Some(message) = &plc.il_message {
        ui.add_space(2.0);
        ui.label(
            RichText::new(message.clone())
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::ERROR),
        );
    }
}

/// Is a text field holding the keyboard?
///
/// A button keeps keyboard focus after a click, so checking "some widget is
/// focused" would swallow every ladder key for the rest of the session. Only a
/// text field should take the keys away from the editor.
pub fn text_field_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.focused())
        .map(|id| egui::widgets::text_edit::TextEditState::load(ctx, id).is_some())
        .unwrap_or(false)
}

/// Keys the IL editor owns, as the manual's line editor describes them.
fn il_keys(plc: &mut PlcUi, ui: &Ui, actions: &mut Vec<Action>) {
    if plc.page != PlcPage::Edit || plc.text_editing {
        return;
    }
    let is_il = plc
        .current_pou()
        .map(|pou| pou.language == PouLanguage::Il)
        .unwrap_or(false);
    if !is_il {
        return;
    }
    plc.sync_il_line();
    let mut lines = plc.il_lines();
    if lines.is_empty() {
        lines.push(String::new());
    }

    let events = ui.input(|input| input.events.clone());
    for event in events {
        match event {
            egui::Event::Text(text) => {
                for character in text.chars() {
                    if !character.is_ascii() || character == '\n' {
                        continue;
                    }
                    if character == ' '
                        && !plc.il_buffer.ends_with(' ')
                        && linc_plc_core::il::SUPPORTED
                            .iter()
                            .chain(linc_plc_core::il::UNSUPPORTED.iter())
                            .any(|known| known.eq_ignore_ascii_case(plc.il_buffer.trim()))
                    {
                        // A known mnemonic uppercases and the cursor moves on,
                        // which is what SPACE does on the ZX line.
                        plc.il_buffer = format!("{} ", plc.il_buffer.trim().to_ascii_uppercase());
                    } else {
                        plc.il_buffer.push(character);
                    }
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => match key {
                egui::Key::Backspace => {
                    plc.il_buffer.pop();
                }
                egui::Key::Enter => {
                    if modifiers.ctrl {
                        plc.il_store_buffer(&mut lines, false);
                        lines.insert(plc.il_cursor + 1, String::new());
                        plc.il_cursor += 1;
                        plc.il_buffer.clear();
                    } else if plc.il_buffer.trim().is_empty() && plc.il_cursor == 0 {
                        lines.insert(0, String::new());
                        plc.il_cursor = 0;
                        plc.il_buffer.clear();
                    } else if let Some(message) = {
                        let check = plc.il_check(&plc.il_buffer);
                        if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
                            eprintln!(
                                "il enter buf={:?} check={:?} cursor={} lines={}",
                                plc.il_buffer,
                                check,
                                plc.il_cursor,
                                lines.len()
                            );
                        }
                        check
                    } {
                        plc.il_message = Some(message);
                        break;
                    } else {
                        plc.il_store_buffer(&mut lines, true);
                        if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
                            eprintln!(
                                "il stored project_il={:?}",
                                plc.current_pou()
                                    .map(|pou| pou.il.clone())
                                    .unwrap_or_default()
                            );
                        }
                        // Commit is one line: the cursor moves on and the buffer is
                        // either the next stored line or empty. Leaving the typed
                        // text in the buffer glued the next line onto it
                        // (`LD pumpST run`).
                        if plc.il_cursor + 1 < lines.len() {
                            plc.il_cursor += 1;
                            plc.il_buffer = lines[plc.il_cursor].clone();
                        } else {
                            plc.il_cursor = lines.len();
                            plc.il_buffer.clear();
                        }
                    }
                    plc.il_message = None;
                    break;
                }
                egui::Key::Delete => {
                    if lines
                        .get(plc.il_cursor)
                        .map(|line| line.trim().is_empty())
                        .unwrap_or(true)
                    {
                        if plc.il_cursor < lines.len() {
                            lines.remove(plc.il_cursor);
                        }
                        plc.il_cursor = plc.il_cursor.min(lines.len().saturating_sub(1));
                        plc.il_buffer = lines.get(plc.il_cursor).cloned().unwrap_or_default();
                        plc.il_store_lines(lines);
                    }
                    break;
                }
                egui::Key::ArrowUp => {
                    plc.il_store_buffer(&mut lines, false);
                    if plc.il_cursor > 0 {
                        plc.il_cursor -= 1;
                        plc.il_buffer = lines[plc.il_cursor].clone();
                    }
                    break;
                }
                egui::Key::ArrowDown => {
                    plc.il_store_buffer(&mut lines, false);
                    if plc.il_cursor + 1 < lines.len() {
                        plc.il_cursor += 1;
                        plc.il_buffer = lines[plc.il_cursor].clone();
                    }
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }
    let _ = actions;
}

/// Draw the lines as a diagram: inputs enter on their own rows, the block spans
/// them, and its output leaves on the top row, which is the shape ZX draws.
/// The value list for an FBD output or input: declared symbols (with their
/// comments), the word bank, block members, booleans, numbers and times.
/// Instance and preset of the block on one FBD line. The same choices the ladder/// Instance and preset of the block under the FBD cursor.
fn fbd_box_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((row, col)) = plc.fbd_block_edit else {
        return;
    };
    let grid = fbd_grid_of(plc);
    let Some(block) = grid
        .blocks
        .iter()
        .find(|block| block.row == row && block.col == col)
        .cloned()
    else {
        plc.fbd_block_edit = None;
        return;
    };
    let letters: String = block
        .kind
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let timer = matches!(letters.as_str(), "TON" | "TOF" | "TP");
    let counter = matches!(letters.as_str(), "CTU" | "CTD");
    let mut close = false;
    let mut next_block: Option<String> = None;
    let mut next_preset: Option<String> = None;
    egui::Window::new(format!("{} · row {} col {}", block.kind, row + 1, col + 1))
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if timer || counter || matches!(letters.as_str(), "SR" | "RS" | "RT" | "RF") {
                ui.label(
                    RichText::new("instance")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.horizontal(|ui| {
                    for number in 0..4 {
                        let name = format!("{letters}{number}");
                        if theme::action_button(ui, &name, true).clicked() {
                            next_block = Some(name);
                        }
                    }
                });
                ui.add_space(4.0);
            }
            if timer || counter {
                ui.label(
                    RichText::new("preset")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.horizontal_wrapped(|ui| {
                    let presets: &[&str] = if timer {
                        &["T#100ms", "T#500ms", "T#1s", "T#2s", "T#5s", "T#10s"]
                    } else {
                        &["1", "5", "10", "50", "100", "1000"]
                    };
                    for value in presets {
                        if ui.button(*value).clicked() {
                            next_preset = Some((*value).to_string());
                        }
                    }
                });
            }
            ui.add_space(4.0);
            if theme::action_button(ui, "Close", true).clicked() {
                close = true;
            }
        });
    if next_block.is_some() || next_preset.is_some() {
        let mut grid = grid.clone();
        for item in grid.blocks.iter_mut() {
            if item.row == row && item.col == col {
                if let Some(kind) = &next_block {
                    item.kind = kind.clone();
                }
                if let Some(preset) = &next_preset {
                    item.preset = preset.clone();
                }
            }
        }
        actions.push(Action::SetFbdGrid(grid));
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.fbd_block_edit = None;
    }
}

/// The value list for an FBD output or input: declared symbols (with their
/// comments), the word bank, block members, booleans, numbers and times.
/// Function block diagram: one assignment per line, blocks nesting by naming
/// an earlier line's output. Inputs are typed here; the value pickers the
/// ladder editor has are not wired to this page yet.
/// The grid the editor works on: the POU's own, or the line body laid out so an
/// older project can be edited as a grid without being rewritten first.
fn fbd_grid_of(plc: &PlcUi) -> FbdGrid {
    plc.current_pou()
        .and_then(|pou| match pou.fbd_grid {
            Some(grid) if !grid.blocks.is_empty() => Some(grid),
            _ if !pou.fbd.is_empty() => Some(FbdGrid::from_lines(&pou.fbd)),
            _ => pou.fbd_grid,
        })
        .unwrap_or_default()
}

fn is_hidden(grid: &FbdGrid, row: usize, col: usize) -> bool {
    grid.blocks
        .iter()
        .any(|block| block.col == col && row > block.row && row < block.row + block.ins.max(1))
}

/// Draw the grid: cells, blocks spanning their rows, and the cursor.

/// Live output of an FBD block. A named function block publishes its `Q`; a generic
/// gate is evaluated from the input cells on its rows. A block whose inputs are not
/// readable answers `None` rather than guessing.
fn fbd_block_active(
    plc: &PlcUi,
    grid: &FbdGrid,
    block: &linc_plc_core::model::FbdBlock,
) -> Option<bool> {
    if !block.instance.is_empty() {
        return plc.bit_value(&format!("{}.Q", block.instance));
    }
    let row_value = |row: usize| -> Option<bool> {
        match grid.cell(row, block.col.saturating_sub(1)) {
            FbdCell::In { operand } | FbdCell::Out { operand } => {
                plc.bit_value(operand.trim_start_matches('!'))
            }
            _ => None,
        }
    };
    let rows = block.ins.max(1);
    let values: Vec<bool> = (0..rows).filter_map(row_value).collect();
    if values.len() != rows {
        return None;
    }
    Some(match block.kind.as_str() {
        "AND" => values.iter().all(|value| *value),
        "OR" => values.iter().any(|value| *value),
        "XOR" => values.iter().filter(|value| **value).count() % 2 == 1,
        "NOT" => !values[0],
        _ => return None,
    })
}

fn fbd_grid_view(
    plc: &mut PlcUi,
    grid: &FbdGrid,
    ui: &mut Ui,
    height: f32,
    actions: &mut Vec<Action>,
) {
    let _ = actions;
    let cell_w = 56.0_f32;
    let cell_h = 22.0_f32;
    let rows = grid.rows.max(1);
    let columns = grid.columns.max(6);
    let width = (columns as f32) * cell_w;
    let height_needed = (rows as f32) * cell_h + 8.0;
    egui::ScrollArea::both()
        .id_salt("plc-fbd-grid")
        .max_height((height - 40.0).max(120.0))
        .show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(vec2(width + 8.0, height_needed), Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect, 3.0, theme::SURFACE);
            let origin = rect.min + vec2(4.0, 4.0);
            let at =
                |row: usize, col: usize| origin + vec2(col as f32 * cell_w, row as f32 * cell_h);
            // Cells.
            for row in 0..rows {
                for col in 0..columns {
                    if is_hidden(grid, row, col) {
                        continue;
                    }
                    let cell_rect = egui::Rect::from_min_size(at(row, col), vec2(cell_w, cell_h));
                    painter.rect_stroke(
                        cell_rect,
                        1.0,
                        Stroke::new(1.0, theme::BORDER),
                        egui::StrokeKind::Inside,
                    );
                    let text = match grid.cell(row, col) {
                        FbdCell::In { operand } => {
                            plc.operand_label(operand.trim_start_matches('!'))
                        }
                        FbdCell::Out { operand } => {
                            plc.operand_label(operand.trim_start_matches('!'))
                        }
                        FbdCell::Wire => "—".to_string(),
                        FbdCell::Empty => String::new(),
                    };
                    if !text.is_empty() {
                        // A TRUE operand is green, as the manual's FBD monitor shows.
                        let operand = match grid.cell(row, col) {
                            FbdCell::In { operand } | FbdCell::Out { operand } => {
                                Some(operand.trim_start_matches('!').to_string())
                            }
                            _ => None,
                        };
                        let live = operand
                            .as_deref()
                            .and_then(|name| plc.bit_value(name))
                            .unwrap_or(false);
                        painter.text(
                            cell_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            text,
                            egui::FontId::monospace(theme::font_size(8.5)),
                            if live { theme::C4 } else { theme::FG },
                        );
                    }
                }
            }
            // Blocks span their rows.
            for block in &grid.blocks {
                let span = block.ins.max(1) as f32;
                let block_rect = egui::Rect::from_min_size(
                    at(block.row, block.col) + vec2(1.0, 1.0),
                    vec2(cell_w - 2.0, span * cell_h - 2.0),
                );
                painter.rect_filled(block_rect, 2.0, theme::PANEL_RAISED);
                painter.rect_stroke(
                    block_rect,
                    2.0,
                    Stroke::new(1.0, theme::ACCENT),
                    egui::StrokeKind::Inside,
                );
                // A block is coloured by its own output, which the snapshot now
                // publishes as `TON0.Q` etc.
                let q = fbd_block_active(plc, grid, block).unwrap_or(false);
                let block_colour = if q { theme::C4 } else { theme::ACCENT };
                painter.text(
                    block_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &block.kind,
                    egui::FontId::monospace(theme::font_size(9.0)),
                    block_colour,
                );
                if !block.preset.is_empty() {
                    painter.text(
                        egui::pos2(block_rect.right() + 2.0, block_rect.top() + cell_h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        &block.preset,
                        egui::FontId::monospace(theme::font_size(7.5)),
                        theme::MUTED,
                    );
                }
            }
            // The cursor.
            if let Some((row, col)) = plc.fbd_cursor {
                let cursor_rect = egui::Rect::from_min_size(at(row, col), vec2(cell_w, cell_h));
                painter.rect_stroke(
                    cursor_rect,
                    1.0,
                    Stroke::new(1.8, theme::WARNING),
                    egui::StrokeKind::Inside,
                );
            }
            // A click lands on a cell; a click inside a block body lands on the
            // block's anchor, because the body is not a position.
            if let Some(pointer) = response.interact_pointer_pos() {
                if response.clicked() {
                    let dx = (pointer.x - origin.x) / cell_w;
                    let dy = (pointer.y - origin.y) / cell_h;
                    if dx >= 0.0 && dy >= 0.0 {
                        let mut row = dy as usize;
                        let col = dx as usize;
                        for block in &grid.blocks {
                            if block.col == col
                                && row > block.row
                                && row < block.row + block.ins.max(1)
                            {
                                row = block.row;
                            }
                        }
                        if row < rows && col < columns {
                            plc.fbd_cursor = Some((row, col));
                            // The PT/PV cell belongs to the block's preset, not
                            // to the element list.
                            match fbd_preset_cell(grid, row, col) {
                                Some(anchor) => plc.fbd_block_edit = Some(anchor),
                                None => plc.fbd_grid_pick = Some((row, col)),
                            }
                        }
                    }
                }
            }
        });
}

/// Rows a block of this kind spans, which is how many inputs it draws.
fn fbd_block_rows(kind: &str) -> usize {
    let name = kind.trim().to_ascii_uppercase();
    if matches!(name.as_str(), "NOT" | "RT0" | "RF0") || name.starts_with("R_TRIG") {
        return 1;
    }
    if name.starts_with("CTU") || name.starts_with("CTD") {
        return 3;
    }
    2
}

/// The width a block occupies: its input cell, the box, and its output cell.
const FBD_BLOCK_WIDTH: usize = 2;

/// Rows a block spans; a block is one network with everything it overlaps.
fn fbd_network_rows(grid: &FbdGrid, row: usize, ins: usize) -> (usize, usize) {
    let mut low = row;
    let mut high = row + ins.saturating_sub(1);
    loop {
        let mut grew = false;
        for block in &grid.blocks {
            let top = block.row;
            let bottom = block.row + block.ins.max(1).saturating_sub(1);
            if top <= high + 1 && bottom + 1 >= low && (top < low || bottom > high) {
                if top < low {
                    low = top;
                    grew = true;
                }
                if bottom > high {
                    high = bottom;
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    (low, high)
}

/// Shift a network right by `amount` columns. Returns false when it would run
/// off the grid, in which case nothing is moved.
fn fbd_shift_right(
    grid: &mut FbdGrid,
    row: usize,
    ins: usize,
    from_col: usize,
    amount: usize,
) -> bool {
    let (low, high) = fbd_network_rows(grid, row, ins);
    let rightmost = grid
        .blocks
        .iter()
        .filter(|block| block.row >= low && block.row <= high && block.col >= from_col)
        .map(|block| block.col)
        .max()
        .unwrap_or(from_col);
    if rightmost + amount + 2 >= grid.columns {
        return false;
    }
    // Everything from the cursor rightwards moves; what is left of it stays, so
    // the input that fed the network still feeds the new block.
    for r in low..=high {
        for col in (from_col + amount..grid.columns).rev() {
            let cell = grid.cell(r, col - amount);
            let keep = matches!(
                cell,
                FbdCell::In { .. } | FbdCell::Out { .. } | FbdCell::Wire
            );
            grid.set_cell(r, col, if keep { cell } else { FbdCell::Empty });
        }
        for col in from_col..(from_col + amount).min(grid.columns) {
            grid.set_cell(r, col, FbdCell::Empty);
        }
    }
    for block in grid.blocks.iter_mut() {
        if block.row >= low && block.row <= high && block.col >= from_col {
            block.col += amount;
        }
    }
    true
}

/// Can this block sit with its anchor at `(row, col)` without covering a signal?
fn fbd_room(grid: &FbdGrid, row: usize, col: usize, ins: usize) -> bool {
    for offset in 0..ins {
        // The cell to the left is the input the block reads, so a signal there
        // is what we want; the box itself and its output must be free.
        for column in col..=col + 1 {
            match grid.cell(row + offset, column) {
                FbdCell::Empty => {}
                FbdCell::Wire => {}
                _ => return false,
            }
        }
    }
    true
}

fn fbd_place_block(
    plc: &mut PlcUi,
    kind: &str,
    actions: &mut Vec<Action>,
    message: &mut Option<String>,
) {
    // The picker's coordinate is authoritative: it is the cell the operator chose.
    // Falling back to the cursor kept the block silent when the two differed (the
    // cursor could be `None` while the picker was open).
    let Some((row, col)) = plc.fbd_grid_pick.or(plc.fbd_cursor) else {
        return;
    };
    let mut grid = fbd_grid_of(plc);
    let ins = fbd_block_rows(kind);
    if !fbd_room(&grid, row, col, ins) {
        // Make room by moving the network right, as the manual describes; if it
        // cannot move, nothing changes at all.
        if !fbd_shift_right(&mut grid, row, ins, col, FBD_BLOCK_WIDTH) {
            *message = Some("No room for the block".to_string());
            return;
        }
        if !fbd_room(&grid, row, col, ins) {
            *message = Some("No room for the block".to_string());
            return;
        }
    }
    grid.blocks
        .retain(|block| !(block.row == row && block.col == col));
    for offset in 0..ins {
        grid.set_cell(row + offset, col, FbdCell::Empty);
    }
    grid.blocks.push(linc_plc_core::model::FbdBlock {
        row,
        col,
        ins,
        kind: kind.to_string(),
        instance: String::new(),
        preset: String::new(),
    });
    grid.rows = grid.rows.max(row + ins);
    actions.push(Action::SetFbdGrid(grid));
}

/// The block whose preset cell this is: a timer's PT on its second row and a
/// counter's PV on its third, both written on the cell left of that row.
fn fbd_preset_cell(grid: &FbdGrid, row: usize, col: usize) -> Option<(usize, usize)> {
    for block in &grid.blocks {
        let timer = block.kind.starts_with("TON")
            || block.kind.starts_with("TOF")
            || block.kind.starts_with("TP");
        let counter = block.kind.starts_with("CTU") || block.kind.starts_with("CTD");
        if col + 1 != block.col {
            continue;
        }
        if timer && row == block.row + 1 {
            return Some((block.row, block.col));
        }
        if counter && row == block.row + 2 {
            return Some((block.row, block.col));
        }
    }
    None
}

/// Close the gap a deleted block left: move the network back left, then drop
/// the rows that ended up empty.
fn fbd_close_up(grid: &mut FbdGrid, row: usize, ins: usize) {
    let (low, high) = fbd_network_rows(grid, row, ins);
    let leftmost = grid
        .blocks
        .iter()
        .filter(|block| block.row >= low && block.row <= high)
        .map(|block| block.col.saturating_sub(1))
        .min()
        .unwrap_or(0);
    if leftmost > 1 {
        let amount = leftmost - 1;
        for r in low..=high {
            for col in 1..grid.columns {
                let source = col + amount;
                let cell = grid.cell(r, source);
                let keep = matches!(
                    cell,
                    FbdCell::In { .. } | FbdCell::Out { .. } | FbdCell::Wire
                );
                grid.set_cell(r, col, if keep { cell } else { FbdCell::Empty });
            }
        }
        for block in grid.blocks.iter_mut() {
            if block.row >= low && block.row <= high {
                block.col = block.col.saturating_sub(amount);
            }
        }
    }
    // Empty rows below the network disappear.
    while grid.rows > 1 {
        let last = grid.rows - 1;
        let empty = (0..grid.columns).all(|col| matches!(grid.cell(last, col), FbdCell::Empty))
            && !grid.blocks.iter().any(|block| block.row == last);
        if !empty {
            break;
        }
        grid.rows -= 1;
        grid.cells.truncate(grid.rows);
    }
}

/// The element list for a grid cell: signals, gates, blocks.

/// What a key means while the FBD element picker is open.
enum FbdPick {
    Wire,
    Input,
    Output,
    Block(&'static str),
    Close,
}

/// The picker's letter map, mirroring the manual's FBD table.
fn fbd_pick_for_char(character: char) -> Option<FbdPick> {
    Some(match character.to_ascii_uppercase() {
        'W' | '-' => FbdPick::Wire,
        'I' => FbdPick::Input,
        'Q' => FbdPick::Output,
        'A' => FbdPick::Block("AND"),
        'O' => FbdPick::Block("OR"),
        'X' => FbdPick::Block("XOR"),
        'N' | '!' => FbdPick::Block("NOT"),
        'T' => FbdPick::Block("TON"),
        'F' => FbdPick::Block("TOF"),
        'U' => FbdPick::Block("TP"),
        'K' => FbdPick::Block("CTU"),
        'D' => FbdPick::Block("CTD"),
        _ => return None,
    })
}

/// Read the picker's choice from the frame's events. Letters arrive as
/// `Event::Text` (a typed key), so `consume_key` alone never matched them; `-` and
/// Escape stay on the key path because they produce no text.
fn fbd_pick_key(ctx: &egui::Context) -> Option<FbdPick> {
    let mut picked = None;
    for event in ctx.input(|input| input.events.clone()) {
        match event {
            egui::Event::Text(text) => {
                for character in text.chars() {
                    if let Some(choice) = fbd_pick_for_char(character) {
                        picked = Some(choice);
                        break;
                    }
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if modifiers.is_none() => match key {
                egui::Key::Minus => picked = Some(FbdPick::Wire),
                egui::Key::Escape => picked = Some(FbdPick::Close),
                _ => {}
            },
            _ => {}
        }
        if picked.is_some() {
            break;
        }
    }
    picked
}

fn fbd_grid_menu(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
        eprintln!(
            "fbd menu: pick={:?} operand={:?} filter={:?}",
            plc.fbd_grid_pick, plc.fbd_operand_pick, plc.operand_filter
        );
    }
    let Some((row, col)) = plc.fbd_grid_pick else {
        return;
    };
    let mut close = false;
    let mut message: Option<String> = None;
    let mut set_cell: Option<FbdCell> = None;
    let mut operand_pick: Option<bool> = None;
    let mut block: Option<String> = None;
    let mut box_edit = false;
    // The picker owns the letters while it is open: the grid cursor moves with the
    // arrows and a letter chooses the element, so placing never needs a click.
    let keyed = fbd_pick_key(ctx);
    if cfg!(debug_assertions) {
        eprintln!(
            "fbd pick key -> {:?}",
            keyed.as_ref().map(|pick| match pick {
                FbdPick::Wire => "wire",
                FbdPick::Input => "input",
                FbdPick::Output => "output",
                FbdPick::Block(kind) => kind,
                FbdPick::Close => "close",
            })
        );
    }
    if keyed.is_some() {
        // The picker consumed the letters: leaving them in the frame's events let
        // the operand window that opens in the same frame append them to its filter
        // (`i` became part of `ipump`).
        ctx.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Text(_)))
        });
    }
    if let Some(choice) = keyed {
        match choice {
            FbdPick::Wire => set_cell = Some(FbdCell::Wire),
            FbdPick::Input => operand_pick = Some(false),
            FbdPick::Output => operand_pick = Some(true),
            FbdPick::Block(kind) => block = Some(kind.to_string()),
            FbdPick::Close => close = true,
        }
    }
    egui::Window::new(format!("FBD  row {} col {}", row + 1, col + 1))
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if theme::action_button(ui, "wire  -", true).clicked() {
                    set_cell = Some(FbdCell::Wire);
                }
                if theme::action_button(ui, "input  I", true).clicked() {
                    operand_pick = Some(false);
                }
                if theme::action_button(ui, "output  Q", true).clicked() {
                    operand_pick = Some(true);
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                for kind in [
                    "AND", "OR", "XOR", "NOT", "TON0", "TOF0", "TP0", "CTU0", "CTD0",
                ] {
                    if theme::action_button(ui, kind, true).clicked() {
                        block = Some(kind.to_string());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for kind in [
                    "SR0", "RS0", "RT0", "RF0", "GT", "GE", "EQ", "NE", "LE", "LT",
                ] {
                    if theme::action_button(ui, kind, true).clicked() {
                        block = Some(kind.to_string());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for kind in ["ADD", "SUB", "MUL", "DIV", "MOVE"] {
                    if theme::action_button(ui, kind, true).clicked() {
                        block = Some(kind.to_string());
                    }
                }
            });
            ui.separator();
            let existing = fbd_grid_of(plc)
                .blocks
                .iter()
                .any(|block| block.row == row && block.col == col);
            ui.add_enabled_ui(existing, |ui| {
                if theme::action_button(ui, "instance / preset…", true).clicked() {
                    box_edit = true;
                }
            });
            ui.add_space(4.0);
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
    if let Some(cell) = set_cell {
        let mut grid = fbd_grid_of(plc);
        grid.set_cell(row, col, cell);
        actions.push(Action::SetFbdGrid(grid));
        close = true;
    }
    if let Some(output) = operand_pick {
        // A fresh picker starts with an empty filter: stale text from an earlier
        // placement made the next typed name match nothing (`pumpstopqrun`).
        plc.operand_filter.clear();
        plc.fbd_operand_pick = Some((row, col, output));
        close = true;
    }
    if let Some(kind) = block {
        fbd_place_block(plc, &kind, actions, &mut message);
        close = true;
    }
    if box_edit {
        plc.fbd_block_edit = Some((row, col));
        close = true;
    }
    if let Some(message) = message {
        plc.il_message = Some(message);
    }
    if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
        eprintln!(
            "fbd menu tail: close={} operand={:?} grid_pick={:?} actions={} msg={:?}",
            close,
            plc.fbd_operand_pick,
            plc.fbd_grid_pick,
            actions.len(),
            plc.il_message
        );
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.fbd_grid_pick = None;
    }
}

/// A symbol for an input or output cell.
fn fbd_operand_menu(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((row, col, output)) = plc.fbd_operand_pick else {
        return;
    };
    let mut close = false;
    let mut chosen: Option<String> = None;
    // Keyboard, as the ladder's operand picker has: typed text filters, Enter takes
    // the first match, Escape closes. The list was click-only, so `Enter → I → pump`
    // placed nothing.
    {
        let mut typed = String::new();
        let mut backspace = false;
        let mut enter = false;
        let mut escape = false;
        for event in ctx.input(|input| input.events.clone()) {
            match event {
                egui::Event::Text(text) => typed.push_str(&text),
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    egui::Key::Backspace => backspace = true,
                    egui::Key::Enter => enter = true,
                    egui::Key::Escape => escape = true,
                    _ => {}
                },
                _ => {}
            }
        }
        plc.operand_filter.push_str(&typed);
        if backspace {
            plc.operand_filter.pop();
        }
        if escape {
            plc.fbd_operand_pick = None;
            plc.operand_filter.clear();
            return;
        }
        if enter {
            let needle = plc.operand_filter.trim().to_ascii_lowercase();
            let first = plc
                .project
                .io
                .iter()
                .find(|point| {
                    needle.is_empty() || point.name.to_ascii_lowercase().contains(&needle)
                })
                .map(|point| point.name.clone());
            if let Some(name) = first {
                chosen = Some(name);
            }
        }
    }
    egui::Window::new(if output {
        "FBD OUTPUT CELL"
    } else {
        "FBD INPUT CELL"
    })
    .order(egui::Order::Tooltip)
    .collapsible(false)
    .resizable(true)
    .default_width(320.0)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                let needle = plc.operand_filter.trim().to_ascii_lowercase();
                for point in plc.project.io.clone().into_iter().filter(|point| {
                    needle.is_empty() || point.name.to_ascii_lowercase().contains(&needle)
                }) {
                    if ui
                        .button(format!("{}   {}", point.name, point.comment))
                        .clicked()
                    {
                        chosen = Some(point.name);
                    }
                }
                for var in plc.project.vars.clone() {
                    if ui
                        .button(format!("{}   {}", var.name, var.comment))
                        .clicked()
                    {
                        chosen = Some(var.name);
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    for value in ["TRUE", "FALSE"] {
                        if ui.button(value).clicked() {
                            chosen = Some((*value).to_string());
                        }
                    }
                    for index in 0..16 {
                        if ui.button(format!("MW{index}")).clicked() {
                            chosen = Some(format!("MW{index}"));
                        }
                    }
                });
            });
        ui.add_space(4.0);
        if theme::action_button(ui, "Cancel", true).clicked() {
            close = true;
        }
    });
    if let Some(operand) = chosen {
        let operand = if plc.fbd_negate && !operand.starts_with('!') {
            format!("!{operand}")
        } else {
            operand
        };
        let mut grid = fbd_grid_of(plc);
        grid.set_cell(
            row,
            col,
            if output {
                FbdCell::Out { operand }
            } else {
                FbdCell::In { operand }
            },
        );
        actions.push(Action::SetFbdGrid(grid));
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.fbd_operand_pick = None;
        plc.fbd_negate = false;
        plc.operand_filter.clear();
    }
}

/// Keys the grid owns: arrows move between cells, Enter opens the element list.
fn fbd_keys(plc: &mut PlcUi, ui: &Ui, actions: &mut Vec<Action>) {
    if std::env::var_os("LINC_PLC_KEY_DEBUG").is_some() {
        eprintln!(
            "fbd keys: page={:?} editing={} field={} lang={:?} pick={:?}",
            plc.page,
            plc.text_editing,
            text_field_focused(ui.ctx()),
            plc.current_pou().map(|pou| pou.language),
            plc.fbd_grid_pick
        );
    }
    // An open picker owns the keyboard: without this the grid handled the same
    // Enter that chose an element and re-opened the picker, so nothing was placed.
    if plc.fbd_grid_pick.is_some() || plc.fbd_operand_pick.is_some() {
        return;
    }
    if plc.page != PlcPage::Edit || plc.text_editing || text_field_focused(ui.ctx()) {
        return;
    }
    let is_fbd = plc
        .current_pou()
        .map(|pou| pou.language == PouLanguage::Fbd)
        .unwrap_or(false);
    if !is_fbd {
        return;
    }
    let grid = fbd_grid_of(plc);
    let rows = grid.rows.max(1);
    let columns = grid.columns.max(6);
    let mut cursor = plc.fbd_cursor.unwrap_or((0, 0));
    let (left, right, up, down, enter, delete) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
        )
    });
    if right {
        cursor.1 = (cursor.1 + 1).min(columns - 1);
    }
    if left {
        cursor.1 = cursor.1.saturating_sub(1);
    }
    if down {
        cursor.0 = (cursor.0 + 1).min(rows - 1);
    }
    if up {
        cursor.0 = cursor.0.saturating_sub(1);
    }
    // A block body is not a position: land on its anchor row.
    for block in &grid.blocks {
        if block.col == cursor.1 && cursor.0 > block.row && cursor.0 < block.row + block.ins.max(1)
        {
            cursor.0 = block.row;
        }
    }
    plc.fbd_cursor = Some(cursor);
    if enter {
        match fbd_preset_cell(&grid, cursor.0, cursor.1) {
            Some(anchor) => plc.fbd_block_edit = Some(anchor),
            None => plc.fbd_grid_pick = Some(cursor),
        }
    }
    if delete {
        let mut grid = grid;
        let removed = grid
            .blocks
            .iter()
            .find(|block| block.row == cursor.0 && block.col == cursor.1)
            .cloned();
        match removed {
            Some(block) => {
                grid.blocks
                    .retain(|item| !(item.row == block.row && item.col == block.col));
                let input_col = block.col.saturating_sub(1);
                // The first input's signal goes on through a wire; the rest of
                // what fed the block is removed with it.
                if matches!(grid.cell(block.row, input_col), FbdCell::In { .. }) {
                    grid.set_cell(block.row, input_col, FbdCell::Wire);
                }
                for offset in 1..block.ins.max(1) {
                    grid.set_cell(block.row + offset, input_col, FbdCell::Empty);
                }
                fbd_close_up(&mut grid, block.row, block.ins);
            }
            None => grid.set_cell(cursor.0, cursor.1, FbdCell::Empty),
        }
        actions.push(Action::SetFbdGrid(grid));
    }
}

fn fbd_editor(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    let grid = fbd_grid_of(plc);
    ui.label(
        RichText::new(
            "grid: arrows move · Enter places an element · click a block for instance and preset",
        )
        .monospace()
        .size(theme::font_size(8.5))
        .color(theme::MUTED),
    );
    ui.add_space(2.0);
    fbd_grid_view(plc, &grid, ui, height, actions);
}

#[allow(dead_code)]
fn fbd_notice(ui: &mut Ui) {
    ui.label(
        RichText::new("FBD editor is the next slice: the network model and compiler land first.")
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
    );
}

/// Symbols: the declared I/O and variables with their live values.
fn symbols_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    common::card_title(ui, "SYMBOLS");
    ui.label(
        RichText::new("N NEW · CTRL+I INPUT · CTRL+O OUTPUT · CTRL+V VAR · ENTER EDITS")
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    ui.add_space(4.0);
    let mut rows = plc
        .project
        .io
        .iter()
        .map(|point| {
            (
                point.name.clone(),
                match point.direction {
                    IoDirection::In => "input",
                    IoDirection::Out => "output",
                }
                .to_string(),
                point.comment.clone(),
            )
        })
        .chain(plc.project.vars.iter().map(|var| {
            (
                var.name.clone(),
                if var.retain {
                    "retained var".to_string()
                } else {
                    "var".to_string()
                },
                var.comment.clone(),
            )
        }))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (left.1.as_str(), left.0.as_str()).cmp(&(right.1.as_str(), right.0.as_str()))
    });
    plc.symbol_cursor = plc.symbol_cursor.min(rows.len().saturating_sub(1));
    plc.symbol_column = plc.symbol_column.min(3);

    let mut remove: Option<String> = None;
    let mut rename: Option<(String, String)> = None;
    let mut comment_set: Option<(String, String)> = None;
    let typing = plc.symbol_edit.is_some() || plc.symbol_comment_edit.is_some();
    // Both editable columns use the page's own buffer, like the box preset: no
    // egui TextEdit can lose the ring to the grid anchor.
    if let Some(mut buffer) = plc.symbol_edit.clone() {
        for event in ui.input(|input| input.events.clone()) {
            match event {
                egui::Event::Text(text) => buffer.push_str(&text),
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    egui::Key::Backspace => {
                        buffer.pop();
                    }
                    egui::Key::Enter => {
                        if let Some((name, _, _)) = rows.get(plc.symbol_edit_row) {
                            if !buffer.trim().is_empty() {
                                rename = Some((name.clone(), buffer.clone()));
                            }
                        }
                        plc.symbol_edit = None;
                        plc.text_editing = false;
                    }
                    egui::Key::Escape => {
                        plc.symbol_edit = None;
                        plc.text_editing = false;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if plc.symbol_edit.is_some() {
            plc.symbol_edit = Some(buffer);
        }
    }
    if let Some(mut buffer) = plc.symbol_comment_edit.clone() {
        for event in ui.input(|input| input.events.clone()) {
            match event {
                egui::Event::Text(text) => buffer.push_str(&text),
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    egui::Key::Backspace => {
                        buffer.pop();
                    }
                    egui::Key::Enter => {
                        if let Some((name, _, _)) = rows.get(plc.symbol_comment_row) {
                            comment_set = Some((name.clone(), buffer.clone()));
                        }
                        plc.symbol_comment_edit = None;
                        plc.text_editing = false;
                    }
                    egui::Key::Escape => {
                        plc.symbol_comment_edit = None;
                        plc.text_editing = false;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if plc.symbol_comment_edit.is_some() {
            plc.symbol_comment_edit = Some(buffer);
        }
    }
    if !typing {
        let (up, down, page_up, page_down, left, right, enter, delete) = ui.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
            )
        });
        if up {
            plc.symbol_cursor = plc.symbol_cursor.saturating_sub(1);
        }
        if down {
            plc.symbol_cursor = (plc.symbol_cursor + 1).min(rows.len().saturating_sub(1));
        }
        if page_up {
            plc.symbol_cursor = plc.symbol_cursor.saturating_sub(8);
        }
        if page_down {
            plc.symbol_cursor = (plc.symbol_cursor + 8).min(rows.len().saturating_sub(1));
        }
        if left {
            plc.symbol_column = plc.symbol_column.saturating_sub(1);
        }
        if right {
            plc.symbol_column = (plc.symbol_column + 1).min(3);
        }
        if enter {
            if let Some((name, _, comment)) = rows.get(plc.symbol_cursor) {
                match plc.symbol_column {
                    0 => {
                        plc.symbol_edit_row = plc.symbol_cursor;
                        plc.symbol_edit = Some(name.clone());
                        plc.text_editing = true;
                    }
                    3 => {
                        plc.symbol_comment_row = plc.symbol_cursor;
                        plc.symbol_comment_edit = Some(comment.clone());
                        plc.text_editing = true;
                    }
                    _ => {}
                }
            }
        }
        if delete {
            if let Some((name, _, _)) = rows.get(plc.symbol_cursor) {
                remove = Some(name.clone());
            }
        }
    }

    // Column geometry shared by the header and the rows: one source, so the
    // headings sit over their columns.
    const NAME_W: f32 = 150.0;
    const AREA_W: f32 = 90.0;
    const VALUE_W: f32 = 70.0;
    const GAP: f32 = 10.0;
    let comment_w = (ui.available_width() - NAME_W - AREA_W - VALUE_W - GAP * 3.0 - 10.0).max(80.0);
    let column_x = |rect: egui::Rect, column: usize| -> f32 {
        let start = rect.left() + theme::SELECTION_BAR + 6.0;
        match column {
            0 => start,
            1 => start + NAME_W + GAP,
            2 => start + NAME_W + GAP + AREA_W + GAP,
            _ => start + NAME_W + GAP + AREA_W + GAP + VALUE_W + GAP,
        }
    };
    let widths = [NAME_W, AREA_W, VALUE_W, comment_w];

    let (header_rect, header_response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::click());
    for (column, heading) in ["SYMBOL", "AREA", "VALUE", "COMMENT"].iter().enumerate() {
        let selected = column == plc.symbol_column;
        ui.painter().text(
            egui::pos2(column_x(header_rect, column), header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            *heading,
            egui::FontId::monospace(theme::font_size(9.0)),
            if selected {
                theme::ACCENT
            } else {
                theme::MUTED
            },
        );
        if selected {
            // The active column is underlined in the header, the tool table's
            // way of saying where Enter will act.
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(
                        column_x(header_rect, column),
                        header_rect.bottom() - theme::SELECTION_BAR,
                    ),
                    egui::vec2(widths[column] - GAP, theme::SELECTION_BAR),
                ),
                egui::CornerRadius::same(1),
                theme::ACCENT,
            );
        }
    }
    let _ = header_response;
    ui.separator();

    let list_height = (height - 180.0).max(120.0);
    let mut clicked_row: Option<usize> = None;
    let mut clicked_column: Option<usize> = None;
    egui::ScrollArea::vertical()
        .id_salt("plc-symbols")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, (name, area, comment)) in rows.iter().enumerate() {
                let selected = index == plc.symbol_cursor;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 22.0),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    clicked_row = Some(index);
                    let x = response
                        .interact_pointer_pos()
                        .map(|pos| pos.x - rect.left());
                    clicked_column = x.map(|x| {
                        let mut column = 0;
                        let mut edge = column_x(rect, 0) - rect.left();
                        for (candidate, width) in widths.iter().enumerate() {
                            if x >= edge {
                                column = candidate;
                            }
                            edge += width + GAP;
                        }
                        column
                    });
                }
                let painter = ui.painter().clone();
                if selected {
                    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::PANEL_RAISED);
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(theme::SELECTION_BAR, rect.height()),
                        ),
                        egui::CornerRadius::ZERO,
                        theme::ACCENT,
                    );
                } else if index % 2 == 1 {
                    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::SURFACE);
                }
                // The selected cell carries a faint rim, as the tool table's does.
                if selected {
                    let x = column_x(rect, plc.symbol_column);
                    let width = widths[plc.symbol_column];
                    painter.rect_stroke(
                        egui::Rect::from_min_max(
                            egui::pos2(x - 4.0, rect.top() + 1.0),
                            egui::pos2(x + width - GAP + 2.0, rect.bottom() - 1.0),
                        ),
                        egui::CornerRadius::same(2),
                        egui::Stroke::new(1.0, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }
                let editing_name = plc.symbol_edit.is_some() && plc.symbol_edit_row == index;
                let editing_comment =
                    plc.symbol_comment_edit.is_some() && plc.symbol_comment_row == index;
                let name_text = if editing_name {
                    format!("{}▌", plc.symbol_edit.clone().unwrap_or_default())
                } else {
                    name.clone()
                };
                painter.text(
                    egui::pos2(column_x(rect, 0), rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    name_text,
                    egui::FontId::monospace(theme::font_size(10.0)),
                    if editing_name {
                        theme::ACCENT
                    } else {
                        theme::FG
                    },
                );
                painter.text(
                    egui::pos2(column_x(rect, 1), rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    area,
                    egui::FontId::proportional(theme::font_size(9.0)),
                    theme::MUTED,
                );
                let value = plc
                    .vars
                    .iter()
                    .find(|var| var.name == *name)
                    .map(|var| if var.value { "TRUE" } else { "FALSE" })
                    .unwrap_or("-");
                painter.text(
                    egui::pos2(column_x(rect, 2), rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    value,
                    egui::FontId::monospace(theme::font_size(9.0)),
                    if value == "TRUE" {
                        theme::SUCCESS
                    } else {
                        theme::MUTED
                    },
                );
                let comment_text = if editing_comment {
                    format!("{}▌", plc.symbol_comment_edit.clone().unwrap_or_default())
                } else {
                    comment.clone()
                };
                painter.text(
                    egui::pos2(column_x(rect, 3), rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    comment_text,
                    egui::FontId::proportional(theme::font_size(9.0)),
                    if editing_comment {
                        theme::ACCENT
                    } else {
                        theme::MUTED
                    },
                );
            }
        });
    if let Some(row) = clicked_row {
        plc.symbol_cursor = row;
        if let Some(column) = clicked_column {
            plc.symbol_column = column;
        }
    }

    ui.add_space(6.0);
    // The add field is live only when no table cell is being typed in: otherwise
    // it took the ring (and the letters) from the name or comment being edited.
    let new_field_active =
        plc.text_editing && plc.symbol_edit.is_none() && plc.symbol_comment_edit.is_none();
    ui.horizontal(|ui| {
        // Keyboard-only add: `N` claims the field, typed text goes to the buffer,
        // `I` commits an input and `O` an output, Escape cancels.
        let field_id = egui::Id::unique("linc-new-symbol");
        // `N` opens the new-symbol field only when no table cell is being typed
        // in: otherwise the letter in a name or comment started an add, and a
        // later `i` in the same word committed it as a symbol.
        if !typing && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::N)) {
            plc.text_editing = true;
            ui.memory_mut(|memory| memory.request_focus(field_id));
        }
        // The kind chord is read before the field draws: a plain letter would be
        // part of the name, and Ctrl+V would be eaten by the field's paste.
        let (commit_in, commit_out, commit_var, cancel) = if new_field_active {
            (
                ctrl_key(ui, egui::Key::I),
                ctrl_key(ui, egui::Key::O),
                ctrl_key(ui, egui::Key::V),
                ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)),
            )
        } else {
            (false, false, false, false)
        };
        let mut name = plc.symbol_new.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut name)
                .id(field_id)
                .desired_width(140.0)
                .hint_text("new symbol name"),
        );
        if new_field_active && !response.has_focus() {
            response.request_focus();
        }
        if response.changed() {
            plc.symbol_new = name.clone();
        }
        if new_field_active {
            let trimmed = plc.symbol_new.trim().to_string();
            if commit_in && !trimmed.is_empty() {
                actions.push(Action::AddInput(trimmed));
                plc.symbol_new.clear();
                plc.text_editing = false;
            } else if commit_out && !trimmed.is_empty() {
                actions.push(Action::AddSymbol(trimmed));
                plc.symbol_new.clear();
                plc.text_editing = false;
            } else if commit_var && !trimmed.is_empty() {
                actions.push(Action::AddVar(trimmed));
                plc.symbol_new.clear();
                plc.text_editing = false;
            } else if cancel {
                plc.symbol_new.clear();
                plc.text_editing = false;
            }
        }
    });

    if let Some(name) = remove {
        actions.push(Action::RemoveSymbol(name));
    }
    if let Some((from, to)) = rename {
        actions.push(Action::RenameSymbol { from, to });
    }
    if let Some((name, comment)) = comment_set {
        actions.push(Action::SetSymbolComment(name, comment));
    }
}

/// Simulation: the live program above, the input/output LED panel below.
fn sim_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    // The monitor gets its own bounded scroll area so the I/O controls below are
    // always on screen: with the whole page in one scroll area the ladder pushed
    // them past the fold, and the wheel did not reach them.
    // The ladder takes everything the signal panel does not: a fixed share of
    // the window left most of it empty and cut long programs short.
    let monitor = (height - 132.0).max(180.0);
    // The monitor shows the current POU in its own language. Rendering the ladder
    // unconditionally made an IL or FBD POU read "No sections" while the program was
    // running and the header said otherwise.
    let language = plc
        .current_pou()
        .map(|pou| pou.language)
        .unwrap_or(PouLanguage::Ld);
    let sections = plc.current_pou_sections();
    egui::ScrollArea::vertical()
        .id_salt("plc-sim-monitor")
        .max_height(monitor)
        .auto_shrink([false, false])
        .show(ui, |ui| match language {
            PouLanguage::Ld => {
                ladder_body(plc, &sections, ui, monitor, actions, false);
            }
            PouLanguage::Il => il_editor(plc, ui, monitor),
            PouLanguage::Fbd => fbd_editor(plc, ui, monitor, actions),
        });
    ui.add_space(4.0);
    sim_view(plc, ui, actions);
}

fn watch_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    common::card_title(ui, "WATCH");
    ui.label(
        RichText::new("Ctrl+W on a cell pins its operand. The trend page follows this list.")
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    ui.add_space(4.0);
    if plc.project.watch.is_empty() {
        ui.label(
            RichText::new("Nothing pinned yet.")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
    }
    let mut unpin: Option<String> = None;
    egui::ScrollArea::vertical()
        .max_height((height - 120.0).max(80.0))
        .show(ui, |ui| {
            for name in plc.project.watch.clone() {
                let text = if let Some(value) = plc.words.iter().find(|word| word.name == name) {
                    format!("{}  =  {}", name, value.value)
                } else if let Some(value) = plc.vars.iter().find(|var| var.name == name) {
                    format!(
                        "{}  =  {}",
                        name,
                        if value.value { "TRUE" } else { "FALSE" }
                    )
                } else {
                    format!("{name}  =  -")
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(text)
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::FG),
                    );
                    // BOOL rows can be forced, word rows cannot (manual §11).
                    let forced = plc.forced.iter().any(|item| item == &name);
                    let word = plc.words.iter().any(|word| word.name == name);
                    if !word {
                        let (label, value) = match (
                            forced,
                            plc.vars
                                .iter()
                                .find(|var| var.name == name)
                                .map(|var| var.value),
                        ) {
                            (false, _) => ("force T", Some(true)),
                            (true, Some(true)) => ("force F", Some(false)),
                            (true, _) => ("release", None),
                        };
                        if theme::action_button(ui, label, true).clicked() {
                            match value {
                                Some(value) => actions.push(Action::Force(name.clone(), value)),
                                None => actions.push(Action::Unforce(name.clone())),
                            }
                        }
                    }
                    if theme::action_button(ui, "unpin", true).clicked() {
                        unpin = Some(name.clone());
                    }
                });
            }
        });
    if let Some(name) = unpin {
        actions.push(Action::ToggleWatch(name));
    }
    ui.add_space(6.0);
    variables_card(plc, ui, height, actions)
}

fn trend_page(plc: &PlcUi, ui: &mut Ui) {
    if !plc.trend_remote.samples.is_empty() {
        draw_remote_trend(plc, ui);
        return;
    }
    let trend = &plc.trend;
    common::card_title(ui, "TREND");
    if trend.channels.is_empty() || trend.samples == 0 {
        ui.label(
            RichText::new(
                "No samples yet. Start the PLC: the first eight symbols and four block values are traced.",
            )
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
        return;
    }
    ui.label(
        RichText::new(format!(
            "{} samples, {} lanes, newest at the right",
            trend.samples,
            trend.channels.len()
        ))
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.add_space(4.0);
    let width = ui.available_width();
    let lane_height = 26.0;
    for (name, lane) in trend.channels.iter().zip(trend.lanes.iter()) {
        let (rect, _) = ui.allocate_exact_size(vec2(width, lane_height), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, theme::SURFACE);
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        painter.text(
            pos2(rect.left() + 4.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::monospace(theme::font_size(9.0)),
            theme::MUTED,
        );
        let plot_left = rect.left() + 92.0;
        let plot_right = rect.right() - 4.0;
        let height = rect.height() - 6.0;
        let step = (plot_right - plot_left) / TrendBuffer::DEPTH as f32;
        let offset = TrendBuffer::DEPTH.saturating_sub(lane.len()) as f32 * step;
        let mut points = Vec::with_capacity(lane.len());
        for (index, value) in lane.iter().enumerate() {
            let x = plot_left + offset + index as f32 * step;
            let y = if *value {
                rect.top() + 3.0
            } else {
                rect.top() + 3.0 + height
            };
            points.push(pos2(x, y));
        }
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                Stroke::new(
                    1.2,
                    if lane.last().copied().unwrap_or(false) {
                        theme::SUCCESS
                    } else {
                        theme::C1
                    },
                ),
            ));
        }
    }
}

/// The daemon's per-scan window: one lane per watch entry, words auto-scaled.
fn draw_remote_trend(plc: &PlcUi, ui: &mut Ui) {
    let data = &plc.trend_remote;
    common::card_title(ui, "TREND");
    ui.label(
        RichText::new(format!(
            "{} samples over {} s (every {} scans), newest at the right",
            data.samples.len(),
            plc.trend_window_ms / 1000,
            data.stride.max(1)
        ))
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.add_space(4.0);
    let width = ui.available_width();
    let lane_height = 26.0;
    for (index, name) in data.names.iter().enumerate() {
        let column = data
            .samples
            .iter()
            .map(|row| row.get(index).copied().unwrap_or(0))
            .collect::<Vec<i32>>();
        let (rect, _) = ui.allocate_exact_size(vec2(width, lane_height), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, theme::SURFACE);
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        if data.words.get(index).copied().unwrap_or(false) {
            let low = column.iter().copied().min().unwrap_or(0);
            let high = column.iter().copied().max().unwrap_or(0);
            painter.text(
                pos2(rect.left() + 4.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{} {low}..{high}", plc.operand_label(name)),
                egui::FontId::monospace(theme::font_size(8.5)),
                theme::MUTED,
            );
            let step = (rect.right() - 4.0 - (rect.left() + 110.0)) / 512.0;
            let offset = 512usize.saturating_sub(column.len()) as f32 * step;
            let span = (high - low).max(1) as f32;
            let height = (rect.height() - 6.0).max(1.0);
            let points = column
                .iter()
                .enumerate()
                .map(|(position, value)| {
                    let x = rect.left() + 110.0 + offset + position as f32 * step;
                    let y = rect.bottom() - 3.0 - (*value - low) as f32 / span * height;
                    pos2(x, y)
                })
                .collect::<Vec<_>>();
            if points.len() > 1 {
                painter.add(egui::Shape::line(points, Stroke::new(1.2, theme::C2)));
            }
        } else {
            painter.text(
                pos2(rect.left() + 4.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                plc.operand_label(name),
                egui::FontId::monospace(theme::font_size(9.0)),
                theme::MUTED,
            );
            let step = (rect.right() - 4.0 - (rect.left() + 92.0)) / 512.0;
            let offset = 512usize.saturating_sub(column.len()) as f32 * step;
            let points = column
                .iter()
                .enumerate()
                .map(|(position, value)| {
                    let x = rect.left() + 92.0 + offset + position as f32 * step;
                    let y = if *value != 0 {
                        rect.top() + 3.0
                    } else {
                        rect.bottom() - 3.0
                    };
                    pos2(x, y)
                })
                .collect::<Vec<_>>();
            if points.len() > 1 {
                painter.add(egui::Shape::line(points, Stroke::new(1.2, theme::C1)));
            }
        }
    }
}

/// The simulation panel: one LED per declared I/O point. A click on an input
/// forces it for the daemon's force TTL, which is how the ZX simulation's LED
/// panel sets an input. Outputs are read-only.
/// The configured input at `bit` of the selected byte, if that slot is bound. The
/// panel shows unbound slots disabled rather than inventing names, as the manual's
/// 16 fixed inputs do.

/// The manual's 16 input bit positions and the output row: a bound slot shows the
/// operand name and its value, an unbound one is drawn disabled. The selected byte
/// has the brighter paper, and a click toggles a bound input.
fn sim_bit_panel(plc: &PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    for (title, direction) in [("IN", IoDirection::In), ("OUT", IoDirection::Out)] {
        let points = plc
            .project
            .io
            .iter()
            .filter(|point| point.direction == direction)
            .collect::<Vec<_>>();
        // An empty slot says nothing and took a row of space: only bound points
        // are drawn, numbered by their position among their own direction.
        if points.is_empty() {
            continue;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.allocate_ui_with_layout(
                egui::vec2(24.0, 36.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(title)
                            .monospace()
                            .size(theme::font_size(8.0))
                            .color(theme::MUTED),
                    );
                },
            );
            for (index, point) in points.iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(38.0, 36.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 2.0, theme::PANEL);
                let value = plc
                    .vars
                    .iter()
                    .find(|var| var.name == point.name)
                    .map(|var| var.value)
                    .unwrap_or(false);
                let forced = plc.is_forced(&point.name);
                let colour = if value {
                    if direction == IoDirection::In {
                        theme::C4
                    } else {
                        theme::C5
                    }
                } else {
                    theme::MUTED
                };
                // Bit number above, lamp in the middle, name below it: the label
                // used to be painted across the lamp.
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.top() + 7.0),
                    Align2::CENTER_CENTER,
                    format!("{index}"),
                    FontId::monospace(7.0),
                    theme::MUTED,
                );
                ui.painter().circle_filled(
                    egui::pos2(rect.center().x, rect.top() + 17.0),
                    3.5,
                    colour,
                );
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.bottom() - 2.0),
                    Align2::CENTER_BOTTOM,
                    if forced {
                        format!("F {}", point.name)
                    } else {
                        point.name.clone()
                    },
                    FontId::monospace(6.5),
                    colour,
                );
                if direction == IoDirection::In && response.clicked() {
                    actions.push(Action::Force(point.name.clone(), !value));
                }
            }
        });
        ui.add_space(2.0);
    }
}

fn sim_view(plc: &PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    common::card_title(ui, "TEST RUN");
    // The 16-bit panel is the control surface: the named button row below it said
    // the same thing twice and is gone.
    sim_bit_panel(plc, ui, actions);
}

/// ZX-PLC's status bar: the symbol and comment of the selection on the left,
/// free space and the run state on the right.
/// Rename or copy a POU: the name is typed once and applied on Enter.
fn pou_edit_dialog(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(mut edit) = plc.pou_edit.clone() else {
        return;
    };
    let mut close = false;
    let mut apply: Option<String> = None;
    egui::Window::new(if edit.duplicate {
        "DUPLICATE POU"
    } else {
        "RENAME POU"
    })
    // Above the page: the PLC screen's cards are painted in the shell layer and
    // a default-order window ends up underneath them.
    .order(egui::Order::Tooltip)
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        ui.set_min_width(260.0);
        ui.label(
            RichText::new("POU NAME, UP TO 8 CHARACTERS")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        // The field writes into the dialog's own buffer: a fresh String per
        // frame would throw every typed character away before the next draw.
        let response = ui.add(
            egui::TextEdit::singleline(&mut edit.name)
                .id(egui::Id::unique("linc-plc-pou-name"))
                .desired_width(220.0)
                .char_limit(8),
        );
        // The window owns the keyboard while it is up, so the name can be typed
        // straight away. A singleline field keeps focus on Enter in egui, so the
        // apply is a plain key test rather than a lost-focus one.
        if !response.has_focus() {
            response.request_focus();
        }
        if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            apply = Some(edit.name.clone());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::action_button(ui, "Apply", true).clicked() {
                apply = Some(edit.name.clone());
            }
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
        ui.label(
            RichText::new("ENTER APPLIES · ESC CANCELS")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
    if let Some(name) = apply {
        let name = name.trim().to_string();
        if !name.is_empty() {
            actions.push(if edit.duplicate {
                Action::DuplicatePou {
                    index: edit.index,
                    name,
                }
            } else {
                Action::RenamePou {
                    index: edit.index,
                    name,
                }
            });
        }
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.pou_edit = None;
    } else {
        plc.pou_edit = Some(edit);
    }
}

/// Copy the marked block (or the cell under the cursor) without a keyboard.
fn status_bar(plc: &PlcUi, ui: &mut Ui) {
    // The cell under the cursor wins, because that is what ZX shows; with no
    // cursor the selected rung's coil and its comment stand in.
    let sections = plc.current_pou_sections();
    let rung_at = |section: usize, rung: usize| {
        sections
            .get(section)
            .and_then(|section| section.rungs.get(rung))
    };
    let selected = plc
        .cell_cursor
        .and_then(|(section, rung, position)| {
            let rung = rung_at(section, rung)?;
            let operand = if position >= POSITIONS {
                Some(rung.coil.var.clone())
            } else {
                rung.cells
                    .get(position)
                    .and_then(|cell| cell.operand())
                    .map(|operand| operand.to_string())
            };
            operand.map(|operand| {
                let comment = plc.symbol_comment(&operand).unwrap_or("").to_string();
                (operand, comment)
            })
        })
        .or_else(|| {
            let node = plc.selection.as_ref()?;
            let rung = rung_at(node.section, node.rung)?;
            Some((rung.coil.var.clone(), rung.comment.clone()))
        });
    let (state_label, state_color) = run_state_label(plc.state);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        match &selected {
            Some((operand, comment)) => {
                common::nav_badge(ui, &plc.operand_label(operand), theme::ACCENT);
                if !comment.is_empty() {
                    ui.label(
                        RichText::new(comment.clone())
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                }
            }
            None => {
                ui.label(
                    RichText::new("no selection")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            }
        }
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "scan {} us ({}..{}) · cycle {} us · late {} us · OVERRUN {}",
                    plc.timing.scan.now_us,
                    plc.timing.scan.min_us,
                    plc.timing.scan.max_us,
                    plc.timing.cycle.now_us,
                    plc.timing.latency.now_us,
                    plc.timing.overruns,
                ))
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            ui.add_space(8.0);
            common::nav_badge(ui, state_label, state_color);
        });
    });
}

/// The ladder canvas over an explicit section list.
/// A row that holds only a network comment. Clicking it edits the text in
/// place, as the manual's `//` rows do; Enter stores, Escape cancels.
fn comment_row(
    plc: &mut PlcUi,
    ui: &mut Ui,
    section: usize,
    rung: usize,
    row: &Rung,
    actions: &mut Vec<Action>,
) {
    if plc.comment_row == Some((section, rung)) {
        let response = ui.add(
            egui::TextEdit::singleline(&mut plc.comment_buffer)
                .desired_width((ui.available_width() - 8.0).max(120.0))
                .hint_text("network comment"),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            actions.push(Action::SetRungComment {
                section,
                rung,
                comment: plc.comment_buffer.trim().to_string(),
            });
            plc.comment_row = None;
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            plc.comment_row = None;
        }
        return;
    }
    let text = if row.comment.trim().is_empty() {
        "//".to_string()
    } else if row.comment.trim_start().starts_with("//") {
        row.comment.clone()
    } else {
        format!("// {}", row.comment)
    };
    let response = ui.add(
        egui::Label::new(
            RichText::new(text)
                .monospace()
                .size(theme::font_size(9.5))
                .color(theme::COMMENT),
        )
        .sense(Sense::click()),
    );
    if response.clicked() {
        plc.comment_row = Some((section, rung));
        plc.comment_buffer = row
            .comment
            .trim()
            .trim_start_matches("//")
            .trim_start()
            .to_string();
    }
}

/// Make consecutive rungs exactly one strip apart. Returns the spacing it set, so a
/// caller (or a test) can assert the ladder is not inheriting egui's default.
pub fn zero_rung_spacing(ui: &mut Ui) -> f32 {
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing().item_spacing.y
}

fn ladder_body(
    plc: &mut PlcUi,
    sections: &[linc_plc_core::model::Section],
    ui: &mut Ui,
    height: f32,
    actions: &mut Vec<Action>,
    follow_cursor: bool,
) -> Option<egui::Rect> {
    if sections.is_empty() {
        ui.label(
            RichText::new("No sections. Add rung / Add section starts one.")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        return None;
    }
    let mut cursor_rect = None;
    egui::ScrollArea::both()
        .id_salt("plc-ladder")
        .max_height((height - 6.0).max(80.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The rungs stack themselves: `grid_rung` is exactly CELL_HEIGHT tall
            // and the link's drop assumes that pitch, so egui's default vertical
            // item spacing must not add to it.
            zero_rung_spacing(ui);
            for (section_index, section) in sections.iter().enumerate() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("{} · {}", section.name, section.rungs.len()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::C3),
                );
                for (rung_index, rung) in section.rungs.iter().enumerate() {
                    if rung.is_comment_row() {
                        comment_row(plc, ui, section_index, rung_index, rung, actions);
                        continue;
                    }
                    // One model: every rung is cells. `normalise` lays a tree
                    // out on the way in, and a rung that still has none is
                    // shown as an empty row rather than in a second editor.
                    crate::screens::plc_grid::grid_rung(
                        plc,
                        ui,
                        section_index,
                        rung_index,
                        rung,
                        actions,
                        &mut cursor_rect,
                    );
                }
            }
            // The scroll belongs to this area, not to the pane around it: the
            // cursor rect lives in this content, and scrolling the outer clip
            // with it moved nothing.
            if follow_cursor {
                if let Some(rect) = cursor_rect {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            }
        });
    cursor_rect
}

/// A card with an exact rect, sized by the caller so the page's columns end on
/// the same line. The 44 px target is restored here: the shared card shell sets
/// the dense 14 px row height for toolpath cards.
fn card<R>(ui: &mut Ui, size: Vec2, add: impl FnOnce(&mut Ui, f32) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    common::shell_card(ui, rect, |ui| {
        ui.spacing_mut().interact_size.y = theme::MIN_TARGET;
        ui.spacing_mut().item_spacing.y = 4.0;
        add(ui, size.y)
    })
}

fn status_card(plc: &mut PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    let (state_label, state_color) = run_state_label(plc.state);
    let banner = plc.banner.clone();
    let enabled = plc.enabled;
    let project_name = plc.project.name.clone();
    let online = plc.online;
    let (dot_text, dot_color) = if !enabled {
        ("PLC DISABLED", theme::WARNING)
    } else if online {
        ("SERVICE ONLINE", theme::SUCCESS)
    } else {
        ("SERVICE OFFLINE", theme::ERROR)
    };
    let status_text = if !enabled {
        format!("disabled in the profile — {DISABLED_HINT}")
    } else if banner.is_empty() {
        "no daemon banner".to_string()
    } else {
        banner
    };
    let running = matches!(plc.state, RunState::Running);
    common::card_title(ui, "PLC SERVICE");
    // Facts get their own line. Sharing one row with a right-to-left block is
    // what made the badge, the banner and the counters paint over each other.
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        theme::status_dot(ui, dot_color, dot_text, &status_text);
        ui.add_space(10.0);
        common::nav_badge(ui, state_label, state_color);
    });
    ui.label(
        RichText::new(status_text)
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    ui.add_space(2.0);
    ui.label(
        RichText::new(format!(
            "{}  ·  SCAN {}  ·  {} us  ·  OVERRUN {}  ·  {}",
            project_name,
            plc.scan_count,
            plc.scan_time_us,
            plc.overruns,
            if plc.alive {
                "HEARTBEAT HEALTHY"
            } else {
                "HEARTBEAT IDLE"
            }
        ))
        .monospace()
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.add_space(6.0);
    // The ladder's only controls are its mode: loading is part of START, and a
    // running ladder stops when asked. The socket, the project path and the
    // program files are the application's business; the operator sees the
    // program list and the daemon's own state here.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if theme::accent_button(ui, "START", online && !running).clicked() {
            actions.push(Action::Start);
        }
        if theme::action_button(ui, "STOP", online && running).clicked() {
            actions.push(Action::Stop);
        }
    });
    ui.label(
        RichText::new(if running {
            "the ladder is scanning · output follows the program"
        } else {
            "START loads the program and runs it"
        })
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.label(
        RichText::new("Ctrl+Enter START · Ctrl+Shift+Enter STOP")
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
}

#[allow(dead_code)]
fn draw_rung(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung_index: usize,
    rung: &Rung,
    actions: &mut Vec<Action>,
) {
    let root = NodePath::root(section, rung_index);
    let logic_size = vec2(logic_width(&rung.logic), logic_height(&rung.logic));
    let row_height = (logic_size.y + 6.0).max(BOX.y + 6.0);
    let (row_rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("plc-rung", section, rung_index))
            .max_rect(row_rect)
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    row.set_clip_rect(row_rect.intersect(ui.clip_rect()));
    row.spacing_mut().item_spacing.x = 0.0;

    // Left power rail: one tick per rung, so the column reads as a rail.
    let (number_rect, _) = row.allocate_exact_size(vec2(28.0, row_height), Sense::hover());
    row.painter().text(
        pos2(number_rect.right() - 4.0, number_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{:>3}", rung_index + 1),
        common::font_mono(9.0),
        theme::LINE_NUMBER,
    );
    let rail_x = number_rect.right() - 1.0;
    let wire = Stroke::new(1.5, theme::BORDER);
    row.painter().line_segment(
        [
            pos2(rail_x, row_rect.top()),
            pos2(rail_x, row_rect.bottom()),
        ],
        wire,
    );

    let (logic_rect, _) = row.allocate_exact_size(vec2(logic_size.x, row_height), Sense::hover());
    let block = egui::Rect::from_center_size(logic_rect.center(), logic_size);
    row.painter().line_segment(
        [
            pos2(rail_x, block.center().y),
            pos2(block.left(), block.center().y),
        ],
        wire,
    );
    paint_logic(&mut row, block, plc, &rung.logic, &root, actions);

    let _ = row.allocate_space(vec2(RAIL_GAP, BOX.y));
    let (coil_rect, _) = row.allocate_exact_size(vec2(BOX.x, row_height), Sense::hover());
    let coil_box = egui::Rect::from_center_size(coil_rect.center(), BOX);
    row.painter().line_segment(
        [
            pos2(block.right(), coil_box.center().y),
            pos2(coil_box.left(), coil_box.center().y),
        ],
        wire,
    );

    let live = plc.live_value(&rung.coil.var).unwrap_or(false);
    let selected = plc.selection.as_ref() == Some(&root);
    let mut coil = row.new_child(
        egui::UiBuilder::new()
            .id_salt(("plc-coil", section, rung_index))
            .max_rect(coil_box),
    );
    let response = coil.add_sized(
        BOX,
        ladder_box(
            &format!("{}\n{}", rung.coil.var, coil_label(rung.coil.kind)),
            live,
            selected,
            plc.is_forced(&rung.coil.var),
        ),
    );
    if response.clicked() {
        actions.push(Action::Select(root.clone()));
    }
    if !rung.comment.is_empty() {
        row.painter().text(
            pos2(coil_box.right() + 8.0, coil_box.center().y),
            egui::Align2::LEFT_CENTER,
            &rung.comment,
            common::font_mono(9.0),
            theme::COMMENT,
        );
    }
}

/// Recursively paint a logic block into its rect. Leaves are real buttons, so
/// the arrows reach them and Enter activates the one that holds focus.
fn paint_logic(
    parent: &mut Ui,
    rect: egui::Rect,
    plc: &PlcUi,
    logic: &Logic,
    node: &NodePath,
    actions: &mut Vec<Action>,
) {
    let wire = Stroke::new(1.5, theme::BORDER);
    match logic {
        Logic::Series { items } => {
            let mut x = rect.left();
            let mut previous_right: Option<f32> = None;
            for (index, item) in items.iter().enumerate() {
                let size = vec2(logic_width(item), logic_height(item));
                let child =
                    egui::Rect::from_center_size(pos2(x + size.x * 0.5, rect.center().y), size);
                if let Some(right) = previous_right {
                    parent.painter().line_segment(
                        [
                            pos2(right, child.center().y),
                            pos2(child.left(), child.center().y),
                        ],
                        wire,
                    );
                }
                paint_logic(parent, child, plc, item, &node.child(index), actions);
                previous_right = Some(child.right());
                x = child.right() + RAIL_GAP;
            }
        }
        Logic::Parallel { items } => {
            let left_rail = rect.left() + PARALLEL_INSET * 0.5;
            let right_rail = rect.right() - PARALLEL_INSET * 0.5;
            let mut y = rect.top();
            let mut centers = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let size = vec2(logic_width(item), logic_height(item));
                let child = egui::Rect::from_min_size(pos2(rect.left() + PARALLEL_INSET, y), size);
                parent.painter().line_segment(
                    [
                        pos2(left_rail, child.center().y),
                        pos2(child.left(), child.center().y),
                    ],
                    wire,
                );
                parent.painter().line_segment(
                    [
                        pos2(child.right(), child.center().y),
                        pos2(right_rail, child.center().y),
                    ],
                    wire,
                );
                paint_logic(parent, child, plc, item, &node.child(index), actions);
                centers.push(child.center().y);
                y = child.bottom() + BRANCH_GAP;
            }
            if let (Some(first), Some(last)) = (centers.first(), centers.last()) {
                parent
                    .painter()
                    .line_segment([pos2(left_rail, *first), pos2(left_rail, *last)], wire);
                parent
                    .painter()
                    .line_segment([pos2(right_rail, *first), pos2(right_rail, *last)], wire);
            }
        }
        Logic::Block { .. } | Logic::Edge { .. } | Logic::Compare { .. } => {
            // Blocks, edges and compares are drawn as a named box; the tree
            // canvas is only a fallback for rungs the grid cannot show.
            let label = match logic {
                Logic::Block {
                    block, instance, ..
                } => {
                    if instance.is_empty() {
                        block.clone()
                    } else {
                        format!("{block} {instance}")
                    }
                }
                Logic::Edge { var, rising } => {
                    format!("{}{var}", if *rising { "P " } else { "N " })
                }
                Logic::Compare {
                    operator,
                    left,
                    right,
                } => format!("{left}{operator}{right}"),
                _ => String::new(),
            };
            let live = plc.live_value(label.as_str()).unwrap_or(false);
            let selected = plc.selection.as_ref() == Some(node);
            let mut child = parent.new_child(
                egui::UiBuilder::new()
                    .id_salt(("plc-element", node.clone()))
                    .max_rect(rect),
            );
            let response = child.add_sized(BOX, ladder_box(&label, live, selected, false));
            if response.clicked() {
                actions.push(Action::Select(node.clone()));
            }
        }
        Logic::Contact { var, negated } => {
            let live = plc.live_value(var).unwrap_or(false);
            let selected = plc.selection.as_ref() == Some(node);
            let label = if *negated {
                format!("/{var}")
            } else {
                var.clone()
            };
            let mut child = parent.new_child(
                egui::UiBuilder::new()
                    .id_salt(("plc-contact", node.clone()))
                    .max_rect(rect),
            );
            let response =
                child.add_sized(BOX, ladder_box(&label, live, selected, plc.is_forced(var)));
            if response.clicked() {
                actions.push(Action::Select(node.clone()));
            }
            if response.double_clicked() {
                actions.push(Action::ToggleContact(node.clone()));
            }
        }
        Logic::Const { value } => {
            let selected = plc.selection.as_ref() == Some(node);
            let mut child = parent.new_child(
                egui::UiBuilder::new()
                    .id_salt(("plc-const", node.clone()))
                    .max_rect(rect),
            );
            let label = if *value { "1" } else { "0" };
            let response = child.add_sized(BOX, ladder_box(label, *value, selected, false));
            if response.clicked() {
                actions.push(Action::Select(node.clone()));
            }
            if response.double_clicked() {
                actions.push(Action::ToggleConstant(node.clone()));
            }
        }
    }
}

/// One ladder box: filled while its variable is true, outlined while selected,
/// tinted while forced.
fn ladder_box(label: &str, live: bool, selected: bool, forced: bool) -> egui::Button<'static> {
    let fill = if forced {
        theme::tint(theme::WARNING, 56)
    } else if live {
        theme::tint(theme::SUCCESS, 44)
    } else {
        theme::PANEL_RAISED
    };
    let stroke = if selected {
        Stroke::new(2.0, theme::ACCENT)
    } else if forced {
        Stroke::new(1.5, theme::WARNING)
    } else {
        Stroke::new(1.0, theme::BORDER)
    };
    egui::Button::new(
        RichText::new(label.to_string())
            .monospace()
            .size(theme::font_size(10.0))
            .color(if live { theme::SUCCESS } else { theme::FG }),
    )
    .fill(fill)
    .stroke(stroke)
    .corner_radius(egui::CornerRadius::same(4))
    .min_size(BOX)
}

#[allow(dead_code)]
fn property_card(plc: &mut PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    common::card_title(ui, "SELECTED RUNG / NODE");
    let Some(node) = plc.selection.clone() else {
        ui.label(
            RichText::new("Nothing selected. Click a rung to edit it.")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        return;
    };
    // The buffers follow the selection, never the other way round: a frame that
    // re-synced on every draw would fight the operator's typing.
    if plc.buffer_key.as_ref() != Some(&node) {
        plc.sync_buffers();
    }
    let logic = logic_at(&plc.project, &node).cloned();
    let coil = rung_at(&plc.project, &node).map(|rung| rung.coil.clone());
    let kind = match &logic {
        Some(Logic::Contact { negated, .. }) => {
            if *negated {
                "CONTACT NC"
            } else {
                "CONTACT NO"
            }
        }
        Some(Logic::Const { .. }) => "CONSTANT",
        Some(Logic::Series { .. }) => "SERIES",
        Some(Logic::Parallel { .. }) => "PARALLEL",
        Some(Logic::Block { .. }) => "BLOCK",
        Some(Logic::Edge { .. }) => "EDGE",
        Some(Logic::Compare { .. }) => "COMPARE",
        None => "RUNG",
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(node.label())
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::FG),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(kind)
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });

    if matches!(logic, Some(Logic::Contact { .. })) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("VAR")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            let width = (ui.available_width() - 110.0).max(60.0);
            let response = ui.add_sized(
                [width, theme::MIN_TARGET],
                egui::TextEdit::singleline(&mut plc.var_text)
                    .id(egui::Id::unique("linc-plc-var"))
                    .font(egui::FontId::monospace(theme::font_size(10.0))),
            );
            theme::focus_ring(ui, &response);
            if response.clicked() {
                plc.text_editing = true;
            }
            if response.lost_focus() {
                plc.text_editing = false;
            }
            if response.changed() {
                actions.push(Action::SetVariable(plc.var_text.clone()));
            }
            let negated = matches!(&logic, Some(Logic::Contact { negated: true, .. }));
            if theme::action_button(ui, if negated { "NO" } else { "NC" }, true).clicked() {
                actions.push(Action::ToggleContact(node.clone()));
            }
        });
    } else if let Some(Logic::Const { value }) = &logic {
        if theme::action_button(ui, if *value { "VALUE 1" } else { "VALUE 0" }, true).clicked() {
            actions.push(Action::ToggleConstant(node.clone()));
        }
    }

    if let Some(coil) = coil {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("COIL")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            let width = (ui.available_width() - 230.0).max(60.0);
            let response = ui.add_sized(
                [width, theme::MIN_TARGET],
                egui::TextEdit::singleline(&mut plc.coil_text)
                    .id(egui::Id::unique("linc-plc-coil"))
                    .font(egui::FontId::monospace(theme::font_size(10.0))),
            );
            theme::focus_ring(ui, &response);
            if response.clicked() {
                plc.text_editing = true;
            }
            if response.lost_focus() {
                plc.text_editing = false;
            }
            if response.changed() {
                actions.push(Action::SetCoilVariable(plc.coil_text.clone()));
            }
            let active = coil_index(&coil.kind);
            for (index, candidate) in [CoilKind::Normal, CoilKind::Set, CoilKind::Reset]
                .into_iter()
                .enumerate()
            {
                let is_active = index == active;
                let response = ui.add_sized(
                    [60.0, theme::MIN_TARGET],
                    egui::Button::new(
                        RichText::new(coil_label(candidate))
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(if is_active { theme::FG } else { theme::MUTED }),
                    )
                    .fill(if is_active {
                        theme::tint(theme::ACCENT, 60)
                    } else {
                        theme::PANEL_RAISED
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if is_active {
                            theme::ACCENT
                        } else {
                            theme::BORDER
                        },
                    )),
                );
                theme::focus_ring(ui, &response);
                if response.clicked() && !is_active {
                    actions.push(Action::SetCoilKind(candidate));
                }
            }
        });
    }

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("NOTE")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        let response = ui.add_sized(
            [ui.available_width(), theme::MIN_TARGET],
            egui::TextEdit::singleline(&mut plc.comment_text)
                .id(egui::Id::unique("linc-plc-comment"))
                .font(egui::FontId::monospace(theme::font_size(10.0))),
        );
        theme::focus_ring(ui, &response);
        if response.changed() {
            actions.push(Action::SetComment(plc.comment_text.clone()));
        }
    });

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if theme::action_button(ui, "SERIES", true).clicked() {
            actions.push(Action::WrapSeries);
        }
        if theme::action_button(ui, "PARALLEL", true).clicked() {
            actions.push(Action::WrapParallel);
        }
        if theme::danger_button(ui, "DELETE", true).clicked() {
            actions.push(Action::Delete);
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if theme::action_button(ui, "ADD RUNG", true).clicked() {
            actions.push(Action::AddRung);
        }
        if theme::action_button(ui, "MOVE UP", true).clicked() {
            actions.push(Action::MoveUp);
        }
        if theme::action_button(ui, "MOVE DOWN", true).clicked() {
            actions.push(Action::MoveDown);
        }
    });
}

struct VariableRow {
    name: String,
    kind: &'static str,
    detail: String,
}

/// The project declares the variables; the daemon's snapshot supplies the
/// values. A project variable missing from the snapshot shows a dash, which is
/// what an offline page looks like.
fn variable_rows(plc: &PlcUi) -> Vec<VariableRow> {
    let mut rows = Vec::new();
    for point in &plc.project.io {
        rows.push(VariableRow {
            name: point.name.clone(),
            kind: match &point.direction {
                IoDirection::In => "IN",
                IoDirection::Out => "OUT",
            },
            detail: String::new(),
        });
    }
    for var in &plc.project.vars {
        rows.push(VariableRow {
            name: var.name.clone(),
            kind: "VAR",
            detail: if var.retain {
                "retained".to_string()
            } else {
                String::new()
            },
        });
    }
    rows
}

fn variables_card(plc: &PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    common::card_title(ui, "VARIABLES / I/O");
    let rows = variable_rows(plc);
    let list_height = (height - 84.0).max(44.0);
    egui::ScrollArea::vertical()
        .id_salt("plc-vars")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                let armed = plc.armed.as_deref() == Some(row.name.as_str());
                let live = plc.live_value(&row.name);
                let forced = plc.is_forced(&row.name);
                let value = match live {
                    Some(true) => "1",
                    Some(false) => "0",
                    None => "-",
                };
                let mut text = format!("{:<10}{:<4}{:<5}{}", row.name, value, row.kind, row.detail);
                if forced {
                    text.push_str(" FORCED");
                }
                let response = ui.add_sized(
                    [ui.available_width(), theme::MIN_TARGET],
                    egui::Button::new(
                        RichText::new(text)
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(if armed {
                                theme::ACCENT
                            } else if matches!(live, Some(true)) {
                                theme::SUCCESS
                            } else {
                                theme::FG
                            }),
                    )
                    .fill(if armed {
                        theme::tint(theme::ACCENT, 48)
                    } else {
                        theme::PANEL_RAISED
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if armed { theme::ACCENT } else { theme::BORDER },
                    )),
                );
                theme::focus_ring(ui, &response);
                if response.clicked() {
                    actions.push(Action::Arm(row.name.clone()));
                }
            }
        });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let armed = plc.armed.clone();
        let can_force = plc.online && armed.is_some();
        if theme::action_button(ui, "FORCE ON", can_force).clicked() {
            if let Some(name) = armed.clone() {
                actions.push(Action::Force(name, true));
            }
        }
        if theme::action_button(ui, "FORCE OFF", can_force).clicked() {
            if let Some(name) = armed {
                actions.push(Action::Force(name, false));
            }
        }
        if theme::action_button(ui, "CLEAR FORCES", plc.online && !plc.forced.is_empty()).clicked()
        {
            actions.push(Action::ClearForces);
        }
    });
}

fn diagnostics_card(plc: &PlcUi, ui: &mut Ui, height: f32) {
    common::card_title(ui, "DIAGNOSTICS");
    let list_height = (height - 34.0).max(44.0);
    egui::ScrollArea::vertical()
        .id_salt("plc-diagnostics")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !plc.enabled {
                ui.label(
                    RichText::new(format!("PLC DISABLED IN THE PROFILE — {DISABLED_HINT}"))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::WARNING),
                );
            }
            if let Some(error) = &plc.last_error {
                ui.label(
                    RichText::new(format!("LINK {error}"))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::ERROR),
                );
            }
            if !plc.message.is_empty() {
                ui.label(
                    RichText::new(plc.message.clone())
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            }
            if let Some(fault) = &plc.fault {
                ui.label(
                    RichText::new(format!("FAULT {fault}"))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::ERROR),
                );
            }
            if !plc.hash.is_empty() {
                ui.label(
                    RichText::new(format!("HASH {}", plc.hash))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::C1),
                );
            }
            for diagnostic in &plc.local_diags {
                diagnostic_row(ui, "LOCAL", diagnostic);
            }
            if plc.daemon_diags.is_empty() && plc.local_diags.is_empty() {
                ui.label(
                    RichText::new("No diagnostics. START checks the program before loading it.")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            }
            for diagnostic in &plc.daemon_diags {
                diagnostic_row(ui, "DAEMON", diagnostic);
            }
        });
}

fn diagnostic_row(ui: &mut Ui, source: &str, diagnostic: &Diagnostic) {
    let (label, color) = match diagnostic.severity {
        Severity::Error => ("ERROR", theme::ERROR),
        Severity::Warning => ("WARN", theme::WARNING),
    };
    let path = if diagnostic.path.is_empty() {
        String::new()
    } else {
        format!(" · {}", diagnostic.path)
    };
    ui.label(
        RichText::new(format!(
            "{source} {label} {} {}{path}",
            diagnostic.code, diagnostic.message
        ))
        .monospace()
        .size(theme::font_size(9.0))
        .color(color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        Project::example()
    }

    fn first_rung(project: &Project) -> &Rung {
        &project.sections[0].rungs[0]
    }

    fn contact_var(logic: &Logic) -> Option<&str> {
        match logic {
            Logic::Contact { var, .. } => Some(var.as_str()),
            _ => None,
        }
    }

    #[test]
    fn a_new_rung_lands_after_the_selected_one_and_starts_on_a_declared_io() {
        let mut project = project();
        let before = project.sections[0].rungs.len();
        let added = add_rung(&mut project, 0, Some(0)).expect("a rung");
        assert_eq!(project.sections[0].rungs.len(), before + 1);
        assert_eq!(added.rung, 1);
        assert_eq!(added.section, 0);
        assert_eq!(added.path, Vec::<usize>::new());
        let rung = &project.sections[0].rungs[1];
        assert_eq!(contact_var(&rung.logic), Some("start"));
        assert_eq!(rung.coil.var, "run");
        // Appending is the same as after the last rung.
        let appended = add_rung(&mut project, 0, None).expect("a rung");
        assert_eq!(appended.rung, before + 1);
    }

    #[test]
    fn deleting_a_rung_root_removes_the_rung_and_deleting_a_node_keeps_it() {
        let mut project = project();
        let count = project.sections[0].rungs.len();
        assert!(delete_node(&mut project, &NodePath::root(0, 0)));
        assert_eq!(
            project.sections[0].rungs.len(),
            count - 1,
            "a rung root removes the whole rung"
        );
        // The rung that moved up is the seal contact; wrapping it and deleting
        // the new contact leaves the original node in place.
        assert_eq!(contact_var(&first_rung(&project).logic), Some("seal"));
        assert!(wrap_series(&mut project, &NodePath::root(0, 0)));
        let added = NodePath::root(0, 0).child(1);
        assert!(delete_node(&mut project, &added));
        assert_eq!(contact_var(&first_rung(&project).logic), Some("seal"));
        assert!(
            !delete_node(&mut project, &NodePath::root(0, 9)),
            "an index past the end is refused"
        );
    }

    #[test]
    fn wrapping_keeps_the_original_logic_as_the_first_item() {
        let mut project = project();
        let var = default_input_var(&project);
        // The example's second rung is a bare contact, so the wrap is visible.
        let target = NodePath::root(0, 1);
        assert_eq!(
            contact_var(logic_at(&project, &target).unwrap()),
            Some("seal")
        );
        assert!(wrap_series(&mut project, &target));
        match logic_at(&project, &target) {
            Some(Logic::Series { items }) => {
                assert_eq!(items.len(), 2);
                assert_eq!(contact_var(&items[0]), Some("seal"));
                assert_eq!(contact_var(&items[1]), Some(var.as_str()));
                match &items[1] {
                    Logic::Contact { negated, .. } => assert!(!negated),
                    other => panic!("expected a contact, got {other:?}"),
                }
            }
            other => panic!("expected a series, got {other:?}"),
        }
        // Wrapping a series again is refused: the block is already a chain.
        assert!(!wrap_series(&mut project, &target));
    }

    #[test]
    fn a_nested_node_can_be_wrapped_and_its_new_branch_deleted() {
        let mut project = project();
        let nested = NodePath::root(0, 0).child(0);
        assert_eq!(
            match logic_at(&project, &nested) {
                Some(Logic::Parallel { items }) => items.len(),
                other => panic!("expected a parallel, got {other:?}"),
            },
            2
        );
        // The block is already a parallel, so a parallel wrap is refused; a
        // series wrap of the same node is what the operator gets instead.
        assert!(!wrap_parallel(&mut project, &nested));
        assert!(wrap_series(&mut project, &nested));
        match logic_at(&project, &nested) {
            Some(Logic::Series { items }) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], Logic::Parallel { .. }));
                assert_eq!(contact_var(&items[1]), Some("start"));
            }
            other => panic!("expected a nested series, got {other:?}"),
        }
        // Deleting the branch this test added collapses the wrapper and hands
        // the original parallel back untouched.
        assert!(delete_node(&mut project, &nested.child(1)));
        match logic_at(&project, &nested) {
            Some(Logic::Parallel { items }) => {
                assert_eq!(items.len(), 2);
                assert_eq!(contact_var(&items[0]), Some("start"));
                assert_eq!(contact_var(&items[1]), Some("seal"));
            }
            other => panic!("expected the original parallel back, got {other:?}"),
        }
    }

    #[test]
    fn a_parallel_wrap_adds_a_visible_branch_and_a_branch_can_be_deleted() {
        let mut project = project();
        let target = NodePath::root(0, 1);
        assert!(wrap_parallel(&mut project, &target));
        match logic_at(&project, &target) {
            Some(Logic::Parallel { items }) => assert_eq!(items.len(), 2),
            other => panic!("expected a parallel, got {other:?}"),
        }
        assert!(delete_node(&mut project, &target.child(1)));
        assert_eq!(
            contact_var(logic_at(&project, &target).unwrap()),
            Some("seal")
        );
    }

    #[test]
    fn toggling_negation_flips_a_contact_and_ignores_a_series() {
        let mut project = project();
        let contact = NodePath::root(0, 1);
        assert!(toggle_negated(&mut project, &contact));
        assert!(matches!(
            logic_at(&project, &contact),
            Some(Logic::Contact { negated: true, .. })
        ));
        assert!(toggle_negated(&mut project, &contact));
        assert!(matches!(
            logic_at(&project, &contact),
            Some(Logic::Contact { negated: false, .. })
        ));
        // A series carries no negation of its own.
        assert!(!toggle_negated(&mut project, &NodePath::root(0, 0)));
        // The example's normally-closed stop contact flips back to open.
        let stop = NodePath::root(0, 0).child(1);
        assert!(matches!(
            logic_at(&project, &stop),
            Some(Logic::Contact { negated: true, .. })
        ));
        assert!(toggle_negated(&mut project, &stop));
        assert!(matches!(
            logic_at(&project, &stop),
            Some(Logic::Contact { negated: false, .. })
        ));
    }

    #[test]
    fn a_constant_toggles_between_zero_and_one() {
        let mut project = project();
        let root = NodePath::root(0, 0);
        if let Some(slot) = logic_at_mut(&mut project, &root) {
            *slot = Logic::Const { value: false };
        }
        assert!(toggle_constant(&mut project, &root));
        assert!(matches!(
            logic_at(&project, &root),
            Some(Logic::Const { value: true })
        ));
        assert!(toggle_constant(&mut project, &root));
        assert!(matches!(
            logic_at(&project, &root),
            Some(Logic::Const { value: false })
        ));
    }

    #[test]
    fn moving_a_rung_swaps_it_with_its_neighbour_and_stops_at_the_ends() {
        let mut project = project();
        let count = project.sections[0].rungs.len();
        assert!(count >= 3, "the example has three rungs");
        assert_eq!(move_rung(&mut project, 0, 0, 1), Some(1));
        assert_eq!(
            move_rung(&mut project, 0, count - 1, 1),
            None,
            "the last rung cannot move down"
        );
        assert_eq!(move_rung(&mut project, 0, 1, -1), Some(0));
        assert_eq!(
            move_rung(&mut project, 0, 0, -1),
            None,
            "the first rung cannot move up"
        );
        assert_eq!(project.sections[0].rungs.len(), count);
    }

    #[test]
    fn adding_a_section_extends_the_project() {
        let mut project = project();
        let before = project.sections.len();
        let index = add_section(&mut project);
        assert_eq!(index, before);
        assert_eq!(project.sections.len(), before + 1);
        assert!(project.sections[index].rungs.is_empty());
        let added = add_rung(&mut project, index, None).expect("a rung");
        assert_eq!(added.section, index);
        assert_eq!(added.rung, 0);
    }

    #[test]
    fn a_project_round_trips_through_the_saved_json() {
        let project = project();
        let text = project_to_json(&project).expect("serialise");
        assert!(text.starts_with('{'));
        assert!(text.contains('\n'), "the file is written pretty-printed");
        let parsed = project_from_json(&text).expect("deserialise");
        assert_eq!(
            project_to_json(&parsed).expect("reserialise"),
            text,
            "a saved project must load back unchanged"
        );
    }

    #[test]
    fn the_project_path_follows_xdg_config_home_then_home() {
        let xdg = project_path_from(Some(PathBuf::from("/cfg")), Some(PathBuf::from("/home/u")));
        assert_eq!(xdg, PathBuf::from("/cfg/linc/plc.json"));
        let home = project_path_from(None, Some(PathBuf::from("/home/u")));
        assert_eq!(home, PathBuf::from("/home/u/.config/linc/plc.json"));
        let empty = project_path_from(Some(PathBuf::new()), None);
        assert_eq!(empty, PathBuf::from("./linc/plc.json"));
    }

    #[test]
    fn every_box_clears_the_minimum_hit_target() {
        assert!(BOX.x >= theme::MIN_TARGET);
        assert!(BOX.y >= theme::MIN_TARGET);
        let series = Logic::Series {
            items: vec![
                Logic::Contact {
                    var: "X0".into(),
                    negated: false,
                },
                Logic::Contact {
                    var: "X1".into(),
                    negated: true,
                },
            ],
        };
        assert!(logic_width(&series) >= 2.0 * BOX.x);
        assert!(logic_height(&series) >= BOX.y);
        let parallel = Logic::Parallel {
            items: vec![series.clone(), series],
        };
        assert!(logic_height(&parallel) >= 2.0 * BOX.y);
        assert!(logic_width(&parallel) >= BOX.x + 2.0 * PARALLEL_INSET);
    }

    #[test]
    fn the_state_badge_covers_every_run_state() {
        for state in [
            RunState::Idle,
            RunState::Loaded,
            RunState::Running,
            RunState::Fault,
        ] {
            let (label, _) = run_state_label(state);
            assert!(!label.is_empty());
        }
        assert_eq!(coil_label(CoilKind::Normal), "( )");
        assert_eq!(coil_label(CoilKind::Set), "(S)");
        assert_eq!(coil_label(CoilKind::Reset), "(R)");
    }

    #[test]
    fn the_worker_dials_the_socket_and_the_page_adopts_what_the_daemon_says() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::TempDir::new().expect("a temp dir");
        let path = directory.path().join("plc-test.sock");
        let listener = UnixListener::bind(&path).expect("bind the fake daemon");
        let (seen_tx, seen_rx) = mpsc::channel::<String>();

        // A fake daemon: one connection, answer everything, report each request
        // it saw so the test can check what the page actually sent.
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("a read timeout");
            let snapshot = Response::State(StateSnapshot {
                state: RunState::Running,
                project: "example".into(),
                hash: "cafe".into(),
                scan_count: 7,
                scan_time_us: 42,
                overruns: 1,
                alive: true,
                fault: None,
                timing: Default::default(),
                vars: vec![VarValue {
                    name: "seal".into(),
                    value: true,
                    kind: linc_plc_core::SymbolKind::Var,
                    forced: false,
                }],
                words: Vec::new(),
                forced: vec!["start".into()],
            });
            while let Ok(request) = linc_plc_core::protocol::read_message::<_, Request>(&mut stream)
            {
                let reply = match request {
                    Request::Hello { .. } => {
                        let _ = seen_tx.send("hello".into());
                        Response::Hello {
                            protocol: PROTOCOL_VERSION,
                            schema: 1,
                            daemon: "fake".into(),
                            hal: false,
                        }
                    }
                    Request::Snapshot => {
                        let _ = seen_tx.send("snapshot".into());
                        snapshot.clone()
                    }
                    Request::Force {
                        name,
                        value,
                        ttl_ms,
                    } => {
                        let _ = seen_tx.send(format!("force {name}={value} ttl {ttl_ms}"));
                        snapshot.clone()
                    }
                    other => {
                        let _ = seen_tx.send(format!("{other:?}"));
                        Response::Bye
                    }
                };
                if linc_plc_core::protocol::write_message(&mut stream, &reply).is_err() {
                    break;
                }
            }
        });

        let mut plc = PlcUi::default();
        plc.socket_text = path.display().to_string();
        plc.dial();
        // Drive frames until the daemon's state has been adopted.
        for _ in 0..50 {
            plc.poll();
            if matches!(plc.state, RunState::Running) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(plc.online, "the page must report the daemon as online");
        assert!(matches!(plc.state, RunState::Running));
        assert_eq!(plc.hash, "cafe");
        assert_eq!(plc.scan_count, 7);
        assert_eq!(plc.scan_time_us, 42);
        assert_eq!(plc.overruns, 1);
        assert!(plc.alive, "the heartbeat health flag comes from the daemon");
        assert_eq!(plc.live_value("seal"), Some(true));
        assert!(plc.is_forced("start"));
        assert!(plc.banner.contains("fake"));

        // A force travels as the documented request, with the page's TTL.
        plc.apply(Action::Force("seal".to_string(), true));
        for _ in 0..10 {
            plc.poll();
            thread::sleep(Duration::from_millis(20));
        }
        drop(plc);
        assert!(server.join().is_ok(), "the fake daemon finished cleanly");

        let mut seen = Vec::new();
        while let Ok(request) = seen_rx.try_recv() {
            seen.push(request);
        }
        assert!(seen.iter().any(|request| request == "hello"), "{seen:?}");
        assert!(
            seen.iter().any(|request| request == "snapshot"),
            "the page polls for live values: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|request| request == "force seal=true ttl 0"),
            "a simulation force is persistent until released: {seen:?}"
        );
    }

    #[test]
    fn the_live_values_follow_the_snapshot_but_the_rows_follow_the_project() {
        let mut plc = PlcUi::default();
        assert_eq!(plc.live_value("seal"), None, "offline: no snapshot yet");
        assert!(!plc.is_forced("seal"));
        plc.vars = vec![VarValue {
            name: "seal".into(),
            value: true,
            kind: linc_plc_core::SymbolKind::Var,
            forced: false,
        }];
        plc.forced = vec!["seal".into()];
        assert_eq!(plc.live_value("seal"), Some(true));
        assert!(plc.is_forced("seal"));
        // A variable the daemon has not reported yet still gets a row.
        let rows = variable_rows(&plc);
        assert_eq!(rows.len(), plc.project.io.len() + plc.project.vars.len());
        assert!(rows
            .iter()
            .any(|row| row.name == "start" && row.kind == "IN"));
        assert!(rows
            .iter()
            .any(|row| row.name == "run" && row.kind == "OUT"));
        assert!(rows
            .iter()
            .any(|row| row.name == "alarm" && row.kind == "VAR"));
    }
}

#[cfg(test)]
mod fbd_grid_tests {
    use super::*;

    fn grid_with_input() -> FbdGrid {
        let mut grid = FbdGrid {
            columns: 8,
            rows: 1,
            cells: Vec::new(),
            blocks: Vec::new(),
        };
        grid.set_cell(
            0,
            2,
            FbdCell::In {
                operand: "a".into(),
            },
        );
        grid.set_cell(
            1,
            2,
            FbdCell::In {
                operand: "b".into(),
            },
        );
        grid
    }

    #[test]
    fn a_timer_spans_two_rows_and_a_counter_three() {
        assert_eq!(fbd_block_rows("NOT"), 1);
        assert_eq!(fbd_block_rows("RT0"), 1);
        assert_eq!(fbd_block_rows("TON0"), 2);
        assert_eq!(fbd_block_rows("CTU0"), 3);
        assert_eq!(fbd_block_rows("AND"), 2);
    }

    #[test]
    fn an_occupied_cell_makes_room_by_moving_the_network_right() {
        let mut grid = grid_with_input();
        // A block sits where the new one wants to go.
        grid.blocks.push(linc_plc_core::model::FbdBlock {
            row: 0,
            col: 3,
            ins: 2,
            kind: "AND".into(),
            instance: String::new(),
            preset: String::new(),
        });
        grid.set_cell(
            0,
            4,
            FbdCell::Out {
                operand: "y".into(),
            },
        );
        assert!(!fbd_room(&grid, 0, 3, 2), "the cell is taken");
        assert!(fbd_shift_right(&mut grid, 0, 2, 3, FBD_BLOCK_WIDTH));
        assert_eq!(grid.blocks[0].col, 5, "the old block moved right");
        assert!(
            matches!(grid.cell(0, 2), FbdCell::In { .. }),
            "the input that fed the network stayed put"
        );
        assert!(fbd_room(&grid, 0, 3, 2));
    }

    #[test]
    fn a_full_grid_refuses_without_moving_anything() {
        let mut grid = grid_with_input();
        grid.blocks.push(linc_plc_core::model::FbdBlock {
            row: 0,
            col: 5,
            ins: 2,
            kind: "AND".into(),
            instance: String::new(),
            preset: String::new(),
        });
        let before = grid.clone();
        assert!(!fbd_shift_right(&mut grid, 0, 2, 5, FBD_BLOCK_WIDTH));
        assert_eq!(grid.blocks, before.blocks, "nothing changed");
        assert_eq!(grid.cells, before.cells, "nothing changed");
    }

    #[test]
    fn deleting_a_block_leaves_its_first_input_on_a_wire() {
        let mut grid = grid_with_input();
        grid.blocks.push(linc_plc_core::model::FbdBlock {
            row: 0,
            col: 3,
            ins: 2,
            kind: "AND".into(),
            instance: String::new(),
            preset: String::new(),
        });
        grid.blocks
            .retain(|block| !(block.row == 0 && block.col == 3));
        grid.set_cell(0, 2, FbdCell::Wire);
        for offset in 1..2 {
            grid.set_cell(offset, 2, FbdCell::Empty);
        }
        fbd_close_up(&mut grid, 0, 2);
        assert!(matches!(grid.cell(0, 2), FbdCell::Wire));
        assert!(matches!(grid.cell(1, 2), FbdCell::Empty));
    }

    #[test]
    fn a_timers_preset_cell_is_the_second_row_on_its_left() {
        let mut grid = grid_with_input();
        grid.blocks.push(linc_plc_core::model::FbdBlock {
            row: 0,
            col: 3,
            ins: 2,
            kind: "TON0".into(),
            instance: String::new(),
            preset: "T#1s".into(),
        });
        assert_eq!(fbd_preset_cell(&grid, 1, 2), Some((0, 3)));
        assert_eq!(fbd_preset_cell(&grid, 0, 2), None);
    }
}

#[cfg(test)]
mod coil_pick_tests {
    use super::*;

    /// The picker's commit path: choosing a name writes the coil operand of the
    /// row it was opened for, and closes the picker.
    #[test]
    fn choosing_a_coil_operand_sets_it_and_closes_the_picker() {
        let mut plc = PlcUi::default();
        plc.coil_pick = Some((0, 0));
        plc.apply(Action::SetCoilVar {
            section: 0,
            rung: 0,
            var: "fan".to_string(),
        });
        plc.coil_pick = None;
        let rungs = plc.current_pou_sections();
        assert_eq!(rungs[0].rungs[0].coil.var, "fan");
        assert!(plc.coil_pick.is_none(), "the picker closes on a pick");
    }
}

#[cfg(test)]
mod negated_contact_tests {
    use super::*;

    /// The chain behind `X`: the picker's cell keeps its negation, `SetCell`
    /// stores it, and the grid draws the `/` that tells the operator so.
    #[test]
    fn a_negated_contact_survives_the_picker_and_draws_a_slash() {
        let picked = crate::screens::plc_grid::with_operand(
            &Cell::Contact {
                var: String::new(),
                negated: true,
            },
            "estop",
        );
        assert_eq!(
            picked,
            Cell::Contact {
                var: "estop".into(),
                negated: true
            }
        );

        let mut plc = PlcUi::default();
        plc.apply(Action::SetCell {
            section: 0,
            rung: 0,
            position: 2,
            cell: picked.clone(),
        });
        let rungs = plc.current_pou_sections();
        assert_eq!(
            rungs[0].rungs[0].cells[2], picked,
            "the cell keeps its negation in the project"
        );

        assert_eq!(
            crate::screens::plc_grid::grid_cell_label(&plc, &picked),
            "/estop",
            "a negated contact is drawn with its slash"
        );
    }
}

#[cfg(test)]
mod block_field_tests {
    use super::*;

    /// The box window patches the cell it owns: a placed block keeps its cell
    /// and only its instance and preset change, so no stale whole-cell write can
    /// replace it.
    #[test]
    fn patching_a_block_keeps_the_cell_and_sets_preset() {
        let mut plc = PlcUi::default();
        plc.apply(Action::SetCell {
            section: 0,
            rung: 0,
            position: 1,
            cell: Cell::Block {
                block: "TOF".into(),
                instance: "TOF0".into(),
                preset: "T#1s".into(),
            },
        });
        plc.apply(Action::SetBlockField {
            section: 0,
            rung: 0,
            position: 1,
            instance: None,
            preset: Some("T#5s".into()),
        });
        let rungs = plc.current_pou_sections();
        assert_eq!(
            rungs[0].rungs[0].cells[1],
            Cell::Block {
                block: "TOF".into(),
                instance: "TOF0".into(),
                preset: "T#5s".into()
            }
        );
    }
}

#[cfg(test)]
mod picker_key_tests {
    use super::*;
    use linc_plc_core::{IoDirection, IoKind, IoPoint};

    fn plc_with_symbols() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.io = vec![
            IoPoint {
                name: "pump".into(),
                direction: IoDirection::In,
                kind: IoKind::Bit,
                comment: String::new(),
            },
            IoPoint {
                name: "estop".into(),
                direction: IoDirection::In,
                kind: IoKind::Bit,
                comment: String::new(),
            },
        ];
        plc
    }

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            crate::screens::plc_grid::pickers(plc, ctx, &mut actions);
        });
        output.textures_delta.clear();
        actions
    }

    fn text(value: &str) -> Vec<egui::Event> {
        vec![egui::Event::Text(value.to_string())]
    }

    /// A letter pressed while the element list is open picks the item, exactly
    /// as clicking it does. Before this the list ignored the keyboard, so the
    /// manual's element letters looked dead whenever the list was up.
    #[test]
    fn a_letter_opens_the_operand_list_from_the_element_picker() {
        let mut plc = plc_with_symbols();
        plc.picker = Some((0, 0, 1));
        let _ = frame(&mut plc, text("c"));
        assert!(plc.picker.is_none(), "element list closed after the letter");
        match plc.operand_pick {
            Some((_, _, position, Cell::Contact { var, negated })) => {
                assert_eq!(position, 1);
                assert_eq!(var, "pump");
                assert!(!negated);
            }
            ref other => panic!("expected a contact pick, got {other:?}"),
        }
    }

    /// Escape closes a picker, not the workbench: the early editor-close handler
    /// used to take it first and strand `coil_pick` with no window.
    #[test]
    fn escape_does_not_close_the_editor_while_a_picker_is_open() {
        let mut plc = plc_with_symbols();
        plc.coil_pick = Some((0, 0));
        assert!(!escape_closes_editor(&plc), "picker keeps the editor open");
        let _ = frame(
            &mut plc,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(plc.coil_pick.is_none(), "the picker took the Escape");
        assert!(
            escape_closes_editor(&plc),
            "and now Escape closes the editor"
        );
    }

    /// Escape closes every picker before the editor's own Escape: a list left
    /// open used to keep its window while the page still believed in it, and the
    /// keyboard went nowhere.
    #[test]
    fn escape_closes_every_picker() {
        let mut plc = plc_with_symbols();
        plc.picker = Some((0, 0, 1));
        plc.operand_pick = Some((0, 0, 1, Cell::Wire));
        plc.coil_pick = Some((0, 0));
        plc.operand_filter = "half".into();
        let _ = frame(
            &mut plc,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(plc.picker.is_none());
        assert!(plc.operand_pick.is_none());
        assert!(plc.coil_pick.is_none());
        assert!(plc.operand_filter.is_empty());
    }

    /// A state flag whose window did not draw is stale, and the page drops it:
    /// nothing can render or dismiss such a picker, so keeping it only makes the
    /// editor deaf. A drawn window is kept.
    #[test]
    fn only_an_undrawn_picker_is_stale() {
        use crate::screens::plc_grid::picker_is_stale;
        assert!(
            picker_is_stale(true, false),
            "state without a window is stale"
        );
        assert!(!picker_is_stale(true, true), "a drawn picker stays");
        assert!(!picker_is_stale(false, false), "no state, nothing to drop");
    }

    /// The coil list commits on Enter too: at a coil position `C` sets the
    /// normal kind and opens this list, so typing the output and pressing Enter
    /// is the whole gesture.
    #[test]
    fn the_coil_list_commits_the_typed_output() {
        let mut plc = plc_with_symbols();
        plc.project.io.push(IoPoint {
            name: "run".into(),
            direction: IoDirection::Out,
            kind: IoKind::Bit,
            comment: String::new(),
        });
        plc.coil_pick = Some((0, 0));
        let mut events = text("run");
        events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let actions = frame(&mut plc, events);
        assert!(plc.coil_pick.is_none(), "coil list closed");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SetCoilVar { var, .. } if var == "run")),
            "no coil action; filter {:?}",
            plc.operand_filter
        );
    }

    /// The list is a table with a cursor: the arrows walk it and Enter takes the
    /// row, which is the keyboard path the buttons never had.
    #[test]
    fn the_arrows_walk_the_operand_list() {
        let mut plc = plc_with_symbols();
        plc.picker = Some((0, 0, 1));
        let _ = frame(&mut plc, text("c"));
        assert!(plc.operand_pick.is_some(), "contact list open");
        let key = |key: egui::Key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        assert_eq!(plc.operand_cursor, 1, "the second row is under the cursor");
        let actions = frame(&mut plc, key(egui::Key::Enter));
        let cell = actions
            .iter()
            .find_map(|action| match action {
                Action::SetCell { cell, .. } => Some(cell.clone()),
                _ => None,
            })
            .expect("the highlighted row was taken");
        assert_eq!(
            cell,
            Cell::Contact {
                var: "estop".into(),
                negated: false
            }
        );
    }

    /// Typing narrows the operand list and Enter commits the typed name on the
    /// frame it arrives, with the kind the letter chose.
    #[test]
    fn typing_then_enter_commits_the_typed_operand() {
        let mut plc = plc_with_symbols();
        plc.picker = Some((0, 0, 1));
        let _ = frame(&mut plc, text("x"));
        assert!(plc.operand_pick.is_some(), "negated contact list open");
        let mut events = text("estop");
        events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let actions = frame(&mut plc, events);
        assert!(
            !actions.is_empty(),
            "no action; filter was {:?}, pick {}",
            plc.operand_filter,
            plc.operand_pick.is_some()
        );
        let cell = actions
            .iter()
            .find_map(|action| match action {
                Action::SetCell { cell, position, .. } => {
                    assert_eq!(*position, 1);
                    Some(cell.clone())
                }
                _ => None,
            })
            .expect("a cell was set");
        assert_eq!(
            cell,
            Cell::Contact {
                var: "estop".into(),
                negated: true
            }
        );
    }
}

#[cfg(test)]
mod add_rung_tests {
    use super::*;

    /// A new row must land in the POU list the compiler reads, and the legacy
    /// `sections` must mirror it for a single MAIN ladder. When they diverged,
    /// rows appeared in the editor and never reached the program.
    #[test]
    fn adding_a_rung_keeps_both_representations_and_the_compiler_in_step() {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let before = plc.project.all_pous()[0].sections[0].rungs.len();
        plc.selection = Some(NodePath::root(0, 0));
        plc.apply(Action::AddRung);
        let pous = plc.project.all_pous();
        assert_eq!(pous[0].sections[0].rungs.len(), before + 1, "POU grew");
        assert_eq!(
            plc.project.sections[0].rungs.len(),
            pous[0].sections[0].rungs.len(),
            "legacy sections mirror the POU"
        );
        // The compiler walks `all_pous`, so agreement between the POU list and
        // the legacy mirror is what keeps the editor and the program in step.
        let compiled = linc_plc_core::compile::compile(&plc.project);
        let diagnostics = compiled.err().unwrap_or_default();
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "pou_unreachable"),
            "the added row is part of the reachable program"
        );
    }
}

#[cfg(test)]
mod delete_rung_tests {
    use super::*;

    fn with_rows(rows: usize) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        for _ in 1..rows {
            plc.selection = Some(NodePath::root(0, rows - 2));
            plc.apply(Action::AddRung);
        }
        plc
    }

    /// A delete writes the POU list and the legacy mirror together, like a
    /// row insert does, so the compiler and the editor cannot drift.
    #[test]
    fn deleting_a_rung_keeps_pous_and_sections_equal() {
        let mut plc = with_rows(5);
        let before = plc.project.all_pous()[0].sections[0].rungs.len();
        plc.apply(Action::DeleteRung {
            section: 0,
            rung: 3,
        });
        let after = plc.project.all_pous()[0].sections[0].rungs.len();
        assert_eq!(after, before - 1);
        assert_eq!(
            plc.project.sections[0].rungs.len(),
            after,
            "legacy sections mirror the POU"
        );
    }

    /// The row under a Link is that link's branch: deleting it would join the
    /// link to an unrelated row, so the delete is refused.
    #[test]
    fn a_rung_under_a_link_is_not_deleted() {
        let mut plc = with_rows(3);
        let mut pous = plc.project.all_pous();
        pous[0].sections[0].rungs[0].cells = vec![
            Cell::Contact {
                var: "a".into(),
                negated: false,
            },
            Cell::Link,
        ];
        plc.project.set_pous(pous);
        let before = plc.project.all_pous()[0].sections[0].rungs.len();
        plc.apply(Action::DeleteRung {
            section: 0,
            rung: 1,
        });
        assert_eq!(
            plc.project.all_pous()[0].sections[0].rungs.len(),
            before,
            "the branch row stays"
        );
    }
}

#[cfg(test)]
mod rail_spacing_tests {
    use super::*;

    /// The rail spaces itself: rows sit a hair apart inside a group, groups get a
    /// real gap, and nothing is left to egui's default vertical spacing.
    #[test]
    fn the_shortcut_rail_spaces_its_own_rows() {
        assert_eq!(RAIL_INNER_GAP, theme::NAV_ROW_GAP);
        assert!(
            RAIL_GROUP_GAP > RAIL_INNER_GAP * 2.0,
            "a group gap reads as a gap, not as a row"
        );
        assert_eq!(theme::NAV_ROW_GAP, 2.0);
    }
}

#[cfg(test)]
mod sim_keys_tests {
    use super::*;

    fn plc_with_inputs() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.io = (0..10)
            .map(|index| linc_plc_core::IoPoint {
                name: format!("in{index}"),
                direction: linc_plc_core::IoDirection::In,
                kind: linc_plc_core::IoKind::Bit,
                comment: String::new(),
            })
            .collect();
        plc
    }
}

#[cfg(test)]
mod il_editor_tests {
    use super::*;

    fn il_plc() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let mut pous = plc.project.all_pous();
        pous[0].language = PouLanguage::Il;
        pous[0].il = Vec::new();
        plc.project.set_pous(pous);
        plc.page = PlcPage::Edit;
        plc.pou = 0;
        plc.il_cursor = 0;
        plc.il_buffer.clear();
        plc
    }

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("il-keys"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    il_keys(plc, ui, &mut Vec::new());
                });
        });
        output.textures_delta.clear();
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// One ENTER commits one line. Before this the typed text stayed in the buffer
    /// and the next line was glued onto it (`LD pumpST run`).
    #[test]
    fn enter_commits_one_line_and_leaves_an_empty_buffer() {
        let mut plc = il_plc();
        for events in [
            vec![egui::Event::Text("LD".into())],
            vec![egui::Event::Text(" ".into())],
            vec![egui::Event::Text("pump".into())],
            vec![key(egui::Key::Enter)],
            vec![egui::Event::Text("ST".into())],
            vec![egui::Event::Text(" ".into())],
            vec![egui::Event::Text("run".into())],
            vec![key(egui::Key::Enter)],
        ] {
            frame(&mut plc, events);
        }
        let lines = plc.il_lines();
        assert_eq!(lines[0], "LD pump", "first line kept as typed");
        assert_eq!(lines[1], "ST run", "second line is its own line, not glued");
        assert!(
            plc.il_buffer.is_empty(),
            "the buffer is empty after a commit"
        );
    }

    /// Backspace edits the line being typed, however many times it is pressed.
    #[test]
    fn backspace_clears_the_typed_text() {
        let mut plc = il_plc();
        frame(&mut plc, vec![egui::Event::Text("LD pumq".into())]);
        let before = plc.il_buffer.chars().count();
        for _ in 0..4 {
            frame(&mut plc, vec![key(egui::Key::Backspace)]);
        }
        assert_eq!(
            plc.il_buffer.chars().count(),
            before - 4,
            "four presses remove four characters: {:?}",
            plc.il_buffer
        );
        frame(&mut plc, vec![egui::Event::Text("pump".into())]);
        assert_eq!(plc.il_buffer, "LD pump");
    }
}

#[cfg(test)]
mod menu_ownership_tests {
    use super::*;

    fn il_plc() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let mut pous = plc.project.all_pous();
        pous[0].language = PouLanguage::Il;
        pous[0].il = vec!["LD pump".to_string()];
        plc.project.set_pous(pous);
        plc.page = PlcPage::Edit;
        plc.il_cursor = 1;
        plc.il_buffer.clear();
        plc.menu_open = true;
        plc
    }

    /// While a menu is open the menu owns arrows and Enter: the IL line editor must
    /// not consume them, or File > New can never be confirmed from the keyboard.
    #[test]
    fn an_open_menu_keeps_enter_and_arrows_from_the_editor() {
        let mut plc = il_plc();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![
                egui::Event::Text("ST".into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("menu-ownership"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    if !plc.menu_open {
                        il_keys(&mut plc, ui, &mut Vec::new());
                    }
                });
        });
        output.textures_delta.clear();
        assert!(
            plc.il_buffer.is_empty(),
            "the editor took text while a menu was open"
        );
        assert_eq!(plc.il_lines()[0], "LD pump", "the stored line is untouched");
    }

    /// With no menu open the same keys do reach the editor, so the guard is the
    /// menu and not a permanent mute.
    #[test]
    fn without_a_menu_the_editor_still_owns_the_keys() {
        let mut plc = il_plc();
        plc.menu_open = false;
        // An empty POU, so the synced buffer starts empty and the assertion is
        // about the typed text alone.
        let mut pous = plc.project.all_pous();
        pous[0].il = Vec::new();
        plc.project.set_pous(pous);
        plc.il_cursor = 0;
        plc.il_buffer.clear();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Text("ST".into())],
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("no-menu"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    il_keys(&mut plc, ui, &mut Vec::new());
                });
        });
        output.textures_delta.clear();
        assert_eq!(plc.il_buffer, "ST");
    }
}

#[cfg(test)]
mod launcher_tests {
    use super::*;

    /// Ctrl+E opens the workbench from the keyboard, so acceptance does not hinge on
    /// the pointer landing inside the OPEN EDITOR button.
    #[test]
    fn ctrl_e_is_the_keyboard_launcher() {
        let ctx = egui::Context::default();
        let mut opened = false;
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::E,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("launcher"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::E))
                    {
                        opened = true;
                    }
                });
        });
        output.textures_delta.clear();
        assert!(opened, "Ctrl+E must set the open flag");
    }
}

#[cfg(test)]
mod open_frame_tests {
    use super::*;

    /// The frame that opens the workbench must not also close it: the click or chord
    /// can arrive with a stale Escape in the same input batch.
    #[test]
    fn an_escape_on_the_opening_frame_does_not_close_the_workbench() {
        let mut plc = PlcUi::default();
        plc.opened_frame = 7;
        // Same frame as the open: the guard must refuse to close.
        let same_frame_guard = |frame: u64, opened: u64| frame <= opened;
        assert!(
            same_frame_guard(7, plc.opened_frame),
            "opening frame is protected"
        );
        // A later frame may close it, so the workbench is not stuck open.
        assert!(
            !same_frame_guard(8, plc.opened_frame),
            "the next frame may close"
        );
    }
}

#[cfg(test)]
mod pou_keys_tests {
    use super::*;

    /// A new project is empty: `File > New` must not bring the example ladder.
    #[test]
    fn a_new_project_is_empty() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        let pous = plc.project.all_pous();
        assert_eq!(pous.len(), 1, "one MAIN");
        assert!(
            pous[0]
                .sections
                .iter()
                .all(|section| section.rungs.is_empty()),
            "no rungs come with a new project"
        );
    }

    /// The language of an empty POU cycles, so an empty MAIN can become IL from the
    /// keyboard; a POU with a program keeps its language.
    #[test]
    fn the_language_cycles_only_for_an_empty_pou() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        plc.apply(Action::SetPouLanguage {
            index: 0,
            language: PouLanguage::Il,
        });
        assert_eq!(plc.project.all_pous()[0].language, PouLanguage::Il);
        let mut with_program = PlcUi::default();
        with_program.project = Project::example();
        with_program.project.normalise();
        with_program.apply(Action::SetPouLanguage {
            index: 0,
            language: PouLanguage::Fbd,
        });
        assert_eq!(
            with_program.project.all_pous()[0].language,
            PouLanguage::Fbd,
            "the action itself sets the language; the list guards it for non-empty POUs"
        );
    }

    /// Adding a POU opens it, which is what the list keys rely on.
    #[test]
    fn adding_a_pou_opens_it() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        plc.apply(Action::AddPou(PouLanguage::Fbd));
        let pous = plc.project.all_pous();
        assert_eq!(pous.len(), 2);
        assert_eq!(plc.pou, 1, "the new POU is current");
        assert_eq!(pous[1].language, PouLanguage::Fbd);
    }
}

#[cfg(test)]
mod symbol_focus_tests {
    use super::*;

    /// A claimed field must keep the keyboard on the Symbols page: the editor used to
    /// clear `text_editing` on every page that was not Edit.
    #[test]
    fn the_symbol_field_keeps_the_keyboard_on_the_symbols_page() {
        let mut plc = PlcUi::default();
        plc.page = PlcPage::Symbols;
        plc.text_editing = true;
        // The rule the editor now applies.
        let page_lost_owner = !matches!(plc.page, PlcPage::Edit | PlcPage::Symbols);
        assert!(!page_lost_owner, "Symbols owns text fields");
        assert!(plc.text_editing, "the claim survives");
        plc.page = PlcPage::PouList;
        let page_lost_owner = !matches!(plc.page, PlcPage::Edit | PlcPage::Symbols);
        assert!(page_lost_owner, "leaving for another page releases it");
    }
}

#[cfg(test)]
mod save_pous_tests {
    use super::*;

    /// A new project plus an added POU must serialise both: MAIN and the new IL POU.
    /// If this ever fails, the empty save is a real defect rather than a stray New.
    #[test]
    fn a_new_project_with_an_added_pou_saves_both() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        plc.apply(Action::AddPou(PouLanguage::Il));
        let all = plc.project.all_pous();
        plc.project.set_pous(all);
        let text = project_to_json(&plc.project).expect("serialize");
        let parsed: Project = serde_json::from_str(&text).expect("parse back");
        let names: Vec<String> = parsed.all_pous().into_iter().map(|pou| pou.name).collect();
        assert_eq!(names.len(), 2, "both POUs are written: {names:?}");
        assert!(names.iter().any(|name| name == "MAIN"), "{names:?}");
    }
}

#[cfg(test)]
mod pou_guard_tests {
    use super::*;

    fn il_project() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let mut pous = plc.project.all_pous();
        pous[0].language = PouLanguage::Il;
        plc.project.set_pous(pous);
        plc.apply(Action::AddPou(PouLanguage::Il));
        plc.pou = 1;
        plc.il_cursor = 0;
        plc.il_buffer.clear();
        plc
    }

    /// On the Edit page the POU list must not touch the POU set: typing an IL body
    /// used to cycle languages, duplicate and delete POUs.
    #[test]
    fn pou_list_keys_are_inert_outside_the_list_page() {
        let mut plc = il_project();
        plc.page = PlcPage::Edit;
        let before = plc.project.all_pous().len();
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let input = egui::RawInput {
            events: vec![
                egui::Event::Text("LD".into()),
                egui::Event::Key {
                    key: egui::Key::U,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("pou-guard"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    pou_list_keys(&mut plc, ui, &mut actions);
                });
        });
        output.textures_delta.clear();
        assert!(actions.is_empty(), "no POU action from the Edit page");
        assert_eq!(
            plc.project.all_pous().len(),
            before,
            "no POU added or removed"
        );
    }
}

#[cfg(test)]
mod rail_tests {
    use super::*;

    fn rows(plc: &PlcUi) -> Vec<(&'static str, &'static str)> {
        rail_entries(plc)
            .into_iter()
            .filter_map(|entry| match entry {
                RailEntry::Row { text, key, .. } => Some((text, key)),
                RailEntry::Heading(_) => None,
            })
            .collect()
    }

    /// The pane lists the keys the page in front actually has: the editor's
    /// element letters must not appear on the program list, and the list's
    /// rename and duplicate must.
    #[test]
    fn the_shortcut_pane_follows_the_page() {
        let mut plc = PlcUi::default();
        plc.page = PlcPage::PouList;
        let list = rows(&plc);
        for wanted in [
            ("Open", "Enter"),
            ("New POU", "N"),
            ("Rename", "R"),
            ("Duplicate", "D"),
            ("Delete POU", "Bksp"),
            ("Start", "Ctrl+Enter"),
            ("Stop", "Ctrl+Shift+Enter"),
            ("Save project", "Ctrl+S"),
        ] {
            assert!(list.contains(&wanted), "{wanted:?} missing from {list:?}");
        }
        assert!(
            !list.iter().any(|(_, key)| *key == "G M" || *key == "C S R"),
            "element and coil keys belong to the ladder: {list:?}"
        );

        let mut plc = PlcUi::default();
        plc.page = PlcPage::Edit;
        let edit = rows(&plc);
        assert!(edit.contains(&("Compare / compute", "G M")), "{edit:?}");
        assert!(edit.contains(&("Insert row", "Ctrl+Enter")), "{edit:?}");
        assert!(edit.contains(&("Delete row", "Ctrl+Bksp")), "{edit:?}");
        assert!(edit.contains(&("Close editor", "Esc")), "{edit:?}");
        assert!(edit.contains(&("Ladder", "Ctrl+1")), "{edit:?}");
        assert!(edit.contains(&("Test run", "Ctrl+3")), "{edit:?}");
        assert!(
            !edit.iter().any(|(text, _)| *text == "Program list"),
            "the list is not a page of this window: {edit:?}"
        );
        assert!(
            !edit
                .iter()
                .any(|(text, _)| *text == "Watch page" || *text == "Trend page"),
            "watch and trend are in PAGES once: {edit:?}"
        );
        assert!(
            !edit.iter().any(|(text, _)| *text == "Duplicate"),
            "the list's keys do not work in the editor: {edit:?}"
        );
    }

    /// Run / stop used to be printed as `R`, which renames on the list and does
    /// nothing in the editor. The real chords are the ones the screen handles.
    #[test]
    fn the_pane_does_not_name_a_key_the_page_ignores() {
        for page in [PlcPage::PouList, PlcPage::Edit, PlcPage::Service] {
            let mut plc = PlcUi::default();
            plc.page = page;
            let keys = rows(&plc);
            assert!(
                !keys
                    .iter()
                    .any(|(text, key)| *text == "Run / stop" && *key == "R"),
                "{page:?} still prints Run / stop = R"
            );
        }
    }
}

#[cfg(test)]
mod pou_list_n_tests {
    use super::*;

    fn press(plc: &mut PlcUi, key: egui::Key) -> Vec<Action> {
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::unique("pou-keys"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    pou_list_keys(plc, ui, &mut actions);
                });
        });
        output.textures_delta.clear();
        actions
    }

    fn plc_with_two_pous() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.add_pou(PouLanguage::Ld);
        // `add_pou` opens the new body; the list fixture puts the list back.
        plc.page = PlcPage::PouList;
        plc.pou = 1;
        plc
    }

    /// `N` on the program list adds a POU. The key used to be taken by the page
    /// behind the list as "new project", which showed no question and did
    /// nothing, so the list's own footer was a lie.
    #[test]
    fn n_on_the_list_asks_for_a_new_pou() {
        let mut plc = PlcUi::default();
        plc.page = PlcPage::PouList;
        let actions = press(&mut plc, egui::Key::N);
        assert!(
            matches!(actions.first(), Some(Action::AddPou(PouLanguage::Ld))),
            "N must ask for a ladder POU, got {actions:?}"
        );
    }

    /// `R` asks for a name instead of renaming the POU to itself, which is what
    /// the list used to do: the action carried the current name, so R was dead.
    #[test]
    fn r_opens_the_rename_window() {
        let mut plc = plc_with_two_pous();
        let actions = press(&mut plc, egui::Key::R);
        assert!(actions.is_empty(), "no silent rename, got {actions:?}");
        let edit = plc.pou_edit.expect("the name window is up");
        assert_eq!(edit.index, 1);
        assert!(!edit.duplicate);
        assert_eq!(
            edit.name,
            plc.project.all_pous()[1].name,
            "the name is prefilled"
        );
    }

    /// `D` duplicates, `Backspace` deletes; the old table had `D` deleting and a
    /// duplicate key that the footer also called duplicate.
    #[test]
    fn d_duplicates_and_backspace_deletes() {
        let mut plc = plc_with_two_pous();
        let actions = press(&mut plc, egui::Key::D);
        assert!(actions.is_empty(), "D asks for a copy, got {actions:?}");
        let edit = plc.pou_edit.expect("the copy window is up");
        assert!(edit.duplicate);
        assert!(edit.name.ends_with("_2"), "got {}", edit.name);

        let mut plc = plc_with_two_pous();
        let actions = press(&mut plc, egui::Key::Backspace);
        assert!(
            matches!(actions.first(), Some(Action::DeletePou { index: 1 })),
            "Backspace deletes the selected POU, got {actions:?}"
        );

        // MAIN stays: the project needs one POU to open into.
        let mut plc = plc_with_two_pous();
        plc.pou = 0;
        let actions = press(&mut plc, egui::Key::Backspace);
        assert!(
            actions.is_empty(),
            "MAIN cannot be deleted, got {actions:?}"
        );
    }
}

#[cfg(test)]
mod symbol_add_tests {
    use super::*;

    /// Variables are created from the same name field as inputs and outputs:
    /// `V` commits an internal bit that is writable and bound to no HAL pin.
    #[test]
    fn v_adds_an_internal_variable_once() {
        let mut plc = PlcUi::default();
        // The default page loads the startup project from disk; the test needs a
        // known, empty project.
        plc.project = linc_plc_core::model::Project::new("t");
        plc.apply(Action::AddVar("seal".into()));
        assert_eq!(plc.project.vars.len(), 1);
        assert_eq!(plc.project.vars[0].name, "seal");
        assert_eq!(plc.project.vars[0].kind, linc_plc_core::model::VarKind::Bit);
        assert!(plc.project.vars[0].kind == linc_plc_core::model::VarKind::Bit);
        assert!(plc.project.io.is_empty(), "a variable is not an I/O point");
        plc.apply(Action::AddVar("seal".into()));
        assert_eq!(plc.project.vars.len(), 1, "the name is taken");
    }
}

#[cfg(test)]
mod symbol_rename_tests {
    use super::*;

    /// Renaming a symbol moves every cell and coil that names it, so the ladder
    /// never points at a name the symbol table no longer has.
    #[test]
    fn a_rename_updates_the_ladder() {
        let mut plc = PlcUi::default();
        let mut pou = linc_plc_core::model::Pou::new("main", PouLanguage::Ld);
        pou.sections = vec![linc_plc_core::model::Section {
            name: String::new(),
            rungs: vec![linc_plc_core::model::Rung {
                comment: String::new(),
                logic: linc_plc_core::model::Logic::Const { value: true },
                coil: linc_plc_core::model::Coil::new(
                    "pump",
                    linc_plc_core::model::CoilKind::Normal,
                ),
                cells: vec![Cell::Contact {
                    var: "pump".into(),
                    negated: false,
                }],
            }],
        }];
        plc.project.pous = vec![pou];
        plc.apply(Action::RenameSymbol {
            from: "pump".into(),
            to: "vacuum".into(),
        });
        let pou = &plc.project.all_pous()[0];
        let rung = &pou.sections[0].rungs[0];
        assert_eq!(rung.coil.var, "vacuum");
        assert_eq!(
            rung.cells[0],
            Cell::Contact {
                var: "vacuum".into(),
                negated: false
            }
        );
    }
}

#[cfg(test)]
mod canonical_save_tests {
    use super::*;

    /// `pous` empty with a section present must still serialise MAIN.
    #[test]
    fn an_empty_pou_list_with_one_section_serialises_main() {
        let mut project = Project::new("t");
        project.sections.push(linc_plc_core::model::Section {
            name: "main".to_string(),
            rungs: vec![Rung::new(
                Logic::contact("start", false),
                Coil::new("run", CoilKind::Normal),
            )],
        });
        project.pous.clear();
        let text = project_to_json(&project).expect("serialize");
        let parsed: Project = serde_json::from_str(&text).expect("parse");
        let names: Vec<String> = parsed.all_pous().into_iter().map(|pou| pou.name).collect();
        assert!(names.iter().any(|name| name == "MAIN"), "{names:?}");
        assert!(
            !parsed.all_pous()[0].sections.is_empty(),
            "the ladder section survives the canonicalising write"
        );
    }

    /// A new project plus an added IL POU serialises both, with the IL body.
    #[test]
    fn a_new_project_with_an_il_pou_serialises_both_and_its_body() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        plc.apply(Action::AddPou(PouLanguage::Il));
        let mut pous = plc.project.all_pous();
        pous[1].il = vec!["LD pump".to_string(), "ST run".to_string()];
        plc.project.set_pous(pous);
        let text = project_to_json(&plc.project).expect("serialize");
        let parsed: Project = serde_json::from_str(&text).expect("parse");
        let pous = parsed.all_pous();
        assert_eq!(pous.len(), 2, "both POUs written");
        assert_eq!(
            pous[1].il,
            vec!["LD pump".to_string(), "ST run".to_string()]
        );
    }
}

#[cfg(test)]
mod pou_index_tests {
    use super::*;

    /// A stale POU index must never leave the editor pointing past the POU list:
    /// that produced a blank header and an empty body after File > New.
    #[test]
    fn a_stale_pou_index_cannot_open_a_pou_that_does_not_exist() {
        let mut plc = PlcUi::default();
        plc.apply(Action::New);
        assert_eq!(plc.pou, 0, "a new project selects MAIN");
        plc.apply(Action::OpenPou(1));
        assert_eq!(plc.pou, 0, "an out-of-range index is ignored");
        assert!(plc.current_pou().is_some(), "the header always has a POU");

        plc.apply(Action::AddPou(PouLanguage::Il));
        assert_eq!(plc.pou, 1, "a real POU opens");
        plc.apply(Action::New);
        assert_eq!(plc.pou, 0, "New resets the index again");
        assert!(plc.current_pou().is_some());
    }
}

#[cfg(test)]
mod fbd_place_keyboard_tests {
    use super::*;

    fn fbd_plc() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let mut pous = plc.project.all_pous();
        pous[0].language = PouLanguage::Fbd;
        pous[0].fbd_grid = None;
        plc.project.set_pous(pous);
        plc.page = PlcPage::Edit;
        plc.pou = 0;
        plc.fbd_grid_pick = Some((0, 0));
        plc.operand_filter.clear();
        plc
    }

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("fbd-place"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    fbd_keys(plc, ui, &mut actions);
                    fbd_grid_menu(plc, ctx, &mut actions);
                    fbd_operand_menu(plc, ctx, &mut actions);
                });
        });
        output.textures_delta.clear();
        actions
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn one_input() -> PlcUi {
        let mut plc = fbd_plc();
        plc.project.io = vec![linc_plc_core::IoPoint {
            name: "pump".into(),
            direction: linc_plc_core::IoDirection::In,
            kind: linc_plc_core::IoKind::Bit,
            comment: String::new(),
        }];
        plc
    }

    #[test]
    fn enter_i_pump_enter_places_an_input() {
        let mut plc = one_input();
        let _ = frame(&mut plc, vec![egui::Event::Text("i".into())]);
        assert!(plc.fbd_operand_pick.is_some(), "I opens the operand list");
        let _ = frame(&mut plc, vec![egui::Event::Text("pump".into())]);
        let actions = frame(&mut plc, vec![key(egui::Key::Enter)]);
        assert!(plc.fbd_operand_pick.is_none(), "Enter closes the list");
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::SetFbdGrid(_))),
            "Enter writes the cell"
        );
    }

    #[test]
    fn a_places_an_and_block() {
        let mut plc = fbd_plc();
        let actions = frame(&mut plc, vec![egui::Event::Text("a".into())]);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::SetFbdGrid(_))),
            "A places the block"
        );
    }

    #[test]
    fn a_block_lands_on_the_picker_cell_not_the_cursor() {
        let mut plc = fbd_plc();
        // Cursor deliberately elsewhere: the picker coordinate must win.
        plc.fbd_cursor = Some((3, 1));
        plc.fbd_grid_pick = Some((0, 2));
        let actions = frame(&mut plc, vec![egui::Event::Text("a".into())]);
        let grid = actions
            .iter()
            .find_map(|action| match action {
                Action::SetFbdGrid(grid) => Some(grid.clone()),
                _ => None,
            })
            .expect("a grid is written");
        let block = grid.blocks.first().expect("the AND block is placed");
        assert_eq!(
            (block.row, block.col),
            (0, 2),
            "the picker cell, not the cursor"
        );
    }

    #[test]
    fn q_places_an_output() {
        let mut plc = one_input();
        let _ = frame(&mut plc, vec![egui::Event::Text("q".into())]);
        assert!(plc.fbd_operand_pick.is_some(), "Q opens the output list");
        let _ = frame(&mut plc, vec![egui::Event::Text("pump".into())]);
        let actions = frame(&mut plc, vec![key(egui::Key::Enter)]);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::SetFbdGrid(_))),
            "Enter writes the output cell"
        );
    }
}

#[cfg(test)]
mod fbd_filter_tests {
    use super::*;

    fn fbd_plc() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.project.normalise();
        let mut pous = plc.project.all_pous();
        pous[0].language = PouLanguage::Fbd;
        plc.project.set_pous(pous);
        plc.page = PlcPage::Edit;
        plc.pou = 0;
        plc.fbd_grid_pick = Some((0, 0));
        plc.project.io = vec![linc_plc_core::IoPoint {
            name: "pump".into(),
            direction: linc_plc_core::IoDirection::In,
            kind: linc_plc_core::IoKind::Bit,
            comment: String::new(),
        }];
        plc
    }

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::new("fbd-filter"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    fbd_keys(plc, ui, &mut actions);
                    fbd_grid_menu(plc, ctx, &mut actions);
                    fbd_operand_menu(plc, ctx, &mut actions);
                });
        });
        output.textures_delta.clear();
        actions
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Every new operand pick starts with an empty filter, even after a previous
    /// placement left text behind.
    #[test]
    fn the_operand_filter_starts_empty_for_each_pick() {
        let mut plc = fbd_plc();
        plc.operand_filter = "stale".into();
        let _ = frame(&mut plc, vec![egui::Event::Text("i".into())]);
        assert!(plc.fbd_operand_pick.is_some(), "I opens the list");
        assert!(
            plc.operand_filter.is_empty(),
            "the stale filter was cleared: {:?}",
            plc.operand_filter
        );
    }

    /// A commit clears the filter, so the next pick cannot inherit the name.
    #[test]
    fn the_operand_filter_clears_after_a_commit() {
        let mut plc = fbd_plc();
        let _ = frame(&mut plc, vec![egui::Event::Text("i".into())]);
        let _ = frame(&mut plc, vec![egui::Event::Text("pump".into())]);
        let actions = frame(&mut plc, vec![key(egui::Key::Enter)]);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::SetFbdGrid(_))),
            "the cell is written"
        );
        assert!(plc.fbd_operand_pick.is_none(), "the list closed");
        assert!(plc.operand_filter.is_empty(), "the filter is clean again");
    }
}

#[cfg(test)]
mod fbd_active_tests {
    use super::*;
    use linc_plc_core::model::{FbdBlock, FbdCell, FbdGrid};

    fn plc_with(values: &[(&str, bool)]) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.vars = values
            .iter()
            .map(|(name, value)| linc_plc_core::VarValue {
                name: (*name).to_string(),
                value: *value,
                kind: linc_plc_core::SymbolKind::Input,
                forced: false,
            })
            .collect();
        plc
    }

    /// A generic AND has no `Q` in the snapshot: it is evaluated from its input
    /// cells, which is what makes the block and its output light up in Sim.
    #[test]
    fn a_generic_and_reads_true_from_both_inputs() {
        let plc = plc_with(&[("pump", true), ("stop", true), ("run", true)]);
        let mut grid = FbdGrid {
            columns: 3,
            rows: 2,
            cells: vec![vec![FbdCell::Empty; 3]; 2],
            blocks: Vec::new(),
        };
        grid.cells[0][0] = FbdCell::In {
            operand: "pump".into(),
        };
        grid.cells[1][0] = FbdCell::In {
            operand: "stop".into(),
        };
        grid.cells[0][2] = FbdCell::Out {
            operand: "run".into(),
        };
        let block = FbdBlock {
            row: 0,
            col: 1,
            ins: 2,
            kind: "AND".into(),
            instance: String::new(),
            preset: String::new(),
        };
        assert_eq!(fbd_block_active(&plc, &grid, &block), Some(true));

        let mut off = plc_with(&[("pump", true), ("stop", false), ("run", false)]);
        off.vars[1].value = false;
        assert_eq!(fbd_block_active(&off, &grid, &block), Some(false));
    }

    /// A block whose input cells carry no readable operand stays neutral.
    #[test]
    fn an_unreadable_block_is_not_guessed() {
        let plc = plc_with(&[]);
        let grid = FbdGrid {
            columns: 3,
            rows: 2,
            cells: vec![vec![FbdCell::Empty; 3]; 2],
            blocks: Vec::new(),
        };
        let block = FbdBlock {
            row: 0,
            col: 1,
            ins: 2,
            kind: "AND".into(),
            instance: String::new(),
            preset: String::new(),
        };
        assert_eq!(fbd_block_active(&plc, &grid, &block), None);
    }
}

#[cfg(test)]
mod force_persist_tests {
    use super::*;

    /// A simulation force is persistent: zero TTL means "until released or reset",
    /// so navigating between POUs cannot drop it mid-session.
    #[test]
    fn a_simulation_force_does_not_expire_on_its_own() {
        assert_eq!(FORCE_TTL_MS, 0, "the UI asks for a persistent force");
    }
}
