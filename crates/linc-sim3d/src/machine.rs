//! A simple moving-column milling machine built from the LinuxCNC INI.
//!
//! Machine millimetres, Z up. The base and table are fixed, the column follows
//! the spindle in X/Y, and the complete belt-drive head follows its gauge line
//! in X/Y/Z. Meshes are stored at the reference position; the viewer applies the
//! live translation without rebuilding their lighting.

use linc_core::ini::MachineIni;

use crate::config::Sim3dConfig;
use crate::types::{Bounds, TriMesh, P3};

const TABLE_CLEARANCE_MM: f64 = 20.0;
const TABLE_THICKNESS_MM: f64 = 40.0;
const BASE_THICKNESS_MM: f64 = 70.0;
const BASE_OVERHANG_MM: f64 = 45.0;
const COLUMN_HALF_WIDTH_MM: f64 = 70.0;
const COLUMN_FRONT_OFFSET_MM: f64 = 50.0;
const COLUMN_DEPTH_MM: f64 = 80.0;
const HEAD_HALF_WIDTH_MM: f64 = 75.0;
const SPINDLE_RADIUS_MM: f64 = 32.0;
const SPINDLE_HEIGHT_MM: f64 = 65.0;
const HOLDER_RADIUS_MM: f64 = 18.0;
const MOTOR_OFFSET_X_MM: f64 = 105.0;
const MOTOR_HALF_WIDTH_MM: f64 = 28.0;
const LIMIT_PLANE_THICKNESS_MM: f64 = 2.0;

/// The machine geometry at its reference position, grouped by axis motion.
#[derive(Debug, Clone)]
pub struct MachineModel {
    pub name: String,
    pub travel: Bounds,
    /// Fixed base and table.
    pub parts: Vec<TriMesh>,
    /// Column casting and Z ways; these follow the head in X/Y.
    pub column_parts: Vec<TriMesh>,
    /// Head, spindle, motor and belt; these follow the gauge line in X/Y/Z.
    pub head_parts: Vec<TriMesh>,
    /// Gauge-line position corresponding to the stored moving meshes.
    pub reference_gauge: P3,
    pub limit_planes: Vec<(String, TriMesh)>,
    /// Visual envelope, including the moving assemblies throughout travel.
    pub bounds: Bounds,
}

