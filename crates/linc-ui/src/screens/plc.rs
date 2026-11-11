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

use egui::{pos2, vec2, Align, Color32, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_client::Client;
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::model::FbdLine;
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
const FORCE_TTL_MS: u32 = 10_000;
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
    Stop,
    ClearForces,
    Force(String, bool),
    /// Pin an operand to the watch list, or unpin it.
    ToggleWatch(String),
    /// Release one operand's force.
    Unforce(String),
    /// Set the comment of a declared symbol.
    SetSymbolComment(String, String),
    /// Add an output symbol, which is what a new name in the table means.
    AddSymbol(String),
    /// Remove a declared symbol.
    RemoveSymbol(String),
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
    serde_json::to_string_pretty(project).map_err(|error| error.to_string())
}

pub fn project_from_json(text: &str) -> Result<Project, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
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

/// Which field of an FBD line the value picker fills: the output when `input`
/// is `None`, otherwise that input position.
#[derive(Debug, Clone, Copy)]
pub struct FbdPick {
    pub line: usize,
    pub input: Option<usize>,
}

/// One compare (`G`) or compute (`M`) cell being assembled: operator, then the
/// values, then (for compute) the destination word.
#[derive(Debug, Clone)]
pub struct ExprEdit {
    pub section: usize,
    pub rung: usize,
    pub position: usize,
    pub compute: bool,
    /// 0 operator, 1 left value, 2 right value, 3 destination.
    pub stage: u8,
    pub op: String,
    pub left: String,
    pub right: String,
    pub target: String,
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
    /// Effective service state from the profile (or `LINC_PLC`). False means no
    /// daemon was started at all, which is not the same as a daemon gone away.
    pub enabled: bool,
    pub banner: String,
    pub state: RunState,
    pub daemon_project: String,
    pub hash: String,
    pub scan_count: u64,
    pub scan_time_us: u32,
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
    /// The operand list is open for the cell a kind was just chosen for.
    pub operand_pick: Option<(usize, usize, usize, Cell)>,
    /// A compare or compute expression is being built step by step.
    pub expr_edit: Option<ExprEdit>,
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
    /// An FBD line's output or one of its inputs is being picked.
    pub fbd_pick: Option<FbdPick>,
    /// The block on this FBD line is being edited (instance and preset).
    pub fbd_box_edit: Option<usize>,
    /// The next FBD value chosen is negated (`!`).
    pub fbd_negate: bool,
    /// A POU is being renamed or copied.
    pub pou_edit: Option<PouEdit>,
    /// The trend window in milliseconds: 4 s, 20 s or 100 s.
    pub trend_window_ms: u32,
    /// Show operands by IEC address instead of symbol name.
    pub view_addresses: bool,
    /// The name typed into the symbols table's new row.
    pub symbol_new: String,
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
            project: Project::example(),
            path_text: default_project_path().display().to_string(),
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
            operand_pick: None,
            expr_edit: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cell_clipboard: None,
            cell_block: Vec::new(),
            cell_mark: None,
            xref: None,
            block_edit: None,
            fbd_pick: None,
            fbd_box_edit: None,
            fbd_negate: false,
            pou_edit: None,
            trend_window_ms: 20_000,
            view_addresses: false,
            symbol_new: String::new(),
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
            Action::AddRung => {
                let after = self.selection.as_ref().map(|node| node.rung);
                let section = self.selected_section();
                if let Some(node) = add_rung(&mut self.project, section, after) {
                    self.selection = Some(node);
                }
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
                self.project = Project::example();
                self.selection = Some(NodePath::root(0, 0));
                self.daemon_diags.clear();
                self.message = "NEW PROJECT".to_string();
                self.normalise_selection();
            }
            Action::Open => self.open(),
            Action::Save => self.save(),
            Action::Validate => self.validate(),
            Action::Download => self.download(),
            Action::Start => {
                self.send(Tag::Start, Request::Start);
                self.message = "START SENT".to_string();
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
            Action::AddSymbol(name) => {
                if !self.project.io.iter().any(|point| point.name == name)
                    && !self.project.vars.iter().any(|var| var.name == name)
                {
                    self.project
                        .io
                        .push(linc_plc_core::IoPoint::new(name, IoDirection::Out));
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
                Ok(project) => {
                    self.project = project;
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

impl PlcPage {
    fn label(self) -> &'static str {
        match self {
            Self::PouList => "POU LIST",
            Self::Edit => "EDIT",
            Self::Symbols => "SYMBOLS",
            Self::Sim => "SIMULATION",
            Self::Watch => "WATCH",
            Self::Trend => "TREND",
            Self::Messages => "MESSAGES",
            Self::Service => "SERVICE",
        }
    }

    const ALL: [PlcPage; 6] = [
        Self::PouList,
        Self::Edit,
        Self::Symbols,
        Self::Sim,
        Self::Watch,
        Self::Trend,
    ];
}

/// The page's status bar height, below the content.
const STATUS_BAR_HEIGHT: f32 = 22.0;

/// The PLC page is the launcher. ZX-PLC is a keyboard application, so the
/// workbench itself is a popped-out overlay that owns every key; the page keeps
/// the project, the service and the way in.
pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.plc.poll();
    common::focus_anchor(ui, "plc-page-focus");

    let mut actions: Vec<Action> = Vec::new();
    let mut open_editor = false;
    let plc = &mut app.plc;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if theme::accent_button(ui, "OPEN EDITOR", true).clicked() {
            open_editor = true;
        }
        ui.label(
            RichText::new(
                "The editor opens in its own window and takes the keyboard; Esc closes it.",
            )
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
    });
    ui.add_space(4.0);

    let width = ui.available_width();
    let left_width = (width * 0.36).clamp(260.0, 380.0);
    let tree_width = (width - left_width - 8.0).max(320.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(left_width, 220.0),
            egui::Layout::top_down(Align::Min),
            |ui| {
                card(ui, vec2(left_width, 220.0), |ui, _| {
                    status_card(plc, ui, &mut actions)
                });
            },
        );
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(
            vec2(tree_width, 220.0),
            egui::Layout::top_down(Align::Min),
            |ui| pou_list_page(plc, ui, 220.0, &mut actions),
        );
    });

