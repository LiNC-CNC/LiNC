//! Simulator configuration: configs/sim3d.toml.
//!
//! Every path in the file resolves against the config file's own directory, so
//! configs/sim3d.toml can name "sim.ini" and "../parts/vise.step" and mean
//! configs/sim.ini and parts/vise.step.
//!
//! Missing sections and missing keys fall back to the defaults below, which is
//! what the UI relies on when it rewrites a config that only carries a
//! workpiece list.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The config path the simulator uses when the command line names none.
pub const DEFAULT_PATH: &str = "configs/sim3d.toml";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read simulator config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse simulator config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("cannot serialize simulator config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("cannot write simulator config {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The whole simulator config, as stored in TOML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sim3dConfig {
    pub machine: MachineConfig,
    pub probe: ProbeConfig,
    pub tools: ToolsConfig,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cutters: Vec<CutterConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workpieces: Vec<WorkpieceEntry>,
    pub hal: HalConfig,
    /// Directory the config was loaded from; relative paths resolve against it.
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// The file the config was loaded from, kept so a reload can reread it.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

impl Default for Sim3dConfig {
    fn default() -> Self {
        Self {
            machine: MachineConfig::default(),
            probe: ProbeConfig::default(),
            tools: ToolsConfig::default(),
            cutters: Vec::new(),
            workpieces: Vec::new(),
            hal: HalConfig::default(),
            base_dir: PathBuf::new(),
            source_path: None,
        }
    }
}

impl Sim3dConfig {
    /// "configs/sim3d.toml", relative to the working directory.
    pub fn default_path() -> PathBuf {
        PathBuf::from(DEFAULT_PATH)
    }

    /// Read and parse a config file; relative paths inside it resolve against
    /// its directory.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// Parse config text as if it lived at the given path.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.base_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        config.source_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Resolve a path from the config file against the config's directory.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base_dir.join(path)
        }
    }

    /// The machine INI, resolved.
    pub fn machine_ini_path(&self) -> PathBuf {
        self.resolve(&self.machine.ini)
    }

    /// A workpiece's STEP file, resolved.
    pub fn workpiece_path(&self, entry: &WorkpieceEntry) -> PathBuf {
        self.resolve(&entry.path)
    }

    /// Render the config as TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Write the config, creating the directory if needed.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        fs::write(path, text).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// The machine section: which machine this simulator stands in for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    /// LinuxCNC INI, relative to the config file.
    pub ini: PathBuf,
    /// How far the body reaches outside the travel, in mm.
    pub margin_mm: f64,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            ini: PathBuf::from("sim.ini"),
            margin_mm: 40.0,
        }
    }
}

/// The probe section: the touch probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeConfig {
    /// The tool number that holds the probe.
    pub tool_number: u16,
    /// Ball radius in mm; None reads D/2 from the tool table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_radius_mm: Option<f64>,
    /// Stylus length above the ball, in mm.
    pub stylus_length_mm: f64,
    /// Tool offset used when the tool row's Z is zero or missing, in mm.
    pub tip_offset_mm: f64,
    /// Probe triggers this far before the ball actually touches, in mm.
    pub trigger_slack_mm: f64,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            tool_number: 99,
            tip_radius_mm: None,
            stylus_length_mm: 30.0,
            tip_offset_mm: -30.0,
            trigger_slack_mm: 0.0,
        }
    }
}

/// The tools section: geometry shared by the cutters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Tool offset used when a row's Z is zero or missing, in mm.
    pub fallback_tip_offset_mm: f64,
    /// Flute length below the tip that the capsule covers, in mm.
    pub flute_length_mm: f64,
    /// Probe stem radius; None follows the ball radius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shank_radius_mm: Option<f64>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            fallback_tip_offset_mm: -30.0,
            flute_length_mm: 20.0,
            shank_radius_mm: None,
        }
    }
}

/// One cutter override row: a profile override for a tool number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutterConfig {
    pub tool: u16,
    /// Shape profile; flat when the row names no profile.
    #[serde(default)]
    pub kind: CutterKind,
    /// Bull nose corner radius in mm; defaults to a quarter of the diameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner_radius_mm: Option<f64>,
}

/// The cutter profiles the simulator knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CutterKind {
    #[default]
    Flat,
    Ball,
    Bull,
    Drill,
    Chamfer,
}

impl CutterKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Ball => "ball",
            Self::Bull => "bull",
            Self::Drill => "drill",
            Self::Chamfer => "chamfer",
        }
    }
}

