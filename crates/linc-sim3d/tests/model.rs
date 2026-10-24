//! Acceptance tests for the machine/workpiece model of linc-sim3d.
//!
//! Contact distances follow the convention in linc_sim3d::types: the reported
//! distance is measured from the ball centre or capsule segment to the mesh
//! and then reduced by the radius, so negative means penetration. The probe
//! tip is the centre of the probe ball.
//!
//! Machine tests read configs/sim.ini directly. Its soft limits sit inside the
//! mechanical travel, so the assertions compare against MachineIni::limits_for
//! rather than against copied numbers.

use std::fs;
use std::path::{Path, PathBuf};

use linc_core::ini::MachineIni;
use linc_sim3d::config::{CutterConfig, CutterKind, Sim3dConfig, WorkpieceEntry};
use linc_sim3d::model::{Action, CollisionTarget, ModelError, SharedState, SimModel, Snapshot};
use linc_sim3d::tools::{self, CutterShape};
use linc_sim3d::types::Transform;

const TOL: f64 = 1e-9;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    manifest_dir().join("tests").join("fixtures").join(name)
}

fn repo_root() -> PathBuf {
    manifest_dir().join("../..")
}

/// A config on the model fixture INI, whose travel is -372/5, -346/4, -253/19.
fn fixture_config() -> Sim3dConfig {
    let mut config = Sim3dConfig::default();
    config.machine.ini = fixture("model_machine.ini");
    config
}

/// The fixture config with a trigger slack that absorbs the last bit of
/// floating point; the tangent test needs it.
fn contact_config() -> Sim3dConfig {
    let mut config = fixture_config();
    config.probe.trigger_slack_mm = TOL;
    config.cutters = vec![
        CutterConfig {
            tool: 1,
            kind: CutterKind::Flat,
            corner_radius_mm: None,
        },
        CutterConfig {
            tool: 2,
            kind: CutterKind::Ball,
            corner_radius_mm: None,
        },
    ];
    config
}

/// The fixture model with tests/fixtures/box.step (20 x 20 x 10 mm) placed so
/// its top face sits at machine Z = -100.
///
/// The STEP import needs an OCCT helper or freecadcmd; without either the test
/// reports why it skipped, the same way the geometry suite does.
fn box_model(config: &Sim3dConfig) -> Option<SimModel> {
    let mut model = SimModel::from_config(config).expect("model from fixture config");
    let transform = Transform::identity().with_translation([0.0, 0.0, -110.0]);
    match model.apply(Action::AddWorkpiece {
        path: fixture("box.step"),
        transform,
    }) {
        Ok(()) => {
            let mesh = &model.workpieces()[0].mesh;
            eprintln!(
                "box.step: {} triangles, {} vertices, bounds {:?}..{:?}",
                mesh.triangle_count(),
                mesh.vertex_count(),
                mesh.bounds.min,
                mesh.bounds.max
            );
            Some(model)
        }
        Err(error) => {
            eprintln!("skipping: cannot import box.step: {error}");
            None
        }
    }
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= TOL,
        "{what}: got {actual}, expected {expected}"
    );
}

// ------------------------------------------------------------- config

#[test]
fn config_round_trip_resolves_relative_paths() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sim3d.toml");
    fs::write(
        &path,
        "[machine]\nini = \"sim.ini\"\n\n\
         [[workpieces]]\nname = \"block\"\npath = \"parts/block.step\"\n\
         translate_mm = [1, 2, 3]\n",
    )
    .expect("write config");

    let config = Sim3dConfig::load(&path).expect("load config");
    assert_eq!(config.machine_ini_path(), dir.path().join("sim.ini"));
    assert_eq!(
        config.workpiece_path(&config.workpieces[0]),
        dir.path().join("parts/block.step")
    );

    config.save(&path).expect("save config");
    let reparsed = Sim3dConfig::load(&path).expect("reload config");
    assert_eq!(config, reparsed, "the saved config must reparse equal");
    assert_eq!(
        reparsed.workpieces[0].path,
        PathBuf::from("parts/block.step"),
        "relative paths stay relative in the file"
    );
    assert_eq!(reparsed.workpieces[0].translate_mm, [1.0, 2.0, 3.0]);
    assert!(reparsed.workpieces[0].visible);
}