    for action in actions {
        plc.apply(action);
    }
    if open_editor {
        app.overlay.plc_editor = true;
    }
}

/// The popped-out workbench: menu bar, pages, status bar, keyboard owner.
pub fn editor(app: &mut LincApp, ui: &mut Ui) {
    if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        && !ui.ctx().text_edit_focused()
    {
        app.overlay.plc_editor = false;
        return;
    }

    app.plc.poll();
    common::focus_anchor(ui, "plc-editor-focus");
    let mut actions: Vec<Action> = Vec::new();
    crate::screens::plc_grid::editor_keys(app, ui, &mut actions);
    il_keys(&mut app.plc, ui, &mut actions);
    crate::screens::plc_grid::pickers(&mut app.plc, ui.ctx(), &mut actions);
    pou_edit_dialog(&mut app.plc, ui.ctx(), &mut actions);
    let plc = &mut app.plc;

    menu_bar(plc, ui, &mut actions);
    ui.add_space(2.0);

    let height = (ui.available_height() - STATUS_BAR_HEIGHT - 4.0).max(160.0);
    egui::ScrollArea::vertical()
        .id_salt("plc-page")
        .auto_shrink([false, false])
        .show(ui, |ui| page_body(plc, ui, height, &mut actions));
    status_bar(plc, ui);

    for action in actions {
        plc.apply(action);
    }
    if plc.close_requested {
        plc.close_requested = false;
        app.overlay.plc_editor = false;
    }
}

