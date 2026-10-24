//! The machine body and travel envelope, built from the LinuxCNC INI.
//!
//! Machine millimetres, Z up, origin at the machine X0Y0Z0. The body is a small
//! set of axis-aligned boxes sized from the axis limits, so a change in the INI
//! moves the model: the table spans the X/Y travel, the spindle stands at the
//! machine origin (joint zero puts the tip at X0Y0) and everything else hangs
//! off that.

use linc_core::ini::MachineIni;

use crate::config::Sim3dConfig;
use crate::types::{Bounds, TriMesh, P3};

/// Clearance between the lowest tip and the table top, in mm.
const TABLE_CLEARANCE_MM: f64 = 20.0;
/// Table thickness, in mm.
const TABLE_THICKNESS_MM: f64 = 40.0;
/// Base thickness below the table, in mm.
const BASE_THICKNESS_MM: f64 = 70.0;
/// Half width of the column, in mm.
const COLUMN_HALF_WIDTH_MM: f64 = 90.0;
/// Half width of the head, in mm.
const HEAD_HALF_WIDTH_MM: f64 = 80.0;
/// Height of the head above the top of Z travel, in mm.
const HEAD_BOTTOM_MM: f64 = 30.0;
/// Head height, in mm.
const HEAD_HEIGHT_MM: f64 = 100.0;
/// How far the column stops below the top of the head, in mm.
const COLUMN_TOP_INSET_MM: f64 = 10.0;
/// Half width of the spindle, in mm.
const SPINDLE_HALF_WIDTH_MM: f64 = 35.0;
/// Spindle height, in mm.
const SPINDLE_HEIGHT_MM: f64 = 40.0;
/// Half width of the tool holder, in mm.
const HOLDER_HALF_WIDTH_MM: f64 = 22.0;
/// Extra reach of the base in X, in mm.
const BASE_OVERHANG_MM: f64 = 20.0;
/// Thickness of a limit plane slab, in mm.
const LIMIT_PLANE_THICKNESS_MM: f64 = 2.0;

/// The machine: its travel envelope and the boxes that make up the body.
#[derive(Debug, Clone)]
pub struct MachineModel {
    /// Display name, from the INI.
    pub name: String,
    /// Axis travel limits, from the INI.
    pub travel: Bounds,
    /// The body boxes, named base/table/column/head/spindle/holder.
    pub parts: Vec<TriMesh>,
    /// Thin slabs at each travel limit, for drawing.
    pub limit_planes: Vec<(String, TriMesh)>,
    /// Envelope of every body part.
    pub bounds: Bounds,
}

impl MachineModel {
    /// Build the body from the INI's axis limits.
    ///
    /// The config supplies the body margin and the holder length; the limits
    /// themselves come from the INI alone.
    pub fn from_ini(ini: &MachineIni, config: &Sim3dConfig) -> Self {
        let margin = config.machine.margin_mm.max(0.0);
        let (xmin, xmax) = axis_limits(ini, "X", (-300.0, 0.0));
        let (ymin, ymax) = axis_limits(ini, "Y", (-200.0, 0.0));
        let (zmin, zmax) = axis_limits(ini, "Z", (-200.0, 0.0));
        let travel = Bounds {
            min: [xmin, ymin, zmin],
            max: [xmax, ymax, zmax],
        };

        // The spindle stands at the machine origin. Clamp it into travel so a
        // machine whose limits exclude zero still gets a body over its table.
        let sx = 0.0f64.clamp(xmin, xmax);
        let sy = 0.0f64.clamp(ymin, ymax);

        let table_top = zmin - TABLE_CLEARANCE_MM;
        let table_bottom = table_top - TABLE_THICKNESS_MM;
        let base_bottom = table_bottom - BASE_THICKNESS_MM;
        let head_top = zmax + HEAD_BOTTOM_MM + HEAD_HEIGHT_MM;
        // The holder stands in for the gauge line, so its length follows the
        // configured tool offset.
        let holder_len = config.tools.fallback_tip_offset_mm.abs().max(10.0);

        let base = Bounds {
            min: [
                xmin - margin - BASE_OVERHANG_MM,
                ymin - margin - BASE_OVERHANG_MM,
                base_bottom,
            ],
            max: [
                xmax + margin + BASE_OVERHANG_MM,
                ymax + margin * 3.0,
                table_bottom,
            ],
        };
        let table = Bounds {
            min: [xmin - margin, ymin - margin, table_bottom],
            max: [xmax + margin, ymax + margin, table_top],
        };
        let column = Bounds {
            min: [sx - COLUMN_HALF_WIDTH_MM, ymax + margin, base_bottom],
            max: [
                sx + COLUMN_HALF_WIDTH_MM,
                ymax + margin * 3.0,
                head_top - COLUMN_TOP_INSET_MM,
            ],
        };
        let head = Bounds {
            min: [
                sx - HEAD_HALF_WIDTH_MM,
                sy - margin * 2.0,
                zmax + HEAD_BOTTOM_MM,
            ],
            max: [sx + HEAD_HALF_WIDTH_MM, ymax + margin * 2.0, head_top],
        };
        let spindle = Bounds {
            min: [sx - SPINDLE_HALF_WIDTH_MM, sy - SPINDLE_HALF_WIDTH_MM, zmax],
            max: [
                sx + SPINDLE_HALF_WIDTH_MM,
                sy + SPINDLE_HALF_WIDTH_MM,
                zmax + SPINDLE_HEIGHT_MM,
            ],
        };
        let holder = Bounds {
            min: [
                sx - HOLDER_HALF_WIDTH_MM,
                sy - HOLDER_HALF_WIDTH_MM,
                zmax - holder_len,
            ],
            max: [sx + HOLDER_HALF_WIDTH_MM, sy + HOLDER_HALF_WIDTH_MM, zmax],
        };
        let parts = vec![
            TriMesh::box_mesh("base", &base),
            TriMesh::box_mesh("table", &table),
            TriMesh::box_mesh("column", &column),
            TriMesh::box_mesh("head", &head),
            TriMesh::box_mesh("spindle", &spindle),
            TriMesh::box_mesh("holder", &holder),
        ];

        let thickness = LIMIT_PLANE_THICKNESS_MM;
        let slab = |name: &str, min: P3, max: P3| {
            (
                name.to_string(),
                TriMesh::box_mesh(name, &Bounds { min, max }),
            )
        };
        let limit_planes = vec![
            slab("x-min", [xmin, ymin, zmin], [xmin + thickness, ymax, zmax]),
            slab("x-max", [xmax - thickness, ymin, zmin], [xmax, ymax, zmax]),
            slab("y-min", [xmin, ymin, zmin], [xmax, ymin + thickness, zmax]),
            slab("y-max", [xmin, ymax - thickness, zmin], [xmax, ymax, zmax]),
            slab("z-min", [xmin, ymin, zmin], [xmax, ymax, zmin + thickness]),
            slab("z-max", [xmin, ymin, zmax - thickness], [xmax, ymax, zmax]),
        ];

        let bounds = parts
            .iter()
            .fold(Bounds::empty(), |acc, part| acc.union(&part.bounds));

        Self {
            name: ini.machine_name.clone(),
            travel,
            parts,
            limit_planes,
            bounds,
        }
    }

