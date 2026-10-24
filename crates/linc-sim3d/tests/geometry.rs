//! Acceptance tests for the linc-sim3d foundation layer.
//!
//! Every distance assertion follows the convention documented in
//! linc_sim3d::types: Contact::distance is the distance between the sphere
//! centre (or capsule segment) and the mesh, minus the radius, so a negative
//! value means penetration. Contact::point is the closest point on the mesh.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use linc_sim3d::mesh::{point_triangle_closest, segment_triangle_closest};
use linc_sim3d::step::{parse_lincmesh, StepImporter};
use linc_sim3d::stl::read_stl;
use linc_sim3d::types::{Bounds, Capsule, Sphere, Transform, TriMesh, P3};
use linc_sim3d::MeshIndex;

const TOL: f64 = 1e-9;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn unit_box() -> TriMesh {
    TriMesh::box_mesh(
        "box",
        &Bounds {
            min: [0.0, 0.0, 0.0],
            max: [10.0, 10.0, 10.0],
        },
    )
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: got {actual}, expected {expected} (tolerance {tolerance})"
    );
}

fn assert_point(actual: P3, expected: P3, tolerance: f64, what: &str) {
    for axis in 0..3 {
        assert_close(
            actual[axis],
            expected[axis],
            tolerance,
            &format!("{what} [{axis}]"),
        );
    }
}

fn assert_bounds(bounds: &Bounds, min: P3, max: P3, tolerance: f64) {
    assert_point(bounds.min, min, tolerance, "bounds.min");
    assert_point(bounds.max, max, tolerance, "bounds.max");
}

/// Signed volume of a closed mesh (positive when triangles wind outwards).
fn signed_volume(mesh: &TriMesh) -> f64 {
    let mut total = 0.0;
    for i in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.triangle(i).expect("triangle");
        let cross = [
            b[1] * c[2] - b[2] * c[1],
            b[2] * c[0] - b[0] * c[2],
            b[0] * c[1] - b[1] * c[0],
        ];
        total += a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2];
    }
    total / 6.0
}

// ---------------------------------------------------------------- meshes

#[test]
fn box_mesh_bounds_and_volume() {
    let mesh = TriMesh::box_mesh(
        "crate",
        &Bounds {
            min: [0.0, 0.0, 0.0],
            max: [20.0, 20.0, 10.0],
        },
    );
    assert_eq!(mesh.name, "crate");
    assert_eq!(mesh.triangle_count(), 12);
    assert_eq!(mesh.vertex_count(), 8);
    assert_bounds(&mesh.bounds, [0.0, 0.0, 0.0], [20.0, 20.0, 10.0], 0.0);
    // 20 x 20 x 10 mm and outward winding, so the signed volume is positive.
    assert_close(signed_volume(&mesh), 4000.0, TOL, "volume");
    assert_close(mesh.bounds.center()[2], 5.0, TOL, "center z");
    assert_point(
        mesh.bounds.half_extent(),
        [10.0, 10.0, 5.0],
        TOL,
        "half extent",
    );
    assert!(mesh.bounds.contains([0.0, 0.0, 0.0], TOL));
    assert!(mesh.bounds.contains([10.0, 10.0, 5.0], TOL));
    assert!(!mesh.bounds.contains([10.0, 10.0, 10.1], TOL));
    assert!(!mesh.bounds.contains([-0.1, 5.0, 5.0], TOL));
    assert_eq!(mesh.triangle(12), None);
    assert!(TriMesh::box_mesh("empty", &Bounds::empty()).is_empty());
}

#[test]
fn bounds_union_expand_intersect() {
    let a = Bounds {
        min: [0.0, 0.0, 0.0],
        max: [10.0, 10.0, 10.0],
    };
    let b = Bounds {
        min: [5.0, -5.0, 5.0],
        max: [15.0, 5.0, 15.0],
    };
    let union = a.union(&b);
    assert_bounds(&union, [0.0, -5.0, 0.0], [15.0, 10.0, 15.0], 0.0);

    let overlap = a.intersect(&b).expect("overlap");
    assert_bounds(&overlap, [5.0, 0.0, 5.0], [10.0, 5.0, 10.0], 0.0);
    assert!(a.intersects(&b));

    let apart = Bounds {
        min: [20.0, 20.0, 20.0],
        max: [21.0, 21.0, 21.0],
    };
    assert!(a.intersect(&apart).is_none());
    assert!(!a.intersects(&apart));

    let mut grown = Bounds::default();
    assert!(grown.is_empty());
    grown.expand([1.0, 2.0, 3.0]);
    grown.expand([-4.0, 5.0, 0.0]);
    assert_bounds(&grown, [-4.0, 2.0, 0.0], [1.0, 5.0, 3.0], 0.0);
    assert_eq!(Bounds::default().union(&a), a);
    assert!(Bounds::default().union(&a).intersects(&a));
}

