//! Probe tip and cutter geometry from the tool table.
//!
//! The tool table is the single source of truth for what hangs in the spindle:
//! the D column is the diameter in mm, the Z column the tool offset. The
//! config only adds what the table cannot say - a probe ball override, the
//! stylus and flute lengths, and cutter profiles for tools the table describes
//! only by diameter.

use linc_core::ini::MachineIni;
use linc_core::model::{ToolChangerKind, ToolRow};
use linc_core::tooltable::ToolTable;

use crate::config::{CutterKind, Sim3dConfig};
use crate::types::{Capsule, Sphere, P3};

/// Ball radius used when neither the tool table nor the config gives one.
pub const DEFAULT_PROBE_TIP_RADIUS_MM: f64 = 1.5;
/// Columns assumed when the INI declares none.
pub const DEFAULT_TOOL_TABLE_COLUMNS: &str = "TZDR";

/// The probe, as the tool table and config describe it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeGeom {
    pub tool: u16,
    /// Ball radius in mm.
    pub tip_radius: f64,
    /// Stylus length above the ball, in mm.
    pub stylus_len: f64,
    /// Tool offset used when the engine reports none, in mm.
    pub tip_offset: f64,
}

/// The cutting profile of a tool, or the probe ball.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CutterShape {
    Flat { diameter: f64 },
    Ball { diameter: f64 },
    Bull { diameter: f64, corner_radius: f64 },
    Drill { diameter: f64 },
    Chamfer { diameter: f64 },
    Probe { tip_radius: f64 },
}

impl CutterShape {
    /// True for the probe profile.
    pub fn is_probe(&self) -> bool {
        matches!(self, Self::Probe { .. })
    }

    /// Nominal diameter in mm; twice the ball radius for the probe.
    pub fn diameter(&self) -> f64 {
        match *self {
            Self::Flat { diameter }
            | Self::Ball { diameter }
            | Self::Bull { diameter, .. }
            | Self::Drill { diameter }
            | Self::Chamfer { diameter } => diameter,
            Self::Probe { tip_radius } => tip_radius * 2.0,
        }
    }

    /// Radius of the volume at the very tip in mm.
    pub fn tip_radius(&self) -> f64 {
        match *self {
            Self::Bull { corner_radius, .. } if corner_radius > 0.0 => corner_radius,
            Self::Probe { tip_radius } => tip_radius,
            other => other.diameter() / 2.0,
        }
    }

    /// Radius the swept body has above the tip, in mm.
    pub fn body_radius(&self) -> f64 {
        match *self {
            Self::Probe { tip_radius } => tip_radius,
            other => other.diameter() / 2.0,
        }
    }

    /// Build a shape from a profile override; a bull nose without a corner
    /// radius uses a quarter of the diameter.
    pub fn from_override(kind: CutterKind, diameter: f64, corner_radius_mm: Option<f64>) -> Self {
        let diameter = diameter.abs();
        match kind {
            CutterKind::Flat => Self::Flat { diameter },
            CutterKind::Ball => Self::Ball { diameter },
            CutterKind::Bull => Self::Bull {
                diameter,
                corner_radius: corner_radius_mm
                    .filter(|radius| *radius > 0.0)
                    .unwrap_or(diameter / 4.0),
            },
            CutterKind::Drill => Self::Drill { diameter },
            CutterKind::Chamfer => Self::Chamfer { diameter },
        }
    }
}

/// The tool currently in the spindle, with the lengths the queries need.
///
/// The first four fields are the shared contract; body_radius and length are
/// additive and only feed contact_shapes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActiveTool {
    pub number: u16,
    /// Tool offset from the gauge line, in mm (negative = below it).
    pub z_offset: f64,
    /// Radius of the volume at the tip, in mm.
    pub tip_radius: f64,
    pub shape: CutterShape,
    /// Radius of the body above the tip, in mm.
    pub body_radius: f64,
    /// Stylus or flute length along +Z from the tip, in mm.
    pub length: f64,
}

impl ActiveTool {
    /// True when the spindle holds the probe.
    pub fn is_probe(&self) -> bool {
        self.shape.is_probe()
    }

    /// The probe ball and the stylus/cutter capsule, both anchored at the tip
    /// and growing along +Z.
    pub fn contact_shapes(&self, tip: P3) -> (Sphere, Capsule) {
        let sphere = Sphere {
            center: tip,
            radius: self.tip_radius,
        };
        let capsule = Capsule {
            a: tip,
            b: [tip[0], tip[1], tip[2] + self.length],
            radius: self.body_radius,
        };
        (sphere, capsule)
    }
}