/// ZX-PLC's menu bar: FILE, POU, EDIT, VIEW, RUN, HELP, with the current POU and
/// its language at the right, as the ZX screen shows `DEMO_LD (L10)`.
fn menu_bar(plc: &mut PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked() {
                actions.push(Action::New);
            }
            if ui.button("Open").clicked() {
                actions.push(Action::Open);
            }
            if ui.button("Save").clicked() {
                actions.push(Action::Save);
            }
            ui.separator();
            if ui.button("Validate").clicked() {
                actions.push(Action::Validate);
            }
            if ui.button("Download").clicked() {
                actions.push(Action::Download);
            }
            ui.separator();
            if ui.button("Service").clicked() {
                plc.page = PlcPage::Service;
            }
            if ui.button("Close editor").clicked() {
                plc.close_requested = true;
            }
        });
        ui.menu_button("POU", |ui| {
            if ui.button("POU list").clicked() {
                plc.page = PlcPage::PouList;
            }
            ui.separator();
            for language in [PouLanguage::Il, PouLanguage::Ld, PouLanguage::Fbd] {
                if ui.button(format!("New {} POU", language.label())).clicked() {
                    plc.add_pou(language);
                }
            }
            ui.separator();
            if ui.button("Rename / comment…").clicked() {
                plc.pou_edit = Some(PouEdit {
                    index: plc.pou,
                    duplicate: false,
                    name: plc
                        .current_pou()
                        .map(|pou| pou.name)
                        .unwrap_or_else(|| "MAIN".to_string()),
                });
            }
            if ui.button("Duplicate…").clicked() {
                let name = plc
                    .current_pou()
                    .map(|pou| format!("{}_2", pou.name))
                    .unwrap_or_else(|| "COPY".to_string());
                plc.pou_edit = Some(PouEdit {
                    index: plc.pou,
                    duplicate: true,
                    name,
                });
            }
            let is_main = plc
                .current_pou()
                .map(|pou| pou.name.eq_ignore_ascii_case("MAIN"))
                .unwrap_or(true);
            ui.add_enabled_ui(!is_main, |ui| {
                if ui.button("Delete").clicked() {
                    actions.push(Action::DeletePou { index: plc.pou });
                }
            });
            ui.separator();
            for language in [PouLanguage::Il, PouLanguage::Ld, PouLanguage::Fbd] {
                if ui
                    .button(format!("Language: {}", language.label()))
                    .clicked()
                {
                    plc.set_language(language);
                }
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui.button("Undo").clicked() {
                plc.undo();
            }
            if ui.button("Redo").clicked() {
                plc.redo();
            }
            ui.separator();
            if ui.button("Copy").clicked() {
                plic_copy(plc);
            }
            if ui.button("Cut").clicked() {
                plic_cut(plc, actions);
            }
            ui.separator();
            if ui.button("Add comment row").clicked() {
                if let Some((section, rung, _)) = plc.cell_cursor {
                    actions.push(Action::AddCommentRow { section, rung });
                }
            }
            if ui.button("Symbols").clicked() {
                plc.page = PlcPage::Symbols;
            }
            ui.separator();
            let selected = plc.selection.is_some();
            if ui
                .add_enabled(selected, egui::Button::new("Delete"))
                .clicked()
            {
                actions.push(Action::Delete);
            }
            if ui
                .add_enabled(selected, egui::Button::new("Negate"))
                .clicked()
            {
                if let Some(node) = plc.selection.clone() {
                    actions.push(Action::ToggleContact(node));
                }
            }
            if ui
                .add_enabled(selected, egui::Button::new("Series"))
                .clicked()
            {
                actions.push(Action::WrapSeries);
            }
            if ui
                .add_enabled(selected, egui::Button::new("Parallel"))
                .clicked()
            {
                actions.push(Action::WrapParallel);
            }
            ui.separator();
            if ui.button("Add rung").clicked() {
                actions.push(Action::AddRung);
            }
            if ui.button("Add section").clicked() {
                actions.push(Action::AddSection);
            }
        });
        ui.menu_button("View", |ui| {
            for page in PlcPage::ALL {
                let active = plc.page == page;
                if ui.selectable_label(active, page.label()).clicked() {
                    plc.page = page;
                }
            }
            ui.separator();
            if ui
                .selectable_label(!plc.view_addresses, "Symbols")
                .clicked()
            {
                plc.view_addresses = false;
            }
            if ui
                .selectable_label(plc.view_addresses, "Addresses")
                .clicked()
            {
                plc.view_addresses = true;
            }
            ui.separator();
            if ui.button("Messages").clicked() {
                plc.page = PlcPage::Messages;
            }
            if ui.button("Service").clicked() {
                plc.page = PlcPage::Service;
            }
        });
        ui.menu_button("Run", |ui| {
            let running = matches!(plc.state, RunState::Running);
            if ui
                .add_enabled(plc.online && !running, egui::Button::new("Run"))
                .clicked()
            {
                actions.push(Action::Start);
            }
            if ui
                .add_enabled(plc.online && running, egui::Button::new("Stop"))
                .clicked()
            {
                actions.push(Action::Stop);
            }
            ui.separator();
            if ui.button("Clear forces").clicked() {
                actions.push(Action::ClearForces);
            }
        });
        ui.menu_button("Help", |ui| {
            ui.label(format!("POU {} of {}", plc.pou + 1, plc.pou_count()));
            ui.label(DISABLED_HINT);
        });
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let (state_label, state_color) = run_state_label(plc.state);
            common::nav_badge(ui, state_label, state_color);
            ui.add_space(8.0);
            if let Some(pou) = plc.current_pou() {
                ui.label(
                    RichText::new(format!("{}  {}", pou.name, pou.language.label()))
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::FG),
                );
            }
        });
    });
}

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