#[test]
fn config_defaults_and_missing_file_error() {
    assert_eq!(
        Sim3dConfig::default_path(),
        PathBuf::from("configs/sim3d.toml")
    );
    let error = Sim3dConfig::load(Path::new("/nonexistent/model_sim3d.toml"))
        .expect_err("missing config must fail");
    assert!(
        error.to_string().contains("/nonexistent/model_sim3d.toml"),
        "error must name the path: {error}"
    );
}

#[test]
fn shipped_sim3d_toml_loads_and_matches_the_ini() {
    let path = repo_root().join("configs/sim3d.toml");
    let config = Sim3dConfig::load(&path).expect("configs/sim3d.toml must load");
    assert_eq!(
        config.machine_ini_path(),
        repo_root().join("configs/sim.ini")
    );
    assert_eq!(config.probe.tool_number, 99);
    assert!(
        config.workpieces.is_empty(),
        "the shipped file keeps its workpieces commented out"
    );
    assert_eq!(config.hal.component, "linc-sim3d");

    let ini = MachineIni::parse_file(&config.machine_ini_path()).expect("configs/sim.ini");
    let model = SimModel::from_config(&config).expect("model from the shipped config");
    let travel = model.machine().travel;
    let (xmin, xmax) = ini.limits_for("X").expect("X limits");
    let (ymin, ymax) = ini.limits_for("Y").expect("Y limits");
    let (zmin, zmax) = ini.limits_for("Z").expect("Z limits");
    assert_eq!(travel.min, [xmin, ymin, zmin]);
    assert_eq!(travel.max, [xmax, ymax, zmax]);
    assert_close(model.probe().tip_radius, 1.5, "T99 D+3 ball radius");
}

// ------------------------------------------------------------ machine

#[test]
fn machine_body_matches_the_ini_limits() {
    let mut config = Sim3dConfig::default();
    config.machine.ini = repo_root().join("configs/sim.ini");
    let ini = MachineIni::parse_file(&config.machine_ini_path()).expect("configs/sim.ini");
    let model = SimModel::from_config(&config).expect("model from configs/sim.ini");
    let travel = model.machine().travel;
    let (xmin, xmax) = ini.limits_for("X").expect("X limits");
    let (ymin, ymax) = ini.limits_for("Y").expect("Y limits");
    let (zmin, zmax) = ini.limits_for("Z").expect("Z limits");
    eprintln!(
        "configs/sim.ini travel: X {xmin}..{xmax}, Y {ymin}..{ymax}, Z {zmin}..{zmax}; body {:?}..{:?}",
        model.machine_bounds().min,
        model.machine_bounds().max
    );
    assert_eq!(travel.min, [xmin, ymin, zmin]);
    assert_eq!(travel.max, [xmax, ymax, zmax]);

    let machine = model.machine();
    assert_eq!(
        machine
            .parts
            .iter()
            .map(|part| part.name.as_str())
            .collect::<Vec<_>>(),
        vec!["base", "table", "column", "head", "spindle", "holder"]
    );
    assert_eq!(machine.limit_planes.len(), 6);
    for part in &machine.parts {
        assert!(!part.is_empty(), "{} has no triangles", part.name);
        assert!(
            machine.machine_bounds().contains(part.bounds.min, TOL)
                && machine.machine_bounds().contains(part.bounds.max, TOL),
            "{} leaves the machine bounds",
            part.name
        );
    }
    for (name, plane) in &machine.limit_planes {
        assert!(!plane.is_empty(), "limit plane {name} has no triangles");
    }
}

#[test]
fn machine_body_follows_the_fixture_limits() {
    let model = SimModel::from_config(&fixture_config()).expect("fixture model");
    let travel = model.machine().travel;
    assert_eq!(travel.min, [-372.0, -346.0, -253.0]);
    assert_eq!(travel.max, [5.0, 4.0, 19.0]);

    // The spindle stands at the machine origin, the table spans the travel.
    let spindle = model.machine().part("spindle").expect("spindle");
    assert!(spindle.bounds.contains([0.0, 0.0, 19.0], TOL));
    let table = model.machine().part("table").expect("table");
    assert!(
        table.bounds.max[2] < -253.0,
        "the table sits below the travel"
    );
    for part in &model.machine().parts {
        assert!(!part.is_empty(), "{} has no triangles", part.name);
    }
}