impl MachineModel {
    pub fn from_ini(ini: &MachineIni, config: &Sim3dConfig) -> Self {
        let margin = config.machine.margin_mm.max(0.0);
        let (xmin, xmax) = axis_limits(ini, "X", (-300.0, 0.0));
        let (ymin, ymax) = axis_limits(ini, "Y", (-200.0, 0.0));
        let (zmin, zmax) = axis_limits(ini, "Z", (-200.0, 0.0));
        let travel = Bounds {
            min: [xmin, ymin, zmin],
            max: [xmax, ymax, zmax],
        };
        let sx = 0.0f64.clamp(xmin, xmax);
        let sy = 0.0f64.clamp(ymin, ymax);
        let holder_len = config.tools.fallback_tip_offset_mm.abs().max(10.0);
        let reference_gauge = [sx, sy, zmax + holder_len];

        let table_top = zmin - TABLE_CLEARANCE_MM;
        let table_bottom = table_top - TABLE_THICKNESS_MM;
        let base_bottom = table_bottom - BASE_THICKNESS_MM;
        let head_top = reference_gauge[2] + 100.0;
        let base_x_margin = (margin + BASE_OVERHANG_MM).max(COLUMN_HALF_WIDTH_MM + 20.0);

        let base = Bounds {
            min: [
                xmin - base_x_margin,
                ymin - margin - BASE_OVERHANG_MM,
                base_bottom,
            ],
            max: [
                xmax + base_x_margin,
                ymax + margin + COLUMN_FRONT_OFFSET_MM + COLUMN_DEPTH_MM + 20.0,
                table_bottom,
            ],
        };
        let table = Bounds {
            min: [xmin - margin, ymin - margin, table_bottom],
            max: [xmax + margin, ymax + margin, table_top],
        };
        let parts = vec![
            TriMesh::box_mesh("base", &base),
            TriMesh::box_mesh("table", &table),
        ];

        let column = Bounds {
            min: [
                sx - COLUMN_HALF_WIDTH_MM,
                sy + COLUMN_FRONT_OFFSET_MM,
                table_bottom,
            ],
            max: [
                sx + COLUMN_HALF_WIDTH_MM,
                sy + COLUMN_FRONT_OFFSET_MM + COLUMN_DEPTH_MM,
                head_top,
            ],
        };
        let column_foot = Bounds {
            min: [sx - COLUMN_HALF_WIDTH_MM - 18.0, sy + 30.0, table_bottom],
            max: [
                sx + COLUMN_HALF_WIDTH_MM + 18.0,
                sy + COLUMN_FRONT_OFFSET_MM + COLUMN_DEPTH_MM + 18.0,
                table_bottom + 30.0,
            ],
        };
        let rail = |name: &str, x: f64| {
            TriMesh::box_mesh(
                name,
                &Bounds {
                    min: [x - 9.0, sy + COLUMN_FRONT_OFFSET_MM - 8.0, table_top],
                    max: [x + 9.0, sy + COLUMN_FRONT_OFFSET_MM, head_top - 20.0],
                },
            )
        };
        let column_parts = vec![
            TriMesh::box_mesh("column", &column),
            TriMesh::box_mesh("column-foot", &column_foot),
            rail("z-rail-left", sx - 45.0),
            rail("z-rail-right", sx + 45.0),
        ];

        let gauge = reference_gauge[2];
        let head = Bounds {
            min: [sx - HEAD_HALF_WIDTH_MM, sy - 42.0, gauge + 10.0],
            max: [sx + HEAD_HALF_WIDTH_MM, sy + 62.0, gauge + 85.0],
        };
        let motor = Bounds {
            min: [
                sx + MOTOR_OFFSET_X_MM - MOTOR_HALF_WIDTH_MM,
                sy - 32.0,
                gauge + 20.0,
            ],
            max: [
                sx + MOTOR_OFFSET_X_MM + MOTOR_HALF_WIDTH_MM,
                sy + 32.0,
                gauge + 84.0,
            ],
        };
        let belt = |name: &str, y0: f64, y1: f64| {
            TriMesh::box_mesh(
                name,
                &Bounds {
                    min: [sx, sy + y0, gauge + 87.0],
                    max: [sx + MOTOR_OFFSET_X_MM, sy + y1, gauge + 93.0],
                },
            )
        };
        let head_parts = vec![
            TriMesh::box_mesh("head", &head),
            cylinder_z(
                "spindle",
                sx,
                sy,
                gauge,
                gauge + SPINDLE_HEIGHT_MM,
                SPINDLE_RADIUS_MM,
                24,
            ),
            cylinder_z("holder", sx, sy, zmax, gauge, HOLDER_RADIUS_MM, 24),
            TriMesh::box_mesh("motor", &motor),
            cylinder_z(
                "spindle-pulley",
                sx,
                sy,
                gauge + 84.0,
                gauge + 96.0,
                32.0,
                24,
            ),
            cylinder_z(
                "motor-pulley",
                sx + MOTOR_OFFSET_X_MM,
                sy,
                gauge + 84.0,
                gauge + 96.0,
                25.0,
                24,
            ),
            belt("belt-front", -31.0, -25.0),
            belt("belt-back", 25.0, 31.0),
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
            slab("x-max", [xmax - thickness, ymin, zmax], [xmax, ymax, zmax]),
            slab("y-min", [xmin, ymin, zmin], [xmax, ymin + thickness, zmax]),
            slab("y-max", [xmin, ymax - thickness, zmin], [xmax, ymax, zmax]),
            slab("z-min", [xmin, ymin, zmin], [xmax, ymax, zmin + thickness]),
            slab("z-max", [xmin, ymin, zmax - thickness], [xmax, ymax, zmax]),
        ];

        let mut bounds = parts
            .iter()
            .fold(Bounds::empty(), |acc, part| acc.union(&part.bounds));
        for part in &column_parts {
            for x in [xmin, xmax] {
                for y in [ymin, ymax] {
                    bounds = bounds.union(&translated_bounds(&part.bounds, [x - sx, y - sy, 0.0]));
                }
            }
        }
        for part in &head_parts {
            for x in [xmin, xmax] {
                for y in [ymin, ymax] {
                    for z in [zmin - zmax, 0.0] {
                        bounds =
                            bounds.union(&translated_bounds(&part.bounds, [x - sx, y - sy, z]));
                    }
                }
            }
        }

        Self {
            name: ini.machine_name.clone(),
            travel,
            parts,
            column_parts,
            head_parts,
            reference_gauge,
            limit_planes,
            bounds,
        }
    }

    pub fn machine_bounds(&self) -> Bounds {
        self.bounds
    }

    /// A body part by name, regardless of its motion group.
    pub fn part(&self, name: &str) -> Option<&TriMesh> {
        self.parts
            .iter()
            .chain(&self.column_parts)
            .chain(&self.head_parts)
            .find(|part| part.name == name)
    }

