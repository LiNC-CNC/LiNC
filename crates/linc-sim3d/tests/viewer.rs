//! Acceptance tests for the window: the camera, the baked scene, the tool
//! geometry and the panel's pure helpers.
//!
//! Everything here runs without a display: egui meshes are plain vertex and
//! index lists, so the projection, the culling and the baked lighting are
//! checked directly.

use std::path::PathBuf;

use linc_sim3d::config::Sim3dConfig;
use linc_sim3d::model::SimModel;
use linc_sim3d::tools::{ActiveTool, CutterShape};
use linc_sim3d::types::{Bounds, TriMesh, P3};
use linc_sim3d::ui;
use linc_sim3d::viewer::{self, Camera, Projector, Scene, Viewer};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    manifest_dir().join("tests").join("fixtures").join(name)
}

/// The fixture machine: travel -372..5, -346..4, -253..19, no workpieces, so
/// the scene is the machine body alone and no STEP backend is needed.
fn fixture_config() -> Sim3dConfig {
    let mut config = Sim3dConfig::default();
    config.machine.ini = fixture("model_machine.ini");
    config
}

fn box_mesh() -> TriMesh {
    TriMesh::box_mesh(
        "box",
        &Bounds {
            min: [0.0, 0.0, 0.0],
            max: [20.0, 20.0, 10.0],
        },
    )
}

fn probe_tool() -> ActiveTool {
    ActiveTool {
        number: 99,
        z_offset: -30.0,
        tip_radius: 1.5,
        shape: CutterShape::Probe { tip_radius: 1.5 },
        body_radius: 1.0,
        length: 30.0,
    }
}

fn flat_tool() -> ActiveTool {
    ActiveTool {
        number: 1,
        z_offset: -56.0,
        tip_radius: 3.0,
        shape: CutterShape::Flat { diameter: 6.0 },
        body_radius: 3.0,
        length: 20.0,
    }
}

fn radial_distance(point: P3, axis: P3) -> f64 {
    let (dx, dy) = (point[0] - axis[0], point[1] - axis[1]);
    (dx * dx + dy * dy).sqrt()
}

// ------------------------------------------------------------------ camera

#[test]
fn fit_frames_every_corner_of_the_box() {
    let bounds = Bounds {
        min: [-367.0, -341.0, -248.0],
        max: [2.0, 1.0, 16.0],
    };
    let mut camera = Camera::iso();
    camera.fit(&bounds);
    assert_eq!(camera.target, bounds.center(), "fit centres on the box");

    let viewport = egui::Rect::from_min_size(egui::Pos2::new(40.0, 20.0), egui::vec2(900.0, 600.0));
    let projector = Projector::new(&camera, viewport);
    for corner in corners(&bounds) {
        let (pos, depth) = projector
            .project(corner)
            .unwrap_or_else(|| panic!("corner {corner:?} is behind the camera"));
        assert!(depth > 0.0);
        assert!(
            viewport.contains(pos),
            "corner {corner:?} projects to {pos:?}, outside {viewport:?}"
        );
    }
}

#[test]
fn fit_of_an_empty_box_leaves_the_camera_alone() {
    let mut camera = Camera::iso();
    let before = camera;
    camera.fit(&Bounds::empty());
    assert_eq!(camera, before);
}

#[test]
fn orbit_clamps_the_pitch_and_wraps_the_yaw() {
    let mut camera = Camera::iso();
    camera.orbit(0.0, 1000.0);
    assert!(
        camera.pitch_deg <= 88.0,
        "pitch {} ran away",
        camera.pitch_deg
    );
    camera.orbit(0.0, -1000.0);
    assert!(
        camera.pitch_deg >= -88.0,
        "pitch {} ran away",
        camera.pitch_deg
    );
    camera.orbit(720.0 + 30.0, 0.0);
    assert!(
        (-180.0..=180.0).contains(&camera.yaw_deg),
        "yaw {} is not wrapped",
        camera.yaw_deg
    );
}

#[test]
fn zoom_keeps_the_distance_positive_and_bounded() {
    let mut camera = Camera::iso();
    let start = camera.distance_mm;
    camera.zoom(10_000.0);
    assert!(camera.distance_mm > 0.0);
    assert!(camera.distance_mm < start, "a positive scroll zooms in");
    camera.zoom(-10_000.0);
    assert!(camera.distance_mm.is_finite());
    assert!(
        camera.distance_mm <= 100_000.0,
        "distance {} is not clamped",
        camera.distance_mm
    );
}