// ------------------------------------------------------------- probe

#[test]
fn probe_contact_gap_touch_and_penetration() {
    let config = contact_config();
    let Some(model) = box_model(&config) else {
        return;
    };
    assert_close(model.probe().tip_radius, 1.5, "T99 D+3 ball radius");

    // Ball bottom at -99.5, part top at -100: 0.5 mm of air.
    let free = model.probe_contact([10.0, 10.0, -98.0]);
    assert!(
        !free.touching,
        "0.5 mm above the part is not contact: {free:?}"
    );
    assert_close(free.distance, 0.5, "gap");
    assert_eq!(free.workpiece, Some(0));

    // 0.1 mm of the ball through the top face.
    let touching = model.probe_contact([10.0, 10.0, -98.6]);
    assert!(
        touching.touching,
        "0.1 mm penetration must touch: {touching:?}"
    );
    assert_close(touching.distance, -0.1, "first contact depth");
    assert_eq!(touching.workpiece, Some(0));
    assert_close(touching.point[0], 10.0, "contact point x");
    assert_close(touching.point[1], 10.0, "contact point y");
    assert_close(touching.point[2], -100.0, "contact point z");

    // Another 0.5 mm into the part stays negative.
    let inside = model.probe_contact([10.0, 10.0, -99.0]);
    assert!(inside.touching);
    assert!(inside.distance < 0.0, "penetration must be negative");
    assert_close(inside.distance, -0.5, "penetration depth");
}

#[test]
fn probe_contact_side_tangent_and_far_miss() {
    let config = contact_config();
    let Some(model) = box_model(&config) else {
        return;
    };

    // Ball tangent to the +X face at x = 20: centre at 21.5.
    let tangent = model.probe_contact([21.5, 10.0, -105.0]);
    assert!(tangent.touching, "tangent must touch: {tangent:?}");
    assert!(
        tangent.distance.abs() <= TOL,
        "tangent distance must be zero: {tangent:?}"
    );
    assert_close(tangent.point[0], 20.0, "tangent point x");

    let miss = model.probe_contact([200.0, 200.0, -105.0]);
    assert!(!miss.touching);
    assert!(
        miss.distance > 100.0,
        "a far miss reports the gap: {miss:?}"
    );
    assert_eq!(miss.workpiece, Some(0), "still reports the nearest part");
}

// ------------------------------------------------------------ cutter

#[test]
fn cutter_contact_hits_a_workpiece_and_the_probe_does_not() {
    let config = contact_config();
    let Some(model) = box_model(&config) else {
        return;
    };
    let cutter = model.active_tool(1, 0.0);
    assert_eq!(cutter.shape, CutterShape::Flat { diameter: 6.0 });
    assert_eq!(cutter.length, 20.0);
    assert_eq!(
        cutter.z_offset, -56.068,
        "the tool row Z supplies the offset"
    );

    let hit = model.cutter_contact([10.0, 10.0, -102.0], &cutter);
    assert!(
        hit.hit,
        "a 6 mm cutter below the top face must hit: {hit:?}"
    );
    assert!(hit.distance < 0.0);
    assert_eq!(hit.target, Some(CollisionTarget::Workpiece(0)));

    let probe = model.active_tool(99, 0.0);
    assert!(probe.is_probe());
    let ignored = model.cutter_contact([10.0, 10.0, -102.0], &probe);
    assert!(!ignored.hit, "the probe is not a cutter: {ignored:?}");
    assert_eq!(ignored.target, None);
}