#[test]
fn transform_round_trip() {
    let transform = Transform::default()
        .with_translation([12.5, -7.25, 3.0])
        .with_rotation([30.0, -45.0, 60.0]);
    assert!(!transform.is_identity());
    assert!(Transform::identity().is_identity());

    for point in [
        [0.0, 0.0, 0.0],
        [1.0, 2.0, 3.0],
        [-5.5, 4.25, -9.0],
        [100.0, -200.0, 300.0],
    ] {
        let placed = transform.apply(point);
        let back = transform.inverse_apply(placed);
        for axis in 0..3 {
            assert_close(back[axis], point[axis], 1e-9, "round trip");
        }
    }

    // A direction ignores the translation.
    let direction = transform.apply_dir([0.0, 0.0, 1.0]);
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    assert_close(length, 1.0, 1e-12, "rotated unit vector length");

    // Rotation order is X, then Y, then Z, extrinsic.
    let only_x = Transform::identity().with_rotation([90.0, 0.0, 0.0]);
    assert_point(
        only_x.apply([0.0, 1.0, 0.0]),
        [0.0, 0.0, 1.0],
        1e-12,
        "Rx(y)",
    );
    let only_y = Transform::identity().with_rotation([0.0, 90.0, 0.0]);
    assert_point(
        only_y.apply([0.0, 0.0, 1.0]),
        [1.0, 0.0, 0.0],
        1e-12,
        "Ry(z)",
    );
    let only_z = Transform::identity().with_rotation([0.0, 0.0, 90.0]);
    assert_point(
        only_z.apply([1.0, 0.0, 0.0]),
        [0.0, 1.0, 0.0],
        1e-12,
        "Rz(x)",
    );

    // X first: Rz*Ry*Rx sends +Z back to -Y. The reverse order would send it to
    // +X, so this pins the composition order down.
    let xy = Transform::identity().with_rotation([90.0, 90.0, 0.0]);
    assert_point(
        xy.apply([0.0, 0.0, 1.0]),
        [0.0, -1.0, 0.0],
        1e-12,
        "Ry * Rx * z",
    );
}

#[test]
fn transformed_mesh_recomputes_bounds() {
    let mesh = unit_box();
    let moved = mesh.transformed(&Transform::identity().with_translation([100.0, 0.0, -50.0]));
    assert_bounds(
        &moved.bounds,
        [100.0, 0.0, -50.0],
        [110.0, 10.0, -40.0],
        1e-12,
    );
    assert_close(signed_volume(&moved), 1000.0, 1e-9, "volume is preserved");
    assert_eq!(moved.name, mesh.name);
    let spun = mesh.transformed(&Transform::identity().with_rotation([0.0, 0.0, 45.0]));
    assert_eq!(spun.triangle_count(), 12);
}

// --------------------------------------------------------------- spheres

#[test]
fn sphere_above_a_face_reports_the_gap() {
    let index = MeshIndex::build(&unit_box());
    let contact = index
        .nearest_sphere(&Sphere {
            center: [5.0, 5.0, 15.0],
            radius: 2.0,
        })
        .expect("contact");
    // centre is 5 mm above the z = 10 face; 5 - 2 = 3.
    assert_close(contact.distance, 3.0, TOL, "gap");
    assert_point(contact.point, [5.0, 5.0, 10.0], TOL, "touch point");
}

#[test]
fn sphere_inside_a_face_reports_negative_depth() {
    let index = MeshIndex::build(&unit_box());
    let contact = index
        .nearest_sphere(&Sphere {
            center: [5.0, 5.0, 11.0],
            radius: 2.0,
        })
        .expect("contact");
    // the centre is 1 mm above the face and the probe reaches 2 mm deeper.
    assert_close(contact.distance, -1.0, TOL, "penetration");
    assert_point(contact.point, [5.0, 5.0, 10.0], TOL, "touch point");

    // Halfway through the face: the centre is 0.5 mm above it.
    let half = index
        .nearest_sphere(&Sphere {
            center: [5.0, 5.0, 9.5],
            radius: 1.0,
        })
        .expect("contact");
    assert_close(half.distance, -0.5, TOL, "halfway through the face");

    // The distance is measured to the nearest surface, not to "inside the
    // solid": a sphere swallowed by the box reports the gap to the nearest wall
    // (5 mm) minus its radius.
    let swallowed = index
        .nearest_sphere(&Sphere {
            center: [5.0, 5.0, 5.0],
            radius: 1.0,
        })
        .expect("contact");
    assert_close(
        swallowed.distance,
        5.0 - 1.0,
        TOL,
        "swallowed, surface-based",
    );
}