#[test]
fn points_behind_the_eye_are_dropped() {
    let mut camera = Camera::iso();
    camera.fit(&Bounds {
        min: [0.0, 0.0, 0.0],
        max: [10.0, 10.0, 10.0],
    });
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let projector = Projector::new(&camera, viewport);
    assert!(projector.project(camera.target).is_some());

    let eye = camera.eye();
    let behind = [
        eye[0] + (eye[0] - camera.target[0]),
        eye[1] + (eye[1] - camera.target[1]),
        eye[2] + (eye[2] - camera.target[2]),
    ];
    assert!(
        projector.project(behind).is_none(),
        "a point behind the eye must not project"
    );
    assert!(projector.view_depth(behind) < 0.0);
    assert!(projector.view_depth(camera.target) > 0.0);
}

fn corners(bounds: &Bounds) -> Vec<P3> {
    let [x0, y0, z0] = bounds.min;
    let [x1, y1, z1] = bounds.max;
    let mut out = Vec::new();
    for x in [x0, x1] {
        for y in [y0, y1] {
            for z in [z0, z1] {
                out.push([x, y, z]);
            }
        }
    }
    out
}

// ------------------------------------------------------------------- scene

#[test]
fn scene_bakes_the_body_grid_limit_planes_and_triad() {
    let config = fixture_config();
    let model = SimModel::from_config(&config).expect("fixture model");
    let machine = model.machine();
    let scene = Scene::build(&model);

    let fixed_triangles: usize = machine.parts.iter().map(|part| part.triangle_count()).sum();
    assert_eq!(scene.triangle_count(), fixed_triangles);
    let gauge = machine.reference_gauge;
    let column = scene.column_at(gauge);
    let head = scene.head_at(gauge);
    assert_eq!(
        column.len(),
        machine
            .column_parts
            .iter()
            .map(|part| part.triangle_count())
            .sum::<usize>()
    );
    assert_eq!(
        head.len(),
        machine
            .head_parts
            .iter()
            .map(|part| part.triangle_count())
            .sum::<usize>()
    );
    assert!(
        scene
            .tris
            .iter()
            .chain(&column)
            .chain(&head)
            .all(|tri| tri.color.a() == 255),
        "machine surfaces must be opaque"
    );

    assert!(!scene.grid.is_empty(), "the table grid is drawn");
    assert_eq!(scene.axes.len(), 3, "one line per axis");
    let origin = [0.0, 0.0, 0.0];
    assert!(scene.axes.iter().all(|axis| axis.a == origin));
    assert!(
        scene.outlines.len() >= machine.limit_planes.len() * 12,
        "every limit plane contributes its box outline"
    );
    assert_eq!(scene.labels.len(), 3, "X, Y and Z are labelled");
    for name in ["X", "Y", "Z"] {
        assert!(
            scene
                .labels
                .iter()
                .any(|label| label.text.starts_with(name)),
            "{name} has no label: {:?}",
            scene.labels.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }
    assert_eq!(scene.bounds, model.machine_bounds());
    assert!(!scene.is_empty());
}

#[test]
fn moving_assemblies_follow_the_live_gauge_line() {
    let model = SimModel::from_config(&fixture_config()).expect("fixture model");
    let scene = Scene::build(&model);
    let reference = model.machine().reference_gauge;
    let gauge = [
        reference[0] + 32.0,
        reference[1] - 24.0,
        reference[2] - 60.0,
    ];

    fn check(before: &[viewer::Tri], after: &[viewer::Tri], delta: P3) {
        assert_eq!(before.len(), after.len());
        for (source, moved) in before.iter().zip(after) {
            for axis in 0..3 {
                for (a, b) in [
                    (source.a, moved.a),
                    (source.b, moved.b),
                    (source.c, moved.c),
                ] {
                    assert!((b[axis] - a[axis] - delta[axis]).abs() < 1e-9);
                }
            }
            assert_eq!(moved.normal, source.normal);
            assert_eq!(moved.color, source.color);
        }
    }

    check(
        &scene.column_at(reference),
        &scene.column_at(gauge),
        [32.0, -24.0, 0.0],
    );
    check(
        &scene.head_at(reference),
        &scene.head_at(gauge),
        [32.0, -24.0, -60.0],
    );
}

#[test]
fn the_moving_column_and_head_start_hidden() {
    let model = SimModel::from_config(&fixture_config()).expect("fixture model");
    let scene = Scene::build(&model);
    let gauge = model.machine().reference_gauge;
    let column = scene.column_at(gauge).len();
    let head = scene.head_at(gauge).len();
    assert!(column > 0 && head > 0, "the fixture has both assemblies");

    let mut viewer = Viewer::new(scene);
    assert!(
        viewer.moving_machine_at(gauge).is_empty(),
        "the tool must be unobstructed by the column and head by default"
    );
    viewer.show_column = true;
    assert_eq!(viewer.moving_machine_at(gauge).len(), column);
    viewer.show_head = true;
    assert_eq!(viewer.moving_machine_at(gauge).len(), column + head);
}

#[test]
fn grid_spacing_is_round_and_covers_the_table() {
    // A round step at least a twelfth of the table across, so the grid is
    // readable at any machine size.
    for extent in [0.0, 10.0, 120.0, 369.0, 3000.0] {
        let spacing = viewer::grid_spacing(extent);
        assert!(spacing >= (extent / 12.0).max(1.0), "too fine for {extent}");
        assert!(
            [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 500.0, 1000.0].contains(&spacing),
            "{spacing} is not a round step"
        );
    }
    assert_eq!(viewer::grid_spacing(369.0), 50.0);
}

#[test]
fn back_faces_are_culled_and_faces_toward_the_eye_are_kept() {
    let mesh = box_mesh();
    let tris = viewer::mesh_tris(&mesh, egui::Color32::from_rgb(0x9c, 0xcf, 0xf2));
    assert_eq!(tris.len(), 12, "a box bakes into 12 triangles");
    for tri in &tris {
        let centroid = tri.centroid();
        let center = mesh.bounds.center();
        let outward = (0..3)
            .map(|axis| (centroid[axis] - center[axis]) * tri.normal[axis])
            .sum::<f64>();
        assert!(
            outward >= -1e-9,
            "normal {:?} points into the box",
            tri.normal
        );
    }

    let mut camera = Camera::iso();
    camera.fit(&mesh.bounds);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0));
    let mesh3d = viewer::screen_mesh_of(&tris, &Projector::new(&camera, viewport));
    let visible = mesh3d.indices.len() / 3;
    assert!(visible > 0, "nothing visible from the iso view");
    assert!(
        visible < 12,
        "the far side must be culled, got {visible} of 12"
    );
    assert_eq!(mesh3d.vertices.len(), visible * 3);
    assert_eq!(mesh3d.indices.len() % 3, 0);

    // From inside the box every normal faces away from the eye.
    camera.target = mesh.bounds.center();
    camera.distance_mm = 1.0;
    let inside = viewer::screen_mesh_of(&tris, &Projector::new(&camera, viewport));
    assert!(
        inside.vertices.is_empty(),
        "an eye inside a closed box sees nothing, got {}",
        inside.vertices.len()
    );
}

