//! Acceptance tests for the linc-sim3d engine: one tick, and the loop body.
//!
//! The fixture is the same one tests/model.rs uses: the fixture INI and tool
//! table, plus tests/fixtures/box.step (20 x 20 x 10 mm) placed so its top face
//! sits at machine Z = -100 and its +X face at X = 20.
//!
//! Geometry, with the probe T99 (D+3, so a 1.5 mm ball) and its -30 mm offset:
//!   joint z -68.0 -> tip z -98.0 -> ball bottom -99.5 -> 0.5 mm of air
//!   joint z -68.6 -> tip z -98.6 -> ball bottom -100.1 -> 0.1 mm penetration
//!   tip [21.5, 10, -105] is tangent to the +X face
//! T1 is the 6 mm endmill with the table's Z-56.068, so joint z -50 puts its tip
//! at -106.068, below the top face.
//!
//! The STEP import needs the OCCT helper or freecadcmd; without either these
//! tests report why they skipped, the same way the model suite does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linc_sim3d::config::{CutterConfig, CutterKind, Sim3dConfig};
use linc_sim3d::engine::{run_engine, step_once, MockPositionSource, PositionSource, Positions};
use linc_sim3d::model::{Action, CollisionTarget, SharedState, SimModel};
use linc_sim3d::sink::Sim3dSignal;
use linc_sim3d::types::{Transform, P3};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_config() -> Sim3dConfig {
    let mut config = Sim3dConfig::default();
    config.machine.ini = fixture("model_machine.ini");
    // The tangent test needs the slack to absorb the last floating-point bit.
    config.probe.trigger_slack_mm = 1e-9;
    config.cutters = vec![CutterConfig {
        tool: 1,
        kind: CutterKind::Flat,
        corner_radius_mm: None,
    }];
    config
}

/// The fixture model with box.step on the table, or `None` when no STEP
/// backend is available.
fn box_model() -> Option<SimModel> {
    let mut model = SimModel::from_config(&fixture_config()).expect("fixture model");
    let transform = Transform::identity().with_translation([0.0, 0.0, -110.0]);
    match model.apply(Action::AddWorkpiece {
        path: fixture("box.step"),
        transform,
    }) {
        Ok(()) => Some(model),
        Err(error) => {
            eprintln!("skipping: cannot import box.step: {error}");
            None
        }
    }
}

/// A machine position with the probe loaded (T99), no live tool offset.
fn probe_at(x: f64, y: f64, z: f64) -> Positions {
    Positions {
        x,
        y,
        z,
        tool_offset_z: 0.0,
        tool_number: 99,
    }
}

/// Everything step_once and run_engine pushed into the signal.
#[derive(Clone, Default)]
struct Recorder {
    probe: Arc<AtomicBool>,
    collision: Arc<AtomicBool>,
    tips: Arc<Mutex<Vec<P3>>>,
    faults: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    fn probe_now(&self) -> bool {
        self.probe.load(Ordering::Relaxed)
    }

    fn collision_now(&self) -> bool {
        self.collision.load(Ordering::Relaxed)
    }

    fn tips(&self) -> Vec<P3> {
        self.tips.lock().unwrap().clone()
    }

    fn faults(&self) -> Vec<String> {
        self.faults.lock().unwrap().clone()
    }
}

impl Sim3dSignal for Recorder {
    fn probe(&self, contact: bool) {
        self.probe.store(contact, Ordering::Relaxed);
    }

    fn collision(&self, hit: bool) {
        self.collision.store(hit, Ordering::Relaxed);
    }

    fn tip(&self, point: P3) {
        self.tips.lock().unwrap().push(point);
    }

    fn fault(&self, message: &str) {
        self.faults.lock().unwrap().push(message.to_owned());
    }
}

/// A finite source: the engine stops when the samples run out.
struct ScriptedSource {
    samples: std::vec::IntoIter<Positions>,
}

impl ScriptedSource {
    fn new(samples: Vec<Positions>) -> Self {
        Self {
            samples: samples.into_iter(),
        }
    }
}

impl PositionSource for ScriptedSource {
    fn sample(&mut self) -> Option<Positions> {
        self.samples.next()
    }
}

// ---------------------------------------------------------------- a tick

