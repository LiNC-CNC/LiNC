//! The interpreter's numbered-parameter file.
//!
//! LinuxCNC writes this file at interpreter synchronization points and exit,
//! not on every numbered-parameter assignment. It is therefore not a live
//! Probe Basic result API. LiNC also mirrors its G54–G59.3 writes promptly so
//! an unclean shutdown cannot discard an offset acknowledged by the HMI.
//!
//! The parameter numbers are fixed by LinuxCNC:
//!
//! - `5200 + index * 20 + 1 .. +9`: work offsets for G54..G59.3 (`index 1..9`)
//! - `5200 + index * 20 + 10`: XY rotation for that system
//! - `5211 .. 5219`: the G92 offset
//! - `5220`: the active work system
//!
//! The file format is two whitespace-separated columns, sorted by parameter
//! number. Lines that do not parse are preserved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use linc_core::model::AxisMap;

const WORK_OFFSET_BASE: u32 = 5200;
const WORK_OFFSET_STRIDE: u32 = 20;
const G92_BASE: u32 = 5211;
const ACTIVE_WCS: u32 = 5220;

#[derive(Clone, Debug, Default)]
pub struct ParameterFile {
    path: Option<PathBuf>,
    values: BTreeMap<u32, f64>,
    dirty: BTreeSet<u32>,
    last_error: Option<String>,
}

impl ParameterFile {
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut parameters = Self {
            path,
            values: BTreeMap::new(),
            dirty: BTreeSet::new(),
            last_error: None,
        };
        let Some(path) = parameters.path.clone() else {
            return parameters;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines() {
                    let mut parts = line.split_whitespace();
                    let (Some(key), Some(value), None) = (parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    let Ok(key) = key.parse::<u32>() else {
                        continue;
                    };
                    let Ok(value) = value.parse::<f64>() else {
                        continue;
                    };
                    parameters.values.insert(key, value);
                }
            }
            Err(error) => {
                parameters.last_error = Some(format!("could not read {}: {error}", path.display()));
            }
        }
        parameters
    }

    pub fn get(&self, key: u32) -> f64 {
        self.values.get(&key).copied().unwrap_or(0.0)
    }

    pub fn has(&self, key: u32) -> bool {
        self.values.contains_key(&key)
    }

    /// The work system stored in the parameter file, clamped to G54..G59.3.
    pub fn active_index(&self) -> u8 {
        (self.get(ACTIVE_WCS).round() as i32).clamp(1, 9) as u8
    }

    pub fn work_offset(&self, index: u8, axes: &[String]) -> AxisMap<f64> {
        let mut values = AxisMap::new();
        for (axis_index, axis) in axes.iter().enumerate() {
            values.insert(axis.clone(), self.get(offset_key(index, axis_index as u32)));
        }
        values
    }

    /// The stored value for one axis of `index`, when the file carries it.
    pub fn work_axis(&self, index: u8, axis_index: usize) -> Option<f64> {
        let key = offset_key(index, axis_index as u32);
        self.has(key).then(|| self.get(key))
    }

    pub fn rotation(&self, index: u8) -> f64 {
        self.get(rotation_key(index))
    }

    /// Record a `G10 L2` row. The caller has already checked that the values
    /// belong to `index`.
    pub fn record_work_offset(
        &mut self,
        index: u8,
        values: &AxisMap<f64>,
        axes: &[String],
        rotation: Option<f64>,
    ) {
        self.record_offset_axes(index, values, axes);
        if let Some(rotation) = rotation {
            self.record_rotation(index, rotation);
        }
    }

    /// Record only the axes in `values`; an offset row keeps the unmentioned
    /// axes. Used after `G10 L20`, where the controller computes the value.
    pub fn record_offset_axes(&mut self, index: u8, values: &AxisMap<f64>, axes: &[String]) {
        for (axis_index, axis) in axes.iter().enumerate() {
            if let Some(value) = values.get(axis) {
                let key = offset_key(index, axis_index as u32);
                self.values.insert(key, *value);
                self.dirty.insert(key);
            }
        }
    }

    pub fn record_rotation(&mut self, index: u8, rotation: f64) {
        let key = rotation_key(index);
        self.values.insert(key, rotation);
        self.dirty.insert(key);
    }

    /// Record the active work system (`5220`), so a session killed before the
    /// interpreter syncs still restores the system the operator selected.
    pub fn record_active_index(&mut self, index: u8) {
        self.values.insert(ACTIVE_WCS, index.clamp(1, 9) as f64);
        self.dirty.insert(ACTIVE_WCS);
    }

    pub fn record_g92(&mut self, values: &AxisMap<f64>, axes: &[String]) {
        for (axis_index, axis) in axes.iter().enumerate() {
            if let Some(value) = values.get(axis) {
                let key = G92_BASE + axis_index as u32;
                self.values.insert(key, *value);
                self.dirty.insert(key);
            }
        }
    }

    /// Does any axis row for `index` exist in the file?
    pub fn has_work_offset(&self, index: u8, axis_count: usize) -> bool {
        (0..axis_count as u32).any(|axis| self.has(offset_key(index, axis)))
    }

    /// Write the changed values back. Returns an error instead of reporting a
    /// write that did not reach the file; LinuxCNC reads this file at startup.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            let message = "no [RS274NGC] PARAMETER_FILE in the INI".to_string();
            self.last_error = Some(message.clone());
            return Err(message);
        };
        if self.dirty.is_empty() {
            self.last_error = None;
            return Ok(());
        }
        let original = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
        let written: BTreeMap<u32, f64> = self
            .dirty
            .iter()
            .filter_map(|key| self.values.get(key).map(|value| (*key, *value)))
            .collect();

        for line in &mut lines {
            let mut parts = line.split_whitespace();
            let (Some(key), Some(_), None) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let Ok(key) = key.parse::<u32>() else {
                continue;
            };
            if let Some(value) = written.get(&key) {
                *line = format!("{key}\t{value:.6}");
            }
        }

        // Insert missing keys in numeric order. The interpreter rejects the
        // whole file when its keys are out of order.
        let existing: std::collections::HashSet<u32> = lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let (Some(key), Some(_), None) = (parts.next(), parts.next(), parts.next()) else {
                    return None;
                };
                key.parse().ok()
            })
            .collect();
        for (key, value) in &written {
            if existing.contains(key) {
                continue;
            }
            let position = lines
                .iter()
                .position(|line| {
                    let mut parts = line.split_whitespace();
                    let (Some(other), Some(_), None) = (parts.next(), parts.next(), parts.next())
                    else {
                        return false;
                    };
                    other
                        .parse::<u32>()
                        .map(|other| other > *key)
                        .unwrap_or(false)
                })
                .unwrap_or(lines.len());
            lines.insert(position, format!("{key}\t{value:.6}"));
        }

        let mut output = lines.join("\n");
        output.push('\n');
        write_atomic(&path, output.as_bytes())
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;

        // Read it back: a value that was overwritten later in the file must
        // not be reported as saved.
        let check = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read back {}: {error}", path.display()))?;
        let mut actual = BTreeMap::new();
        for line in check.lines() {
            let mut parts = line.split_whitespace();
            let (Some(key), Some(value), None) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let (Ok(key), Ok(value)) = (key.parse::<u32>(), value.parse::<f64>()) else {
                continue;
            };
            actual.insert(key, value);
        }
        let wrong: Vec<String> = written
            .iter()
            .filter_map(|(key, value)| match actual.get(key) {
                None => Some(format!("{key}=missing (wrote {value})")),
                Some(found) if (found - value).abs() > 1e-6 => {
                    Some(format!("{key}={found} (wrote {value})"))
                }
                Some(_) => None,
            })
            .collect();
        if !wrong.is_empty() {
            let message = format!("{} did not keep: {}", path.display(), wrong.join(", "));
            self.last_error = Some(message.clone());
            return Err(message);
        }
        self.dirty.clear();
        self.last_error = None;
        Ok(())
    }
}