#[test]
fn screen_mesh_is_sorted_far_to_near() {
    let mesh = box_mesh();
    let tris = viewer::mesh_tris(&mesh, egui::Color32::from_rgb(0x80, 0x80, 0x80));
    let mut camera = Camera::iso();
    camera.fit(&mesh.bounds);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0));
    let projector = Projector::new(&camera, viewport);

    // The first triangle of the mesh must be the farthest of the visible ones,
    // because egui's painter's algorithm draws in order.
    let mut depths: Vec<f64> = tris
        .iter()
        .filter(|tri| {
            (0..3)
                .map(|axis| (tri.centroid()[axis] - projector.eye()[axis]) * tri.normal[axis])
                .sum::<f64>()
                < 0.0
        })
        .map(|tri| projector.view_depth(tri.centroid()))
        .collect();
    depths.sort_by(|left, right| right.total_cmp(left));
    assert!(depths.len() >= 2);
    assert!(depths[0] > depths[depths.len() - 1]);
}

#[test]
fn flat_lighting_is_baked_into_the_vertex_colours() {
    let mesh = box_mesh();
    let base = egui::Color32::from_rgb(0x80, 0x80, 0x80);
    let tris = viewer::mesh_tris(&mesh, base);
    let up = tris
        .iter()
        .find(|tri| tri.normal[2] > 0.99)
        .expect("a top face");
    let down = tris
        .iter()
        .find(|tri| tri.normal[2] < -0.99)
        .expect("a bottom face");
    assert!(
        up.color.r() > down.color.r(),
        "the lit top face ({}) must be brighter than the dark bottom face ({})",
        up.color.r(),
        down.color.r()
    );
    assert!(
        down.color.r() > 0,
        "ambient light keeps an unlit face visible"
    );
    assert_eq!(viewer::lit(base, [0.0, 0.0, 1.0]).r(), up.color.r());
}

