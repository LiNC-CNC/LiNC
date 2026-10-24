//! Acceptance tests for the file picker and the recent-setup list.
//!
//! The listing and the store are pure path logic, so they are exercised here
//! without a window; the modal itself is drawn by egui and shares the same
//! functions.

use std::fs;
use std::path::{Path, PathBuf};

use linc_sim3d::browser::{
    self, extension_matches, list_directory, parent_of, resolve_typed, Browser, Purpose, Recents,
    RECENT_LIMIT,
};

fn touch(path: &Path) {
    fs::write(path, b"x").expect("write fixture file");
}

#[test]
fn the_listing_sorts_directories_first_and_keeps_only_the_purpose() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir(dir.path().join("zeta")).expect("dir");
    fs::create_dir(dir.path().join("Alpha")).expect("dir");
    touch(&dir.path().join("part.step"));
    touch(&dir.path().join("Part.STEP"));
    touch(&dir.path().join("raw.stp"));
    touch(&dir.path().join("mesh.stl"));
    touch(&dir.path().join("notes.txt"));

    let entries = list_directory(dir.path(), Purpose::Step, false).expect("listing");
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Alpha",
            "zeta",
            "mesh.stl",
            "Part.STEP",
            "part.step",
            "raw.stp"
        ],
        "directories first, then files by lowercase name, ties by exact name"
    );
    assert!(entries[0].is_dir && entries[1].is_dir);
    assert!(entries[2..].iter().all(|entry| !entry.is_dir));

    let setups = list_directory(dir.path(), Purpose::Setup, false).expect("listing");
    let names: Vec<&str> = setups.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        ["Alpha", "zeta"],
        "the setup listing keeps the directories and drops the STEP files"
    );
}

#[test]
fn hidden_entries_need_the_toggle_and_the_listing_is_case_insensitive() {
    let dir = tempfile::tempdir().expect("temp dir");
    touch(&dir.path().join(".secret.step"));
    fs::create_dir(dir.path().join(".git")).expect("dir");
    touch(&dir.path().join("visible.STEP"));

    let plain = list_directory(dir.path(), Purpose::Step, false).expect("listing");
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].name, "visible.STEP");

    let all = list_directory(dir.path(), Purpose::Step, true).expect("listing");
    let names: Vec<&str> = all.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, [".git", ".secret.step", "visible.STEP"]);

    assert!(extension_matches(Path::new("a.Step"), Purpose::Step));
    assert!(!extension_matches(Path::new("a.step.bak"), Purpose::Step));
    assert!(!extension_matches(Path::new("a.toml"), Purpose::Step));
    assert!(extension_matches(Path::new("sim3d.TOML"), Purpose::Setup));
}

#[test]
fn a_listing_of_a_missing_directory_is_an_error_not_a_panic() {
    let error = list_directory(Path::new("/nonexistent/parts"), Purpose::Step, false)
        .expect_err("missing directory");
    assert!(error.contains("/nonexistent/parts"), "{error}");
}

#[test]
fn parent_of_walks_up_and_stops_at_the_root() {
    assert_eq!(parent_of(Path::new("/a/b")), Some(PathBuf::from("/a")));
    assert_eq!(parent_of(Path::new("/")), None);
    assert_eq!(
        parent_of(Path::new("configs")),
        None,
        "a bare relative directory has nowhere to walk up to"
    );
}

#[test]
fn a_typed_path_resolves_against_the_open_directory() {
    let dir = Path::new("/parts");
    assert_eq!(
        resolve_typed("  ../lib/block.step  ", dir),
        Some(PathBuf::from("/parts/../lib/block.step"))
    );
    assert_eq!(
        resolve_typed("/abs/block.step", dir),
        Some(PathBuf::from("/abs/block.step"))
    );
    assert_eq!(resolve_typed("   ", dir), None);
    assert_eq!(
        resolve_typed("~/block.step", dir),
        browser::home_dir().map(|home| home.join("block.step"))
    );
}

#[test]
fn the_browser_opens_on_a_directory_and_can_move() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("nested");
    fs::create_dir(&nested).expect("dir");
    touch(&nested.join("block.step"));

    let mut picker = Browser::new(Purpose::Step, dir.path().to_path_buf());
    assert!(picker.is_open());
    assert_eq!(picker.purpose(), Purpose::Step);
    assert_eq!(picker.entries().len(), 1);
    assert!(picker.entries()[0].is_dir);

    picker.set_dir(nested.clone());
    assert_eq!(picker.dir(), nested.as_path());
    assert_eq!(picker.entries().len(), 1);
    assert_eq!(picker.entries()[0].name, "block.step");

    picker.set_dir(PathBuf::from("/nonexistent/parts"));
    assert!(picker.entries().is_empty(), "a bad directory lists nothing");
    assert!(picker.is_open(), "a bad directory is not a cancel");
}

