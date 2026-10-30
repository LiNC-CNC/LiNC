//! Minimal LinuxCNC INI reader used for machine data.
//!
//! The INI remains authoritative for geometry, travel, units, homing, spindle
//! count, ATC pockets, the tool table and the program prefix. Nothing is copied
//! into the LiNC profile.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IniError {
    #[error("cannot read INI {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("INI {0} has no [TRAJ] COORDINATES")]
    MissingCoordinates(PathBuf),
    #[error("INI {0} has no [EMCIO] TOOL_TABLE")]
    MissingToolTable(PathBuf),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IniSection {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

impl IniSection {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(&key.to_ascii_uppercase())
            .map(String::as_str)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.trim().parse().ok()
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        let value = self.get(key)?.trim().to_ascii_lowercase();
        match value.as_str() {
            "1" | "yes" | "true" | "on" => Some(true),
            "0" | "no" | "false" | "off" => Some(false),
            _ => None,
        }
    }

    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key)?.trim().parse().ok()
    }
}

#[derive(Clone, Debug, Default)]
pub struct INI {
    pub sections: BTreeMap<String, IniSection>,
}

impl INI {
    pub fn parse(text: &str) -> Self {
        let mut sections: BTreeMap<String, IniSection> = BTreeMap::new();
        let mut current = String::new();
        for raw_line in text.lines() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim().to_ascii_uppercase();
                sections.entry(name.clone()).or_insert_with(|| IniSection {
                    name: name.clone(),
                    values: BTreeMap::new(),
                });
                current = name;
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let section = sections
                    .entry(current.clone())
                    .or_insert_with(|| IniSection {
                        name: current.clone(),
                        values: BTreeMap::new(),
                    });
                section
                    .values
                    .insert(key.trim().to_ascii_uppercase(), value.trim().to_string());
            }
        }
        Self { sections }
    }

    pub fn section(&self, name: &str) -> Option<&IniSection> {
        self.sections.get(&name.to_ascii_uppercase())
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.section(section)?.get(key)
    }

    pub fn get_f64(&self, section: &str, key: &str) -> Option<f64> {
        self.section(section)?.get_f64(key)
    }

    pub fn get_u32(&self, section: &str, key: &str) -> Option<u32> {
        self.section(section)?.get_u32(key)
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '#' | ';' if !in_quote => return &line[..index],
            _ => {}
        }
    }
    line
}

