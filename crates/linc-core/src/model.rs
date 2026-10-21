//! Machine state shared by the UI, backends and panel service.
//!
//! The model is deliberately data-only. Nothing here touches HAL, NML, the
//! filesystem or an egui context.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const MAX_EVENT_LOG: usize = 1000;

/// Ordered axis map. Axis order is the LinuxCNC `COORDINATES` order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisMap<T> {
    entries: Vec<(String, T)>,
}

impl<T> AxisMap<T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, T)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    pub fn insert(&mut self, axis: impl Into<String>, value: T) {
        let axis = axis.into();
        if let Some((_, slot)) = self.entries.iter_mut().find(|(name, _)| *name == axis) {
            *slot = value;
        } else {
            self.entries.push((axis, value));
        }
    }

    pub fn get(&self, axis: &str) -> Option<&T> {
        self.entries
            .iter()
            .find(|(name, _)| name == axis)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, axis: &str) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|(name, _)| name == axis)
            .map(|(_, value)| value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<T: Clone> AxisMap<T> {
    pub fn values(&self) -> Vec<T> {
        self.entries
            .iter()
            .map(|(_, value)| value.clone())
            .collect()
    }

    pub fn mapped<U>(&self, mut f: impl FnMut(&str, &T) -> U) -> AxisMap<U> {
        AxisMap::from_entries(
            self.entries
                .iter()
                .map(|(name, value)| (name.clone(), f(name, value))),
        )
    }
}

impl<T: Default + Clone> AxisMap<T> {
    pub fn zeros_like(&self) -> Self {
        self.mapped(|_, _| T::default())
    }
}

impl AxisMap<f64> {
    pub fn all_zero(&self) -> bool {
        self.iter().all(|(_, value)| value.abs() < 1e-12)
    }

    pub fn add(&self, other: &Self) -> Self {
        self.mapped(|name, value| value + other.get(name).copied().unwrap_or(0.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineVariant {
    Mill,
    Lathe,
}

impl MachineVariant {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mill => "MILL",
            Self::Lathe => "LATHE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PanelMode {
    Edit,
    Auto,
    Mdi,
    Jog,
    Inc,
    Mpg,
    Home,
    ProgHome,
}

impl PanelMode {
    pub const ALL: [PanelMode; 8] = [
        PanelMode::Edit,
        PanelMode::Auto,
        PanelMode::Mdi,
        PanelMode::Jog,
        PanelMode::Inc,
        PanelMode::Mpg,
        PanelMode::Home,
        PanelMode::ProgHome,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Edit => "EDIT",
            Self::Auto => "AUTO",
            Self::Mdi => "MDI",
            Self::Jog => "JOG",
            Self::Inc => "INC",
            Self::Mpg => "MPG",
            Self::Home => "HOME",
            Self::ProgHome => "PROG.HOME",
        }
    }

    pub fn is_manual(self) -> bool {
        matches!(self, Self::Jog | Self::Inc | Self::Mpg)
    }
}

/// Result of decoding the physical one-hot mode inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PanelModeValidity {
    Valid(PanelMode),
    /// Zero or multiple inputs active. Fails closed to `Unknown`.
    Invalid(Vec<PanelMode>),
    /// The panel link is down or the snapshot is stale.
    Unknown,
}

impl Default for PanelModeValidity {
    fn default() -> Self {
        Self::Unknown
    }
}

impl PanelModeValidity {
    pub fn mode(&self) -> Option<PanelMode> {
        match self {
            Self::Valid(mode) => Some(*mode),
            Self::Invalid(_) | Self::Unknown => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid(_))
    }

    pub fn label(&self) -> &'static str {
        self.mode().map(PanelMode::label).unwrap_or("UNKNOWN")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskMode {
    Manual,
    Auto,
    Mdi,
    Unknown,
}

impl TaskMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL",
            Self::Auto => "AUTO",
            Self::Mdi => "MDI",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterpState {
    Idle,
    Reading,
    Paused,
    Waiting,
    Unknown,
}

impl InterpState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Reading => "READING",
            Self::Paused => "PAUSED",
            Self::Waiting => "WAITING",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachinePower {
    On,
    Off,
    Estop,
    EstopReset,
    Unknown,
}

impl MachinePower {
    pub fn label(self) -> &'static str {
        match self {
            Self::On => "ON",
            Self::Off => "OFF",
            Self::Estop => "E-STOP",
            Self::EstopReset => "E-STOP RESET",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskState {
    pub power: MachinePower,
    pub mode: TaskMode,
    pub interp: InterpState,
    pub paused: bool,
    pub running: bool,
    pub program_started: bool,
    pub motion_active: bool,
}

impl Default for TaskState {
    fn default() -> Self {
        Self {
            power: MachinePower::Unknown,
            mode: TaskMode::Unknown,
            interp: InterpState::Unknown,
            paused: false,
            running: false,
            program_started: false,
            motion_active: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineState {
    pub on: bool,
    pub estop: bool,
    pub homed: AxisMap<bool>,
    pub homing: AxisMap<bool>,
    pub homing_sequence: Vec<u32>,
    pub limits: Vec<String>,
    pub fault: bool,
    pub force_homing: bool,
    pub following_error: AxisMap<f64>,
    pub servo_load: AxisMap<f64>,
    pub commanded: AxisMap<f64>,
    pub actual: AxisMap<f64>,
    pub coolant_flood: bool,
    pub coolant_mist: bool,
    pub lube: bool,
    pub lube_level_ok: bool,
    pub door_closed: bool,
    pub tool_change_active: bool,
    pub echo_serial: u64,
    pub command_age_ms: u64,
    pub status_age_ms: u64,
    pub nml_command_connected: bool,
    pub nml_status_connected: bool,
    pub hal_thread_ok: bool,
}

impl Default for MachineState {
    fn default() -> Self {
        Self {
            on: false,
            estop: true,
            homed: AxisMap::new(),
            homing: AxisMap::new(),
            homing_sequence: Vec::new(),
            limits: Vec::new(),
            fault: false,
            force_homing: false,
            following_error: AxisMap::new(),
            servo_load: AxisMap::new(),
            commanded: AxisMap::new(),
            actual: AxisMap::new(),
            coolant_flood: false,
            coolant_mist: false,
            lube: false,
            lube_level_ok: true,
            door_closed: true,
            tool_change_active: false,
            echo_serial: 0,
            command_age_ms: 0,
            status_age_ms: 0,
            nml_command_connected: true,
            nml_status_connected: true,
            hal_thread_ok: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PositionSet {
    pub work: f64,
    pub machine: f64,
    pub dtg: f64,
    pub velocity: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Positions {
    pub axes: AxisMap<PositionSet>,
    pub units: String,
    pub angular_units: String,
    pub lathe_diameter_mode: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkOffset {
    pub index: u8,
    pub values: AxisMap<f64>,
    pub rotation: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkOffsetSet {
    pub offsets: Vec<WorkOffset>,
    pub g92: AxisMap<f64>,
    pub active_index: Option<u8>,
    pub revision: u64,
}

impl Default for WorkOffsetSet {
    fn default() -> Self {
        Self {
            offsets: Vec::new(),
            g92: AxisMap::new(),
            active_index: None,
            revision: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ToolColumn {
    T,
    P,
    Z,
    D,
    R,
    X,
    I,
    J,
    Q,
    F,
    B,
    A,
    C,
    U,
    V,
    W,
    H,
    E,
    K,
}

impl ToolColumn {
    pub fn parse(c: char) -> Option<Self> {
        Some(match c.to_ascii_uppercase() {
            'T' => Self::T,
            'P' => Self::P,
            'Z' => Self::Z,
            'D' => Self::D,
            'R' => Self::R,
            'X' => Self::X,
            'I' => Self::I,
            'J' => Self::J,
            'Q' => Self::Q,
            'F' => Self::F,
            'B' => Self::B,
            'A' => Self::A,
            'C' => Self::C,
            'U' => Self::U,
            'V' => Self::V,
            'W' => Self::W,
            'H' => Self::H,
            'E' => Self::E,
            'K' => Self::K,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::T => "T",
            Self::P => "P",
            Self::Z => "Z",
            Self::D => "D",
            Self::R => "R",
            Self::X => "X",
            Self::I => "I",
            Self::J => "J",
            Self::Q => "Q",
            Self::F => "F",
            Self::B => "B",
            Self::A => "A",
            Self::C => "C",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::H => "H",
            Self::E => "E",
            Self::K => "K",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolChangerKind {
    Manual,
    Atc,
    Turret,
}

impl ToolChangerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL CHANGER",
            Self::Atc => "ATC",
            Self::Turret => "POWER TURRET",
        }
    }

    pub fn uses_pockets(self) -> bool {
        !matches!(self, Self::Manual)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "manual" => Some(Self::Manual),
            "atc" => Some(Self::Atc),
            "turret" => Some(Self::Turret),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolGeometry {
    pub values: AxisMap<f64>,
    pub diameter: Option<f64>,
    pub length: Option<f64>,
    pub nose_radius: Option<f64>,
    pub orientation: Option<u8>,
    pub front_angle: Option<f64>,
    pub back_angle: Option<f64>,
    pub wear: AxisMap<f64>,
}

impl Default for ToolGeometry {
    fn default() -> Self {
        Self {
            values: AxisMap::new(),
            diameter: None,
            length: None,
            nose_radius: None,
            orientation: None,
            front_angle: None,
            back_angle: None,
            wear: AxisMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRow {
    pub tool: u16,
    pub pocket: Option<u16>,
    pub description: String,
    pub geometry: ToolGeometry,
    pub kind: String,
}

impl ToolRow {
    pub fn new(tool: u16) -> Self {
        Self {
            tool,
            pocket: None,
            description: String::new(),
            geometry: ToolGeometry::default(),
            kind: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolTableState {
    pub path: PathBuf,
    pub columns: Vec<ToolColumn>,
    pub rows: Vec<ToolRow>,
    pub revision: u64,
    pub changer: ToolChangerKind,
    pub error: Option<String>,
}

impl Default for ToolTableState {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            revision: 0,
            changer: ToolChangerKind::Manual,
            error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpindleState {
    pub index: usize,
    pub name: String,
    pub commanded_speed: f64,
    pub actual_speed: f64,
    pub load: Option<f64>,
    pub override_factor: f64,
    pub direction: i8,
    pub on: bool,
    pub live_tool: bool,
    pub live_tool_axial: bool,
    pub live_tool_max_rpm: f64,
}

impl SpindleState {
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("SPINDLE {}", self.index)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedState {
    pub feed: f64,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub units: String,
    pub feed_mode: String,
    pub distance_mode: String,
    pub feed_override_enabled: bool,
    pub spindle_override_enabled: bool,
    pub adaptive_feed: bool,
}

impl Default for FeedState {
    fn default() -> Self {
        Self {
            feed: 0.0,
            feed_override: 1.0,
            rapid_override: 1.0,
            spindle_override: 1.0,
            units: "mm".into(),
            feed_mode: "G94".into(),
            distance_mode: "G90".into(),
            feed_override_enabled: true,
            spindle_override_enabled: true,
            adaptive_feed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgramState {
    pub path: Option<PathBuf>,
    pub display_name: String,
    pub revision: u64,
    /// Loaded program text. Arc so 20 Hz snapshots are cheap.
    pub content: Option<Arc<String>>,
    pub line_count: usize,
    pub current_line: usize,
    pub next_line: Option<String>,
    pub current_block: String,
    pub modal: Vec<(String, String)>,
    pub running: bool,
    pub paused: bool,
    pub dirty: bool,
    pub syntax_error: Option<SyntaxIssue>,
    pub progress: f32,
    pub loaded_at_ms: u64,
}

impl Default for ProgramState {
    fn default() -> Self {
        Self {
            path: None,
            display_name: String::new(),
            revision: 0,
            content: None,
            line_count: 0,
            current_line: 0,
            next_line: None,
            current_block: String::new(),
            modal: Vec::new(),
            running: false,
            paused: false,
            dirty: false,
            syntax_error: None,
            progress: 0.0,
            loaded_at_ms: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxIssue {
    pub line: usize,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolChangeStatus {
    Idle,
    Issuing,
    ManualPrompt,
    Complete,
    Fault,
}

impl ToolChangeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Issuing => "ISSUING",
            Self::ManualPrompt => "MANUAL CHANGE",
            Self::Complete => "COMPLETE",
            Self::Fault => "FAULT",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolChangeToken(pub u64);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolChangeState {
    pub status: ToolChangeStatus,
    pub current_tool: u16,
    pub prepared_tool: u16,
    pub target_tool: Option<u16>,
    pub message: String,
    pub token: Option<ToolChangeToken>,
    pub confirm_available: bool,
    /// The operator confirmed; the request is still up, so the machine is
    /// catching up. The prompt stays until the HAL request drops.
    pub confirmed: bool,
}

impl Default for ToolChangeState {
    fn default() -> Self {
        Self {
            status: ToolChangeStatus::Idle,
            current_tool: 0,
            prepared_tool: 0,
            target_tool: None,
            message: String::new(),
            token: None,
            confirm_available: false,
            confirmed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub routine: String,
    pub measured: AxisMap<f64>,
    pub error: Option<f64>,
    pub wcs: String,
    pub mode: String,
    pub timestamp_ms: u64,
    pub values: Vec<f64>,
    /// Which `#1001`–`#1009` layout the routine wrote, so the UI labels the
    /// slots by meaning instead of assuming X/Y/Z positions.
    #[serde(default)]
    pub family: crate::probe::ResultFamily,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeState {
    pub enabled: bool,
    pub tool_number: u16,
    pub selected_wcs: String,
    pub calibration_offset: Option<f64>,
    pub calibration_diameter: Option<f64>,
    pub last_result: Option<ProbeResult>,
    pub history: VecDeque<ProbeResult>,
    pub running: bool,
    pub error: Option<String>,
    pub last_routine: Option<String>,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self {
            enabled: true,
            tool_number: 99,
            selected_wcs: "G54".into(),
            calibration_offset: None,
            calibration_diameter: None,
            last_result: None,
            history: VecDeque::new(),
            running: false,
            error: None,
            last_routine: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AtcState {
    pub pockets: u16,
    pub current_pocket: Option<u16>,
    pub prepared_pocket: Option<u16>,
    pub clamped: Option<bool>,
    pub indexing: bool,
    pub station: Option<u16>,
    pub live_tool_rpm: Option<f64>,
    pub live_tool_direction: i8,
    pub live_tool_axial: Option<bool>,
    pub fault: Option<String>,
}

impl Default for AtcState {
    fn default() -> Self {
        Self {
            pockets: 0,
            current_pocket: None,
            prepared_pocket: None,
            clamped: None,
            indexing: false,
            station: None,
            live_tool_rpm: None,
            live_tool_direction: 0,
            live_tool_axial: None,
            fault: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendantState {
    pub enabled: bool,
    pub connected: bool,
    pub jog_only: bool,
    pub jog_enable: bool,
    pub active_axis: Option<String>,
    pub handwheel_scale: Option<f64>,
    pub step_mode: Option<String>,
    pub ignored_functions: Vec<String>,
}

impl Default for PendantState {
    fn default() -> Self {
        Self {
            enabled: true,
            connected: false,
            jog_only: true,
            jog_enable: false,
            active_axis: None,
            handwheel_scale: None,
            step_mode: None,
            ignored_functions: vec![
                "mode".into(),
                "cycle".into(),
                "spindle".into(),
                "coolant".into(),
                "overrides".into(),
                "program".into(),
                "macro".into(),
                "probe".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelLinkStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub crc_errors: u64,
    pub timeouts: u64,
    pub reconnects: u64,
    pub last_rx_age_ms: Option<u64>,
    pub cycle_time_ms: f64,
    pub port: String,
    pub baud: u32,
    pub available: bool,
}

impl Default for PanelLinkStats {
    fn default() -> Self {
        Self {
            rx_bytes: 0,
            tx_bytes: 0,
            crc_errors: 0,
            timeouts: 0,
            reconnects: 0,
            last_rx_age_ms: None,
            cycle_time_ms: 0.0,
            port: String::new(),
            baud: 115200,
            available: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelCapabilities {
    pub buttons: usize,
    pub leds: usize,
    pub displays: usize,
    pub has_status_display: bool,
    pub mode_encoding: String,
    pub override_encoding: String,
    pub pendant_supported: bool,
    pub transport: String,
}

impl Default for PanelCapabilities {
    fn default() -> Self {
        Self {
            buttons: 46,
            leds: 56,
            displays: 3,
            has_status_display: true,
            mode_encoding: "one_hot_steady".into(),
            override_encoding: "analog_steady".into(),
            pendant_supported: true,
            transport: "rs485".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JogState {
    pub mode: String,
    pub active_axis: Option<String>,
    pub speed: f64,
    pub rapid: bool,
    pub increment: Option<f64>,
    pub remaining_step: Option<f64>,
    pub handwheel_scale: Option<f64>,
}

impl Default for JogState {
    fn default() -> Self {
        Self {
            mode: "JOG".into(),
            active_axis: None,
            speed: 0.0,
            rapid: false,
            increment: None,
            remaining_step: None,
            handwheel_scale: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PanelSnapshot {
    pub connected: bool,
    pub communication_error: bool,
    pub mode: PanelModeValidity,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub program_protect: bool,
    /// False when the protect input could not be trusted; callers must fail closed.
    pub program_protect_known: bool,
    pub estop: bool,
    pub current_tool: u16,
    pub next_tool: u16,
    pub buttons: u64,
    pub leds: u64,
    pub sseg_tool: i32,
    pub sseg_next_tool: i32,
    pub sseg_status: i32,
    /// Panel home/proghome outputs for HAL wiring and diagnostics.
    pub home: [bool; 4],
    pub proghome: [bool; 4],
    pub step: bool,
    pub jog: JogState,
    pub link: PanelLinkStats,
    pub capabilities: PanelCapabilities,
    pub pendant: PendantState,
    pub snapshot_age_ms: u64,
    /// Simulation panel power state; real panels derive this from HAL status.
    pub power_on: Option<bool>,
    /// One-shot: the panel E-stop was just released and the controller should
    /// leave E-stop. Kept separate from `power_on` so releasing the mushroom
    /// clears the latch without powering the machine back on.
    #[serde(default)]
    pub estop_reset_pulse: bool,
    /// Momentary POWER ON key edge (simulation panels).
    #[serde(default)]
    pub power_on_pulse: bool,
    /// Momentary POWER OFF key edge (simulation panels).
    #[serde(default)]
    pub power_off_pulse: bool,
}

impl PanelSnapshot {
    /// Program protect is a safety input: unknown or stale means locked.
    pub fn protect_effective(&self) -> bool {
        !self.program_protect_known || self.program_protect
    }

    /// Panel offline also blocks machine-modifying UI actions.
    pub fn ui_actions_blocked(&self) -> bool {
        !self.connected
    }
}

/// Display state driven from the controller/HMI into the panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PanelDisplay {
    pub leds: u64,
    pub sseg_tool: i32,
    pub sseg_next_tool: i32,
    /// `linc.status-display`: 54…59 or -1 for `--`.
    pub sseg_status: i32,
    pub vel: [f64; 3],
    /// Any axis homing; individual home outputs are inhibited while true.
    pub homing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HalOutputs {
    pub hmi_alive: bool,
    pub tool_change_confirm: bool,
    pub status_display: Option<i32>,
    /// Increments whenever the backend wants one `tool-change-confirm` pulse.
    pub tool_change_confirm_seq: u64,
    /// Jog selections the HMI owns. They live here rather than in `panel`
    /// because `panel` is rebuilt from the panel service on every poll, which
    /// would reset them; the bridge carries these across polls.
    pub jog_increment: Option<f64>,
    pub jog_speed: Option<f64>,
    pub heartbeat_seq: u64,
}

impl Default for HalOutputs {
    fn default() -> Self {
        Self {
            hmi_alive: false,
            tool_change_confirm: false,
            status_display: None,
            tool_change_confirm_seq: 0,
            jog_increment: None,
            jog_speed: None,
            heartbeat_seq: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlarmSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl AlarmSeverity {
    /// A fault the operator must act on, as opposed to an informational
    /// message. Used by the Diagnostics alarm/message split.
    pub fn is_alarm(self) -> bool {
        matches!(self, Self::Warning | Self::Error | Self::Critical)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
            Self::Critical => "CRIT",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlarmSource {
    Panel,
    Program,
    Probe,
    Tool,
    Hal,
    Nml,
    Motion,
    Io,
    Ui,
    System,
}

impl AlarmSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Panel => "PANEL",
            Self::Program => "PROGRAM",
            Self::Probe => "PROBE",
            Self::Tool => "TOOL",
            Self::Hal => "HAL",
            Self::Nml => "NML",
            Self::Motion => "MOTION",
            Self::Io => "I/O",
            Self::Ui => "UI",
            Self::System => "SYSTEM",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: u64,
    pub timestamp_ms: u64,
    pub severity: AlarmSeverity,
    pub source: AlarmSource,
    pub code: String,
    pub message: String,
    pub acknowledged: bool,
}

impl Alarm {
    pub fn new(
        id: u64,
        severity: AlarmSeverity,
        source: AlarmSource,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            timestamp_ms: now_ms(),
            severity,
            source,
            code: code.into(),
            message: message.into(),
            acknowledged: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub sequence: u64,
    pub captured_at_ms: u64,
    pub age_ms: u64,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineSnapshot {
    pub meta: SnapshotMeta,
    pub variant: MachineVariant,
    pub machine_name: String,
    pub simulated: bool,
    pub task: TaskState,
    pub machine: MachineState,
    pub positions: Positions,
    pub offsets: WorkOffsetSet,
    pub tool_table: ToolTableState,
    pub spindles: Vec<SpindleState>,
    pub feed: FeedState,
    pub program: ProgramState,
    pub tool_change: ToolChangeState,
    pub probe: ProbeState,
    pub atc: Option<AtcState>,
    pub pendant: PendantState,
    pub panel: PanelSnapshot,
    pub hal: HalOutputs,
    pub alarms: VecDeque<Alarm>,
    /// Live tool-tip trail in machine coordinates, oldest first.
    pub live_trail: Arc<Vec<[f64; 3]>>,
    /// Actual tool tip in machine coordinates, if known.
    pub tool_tip: Option<[f64; 3]>,
}

impl MachineSnapshot {
    pub fn empty(variant: MachineVariant) -> Self {
        Self {
            meta: SnapshotMeta::default(),
            variant,
            machine_name: String::new(),
            simulated: false,
            task: TaskState::default(),
            machine: MachineState::default(),
            positions: Positions::default(),
            offsets: WorkOffsetSet::default(),
            tool_table: ToolTableState::default(),
            spindles: Vec::new(),
            feed: FeedState::default(),
            program: ProgramState::default(),
            tool_change: ToolChangeState::default(),
            probe: ProbeState::default(),
            atc: None,
            pendant: PendantState::default(),
            panel: PanelSnapshot::default(),
            hal: HalOutputs::default(),
            alarms: VecDeque::new(),
            live_trail: Arc::new(Vec::new()),
            tool_tip: None,
        }
    }

    /// A machine-modifying UI action requires a live panel and valid mode.
    pub fn can_modify_machine(&self) -> bool {
        self.panel.connected && !self.panel.communication_error && self.panel.mode.is_valid()
    }

    /// Program/offset/tool writes require idle, machine on, and no protect.
    pub fn writes_allowed(&self) -> bool {
        self.task.power == MachinePower::On
            && self.task.interp == InterpState::Idle
            && !self.task.running
            && !self.panel.protect_effective()
            && self.can_modify_machine()
    }

    pub fn program_editing_allowed(&self) -> bool {
        self.task.interp == InterpState::Idle
            && !self.task.running
            && !self.panel.protect_effective()
            && self.panel.connected
            && self.panel.mode.mode() == Some(PanelMode::Edit)
    }

    pub fn program_viewing_allowed(&self) -> bool {
        true
    }

    pub fn mdi_allowed(&self) -> bool {
        self.panel.connected
            && !self.panel.communication_error
            && self.panel.mode.mode() == Some(PanelMode::Mdi)
            && self.task.power == MachinePower::On
            && self.task.interp == InterpState::Idle
            && !self.task.running
            && self.machine.homed.iter().all(|(_, homed)| *homed)
            && !self.machine.homed.is_empty()
    }

    /// Probe/tool-change issue uses Probe Basic `issue_mdi` preconditions.
    pub fn issue_allowed(&self) -> Result<(), String> {
        if !self.panel.connected || self.panel.communication_error {
            return Err("panel link is not available".into());
        }
        if !self.panel.mode.is_valid() {
            return Err("panel mode is invalid; failing closed".into());
        }
        if self.task.power != MachinePower::On {
            return Err("machine is not ON".into());
        }
        if self.task.interp != InterpState::Idle || self.task.running {
            return Err("interpreter is not idle".into());
        }
        if self.program.running {
            return Err("a program is running".into());
        }
        if self.machine.homed.is_empty() || !self.machine.homed.iter().all(|(_, homed)| *homed) {
            return Err("not all axes are homed".into());
        }
        Ok(())
    }

    pub fn active_wcs(&self) -> Option<u8> {
        self.offsets
            .active_index
            .filter(|index| (1..=9).contains(index))
    }

    /// Two-digit panel status. Unknown never guesses: a stale snapshot or an
    /// offline panel reports `--` rather than the last known value.
    pub fn status_display_value(&self, source: StatusSource) -> Option<i32> {
        if self.meta.stale
            || !self.panel.connected
            || self.panel.communication_error
        {
            return None;
        }
        match source {
            StatusSource::Wcs => self
                .active_wcs()
                .filter(|index| (1..=6).contains(index))
                .map(|index| 53 + index as i32),
            StatusSource::SpindleLoad => self
                .spindles
                .first()
                .and_then(|spindle| spindle.load)
                .map(|load| load.round().clamp(0.0, 99.0) as i32),
            StatusSource::Units => match self.feed.units.to_ascii_lowercase().as_str() {
                "mm" | "metric" => Some(1),
                "inch" | "in" | "imperial" => Some(2),
                _ => None,
            },
        }
    }

    pub fn alarm_count(&self) -> usize {
        self.alarms
            .iter()
            .filter(|alarm| {
                matches!(
                    alarm.severity,
                    AlarmSeverity::Error | AlarmSeverity::Critical
                )
            })
            .count()
    }

    pub fn message_count(&self) -> usize {
        self.alarms
            .iter()
            .filter(|alarm| matches!(alarm.severity, AlarmSeverity::Info | AlarmSeverity::Warning))
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusSource {
    Wcs,
    SpindleLoad,
    Units,
}

impl StatusSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "wcs" => Some(Self::Wcs),
            "spindle_load" | "spindle-load" => Some(Self::SpindleLoad),
            "units" => Some(Self::Units),
            _ => None,
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn format_clock(timestamp_ms: u64) -> String {
    #[cfg(unix)]
    {
        let seconds = (timestamp_ms / 1000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&seconds, &mut tm) };
        if !result.is_null() {
            return format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
        }
    }
    let seconds = (timestamp_ms / 1000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

pub fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms / 1000) % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_fails_closed_when_unknown() {
        let panel = PanelSnapshot {
            program_protect: false,
            program_protect_known: false,
            ..Default::default()
        };
        assert!(panel.protect_effective());
    }

    #[test]
    fn one_hot_mode_invalid_lists_active_inputs() {
        let validity = PanelModeValidity::Invalid(vec![PanelMode::Auto, PanelMode::Mdi]);
        assert_eq!(validity.mode(), None);
        assert_eq!(validity.label(), "UNKNOWN");
    }

    #[test]
    fn issue_fails_closed_with_invalid_panel_mode() {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel.connected = true;
        snapshot.panel.mode = PanelModeValidity::Invalid(vec![PanelMode::Auto, PanelMode::Mdi]);
        snapshot.task.power = MachinePower::On;
        snapshot.task.interp = InterpState::Idle;
        snapshot.machine.homed.insert("X", true);
        assert!(snapshot.issue_allowed().is_err());
        snapshot.panel.mode = PanelModeValidity::Valid(PanelMode::Auto);
        assert!(snapshot.issue_allowed().is_ok());
    }

    #[test]
    fn wcs_status_maps_54_to_59_and_never_guesses() {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        // The two-digit display requires a live, fresh panel: a stale snapshot
        // or an offline panel reports `--` instead of the last work offset.
        snapshot.panel.connected = true;
        snapshot.panel.communication_error = false;
        snapshot.meta.stale = false;
        snapshot.offsets.active_index = Some(1);
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), Some(54));
        snapshot.offsets.active_index = Some(6);
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), Some(59));
        snapshot.offsets.active_index = Some(7);
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), None);
        snapshot.offsets.active_index = None;
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), None);

        // A stale or offline panel must never keep showing the last offset.
        snapshot.offsets.active_index = Some(1);
        snapshot.meta.stale = true;
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), None);
        snapshot.meta.stale = false;
        snapshot.panel.connected = false;
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), None);
        snapshot.panel.connected = true;
        snapshot.panel.communication_error = true;
        assert_eq!(snapshot.status_display_value(StatusSource::Wcs), None);
    }

    #[test]
    fn axis_map_preserves_ini_order() {
        let map = AxisMap::from_entries([("Z".to_string(), 1), ("X".to_string(), 2)]);
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["Z", "X"]);
    }
}