/// One workpiece row: a STEP file and where it sits on the machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpieceEntry {
    /// Display name; the file stem when empty.
    #[serde(default)]
    pub name: String,
    /// STEP file, relative to the config file.
    pub path: PathBuf,
    /// Placement in machine coordinates, mm.
    #[serde(default)]
    pub translate_mm: [f64; 3],
    /// Rotation about X, then Y, then Z, in degrees.
    #[serde(default)]
    pub rotate_deg: [f64; 3],
    /// Draw the part and test contact against it.
    #[serde(default = "default_true")]
    pub visible: bool,
}

impl WorkpieceEntry {
    /// The placement as a Transform.
    pub fn transform(&self) -> crate::types::Transform {
        crate::types::Transform {
            translation: self.translate_mm,
            rotation_deg: self.rotate_deg,
        }
    }
}

fn default_true() -> bool {
    true
}

/// The HAL section: the component task C publishes on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HalConfig {
    pub component: String,
    pub sample_ms: f64,
}

impl Default for HalConfig {
    fn default() -> Self {
        Self {
            component: "linc-sim3d".to_string(),
            sample_ms: 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_contract() {
        let config = Sim3dConfig::default();
        assert_eq!(config.machine.ini, PathBuf::from("sim.ini"));
        assert_eq!(config.probe.tool_number, 99);
        assert_eq!(config.probe.tip_radius_mm, None);
        assert_eq!(config.probe.stylus_length_mm, 30.0);
        assert_eq!(config.probe.tip_offset_mm, -30.0);
        assert_eq!(config.probe.trigger_slack_mm, 0.0);
        assert_eq!(config.tools.fallback_tip_offset_mm, -30.0);
        assert_eq!(config.tools.flute_length_mm, 20.0);
        assert!(config.cutters.is_empty());
        assert!(config.workpieces.is_empty());
        assert_eq!(config.hal.component, "linc-sim3d");
        assert_eq!(config.hal.sample_ms, 2.0);
        assert_eq!(Sim3dConfig::default_path(), PathBuf::from(DEFAULT_PATH));
    }

    #[test]
    fn relative_paths_resolve_against_the_config_directory() {
        let text = r#"
[probe]
tool_number = 42

[[workpieces]]
path = "../parts/block.step"
translate_mm = [1, 2, 3]
"#;
        let config = Sim3dConfig::parse(text, Path::new("/etc/linc/sim3d.toml")).unwrap();
        assert_eq!(config.probe.tool_number, 42);
        assert_eq!(config.machine.ini, PathBuf::from("sim.ini"));
        assert_eq!(
            config.machine_ini_path(),
            PathBuf::from("/etc/linc/sim.ini")
        );
        assert_eq!(
            config.workpiece_path(&config.workpieces[0]),
            PathBuf::from("/etc/linc/../parts/block.step")
        );
        assert_eq!(config.workpieces[0].translate_mm, [1.0, 2.0, 3.0]);
        assert!(config.workpieces[0].visible, "visible defaults to true");
    }

    #[test]
    fn missing_file_names_the_path() {
        let error = Sim3dConfig::load(Path::new("/nonexistent/sim3d.toml")).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("/nonexistent/sim3d.toml"),
            "unhelpful error: {message}"
        );
    }

    #[test]
    fn toml_round_trip_keeps_every_section() {
        let text = r#"
[machine]
ini = "mill.ini"
margin_mm = 25.0

[probe]
tool_number = 7
tip_radius_mm = 2.5
stylus_length_mm = 40.0
tip_offset_mm = -40.0
trigger_slack_mm = 0.02

[tools]
fallback_tip_offset_mm = -25.0
flute_length_mm = 12.0
shank_radius_mm = 3.0

[[cutters]]
tool = 2
kind = "ball"
corner_radius_mm = 1.5

[[workpieces]]
name = "block"
path = "block.step"
translate_mm = [1, 2, 3]
rotate_deg = [4, 5, 6]
visible = false

[hal]
component = "sim"
sample_ms = 1.0
"#;
        let config = Sim3dConfig::parse(text, Path::new("configs/sim3d.toml")).unwrap();
        let rendered = config.to_toml().unwrap();
        let reparsed = Sim3dConfig::parse(&rendered, Path::new("configs/sim3d.toml")).unwrap();
        assert_eq!(config, reparsed);
        assert_eq!(reparsed.cutters[0].kind, CutterKind::Ball);
        assert_eq!(reparsed.workpieces[0].name, "block");
        assert!(!reparsed.workpieces[0].visible);
    }
}