/// ZX-PLC's POU list: name, language, size, comment. Select with a click, open
/// with a second click or Enter.
fn pou_list_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, _actions: &mut Vec<Action>) {
    let pous = plc.project.all_pous();
    egui::Grid::new("plc-pou-list")
        .num_columns(4)
        .spacing(vec2(18.0, 2.0))
        .show(ui, |ui| {
            for header in ["POU NAME", "LANGUAGE", "SIZE", "COMMENT"] {
                ui.label(
                    RichText::new(header)
                        .monospace()
                        .strong()
                        .size(theme::font_size(10.0))
                        .color(theme::C1),
                );
            }
            ui.end_row();
            for (index, pou) in pous.iter().enumerate() {
                let size = match pou.language {
                    PouLanguage::Ld => pou.sections.iter().map(|s| s.rungs.len()).sum::<usize>(),
                    PouLanguage::Il => pou.il.len(),
                    PouLanguage::Fbd => pou.fbd.len(),
                };
                let selected = index == plc.pou;
                let response = ui.selectable_label(
                    selected,
                    RichText::new(&pou.name)
                        .monospace()
                        .size(theme::font_size(10.0)),
                );
                if response.clicked() {
                    if selected {
                        plc.page = PlcPage::Edit;
                    } else {
                        plc.pou = index;
                    }
                }
                ui.label(
                    RichText::new(pou.language.label())
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::C3),
                );
                ui.label(
                    RichText::new(size.to_string())
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(&pou.comment)
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new(format!("{} POU · Free {} B", pous.len(), height as usize))
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
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
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if theme::action_button(ui, "POU LIST", true).clicked() {
                plc.page = PlcPage::PouList;
            }
        });
    });
    if language == PouLanguage::Fbd {
        fbd_picker(plc, ui.ctx(), actions);
        fbd_box_editor(plc, ui.ctx(), actions);
    }
    ui.add_space(2.0);
    let right_width = 296.0_f32.min(ui.available_width() * 0.34);
    let left_width = (ui.available_width() - right_width - 6.0).max(220.0);
    let body_height = (height - 32.0).max(120.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(left_width, body_height),
            egui::Layout::top_down(Align::Min),
            |ui| match language {
                PouLanguage::Ld => {
                    let sections = plc.current_pou_sections();
                    ladder_body(plc, &sections, ui, body_height, actions);
                }
                PouLanguage::Il => il_editor(plc, ui, body_height),
                PouLanguage::Fbd => fbd_editor(plc, ui, body_height, actions),
            },
        );
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(
            vec2(right_width, body_height),
            egui::Layout::top_down(Align::Min),
            |ui| {
                card(ui, vec2(right_width, body_height), |ui, _| {
                    egui::ScrollArea::vertical()
                        .id_salt("plc-properties")
                        .auto_shrink([false, false])
                        .show(ui, |ui| property_card(plc, ui, actions));
                });
            },
        );
    });
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
                    let colour = if line.trim_start().starts_with("//") {
                        theme::COMMENT
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

/// Keys the IL editor owns, as the manual's line editor describes them.
fn il_keys(plc: &mut PlcUi, ui: &Ui, actions: &mut Vec<Action>) {
    if plc.page != PlcPage::Edit || ui.ctx().memory(|memory| memory.focused().is_some()) {
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
                    } else if let Some(message) = plc.il_check(&plc.il_buffer) {
                        plc.il_message = Some(message);
                        break;
                    } else {
                        plc.il_store_buffer(&mut lines, true);
                        if plc.il_cursor + 1 < lines.len() {
                            plc.il_cursor += 1;
                            plc.il_buffer = lines[plc.il_cursor].clone();
                        }
                    }
                    plc.il_message = None;
                }
                egui::Key::Delete => {
                    if lines.get(plc.il_cursor).map(|line| line.trim().is_empty()).unwrap_or(true)
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
fn fbd_diagram(plc: &mut PlcUi, lines: &[FbdLine], ui: &mut Ui) {
    if lines.is_empty() {
        return;
    }
    let row = 18.0_f32;
    for (line_index, line) in lines.iter().enumerate() {
        let rows = line.inputs.len().max(1) as f32;
        let height = rows * row + 6.0;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, theme::SURFACE);
        let box_left = rect.left() + 132.0;
        let box_right = box_left + 96.0;
        let top = rect.top() + 3.0;
        let first_y = top + row / 2.0;
        // Inputs and their wires.
        for (index, input) in line.inputs.iter().enumerate() {
            let y = top + index as f32 * row + row / 2.0;
            painter.text(
                pos2(rect.left() + 6.0, y),
                egui::Align2::LEFT_CENTER,
                plc.operand_label(input.trim_start_matches(['!', '@'])),
                egui::FontId::monospace(theme::font_size(9.0)),
                theme::FG,
            );
            painter.line_segment(
                [pos2(rect.left() + 112.0, y), pos2(box_left, y)],
                Stroke::new(1.0, theme::C1),
            );
        }
        // The vertical bus and the block.
        if line.inputs.len() > 1 {
            let last_y = top + (rows - 1.0) * row + row / 2.0;
            painter.line_segment(
                [pos2(box_left - 8.0, first_y), pos2(box_left - 8.0, last_y)],
                Stroke::new(1.0, theme::C1),
            );
        }
        painter.rect_filled(
            egui::Rect::from_min_max(
                pos2(box_left, top),
                pos2(box_right, top + (rows * row).max(row)),
            ),
            2.0,
            theme::PANEL_RAISED,
        );
        let block_rect = egui::Rect::from_min_max(
            pos2(box_left, top),
            pos2(box_right, top + (rows * row).max(row)),
        );
        painter.rect_stroke(
            block_rect,
            2.0,
            Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        if let Some(pointer) = response.interact_pointer_pos() {
            if block_rect.contains(pointer) && response.clicked() {
                plc.fbd_box_edit = Some(line_index);
            }
        }
        painter.text(
            pos2((box_left + box_right) / 2.0, first_y),
            egui::Align2::CENTER_CENTER,
            &line.block,
            egui::FontId::monospace(theme::font_size(10.0)),
            theme::ACCENT,
        );
        // Output leaves on the top row.
        painter.line_segment(
            [pos2(box_right, first_y), pos2(rect.right() - 74.0, first_y)],
            Stroke::new(1.0, theme::SUCCESS),
        );
        painter.text(
            pos2(rect.right() - 6.0, first_y),
            egui::Align2::RIGHT_CENTER,
            plc.operand_label(line.output.trim_start_matches('!')),
            egui::FontId::monospace(theme::font_size(9.0)),
            theme::SUCCESS,
        );
        if !line.comment.is_empty() {
            painter.text(
                pos2(rect.left() + 6.0, rect.bottom() - 5.0),
                egui::Align2::LEFT_BOTTOM,
                &line.comment,
                egui::FontId::monospace(theme::font_size(8.0)),
                theme::MUTED,
            );
        }
    }
}

/// The value list for an FBD output or input: declared symbols (with their
/// comments), the word bank, block members, booleans, numbers and times.
/// Instance and preset of the block on one FBD line. The same choices the ladder
/// box editor offers, written back through `SetFbd`.
fn fbd_box_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(index) = plc.fbd_box_edit else {
        return;
    };
    let mut lines = plc.current_pou().map(|pou| pou.fbd).unwrap_or_default();
    let Some(line) = lines.get(index).cloned() else {
        plc.fbd_box_edit = None;
        return;
    };
    let letters: String = line
        .block
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let timer = matches!(letters.as_str(), "TON" | "TOF" | "TP");
    let counter = matches!(letters.as_str(), "CTU" | "CTD");
    let latch = matches!(letters.as_str(), "SR" | "RS");
    let _ = latch;
    let mut close = false;
    let mut apply: Option<String> = None;
    egui::Window::new(format!("{} · {}", line.block, line.output))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if timer || counter {
                ui.label(
                    RichText::new("instance")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.horizontal(|ui| {
                    for number in 0..4 {
                        let name = format!("{letters}{number}");
                        if theme::action_button(ui, &name, true).clicked() {
                            apply = Some(name);
                        }
                    }
                });
                ui.add_space(4.0);
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
                            if let Some(edited) = lines.get_mut(index) {
                                edited.preset = (*value).to_string();
                            }
                            actions.push(Action::SetFbd(lines.clone()));
                            close = true;
                        }
                    }
                });
            } else {
                ui.label("This block has no instance or preset.");
            }
            ui.add_space(4.0);
            if theme::action_button(ui, "Close", true).clicked() {
                close = true;
            }
        });
    if let Some(name) = apply {
        if let Some(edited) = lines.get_mut(index) {
            edited.block = name;
        }
        actions.push(Action::SetFbd(lines));
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.fbd_box_edit = None;
    }
}