#[test]
fn cutter_outside_the_travel_hits_the_machine() {
    let config = contact_config();
    let Some(model) = box_model(&config) else {
        return;
    };
    let cutter = model.active_tool(1, 0.0);
    let travel = model.machine().travel;

    let outside = model.cutter_contact([travel.max[0] + 50.0, 10.0, -105.0], &cutter);
    assert!(outside.hit, "X past travel must collide: {outside:?}");
    assert_eq!(outside.target, Some(CollisionTarget::Machine));

    let clear = model.cutter_contact(
        [travel.min[0] + 50.0, travel.min[1] + 50.0, -105.0],
        &cutter,
    );
    assert!(!clear.hit, "clear air inside travel: {clear:?}");
    assert_eq!(clear.target, None);
}

// ------------------------------------------------------------ actions

#[test]
fn set_transform_moves_contact_and_remove_clears_it() {
    let config = contact_config();
    let Some(mut model) = box_model(&config) else {
        return;
    };
    assert!(model.probe_contact([21.5, 10.0, -105.0]).touching);

    model
        .apply(Action::SetTransform {
            index: 0,
            transform: Transform::identity().with_translation([50.0, 0.0, -110.0]),
        })
        .expect("set transform");
    let moved = model.probe_contact([71.5, 10.0, -105.0]);
    assert!(moved.touching, "contact follows the part: {moved:?}");
    assert_close(moved.point[0], 70.0, "moved contact point x");
    assert!(
        !model.probe_contact([21.5, 10.0, -105.0]).touching,
        "the old position is empty now"
    );

    model
        .apply(Action::RemoveWorkpiece { index: 0 })
        .expect("remove workpiece");
    assert!(model.workpieces().is_empty());
    let cleared = model.probe_contact([71.5, 10.0, -105.0]);
    assert!(!cleared.touching);
    assert_eq!(cleared.workpiece, None);
    assert_eq!(cleared.distance, f64::INFINITY);

    let error = model
        .apply(Action::SetTransform {
            index: 0,
            transform: Transform::identity(),
        })
        .expect_err("index 0 is gone");
    assert!(matches!(error, ModelError::Index { index: 0, len: 0 }));
}

