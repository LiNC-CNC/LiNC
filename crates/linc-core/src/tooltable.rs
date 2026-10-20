//! Standard LinuxCNC tool table parsing and atomic writes.
//!
//! Columns come from `TOOL_TABLE_COLUMNS` in the INI. `P` is parsed only when
//! the profile uses an ATC/turret; manual tables never expose or write a
//! pocket column.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::model::{AxisMap, ToolChangerKind, ToolColumn, ToolGeometry, ToolRow, ToolTableState};

#[derive(Debug, Error)]
pub enum ToolTableError {
    #[error("cannot read tool table {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write tool table {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("tool table revision changed")]
    RevisionConflict,
    #[error("invalid tool table: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug)]
pub struct ToolTable {
    pub path: PathBuf,
    pub columns: Vec<ToolColumn>,
    pub rows: Vec<ToolRow>,
    pub revision: u64,
    pub changer: ToolChangerKind,
}

impl ToolTable {
    pub fn load(
        path: &Path,
        columns: &str,
        changer: ToolChangerKind,
    ) -> Result<Self, ToolTableError> {
        let text = std::fs::read_to_string(path).map_err(|source| ToolTableError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &text, columns, changer)
    }

    pub fn parse(
        path: &Path,
        text: &str,
        columns: &str,
        changer: ToolChangerKind,
    ) -> Result<Self, ToolTableError> {
        let mut parsed_columns = Vec::new();
        for ch in columns.chars() {
            if let Some(column) = ToolColumn::parse(ch) {
                if !parsed_columns.contains(&column) {
                    parsed_columns.push(column);
                }
            }
        }
        if !parsed_columns.contains(&ToolColumn::T) {
            return Err(ToolTableError::Invalid(
                "TOOL_TABLE_COLUMNS must include T".into(),
            ));
        }
        if changer.uses_pockets() && !parsed_columns.contains(&ToolColumn::P) {
            return Err(ToolTableError::Invalid(
                "ATC/turret tool table requires the standard P column".into(),
            ));
        }
        if !changer.uses_pockets() && parsed_columns.contains(&ToolColumn::P) {
            return Err(ToolTableError::Invalid(
                "manual changer must not declare the P column".into(),
            ));
        }

        let mut rows = Vec::new();
        for (line_no, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            let (words, description) = match line.split_once(';') {
                Some((words, description)) => (words.trim(), description.trim().to_string()),
                None => (line, String::new()),
            };
            let fields = parse_words(words);
            let tool = fields.get(&ToolColumn::T).copied().ok_or_else(|| {
                ToolTableError::Invalid(format!("line {} has no T word: {raw_line}", line_no + 1))
            })?;
            if tool < 0.0 {
                return Err(ToolTableError::Invalid(format!(
                    "line {} has negative tool number",
                    line_no + 1
                )));
            }
            let tool = tool as u16;
            let pocket = if changer.uses_pockets() {
                fields.get(&ToolColumn::P).map(|value| *value as u16)
            } else {
                None
            };
            let mut geometry = ToolGeometry::default();
            for (column, value) in &fields {
                match column {
                    ToolColumn::T | ToolColumn::P => {}
                    ToolColumn::D => geometry.diameter = Some(*value),
                    ToolColumn::Z => {
                        geometry.length = Some(*value);
                        geometry.values.insert("Z", *value);
                    }
                    ToolColumn::X => {
                        geometry.values.insert("X", *value);
                    }
                    ToolColumn::I => geometry.front_angle = Some(*value),
                    ToolColumn::J => geometry.back_angle = Some(*value),
                    ToolColumn::Q => geometry.orientation = Some(*value as u8),
                    ToolColumn::R => geometry.nose_radius = Some(*value),
                    ToolColumn::A => geometry.values.insert("A", *value),
                    ToolColumn::B => geometry.values.insert("B", *value),
                    ToolColumn::C => geometry.values.insert("C", *value),
                    ToolColumn::U => geometry.values.insert("U", *value),
                    ToolColumn::V => geometry.values.insert("V", *value),
                    ToolColumn::W => geometry.values.insert("W", *value),
                    ToolColumn::H => geometry.values.insert("H", *value),
                    ToolColumn::E => geometry.values.insert("E", *value),
                    ToolColumn::K => geometry.values.insert("K", *value),
                    ToolColumn::F => geometry.values.insert("F", *value),
                }
            }
            let kind = infer_kind(&geometry, changer);
            rows.push(ToolRow {
                tool,
                pocket,
                description,
                geometry,
                kind,
            });
        }
        rows.sort_by_key(|row| row.tool);
        let mut tools = HashSet::new();
        let mut pockets = HashSet::new();
        for row in &rows {
            if !tools.insert(row.tool) {
                return Err(ToolTableError::Invalid(format!(
                    "duplicate tool T{}",
                    row.tool
                )));
            }
            if let Some(pocket) = row.pocket {
                if pocket > 0 && !pockets.insert(pocket) {
                    return Err(ToolTableError::Invalid(format!(
                        "duplicate pocket P{pocket}"
                    )));
                }
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            columns: parsed_columns,
            rows,
            revision: revision_for(text),
            changer,
        })
    }