/// The value list for an FBD output or input: declared symbols (with their
/// comments), the word bank, block members, booleans, numbers and times.
fn fbd_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(pick) = plc.fbd_pick else {
        return;
    };
    let mut close = false;
    let mut chosen: Option<String> = None;

    egui::Window::new(if pick.input.is_some() {
        "FBD INPUT"
    } else {
        "FBD OUTPUT"
    })
    .collapsible(false)
    .resizable(true)
    .default_width(360.0)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        if theme::action_button(
            ui,
            if plc.fbd_negate {
                "negated  !  (next pick)"
            } else {
                "negated  !"
            },
            true,
        )
        .clicked()
        {
            plc.fbd_negate = !plc.fbd_negate;
        }
        ui.separator();
        if pick.input.is_some() && pick.line > 0 {
            ui.label(
                RichText::new("pin of an earlier block on this page")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.horizontal_wrapped(|ui| {
                for line in 1..=pick.line {
                    if ui.button(format!("@{line}")).clicked() {
                        chosen = Some(format!("@{line}"));
                    }
                }
            });
            ui.separator();
        }
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for point in plc.project.io.clone() {
                    let label = format!(
                        "{}   {}   {}",
                        point.name,
                        match point.direction {
                            IoDirection::In => "input",
                            IoDirection::Out => "output",
                        },
                        point.comment
                    );
                    if ui.button(label).clicked() {
                        chosen = Some(point.name);
                    }
                }
                for var in plc.project.vars.clone() {
                    if ui
                        .button(format!("{}   var   {}", var.name, var.comment))
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
                    for value in ["0", "1", "5", "10", "100", "T#1s", "T#10s"] {
                        if ui.button(value).clicked() {
                            chosen = Some((*value).to_string());
                        }
                    }
                    for kind in ["TON", "TOF", "TP"] {
                        for number in 0..4 {
                            let name = format!("{kind}{number}.ET");
                            if ui.button(&name).clicked() {
                                chosen = Some(name);
                            }
                        }
                    }
                    for kind in ["CTU", "CTD"] {
                        for number in 0..4 {
                            let name = format!("{kind}{number}.CV");
                            if ui.button(&name).clicked() {
                                chosen = Some(name);
                            }
                        }
                    }
                });
            });
        ui.add_space(4.0);
        if theme::action_button(ui, "Cancel", true).clicked() {
            close = true;
        }
    });
    if let Some(value) = chosen {
        let value = if plc.fbd_negate && !value.starts_with('!') {
            format!("!{value}")
        } else {
            value
        };
        let mut lines = plc.current_pou().map(|pou| pou.fbd).unwrap_or_default();
        if let Some(line) = lines.get_mut(pick.line) {
            match pick.input {
                None => line.output = value,
                Some(position) => {
                    if position < line.inputs.len() {
                        line.inputs[position] = value;
                    } else {
                        line.inputs.push(value);
                    }
                }
            }
            actions.push(Action::SetFbd(lines));
        }
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.fbd_pick = None;
        plc.fbd_negate = false;
    }
}