#[test]
fn the_probe_touches_only_when_the_ball_reaches_the_part() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();

    let above = probe_at(10.0, 10.0, -68.0);
    let first = step_once(&shared, &above, &recorder);
    assert_eq!(first.tick, 1);
    assert_eq!(
        first.tip,
        [10.0, 10.0, -98.0],
        "tip = [x, y, z + tool.z_offset], with the probe -30 mm below the gauge line"
    );
    assert!(!first.probe_touching, "0.5 mm of air is not contact");
    assert_eq!(first.probe_point, None);
    assert!(!recorder.probe_now());

    let touching = Positions { z: -68.6, ..above };
    let second = step_once(&shared, &touching, &recorder);
    assert!(second.probe_touching, "0.1 mm penetration must touch");
    assert_eq!(second.probe_workpiece, Some(0));
    let point = second.probe_point.expect("contact point");
    assert!(
        (point[2] + 100.0).abs() <= 1e-6,
        "the contact point sits on the top face: {point:?}"
    );
    assert!(recorder.probe_now(), "probe-out follows the contact");

    let retract = step_once(&shared, &above, &recorder);
    assert!(!retract.probe_touching, "retracting releases the probe");
    assert!(!recorder.probe_now());
    assert_eq!(retract.tick, 3);
}

#[test]
fn every_tick_increments_and_publishes_its_snapshot() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();
    let positions = probe_at(10.0, 10.0, -68.6);

    let first = step_once(&shared, &positions, &recorder);
    assert_eq!(first.tick, 1);
    assert_eq!(shared.snapshot(), first, "the snapshot round-trips");

    let second = step_once(&shared, &positions, &recorder);
    assert_eq!(second.tick, 2);
    assert_eq!(shared.snapshot(), second);
    assert_eq!(
        recorder.tips(),
        vec![[10.0, 10.0, -98.6], [10.0, 10.0, -98.6]],
        "the tip goes out on every tick"
    );
}

#[test]
fn a_cutter_below_the_part_top_collides_and_the_probe_does_not() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();

    // Both tools at machine tip z = -100.5, half a millimetre below the top
    // face. T1 is the 6 mm endmill with the table's Z-56.068, the probe sits
    // 30 mm below the gauge line.
    let tip_z = -100.5;
    let cutter = Positions {
        x: 10.0,
        y: 10.0,
        z: tip_z + 56.068,
        tool_offset_z: 0.0,
        tool_number: 1,
    };
    let hit = step_once(&shared, &cutter, &recorder);
    assert!(
        (hit.tip[2] - tip_z).abs() <= 1e-9,
        "the tool row's Z supplies the offset: {:?}",
        hit.tip
    );
    assert!(hit.collision, "a cutter below the top face must hit");
    assert_eq!(hit.collision_target, Some(CollisionTarget::Workpiece(0)));
    assert!(recorder.collision_now());
    assert!(
        !hit.probe_touching,
        "a cutter never drives the touch-probe input, even where the probe would touch"
    );

    let probe = Positions {
        tool_number: 99,
        z: tip_z + 30.0,
        ..cutter
    };
    let probed = step_once(&shared, &probe, &recorder);
    assert!(
        (probed.tip[2] - tip_z).abs() <= 1e-9,
        "same tip, different tool: {:?}",
        probed.tip
    );
    assert!(!probed.collision, "the probe is not a cutter");
    assert_eq!(probed.collision_target, None);
    assert!(!recorder.collision_now(), "collision releases");
    assert!(
        probed.probe_touching,
        "the same tip with the probe loaded is contact: {probed:?}"
    );
}

#[test]
fn a_queued_transform_moves_the_contact_point_on_the_next_tick() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();

    let tangent = probe_at(21.5, 10.0, -75.0);
    let first = step_once(&shared, &tangent, &recorder);
    assert!(
        first.probe_touching,
        "the ball is tangent to the +X face at x = 20: {first:?}"
    );

    shared.push(Action::SetTransform {
        index: 0,
        transform: Transform::identity().with_translation([50.0, 0.0, -110.0]),
    });
    let moved_away = step_once(&shared, &tangent, &recorder);
    assert!(
        !moved_away.probe_touching,
        "the part moved 50 mm in +X, so the old position is empty"
    );

    let moved = probe_at(71.5, 10.0, -75.0);
    let second = step_once(&shared, &moved, &recorder);
    assert!(second.probe_touching, "contact follows the part");
    let point = second.probe_point.expect("contact point");
    assert!(
        (point[0] - 70.0).abs() <= 1e-9,
        "the contact point moved with the part: {point:?}"
    );
}