fn offset_key(index: u8, axis_index: u32) -> u32 {
    WORK_OFFSET_BASE + index as u32 * WORK_OFFSET_STRIDE + 1 + axis_index
}

fn rotation_key(index: u8) -> u32 {
    WORK_OFFSET_BASE + index as u32 * WORK_OFFSET_STRIDE + 10
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_rewrites_work_offsets_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rs274ngc.var");
        std::fs::write(
            &path,
            "100\t1.000000\n5220\t1.000000\n5221\t-11.100000\n5222\t-22.200000\n",
        )
        .unwrap();
        let mut parameters = ParameterFile::load(Some(path.clone()));
        assert_eq!(parameters.active_index(), 1);
        let axes = vec!["X".into(), "Y".into()];
        let mut values = AxisMap::new();
        values.insert("X".to_string(), -1.5);
        values.insert("Y".to_string(), 2.25);
        parameters.record_work_offset(2, &values, &axes, Some(30.0));
        parameters.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let keys: Vec<u32> = text
            .lines()
            .filter_map(|line| line.split_whitespace().next()?.parse().ok())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "parameter keys must stay ascending");
        assert!(text.contains("5241\t-1.500000"));
        assert!(text.contains("5242\t2.250000"));
        assert!(text.contains("5250\t30.000000"));

        let reread = ParameterFile::load(Some(path));
        assert_eq!(reread.work_offset(2, &axes).get("X"), Some(&-1.5));
        assert_eq!(reread.rotation(2), 30.0);
    }

    #[test]
    fn missing_axis_keeps_the_old_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rs274ngc.var");
        std::fs::write(&path, "5220\t1\n5221\t-1\n5222\t-2\n").unwrap();
        let mut parameters = ParameterFile::load(Some(path));
        let axes = vec!["X".into(), "Y".into()];
        let mut values = AxisMap::new();
        values.insert("Y".to_string(), 5.0);
        parameters.record_offset_axes(1, &values, &axes);
        parameters.save().unwrap();
        let reread = ParameterFile::load(Some(dir.path().join("rs274ngc.var")));
        assert_eq!(reread.work_offset(1, &axes).get("X"), Some(&-1.0));
        assert_eq!(reread.work_offset(1, &axes).get("Y"), Some(&5.0));
    }

    #[test]
    fn the_selected_work_system_is_mirrored_to_the_parameter_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rs274ngc.var");
        std::fs::write(&path, "5220\t1.000000\n").unwrap();
        let mut parameters = ParameterFile::load(Some(path.clone()));
        parameters.record_active_index(5);
        parameters.save().unwrap();
        assert_eq!(ParameterFile::load(Some(path)).active_index(), 5);
    }
}