/// Function block diagram: one assignment per line, blocks nesting by naming
/// an earlier line's output. Inputs are typed here; the value pickers the
/// ladder editor has are not wired to this page yet.
fn fbd_editor(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    const BLOCKS: [&str; 20] = [
        "AND", "OR", "XOR", "NOT", "TON0", "TOF0", "TP0", "CTU0", "CTD0", "SR0", "RS0", "RT0",
        "RF0", "GT", "GE", "EQ", "NE", "LE", "LT", "MOVE",
    ];
    let mut lines = plc.current_pou().map(|pou| pou.fbd).unwrap_or_default();
    let mut changed = false;
    fbd_diagram(plc, &lines, ui);
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "click a block in the diagram for its instance and preset · output := BLOCK(inputs)",
        )
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.add_space(4.0);
    let mut remove: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height((height - 96.0).max(120.0))
        .show(ui, |ui| {
            for (index, line) in lines.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if theme::action_button(
                        ui,
                        if line.output.is_empty() {
                            "output…"
                        } else {
                            &line.output
                        },
                        true,
                    )
                    .clicked()
                    {
                        plc.fbd_pick = Some(FbdPick {
                            line: index,
                            input: None,
                        });
                    }
                    ui.label(RichText::new(":=").monospace().color(theme::MUTED));
                    if theme::action_button(ui, &line.block, true).clicked() {
                        let position = BLOCKS
                            .iter()
                            .position(|block| block.eq_ignore_ascii_case(&line.block))
                            .map(|position| (position + 1) % BLOCKS.len())
                            .unwrap_or(0);
                        line.block = BLOCKS[position].to_string();
                        changed = true;
                    }
                    for (position, input) in line.inputs.clone().iter().enumerate() {
                        if theme::action_button(ui, input, true).clicked() {
                            plc.fbd_pick = Some(FbdPick {
                                line: index,
                                input: Some(position),
                            });
                        }
                    }
                    if theme::action_button(ui, "+ in", true).clicked() {
                        plc.fbd_pick = Some(FbdPick {
                            line: index,
                            input: Some(line.inputs.len()),
                        });
                    }
                    let timer = line.block.starts_with("TON")
                        || line.block.starts_with("TOF")
                        || line.block.starts_with("TP")
                        || line.block.starts_with("CTU")
                        || line.block.starts_with("CTD");
                    if timer {
                        let mut preset = line.preset.clone();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut preset)
                                    .desired_width(80.0)
                                    .hint_text("T#1s"),
                            )
                            .changed()
                        {
                            line.preset = preset;
                            changed = true;
                        }
                    }
                    if theme::action_button(ui, "remove", true).clicked() {
                        remove = Some(index);
                    }
                });
                let mut comment = line.comment.clone();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut comment)
                            .desired_width(420.0)
                            .hint_text("comment"),
                    )
                    .changed()
                {
                    line.comment = comment;
                    changed = true;
                }
                ui.add_space(4.0);
            }
        });
    if let Some(index) = remove {
        lines.remove(index);
        changed = true;
    }
    if theme::action_button(ui, "ADD LINE", true).clicked() {
        lines.push(FbdLine::new("output", "AND"));
        changed = true;
    }
    if changed {
        actions.push(Action::SetFbd(lines));
    }
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
        RichText::new(
            "Names as the operand picker and the status bar show them, with their comments.",
        )
        .size(theme::font_size(9.0))
        .color(theme::MUTED),
    );
    ui.add_space(4.0);
    let mut edits: Vec<(String, String)> = Vec::new();
    let mut remove: Option<String> = None;
    egui::ScrollArea::vertical()
        .max_height((height - 140.0).max(120.0))
        .show(ui, |ui| {
            egui::Grid::new("plc-symbols")
                .num_columns(4)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for heading in ["symbol", "area", "value", "comment"] {
                        ui.label(
                            RichText::new(heading)
                                .size(theme::font_size(9.0))
                                .color(theme::MUTED),
                        );
                    }
                    ui.end_row();
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
                        (left.1.as_str(), left.0.as_str())
                            .cmp(&(right.1.as_str(), right.0.as_str()))
                    });
                    for (name, area, comment) in rows {
                        ui.label(
                            RichText::new(&name)
                                .monospace()
                                .size(theme::font_size(10.0))
                                .color(theme::FG),
                        );
                        ui.label(
                            RichText::new(area)
                                .size(theme::font_size(9.0))
                                .color(theme::MUTED),
                        );
                        let value = plc
                            .vars
                            .iter()
                            .find(|var| var.name == name)
                            .map(|var| if var.value { "TRUE" } else { "FALSE" })
                            .unwrap_or("-");
                        ui.label(
                            RichText::new(value)
                                .monospace()
                                .size(theme::font_size(9.0))
                                .color(if value == "TRUE" {
                                    theme::SUCCESS
                                } else {
                                    theme::MUTED
                                }),
                        );
                        let mut buffer = comment;
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut buffer)
                                    .desired_width(180.0)
                                    .font(egui::TextStyle::Small),
                            )
                            .changed()
                        {
                            edits.push((name.clone(), buffer));
                        }
                        if theme::action_button(ui, "remove", true).clicked() {
                            remove = Some(name);
                        }
                        ui.end_row();
                    }
                    ui.label(
                        RichText::new("new")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    ui.label("");
                    ui.label("");
                    ui.horizontal(|ui| {
                        let mut name = plc.symbol_new.clone();
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .desired_width(140.0)
                                .hint_text("a new output name"),
                        );
                        if response.changed() {
                            plc.symbol_new = name.clone();
                        }
                        if theme::action_button(ui, "add", true).clicked()
                            && !name.trim().is_empty()
                        {
                            actions.push(Action::AddSymbol(name.trim().to_string()));
                            plc.symbol_new.clear();
                        }
                    });
                    ui.end_row();
                });
        });
    if let Some(name) = remove {
        actions.push(Action::RemoveSymbol(name));
    }
    for (name, comment) in edits {
        actions.push(Action::SetSymbolComment(name, comment));
    }
}