    pub fn state(&self) -> ToolTableState {
        ToolTableState {
            path: self.path.clone(),
            columns: self.columns.clone(),
            rows: self.rows.clone(),
            revision: self.revision,
            changer: self.changer,
            error: None,
        }
    }

    pub fn row(&self, tool: u16) -> Option<&ToolRow> {
        self.rows.iter().find(|row| row.tool == tool)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(";");
        for column in &self.columns {
            out.push_str(column.label());
            out.push(' ');
        }
        out.push_str(" Remark\n");
        for row in &self.rows {
            out.push_str(&render_row(row, &self.columns, self.changer));
            out.push('\n');
        }
        out
    }

    /// Save only if the on-disk revision still matches. Uses a temporary file
    /// in the same directory and an atomic rename.
    pub fn save(&mut self, expected_revision: u64) -> Result<(), ToolTableError> {
        let current =
            std::fs::read_to_string(&self.path).map_err(|source| ToolTableError::Read {
                path: self.path.clone(),
                source,
            })?;
        if revision_for(&current) != expected_revision || expected_revision != self.revision {
            return Err(ToolTableError::RevisionConflict);
        }
        self.revision = expected_revision.wrapping_add(1);
        atomic_write(&self.path, self.render().as_bytes()).map_err(|source| {
            ToolTableError::Write {
                path: self.path.clone(),
                source,
            }
        })?;
        self.revision = revision_for(&self.render());
        Ok(())
    }

    pub fn upsert(&mut self, row: ToolRow) -> Result<(), ToolTableError> {
        if row.tool == 0 {
            return Err(ToolTableError::Invalid("tool number must be >= 1".into()));
        }
        if self.changer.uses_pockets() && row.pocket.is_none() {
            return Err(ToolTableError::Invalid(
                "ATC/turret row requires a pocket".into(),
            ));
        }
        if let Some(pocket) = row.pocket {
            if pocket > 0
                && self
                    .rows
                    .iter()
                    .any(|existing| existing.tool != row.tool && existing.pocket == Some(pocket))
            {
                return Err(ToolTableError::Invalid(format!(
                    "pocket P{pocket} is already used"
                )));
            }
        }
        if let Some(existing) = self
            .rows
            .iter_mut()
            .find(|existing| existing.tool == row.tool)
        {
            *existing = row;
        } else {
            self.rows.push(row);
            self.rows.sort_by_key(|row| row.tool);
        }
        Ok(())
    }