#[test]
fn sphere_near_an_edge_and_a_corner() {
    let index = MeshIndex::build(&unit_box());

    let edge = index
        .nearest_sphere(&Sphere {
            center: [13.0, 5.0, 13.0],
            radius: 0.0,
        })
        .expect("contact");
    assert_close(edge.distance, (18.0f64).sqrt(), TOL, "edge distance");
    assert_point(edge.point, [10.0, 5.0, 10.0], TOL, "edge point");

    let corner = index
        .nearest_sphere(&Sphere {
            center: [13.0, 13.0, 13.0],
            radius: 1.0,
        })
        .expect("contact");
    assert_close(
        corner.distance,
        (27.0f64).sqrt() - 1.0,
        TOL,
        "corner distance",
    );
    assert_point(corner.point, [10.0, 10.0, 10.0], TOL, "corner point");
}

// -------------------------------------------------------------- capsules

#[test]
fn capsule_beside_a_box() {
    let index = MeshIndex::build(&unit_box());
    let contact = index
        .nearest_capsule(&Capsule {
            a: [15.0, 5.0, 4.0],
            b: [15.0, 5.0, 6.0],
            radius: 2.0,
        })
        .expect("contact");
    // the axis is 5 mm from the x = 10 face; 5 - 2 = 3.
    assert_close(contact.distance, 3.0, TOL, "side gap");
    assert_close(contact.point[0], 10.0, TOL, "touch point on the face");
    assert_close(contact.point[1], 5.0, 1e-9, "touch point y");
    assert!((4.0 - 1e-9..=6.0 + 1e-9).contains(&contact.point[2]));
}

#[test]
fn capsule_above_and_into_a_face() {
    let index = MeshIndex::build(&unit_box());

    let above = index
        .nearest_capsule(&Capsule {
            a: [5.0, 5.0, 12.0],
            b: [5.0, 5.0, 16.0],
            radius: 1.0,
        })
        .expect("contact");
    assert_close(above.distance, 1.0, TOL, "axis 2 mm above, radius 1");
    assert_point(above.point, [5.0, 5.0, 10.0], TOL, "touch point");

    let pushed = index
        .nearest_capsule(&Capsule {
            a: [5.0, 5.0, 9.0],
            b: [5.0, 5.0, 16.0],
            radius: 1.0,
        })
        .expect("contact");
    // the axis crosses the face, so the surface distance is -radius.
    assert_close(pushed.distance, -1.0, TOL, "penetration");
    assert_close(pushed.point[2], 10.0, 1e-9, "contact point on the face");
}

// ------------------------------------------------------------------- BVH

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let bits = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        bits as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.next_f64()
    }

    fn point(&mut self, low: f64, high: f64) -> P3 {
        [
            self.range(low, high),
            self.range(low, high),
            self.range(low, high),
        ]
    }
}

fn random_mesh(seed: u64, triangles: usize) -> TriMesh {
    let mut rng = Rng(seed);
    let mut positions = Vec::with_capacity(triangles * 3);
    for _ in 0..triangles * 3 {
        positions.push(rng.point(0.0, 100.0));
    }
    let indices = (0..triangles as u32)
        .map(|i| [i * 3, i * 3 + 1, i * 3 + 2])
        .collect();
    TriMesh::new("random", positions, indices)
}

fn brute_force_sphere(mesh: &TriMesh, sphere: &Sphere) -> f64 {
    let mut best = f64::INFINITY;
    for i in 0..mesh.triangle_count() {
        let triangle = mesh.triangle(i).expect("triangle");
        let closest = point_triangle_closest(sphere.center, triangle);
        if closest.distance_sq < best {
            best = closest.distance_sq;
        }
    }
    best.sqrt() - sphere.radius
}

fn brute_force_capsule(mesh: &TriMesh, capsule: &Capsule) -> f64 {
    let mut best = f64::INFINITY;
    for i in 0..mesh.triangle_count() {
        let triangle = mesh.triangle(i).expect("triangle");
        let closest = segment_triangle_closest(capsule.a, capsule.b, triangle);
        if closest.distance_sq < best {
            best = closest.distance_sq;
        }
    }
    best.sqrt() - capsule.radius
}