/// Simulation: the live program above, the input/output LED panel below.
fn sim_page(plc: &mut PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    let panel = (height * 0.42).clamp(120.0, 220.0);
    let sections = plc.current_pou_sections();
    ladder_body(plc, &sections, ui, height - panel - 8.0, actions);
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
fn sim_view(plc: &PlcUi, ui: &mut Ui, actions: &mut Vec<Action>) {
    common::card_title(ui, "SIMULATION");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        for point in &plc.project.io {
            let value = plc
                .vars
                .iter()
                .find(|var| var.name == point.name)
                .map(|var| var.value)
                .unwrap_or(false);
            let forced = plc.forced.iter().any(|name| name == &point.name);
            let input = point.direction == IoDirection::In;
            let colour = if value { theme::SUCCESS } else { theme::MUTED };
            let label = format!(
                "{}{}\n{}",
                if forced { "F " } else { "" },
                if value { "\u{25cf}" } else { "\u{25cb}" },
                point.name
            );
            let response = ui
                .add(
                    egui::Button::new(
                        RichText::new(label)
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(colour),
                    )
                    .fill(if value {
                        theme::tint(theme::SUCCESS, 28)
                    } else {
                        theme::PANEL_RAISED
                    })
                    .min_size(vec2(92.0, 44.0)),
                )
                .on_hover_text(if input {
                    "Click forces this input for the force TTL"
                } else {
                    "Output, driven by the scan"
                });
            if input && response.clicked() {
                actions.push(Action::Force(point.name.clone(), !value));
            }
        }
    });
}

/// ZX-PLC's status bar: the symbol and comment of the selection on the left,
/// free space and the run state on the right.
/// Rename or copy a POU: the name is typed once and applied on Enter.
fn pou_edit_dialog(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(edit) = plc.pou_edit.clone() else {
        return;
    };
    let mut close = false;
    let mut apply: Option<String> = None;
    egui::Window::new(if edit.duplicate {
        "DUPLICATE POU"
    } else {
        "RENAME POU"
    })
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        ui.label(
            RichText::new("name, up to eight characters")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        let mut name = edit.name.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(160.0)
                .char_limit(8),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            apply = Some(name.clone());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::action_button(ui, "Apply", true).clicked() {
                apply = Some(name.clone());
            }
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
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
    }
}

/// Copy the marked block (or the cell under the cursor) without a keyboard.
fn plic_copy(plc: &mut PlcUi) {
    let Some((section, rung, position)) = plc.cell_cursor else {
        return;
    };
    let sections = plc.current_pou_sections();
    let cells = match plc.cell_mark {
        Some((mark_section, mark_rung, mark_position)) if mark_section == section => {
            let (low_rung, high_rung) = if mark_rung <= rung {
                (mark_rung, rung)
            } else {
                (rung, mark_rung)
            };
            let (low, high) = if mark_position <= position {
                (mark_position, position)
            } else {
                (position, mark_position)
            };
            (low_rung..=high_rung)
                .filter_map(|index| sections.get(section).and_then(|s| s.rungs.get(index)))
                .map(|row| {
                    (low..=high)
                        .map(|slot| row.cells.get(slot).cloned().unwrap_or(Cell::Wire))
                        .collect::<Vec<Cell>>()
                })
                .collect::<Vec<_>>()
        }
        _ => Vec::new(),
    };
    if cells.is_empty() {
        plc.cell_clipboard = sections
            .get(section)
            .and_then(|section| section.rungs.get(rung))
            .and_then(|row| row.cells.get(position))
            .cloned();
        plc.cell_block.clear();
    } else {
        plc.cell_block = cells;
        plc.cell_clipboard = None;
    }
}

