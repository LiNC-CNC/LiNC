//! Acceptance tests for writing the workpiece state back out.
//!
//! The fault this pins down: Save rewrote only the entries the config already
//! had and skipped every other row, so a part imported in the window was never
//! written, and Open setup on the file then found nothing.

use std::fs;
use std::path::{Path, PathBuf};

use linc_sim3d::config::{CutterConfig, CutterKind, Sim3dConfig, WorkpieceEntry};
use linc_sim3d::types::Transform;
use linc_sim3d::ui::{entries_for_file, entries_from_rows, write_workpieces, WorkpieceRow};

fn row(name: &str, source: &Path, translation: [f64; 3], visible: bool) -> WorkpieceRow {
    WorkpieceRow {
        name: name.to_string(),
        source: source.to_path_buf(),
        transform: Transform::identity()
            .with_translation(translation)
            .with_rotation([0.0, 0.0, 30.0]),
        visible,
        triangles: 12,
    }
}

/// The steps a save performs, in the order the panel performs them.
fn save_session(
    config: &mut Sim3dConfig,
    rows: &[WorkpieceRow],
    target: &Path,
) -> Result<(), String> {
    let entries = entries_from_rows(rows, &config.workpieces, &config.base_dir);
    config.workpieces = entries;
    let entries = entries_for_file(config, target);
    let fallback = config.clone();
    write_workpieces(target, entries, &fallback)
}

#[test]
fn a_workpiece_imported_in_the_window_is_saved_and_reads_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    let parts = dir.path().join("parts");
    fs::create_dir(&parts).expect("parts");
    let step = parts.join("block.step");
    fs::write(&step, b"solid x\nendsolid x\n").expect("step");

    let target = dir.path().join("setup.toml");
    let mut config = Sim3dConfig::default();
    config.base_dir = dir.path().to_path_buf();

    let rows = vec![row("block", &step, [-170.0, -180.0, -258.0], false)];
    save_session(&mut config, &rows, &target).expect("save");

    let saved = Sim3dConfig::load(&target).expect("reload");
    assert_eq!(
        saved.workpieces.len(),
        1,
        "the imported part is in the file"
    );
    let entry = &saved.workpieces[0];
    assert_eq!(entry.name, "block");
    assert_eq!(
        entry.path,
        PathBuf::from("parts/block.step"),
        "stored relative to the setup that names it"
    );
    assert_eq!(entry.translate_mm, [-170.0, -180.0, -258.0]);
    assert_eq!(entry.rotate_deg, [0.0, 0.0, 30.0]);
    assert!(!entry.visible, "visibility is stored");
    assert_eq!(
        saved.resolve(&entry.path).canonicalize().expect("resolve"),
        step.canonicalize().expect("canonical"),
        "opening the setup finds the same file"
    );
}

#[test]
fn saving_keeps_the_targets_own_machine_and_tools() {
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("other.toml");

    let mut other = Sim3dConfig::default();
    other.machine.ini = PathBuf::from("other.ini");
    other.machine.margin_mm = 7.0;
    other.cutters = vec![CutterConfig {
        tool: 3,
        kind: CutterKind::Ball,
        corner_radius_mm: None,
    }];
    other.save(&target).expect("write the other profile");

    let mut session = Sim3dConfig::default();
    session.machine.ini = PathBuf::from("sim.ini");
    session.machine.margin_mm = 40.0;
    session.base_dir = dir.path().to_path_buf();

    let step = dir.path().join("part.step");
    fs::write(&step, b"x").expect("step");
    save_session(
        &mut session,
        &[row("part", &step, [1.0, 2.0, 3.0], true)],
        &target,
    )
    .expect("save");

    let saved = Sim3dConfig::load(&target).expect("reload");
    assert_eq!(saved.machine.ini, PathBuf::from("other.ini"));
    assert_eq!(saved.machine.margin_mm, 7.0);
    assert_eq!(saved.cutters.len(), 1, "the setup keeps its own cutters");
    assert_eq!(saved.workpieces.len(), 1, "and gains our workpiece");
}

#[test]
fn a_removed_workpiece_leaves_the_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("first.step");
    let second = dir.path().join("second.step");
    fs::write(&first, b"x").expect("first");
    fs::write(&second, b"x").expect("second");
    let target = dir.path().join("setup.toml");
    let mut config = Sim3dConfig::default();
    config.base_dir = dir.path().to_path_buf();

    save_session(
        &mut config,
        &[
            row("first", &first, [0.0; 3], true),
            row("second", &second, [0.0; 3], true),
        ],
        &target,
    )
    .expect("save both");
    assert_eq!(
        Sim3dConfig::load(&target).expect("reload").workpieces.len(),
        2
    );

    save_session(
        &mut config,
        &[row("second", &second, [0.0; 3], true)],
        &target,
    )
    .expect("save one");
    let saved = Sim3dConfig::load(&target).expect("reload");
    assert_eq!(saved.workpieces.len(), 1);
    assert_eq!(saved.workpieces[0].name, "second");
}

#[test]
fn a_row_keeps_the_name_the_config_gave_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let step = dir.path().join("vice.step");
    fs::write(&step, b"x").expect("step");

    let existing = vec![WorkpieceEntry {
        name: "vice jaw blank".to_string(),
        path: PathBuf::from("vice.step"),
        translate_mm: [0.0; 3],
        rotate_deg: [0.0; 3],
        visible: true,
    }];
    let entries = entries_from_rows(
        &[row("vice", &step, [9.0, 8.0, 7.0], true)],
        &existing,
        dir.path(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "vice jaw blank");
    assert_eq!(entries[0].path, PathBuf::from("vice.step"));
    assert_eq!(entries[0].translate_mm, [9.0, 8.0, 7.0]);
}

#[test]
fn a_path_outside_the_setup_directory_is_stored_absolute() {
    let dir = tempfile::tempdir().expect("temp dir");
    let elsewhere = tempfile::tempdir().expect("other dir");
    let step = elsewhere.path().join("far.step");
    fs::write(&step, b"x").expect("step");

    let entries = entries_from_rows(&[row("far", &step, [0.0; 3], true)], &[], dir.path());
    assert!(
        entries[0].path.is_absolute(),
        "a part outside the setup's directory keeps its full path: {:?}",
        entries[0].path
    );
    assert_eq!(entries[0].path, step.canonicalize().expect("canonical"));
}