    /// Envelope of the body.
    pub fn machine_bounds(&self) -> Bounds {
        self.bounds
    }

    /// A body part by name.
    pub fn part(&self, name: &str) -> Option<&TriMesh> {
        self.parts.iter().find(|part| part.name == name)
    }

    /// True when the tip leaves the travel envelope, or the tool body leaves
    /// the machine body.
    ///
    /// The spindle and holder legitimately stand above the top of Z travel, so
    /// only the tip is tested against the travel box; the body that rises from
    /// the tip is tested against the machine body instead. Without that split
    /// every cutter at Z 0 would report a collision, because its flutes reach
    /// above the top of travel.
    pub fn envelope_violated(&self, tip: P3, body_top: P3) -> bool {
        const EPS: f64 = 1e-9;
        !self.travel.contains(tip, EPS) || !self.bounds.contains(body_top, EPS)
    }
}

/// Limits for one axis, or the fallback when the INI has no such axis.
fn axis_limits(ini: &MachineIni, name: &str, fallback: (f64, f64)) -> (f64, f64) {
    match ini.limits_for(name) {
        Some((min, max)) if min < max => (min, max),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini_text() -> &'static str {
        "[EMC]\n\
         MACHINE = Test Mill\n\
         [KINS]\n\
         JOINTS = 3\n\
         KINEMATICS = trivkins coordinates=XYZ\n\
         [TRAJ]\n\
         COORDINATES = XYZ\n\
         [EMCIO]\n\
         TOOL_TABLE = tool.tbl\n\
         [AXIS_X]\n\
         MIN_LIMIT = -100\n\
         MAX_LIMIT = 0\n\
         [AXIS_Y]\n\
         MIN_LIMIT = -80\n\
         MAX_LIMIT = 2\n\
         [AXIS_Z]\n\
         MIN_LIMIT = -50\n\
         MAX_LIMIT = 10\n"
    }

    #[test]
    fn body_follows_the_axis_limits() {
        let ini = MachineIni::parse(std::path::Path::new("test.ini"), ini_text()).unwrap();
        let machine = MachineModel::from_ini(&ini, &Sim3dConfig::default());
        assert_eq!(machine.name, "Test Mill");
        assert_eq!(machine.travel.min, [-100.0, -80.0, -50.0]);
        assert_eq!(machine.travel.max, [0.0, 2.0, 10.0]);
        assert_eq!(machine.parts.len(), 6);
        assert_eq!(machine.limit_planes.len(), 6);
        for part in &machine.parts {
            assert!(!part.is_empty(), "{} is empty", part.name);
            assert!(
                machine.bounds.contains(part.bounds.min, 1e-9)
                    && machine.bounds.contains(part.bounds.max, 1e-9),
                "{} leaves the machine bounds",
                part.name
            );
        }
        for (name, plane) in &machine.limit_planes {
            assert!(!plane.is_empty(), "limit plane {name} is empty");
        }
    }

    #[test]
    fn envelope_checks_the_tip_and_the_body() {
        let ini = MachineIni::parse(std::path::Path::new("test.ini"), ini_text()).unwrap();
        let machine = MachineModel::from_ini(&ini, &Sim3dConfig::default());
        assert!(!machine.envelope_violated([-50.0, -40.0, -50.0], [-50.0, -40.0, -30.0]));
        assert!(
            machine.envelope_violated([10.0, -40.0, -50.0], [10.0, -40.0, -30.0]),
            "X past the travel maximum"
        );
        assert!(
            !machine.envelope_violated([0.0, 0.0, 10.0], [0.0, 0.0, 30.0]),
            "a cutter at the top of travel still fits inside the machine body"
        );
        assert!(
            machine.envelope_violated([-50.0, -40.0, -500.0], [-50.0, -40.0, -480.0]),
            "below the base"
        );
    }
}