// -------------------------------------------------------------------- tool

#[test]
fn the_probe_is_a_ball_with_a_stylus_above_it() {
    let tip = [-100.0, -120.0, -200.0];
    let tris = viewer::tool_tris(&probe_tool(), tip, false);
    assert!(!tris.is_empty());
    let lowest = tris
        .iter()
        .flat_map(|tri| [tri.a[2], tri.b[2], tri.c[2]])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (lowest - (tip[2] - 1.5)).abs() < 0.01,
        "the ball must reach one radius below the tip, got {lowest}"
    );
    let widest = tris
        .iter()
        .flat_map(|tri| [tri.a, tri.b, tri.c])
        .map(|point| radial_distance(point, tip))
        .fold(0.0, f64::max);
    assert!(
        (widest - 1.5).abs() < 0.05,
        "the ball radius is 1.5 mm, got {widest}"
    );
    let highest = tris
        .iter()
        .flat_map(|tri| [tri.a[2], tri.b[2], tri.c[2]])
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        highest <= tip[2] + 30.0 + 1e-6,
        "the stylus ends at the configured length, got {highest}"
    );
}

#[test]
fn a_flat_cutter_is_a_cylinder_of_its_diameter() {
    let tip = [10.0, -20.0, -150.0];
    let tris = viewer::tool_tris(&flat_tool(), tip, false);
    let lowest = tris
        .iter()
        .flat_map(|tri| [tri.a[2], tri.b[2], tri.c[2]])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (lowest - tip[2]).abs() < 1e-6,
        "a flat cutter's bottom is the tip, got {lowest}"
    );
    let widest = tris
        .iter()
        .flat_map(|tri| [tri.a, tri.b, tri.c])
        .map(|point| radial_distance(point, tip))
        .fold(0.0, f64::max);
    assert!(
        (widest - 3.0).abs() < 0.05,
        "a 6 mm cutter is 3 mm in radius, got {widest}"
    );
}

#[test]
fn a_colliding_tool_is_red() {
    let tip = [0.0, 0.0, 0.0];
    let steel = viewer::tool_tris(&flat_tool(), tip, false);
    let red = viewer::tool_tris(&flat_tool(), tip, true);
    assert_ne!(steel[0].color, red[0].color);
    let red_like = red
        .iter()
        .all(|tri| tri.color.r() > tri.color.g() && tri.color.r() > tri.color.b());
    assert!(red_like, "a colliding tool is painted red");
}

// --------------------------------------------------------------------- ui

#[test]
fn expand_path_takes_tilde_and_keeps_relative_paths() {
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert_eq!(
            ui::expand_path("~/parts/a.step"),
            PathBuf::from(&home).join("parts/a.step")
        );
        assert_eq!(ui::expand_path("~"), PathBuf::from(&home));
    }
    assert_eq!(
        ui::expand_path("  ../parts/a.step  "),
        PathBuf::from("../parts/a.step")
    );
    assert_eq!(ui::expand_path("/abs/b.step"), PathBuf::from("/abs/b.step"));
}

#[test]
fn status_labels_name_the_tool_and_the_collision_target() {
    assert_eq!(
        ui::shape_label(&CutterShape::Probe { tip_radius: 1.5 }),
        "probe ball R1.50"
    );
    assert_eq!(
        ui::shape_label(&CutterShape::Flat { diameter: 6.0 }),
        "flat D6.00"
    );
    assert_eq!(
        ui::target_label(Some(linc_sim3d::model::CollisionTarget::Workpiece(2))),
        "workpiece 2"
    );
    assert_eq!(
        ui::target_label(Some(linc_sim3d::model::CollisionTarget::Machine)),
        "machine envelope"
    );
    assert_eq!(ui::target_label(None), "collision");
    let tip = ui::format_tip([1.5, -2.0, 0.0]);
    assert!(tip.ends_with(" mm"), "the tip is in millimetres: {tip:?}");
    assert!(tip.contains("1.500") && tip.contains("-2.000") && tip.contains("0.000"));
}