fn resolve_ini_path(base: &Path, value: &str) -> PathBuf {
    let expanded = expand_tilde(value.trim());
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

pub fn expand_tilde(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if value == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(value)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxisIni {
    pub name: String,
    pub min_limit: f64,
    pub max_limit: f64,
    pub max_velocity: f64,
    pub max_acceleration: f64,
    pub home_sequence: u32,
    pub home_search_vel: f64,
    pub home_latch_vel: f64,
    pub home_offset: f64,
    pub home_use_index: bool,
    pub home_ignore_limits: bool,
    pub home_is_shared: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MachineIni {
    pub path: PathBuf,
    pub config_dir: PathBuf,
    pub machine_name: String,
    pub coordinates: Vec<String>,
    pub joints: usize,
    pub kinematics: String,
    pub linear_units: String,
    pub angular_units: String,
    pub axes: Vec<AxisIni>,
    pub spindles: usize,
    pub max_spindle_speed: f64,
    pub min_spindle_override: f64,
    pub max_spindle_override: f64,
    pub max_feed_override: f64,
    pub tool_table: PathBuf,
    pub tool_table_columns: String,
    /// `[RS274NGC] PARAMETER_FILE`, resolved against the INI directory. The
    /// interpreter owns this file; the HMI only reads and updates the work
    /// offset rows so a session cut short does not lose them.
    pub parameter_file: Option<PathBuf>,
    pub program_prefix: PathBuf,
    pub open_file: Option<PathBuf>,
    pub atc_pockets: Option<u16>,
    pub subroutine_paths: Vec<PathBuf>,
    pub no_force_homing: bool,
    pub increments: Vec<(String, f64)>,
    pub default_linear_velocity: f64,
    pub max_linear_velocity: f64,
}

impl MachineIni {
    pub fn parse_file(path: &Path) -> Result<Self, IniError> {
        let text = std::fs::read_to_string(path).map_err(|source| IniError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &text)
    }

    pub fn parse(path: &Path, text: &str) -> Result<Self, IniError> {
        let ini = INI::parse(text);
        let config_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let coordinates_raw = ini
            .get("TRAJ", "COORDINATES")
            .map(str::to_string)
            .or_else(|| {
                ini.get("KINS", "KINEMATICS")
                    .and_then(|kin| kin.split("coordinates=").nth(1))
                    .map(|value| {
                        value
                            .split_whitespace()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .map(|ch| ch.to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
            })
            .ok_or_else(|| IniError::MissingCoordinates(path.to_path_buf()))?;
        let coordinates: Vec<String> = coordinates_raw
            .split_whitespace()
            .flat_map(|part| {
                if part.len() > 1 {
                    part.chars().map(|ch| ch.to_string()).collect::<Vec<_>>()
                } else {
                    vec![part.to_string()]
                }
            })
            .map(|axis| axis.to_ascii_uppercase())
            .collect();

        let joints = ini
            .get_u32("KINS", "JOINTS")
            .map(|value| value as usize)
            .unwrap_or(coordinates.len());

        let kin = ini
            .get("KINS", "KINEMATICS")
            .unwrap_or("trivkins")
            .to_string();

        let mut axes = Vec::new();
        for (index, name) in coordinates.iter().enumerate() {
            let axis_section = ini.section(&format!("AXIS_{name}"));
            let joint_section = ini.section(&format!("JOINT_{index}"));
            let get = |key: &str| -> Option<f64> {
                axis_section
                    .and_then(|section| section.get_f64(key))
                    .or_else(|| joint_section.and_then(|section| section.get_f64(key)))
            };
            let get_bool = |key: &str| -> Option<bool> {
                axis_section
                    .and_then(|section| section.get_bool(key))
                    .or_else(|| joint_section.and_then(|section| section.get_bool(key)))
            };
            let home_sequence = joint_section
                .and_then(|section| section.get_u32("HOME_SEQUENCE"))
                .or_else(|| axis_section.and_then(|section| section.get_u32("HOME_SEQUENCE")))
                .unwrap_or(0);
            axes.push(AxisIni {
                name: name.clone(),
                min_limit: get("MIN_LIMIT").unwrap_or(0.0),
                max_limit: get("MAX_LIMIT").unwrap_or(0.0),
                max_velocity: get("MAX_VELOCITY").unwrap_or(0.0),
                max_acceleration: get("MAX_ACCELERATION").unwrap_or(0.0),
                home_sequence,
                home_search_vel: get("HOME_SEARCH_VEL").unwrap_or(0.0),
                home_latch_vel: get("HOME_LATCH_VEL").unwrap_or(0.0),
                home_offset: get("HOME_OFFSET").unwrap_or(0.0),
                home_use_index: get_bool("HOME_USE_INDEX").unwrap_or(false),
                home_ignore_limits: get_bool("HOME_IGNORE_LIMITS").unwrap_or(false),
                home_is_shared: get_bool("HOME_IS_SHARED").unwrap_or(false),
            });
        }

        let tool_table_value = ini
            .get("EMCIO", "TOOL_TABLE")
            .ok_or_else(|| IniError::MissingToolTable(path.to_path_buf()))?;
        let tool_table = resolve_ini_path(&config_dir, tool_table_value);
        let tool_table_columns = ini
            .get("DISPLAY", "TOOL_TABLE_COLUMNS")
            .or_else(|| ini.get("EMCIO", "TOOL_TABLE_COLUMNS"))
            .unwrap_or("TZDR")
            .trim()
            .to_ascii_uppercase();
        let parameter_file = ini
            .get("RS274NGC", "PARAMETER_FILE")
            .filter(|value| !value.trim().is_empty())
            .map(|value| resolve_ini_path(&config_dir, value));

        let program_prefix_value = ini.get("DISPLAY", "PROGRAM_PREFIX").unwrap_or(".");
        let program_prefix = resolve_ini_path(&config_dir, program_prefix_value);

        let open_file = ini.get("DISPLAY", "OPEN_FILE").map(|value| {
            let path = expand_tilde(value);
            if path.is_absolute() {
                path
            } else {
                program_prefix.join(path)
            }
        });

        let subroutine_paths = ini
            .get("RS274NGC", "SUBROUTINE_PATH")
            .map(|value| {
                value
                    .split(':')
                    .filter(|part| !part.trim().is_empty())
                    .map(|part| resolve_ini_path(&config_dir, part))
                    .collect()
            })
            .unwrap_or_default();

        let increments = ini
            .get("DISPLAY", "INCREMENTS")
            .map(|value| {
                value
                    .split_whitespace()
                    // Keep the whole token as the label ("10mm", ".01in") so a
                    // selector can show exactly what the INI declares. The
                    // first entry is conventionally `JOG`, which selects
                    // continuous jog and therefore carries 0.0.
                    .map(|token| {
                        let numeric = token
                            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                            .parse::<f64>()
                            .unwrap_or(0.0);
                        (token.to_string(), numeric)
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            path: path.to_path_buf(),
            config_dir,
            machine_name: ini.get("EMC", "MACHINE").unwrap_or("LinuxCNC").to_string(),
            coordinates,
            joints,
            kinematics: kin,
            linear_units: ini
                .get("TRAJ", "LINEAR_UNITS")
                .unwrap_or("mm")
                .to_ascii_lowercase(),
            angular_units: ini
                .get("TRAJ", "ANGULAR_UNITS")
                .unwrap_or("degree")
                .to_ascii_lowercase(),
            axes,
            spindles: ini.get_u32("TRAJ", "SPINDLES").unwrap_or(1).max(1) as usize,
            max_spindle_speed: ini.get_f64("DISPLAY", "MAX_SPINDLE_SPEED").unwrap_or(0.0),
            min_spindle_override: ini
                .get_f64("DISPLAY", "MIN_SPINDLE_OVERRIDE")
                .unwrap_or(0.5),
            max_spindle_override: ini
                .get_f64("DISPLAY", "MAX_SPINDLE_OVERRIDE")
                .unwrap_or(2.0),
            max_feed_override: ini.get_f64("DISPLAY", "MAX_FEED_OVERRIDE").unwrap_or(2.0),
            tool_table,
            tool_table_columns,
            parameter_file,
            program_prefix,
            open_file,
            atc_pockets: ini.get_u32("ATC", "POCKETS").map(|value| value as u16),
            subroutine_paths,
            no_force_homing: ini
                .get("TRAJ", "NO_FORCE_HOMING")
                .map(|value| value.trim() == "0")
                .unwrap_or(false),
            increments,
            default_linear_velocity: ini
                .get_f64("TRAJ", "DEFAULT_LINEAR_VELOCITY")
                .unwrap_or(0.0),
            max_linear_velocity: ini.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(0.0),
        })
    }

    pub fn is_lathe(&self) -> bool {
        self.coordinates.iter().any(|axis| axis == "Z")
            && !self.coordinates.iter().any(|axis| axis == "Y")
            && self.coordinates.len() <= 3
            || self
                .kinematics
                .to_ascii_lowercase()
                .contains("coordinates=xz")
    }

    pub fn axis_names(&self) -> Vec<String> {
        self.axes.iter().map(|axis| axis.name.clone()).collect()
    }

    pub fn axis(&self, name: &str) -> Option<&AxisIni> {
        self.axes.iter().find(|axis| axis.name == name)
    }

    pub fn home_order(&self) -> Vec<AxisIni> {
        let mut axes = self.axes.clone();
        axes.sort_by_key(|axis| axis.home_sequence);
        axes
    }

    pub fn limits_for(&self, name: &str) -> Option<(f64, f64)> {
        self.axis(name).map(|axis| (axis.min_limit, axis.max_limit))
    }

    pub fn validate_axis_value(&self, name: &str, value: f64) -> Result<(), String> {
        if !value.is_finite() {
            return Err(format!("{name} value is not finite"));
        }
        if let Some(axis) = self.axis(name) {
            if (axis.min_limit != 0.0 || axis.max_limit != 0.0)
                && (value < axis.min_limit || value > axis.max_limit)
            {
                return Err(format!(
                    "{name} {value:.3} outside travel {:.3}…{:.3}",
                    axis.min_limit, axis.max_limit
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIM: &str = r#"
[DISPLAY]
TOOL_TABLE_COLUMNS = TZDR
PROGRAM_PREFIX = gcode
OPEN_FILE = gcode/blank.ngc
INCREMENTS = JOG 10mm 1mm .1mm .01mm

[TRAJ]
COORDINATES = XYZ
LINEAR_UNITS = mm
SPINDLES = 1

[KINS]
JOINTS = 3
KINEMATICS = trivkins coordinates=XYZ

[RS274NGC]
PARAMETER_FILE = sim.var

[EMCIO]
TOOL_TABLE = tool.tbl

[ATC]
POCKETS = 12

[AXIS_X]
MIN_LIMIT = -372
MAX_LIMIT = 5
MAX_VELOCITY = 80

[JOINT_0]
HOME_SEQUENCE = 2
HOME_SEARCH_VEL = 30
HOME_LATCH_VEL = 5
HOME_USE_INDEX = YES
"#;

    #[test]
    fn parses_authoritative_machine_data() {
        let ini = MachineIni::parse(Path::new("/tmp/machine/probe_basic.ini"), SIM).unwrap();
        assert_eq!(ini.coordinates, ["X", "Y", "Z"]);
        assert_eq!(ini.joints, 3);
        assert_eq!(ini.spindles, 1);
        assert_eq!(ini.axes[0].name, "X");
        assert_eq!(ini.axes[0].min_limit, -372.0);
        assert_eq!(ini.axes[0].max_limit, 5.0);
        assert_eq!(ini.axes[0].home_sequence, 2);
        assert!(ini.axes[0].home_use_index);
        assert_eq!(ini.tool_table_columns, "TZDR");
        assert_eq!(ini.atc_pockets, Some(12));
        assert!(ini.tool_table.ends_with("tool.tbl"));
        assert_eq!(
            ini.parameter_file.as_deref(),
            Some(Path::new("/tmp/machine/sim.var"))
        );
        // `JOG` is kept as the continuous entry, and each label is the whole
        // token so a selector can show what the INI declares.
        assert_eq!(
            ini.increments,
            vec![
                ("JOG".to_string(), 0.0),
                ("10mm".to_string(), 10.0),
                ("1mm".to_string(), 1.0),
                (".1mm".to_string(), 0.1),
                (".01mm".to_string(), 0.01),
            ]
        );
    }

    #[test]
    fn strips_comments_and_handles_bool() {
        let ini = INI::parse("[A]\nx = 1 # comment\ny = YES ; ok\n");
        assert_eq!(ini.get("A", "x"), Some("1"));
        assert_eq!(ini.get("A", "x"), Some("1"));
        assert_eq!(ini.section("A").unwrap().get_bool("y"), Some(true));
    }
}