/// Copy the block and empty it, which is Cut in the manual's Edit menu.
fn plic_cut(plc: &mut PlcUi, actions: &mut Vec<Action>) {
    plic_copy(plc);
    let Some((section, rung, position)) = plc.cell_cursor else {
        return;
    };
    let rows = plc.cell_block.len();
    let columns = plc.cell_block.first().map(Vec::len).unwrap_or(1);
    for row in 0..rows.max(1) {
        for column in 0..columns {
            if position + column < POSITIONS {
                actions.push(Action::SetCell {
                    section,
                    rung: rung + row,
                    position: position + column,
                    cell: Cell::Wire,
                });
            }
        }
    }
    plc.cell_block.clear();
}

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
                    "{} us · OVERRUN {}",
                    plc.scan_time_us, plc.overruns
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

fn ladder_body(
    plc: &mut PlcUi,
    sections: &[linc_plc_core::model::Section],
    ui: &mut Ui,
    height: f32,
    actions: &mut Vec<Action>,
) {
    if sections.is_empty() {
        ui.label(
            RichText::new("No sections. Add rung / Add section starts one.")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        return;
    }
    egui::ScrollArea::both()
        .id_salt("plc-ladder")
        .max_height((height - 6.0).max(80.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
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
                    if rung.cells.is_empty() && linc_plc_core::grid::migrate(&rung.logic).is_none()
                    {
                        draw_rung(plc, ui, section_index, rung_index, rung, actions);
                    } else {
                        crate::screens::plc_grid::grid_rung(
                            plc,
                            ui,
                            section_index,
                            rung_index,
                            rung,
                            actions,
                        );
                    }
                }
            }
        });
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
    let any_force = !plc.forced.is_empty();
    common::card_title(ui, "PLC SERVICE");
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        theme::status_dot(ui, dot_color, dot_text, &status_text);
        ui.add_space(10.0);
        common::nav_badge(ui, state_label, state_color);
        ui.add_space(10.0);
        ui.label(
            RichText::new(status_text)
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} · SCAN {} · {} us · OVERRUN {} · {}",
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
        });
    });
    ui.horizontal(|ui| {
        field(ui, "SOCKET", "linc-plc-socket", &mut plc.socket_text);
        ui.add_space(6.0);
        field(ui, "FILE", "linc-plc-file", &mut plc.path_text);
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if theme::action_button(ui, "CONNECT", true).clicked() {
            actions.push(Action::Connect);
        }
        if theme::action_button(ui, "NEW", true).clicked() {
            actions.push(Action::New);
        }
        if theme::action_button(ui, "OPEN", true).clicked() {
            actions.push(Action::Open);
        }
        if theme::action_button(ui, "SAVE", true).clicked() {
            actions.push(Action::Save);
        }
        if theme::action_button(ui, "VALIDATE", true).clicked() {
            actions.push(Action::Validate);
        }
        // Load sets the daemon phase to Loaded and drives every output false:
        // it does not resume a running ladder, so the label says so.
        let download = if running {
            "DOWNLOAD (STOPS)"
        } else {
            "DOWNLOAD"
        };
        if theme::accent_button(ui, download, online).clicked() {
            actions.push(Action::Download);
        }
        if theme::action_button(ui, "START", online && !running).clicked() {
            actions.push(Action::Start);
        }
        if theme::action_button(ui, "STOP", online && running).clicked() {
            actions.push(Action::Stop);
        }
        if theme::action_button(ui, "CLEAR FORCES", online && any_force).clicked() {
            actions.push(Action::ClearForces);
        }
    });
}

fn field(ui: &mut Ui, label: &str, id: &str, text: &mut String) {
    ui.label(
        RichText::new(label)
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    let width = (ui.available_width() * 0.46).max(120.0);
    let response = ui.add_sized(
        [width, theme::MIN_TARGET],
        egui::TextEdit::singleline(text)
            .id(egui::Id::unique(id))
            .font(egui::FontId::monospace(theme::font_size(10.0))),
    );
    theme::focus_ring(ui, &response);
}

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
            if matches!(plc.state, RunState::Running) {
                ui.label(
                    RichText::new("RUNNING: DOWNLOAD STOPS THE LADDER. PRESS START AFTERWARDS.")
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
                    RichText::new("No diagnostics. VALIDATE runs the library locally.")
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
                .any(|request| request == "force seal=true ttl 10000"),
            "the force carries the page's TTL: {seen:?}"
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
