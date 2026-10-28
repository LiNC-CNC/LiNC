//! Layered machine profile loader.
//!
//! Rules implemented here:
//! * `base.toml` is shared defaults and cannot be launched by itself.
//! * `extends` is resolved relative to the profile, tables deep-merge and
//!   arrays replace.
//! * Unknown keys are errors.
//! * Paths are resolved relative to the profile (or the INI for INI paths).
//! * Safety policy cannot be weakened.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::command::LincOutput;
use crate::ini::{expand_tilde, IniError, MachineIni};
use crate::model::{MachineVariant, StatusSource, ToolChangerKind};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no profile supplied; use --config <profile.toml> or LINC_CONFIG")]
    NoProfile,
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("configuration error: {0}")]
    Invalid(String),
    #[error("extended profile {path} does not exist")]
    MissingBase { path: PathBuf },
    #[error(transparent)]
    Ini(#[from] IniError),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub profile: String,
    #[serde(default)]
    pub extends: Option<String>,
    pub machine: MachineConfig,
    #[serde(default)]
    pub mill: Option<MillConfig>,
    #[serde(default)]
    pub lathe: Option<LatheConfig>,
    pub ui: UiConfig,
    pub program: ProgramConfig,
    pub files: FilesConfig,
    pub probe: ProbeConfig,
    pub tools: ToolsConfig,
    pub manual: ManualConfig,
    pub homing: HomingConfig,
    pub mdi: MdiConfig,
    pub toolpath: ToolpathConfig,
    pub keys: KeysConfig,
    pub help: HelpConfig,
    pub safety: SafetyConfig,
    #[serde(default)]
    pub jog: JogConfig,
    pub backend: BackendConfig,
    pub hal: HalConfig,
    pub panel: PanelConfig,
    pub pendant: PendantConfig,
    pub preferences: PreferencesConfig,
    pub diagnostics: DiagnosticsConfig,
    #[serde(default)]
    pub simulation: SimulationConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    pub name: String,
    pub kind: String,
    pub gui: String,
    pub linuxcnc_ini: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MillConfig {
    #[serde(default)]
    pub max_axes: Option<u8>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatheConfig {
    #[serde(default)]
    pub turret_stations: Option<u16>,
    #[serde(default)]
    pub power_turret: Option<bool>,
    #[serde(default)]
    pub live_tooling: Option<bool>,
    #[serde(default)]
    pub live_tool_max_rpm: Option<f64>,
    #[serde(default)]
    pub c_axis: Option<bool>,
    #[serde(default)]
    pub y_axis: Option<bool>,
    #[serde(default)]
    pub sub_spindle: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    pub theme: String,
    pub start_page: String,
    pub window_title: String,
    pub scale: String,
    pub axis_order: Vec<String>,
    pub pages: Vec<String>,
    /// Load bars on the Machine page: any of the machine's axes, plus `"S"`
    /// for the spindle. An empty list removes the SERVO LOAD pane.
    #[serde(default)]
    pub servo_load: Vec<String>,
    /// Startup window state: `"auto"` takes the whole screen on a real machine
    /// and opens a window in simulation. A control-panel machine that wants a
    /// borderless panel window sets `"maximized"` and `decorations = false`.
    #[serde(default = "default_window_mode")]
    pub window_mode: String,
    /// Window-manager decorations (title bar, resize border).
    #[serde(default = "default_true")]
    pub decorations: bool,
}

fn default_window_mode() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramConfig {
    pub default_view: String,
    pub load_view: String,
    pub views: Vec<String>,
    pub edit_view: String,
    pub line_numbers: bool,
    pub highlight_current_line: bool,
    pub autosave: bool,
    pub live_preview: bool,
    pub preview_requires_syntax_ok: bool,
    pub preview_debounce_ms: u64,
    pub tab_width: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesConfig {
    pub nc_files: String,
    pub usb_backend: String,
    pub usb_automount: bool,
    pub usb_mount_options: Vec<String>,
    pub load_extensions: Vec<String>,
    pub overwrite: String,
    pub copy_while_running: bool,
    /// Show the `..` row in NC_FILES at its root, so the browser can read the
    /// tree above PROGRAM_PREFIX. Loading is still limited to NC_FILES.
    #[serde(default)]
    pub show_parent_above_root: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    pub enabled: bool,
    pub tool_number: u16,
    pub wcs: String,
    pub subroutines: String,
    pub clearance: f64,
    pub probe_depth: f64,
    pub fast_feed: f64,
    pub slow_feed: f64,
    pub probe_diameter: f64,
    pub max_travel: f64,
    pub calibration_diameter: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    pub changer: String,
    pub atc_pockets: String,
    pub manual_confirm: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualConfig {
    pub auto_open: bool,
    pub close_on_mode_exit: bool,
    pub show_machine_coordinates: bool,
    pub show_dtg: bool,
    pub trail: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomingConfig {
    pub auto_open: bool,
    pub close_on_mode_exit: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdiConfig {
    pub execute_on_enter: bool,
    pub auto_focus: bool,
    /// Cap on the MDI block buffer: the blocks entered this session, oldest
    /// dropped first. The buffer outlives leaving the mode, so it is not a
    /// per-visit list.
    pub history_limit: usize,
    pub max_command_length: usize,
    /// Blocks written ahead of the buffer when the save dialog asks for a
    /// pre/postamble. A lathe profile overrides the plane word: G17 is XY and
    /// would put a lathe's XZ arcs in the wrong plane.
    pub save_preamble: Vec<String>,
    /// Blocks written after the buffer, so the saved program stops the spindle,
    /// stops coolant and rewinds instead of leaving the machine running.
    pub save_postamble: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolpathConfig {
    pub show_grid: bool,
    pub show_live_trail: bool,
    pub trail_max_points: usize,
    pub clear_trail_action: bool,
    pub show_program_bounds: bool,
    pub show_machine_bounds: bool,
    pub show_stock: bool,
    /// Stock fill opacity, 0-255. 0 draws no stock at all; the default is fully
    /// transparent so the block cannot hide the toolpath.
    #[serde(default)]
    pub stock_alpha: u8,
    pub show_program_labels: bool,
    pub show_machine_labels: bool,
    pub show_program_ticks: bool,
    pub show_machine_ticks: bool,
    pub perspective: bool,
    pub multitool_colors: bool,
    pub highlight_selected_line: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    pub machine: String,
    pub program: String,
    pub program_cycle_view: String,
    /// Cycle the toolpath view ISO -> X -> Y -> Z.
    pub view_cycle: String,
    /// Show or hide the program bounding box and its dimensions.
    pub toggle_bounds: String,
    /// REMAINING: drop the live trail and eat the program path block by
    /// completed block, so what is drawn is what is still to come.
    pub remaining: String,
    /// Lathe only: mirror X about the Z centreline in the XZ/TURN toolpath.
    pub toggle_lathe_mirror: String,
    /// Lathe only: read the X position as a radius (`X`) or a diameter (`2X`).
    pub toggle_lathe_diameter: String,
    pub graph_fit: String,
    pub graph_clear: String,
    /// Pages with a toolpath: pan the view with the modifier and the arrows.
    /// Taken only while the pointer is over the toolpath, so the shell keeps
    /// Ctrl+Left/Right for region focus and the editor keeps Ctrl+Up/Down.
    pub view_pan_left: String,
    pub view_pan_right: String,
    pub view_pan_up: String,
    pub view_pan_down: String,
    /// Pages with a toolpath: zoom the view in and out.
    pub view_zoom_in: String,
    pub view_zoom_out: String,
    /// Page-local to Program: jump to a line number.
    pub goto_line: String,
    pub program_run_from_line: String,
    pub files_delete: String,
    pub tools: String,
    pub offsets: String,
    /// Page-local to the Tools page: type a tool number and load it, for an
    /// operator who knows the number and does not want to cursor to the row.
    pub tool_number: String,
    /// Toggles the limit-switch override; it has no button on the jog screens.
    pub limits_override: String,
    pub diagnostics: String,
    /// Open the offline G-code reference from any page.
    pub help: String,
    /// Open the full-screen messages dialog from any page.
    pub messages: String,
    pub back: String,
    pub quit: String,
    pub focus_next: String,
    pub focus_prev: String,
    pub activate: String,
    pub open: String,
    pub save: String,
    pub undo: String,
    pub redo: String,
    pub find: String,
    pub select_all: String,
    pub copy: String,
    pub cut: String,
    pub paste: String,
    pub refresh: String,
    pub mdi_issue: String,
    pub mdi_clear: String,
    pub mdi_history_up: String,
    pub mdi_history_down: String,
    /// Page-local to the MDI pane: move the selected buffer block up/down.
    /// Ctrl by default: the panel keyboard has no Shift key.
    pub mdi_line_up: String,
    pub mdi_line_down: String,
    /// Page-local to the MDI pane: drop the selected buffer block.
    pub mdi_buffer_delete: String,
    pub help_search: String,
}

impl KeysConfig {
    /// The spec-default bindings (SPEC §3.1, CONFIGURATION.md §10). Used by
    /// tests and as the reference the Settings screen shows.
    pub fn spec_default() -> Self {
        Self {
            machine: "F5".into(),
            program: "F6".into(),
            program_cycle_view: "F10".into(),
            view_cycle: "Ctrl+F10".into(),
            toggle_bounds: "Ctrl+B".into(),
            toggle_lathe_mirror: "Ctrl+J".into(),
            toggle_lathe_diameter: "Ctrl+E".into(),
            remaining: "Ctrl+M".into(),
            graph_fit: "Ctrl+0".into(),
            graph_clear: "Ctrl+L".into(),
            view_pan_left: "Ctrl+Left".into(),
            view_pan_right: "Ctrl+Right".into(),
            view_pan_up: "Ctrl+Up".into(),
            view_pan_down: "Ctrl+Down".into(),
            view_zoom_in: "Ctrl+Plus".into(),
            view_zoom_out: "Ctrl+Minus".into(),
            goto_line: "Ctrl+G".into(),
            program_run_from_line: "Ctrl+R".into(),
            files_delete: "Delete".into(),
            tools: "F7".into(),
            offsets: "F8".into(),
            tool_number: "Ctrl+T".into(),
            limits_override: "Ctrl+Shift+O".into(),
            diagnostics: "Ctrl+D".into(),
            help: "Ctrl+H".into(),
            messages: "M".into(),
            back: "Escape".into(),
            quit: "Ctrl+Q".into(),
            focus_next: "Down".into(),
            focus_prev: "Up".into(),
            activate: "Enter".into(),
            open: "Ctrl+O".into(),
            save: "Ctrl+S".into(),
            undo: "Ctrl+Z".into(),
            redo: "Ctrl+Y".into(),
            find: "Ctrl+F".into(),
            select_all: "Ctrl+A".into(),
            copy: "Ctrl+C".into(),
            cut: "Ctrl+X".into(),
            paste: "Ctrl+V".into(),
            refresh: "Ctrl+R".into(),
            mdi_issue: "Enter".into(),
            mdi_clear: "Escape".into(),
            mdi_history_up: "Up".into(),
            mdi_history_down: "Down".into(),
            mdi_line_up: "Ctrl+Up".into(),
            mdi_line_down: "Ctrl+Down".into(),
            mdi_buffer_delete: "Ctrl+Backspace".into(),
            help_search: "/".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpConfig {
    pub source: String,
    pub bundle: String,
    pub search: bool,
    pub history_limit: usize,
}

/// Continuous jog speed ceilings, in machine units per minute.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JogConfig {
    /// Fastest jog at 100 %.
    pub max_speed: f64,
    /// Fastest jog at 100 % while the panel's RAPID key is held.
    pub rapid_speed: f64,
}

impl Default for JogConfig {
    fn default() -> Self {
        Self {
            max_speed: 3000.0,
            rapid_speed: 6000.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    pub machine_control: String,
    pub software_estop: bool,
    pub software_cycle_control: bool,
    pub software_jog: bool,
    pub software_spindle: bool,
    pub software_coolant: bool,
    pub software_overrides: bool,
    pub program_edit_while_running: String,
    pub program_protect: String,
    pub save_requires_idle: bool,
    pub confirm_discard: bool,
    pub mdi_execution: String,
    pub quit_requires_idle: bool,
    pub confirm_quit: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    pub kind: String,
    pub transport: String,
    pub status_hz: u32,
    pub hal_hz: u32,
    pub connect_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HalConfig {
    pub panel_component: String,
    pub linc_component: String,
    pub write_policy: String,
    pub output_prefix: String,
    pub allowed_outputs: Vec<String>,
    pub pins: HalPinsConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HalPinsConfig {
    pub panel_connected: String,
    pub panel_error: String,
    pub program_protect: String,
    pub feed_override: String,
    pub rapid_override: String,
    pub spindle_override: String,
    pub spindle_load: String,
    pub spindle_speed: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    pub owner: String,
    pub kind: String,
    pub driver: String,
    pub mode_encoding: String,
    pub override_encoding: String,
    pub read_only: bool,
    #[serde(default)]
    pub label: Option<String>,
    /// Whether the panel link's own E-stop bit counts as an E-stop.
    ///
    /// On a machine whose mushroom is hardwired into the safety chain, LinuxCNC
    /// already reports it through the task status, and the panel's status bit
    /// may be inverted or meaningless — displaying it then shows E-STOP on a
    /// machine that is running. Such a machine sets this to `false`; the
    /// simulation relies on the panel owning the latch and keeps the default.
    #[serde(default = "default_true")]
    pub estop_from_panel: bool,
    pub status: PanelStatusConfig,
    pub link: PanelLinkConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelStatusConfig {
    pub source: String,
    pub unknown: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelLinkConfig {
    pub read_only: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub baud: Option<u32>,
    #[serde(default)]
    pub data_bits: Option<u8>,
    #[serde(default)]
    pub parity: Option<String>,
    #[serde(default)]
    pub stop_bits: Option<u8>,
    #[serde(default)]
    pub cycle_time_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendantConfig {
    pub enabled: bool,
    pub kind: String,
    pub jog_only: bool,
    pub ignored_functions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesConfig {
    pub file: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub event_log_capacity: usize,
    pub panel_link_stats: String,
    #[serde(default)]
    pub turret_tab: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rio: Option<String>,
    #[serde(default)]
    pub estop: Option<String>,
    #[serde(default)]
    pub panel: Option<String>,
    #[serde(default)]
    pub panel_window: Option<bool>,
    #[serde(default)]
    pub panel_title: Option<String>,
    #[serde(default)]
    pub panel_scale: Option<f64>,
}

/// Fully resolved profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub config: Config,
    pub path: PathBuf,
    pub machine_ini_path: PathBuf,
    pub machine_ini: MachineIni,
    pub preferences_path: PathBuf,
    pub variant: MachineVariant,
    pub status_source: StatusSource,
    pub changer: ToolChangerKind,
}

impl UiConfig {
    /// Effective UI zoom factor. `"auto"` leaves the native scale untouched;
    /// numeric strings are validated by the loader.
    pub fn zoom_factor(&self) -> Option<f32> {
        let value = self.scale.trim().to_ascii_lowercase();
        if value == "auto" {
            return None;
        }
        let is_percent = value.ends_with('%');
        value
            .trim_end_matches('x')
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map(|value| if is_percent { value / 100.0 } else { value })
    }
}

impl Profile {
    pub fn ui_scale(&self) -> Option<f32> {
        self.config.ui.zoom_factor()
    }

    pub fn is_sim(&self) -> bool {
        self.config.machine.kind == "sim"
    }

    pub fn panel_driver(&self) -> &str {
        &self.config.panel.driver
    }

    pub fn nc_files_root(&self) -> &Path {
        &self.machine_ini.program_prefix
    }

    pub fn max_axes(&self) -> usize {
        match self.variant {
            MachineVariant::Mill => self
                .config
                .mill
                .as_ref()
                .and_then(|mill| mill.max_axes)
                .map(|value| value as usize)
                .unwrap_or(5),
            MachineVariant::Lathe => {
                let mut count = 2; // X/Z
                if self.config.lathe.as_ref().and_then(|lathe| lathe.c_axis) == Some(true) {
                    count += 1;
                }
                if self.config.lathe.as_ref().and_then(|lathe| lathe.y_axis) == Some(true) {
                    count += 1;
                }
                count
            }
        }
    }

    pub fn turret_stations(&self) -> Option<u16> {
        self.config
            .lathe
            .as_ref()
            .and_then(|lathe| lathe.turret_stations)
            .or(self.machine_ini.atc_pockets)
    }

    pub fn live_tooling(&self) -> bool {
        self.config
            .lathe
            .as_ref()
            .and_then(|lathe| lathe.live_tooling)
            .unwrap_or(false)
    }

    pub fn power_turret(&self) -> bool {
        self.config
            .lathe
            .as_ref()
            .and_then(|lathe| lathe.power_turret)
            .unwrap_or(false)
    }

    pub fn load_path() -> Result<PathBuf, ConfigError> {
        let value = std::env::var_os("LINC_CONFIG")
            .map(PathBuf::from)
            .ok_or(ConfigError::NoProfile)?;
        Ok(value)
    }

    pub fn load(path: &Path, ini_override: Option<PathBuf>) -> Result<Self, ConfigError> {
        let profile_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
        let base_dir = profile_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let raw_text =
            std::fs::read_to_string(&profile_path).map_err(|source| ConfigError::Read {
                path: profile_path.clone(),
                source,
            })?;
        let profile_value: toml::Value =
            toml::from_str(&raw_text).map_err(|source| ConfigError::Toml {
                path: profile_path.clone(),
                source,
            })?;
        let table = profile_value
            .as_table()
            .ok_or_else(|| ConfigError::Invalid("profile root must be a TOML table".into()))?;

        if let Some(extends) = table.get("extends").and_then(toml::Value::as_str) {
            let base_path = resolve_path(&base_dir, extends);
            if !base_path.exists() {
                return Err(ConfigError::MissingBase { path: base_path });
            }
            let base_text =
                std::fs::read_to_string(&base_path).map_err(|source| ConfigError::Read {
                    path: base_path.clone(),
                    source,
                })?;
            let base_value: toml::Value =
                toml::from_str(&base_text).map_err(|source| ConfigError::Toml {
                    path: base_path,
                    source,
                })?;
            let mut merged = base_value;
            deep_merge(&mut merged, profile_value);
            let config: Config = merged.try_into().map_err(|source| ConfigError::Toml {
                path: profile_path.clone(),
                source,
            })?;
            Self::finish(config, profile_path, base_dir, ini_override)
        } else {
            let config: Config = profile_value
                .try_into()
                .map_err(|source| ConfigError::Toml {
                    path: profile_path.clone(),
                    source,
                })?;
            if config.extends.is_some() {
                return Err(ConfigError::Invalid("extends must be a string path".into()));
            }
            Self::finish(config, profile_path, base_dir, ini_override)
        }
    }

    fn finish(
        mut config: Config,
        profile_path: PathBuf,
        base_dir: PathBuf,
        ini_override: Option<PathBuf>,
    ) -> Result<Self, ConfigError> {
        // Paths resolve relative to the profile.
        let ini_path =
            ini_override.unwrap_or_else(|| resolve_path(&base_dir, &config.machine.linuxcnc_ini));
        config.machine.linuxcnc_ini = ini_path.to_string_lossy().into_owned();
        if !ini_path.is_file() {
            return Err(ConfigError::Invalid(format!(
                "machine.linuxcnc_ini does not exist: {}",
                ini_path.display()
            )));
        }
        let machine_ini = MachineIni::parse_file(&ini_path)?;
        validate(&config, &machine_ini, &base_dir)?;
        let variant = if config.machine.gui == "lathe" {
            MachineVariant::Lathe
        } else {
            MachineVariant::Mill
        };
        let status_source = StatusSource::parse(&config.panel.status.source)
            .ok_or_else(|| ConfigError::Invalid("invalid panel.status.source".into()))?;
        let changer = ToolChangerKind::parse(&config.tools.changer)
            .ok_or_else(|| ConfigError::Invalid("invalid tools.changer".into()))?;
        let preferences_template = config.preferences.file.clone();
        let preferences_path = resolve_path(
            &base_dir,
            &preferences_template.replace("{profile}", &config.profile),
        );
        Ok(Self {
            config,
            path: profile_path,
            machine_ini_path: ini_path,
            machine_ini,
            preferences_path,
            variant,
            status_source,
            changer,
        })
    }

    pub fn summary(&self) -> Vec<(String, String)> {
        let axes = self.machine_ini.coordinates.join("");
        vec![
            ("Profile".into(), self.config.profile.clone()),
            ("Machine".into(), self.config.machine.name.clone()),
            (
                "Kind".into(),
                if self.is_sim() {
                    "simulation".into()
                } else {
                    "real".into()
                },
            ),
            ("GUI".into(), self.config.machine.gui.clone()),
            ("INI".into(), self.machine_ini_path.display().to_string()),
            (
                "Axes".into(),
                format!("{axes} ({} joints)", self.machine_ini.joints),
            ),
            ("Units".into(), self.machine_ini.linear_units.clone()),
            (
                "Program prefix".into(),
                self.machine_ini.program_prefix.display().to_string(),
            ),
            (
                "Tool table".into(),
                format!(
                    "{} [{}]",
                    self.machine_ini.tool_table.display(),
                    self.machine_ini.tool_table_columns
                ),
            ),
            ("Panel driver".into(), self.config.panel.driver.clone()),
            ("Changer".into(), self.config.tools.changer.clone()),
        ]
    }
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let expanded = expand_tilde(value.trim());
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

fn deep_merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(base_value) => deep_merge(base_value, value),
                    None => {
                        base_table.insert(key, value);
                    }
                }
            }
        }
        (base_value, overlay_value) => *base_value = overlay_value,
    }
}

fn validate(config: &Config, ini: &MachineIni, profile_dir: &Path) -> Result<(), ConfigError> {
    if config.schema_version != 1 {
        return Err(ConfigError::Invalid(format!(
            "unsupported schema_version {}",
            config.schema_version
        )));
    }
    if config.profile.trim().is_empty() {
        return Err(ConfigError::Invalid("profile must not be empty".into()));
    }
    if !matches!(
        config.ui.window_mode.as_str(),
        "auto" | "windowed" | "maximized" | "fullscreen"
    ) {
        return Err(ConfigError::Invalid(format!(
            "ui.window_mode must be auto, windowed, maximized or fullscreen (got {:?})",
            config.ui.window_mode
        )));
    }
    if config.machine.name.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "machine.name must not be empty".into(),
        ));
    }
    if config.machine.kind != "sim" && config.machine.kind != "real" {
        return Err(ConfigError::Invalid(
            "machine.kind must be sim or real".into(),
        ));
    }
    if config.machine.gui != "mill" && config.machine.gui != "lathe" {
        return Err(ConfigError::Invalid(
            "machine.gui must be mill or lathe".into(),
        ));
    }
    if let Some(panel_scale) = config.simulation.panel_scale {
        let valid = panel_scale.is_finite() && (0.5..=3.0).contains(&panel_scale);
        if !valid {
            return Err(ConfigError::Invalid(
                "simulation.panel_scale must be a factor in 0.5..3.0".into(),
            ));
        }
    }
    let scale = config.ui.scale.trim().to_ascii_lowercase();
    if scale != "auto" {
        let numeric = scale
            .trim_end_matches('x')
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| {
                if scale.ends_with('%') {
                    value / 100.0
                } else {
                    value
                }
            });
        match numeric {
            Some(value) if (0.5..=3.0).contains(&value) => {}
            _ => {
                return Err(ConfigError::Invalid(
                    "ui.scale must be auto or a factor in 0.5..3.0 (for example 1.0, 0.8, 80%)"
                        .into(),
                ))
            }
        }
    }
    // `[ui] pages` selects which entries the navigation shows. Unknown keys
    // would silently disappear from the sidebar, so reject them.
    const KNOWN_PAGES: [&str; 7] = [
        "machine",
        "program",
        "offsets",
        "tools",
        "diagnostics",
        "help",
        "probe",
    ];
    for page in &config.ui.pages {
        let key = page.trim().to_ascii_lowercase();
        if !KNOWN_PAGES.contains(&key.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "ui.pages contains '{page}'; known pages are {KNOWN_PAGES:?}"
            )));
        }
    }
    // `[ui] servo_load` names the bars on the Machine page. A typo would leave
    // a silently missing row, so every entry must exist on this machine.
    let mut servo_labels: Vec<String> = Vec::new();
    for entry in &config.ui.servo_load {
        let label = entry.trim().to_ascii_uppercase();
        let axis = ini
            .axes
            .iter()
            .any(|axis| axis.name.eq_ignore_ascii_case(&label));
        if label.is_empty() || (label != "S" && !axis) {
            return Err(ConfigError::Invalid(format!(
                "ui.servo_load contains '{entry}'; use a machine axis or S for the spindle"
            )));
        }
        if label == "S" && ini.spindles == 0 {
            return Err(ConfigError::Invalid(
                "ui.servo_load contains 'S' but the INI configures no spindle".into(),
            ));
        }
        if servo_labels.contains(&label) {
            return Err(ConfigError::Invalid(format!(
                "ui.servo_load lists '{label}' more than once"
            )));
        }
        servo_labels.push(label);
    }
    if config.panel.owner != "linc-panel" {
        return Err(ConfigError::Invalid(
            "panel.owner must be linc-panel; the HMI never owns the link".into(),
        ));
    }
    if config.panel.mode_encoding != "one_hot_steady" {
        return Err(ConfigError::Invalid(
            "panel.mode_encoding must be one_hot_steady".into(),
        ));
    }
    if config.panel.override_encoding != "analog_steady" {
        return Err(ConfigError::Invalid(
            "panel.override_encoding must be analog_steady".into(),
        ));
    }
    if !config.panel.read_only {
        return Err(ConfigError::Invalid(
            "panel.read_only must remain true".into(),
        ));
    }
    if !config.panel.link.read_only {
        return Err(ConfigError::Invalid(
            "panel.link.read_only must remain true".into(),
        ));
    }
    if config.panel.link.kind.as_deref() == Some("rs485") && config.panel.link.port.is_none() {
        return Err(ConfigError::Invalid(
            "panel.link.port is required for rs485".into(),
        ));
    }
    StatusSource::parse(&config.panel.status.source).ok_or_else(|| {
        ConfigError::Invalid("panel.status.source must be wcs, spindle_load or units".into())
    })?;
    if config.panel.status.unknown != "--" {
        return Err(ConfigError::Invalid(
            "panel.status.unknown must be --".into(),
        ));
    }

    // Safety policy is immutable.
    if config.safety.machine_control != "panel-with-explicit-mdi" {
        return Err(ConfigError::Invalid(
            "safety.machine_control cannot be weakened".into(),
        ));
    }
    for (name, value) in [
        ("software_estop", config.safety.software_estop),
        (
            "software_cycle_control",
            config.safety.software_cycle_control,
        ),
        ("software_jog", config.safety.software_jog),
        ("software_spindle", config.safety.software_spindle),
        ("software_coolant", config.safety.software_coolant),
        ("software_overrides", config.safety.software_overrides),
    ] {
        if value {
            return Err(ConfigError::Invalid(format!("safety.{name} must be false")));
        }
    }
    if config.safety.program_edit_while_running != "read-only"
        || config.safety.program_protect != "panel"
        || config.safety.mdi_execution != "explicit-action-only"
        || !config.safety.quit_requires_idle
        || !config.safety.confirm_quit
        || !config.safety.save_requires_idle
        || !config.safety.confirm_discard
    {
        return Err(ConfigError::Invalid(
            "safety policy flags cannot be weakened".into(),
        ));
    }

    if config.hal.write_policy != "declared-outputs-only" {
        return Err(ConfigError::Invalid(
            "hal.write_policy must be declared-outputs-only".into(),
        ));
    }
    let expected: Vec<&str> = LincOutput::ALLOWED
        .iter()
        .map(|output| output.short_name())
        .collect();
    let actual: Vec<&str> = config
        .hal
        .allowed_outputs
        .iter()
        .map(String::as_str)
        .collect();
    if actual != expected {
        return Err(ConfigError::Invalid(format!(
            "hal.allowed_outputs must be exactly {:?}",
            expected
        )));
    }
    if config.hal.output_prefix != "linc." {
        return Err(ConfigError::Invalid(
            "hal.output_prefix must be linc.".into(),
        ));
    }
    if config.hal.panel_component != "cnc_panel" {
        return Err(ConfigError::Invalid(
            "hal.panel_component must be cnc_panel".into(),
        ));
    }

    let required_mount_flags = ["nosuid", "nodev", "noexec"];
    for flag in required_mount_flags {
        if !config
            .files
            .usb_mount_options
            .iter()
            .any(|value| value == flag)
        {
            return Err(ConfigError::Invalid(format!(
                "files.usb_mount_options must include {flag}"
            )));
        }
    }
    if config.files.nc_files != "ini" {
        return Err(ConfigError::Invalid(
            "files.nc_files must be ini; do not duplicate PROGRAM_PREFIX".into(),
        ));
    }
    if config.files.usb_backend != "udisks2" && config.files.usb_backend != "none" {
        return Err(ConfigError::Invalid(
            "files.usb_backend must be udisks2 or none".into(),
        ));
    }
    if config.files.overwrite != "confirm" {
        return Err(ConfigError::Invalid(
            "files.overwrite must be confirm".into(),
        ));
    }

    let changer = ToolChangerKind::parse(&config.tools.changer)
        .ok_or_else(|| ConfigError::Invalid("invalid tools.changer".into()))?;
    if changer.uses_pockets() && !ini.tool_table_columns.contains('P') {
        return Err(ConfigError::Invalid(format!(
            "tools.changer={} requires a standard P column in TOOL_TABLE_COLUMNS ({} found)",
            config.tools.changer, ini.tool_table_columns
        )));
    }
    // A manual machine may still declare P: the column is hidden in the UI,
    // but LinuxCNC needs it in the file to hold more than one tool.
    if changer == ToolChangerKind::Manual && config.tools.atc_pockets != "ini" {
        return Err(ConfigError::Invalid(
            "tools.atc_pockets must be ini or absent for manual changers".into(),
        ));
    }

    if config.machine.gui == "lathe" {
        if config.probe.enabled {
            return Err(ConfigError::Invalid(
                "the mill-oriented touch probe must be disabled for lathe profiles".into(),
            ));
        }
    }
    if config.probe.subroutines != "ini" {
        return Err(ConfigError::Invalid("probe.subroutines must be ini".into()));
    }
    if config.homing.auto_open != true && config.machine.gui == "mill" {
        // Homing is panel-driven; opening automatically is part of the contract.
        return Err(ConfigError::Invalid("homing.auto_open must be true".into()));
    }
    if !config.manual.auto_open || !config.manual.close_on_mode_exit {
        return Err(ConfigError::Invalid(
            "manual.auto_open and close_on_mode_exit must be true".into(),
        ));
    }
    if !config.toolpath.show_live_trail || !config.toolpath.clear_trail_action {
        return Err(ConfigError::Invalid(
            "toolpath.show_live_trail and clear_trail_action must be true".into(),
        ));
    }
    if config.program.autosave {
        return Err(ConfigError::Invalid(
            "program.autosave must be false".into(),
        ));
    }
    if !config.program.preview_requires_syntax_ok {
        return Err(ConfigError::Invalid(
            "program.preview_requires_syntax_ok must be true".into(),
        ));
    }
    for view in &config.program.views {
        if view != "split" && view != "code" {
            return Err(ConfigError::Invalid(format!("unknown program view {view}")));
        }
    }
    if config.help.source != "bundled" {
        return Err(ConfigError::Invalid("help.source must be bundled".into()));
    }
    if config.diagnostics.panel_link_stats != "none"
        && config.diagnostics.panel_link_stats != "optional"
        && config.diagnostics.panel_link_stats != "required"
    {
        return Err(ConfigError::Invalid(
            "diagnostics.panel_link_stats must be none, optional or required".into(),
        ));
    }
    if config.pendant.jog_only != true {
        return Err(ConfigError::Invalid("pendant.jog_only must be true".into()));
    }
    if config.pendant.enabled && config.pendant.kind != "whb04b6" {
        return Err(ConfigError::Invalid("pendant.kind must be whb04b6".into()));
    }
    if config.backend.kind != "linuxcnc" {
        return Err(ConfigError::Invalid("backend.kind must be linuxcnc".into()));
    }
    if config.backend.transport != "nml" {
        return Err(ConfigError::Invalid("backend.transport must be nml".into()));
    }
    if config.backend.status_hz < 10 || config.backend.status_hz > 20 {
        return Err(ConfigError::Invalid(
            "backend.status_hz must be 10..20".into(),
        ));
    }
    if !ini.program_prefix.exists() {
        return Err(ConfigError::Invalid(format!(
            "PROGRAM_PREFIX does not exist: {}",
            ini.program_prefix.display()
        )));
    }
    if !ini.tool_table.exists() {
        return Err(ConfigError::Invalid(format!(
            "tool table does not exist: {}",
            ini.tool_table.display()
        )));
    }
    validate_probe_bundle(config, ini, profile_dir)?;
    Ok(())
}

/// Every routine a generated Probe Basic Run calls: the parameter preamble, the
/// explicit-Apply context helper and one entry per catalogue operation. The
/// names come from `crate::probe` so the catalogue stays the single source;
/// `required_probe_routines_cover_generated_runs` holds them to `blocks()`.
fn required_probe_routines() -> Vec<String> {
    let mut names = vec![
        "touch_probe_param_update".to_string(),
        "linc_probe_context".to_string(),
    ];
    names.extend(
        crate::probe::operations()
            .iter()
            .map(|operation| operation.call.to_string()),
    );
    names.sort();
    names.dedup();
    names
}

/// LiNC's own reviewed bundle: the nearest ancestor of the profile directory
/// that carries `subroutines/probe_basic`. A profile copied outside the
/// checkout has no reference copy, so only presence is validated there.
fn reviewed_probe_bundle(profile_dir: &Path) -> Option<PathBuf> {
    profile_dir
        .ancestors()
        .map(|dir| dir.join("subroutines").join("probe_basic"))
        .find(|dir| dir.is_dir())
}

fn routine_file(dir: &Path, routine: &str) -> Option<PathBuf> {
    let file = dir.join(format!("{routine}.ngc"));
    file.is_file().then_some(file)
}

fn file_hash(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// Resolve the machine INI's `SUBROUTINE_PATH` and fail before Run when the
/// reviewed bundle is not what the search path provides. Without this a
/// machine whose INI lacks `linc_probe_context.ngc` starts, accepts a Probe
/// Run and then fails on the missing `o<linc_probe_context> call`.
fn validate_probe_bundle(
    config: &Config,
    ini: &MachineIni,
    profile_dir: &Path,
) -> Result<(), ConfigError> {
    if !config.probe.enabled {
        return Ok(());
    }
    if ini.subroutine_paths.is_empty() {
        // Minimal simulation fixtures declare no search path and never reach a
        // machine; a physical profile without one cannot resolve any routine.
        return match config.machine.kind.as_str() {
            "real" => Err(ConfigError::Invalid(format!(
                "{} declares no [RS274NGC] SUBROUTINE_PATH, so a generated Probe Basic Run cannot \
                 resolve linc_probe_context.ngc; add LiNC's reviewed bundle first, as \
                 configs/baiermill-pb.ini does (see CONFIGURATION.md, Probe Basic bundle)",
                ini.path.display()
            ))),
            _ => Ok(()),
        };
    }
    let reviewed = reviewed_probe_bundle(profile_dir);
    let mut missing: Vec<String> = Vec::new();
    let mut differing: Option<(String, PathBuf)> = None;
    for routine in required_probe_routines() {
        let Some(found) = ini
            .subroutine_paths
            .iter()
            .find_map(|dir| routine_file(dir, &routine))
        else {
            missing.push(format!("{routine}.ngc"));
            continue;
        };
        let Some(reference) = reviewed
            .as_ref()
            .and_then(|dir| routine_file(dir, &routine))
        else {
            continue;
        };
        if differing.is_none() && file_hash(&found) != file_hash(&reference) {
            differing = Some((format!("{routine}.ngc"), found));
        }
    }
    if !missing.is_empty() {
        return Err(ConfigError::Invalid(format!(
            "SUBROUTINE_PATH in {} does not provide {}; searched {}",
            ini.path.display(),
            summarize(missing),
            ini.subroutine_paths
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(":")
        )));
    }
    if let Some((file, found)) = differing {
        let message = format!(
            "the probe bundle resolved from {} differs from LiNC's reviewed copy \
             (for example {file} resolves to {}); list ../subroutines/probe_basic \
             first in [RS274NGC] SUBROUTINE_PATH",
            ini.path.display(),
            found.display()
        );
        // A physical machine must run exactly the reviewed bundle. A simulation
        // profile may keep its own copy for a live test rig, so report it there
        // without refusing to start.
        return if config.machine.kind == "real" {
            Err(ConfigError::Invalid(message))
        } else {
            log::warn!("{message}");
            Ok(())
        };
    }
    Ok(())
}

/// Keep validation messages readable when a search path provides nothing.
fn summarize(mut names: Vec<String>) -> String {
    const SHOWN: usize = 6;
    names.sort();
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    let extra = names.len() - SHOWN;
    names.truncate(SHOWN);
    format!("{} (and {extra} more)", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    fn base_config() -> &'static str {
        include_str!("../../../configs/base.toml")
    }

    fn make_fixture(dir: &Path, changer_p: bool) {
        let columns = if changer_p { "TZPDR" } else { "TZDR" };
        write(
            &dir.join("probe_basic.ini"),
            &format!(
                "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = {columns}\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -10\nMAX_LIMIT = 10\n",
            ),
        );
        std::fs::create_dir_all(dir.join("gcode")).unwrap();
        write(
            &dir.join("tool.tbl"),
            "; tool table\nT1 P1 Z0 D6 ; end mill\n",
        );
    }

    #[test]
    fn base_profile_cannot_be_loaded() {
        let dir = temp_dir();
        let base = dir.path().join("base.toml");
        write(&base, base_config());
        let error = Profile::load(&base, None).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Toml { .. } | ConfigError::Invalid(_)
        ));
    }

    #[test]
    fn merges_base_and_resolves_paths() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
label = "Sim panel"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
panel_window = true
panel_title = "Sim"
panel_scale = 1.0
"#,
        );
        let profile = Profile::load(&profile, None).unwrap();
        assert_eq!(profile.config.profile, "sim");
        assert_eq!(profile.variant, MachineVariant::Mill);
        assert_eq!(profile.machine_ini.coordinates, ["X", "Y", "Z"]);
        assert_eq!(profile.config.tools.changer, "manual");
        assert!(profile.is_sim());
    }

    #[test]
    fn the_files_parent_row_is_off_by_default_and_a_profile_can_enable_it() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile_path = dir.path().join("sim.toml");
        let profile_text = |files: &str| {
            format!(
                r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[files]
{files}
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
"#
            )
        };

        // base.toml carries the key, and it is off.
        write(&profile_path, &profile_text(""));
        let profile = Profile::load(&profile_path, None).unwrap();
        assert!(!profile.config.files.show_parent_above_root);

        // A profile turns the .. row above PROGRAM_PREFIX on.
        write(
            &profile_path,
            &profile_text("show_parent_above_root = true"),
        );
        let profile = Profile::load(&profile_path, None).unwrap();
        assert!(profile.config.files.show_parent_above_root);
    }

    #[test]
    fn turret_tab_is_off_by_default_and_a_profile_can_enable_it() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile_path = dir.path().join("sim.toml");
        let profile_text = |diagnostics: &str| {
            format!(
                r#"schema_version = 1
profile = "sim"
extends = "base.toml"
{diagnostics}
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
"#
            )
        };

        let omitted: DiagnosticsConfig =
            toml::from_str("event_log_capacity = 1000\npanel_link_stats = \"optional\"").unwrap();
        assert!(
            !omitted.turret_tab,
            "turret_tab without the key is {}",
            omitted.turret_tab
        );

        write(&profile_path, &profile_text(""));
        let profile = Profile::load(&profile_path, None).unwrap();
        assert!(
            !profile.config.diagnostics.turret_tab,
            "turret_tab resolved to {}",
            profile.config.diagnostics.turret_tab
        );

        write(
            &profile_path,
            &profile_text("[diagnostics]\nturret_tab = true"),
        );
        let profile = Profile::load(&profile_path, None).unwrap();
        assert!(
            profile.config.diagnostics.turret_tab,
            "turret_tab resolved to {}",
            profile.config.diagnostics.turret_tab
        );
    }

    #[test]
    fn manual_changer_accepts_a_p_column() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), true);
        let profile = dir.path().join("sim.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
label = "Sim panel"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
"#,
        );
        // The column is hidden in the UI but belongs in the file: LinuxCNC
        // keeps a tool per P value and loads only the last one without it.
        let loaded = Profile::load(&profile, None).expect("manual changer accepts P");
        assert!(loaded.machine_ini.tool_table_columns.contains('P'));
    }

    #[test]
    fn ui_and_panel_scale_are_validated() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        let profile_text = |ui: &str, panel: &str| {
            format!(
                r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[ui]
scale = "{ui}"
[panel]
kind = "simulated"
label = "Sim panel"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
{panel}
"#
            )
        };
        write(&profile, &profile_text("4", ""));
        let error = Profile::load(&profile, None).unwrap_err();
        assert!(error.to_string().contains("ui.scale"), "{error}");
        write(&profile, &profile_text("auto", "panel_scale = 4.0"));
        let error = Profile::load(&profile, None).unwrap_err();
        assert!(
            error.to_string().contains("simulation.panel_scale"),
            "{error}"
        );
    }

    #[test]
    fn ui_scale_accepts_auto_factors_and_percent() {
        let mut ui: UiConfig = toml::from_str(
            r#"theme = "graphite-blue"
start_page = "machine"
window_title = "LiNC"
scale = "auto"
axis_order = ["X", "Y", "Z"]
pages = ["machine"]
"#,
        )
        .unwrap();
        assert_eq!(ui.zoom_factor(), None);
        ui.scale = "0.8".into();
        assert!((ui.zoom_factor().unwrap() - 0.8).abs() < f32::EPSILON);
        ui.scale = "80%".into();
        assert!((ui.zoom_factor().unwrap() - 0.8).abs() < f32::EPSILON);
        ui.scale = "1.25x".into();
        assert!((ui.zoom_factor().unwrap() - 1.25).abs() < f32::EPSILON);
    }

    #[test]
    fn servo_load_names_must_exist_on_the_machine() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        let load = |servo: &str| {
            write(
                &profile,
                &format!(
                    r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
[ui]
servo_load = {servo}
"#
                ),
            );
            Profile::load(&profile, None)
        };
        let loaded = load(r#"["X", "S"]"#).unwrap();
        assert_eq!(loaded.config.ui.servo_load, ["X", "S"]);
        let empty = load("[]").unwrap();
        assert!(empty.config.ui.servo_load.is_empty());
        let error = load(r#"["Q"]"#).unwrap_err();
        assert!(format!("{error}").contains("ui.servo_load"), "{error}");
        let error = load(r#"["X", "X"]"#).unwrap_err();
        assert!(format!("{error}").contains("more than once"), "{error}");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
typo_key = 3
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
"#,
        );
        assert!(matches!(
            Profile::load(&profile, None),
            Err(ConfigError::Toml { .. } | ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn weaker_safety_policy_is_rejected() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[safety]
software_jog = true
"#,
        );
        let error = Profile::load(&profile, None).unwrap_err();
        assert!(error.to_string().contains("software_jog"));
    }

    #[test]
    fn atc_requires_standard_pocket_column() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("machine.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "machine"
extends = "base.toml"
[machine]
name = "Test"
kind = "real"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[tools]
changer = "atc"
"#,
        );
        let error = Profile::load(&profile, None).unwrap_err();
        assert!(error.to_string().contains("P column"));
    }

    #[test]
    fn profile_arrays_replace_base_arrays() {
        let mut base = toml::Value::Table(Default::default());
        let mut overlay = toml::Value::Table(Default::default());
        if let (toml::Value::Table(b), toml::Value::Table(o)) = (&mut base, &mut overlay) {
            b.insert(
                "arr".into(),
                toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]),
            );
            o.insert(
                "arr".into(),
                toml::Value::Array(vec![toml::Value::Integer(9)]),
            );
        }
        deep_merge(&mut base, overlay);
        assert_eq!(
            base.get("arr")
                .and_then(toml::Value::as_array)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn mdi_and_keys_defaults_parse() {
        let root = repo_root();
        let profile = Profile::load(&root.join("configs/sim.toml"), None).unwrap();
        let mdi = &profile.config.mdi;
        assert!(mdi.execute_on_enter);
        assert!(mdi.auto_focus);
        assert_eq!(mdi.history_limit, 50);
        assert_eq!(mdi.max_command_length, 256);
        assert_eq!(
            mdi.save_preamble,
            ["G17", "G21", "G40", "G49", "G80", "G90"]
        );
        assert_eq!(mdi.save_postamble, ["M5", "M9", "M30"]);
        let keys = &profile.config.keys;
        assert_eq!(keys.mdi_issue, "Enter");
        assert_eq!(keys.mdi_history_up, "Up");
        assert_eq!(keys.mdi_buffer_delete, "Ctrl+Backspace");
        assert_eq!(
            KeysConfig::spec_default().mdi_buffer_delete,
            keys.mdi_buffer_delete
        );
        // The lathe display toggles ship with a chord each, and the profile
        // carries the same text the spec default does.
        assert_eq!(keys.toggle_lathe_mirror, "Ctrl+J");
        assert_eq!(keys.toggle_lathe_diameter, "Ctrl+E");
        assert_eq!(
            KeysConfig::spec_default().toggle_lathe_mirror,
            keys.toggle_lathe_mirror
        );
        assert_eq!(
            KeysConfig::spec_default().toggle_lathe_diameter,
            keys.toggle_lathe_diameter
        );
    }

    #[test]
    fn shipped_profiles_drop_quick_commands_and_keep_a_preamble() {
        let base = include_str!("../../../configs/base.toml");
        let lathe = include_str!("../../../configs/lathe-sim.toml");
        for text in [base, lathe] {
            assert!(!text.contains("quick_commands"));
        }
        assert!(base.contains("save_preamble = [\"G17\""));
        // A lathe writes arcs in XZ: the XY plane word would put them elsewhere.
        assert!(lathe.contains("save_preamble = [\"G18\""));
    }

    #[test]
    fn a_profile_that_sets_quick_commands_no_longer_parses() {
        let dir = temp_dir();
        write(&dir.path().join("base.toml"), base_config());
        make_fixture(dir.path(), false);
        let profile = dir.path().join("sim.toml");
        write(
            &profile,
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
label = "Sim panel"
driver = "cnc-panel-sim"
[simulation]
enabled = true
[mdi]
quick_commands = ["G0"]
"#,
        );
        let error = Profile::load(&profile, None).unwrap_err();
        assert!(error.to_string().contains("quick_commands"), "{error}");
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn required_routine_files() -> Vec<String> {
        required_probe_routines()
            .into_iter()
            .map(|name| format!("{name}.ngc"))
            .collect()
    }

    fn write_bundle(dir: &Path, contents: &dyn Fn(&str) -> String) {
        std::fs::create_dir_all(dir).unwrap();
        for file in required_routine_files() {
            let name = file.trim_end_matches(".ngc").to_string();
            write(&dir.join(&file), &contents(&name));
        }
    }

    /// A minimal machine INI plus profile. `extra` is appended after the axis
    /// section, so it must be a complete section such as `[RS274NGC]`.
    fn probe_profile(dir: &Path, kind: &str, extra: &str) -> PathBuf {
        write(&dir.join("base.toml"), base_config());
        write(
            &dir.join("probe_basic.ini"),
            &format!(
                "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = TZDR\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -10\nMAX_LIMIT = 10\n{extra}"
            ),
        );
        std::fs::create_dir_all(dir.join("gcode")).unwrap();
        write(
            &dir.join("tool.tbl"),
            "; tool table\nT1 P1 Z0 D6 ; end mill\n",
        );
        let simulation = if kind == "sim" {
            "[simulation]\nenabled = true\npanel_window = false\npanel_scale = 1.0\n"
        } else {
            ""
        };
        let profile = dir.join("profile.toml");
        write(
            &profile,
            &format!(
                r#"schema_version = 1
profile = "probe"
extends = "base.toml"
[machine]
name = "Test"
kind = "{kind}"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
label = "Sim panel"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
{simulation}"#
            ),
        );
        profile
    }

    #[test]
    fn shipped_profiles_resolve_the_reviewed_probe_bundle() {
        let root = repo_root();
        for profile in ["configs/sim.toml", "configs/baiermill-pb.toml"] {
            Profile::load(&root.join(profile), None)
                .unwrap_or_else(|error| panic!("{profile} must validate: {error}"));
        }
    }

    #[test]
    fn subroutine_path_without_the_context_helper_is_rejected() {
        let dir = temp_dir();
        let root = dir.path();
        write_bundle(&root.join("subroutines"), &|name| format!("({name})\n"));
        std::fs::remove_file(root.join("subroutines/linc_probe_context.ngc")).unwrap();
        let profile = probe_profile(root, "sim", "\n[RS274NGC]\nSUBROUTINE_PATH = subroutines\n");
        let message = Profile::load(&profile, None).unwrap_err().to_string();
        assert!(message.contains("SUBROUTINE_PATH"), "{message}");
        assert!(message.contains("linc_probe_context.ngc"), "{message}");
    }

    #[test]
    fn machine_copy_of_the_bundle_is_rejected_as_incompatible() {
        let dir = temp_dir();
        let root = dir.path();
        // LiNC's reviewed copy: the nearest ancestor of the profile directory.
        write_bundle(&root.join("subroutines/probe_basic"), &|name| {
            format!("({name})\n")
        });
        // The machine's own copy: same names, one routine differs.
        write_bundle(&root.join("proj/subroutines"), &|name| {
            if name == "probe_top_front_edge_angle" {
                "(older Probe Basic copy)\n".to_string()
            } else {
                format!("({name})\n")
            }
        });
        let profile = probe_profile(
            &root.join("proj"),
            "real",
            "\n[RS274NGC]\nSUBROUTINE_PATH = subroutines\n",
        );
        let message = Profile::load(&profile, None).unwrap_err().to_string();
        assert!(
            message.contains("differs from LiNC's reviewed copy"),
            "{message}"
        );
        assert!(
            message.contains("probe_top_front_edge_angle.ngc"),
            "{message}"
        );
    }

    #[test]
    fn simulation_copy_of_the_bundle_only_reports_a_difference() {
        let dir = temp_dir();
        let root = dir.path();
        write_bundle(&root.join("subroutines/probe_basic"), &|name| {
            format!("({name})\n")
        });
        // A live test rig may carry its own copy; it must still start.
        write_bundle(&root.join("proj/subroutines"), &|_| {
            "(fixture copy)\n".to_string()
        });
        let profile = probe_profile(
            &root.join("proj"),
            "sim",
            "\n[RS274NGC]\nSUBROUTINE_PATH = subroutines\n",
        );
        let loaded = Profile::load(&profile, None);
        assert!(
            loaded.is_ok(),
            "a simulation copy must not fail validation: {:?}",
            loaded.err()
        );
    }

    #[test]
    fn physical_profile_without_a_subroutine_path_is_rejected() {
        let dir = temp_dir();
        let profile = probe_profile(dir.path(), "real", "");
        let message = Profile::load(&profile, None).unwrap_err().to_string();
        assert!(message.contains("SUBROUTINE_PATH"), "{message}");
        assert!(message.contains("linc_probe_context.ngc"), "{message}");
    }

    #[test]
    fn bundle_validation_requires_every_routine_a_generated_run_calls() {
        let required = required_probe_routines();
        let parameters = crate::probe::ProbeParameters::default();
        let mut calls = 0;
        for operation in crate::probe::operations() {
            for line in parameters.mdi_text(operation).lines() {
                let Some(name) = line
                    .trim()
                    .strip_prefix("o<")
                    .and_then(|rest| rest.split('>').next())
                else {
                    continue;
                };
                calls += 1;
                assert!(
                    required.iter().any(|routine| routine == name),
                    "the generated Run for {} calls o<{name}>, which bundle validation ignores",
                    operation.id
                );
            }
        }
        assert_eq!(calls, crate::probe::operations().len() * 3);
    }
}