#[test]
fn bvh_matches_brute_force_for_spheres() {
    let mesh = random_mesh(0x5EED_1234, 2000);
    let index = MeshIndex::build(&mesh);
    assert_eq!(index.triangle_count(), 2000);
    assert_eq!(index.bounds(), mesh.bounds);
    assert_eq!(index.mesh().name, mesh.name);

    let mut rng = Rng(0xC0FF_EE01);
    for query in 0..200 {
        let sphere = Sphere {
            center: rng.point(-20.0, 120.0),
            radius: rng.range(0.0, 5.0),
        };
        let expected = brute_force_sphere(&mesh, &sphere);
        let contact = index.nearest_sphere(&sphere).expect("contact");
        assert_close(contact.distance, expected, TOL, &format!("sphere {query}"));
        // The reported point really is the closest point at that distance.
        let dx = contact.point[0] - sphere.center[0];
        let dy = contact.point[1] - sphere.center[1];
        let dz = contact.point[2] - sphere.center[2];
        let to_point = (dx * dx + dy * dy + dz * dz).sqrt();
        assert_close(
            to_point - sphere.radius,
            contact.distance,
            1e-9,
            &format!("sphere {query} point"),
        );
    }
}

#[test]
fn bvh_matches_brute_force_for_capsules() {
    let mesh = random_mesh(0x5EED_5678, 2000);
    let index = MeshIndex::build(&mesh);

    let mut rng = Rng(0xC0FF_EE02);
    for query in 0..32 {
        let capsule = Capsule {
            a: rng.point(-20.0, 120.0),
            b: rng.point(-20.0, 120.0),
            radius: rng.range(0.0, 5.0),
        };
        let expected = brute_force_capsule(&mesh, &capsule);
        let contact = index.nearest_capsule(&capsule).expect("contact");
        assert_close(contact.distance, expected, TOL, &format!("capsule {query}"));
    }
}

#[test]
fn empty_mesh_has_no_contact() {
    let index = MeshIndex::build(&TriMesh::new("none", Vec::new(), Vec::new()));
    assert_eq!(
        index.nearest_sphere(&Sphere {
            center: [0.0; 3],
            radius: 1.0
        }),
        None
    );
    assert_eq!(
        index.nearest_capsule(&Capsule {
            a: [0.0; 3],
            b: [1.0, 0.0, 0.0],
            radius: 1.0
        }),
        None
    );
}

// ------------------------------------------------------------------- STL

#[test]
fn stl_ascii_and_binary_agree() {
    let ascii = read_stl(&fixture("tetra_ascii.stl")).expect("ascii stl");
    let binary = read_stl(&fixture("tetra_bin.stl")).expect("binary stl");

    assert_eq!(ascii.triangle_count(), 4);
    assert_eq!(binary.triangle_count(), 4);
    assert_point(ascii.bounds.min, [0.0, 0.0, 0.0], 1e-6, "ascii min");
    assert_point(ascii.bounds.max, [10.0, 10.0, 10.0], 1e-6, "ascii max");
    assert_point(binary.bounds.min, ascii.bounds.min, 1e-9, "binary min");
    assert_point(binary.bounds.max, ascii.bounds.max, 1e-9, "binary max");

    // Binary STL stores f32, so compare at f32 precision.
    for i in 0..ascii.triangle_count() {
        let a = ascii.triangle(i).expect("ascii triangle");
        let b = binary.triangle(i).expect("binary triangle");
        for corner in 0..3 {
            assert_point(b[corner], a[corner], 1e-6, "same geometry");
        }
    }
    // The tetrahedron is closed: 4 triangles, 3 vertices each, no duplicates.
    assert_eq!(ascii.vertex_count(), 12);
}

// -------------------------------------------------------------- lincmesh

#[test]
fn lincmesh_fixture_parses() {
    let text = fs::read_to_string(fixture("box.lincmesh")).expect("fixture");
    let mesh = parse_lincmesh(&text, "box").expect("parses");
    assert_eq!(mesh.triangle_count(), 12);
    assert_eq!(
        mesh.vertex_count(),
        36,
        "the helper emits three vertices per triangle"
    );
    assert_bounds(&mesh.bounds, [0.0, 0.0, 0.0], [20.0, 20.0, 10.0], 1e-6);
    assert_close(signed_volume(&mesh), 4000.0, 1e-6, "volume");
}