#[test]
fn a_failed_import_leaves_the_model_intact() {
    let config = contact_config();
    let Some(mut model) = box_model(&config) else {
        return;
    };
    let before = model.workpieces().len();
    let error = model
        .apply(Action::AddWorkpiece {
            path: PathBuf::from("/nonexistent/model_missing.step"),
            transform: Transform::identity(),
        })
        .expect_err("a missing STEP file must fail");
    assert!(matches!(error, ModelError::Import { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("/nonexistent/model_missing.step"),
        "error must name the path: {error}"
    );
    assert_eq!(model.workpieces().len(), before);
    assert!(
        model.probe_contact([10.0, 10.0, -98.6]).touching,
        "the previous workpiece still answers"
    );
}

#[test]
fn a_missing_configured_step_is_a_fault_not_a_failure() {
    let mut config = fixture_config();
    config.workpieces = vec![WorkpieceEntry {
        name: "gone".to_string(),
        path: PathBuf::from("/nonexistent/model_absent.step"),
        translate_mm: [0.0; 3],
        rotate_deg: [0.0; 3],
        visible: true,
    }];
    let model = SimModel::from_config(&config).expect("the model still builds");
    assert!(model.workpieces().is_empty());
    assert!(
        model
            .faults()
            .iter()
            .any(|fault| fault.contains("model_absent.step")),
        "faults must name the skipped part: {:?}",
        model.faults()
    );
    assert!(
        !model.machine().parts.is_empty(),
        "the machine is still there"
    );
}

// ------------------------------------------------------------- tools

#[test]
fn tool_table_drives_probe_and_cutter_geometry() {
    let ini = MachineIni::parse_file(&fixture("model_machine.ini")).expect("fixture INI");
    let load = tools::load_tool_table(&ini);
    assert!(load.warning.is_none(), "{:?}", load.warning);
    let table = load.table.expect("fixture tool table");
    let config = contact_config();

    let probe = tools::probe_from_table(&table, &config);
    assert_eq!(probe.tool, 99);
    assert_close(probe.tip_radius, 1.5, "T99 D+3");
    assert_close(probe.stylus_len, 30.0, "stylus");
    assert_close(probe.tip_offset, -30.0, "zero table Z falls back to config");

    assert_eq!(
        tools::cutter_from_row(table.row(2), &config),
        CutterShape::Ball { diameter: 17.3 },
        "the config override turns T2 into a ball nose"
    );
    assert_eq!(
        tools::cutter_from_row(table.row(4), &config),
        CutterShape::Flat { diameter: 13.45 },
        "no override and no default row profile => flat with D"
    );
}

#[test]
fn the_baier_tool_table_gives_the_probe_1_5_mm() {
    let ini = MachineIni::parse_file(&repo_root().join("configs/sim.ini")).expect("sim.ini");
    let load = tools::load_tool_table(&ini);
    let table = load
        .table
        .unwrap_or_else(|| panic!("configs/tool.tbl must load: {:?}", load.warning));
    let mut config = Sim3dConfig::default();
    config.cutters = vec![CutterConfig {
        tool: 2,
        kind: CutterKind::Ball,
        corner_radius_mm: None,
    }];
    let probe = tools::probe_from_table(&table, &config);
    assert_eq!(probe.tool, 99);
    assert_close(probe.tip_radius, 1.5, "configs/tool.tbl T99 D+3");
    assert_eq!(
        tools::cutter_from_row(table.row(2), &config),
        CutterShape::Ball { diameter: 17.3 }
    );
    assert_eq!(
        tools::cutter_from_row(table.row(1), &config),
        CutterShape::Flat { diameter: 6.0 }
    );
}

#[test]
fn a_missing_tool_table_degrades_to_a_warning() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ini_path = dir.path().join("model_machine.ini");
    fs::write(
        &ini_path,
        "[EMC]\nMACHINE = No Table\n\
         [KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n\
         [TRAJ]\nCOORDINATES = XYZ\n\
         [EMCIO]\nTOOL_TABLE = model_absent.tbl\nTOOL_TABLE_COLUMNS = TZDR\n\
         [AXIS_X]\nMIN_LIMIT = -100\nMAX_LIMIT = 0\n\
         [AXIS_Y]\nMIN_LIMIT = -100\nMAX_LIMIT = 0\n\
         [AXIS_Z]\nMIN_LIMIT = -100\nMAX_LIMIT = 0\n",
    )
    .expect("write INI");

    let mut config = Sim3dConfig::default();
    config.machine.ini = ini_path;
    let model = SimModel::from_config(&config).expect("a missing tool table is not fatal");
    assert!(model.tool_table().is_none());
    assert_close(model.probe().tip_radius, 1.5, "documented fallback radius");
    assert!(
        model
            .faults()
            .iter()
            .any(|fault| fault.contains("tool table")),
        "the warning is recorded: {:?}",
        model.faults()
    );
}

// ------------------------------------------------------- shared state

#[test]
fn shared_state_queues_actions_and_publishes_snapshots() {
    let model = SimModel::from_config(&fixture_config()).expect("fixture model");
    let shared = SharedState::new(model);

    assert_eq!(shared.snapshot(), Snapshot::idle());
    shared.push(Action::Reload);
    shared.push(Action::RemoveWorkpiece { index: 3 });
    let actions = shared.take_actions();
    assert_eq!(actions.len(), 2);
    assert!(matches!(actions[0], Action::Reload));
    assert!(matches!(actions[1], Action::RemoveWorkpiece { index: 3 }));
    assert!(shared.take_actions().is_empty(), "actions are taken once");

    let mut snapshot = Snapshot::idle();
    snapshot.tip = [1.0, 2.0, 3.0];
    snapshot.probe_touching = true;
    snapshot.tick = 7;
    shared.set_snapshot(snapshot.clone());
    assert_eq!(shared.snapshot(), snapshot);

    shared.write_model().record_fault("test fault");
    assert_eq!(shared.read_model().faults(), ["test fault".to_string()]);
}

#[test]
fn reload_rereads_the_config_workpieces() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ini_source = fs::read_to_string(fixture("model_machine.ini")).expect("read fixture INI");
    fs::write(dir.path().join("model_machine.ini"), ini_source).expect("copy INI");
    fs::copy(fixture("model_tool.tbl"), dir.path().join("model_tool.tbl")).expect("copy table");
    let config_path = dir.path().join("sim3d.toml");
    fs::write(
        &config_path,
        "[machine]\nini = \"model_machine.ini\"\n\n\
         [[workpieces]]\nname = \"gone\"\npath = \"/nonexistent/model_absent.step\"\n",
    )
    .expect("write config");

    let config = Sim3dConfig::load(&config_path).expect("load config");
    let mut model = SimModel::from_config(&config).expect("model");
    assert!(model.workpieces().is_empty());
    assert!(model
        .faults()
        .iter()
        .any(|fault| fault.contains("model_absent")));

    model.apply(Action::Reload).expect("reload");
    assert!(model.workpieces().is_empty());
    assert!(model
        .faults()
        .iter()
        .any(|fault| fault.contains("model_absent")));
}