#[test]
fn the_recent_list_keeps_one_entry_per_file_newest_first() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("a.toml");
    let second = dir.path().join("b.toml");
    touch(&first);
    touch(&second);

    let mut recents = Recents::default();
    assert!(recents.record(&first));
    assert!(!recents.record(&first), "the same file twice is no change");
    assert!(recents.record(&second));
    assert_eq!(
        recents.entries(),
        [second.clone(), first.clone()].as_slice()
    );

    assert!(recents.record(&first), "an older file moves to the front");
    assert_eq!(
        recents.entries(),
        [first.clone(), second.clone()].as_slice()
    );
}

#[test]
fn the_recent_list_is_capped_at_the_limit() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut recents = Recents::default();
    for index in 0..RECENT_LIMIT + 3 {
        let path = dir.path().join(format!("setup-{index}.toml"));
        touch(&path);
        recents.record(&path);
    }
    assert_eq!(recents.entries().len(), RECENT_LIMIT);
    assert_eq!(
        recents.entries()[0],
        dir.path().join(format!("setup-{}.toml", RECENT_LIMIT + 2))
    );
}

#[test]
fn the_recent_list_survives_a_round_trip_and_shrugs_off_a_bad_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("state").join("sim3d-recent.json");
    let setup = dir.path().join("setup.toml");
    touch(&setup);

    let mut recents = Recents::default();
    recents.record(&setup);
    recents.save(&store).expect("save");
    assert_eq!(Recents::load(&store), recents);

    fs::write(&store, b"not json at all").expect("corrupt the store");
    assert_eq!(
        Recents::load(&store),
        Recents::default(),
        "a corrupt store reads as an empty list"
    );
    assert_eq!(
        Recents::load(&dir.path().join("absent.json")),
        Recents::default()
    );
}

#[test]
fn pruning_drops_setup_files_that_have_gone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let kept = dir.path().join("kept.toml");
    let gone = dir.path().join("gone.toml");
    touch(&kept);
    touch(&gone);

    let mut recents = Recents::default();
    recents.record(&kept);
    recents.record(&gone);
    fs::remove_file(&gone).expect("remove");

    assert!(recents.prune());
    assert_eq!(recents.entries(), [kept].as_slice());
    assert!(!recents.prune(), "a second prune changes nothing");
}

#[test]
fn the_picker_remembers_the_directory_it_closed_in() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("state.json");
    let parts = dir.path().join("parts");
    fs::create_dir(&parts).expect("dir");
    let canonical = fs::canonicalize(&parts).expect("canonical");

    let mut recents = Recents::default();
    assert_eq!(recents.dir_for(Purpose::Step), None);
    assert!(recents.remember_dir(Purpose::Step, &parts));
    assert!(
        !recents.remember_dir(Purpose::Step, &parts),
        "the same directory is no change"
    );
    assert_eq!(recents.dir_for(Purpose::Step), Some(canonical.as_path()));
    assert_eq!(
        recents.dir_for(Purpose::Setup),
        None,
        "the two pickers remember their own directory"
    );

    recents.remember_dir(Purpose::Setup, dir.path());
    recents.save(&store).expect("save");
    let loaded = Recents::load(&store);
    assert_eq!(
        loaded.dir_for(Purpose::Step),
        recents.dir_for(Purpose::Step)
    );
    assert_eq!(
        loaded.dir_for(Purpose::Setup),
        recents.dir_for(Purpose::Setup)
    );
}

#[test]
fn a_store_written_before_directories_were_kept_still_loads() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("state.json");
    let setup = dir.path().join("old.toml");
    fs::write(
        &store,
        format!("{{ \"setups\": [\"{}\"] }}", setup.display()),
    )
    .expect("write");

    let loaded = Recents::load(&store);
    assert_eq!(loaded.entries(), [setup].as_slice());
    assert_eq!(loaded.dir_for(Purpose::Step), None);
    assert_eq!(loaded.dir_for(Purpose::Setup), None);
}

#[test]
fn the_list_reserves_ten_rows_of_height() {
    let row = 20.0;
    let gap = 4.0;
    let extent = linc_sim3d::browser::list_extent(row, gap);
    assert_eq!(extent, row * 10.0 + gap * 9.0);
    assert!(linc_sim3d::browser::VISIBLE_ROWS >= 10);
}

#[test]
fn the_state_path_follows_the_xdg_state_directory() {
    // The harness runs this test with the ambient environment, so only the
    // shape of the answer is asserted: a file name under a linc directory.
    if let Some(path) = browser::state_path() {
        assert_eq!(path.file_name().unwrap(), "sim3d-recent.json");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "linc");
        assert!(path.is_absolute(), "{}", path.display());
    }
}