    pub fn reset_revision_from_disk(&mut self) -> Result<(), ToolTableError> {
        let text = std::fs::read_to_string(&self.path).map_err(|source| ToolTableError::Read {
            path: self.path.clone(),
            source,
        })?;
        self.revision = revision_for(&text);
        Ok(())
    }
}

fn infer_kind(geometry: &ToolGeometry, changer: ToolChangerKind) -> String {
    if changer == ToolChangerKind::Turret {
        if geometry.front_angle.is_some() || geometry.back_angle.is_some() {
            return "TURN".into();
        }
        if geometry.nose_radius.is_some() {
            return "BORING".into();
        }
        return "TOOL".into();
    }
    if let Some(diameter) = geometry.diameter {
        if diameter <= 0.5 {
            "DRILL".into()
        } else {
            "MILL".into()
        }
    } else {
        "MILL".into()
    }
}

fn parse_words(text: &str) -> std::collections::BTreeMap<ToolColumn, f64> {
    let bytes = text.as_bytes();
    let mut fields = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_alphabetic() {
            let column = ToolColumn::parse(ch);
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let start = i;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && ((bytes[i] as char).is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Some(column) = column {
                if let Ok(value) = text[start..i].parse::<f64>() {
                    fields.insert(column, value);
                }
            }
        } else {
            i += 1;
        }
    }
    fields
}

fn render_row(row: &ToolRow, columns: &[ToolColumn], changer: ToolChangerKind) -> String {
    let mut out = String::new();
    for column in columns {
        let value: Option<f64> = match column {
            ToolColumn::T => Some(row.tool as f64),
            ToolColumn::P => {
                if changer.uses_pockets() {
                    row.pocket.map(|pocket| pocket as f64)
                } else {
                    None
                }
            }
            ToolColumn::D => row.geometry.diameter,
            ToolColumn::Z => row
                .geometry
                .values
                .get("Z")
                .copied()
                .or(row.geometry.length),
            ToolColumn::R => row.geometry.nose_radius,
            ToolColumn::Q => row.geometry.orientation.map(|value| value as f64),
            ToolColumn::I => row.geometry.front_angle,
            ToolColumn::J => row.geometry.back_angle,
            other => row.geometry.values.get(other.label()).copied(),
        };
        if let Some(value) = value {
            out.push_str(column.label());
            if value >= 0.0 && *column != ToolColumn::T && *column != ToolColumn::P {
                out.push('+');
            }
            if value.fract().abs() < 1e-9 {
                out.push_str(&format!("{} ", value as i64));
            } else {
                out.push_str(&format!("{value:.6} "));
            }
        }
    }
    if !row.description.is_empty() {
        out.push_str("; ");
        out.push_str(&row.description);
    }
    out.trim_end().to_string()
}

pub fn revision_for(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish().max(1)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Convert a tool table state to rows for the UI.
pub fn axis_values(row: &ToolRow, axes: &[String]) -> AxisMap<f64> {
    let mut values = AxisMap::new();
    for axis in axes {
        values.insert(axis.clone(), *row.geometry.values.get(axis).unwrap_or(&0.0));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    const MILL: &str = ";Tool Pocket Z Offset Diameter Remark\nT1 P0 Z-56.068 D+6.0 ; 6mm end mill\nT2 P0 Z0 D+17.3 ; face mill\n";

    #[test]
    fn parses_standard_table_and_exposes_pocket_only_for_atc() {
        let table =
            ToolTable::parse(Path::new("tool.tbl"), MILL, "TZDR", ToolChangerKind::Manual).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].tool, 1);
        assert!(table.rows[0].pocket.is_none());
        assert_eq!(table.rows[0].geometry.length, Some(-56.068));
        assert_eq!(table.rows[0].geometry.diameter, Some(6.0));
        assert!(!table.columns.contains(&ToolColumn::P));
        let atc =
            ToolTable::parse(Path::new("tool.tbl"), MILL, "TZPDR", ToolChangerKind::Atc).unwrap();
        assert_eq!(atc.rows[0].pocket, Some(0));
    }

    #[test]
    fn manual_rejects_p_column() {
        let error = ToolTable::parse(
            Path::new("tool.tbl"),
            MILL,
            "TZPDR",
            ToolChangerKind::Manual,
        )
        .unwrap_err();
        assert!(error.to_string().contains("must not declare"));
    }

    #[test]
    fn atc_rejects_missing_p_column() {
        let error = ToolTable::parse(Path::new("tool.tbl"), MILL, "TZDR", ToolChangerKind::Atc)
            .unwrap_err();
        assert!(error.to_string().contains("P column"));
    }

    #[test]
    fn roundtrips_and_detects_revision_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool.tbl");
        std::fs::write(&path, MILL).unwrap();
        let mut table = ToolTable::load(&path, "TZDR", ToolChangerKind::Manual).unwrap();
        let revision = table.revision;
        table.rows[0].description = "updated".into();
        table.save(revision).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("updated"));
        assert!(!text.contains("P0"));
        assert!(matches!(
            table.save(revision),
            Err(ToolTableError::RevisionConflict)
        ));
    }

    #[test]
    fn rejects_duplicate_tools_and_pockets() {
        let text = "T1 P1 Z0\nT1 P2 Z0\n";
        assert!(
            ToolTable::parse(Path::new("tool.tbl"), text, "TZP", ToolChangerKind::Atc).is_err()
        );
        let text = "T1 P1 Z0\nT2 P1 Z0\n";
        assert!(
            ToolTable::parse(Path::new("tool.tbl"), text, "TZP", ToolChangerKind::Atc).is_err()
        );
    }

    #[test]
    fn parses_lathe_columns() {
        let text = "T1 P0 X+0.0 Z+0.0 I+90 J+10 D+0.0625 Q2 ; RH turning\n";
        let table = ToolTable::parse(
            Path::new("lathe.tbl"),
            text,
            "TXZIJDQR",
            ToolChangerKind::Manual,
        )
        .unwrap();
        assert_eq!(table.rows[0].geometry.front_angle, Some(90.0));
        assert_eq!(table.rows[0].geometry.back_angle, Some(10.0));
        assert_eq!(table.rows[0].geometry.diameter, Some(0.0625));
        assert_eq!(table.rows[0].geometry.orientation, Some(2));
    }
}