// ----------------------------------------------------------- contract

#[test]
fn the_shared_contract_signatures_exist() {
    // Compile-time binding of the frozen contract. If a signature drifts,
    // this test stops compiling before task C does.
    use std::path::PathBuf;
    use std::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

    use linc_sim3d::model::{CollisionTarget, CutterContact, ProbeContact, Snapshot, Workpiece};
    use linc_sim3d::tools::ActiveTool;
    use linc_sim3d::types::{Bounds, Capsule, Sphere, Transform, TriMesh, P3};

    let _: fn(&Sim3dConfig) -> Result<SimModel, ModelError> = SimModel::from_config;
    let _: fn(&SimModel, u16, f64) -> ActiveTool = SimModel::active_tool;
    let _: fn(&SimModel, P3) -> ProbeContact = SimModel::probe_contact;
    let _: fn(&SimModel, P3, &ActiveTool) -> CutterContact = SimModel::cutter_contact;
    let _: fn(&mut SimModel, Action) -> Result<(), ModelError> = SimModel::apply;
    let _: fn(&SimModel) -> Bounds = SimModel::machine_bounds;
    let _: fn(&SimModel) -> &[Workpiece] = SimModel::workpieces;
    let _: fn(&SimModel) -> &Sim3dConfig = SimModel::config;
    let _: fn(&ActiveTool, P3) -> (Sphere, Capsule) = ActiveTool::contact_shapes;

    let _: fn(SimModel) -> SharedState = SharedState::new;
    let _: fn(&SharedState) -> RwLockReadGuard<'_, SimModel> = SharedState::read_model;
    let _: fn(&SharedState) -> RwLockWriteGuard<'_, SimModel> = SharedState::write_model;
    let _: fn(&SharedState) -> Snapshot = SharedState::snapshot;
    let _: fn(&SharedState, Snapshot) = SharedState::set_snapshot;
    let _: fn(&SharedState, Action) = SharedState::push;
    let _: fn(&SharedState) -> Vec<Action> = SharedState::take_actions;
    let _: fn() -> Snapshot = Snapshot::idle;

    // The exported field names, in one literal each.
    let snapshot = Snapshot {
        tip: [0.0; 3],
        probe_touching: false,
        probe_point: None,
        probe_workpiece: None,
        collision: false,
        collision_point: None,
        collision_target: None,
        tool_number: 0,
        tick: 0,
        faults: Vec::new(),
    };
    let _: P3 = snapshot.tip;
    let _: ProbeContact = ProbeContact {
        touching: false,
        distance: f64::INFINITY,
        point: [0.0; 3],
        workpiece: None,
    };
    let _: CutterContact = CutterContact {
        hit: false,
        distance: f64::INFINITY,
        point: [0.0; 3],
        target: Some(CollisionTarget::Machine),
    };
    let _: CollisionTarget = CollisionTarget::Workpiece(0);
    let _: Action = Action::AddWorkpiece {
        path: PathBuf::new(),
        transform: Transform::identity(),
    };
    let _: Action = Action::RemoveWorkpiece { index: 0 };
    let _: Action = Action::SetTransform {
        index: 0,
        transform: Transform::identity(),
    };
    let _: Action = Action::Reload;
    let _: fn() -> TriMesh = || TriMesh::new("empty", Vec::new(), Vec::new());
    let _: &Mutex<Vec<Action>> = &Mutex::new(Vec::new());
    let _: RwLock<Snapshot> = RwLock::new(Snapshot::idle());
}
