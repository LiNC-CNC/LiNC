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
use linc_plc_core::protocol::{Request, Response, RunState, StateSnapshot, PROTOCOL_VERSION};
use linc_plc_core::{
    validate, Coil, CoilKind, Diagnostic, IoDirection, Logic, Project, Rung, Section, Severity,
    VarValue,
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
        CoilKind::Set => "(S)",
        CoilKind::Reset => "(R)",
    }
}

fn coil_index(kind: &CoilKind) -> usize {
    match kind {
        CoilKind::Normal => 0,
        CoilKind::Set => 1,
        CoilKind::Reset => 2,
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
            next_poll: Instant::now(),
        }
    }
}

impl PlcUi {
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
            Action::ClearForces => {
                self.send(Tag::ClearForces, Request::ClearForces);
                self.message = "CLEAR FORCES SENT".to_string();
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

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.plc.poll();
    page_keys(app, ui);
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "plc-page-focus");

    let mut actions: Vec<Action> = Vec::new();
    let width = ui.available_width();
    let page_top = ui.cursor().min.y;
    let page_bottom =
        ui.ctx().input(|input| input.viewport_rect().bottom()) - theme::BOTTOM_BAR_HEIGHT;
    let page_height = (page_bottom - page_top).max(220.0);

    let status_height = 158.0_f32.min(page_height * 0.42);
    card(ui, vec2(width, status_height), |ui, _| {
        status_card(&mut app.plc, ui, &mut actions)
    });
    ui.add_space(6.0);

    let rest = (page_height - status_height - 6.0).max(160.0);
    let left_width = (width - right_minimum(width)).min(width * 0.62).max(320.0);
    let right_width = (width - left_width - 8.0).max(right_minimum(width));
    let diagnostics_height = (rest * 0.32).clamp(96.0, 170.0);
    let ladder_height = (rest - diagnostics_height - 6.0).max(150.0);
    let node_height = (rest * 0.55).clamp(180.0, 300.0);
    let variables_height = (rest - node_height - 6.0).max(120.0);

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_width, rest),
            egui::Layout::top_down(Align::Min),
            |ui| {
                card(ui, vec2(left_width, ladder_height), |ui, height| {
                    ladder_card(&app.plc, ui, height, &mut actions)
                });
                ui.add_space(6.0);
                card(ui, vec2(left_width, diagnostics_height), |ui, height| {
                    diagnostics_card(&app.plc, ui, height)
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, rest),
            egui::Layout::top_down(Align::Min),
            |ui| {
                card(ui, vec2(right_width, node_height), |ui, _| {
                    property_card(&mut app.plc, ui, &mut actions)
                });
                ui.add_space(6.0);
                card(ui, vec2(right_width, variables_height), |ui, height| {
                    variables_card(&app.plc, ui, height, &mut actions)
                });
            },
        );
    });

    for action in actions {
        app.plc.apply(action);
    }
}

fn right_minimum(width: f32) -> f32 {
    if width >= 900.0 {
        320.0
    } else {
        260.0
    }
}

/// Page-local keys, before any widget draws: the ladder box that holds focus
/// must not eat them first. Ctrl+D stays the navigation key for Diagnostics, so
/// DOWNLOAD is on Ctrl+Enter instead.
fn page_keys(app: &mut LincApp, ui: &Ui) {
    // A text field keeps Backspace for its own editing.
    if ui.ctx().text_edit_focused() {
        return;
    }
    let (save, delete, download) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::CTRL, egui::Key::S),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter),
        )
    });
    if delete {
        app.plc.apply(Action::Delete);
    }
    if save {
        app.plc.apply(Action::Save);
    }
    if download {
        app.plc.apply(Action::Download);
    }
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
        theme::status_dot(ui, dot_color, dot_text);
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

fn ladder_card(plc: &PlcUi, ui: &mut Ui, height: f32, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        common::card_title(ui, "LADDER");
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if theme::action_button(ui, "ADD RUNG", true).clicked() {
                actions.push(Action::AddRung);
            }
            if theme::action_button(ui, "ADD SECTION", true).clicked() {
                actions.push(Action::AddSection);
            }
            ui.label(
                RichText::new(format!(
                    "{} SECTION · {} RUNG",
                    plc.project.sections.len(),
                    plc.project
                        .sections
                        .iter()
                        .map(|section| section.rungs.len())
                        .sum::<usize>()
                ))
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
        });
    });
    let list_height = (height - 56.0).max(60.0);
    egui::ScrollArea::vertical()
        .id_salt("plc-ladder")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if plc.project.sections.is_empty() {
                ui.label(
                    RichText::new("No sections. ADD SECTION starts one.")
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                return;
            }
            for (section_index, section) in plc.project.sections.iter().enumerate() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("{} · {}", section.name, section.rungs.len()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::C3),
                );
                if section.rungs.is_empty() {
                    ui.label(
                        RichText::new("empty section")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                }
                for (rung_index, rung) in section.rungs.iter().enumerate() {
                    draw_rung(plc, ui, section_index, rung_index, rung, actions);
                }
            }
        });
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
            &format!("{}\n{}", rung.coil.var, coil_label(rung.coil.kind.clone())),
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
                        RichText::new(coil_label(candidate.clone()))
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