/// What loading the tool table produced: the table, or the reason it is gone.
#[derive(Debug, Clone)]
pub struct ToolTableLoad {
    pub table: Option<ToolTable>,
    pub warning: Option<String>,
}

/// Load the tool table the INI names.
///
/// A missing or unreadable table is a warning, not a hard failure: the
/// simulator then runs with the config's fallbacks alone.
pub fn load_tool_table(ini: &MachineIni) -> ToolTableLoad {
    let columns = if ini.tool_table_columns.trim().is_empty() {
        DEFAULT_TOOL_TABLE_COLUMNS
    } else {
        ini.tool_table_columns.trim()
    };
    // The columns decide the table format, not the ATC section: linc-core
    // inserts a P word for a manual table, and refuses an ATC table whose
    // columns leave P out. configs/sim.ini declares a 12 pocket carousel but
    // writes TZDR, so it loads as a manual table and keeps its rows.
    let changer = if columns.contains('P') {
        ToolChangerKind::Atc
    } else {
        ToolChangerKind::Manual
    };
    match ToolTable::load(&ini.tool_table, columns, changer) {
        Ok(table) => ToolTableLoad {
            table: Some(table),
            warning: None,
        },
        Err(error) => {
            let warning = format!("tool table {} unusable: {error}", ini.tool_table.display());
            log::warn!("sim3d: {warning}");
            ToolTableLoad {
                table: None,
                warning: Some(warning),
            }
        }
    }
}

/// Probe geometry from the tool table, with the config's overrides.
pub fn probe_from_table(table: &ToolTable, cfg: &Sim3dConfig) -> ProbeGeom {
    probe_from_row(table.row(cfg.probe.tool_number), cfg)
}

/// Probe geometry when no tool table could be loaded.
pub fn probe_without_table(cfg: &Sim3dConfig) -> ProbeGeom {
    probe_from_row(None, cfg)
}

fn probe_from_row(row: Option<&ToolRow>, cfg: &Sim3dConfig) -> ProbeGeom {
    let diameter = row
        .and_then(|row| row.geometry.diameter)
        .unwrap_or(0.0)
        .abs();
    let table_radius = if diameter > 0.0 {
        diameter / 2.0
    } else {
        DEFAULT_PROBE_TIP_RADIUS_MM
    };
    let tip_radius = cfg
        .probe
        .tip_radius_mm
        .filter(|radius| *radius > 0.0)
        .unwrap_or(table_radius);
    let row_z = row
        .and_then(|row| row.geometry.length)
        .filter(|z| *z != 0.0);
    ProbeGeom {
        tool: cfg.probe.tool_number,
        tip_radius,
        stylus_len: cfg.probe.stylus_length_mm.max(0.0),
        tip_offset: row_z.unwrap_or(cfg.probe.tip_offset_mm),
    }
}

/// Cutter shape for a tool table row: a config override, or flat with D.
pub fn cutter_from_row(row: Option<&ToolRow>, cfg: &Sim3dConfig) -> CutterShape {
    let diameter = row
        .and_then(|row| row.geometry.diameter)
        .unwrap_or(0.0)
        .abs();
    match row.and_then(|row| cfg.cutters.iter().find(|cut| cut.tool == row.tool)) {
        Some(over) => CutterShape::from_override(over.kind, diameter, over.corner_radius_mm),
        None => CutterShape::Flat { diameter },
    }
}

/// The active tool, resolving the tool offset from the table and the config.
///
/// A tool offset the engine reports wins; otherwise the tool row's Z; otherwise
/// the config fallback for that kind of tool.
pub fn active_tool(
    tool_number: u16,
    tool_offset_z: f64,
    table: Option<&ToolTable>,
    cfg: &Sim3dConfig,
) -> ActiveTool {
    let row = table.and_then(|table| table.row(tool_number));
    if tool_number == cfg.probe.tool_number {
        let probe = probe_from_row(row, cfg);
        let body_radius = cfg
            .tools
            .shank_radius_mm
            .filter(|radius| *radius > 0.0)
            .unwrap_or(probe.tip_radius);
        return ActiveTool {
            number: tool_number,
            z_offset: nonzero(tool_offset_z).unwrap_or(probe.tip_offset),
            tip_radius: probe.tip_radius,
            shape: CutterShape::Probe {
                tip_radius: probe.tip_radius,
            },
            body_radius,
            length: probe.stylus_len,
        };
    }
    let shape = cutter_from_row(row, cfg);
    let row_z = row
        .and_then(|row| row.geometry.length)
        .filter(|z| *z != 0.0);
    ActiveTool {
        number: tool_number,
        z_offset: nonzero(tool_offset_z)
            .unwrap_or_else(|| row_z.unwrap_or(cfg.tools.fallback_tip_offset_mm)),
        tip_radius: shape.tip_radius(),
        shape,
        body_radius: shape.body_radius(),
        length: cfg.tools.flute_length_mm.max(0.0),
    }
}