    /// True when the tip leaves travel or the tool body leaves the visual envelope.
    pub fn envelope_violated(&self, tip: P3, body_top: P3) -> bool {
        const EPS: f64 = 1e-9;
        !self.travel.contains(tip, EPS) || !self.bounds.contains(body_top, EPS)
    }
}

fn translated_bounds(bounds: &Bounds, delta: P3) -> Bounds {
    Bounds {
        min: [
            bounds.min[0] + delta[0],
            bounds.min[1] + delta[1],
            bounds.min[2] + delta[2],
        ],
        max: [
            bounds.max[0] + delta[0],
            bounds.max[1] + delta[1],
            bounds.max[2] + delta[2],
        ],
    }
}

/// A capped vertical cylinder with outward-wound triangles.
fn cylinder_z(
    name: &str,
    x: f64,
    y: f64,
    z0: f64,
    z1: f64,
    radius: f64,
    segments: usize,
) -> TriMesh {
    let segments = segments.max(3);
    let mut positions = Vec::with_capacity(segments * 2 + 2);
    for z in [z0, z1] {
        for segment in 0..segments {
            let theta = std::f64::consts::TAU * segment as f64 / segments as f64;
            positions.push([x + radius * theta.cos(), y + radius * theta.sin(), z]);
        }
    }
    let bottom_center = positions.len() as u32;
    positions.push([x, y, z0]);
    let top_center = positions.len() as u32;
    positions.push([x, y, z1]);

    let mut indices = Vec::with_capacity(segments * 4);
    for segment in 0..segments {
        let next = (segment + 1) % segments;
        let low = segment as u32;
        let low_next = next as u32;
        let high = (segments + segment) as u32;
        let high_next = (segments + next) as u32;
        indices.extend([
            [low, low_next, high_next],
            [low, high_next, high],
            [bottom_center, low_next, low],
            [top_center, high, high_next],
        ]);
    }
    TriMesh::new(name, positions, indices)
}

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

    fn machine() -> MachineModel {
        let ini = MachineIni::parse(std::path::Path::new("test.ini"), ini_text()).unwrap();
        MachineModel::from_ini(&ini, &Sim3dConfig::default())
    }

    #[test]
    fn body_follows_the_axis_limits() {
        let machine = machine();
        assert_eq!(machine.name, "Test Mill");
        assert_eq!(machine.travel.min, [-100.0, -80.0, -50.0]);
        assert_eq!(machine.travel.max, [0.0, 2.0, 10.0]);
        assert_eq!(machine.parts.len(), 2);
        assert_eq!(machine.column_parts.len(), 4);
        assert_eq!(machine.head_parts.len(), 8);
        assert_eq!(machine.limit_planes.len(), 6);
        assert_eq!(machine.reference_gauge, [0.0, 0.0, 40.0]);
        for part in machine
            .parts
            .iter()
            .chain(&machine.column_parts)
            .chain(&machine.head_parts)
        {
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
    fn belt_drive_and_spindle_share_the_head_reference() {
        let machine = machine();
        for name in [
            "head",
            "spindle",
            "holder",
            "motor",
            "spindle-pulley",
            "motor-pulley",
            "belt-front",
            "belt-back",
        ] {
            assert!(machine.part(name).is_some(), "missing {name}");
        }
        let gauge = machine.reference_gauge;
        let spindle = machine.part("spindle").unwrap();
        assert_eq!(
            spindle.bounds.min,
            [gauge[0] - 32.0, gauge[1] - 32.0, gauge[2]]
        );
        assert!(machine.part("spindle-pulley").unwrap().triangle_count() > 12);
        assert!(machine.part("motor-pulley").unwrap().triangle_count() > 12);
    }

    #[test]
    fn cylinders_are_closed_and_outward_wound() {
        let mesh = cylinder_z("test", 0.0, 0.0, 0.0, 20.0, 10.0, 16);
        assert_eq!(mesh.triangle_count(), 64);
        for index in 0..mesh.triangle_count() {
            let [a, b, c] = mesh.triangle(index).unwrap();
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let normal = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            let center = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0];
            let radial = normal[0] * center[0] + normal[1] * center[1];
            assert!(radial >= -1e-9, "triangle {index} points inward");
        }
    }

    #[test]
    fn envelope_checks_the_tip_and_the_body() {
        let machine = machine();
        assert!(!machine.envelope_violated([-50.0, -40.0, -50.0], [-50.0, -40.0, -30.0]));
        assert!(machine.envelope_violated([10.0, -40.0, -50.0], [10.0, -40.0, -30.0]));
        assert!(!machine.envelope_violated([0.0, 0.0, 10.0], [0.0, 0.0, 30.0]));
        assert!(machine.envelope_violated([-50.0, -40.0, -500.0], [-50.0, -40.0, -480.0]));
    }
}