// ------------------------------------------------------------------ STEP

fn cache_entries(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .expect("cache dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|ext| ext == "lincmesh")
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
}

#[test]
#[ignore = "needs the OCCT helper built at compile time, or freecadcmd"]
fn step_import_box_fixture_and_cache() {
    let importer = StepImporter::discover();
    if importer.backends().is_empty() {
        eprintln!("skipping: no STEP backend available");
        return;
    }
    let cache = tempfile::tempdir().expect("temp cache");
    let importer = importer.with_cache_dir(cache.path().to_path_buf());
    eprintln!(
        "env LINC_SIM3D_STEP_HELPER={:?}",
        std::env::var_os("LINC_SIM3D_STEP_HELPER")
    );
    eprintln!(
        "backends: helper={:?} freecad={:?}",
        importer.helper(),
        importer.freecad()
    );

    let first = importer.import(&fixture("box.step")).expect("first import");
    assert!(
        first.triangle_count() >= 12,
        "{} triangles",
        first.triangle_count()
    );
    assert_bounds(&first.bounds, [0.0, 0.0, 0.0], [20.0, 20.0, 10.0], 1e-3);
    eprintln!(
        "box.step: {} triangles, bounds {:?}..{:?}",
        first.triangle_count(),
        first.bounds.min,
        first.bounds.max
    );

    let entries = cache_entries(cache.path());
    assert_eq!(entries.len(), 1, "one cache file expected, got {entries:?}");
    let before = fs::metadata(&entries[0]).expect("cache metadata");
    let before_modified = before.modified().expect("mtime");

    std::thread::sleep(Duration::from_millis(30));
    let second = importer
        .import(&fixture("box.step"))
        .expect("cached import");
    assert_eq!(second.triangle_count(), first.triangle_count());
    assert_bounds(&second.bounds, first.bounds.min, first.bounds.max, 0.0);

    let after_modified = fs::metadata(&entries[0])
        .expect("cache metadata")
        .modified()
        .expect("mtime");
    assert_eq!(
        before_modified, after_modified,
        "the second import rewrote the cache instead of hitting it"
    );
}

#[test]
#[ignore = "runs freecadcmd once, a few seconds"]
fn step_import_box_fixture_through_freecadcmd() {
    let importer = StepImporter::discover()
        .with_helper(Some(PathBuf::from("/nonexistent/linc-sim3d-step-helper")));
    let Some(freecad) = importer.freecad().map(|path| path.to_path_buf()) else {
        eprintln!("skipping: freecadcmd is not installed");
        return;
    };
    let cache = tempfile::tempdir().expect("temp cache");
    let importer = importer.with_cache_dir(cache.path().to_path_buf());
    eprintln!("freecadcmd at {}", freecad.display());

    let mesh = importer
        .import(&fixture("box.step"))
        .expect("freecadcmd import");
    assert!(
        mesh.triangle_count() >= 12,
        "{} triangles",
        mesh.triangle_count()
    );
    assert_bounds(&mesh.bounds, [0.0, 0.0, 0.0], [20.0, 20.0, 10.0], 1e-3);
    eprintln!(
        "box.step via freecadcmd: {} triangles, bounds {:?}..{:?}",
        mesh.triangle_count(),
        mesh.bounds.min,
        mesh.bounds.max
    );
    assert_eq!(cache_entries(cache.path()).len(), 1);
}

#[test]
#[ignore = "imports $LINC_SIM3D_IMPORT_PATH; set it to a real STEP file"]
fn step_import_any_path() {
    let Some(path) = std::env::var_os("LINC_SIM3D_IMPORT_PATH") else {
        eprintln!("skipping: LINC_SIM3D_IMPORT_PATH is not set");
        return;
    };
    let path = PathBuf::from(path);
    let importer = StepImporter::discover();
    if importer.backends().is_empty() {
        eprintln!("skipping: no STEP backend available");
        return;
    }
    let cache = tempfile::tempdir().expect("temp cache");
    let importer = importer.with_cache_dir(cache.path().to_path_buf());
    let started = std::time::Instant::now();
    let mesh = importer.import(&path).expect("import");
    eprintln!(
        "{}: {} triangles, bounds {:?}..{:?}, {:?}",
        path.display(),
        mesh.triangle_count(),
        mesh.bounds.min,
        mesh.bounds.max,
        started.elapsed()
    );
    assert!(mesh.triangle_count() > 0);
}