fn nonzero(value: f64) -> Option<f64> {
    (value != 0.0 && value.is_finite()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn table(text: &str) -> ToolTable {
        ToolTable::parse(Path::new("tool.tbl"), text, "TZDR", ToolChangerKind::Manual)
            .expect("tool table")
    }

    #[test]
    fn probe_radius_comes_from_the_d_column() {
        let table = table("T99 P0 Z+0 D+3 ; probe\n");
        let probe = probe_from_table(&table, &Sim3dConfig::default());
        assert_eq!(probe.tool, 99);
        assert_eq!(probe.tip_radius, 1.5);
        assert_eq!(probe.stylus_len, 30.0);
        assert_eq!(
            probe.tip_offset, -30.0,
            "zero table Z falls back to the config"
        );
    }

    #[test]
    fn probe_override_beats_the_table() {
        let table = table("T99 P0 Z+0 D+3 ; probe\n");
        let mut cfg = Sim3dConfig::default();
        cfg.probe.tip_radius_mm = Some(2.0);
        assert_eq!(probe_from_table(&table, &cfg).tip_radius, 2.0);
    }

    #[test]
    fn a_missing_row_falls_back_to_the_configured_default() {
        let table = table("T1 P0 Z-56.068 D+6 ; endmill\n");
        let cfg = Sim3dConfig::default();
        assert_eq!(
            probe_from_table(&table, &cfg).tip_radius,
            DEFAULT_PROBE_TIP_RADIUS_MM
        );
        let probe = active_tool(99, 0.0, Some(&table), &cfg);
        assert!(probe.is_probe());
        assert_eq!(probe.z_offset, -30.0);
        assert_eq!(probe.tip_radius, DEFAULT_PROBE_TIP_RADIUS_MM);

        let cutter = active_tool(4, 0.0, Some(&table), &cfg);
        assert!(!cutter.is_probe());
        assert_eq!(cutter.z_offset, cfg.tools.fallback_tip_offset_mm);
        assert_eq!(cutter.shape, CutterShape::Flat { diameter: 0.0 });
    }

    #[test]
    fn cutter_overrides_change_the_profile() {
        let table = table("T1 P0 Z-56 D+6\nT2 P0 Z+0 D+17.3\nT4 P0 Z+0 D+13.45\n");
        let mut cfg = Sim3dConfig::default();
        cfg.cutters = vec![
            crate::config::CutterConfig {
                tool: 2,
                kind: CutterKind::Ball,
                corner_radius_mm: None,
            },
            crate::config::CutterConfig {
                tool: 4,
                kind: CutterKind::Bull,
                corner_radius_mm: None,
            },
        ];
        assert_eq!(
            cutter_from_row(table.row(1), &cfg),
            CutterShape::Flat { diameter: 6.0 }
        );
        assert_eq!(
            cutter_from_row(table.row(2), &cfg),
            CutterShape::Ball { diameter: 17.3 }
        );
        assert_eq!(
            cutter_from_row(table.row(4), &cfg),
            CutterShape::Bull {
                diameter: 13.45,
                corner_radius: 13.45 / 4.0
            }
        );
    }

    #[test]
    fn contact_shapes_grow_along_positive_z() {
        let tool = ActiveTool {
            number: 4,
            z_offset: -30.0,
            tip_radius: 3.0,
            shape: CutterShape::Flat { diameter: 6.0 },
            body_radius: 3.0,
            length: 20.0,
        };
        let (sphere, capsule) = tool.contact_shapes([1.0, 2.0, -100.0]);
        assert_eq!(sphere.radius, 3.0);
        assert_eq!(sphere.center, [1.0, 2.0, -100.0]);
        assert_eq!(capsule.a, [1.0, 2.0, -100.0]);
        assert_eq!(capsule.b, [1.0, 2.0, -80.0]);
        assert_eq!(capsule.radius, 3.0);
    }
}