#[test]
fn a_refused_action_is_a_fault_on_the_snapshot_and_on_the_signal() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();
    let positions = probe_at(10.0, 10.0, -68.0);

    shared.push(Action::AddWorkpiece {
        path: PathBuf::from("/nonexistent/engine_missing.step"),
        transform: Transform::identity(),
    });
    let snapshot = step_once(&shared, &positions, &recorder);
    assert!(
        snapshot
            .faults
            .iter()
            .any(|fault| fault.contains("engine_missing.step")),
        "a refused import is reported, not dropped: {:?}",
        snapshot.faults
    );
    assert_eq!(
        recorder.faults().len(),
        1,
        "one new fault reaches the signal once: {:?}",
        recorder.faults()
    );
    assert!(
        shared.read_model().workpieces().len() == 1,
        "the refused action left the model alone"
    );

    let again = step_once(&shared, &positions, &recorder);
    assert!(
        again
            .faults
            .iter()
            .any(|fault| fault.contains("engine_missing.step")),
        "the snapshot keeps reporting the fault"
    );
    assert_eq!(
        recorder.faults().len(),
        1,
        "and does not push the same fault into the signal twice"
    );
}

#[test]
fn a_tool_row_the_table_lacks_is_a_fault() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = SharedState::new(model);
    let recorder = Recorder::default();

    let unknown = Positions {
        tool_number: 42,
        ..probe_at(10.0, 10.0, -68.0)
    };
    let snapshot = step_once(&shared, &unknown, &recorder);
    assert!(
        snapshot.faults.iter().any(|fault| fault.contains("T42")),
        "a tool the table does not describe is a fault: {:?}",
        snapshot.faults
    );
    assert_eq!(recorder.faults().len(), 1);
    assert!(!snapshot.probe_touching, "T42 is not the probe");
}

// ------------------------------------------------------------- the loop

#[test]
fn the_engine_runs_three_samples_and_releases_the_probe() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = Arc::new(SharedState::new(model));
    let recorder = Recorder::default();
    let source = ScriptedSource::new(vec![
        probe_at(10.0, 10.0, -68.0),
        probe_at(10.0, 10.0, -68.6),
        probe_at(10.0, 10.0, -68.6),
    ]);

    run_engine(
        Arc::clone(&shared),
        Box::new(source),
        Box::new(recorder.clone()),
        Duration::from_millis(1),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(shared.snapshot().tick, 3, "one tick per sample");
    assert_eq!(recorder.tips().len(), 3);
    assert!(
        !recorder.probe_now(),
        "the probe is released when the source ends"
    );
    assert!(!recorder.collision_now());
}

#[test]
fn run_hal_resolves_from_both_modules() {
    // One implementation, reachable as engine::run_hal (the contract's entry
    // point) and as hal::run_hal (where it lives).
    let from_engine: fn(&Path) -> anyhow::Result<()> = linc_sim3d::engine::run_hal;
    let from_hal: fn(&Path) -> anyhow::Result<()> = linc_sim3d::hal::run_hal;
    assert_eq!(from_engine as usize, from_hal as usize);
}

#[test]
fn a_set_stop_flag_stops_the_engine_before_the_first_tick() {
    let Some(model) = box_model() else {
        return;
    };
    let shared = Arc::new(SharedState::new(model));
    let recorder = Recorder::default();
    let source = MockPositionSource::from_positions(probe_at(10.0, 10.0, -68.6));

    run_engine(
        Arc::clone(&shared),
        Box::new(source),
        Box::new(recorder.clone()),
        Duration::from_millis(1),
        Arc::new(AtomicBool::new(true)),
    );

    assert_eq!(shared.snapshot().tick, 0, "a set flag runs no ticks");
    assert!(recorder.tips().is_empty());
    assert!(!recorder.probe_now());
}
