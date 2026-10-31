//! Shell integration tests: a real `LincApp` driven through real egui frames.

use super::*;
use linc_core::model::{PanelModeValidity, ToolChangerKind};

fn fixture() -> (tempfile::TempDir, Profile) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("base.toml"),
        include_str!("../../../../configs/base.toml"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("gcode")).unwrap();
    std::fs::write(
        root.join("tool.tbl"),
        "T1 P0 Z-56 D6 ; end mill\nT12 P0 Z0 D10 ; face mill\n",
    )
    .unwrap();
    std::fs::write(
        root.join("probe_basic.ini"),
        "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = TZDR\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -372\nMAX_LIMIT = 5\n[AXIS_Y]\nMIN_LIMIT = -346\nMAX_LIMIT = 4\n[AXIS_Z]\nMIN_LIMIT = -253\nMAX_LIMIT = 19\n",
    )
    .unwrap();
    std::fs::write(
        root.join("sim.toml"),
        r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
panel_window = false
[preferences]
file = "{root}/prefs/{{profile}}.preferences.toml"
"#
        .replace("{root}", &root.to_string_lossy()),
    )
    .unwrap();
    let profile = Profile::load(&root.join("sim.toml"), None).unwrap();
    (dir, profile)
}

/// A real profile (base.toml merged with a sim profile) with the two
/// fields the Diagnostics turret tab is decided by, overridden.
fn diagnostics_profile(turret_tab: bool, changer: ToolChangerKind) -> Profile {
    let (_dir, mut profile) = fixture();
    profile.config.diagnostics.turret_tab = turret_tab;
    profile.changer = changer;
    profile
}

#[test]
fn diagnostics_turret_tab_needs_the_profile_key_and_stations() {
    let has_turret =
        |profile: &Profile| DiagnosticsTab::visible(profile).contains(&DiagnosticsTab::Turret);
    assert!(!has_turret(&diagnostics_profile(
        false,
        ToolChangerKind::Manual
    )));
    assert!(!has_turret(&diagnostics_profile(
        true,
        ToolChangerKind::Manual
    )));
    assert!(has_turret(&diagnostics_profile(true, ToolChangerKind::Atc)));
    assert!(has_turret(&diagnostics_profile(
        true,
        ToolChangerKind::Turret
    )));
    assert!(!has_turret(&diagnostics_profile(
        false,
        ToolChangerKind::Turret
    )));
    // Visible, the list is the whole strip in its usual order; hidden, it
    // is the same list minus the turret entry.
    assert_eq!(
        DiagnosticsTab::visible(&diagnostics_profile(true, ToolChangerKind::Turret)),
        DiagnosticsTab::ALL.to_vec()
    );
    let hidden = DiagnosticsTab::visible(&diagnostics_profile(false, ToolChangerKind::Atc));
    assert_eq!(hidden.len(), DiagnosticsTab::ALL.len() - 1);
    assert!(hidden.iter().all(|tab| DiagnosticsTab::ALL.contains(tab)));
}

#[test]
fn diagnostics_navigation_and_body_skip_a_hidden_turret_tab() {
    let mut harness = ProbeHarness::ready(PanelMode::Auto, false, true, MachinePower::On);
    harness.app.profile.config.diagnostics.turret_tab = false;
    harness.app.profile.changer = ToolChangerKind::Turret;
    harness.app.set_page(Page::Diagnostics);

    harness.app.diagnostics.tab = DiagnosticsTab::Motion;
    harness.app.page_down();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Io);
    harness.app.page_up();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Motion);
    harness.app.page_up();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::HalNml);

    // Wrapping stays inside the visible list.
    harness.app.diagnostics.tab = DiagnosticsTab::Logs;
    harness.app.page_down();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Overview);
    harness.app.page_up();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Logs);

    // A remembered or restored turret tab never reaches the body.
    harness.app.diagnostics.tab = DiagnosticsTab::Turret;
    let output = harness.frame_full(Vec::new());
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Overview);
    let texts = painted_text(&output);
    assert!(!texts.iter().any(|(text, _)| text == "TURRET / LIVE TOOL"));
    assert!(texts.iter().any(|(text, _)| text == "OVERVIEW"));

    // Visible, the same tab is kept, drawn and navigated like any other.
    harness.app.profile.config.diagnostics.turret_tab = true;
    harness.app.diagnostics.tab = DiagnosticsTab::Turret;
    let output = harness.frame_full(Vec::new());
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Turret);
    assert!(painted_text(&output)
        .iter()
        .any(|(text, _)| text == "TURRET / LIVE TOOL"));
    harness.app.page_down();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Io);
    harness.app.page_up();
    assert_eq!(harness.app.diagnostics.tab, DiagnosticsTab::Turret);
}

fn make_app(mode: PanelMode) -> (tempfile::TempDir, LincApp) {
    let (dir, profile) = fixture();
    let ctx = egui::Context::default();
    let backend = Box::new(LocalBackend::new(profile.clone(), mode));
    let app = LincApp::new(profile, backend, &ctx);
    (dir, app)
}

/// The sidebar SHORTCUTS frame: the only 4 px-corner pane in the nav
/// column. Its rect, so a test can see how tall the pane came out.
fn shortcuts_frame(output: &egui::FullOutput) -> Option<egui::Rect> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Rect(rect) => {
                if rect.fill == theme::PANEL && rect.corner_radius.nw == 4 {
                    out.push(rect.rect);
                }
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, out);
                }
            }
            _ => {}
        }
    }
    let mut rects = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut rects);
    }
    rects
        .into_iter()
        .find(|rect| rect.left() < theme::NAV_WIDTH)
}

/// The bottom bar reads the overrides in the physical panel's order:
/// FEED, RAPID, SPINDLE. The old format printed the spindle value in the
/// RAPID slot and the rapid value in the SPINDLE slot.
/// MACHINE OFF is a fault, not a quiet state: the bar paints it in the error
/// colour, and stale data asks instead of guessing.
#[test]
fn the_machine_field_is_red_when_the_machine_is_off() {
    assert_eq!(
        machine_status_field(false, false),
        ("MACHINE OFF".to_string(), theme::ERROR)
    );
    assert_eq!(
        machine_status_field(true, false),
        ("MACHINE ON".to_string(), theme::SUCCESS)
    );
    assert_eq!(
        machine_status_field(false, true),
        ("MACHINE ?".to_string(), theme::WARNING)
    );
}

/// MACHINE OFF alternates twice a second, bright then dim, so it reads as
/// a fault rather than a steady field the operator's eye skips.
#[test]
fn machine_off_blinks_twice_a_second() {
    assert!(blink_is_bright(0.0));
    assert!(blink_is_bright(0.49));
    assert!(!blink_is_bright(0.5));
    assert!(!blink_is_bright(0.99));
    assert!(blink_is_bright(1.0));
}

/// The probe-trip banner says what actually happened to the machine: a trip
/// raised while the program ran switches it off, one raised while the
/// program was paused leaves it on so the operator can prove the probe input
/// by hand. Both name the key that resets the fault.
#[test]
fn the_probe_trip_banner_has_one_text_per_outcome() {
    use linc_core::faults::{ProbeTripFault, ProbeTripKind};
    let message = ProbeTripKind::NonProbeMove.message();
    let switched_off = ProbeTripFault::new(ProbeTripKind::NonProbeMove, message, 1);
    let left_on =
        ProbeTripFault::new_with_state(ProbeTripKind::NonProbeMove, message, 1, true, false);
    // A paused program being jogged into the probe is not the finger test:
    // the controller reports it as a non-probe move with the pause still set.
    let jogged =
        ProbeTripFault::new_with_state(ProbeTripKind::NonProbeMove, message, 1, true, true);

    let off_text = probe_trip_banner_text(&switched_off);
    let on_text = probe_trip_banner_text(&left_on);
    assert_ne!(
        off_text, on_text,
        "the banner must not claim a switched-off machine when it was left on"
    );
    assert!(
        off_text.contains("MACHINE SWITCHED OFF"),
        "the running-program trip names the switch-off: {off_text}"
    );
    assert!(
        on_text.contains("PAUSED") && on_text.contains("MACHINE LEFT ON"),
        "the paused trip says why the machine stayed on: {on_text}"
    );
    assert_eq!(
        probe_trip_banner_text(&jogged),
        off_text,
        "a jog from a paused program must say the machine was switched off"
    );
    for text in [&off_text, &on_text] {
        assert!(
            text.contains(PROBE_TRIP_ACTION),
            "both banners name the reset key {PROBE_TRIP_ACTION}: {text}"
        );
        assert!(
            text.contains(message),
            "the controller text is carried: {text}"
        );
    }
}

/// One pass of the bottom bar alone, with the machine state the caller set.
/// The probe field is a label, so its presence on the bar is a text shape
/// and its absence is no shape at all.
fn bottom_bar_texts(app: &mut LincApp, ctx: &egui::Context) -> Vec<String> {
    let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
        app.bottom_bar(ui);
    });
    output.textures_delta.clear();
    painted_text(&output)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// The field is drawn only while the probe tool is the one in the spindle:
/// fitted, not fitted, and no probe configured at all. Absent means absent -
/// no placeholder, no dimmed row.
#[test]
fn the_probe_field_is_drawn_only_with_the_probe_in_the_spindle() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let probe_tool = harness.app.profile.config.probe.tool_number;
    harness.app.snapshot.tool_change.current_tool = probe_tool;
    harness.app.snapshot.meta.stale = false;
    harness.app.snapshot.probe.input_tripped = Some(false);
    let fitted = bottom_bar_texts(&mut harness.app, &harness.ctx);
    assert!(
        fitted.iter().any(|text| text == "PROBE READY"),
        "the probe is fitted, so the bar carries its input: {fitted:?}"
    );

    // A cutter in the spindle: the probe is on the bench, so the field goes.
    harness.app.snapshot.tool_change.current_tool = 12;
    let cutter = bottom_bar_texts(&mut harness.app, &harness.ctx);
    assert!(
        !cutter.iter().any(|text| text.starts_with("PROBE")),
        "no probe field while a cutter is in the spindle: {cutter:?}"
    );

    // Nothing fitted and no probe configured: no field either.
    harness.app.snapshot.tool_change.current_tool = 0;
    let empty = bottom_bar_texts(&mut harness.app, &harness.ctx);
    assert!(
        !empty.iter().any(|text| text.starts_with("PROBE")),
        "an empty spindle carries no probe field: {empty:?}"
    );
    harness.app.profile.config.probe.enabled = false;
    harness.app.snapshot.tool_change.current_tool = harness.app.profile.config.probe.tool_number;
    let disabled = bottom_bar_texts(&mut harness.app, &harness.ctx);
    assert!(
        !disabled.iter().any(|text| text.starts_with("PROBE")),
        "a profile without a probe gets no field: {disabled:?}"
    );
}

/// The probe readout is a state word and a colour, and it never claims a
/// reading it does not have: stale data asks, like the machine field.
#[test]
fn the_probe_field_reads_ready_tripped_or_asks() {
    // The gate the bar draws behind: a configured probe is not a fitted one.
    assert!(
        probe_fitted(true, 99, 99),
        "the probe tool is in the spindle"
    );
    assert!(
        !probe_fitted(true, 99, 12),
        "a cutter in the spindle means the probe is not fitted"
    );
    assert!(
        !probe_fitted(true, 99, 0),
        "an empty spindle has no probe in it"
    );
    assert!(
        !probe_fitted(false, 99, 99),
        "no probe configured is no probe fitted"
    );
    assert_eq!(
        probe_input_field(Some(false), false),
        ("PROBE READY".to_string(), theme::SUCCESS)
    );
    assert_eq!(
        probe_input_field(Some(true), false),
        ("PROBE TRIPPED".to_string(), theme::WARNING)
    );
    assert_eq!(
        probe_input_field(None, false),
        ("PROBE ?".to_string(), theme::MUTED)
    );
    for tripped in [Some(true), Some(false), None] {
        assert_eq!(
            probe_input_field(tripped, true),
            ("PROBE ?".to_string(), theme::MUTED),
            "stale data asks, whatever the last poll said: {tripped:?}"
        );
    }
}

/// The probe field is the newest tenant of the bottom bar, and the row has to
/// keep fitting the 1280 px baseline window. Measured from the font rather
/// than guessed: every width below is theme::measure at the field's own size,
/// and 24 px is the gap the row leaves between two fields (add_space(16) plus
/// the horizontal item spacing).
#[test]
fn the_bottom_bar_readout_fits_the_minimum_window() {
    let ctx = egui::Context::default();
    let measure = |texts: &[&'static str]| {
        let mut width = None;
        let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
            let text: f32 = texts
                .iter()
                .map(|text| theme::measure(ui, text, 11.0))
                .sum();
            width = Some(text + 24.0 * (texts.len() - 1) as f32);
        });
        output.textures_delta.clear();
        width.expect("one measuring pass")
    };
    // The left-hand machine fields and the read-only group, each at the
    // widest wording it can show at once: E-stop active, machine off, probe
    // tripped, program protected, a program running with a DTG.
    let left = measure(&["LN 1234/5678 100%", "HOMED", "DTG Z -123.456"]);
    let group = measure(&[
        "E-STOP ACTIVE",
        "MACHINE OFF",
        "PROBE TRIPPED",
        "PROTECT ON",
        "OVR F100 R100 S100",
        PanelMode::ProgHome.label(),
    ]);
    // The same group without the probe field: it is drawn only while the
    // probe is in the spindle, and the absent layout must neither leave a
    // gap nor need less than the row has.
    let group_without_probe = measure(&[
        "E-STOP ACTIVE",
        "MACHINE OFF",
        "PROTECT ON",
        "OVR F100 R100 S100",
        PanelMode::ProgHome.label(),
    ]);
    let overridden = measure(&["LIMITS OVERRIDE ON"]);
    let probe = measure(&["PROBE TRIPPED"]);

    // 16 px either side of the 1280 px baseline.
    let room = 1280.0 - 32.0;
    assert!(
        left + group <= room,
        "the bottom bar overflows 1280 px: {left:.1} + {group:.1} > {room:.1}"
    );
    assert!(
        left + group + overridden <= room,
        "the bar overflows 1280 px with the limits override shown: {left:.1} + {group:.1} + {overridden:.1} > {room:.1}"
    );
    assert!(
        left + group_without_probe <= room,
        "the bottom bar overflows 1280 px without the probe field: {left:.1} + {group_without_probe:.1} > {room:.1}"
    );
    // Absent means gone, not blanked out: the group is exactly the probe
    // field and its 24 px gap narrower, and nothing else moves.
    let probe_cost = group - group_without_probe;
    assert!(
        (probe_cost - (probe + 24.0)).abs() < 0.5,
        "the absent field frees {probe_cost:.1} px; the field and its gap cost {:.1}",
        probe + 24.0
    );
    assert!(
        probe < 160.0,
        "PROBE TRIPPED and its gap cost {probe:.1} px of the row"
    );
}
/// The header's worst and mean are the run's, and a new program starts
/// them again: a fresh run must not be judged against the last one.
#[test]
fn the_following_error_summary_belongs_to_the_run() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let feed = |harness: &mut ProbeHarness, value: f64| {
        // Cutting, so the sample counts towards the run's numbers.
        harness.app.snapshot.motion_kind = linc_core::model::MotionKind::Feed;
        harness
            .app
            .snapshot
            .machine
            .following_error
            .insert("X", value);
        harness.app.sample_following_error();
    };

    // Start from a cleared run: the harness has already sampled its own
    // snapshots while building the app.
    harness.app.following.run_max = linc_core::model::AxisMap::new();
    harness.app.following.run_abs_sum = linc_core::model::AxisMap::new();
    harness.app.following.run_samples = linc_core::model::AxisMap::new();
    harness.app.following.run_active = false;

    // No program yet: jogging still counts towards the numbers.
    feed(&mut harness, -0.002);
    feed(&mut harness, 0.004);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert!((worst - 0.004).abs() < 1e-12, "worst {worst}");
    assert!((mean - 0.003).abs() < 1e-12, "mean {mean}");

    // A program starts: the numbers begin again.
    harness.app.snapshot.program.running = true;
    feed(&mut harness, 0.010);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert!((worst - 0.010).abs() < 1e-12, "worst {worst}");
    assert!((mean - 0.010).abs() < 1e-12, "mean {mean}");

    // Still the same run: the numbers accumulate.
    feed(&mut harness, -0.006);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert!((worst - 0.010).abs() < 1e-12, "worst {worst}");
    assert!((mean - 0.008).abs() < 1e-12, "mean {mean}");

    // The run ends and another starts: everything is cleared, so a quiet
    // new program does not inherit the old one's worst.
    harness.app.snapshot.program.running = false;
    feed(&mut harness, 0.001);
    harness.app.snapshot.program.running = true;
    feed(&mut harness, 0.001);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert!((worst - 0.001).abs() < 1e-12, "worst {worst}");
    assert!((mean - 0.001).abs() < 1e-12, "mean {mean}");
}

/// A rapid's following error is the machine catching up with itself, so the
/// run's worst and mean only take cutting samples; the trace keeps every
/// sample, because the shape of a rapid is still worth seeing.
#[test]
fn the_following_error_numbers_ignore_rapids() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.following.run_max = linc_core::model::AxisMap::new();
    harness.app.following.run_abs_sum = linc_core::model::AxisMap::new();
    harness.app.following.run_samples = linc_core::model::AxisMap::new();
    harness.app.following.history = linc_core::model::AxisMap::new();
    let feed = |harness: &mut ProbeHarness, kind, value: f64| {
        harness.app.snapshot.motion_kind = kind;
        harness
            .app
            .snapshot
            .machine
            .following_error
            .insert("X", value);
        harness.app.sample_following_error();
    };

    feed(&mut harness, linc_core::model::MotionKind::Traverse, 0.500);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert_eq!((worst, mean), (0.0, 0.0), "a rapid is not counted");
    assert_eq!(
        harness.app.following.history.get("X").map(|t| t.len()),
        Some(1)
    );

    feed(&mut harness, linc_core::model::MotionKind::Feed, 0.010);
    feed(&mut harness, linc_core::model::MotionKind::ArcFeed, 0.020);
    let (worst, mean) = harness.app.following_error_summary("X");
    assert!((worst - 0.020).abs() < 1e-12, "worst {worst}");
    assert!((mean - 0.015).abs() < 1e-12, "mean {mean}");

    // A rapid in the middle does not enter the numbers.
    feed(&mut harness, linc_core::model::MotionKind::Traverse, 0.900);
    let (worst, _) = harness.app.following_error_summary("X");
    assert!((worst - 0.020).abs() < 1e-12, "worst {worst}");
}
/// The square key on MAIN swaps the feed/servo cards for the following-error
/// traces, and swaps them back.
#[test]
fn the_square_key_shows_the_following_error_traces_on_main() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    let texts = |harness: &mut ProbeHarness| -> Vec<String> {
        painted_text(&harness.frame_full(Vec::new()))
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    };
    let cards = texts(&mut harness);
    assert!(
        cards.iter().any(|text| text == "SERVO LOAD"),
        "the feed/servo pair is in front: {cards:?}"
    );

    harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
    assert!(harness.app.following.active, "F10 shows the traces");
    let cards = texts(&mut harness);
    assert!(
        cards.iter().any(|text| text == "FOLLOWING ERROR"),
        "the traces replace both cards: {cards:?}"
    );
    assert!(
        !cards.iter().any(|text| text == "SERVO LOAD"),
        "and the servo card is gone: {cards:?}"
    );
    // Three labelled groups - value, worst, mean - one number per axis in
    // each of them.
    // The live values have no label and the mean's is a drawn diameter mark,
    // so the square glyph is the one typed label here; the per-axis count
    // below covers all three groups.
    for label in ["🗖"] {
        assert!(
            cards.iter().any(|text| text == label),
            "{label} is labelled once: {cards:?}"
        );
    }
    for axis in ["X", "Y", "Z"] {
        let numbers = cards
            .iter()
            .filter(|text| text.starts_with(axis) && text.len() > 1)
            .count();
        assert!(
            numbers >= 3,
            "{axis} appears in all three groups: {cards:?}"
        );
    }

    harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
    assert!(!harness.app.following.active, "and back to the cards");
}

/// REMAINING changes what the card means - no trail, and the path eaten behind
/// the tool - so the card says so in its own header, not only in the
/// shortcut list.
#[test]
fn remaining_is_announced_in_the_toolpath_header() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    let header = |harness: &mut ProbeHarness| -> Vec<(String, egui::Rect)> {
        painted_text(&harness.frame_full(Vec::new()))
            .into_iter()
            .filter(|(_, rect)| rect.left() > theme::NAV_WIDTH && rect.center().y < 220.0)
            .collect()
    };
    assert!(
        !header(&mut harness)
            .iter()
            .any(|(text, _)| text == "REMAINING"),
        "no indicator with the mode off"
    );
    harness.app.graph_style.remaining = true;
    assert!(
        header(&mut harness)
            .iter()
            .any(|(text, _)| text == "REMAINING"),
        "the card header says REMAINING while it is on"
    );
}

/// The square key flips the right pane between the program directory and
/// the recently opened programs, newest first, and leaves the left pane
/// alone.
#[test]
fn the_square_key_flips_the_right_pane_to_recent_files() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness.app.set_page(Page::Files);
    harness.app.files.usb_focused = false;
    harness.app.prefs.preferences.recent_files =
        vec!["/nc/newest.ngc".to_string(), "/nc/older.ngc".to_string()];
    let texts = |harness: &mut ProbeHarness| -> Vec<String> {
        painted_text(&harness.frame_full(Vec::new()))
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    };
    let listing = texts(&mut harness);
    assert!(listing.iter().any(|text| text == "NC_FILES"), "{listing:?}");
    assert!(
        !listing.iter().any(|text| text == "newest.ngc"),
        "the directory is in front: {listing:?}"
    );

    harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
    assert!(harness.app.files.recent_shown, "F10 flips the right pane");
    assert!(
        !harness.app.files.usb_focused,
        "and puts the arrows in that pane"
    );
    let listing = texts(&mut harness);
    assert!(listing.iter().any(|text| text == "RECENT"), "{listing:?}");
    let newest = listing
        .iter()
        .position(|text| text == "newest.ngc")
        .expect("the newest program is listed");
    let older = listing
        .iter()
        .position(|text| text == "older.ngc")
        .expect("the older program is listed");
    assert!(newest < older, "most recent first: {listing:?}");

    harness.frame_with(&[(egui::Key::F10, egui::Modifiers::NONE)]);
    assert!(!harness.app.files.recent_shown, "and back to the directory");
}

/// The graph's frame is the controller's offset even when no program is
/// loaded. The empty-text branch used to return before setting it, so a jog
/// screen that had never opened a program kept `preview_offset = None`, the
/// marker shift collapsed to `work - machine`, and the toolhead was drawn on
/// its work coordinate: machine home at WCS zero, off by the whole offset
/// before that.
#[test]
fn a_jog_screen_without_a_program_still_frames_the_offset() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.program.editor.replace_text(String::new());
    harness.app.snapshot.offsets.active_index = Some(1);
    if let Some(row) = harness
        .app
        .snapshot
        .offsets
        .offsets
        .iter_mut()
        .find(|row| row.index == 1)
    {
        row.values.insert("X", -156.381);
        row.values.insert("Y", -67.696);
        row.values.insert("Z", -89.944);
    }
    if let Some(position) = harness.app.snapshot.positions.axes.get_mut("X") {
        position.machine = -156.381;
        position.work = 0.0;
    }
    harness.app.refresh_preview();
    let offset = harness
        .app
        .program
        .preview_offset
        .clone()
        .expect("the controller's offset is the frame, program or not");
    assert_eq!(offset.get("X").copied(), Some(-156.381));
    assert!(
        harness.app.program.preview.segments.is_empty(),
        "no program"
    );
    // The marker lands where the work reading already is: its shift is zero,
    // not the whole offset.
    let shift = crate::state::live_trail_shift(&harness.app.snapshot, Some(&offset));
    assert!(shift[0].abs() < 1e-9, "marker shift {shift:?}");
}

/// The signature is the numbers the frame depends on, in a stable order:
/// the same rows must compare equal however the map was built, and a single
/// moved value must compare different.
#[test]
fn the_offset_signature_sees_every_row_and_axis() {
    let rows = |x: f64| {
        (1..=9u8)
            .map(|index| linc_core::model::WorkOffset {
                index,
                values: linc_core::model::AxisMap::from_entries([
                    ("X".to_string(), x + index as f64),
                    ("Y".to_string(), -(index as f64)),
                    ("Z".to_string(), 0.0),
                ]),
                rotation: None,
            })
            .collect::<Vec<_>>()
    };
    let base = super::offsets_signature(&rows(0.0));
    assert_eq!(base.len(), 27, "nine rows of three axes");
    assert_eq!(base, super::offsets_signature(&rows(0.0)));
    let mut moved = rows(0.0);
    moved[0].values.insert("X", -156.381);
    assert_ne!(
        base,
        super::offsets_signature(&moved),
        "a published G54 must rebuild the preview"
    );
}

/// The coordinate line keeps its columns while the numbers move. A field
/// sized by the value on screen widened the moment an axis crossed 10.000,
/// which shuffled every field to its right - and the machine moves while the
/// operator is reading them.
#[test]
fn the_coordinate_line_keeps_its_columns_as_the_numbers_move() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Program);
    let line_with = |harness: &mut ProbeHarness, work: f64, dtg: f64| {
        for axis in ["X", "Y", "Z"] {
            if let Some(entry) = harness.app.snapshot.positions.axes.get_mut(axis) {
                entry.work = if axis == "X" { work } else { 0.0 };
                entry.dtg = if axis == "X" { dtg } else { 0.0 };
            }
        }
        harness.app.coordinate_status_line()
    };
    let small = line_with(&mut harness, 1.0, 0.5);
    let large = line_with(&mut harness, -123.456, -300.0);
    assert_eq!(
        small.len(),
        large.len(),
        "same columns for both: {small:?} vs {large:?}"
    );
    assert!(large.contains("-123.456"), "{large}");
    assert!(
        small.contains("+1.000"),
        "the small value is padded, not left ragged: {small}"
    );
}

/// The centre of the bottom bar carries every axis position, then the same
/// axes with their distance to go, and the single moving-axis field the bar
/// otherwise shows stands down while it does.
#[test]
fn the_bottom_bar_groups_the_positions_then_their_dtg() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Program);
    assert!(harness.app.shows_coordinate_status());
    let line = harness.app.coordinate_status_line();
    let (position, dtg) = line.split_once("  DTG: ").expect("two groups");
    for axis in ["X", "Y", "Z"] {
        assert!(
            position.contains(axis),
            "{axis} in the position group: {line}"
        );
        assert!(dtg.contains(axis), "{axis} in the DTG group: {line}");
    }
    assert_eq!(position.matches('X').count(), 1, "{line}");
    assert_eq!(dtg.matches('Z').count(), 1, "{line}");

    harness.frame(&[]);
    let texts = painted_text(&harness.frame_full(Vec::new()));
    let painted = texts
        .iter()
        .find(|(text, rect)| rect.center().y > 690.0 && text == &line)
        .map(|(_, rect)| *rect);
    let Some(rect) = painted else {
        panic!(
            "the bar paints {line}, got {:?}",
            texts
                .iter()
                .filter(|(_, r)| r.center().y > 690.0)
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
        );
    };
    // Left-aligned in the flow: it starts after the machine fields (LN, HOMED)
    // rather than floating in the middle of the bar.
    assert!(
        rect.left() > 60.0 && rect.left() < 320.0,
        "the readout starts after the left-hand fields, at {} of a 1280 px bar",
        rect.left()
    );
    for (text, field) in &texts {
        if (text.starts_with("LN ") || text == "HOMED") && field.center().y > 690.0 {
            assert!(
                field.right() < rect.left(),
                "{text} ends before the readout begins: {} vs {}",
                field.right(),
                rect.left()
            );
        }
    }
    assert!(
        !texts
            .iter()
            .any(|(text, rect)| text.starts_with("DTG ") && rect.center().y > 690.0),
        "the single moving-axis DTG field stands down"
    );

    harness.app.set_page(Page::Machine);
    assert!(!harness.app.shows_coordinate_status());
}

/// The zoom chords must not depend on where the trackball happens to be:
/// Ctrl+0 was always page-scoped while Ctrl+Plus/Minus were hover-scoped,
/// which is why they worked only sometimes.
#[test]
fn the_zoom_chords_work_wherever_the_pointer_is_and_either_plus_key() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Program);
    harness.app.program.graph_auto_fit = false;
    harness.frame(&[]);

    // Pointer nowhere near the toolpath: the chord still zooms.
    let before = harness.app.program.graph_zoom;
    harness.frame_with(&[(egui::Key::Plus, ctrl())]);
    assert!(
        harness.app.program.graph_zoom > before,
        "Ctrl+Plus zooms without hovering the card: {} -> {}",
        before,
        harness.app.program.graph_zoom
    );

    // The main-row plus is Shift+equals, which egui reports as Equals.
    let before = harness.app.program.graph_zoom;
    harness.frame_with(&[(
        egui::Key::Equals,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    )]);
    assert!(
        harness.app.program.graph_zoom > before,
        "Shift+equals zooms in too: {} -> {}",
        before,
        harness.app.program.graph_zoom
    );

    // And it reaches past the old 40 ceiling. The ceiling itself belongs to
    // the view now (`linc_view::max_zoom`: 0.01 mm across the card), which
    // no page has drawn in this frame, so the chord simply keeps going.
    harness.app.program.graph_zoom = 70.0;
    harness.frame_with(&[(egui::Key::Plus, ctrl())]);
    assert!(
        harness.app.program.graph_zoom > 80.0,
        "70 zooms on past the old fixed 80 ceiling, got {}",
        harness.app.program.graph_zoom
    );
}

/// The jog and homing screens no longer carry a MODE CONTEXT pane.
#[test]
fn the_jog_screens_have_no_mode_context_pane() {
    for page in [Page::Manual, Page::Homing] {
        let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
        harness.app.set_page(page);
        harness.frame(&[]);
        let texts = painted_text(&harness.frame_full(Vec::new()));
        assert!(
            !texts.iter().any(|(text, _)| text == "MODE CONTEXT"),
            "{page:?} still paints a mode context pane"
        );
    }
}

#[test]
fn the_override_readout_follows_the_panel_order() {
    assert_eq!(override_readout(1.0, 0.5, 0.25), "OVR F100 R50 S25");
    assert_eq!(override_readout(0.0, 2.0, 1.0), "OVR F0 R200 S100");
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts = painted_text(&output);
    assert!(
        texts.iter().any(|(text, _)| text == "OVR F100 R100 S100"),
        "the bar paints the panel order, got {:?}",
        texts
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        !texts.iter().any(|(text, _)| text == "OVR F100 S100 R100"),
        "the old FEED, SPINDLE, RAPID order is gone"
    );
}

/// The rows are as tight as a legible label allows. A short pill inside a
/// tall row is what read as padding before, so the row is now only as tall
/// as the pill plus a hair, and the operator asked for that space back.
#[test]
fn the_navigation_rows_are_tight_and_the_pill_fills_them() {
    for (page, _, _) in NAV_ITEMS {
        let geometry = nav_row_geometry(page);
        assert_eq!(
            geometry.height,
            theme::NAV_ROW_HEIGHT,
            "{page:?} keeps the tight row height"
        );
        assert!(
            geometry.height - geometry.pill <= 2.0,
            "{page:?} leaves at most one pixel above and below the pill: {} of {}",
            geometry.pill,
            geometry.height
        );
    }
    for page in [Page::Machine, Page::Program, Page::Tools, Page::Offsets] {
        assert!(nav_row_geometry(page).icon, "{page:?} keeps its icon");
        assert_eq!(nav_row_geometry(page).pill, theme::NAV_PILL_PRIMARY);
    }
    for page in [Page::Diagnostics, Page::Help] {
        assert!(!nav_row_geometry(page).icon, "{page:?} is a one-line row");
        assert_eq!(nav_row_geometry(page).pill, theme::NAV_PILL_SECONDARY);
    }
    assert_eq!(theme::NAV_ROW_GAP, 2.0);
    // Seven rows now cost less than seven 44 px targets did.
    assert!(theme::NAV_ROW_HEIGHT < theme::MIN_TARGET);
}

/// Measured at 1280x720, from the frame's own shapes: six rows of 28 px with
/// a 2 px gap, 26/24 px pills. DIAGNOSTICS and G-CODE HELP are one-line rows
/// whose labels start where the pill does, with no icon and no keycap under
/// them; SETTINGS has no row at all and is reached with its chord.
#[test]
fn the_navigation_is_compact_and_lists_six_pages() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts = painted_text(&output);
    let nav = region(&harness.app, "nav");
    let in_nav = |text: &str| {
        texts
            .iter()
            .find(|(painted, rect)| painted == text && rect.left() < theme::NAV_WIDTH)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{text} is a navigation row"))
    };
    for label in ["MACHINE", "PROGRAM", "TOOLS", "OFFSETS", "DIAG", "G-CODE"] {
        in_nav(label);
    }
    assert!(
        !texts
            .iter()
            .any(|(text, rect)| text == "SETTINGS" && rect.left() < theme::NAV_WIDTH),
        "SETTINGS has no navigation row"
    );
    // A one-line row keeps its chord on the label's own line, at the pill's
    // right edge, instead of stacking it underneath.
    for (label, keycap) in [("DIAG", "Ctrl+D"), ("G-CODE", "Ctrl+H")] {
        let label_rect = in_nav(label);
        let keycap_rect = in_nav(keycap);
        assert!(
            (keycap_rect.center().y - label_rect.center().y).abs() < 1.5,
            "{keycap} shares the {label} row: {keycap_rect:?} vs {label_rect:?}"
        );
        assert!(
            keycap_rect.left() > label_rect.right(),
            "{keycap} sits to the right of {label}"
        );
    }
    // Six pills: four that carry an icon and a keycap, two one-liners.
    let mut heights: Vec<f32> = painted_rects(&output)
        .into_iter()
        .filter(|rect| {
            rect.left() >= nav.left() && rect.right() <= nav.right() && rect.width() > 100.0
        })
        .map(|rect| rect.height())
        .filter(|height| *height > 10.0 && *height < 40.0)
        .collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(heights.len(), 6, "six painted rows, got {heights:?}");
    // One list: every row paints the same pill height, so the list reads as a
    // stack instead of alternating tall and short blocks. What differs between
    // the kinds is the icon and whether a keycap is drawn beside the label.
    assert_eq!(
        heights
            .iter()
            .filter(|height| (**height - theme::NAV_PILL_PRIMARY).abs() < 0.5)
            .count(),
        6,
        "{heights:?}"
    );
    assert_eq!(
        theme::NAV_PILL_SECONDARY,
        theme::NAV_PILL_PRIMARY,
        "a one-line row paints the same pill as the others"
    );
    // Pitch: the target plus the gap, from the labels that sit in them.
    let pitch = in_nav("PROGRAM").center().y - in_nav("MACHINE").center().y;
    assert!(
        (pitch - (nav_row_geometry(Page::Machine).height + theme::NAV_ROW_GAP)).abs() < 0.6,
        "row pitch {pitch}"
    );
    // A one-line row starts at the pill's inset: no icon column to clear.
    assert!(
        in_nav("DIAG").left() < in_nav("MACHINE").left() - 15.0,
        "DIAG is a one-line row: {} vs {}",
        in_nav("DIAG").left(),
        in_nav("MACHINE").left()
    );
    // The list ends a few pixels above the SHORTCUTS frame, not a band of air.
    let shortcuts = in_nav_top(&texts, "SHORTCUTS");
    // From the end of the last row's target to the SHORTCUTS frame's own
    // margin: the two NAV_BOTTOM_GAPs and the separator's lines, not a band
    // of air. It was 35 px before the list was compacted.
    let last_target_bottom =
        in_nav("G-CODE").center().y + nav_row_geometry(Page::Help).height * 0.5;
    assert!(
        shortcuts - last_target_bottom < 20.0,
        "bottom padding {}",
        shortcuts - last_target_bottom
    );
    // Ctrl+M is REMAINING: the view changes, the page does not.
    let page = harness.app.nav.page;
    harness.frame_with(&[(egui::Key::M, ctrl())]);
    assert!(
        harness.app.graph_style.remaining,
        "Ctrl+M turns REMAINING on"
    );
    assert_eq!(harness.app.nav.page, page, "a view mode, not a page");
    // The pages behind the rows still answer their own chords.
    harness.frame_with(&[(egui::Key::D, ctrl())]);
    assert_eq!(harness.app.nav.page, Page::Diagnostics);
    harness.frame_with(&[(egui::Key::H, ctrl())]);
    assert_eq!(harness.app.nav.page, Page::Help);
}

/// INC shows the steps and takes their numbers; JOG is continuous and MPG
/// selects its step on the handwheel, so both get neither and their digits
/// stay free with no wrong value on screen.
#[test]
fn the_increment_row_and_its_numbers_belong_to_the_step_modes() {
    let steps = || {
        vec![
            ("10mm".to_string(), 10.0),
            ("1mm".to_string(), 1.0),
            (".1mm".to_string(), 0.1),
            (".01mm".to_string(), 0.01),
        ]
    };

    let mut harness = ProbeHarness::new(PanelMode::Inc, false, true);
    harness.app.jog.increments = steps();
    harness.app.jog.index = 0;
    harness.app.set_page(Page::Manual);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts: Vec<String> = painted_text(&output)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    for label in ["10mm", "1mm", ".1mm", ".01mm"] {
        assert!(
            texts.iter().any(|text| text == label),
            "every step is offered: {label} missing from {texts:?}"
        );
    }
    // The number is the key and sits outside the chip, so the button keeps
    // the value alone.
    assert!(
        texts
            .iter()
            .filter(|text| ("1"..="4").contains(&text.as_str()))
            .count()
            >= 4,
        "the four digits are painted next to their steps: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "1-4"),
        "the row says which numbers it takes: {texts:?}"
    );

    harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.jog.index, 2, "3 picks the third step");
    assert_eq!(harness.app.jog_increment_value(), 0.1);
    harness.frame_with(&[(egui::Key::Num9, egui::Modifiers::NONE)]);
    assert_eq!(
        harness.app.jog.index, 2,
        "a digit past the last step is inert"
    );

    // JOG: no row, and the digits do nothing.
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.jog.increments = steps();
    harness.app.jog.index = 0;
    harness.app.set_page(Page::Manual);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts: Vec<String> = painted_text(&output)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert!(
        !texts.iter().any(|text| text == "10mm") && !texts.iter().any(|text| text == "INCREMENT"),
        "a continuous jog shows no increment: {texts:?}"
    );
    harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.jog.index, 0, "and takes no digits");

    // MPG: the handwheel owns the step, so the [DISPLAY] INCREMENTS row is
    // not drawn here either and the digits are not eaten.
    let mut harness = ProbeHarness::new(PanelMode::Mpg, false, true);
    harness.app.jog.increments = steps();
    harness.app.jog.index = 0;
    harness.app.set_page(Page::Manual);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts: Vec<String> = painted_text(&output)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert!(
        !texts.iter().any(|text| text == "10mm") && !texts.iter().any(|text| text == "INCREMENT"),
        "MPG picks its step on the handwheel: {texts:?}"
    );
    harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.jog.index, 0, "and takes no digits");
}

/// The style, not the page: REMAINING is what stops the trail being drawn,
/// and the eaten line reaches the view as it is.
#[test]
fn remaining_takes_the_live_trail_out_of_the_style() {
    let (_dir, profile) = fixture();
    let mut style = crate::state::graph_style(&profile);
    style.show_live_trail = true;
    style.remaining = true;
    let preview = linc_core::gcode::Preview::empty();
    let mut view = linc_view::TrajectoryView::new(&preview, &[], profile.variant);
    style.apply(&mut view, None, None, Some(2));
    assert!(!view.show_live_trail, "REMAINING draws no trail");
    assert!(view.remaining, "the mode reaches the view");
    assert_eq!(view.eaten_through, Some(2), "and so does the eaten line");

    style.remaining = false;
    style.apply(&mut view, None, None, None);
    assert!(
        view.show_live_trail,
        "the trail comes back with the mode off"
    );
    assert_eq!(view.eaten_through, None);
}

fn in_nav_top(texts: &[(String, egui::Rect)], label: &str) -> f32 {
    texts
        .iter()
        .find(|(text, rect)| text == label && rect.left() < theme::NAV_WIDTH)
        .map(|(_, rect)| rect.top())
        .unwrap_or_else(|| panic!("{label} is painted in the sidebar"))
}

/// The arrows walk the navigation over all six rows, and Enter opens the
/// page the highlighted row names.
#[test]
fn the_arrow_keys_walk_all_six_navigation_rows() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    harness.app.focus_nav();
    harness.frame(&[]);
    assert_eq!(harness.app.nav.pages.len(), 6, "six rows to walk");
    assert_eq!(harness.app.nav.nav_focus, 0);
    for expected in 1..6 {
        harness.frame(&[egui::Key::ArrowDown]);
        assert_eq!(
            harness.app.nav.nav_focus, expected,
            "ArrowDown reaches row {expected}"
        );
        assert_eq!(harness.app.nav.focus_region, FocusRegion::Nav);
    }
    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(
        harness.app.nav.nav_focus, 5,
        "the last row is the end of the list"
    );
    harness.app.nav.nav_focus = 5;
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(harness.app.nav.page, Page::Help, "Enter opens G-CODE HELP");
    harness.app.focus_nav();
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.nav.nav_focus, 4, "ArrowUp walks back");
}

/// A shell region's rect, from the frame's own report to the diagnostic
/// bus: the tests read the footer and the navigation column from it rather
/// than re-deriving them.
fn region(app: &LincApp, name: &str) -> egui::Rect {
    let found = app
        .diagnostics
        .regions
        .iter()
        .find(|region| region.name == name)
        .unwrap_or_else(|| panic!("{name} is reported"));
    egui::Rect::from_min_size(
        egui::pos2(found.x, found.y),
        egui::vec2(found.width, found.height),
    )
}

/// The SHORTCUTS pane sizes to its content while that fits, and takes the
/// sidebar's remainder — down to the footer, less a little air — once the
/// list is longer, scrolling inside itself.
#[test]
fn the_shortcuts_pane_sizes_to_content_then_to_the_footer() {
    // Diagnostics has one hint: the pane ends well above the footer.
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Diagnostics);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let nav = region(&harness.app, "nav");
    let pane = shortcuts_frame(&output).expect("the shortcuts pane is drawn");
    assert!(
        pane.bottom() < nav.bottom() - 40.0,
        "a short list keeps the pane at its content height: {pane:?} in {nav:?}"
    );

    // A profile that lists every navigation page still fits at 1280x720:
    // after the rows were compacted the Program page's list stays
    // content-sized instead of running to the footer. It must never pass
    // the navigation column's content edge.
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.nav.pages = nav_pages(
        &[
            "machine",
            "program",
            "tools",
            "offsets",
            "diagnostics",
            "help",
        ]
        .map(str::to_string),
    );
    harness.app.set_page(Page::Program);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let nav = region(&harness.app, "nav");
    // The column's own 8 px bottom margin and the 4 px that keep the pane
    // off the footer.
    let pane = shortcuts_frame(&output).expect("the shortcuts pane is drawn");
    assert!(
        pane.bottom() <= nav.bottom() - 8.0 - 4.0 + 1.5,
        "the pane never passes the navigation column: {pane:?} in {nav:?}"
    );
}

/// Ctrl+Plus / Ctrl+Minus belong to the toolpath under the pointer. They do
/// not rescale the shell: `[ui] scale` is the only source of that.
#[test]
fn ctrl_plus_and_minus_leave_the_ui_scale_alone() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.frame(&[]);
    let before = harness.ctx.zoom_factor();
    harness.frame_with(&[(egui::Key::Plus, ctrl())]);
    assert!(
        (harness.ctx.zoom_factor() - before).abs() < 0.005,
        "Ctrl+Plus rescaled the shell: {before} -> {}",
        harness.ctx.zoom_factor()
    );
    harness.frame_with(&[(egui::Key::Minus, ctrl())]);
    assert!(
        (harness.ctx.zoom_factor() - before).abs() < 0.005,
        "Ctrl+Minus rescaled the shell: {before} -> {}",
        harness.ctx.zoom_factor()
    );
}

/// MAIN ends on one line: the program card on the right and the two cards
/// under the toolpath stop at the footer's top edge, and the MODAL band is
/// pinned above the progress line with no dead space under it.
#[test]
fn the_main_page_cards_end_on_the_page_bottom_edge() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let footer = region(&harness.app, "bottom-bar");
    let cards = painted_cards(&output);
    // The right column's program card: the tall card past the middle.
    let program = cards
        .iter()
        .find(|rect| rect.left() > 900.0 && rect.height() > 300.0)
        .expect("the program card is drawn");
    assert!(
        (footer.top() - program.bottom()).abs() <= 3.0,
        "the program card ends at the page's edge: {program:?} vs {footer:?}"
    );
    // The two cards under the toolpath end on the same edge.
    let bottom_row: Vec<_> = cards
        .iter()
        .filter(|rect| rect.left() < 900.0 && rect.top() > 500.0)
        .collect();
    assert_eq!(
        bottom_row.len(),
        2,
        "feed/spindle and servo: {bottom_row:?}"
    );
    for rect in bottom_row {
        // The bottom row is a fixed height from `bottom_cards_height`, so it
        // lands within a couple of pixels of the same edge rather than on
        // it; the program card above is placed from the footer itself.
        assert!(
            (footer.top() - rect.bottom()).abs() <= 6.0,
            "every card ends at the page's edge: {rect:?} vs {footer:?}"
        );
    }
    // No dead space under the band: its cells end on the progress rule, and
    // the listing runs down to the band rather than stopping a row short.
    let texts = painted_text(&output);
    let modal = texts
        .iter()
        .find(|(text, _)| text == "MODAL")
        .expect("the MODAL band is drawn");
    let progress = texts
        .iter()
        .find(|(text, _)| text == "PROGRESS")
        .expect("the progress line is drawn");
    let band_bottom = texts
        .iter()
        .filter(|(_, rect)| rect.top() > modal.1.top() && rect.bottom() < progress.1.top())
        .map(|(_, rect)| rect.bottom())
        .fold(0.0f32, f32::max);
    assert!(
        progress.1.top() - band_bottom < 12.0,
        "the band ends on the progress rule: band {band_bottom}, progress {}",
        progress.1.top()
    );
    // The last listing line sits within one row of the band's rule.
    let listing_bottom = texts
        .iter()
        .filter(|(_, rect)| {
            rect.left() > program.left()
                && rect.top() > program.top()
                && rect.bottom() < modal.1.top()
        })
        .map(|(_, rect)| rect.bottom())
        .fold(0.0f32, f32::max);
    assert!(
        modal.1.top() - listing_bottom < 30.0,
        "the listing takes the space down to the band: {listing_bottom} vs {}",
        modal.1.top()
    );
}

#[test]
fn machine_bounds_follow_the_ini_and_classify_the_toolpath() {
    let (_dir, profile) = fixture();
    let bounds = machine_bounds(&profile).expect("the INI has X/Y/Z limits");
    assert_eq!(bounds.min, [-372.0, -346.0, -253.0]);
    assert_eq!(bounds.max, [5.0, 4.0, 19.0]);
    // The same box the toolpath painter receives. A move from X 0 to X 10
    // crosses MAX_LIMIT 5 halfway along, so the red starts halfway.
    assert_eq!(
        linc_view::travel::classify_segment([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], &bounds),
        linc_view::travel::TravelClass::Straddling {
            enter: 0.0,
            exit: 0.5,
        }
    );
}

#[test]
fn a_dialog_owns_the_keyboard_in_front_of_the_page() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Program);
    harness.app.overlay.modal = Some(Modal::Info("test".into()));

    harness.frame(&[egui::Key::F7]);
    assert_eq!(
        harness.app.nav.page,
        Page::Program,
        "F7 must not navigate behind the dialog"
    );

    harness.frame(&[egui::Key::Escape]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "Escape dismisses the dialog"
    );
}

/// A dialog that exists to fill a field puts the ring in that field on the
/// frame it opens: there is no click-to-focus and no Tab on this panel, so
/// a dialog that focused its safe button could not be typed into at all.
/// A dialog with no field keeps its safe button under the Enter key.
#[test]
fn a_dialog_with_a_field_focuses_the_field_not_its_safe_button() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness.app.set_page(Page::Program);
    harness.frame(&[]);

    // SAVE PROGRAM AS: Ctrl+S on a buffer with no file names it. It is
    // also what the unsaved-changes prompt turns into when SAVE has no
    // path to write, so the ring must move with the dialog, not only when
    // no dialog was open before.
    harness.app.overlay.modal = Some(Modal::Info("first".into()));
    harness.frame(&[]);
    harness.app.overlay.modal = Some(Modal::SaveProgramAs(crate::state::SaveProgramDialog {
        name: String::new(),
        directory: harness.app.save_as_directory(),
        error: None,
    }));
    harness.frame(&[]);
    assert!(
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-save-as-name"))),
        "the save-as dialog puts the ring in its name field"
    );
    harness.type_text("part");
    assert!(
        matches!(
            &harness.app.overlay.modal,
            Some(Modal::SaveProgramAs(dialog)) if dialog.name == "part"
        ),
        "typing without a click reaches the name field"
    );

    // The MDI buffer's save dialog does the same.
    harness.app.overlay.modal = None;
    harness.frame(&[]);
    harness.app.overlay.modal = Some(Modal::MdiSave(crate::state::MdiSaveDialog::default()));
    harness.frame(&[]);
    assert!(
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-mdi-save-name"))),
        "the MDI save dialog puts the ring in its name field"
    );
    harness.type_text("part2");
    assert!(
        matches!(
            &harness.app.overlay.modal,
            Some(Modal::MdiSave(dialog)) if dialog.name == "part2"
        ),
        "typing without a click reaches the name field"
    );

    // GO TO LINE collects a number the same way.
    harness.app.overlay.modal = None;
    harness.frame(&[]);
    harness.app.overlay.modal = Some(Modal::GoToLine {
        buffer: String::new(),
        mdi: false,
    });
    harness.frame(&[]);
    assert!(
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-goto-line-input"))),
        "the go-to-line dialog puts the ring in its number field"
    );
    harness.type_text("3");
    assert!(
        matches!(
            &harness.app.overlay.modal,
            Some(Modal::GoToLine { buffer, .. }) if buffer == "3"
        ),
        "typing without a click reaches the number field"
    );

    // A dialog with no field keeps its safe button under Enter.
    harness.app.overlay.modal = None;
    harness.frame(&[]);
    harness.app.overlay.modal = Some(Modal::Info("no field".into()));
    harness.frame(&[]);
    harness.frame(&[egui::Key::Enter]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "OK holds the ring in a dialog with no field"
    );
}

#[test]
fn enter_in_the_save_dialog_does_not_act_on_the_page_behind() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.files_page_with_one_file("behind.ngc");
    harness.frame(&[]);
    harness.app.overlay.modal = Some(Modal::MdiSave(crate::state::MdiSaveDialog {
        name: "part".into(),
        wrap: false,
        ..Default::default()
    }));
    harness.sent.borrow_mut().clear();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        harness.app.nav.page,
        Page::Files,
        "Enter must not open the selected file behind the save dialog"
    );
    assert!(
        !harness
            .sent
            .borrow()
            .iter()
            .any(|command| matches!(command, MachineCommand::LoadProgram { .. })),
        "the dialog leaked Enter into the FILE page: {:?}",
        harness.sent_debug()
    );
}

#[test]
fn zero_keys_do_not_fire_behind_a_dialog() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.frame(&[]);
    assert_eq!(harness.app.nav.page, Page::Manual);

    harness.sent.borrow_mut().clear();
    harness.app.overlay.modal = Some(Modal::Info("test".into()));
    harness.frame(&[egui::Key::A]);
    assert!(
        !harness.sent.borrow().iter().any(|command| matches!(
            command,
            MachineCommand::ZeroWorkOffset { .. } | MachineCommand::SetWorkPosition { .. }
        )),
        "A must not zero axes behind a dialog: {:?}",
        harness.sent_debug()
    );
}

#[test]
fn a_activates_the_selected_work_offset_only_on_the_offsets_page() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Offsets);
    harness.app.snapshot.offsets.active_index = Some(1);
    // G55. Keep the page's entry auto-select from resetting the row to the
    // active system if the test thread is descheduled for >200 ms before
    // the first frame. A future stamp makes that gap negative.
    harness.app.offsets.selected_row = 1;
    harness.app.offsets.last_frame_ms = linc_core::diag::now_ms() + 60_000;
    harness.frame(&[egui::Key::A]);
    assert!(
        harness
            .sent
            .borrow()
            .iter()
            .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { index: 2 })),
        "A on OFFSETS must activate the selected G55 row: {:?}",
        harness.sent_debug()
    );
    assert_eq!(harness.app.snapshot.offsets.active_index, Some(2));

    // An in-place edit owns the keyboard first.
    harness.sent.borrow_mut().clear();
    harness.app.offsets.selected_row = 1;
    harness.app.offsets.editing = true;
    harness.frame(&[egui::Key::A]);
    assert!(
        !harness
            .sent
            .borrow()
            .iter()
            .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { .. })),
        "A must not activate while a cell edit is open: {:?}",
        harness.sent_debug()
    );
    harness.app.offsets.editing = false;

    // The key is page-local: it must not reach the Offsets command from
    // another page.
    harness.sent.borrow_mut().clear();
    harness.app.set_page(Page::Machine);
    harness.frame(&[egui::Key::A]);
    assert!(
        !harness
            .sent
            .borrow()
            .iter()
            .any(|command| matches!(command, MachineCommand::ActivateWorkOffset { .. })),
        "A must not leak off OFFSETS: {:?}",
        harness.sent_debug()
    );
}

#[test]
fn probe_preferences_restore_only_operator_owned_values() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.probe.params.slow_feed = 0.0;
    app.probe.params.fast_feed = 234.5;
    app.probe.params.probe_mode = linc_core::probe::ProbeMode::ApplyWcs;
    app.probe.params.wcs_rotation = true;
    app.probe.params.x_hint_boss_pocket = 31.25;
    app.remember_probe_params();
    app.save_preferences(true);

    let saved = linc_core::preferences::Preferences::load(&app.profile.preferences_path);
    let saved_probe = saved.probe.as_ref().unwrap();
    assert_eq!(saved_probe.fast_feed, 234.5);
    assert_eq!(saved_probe.slow_feed, 0.0);
    // Serde defaults prove these machine/controller facts were not written.
    assert_eq!(saved_probe.probe_tool_number, 99);
    assert_eq!(saved_probe.calibration_offset, 0.0);

    app.profile.config.probe.tool_number = 42;
    app.snapshot.probe.calibration_offset = Some(0.375);
    app.probe.params.fast_feed = 999.0;
    let profile = app.profile.clone();
    let snapshot = app.snapshot.clone();
    let backend = Box::new(LocalBackend::new(profile.clone(), PanelMode::Auto));
    let app = LincApp::with_snapshot(profile, backend, snapshot);
    assert_eq!(app.probe.params.fast_feed, 234.5);
    assert_eq!(app.probe.params.slow_feed, 0.0);
    assert_eq!(app.probe.params.probe_tool_number, 42);
    assert_eq!(app.probe.params.calibration_offset, 0.375);
    assert_eq!(
        app.probe.params.probe_mode,
        linc_core::probe::ProbeMode::ApplyWcs
    );
    assert!(app.probe.params.wcs_rotation);
    assert_eq!(app.probe.params.x_hint_boss_pocket, 31.25);
}

#[test]
fn probe_sync_uses_active_wcs_and_new_calibration_without_touching_edit_buffer() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.snapshot.offsets.active_index = Some(2);
    app.snapshot.probe.selected_wcs = "G54".into();
    app.snapshot.probe.calibration_offset = Some(0.125);
    app.probe.param_selected = 1;
    app.probe.param_editing = true;
    app.probe.param_buffer = "77.5".into();
    app.sync_probe();
    assert_eq!(app.probe.wcs, "G55");
    assert_eq!(app.probe.params.calibration_offset, 0.125);
    assert!(app.probe.param_editing);
    assert_eq!(app.probe.param_buffer, "77.5");

    app.snapshot.probe.calibration_offset = Some(0.25);
    app.sync_probe();
    assert_eq!(app.probe.params.calibration_offset, 0.25);
    assert_eq!(app.probe.param_buffer, "77.5");
}

#[test]
fn a_touch_off_reaches_the_offset_table() {
    // The keys issue `G10 L20`; the table rebuilds from the snapshot's
    // revision. A constant revision left the table showing the old value
    // while the DRO moved, which read as "only the DRO changed".
    let (_dir, mut app) = make_app(PanelMode::Jog);
    app.poll_backend();
    let machine_x = app
        .snapshot
        .positions
        .axes
        .get("X")
        .map(|position| position.machine)
        .unwrap_or(0.0);
    crate::screens::common::zero_axes(&mut app, vec!["X".to_string()]);
    app.poll_backend();
    app.sync_offsets_from_snapshot(false);
    let row = app
        .offsets
        .rows
        .get(&1)
        .expect("G54 row present after a touch off");
    let value = row.values.get("X").cloned().unwrap_or_default();
    let parsed: f64 = value.parse().unwrap_or(f64::NAN);
    assert!(
        (parsed - machine_x).abs() < 1e-6,
        "G54 X should hold the machine position {machine_x}, got {value:?}"
    );
}

#[test]
fn mode_driven_pages_open_and_close() {
    let (_dir, mut app) = make_app(PanelMode::Mdi);
    assert_eq!(app.nav.page, Page::Machine, "MDI opens the MAIN page");
    assert!(app.mdi_pane_visible());
    switch_mode(&mut app, PanelMode::Auto);
    assert_eq!(app.nav.page, Page::Machine, "AUTO brings up MAIN");
    assert!(
        !app.mdi_pane_visible(),
        "and the normal program card takes the pane's slot"
    );
    // From anywhere else AUTO lands on MAIN as well.
    switch_mode(&mut app, PanelMode::Mdi);
    app.set_page(Page::Files);
    switch_mode(&mut app, PanelMode::Auto);
    assert_eq!(app.nav.page, Page::Machine, "AUTO brings up MAIN");
    app.on_mode_change(PanelModeValidity::Valid(PanelMode::Home));
    assert_eq!(app.nav.page, Page::Homing);
    app.on_mode_change(PanelModeValidity::Valid(PanelMode::Jog));
    assert_eq!(app.nav.page, Page::Manual);
    app.on_mode_change(PanelModeValidity::Valid(PanelMode::Edit));
    assert_eq!(app.nav.page, Page::Program);
    assert_eq!(app.program.view, ProgramView::Code);
}

#[test]
fn edit_mode_starts_in_code_with_editor_focus() {
    let (_dir, app) = make_app(PanelMode::Edit);
    assert_eq!(app.nav.page, Page::Program);
    assert_eq!(app.program.view, ProgramView::Code);
    assert!(app.program.focus_editor);
    assert!(app.editing_allowed());
}

#[test]
fn unsaved_program_guards_navigation() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.program.editor.replace_text("G0 X0\nG1 X1\n".into());
    assert!(app.program_dirty());
    app.request_page(Page::Tools);
    assert!(matches!(
        app.overlay.modal,
        Some(Modal::ConfirmDiscard(PendingAction::Navigate(Page::Tools)))
    ));
    assert_eq!(app.nav.page, Page::Program);
    app.overlay.modal = None;
    app.perform_pending(PendingAction::Navigate(Page::Tools));
    assert_eq!(app.nav.page, Page::Tools);
    // The buffer survives a non-destructive page change.
    assert_eq!(app.program.editor.text(), "G0 X0\nG1 X1\n");
}

/// DISCARD is the operator agreeing to lose the edits, so it must drop the
/// buffer rather than carry it through the navigation: leaving it dirty is
/// what made the same prompt reappear on every trip to Files.
#[test]
fn discard_reverts_the_buffer_to_the_loaded_program() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    let loaded = app.snapshot.program.content.clone().expect("demo loaded");
    app.program.editor.replace_text("G0 X9\n".into());
    assert!(app.program_dirty());
    app.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::OpenFiles));
    // What the DISCARD button does.
    app.discard_editor_changes();
    app.perform_pending(PendingAction::OpenFiles);
    assert_eq!(app.nav.page, Page::Files);
    assert!(!app.program_dirty(), "the buffer is clean again");
    assert_eq!(app.program.editor.text(), *loaded);
    assert_eq!(app.program.selected_line, 1);
}

/// A program typed straight into EDIT has no file; the save asks for a name
/// instead of failing with "No program path".
#[test]
fn an_untitled_buffer_saves_under_a_name_it_asks_for() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.snapshot.program.path = None;
    app.snapshot.program.content = None;
    app.program.editor = linc_core::editor::Editor::new();
    app.program.editor.replace_text("G0 X0\nG1 X10\n".into());
    app.save_program();
    let Some(Modal::SaveProgramAs(dialog)) = app.overlay.modal.clone() else {
        panic!("the save asks for a name, got {:?}", app.overlay.modal);
    };
    assert!(
        dialog.directory.starts_with(app.profile.nc_files_root()),
        "the name lands under NC_FILES: {:?}",
        dialog.directory
    );
    app.save_program_as("part");
    app.poll_backend();
    assert!(app.overlay.modal.is_none());
    let path = app
        .program
        .editor
        .path
        .clone()
        .expect("the buffer has a path");
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        "part.ngc",
        "the configured G-code extension is appended"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "G0 X0\nG1 X10\n");
    assert!(!app.program_dirty(), "the save clears the marker");
}

/// MAIN is the screen for the mode in front of the operator, not always the
/// AUTO dashboard; on the Program page it does not also flip SPLIT/CODE.
#[test]
fn main_opens_the_page_the_panel_mode_owns() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    assert_eq!(app.nav.page, Page::Program);
    app.request_mode_page();
    assert_eq!(
        app.nav.page,
        Page::Program,
        "already there, and not toggled"
    );
    assert_eq!(app.program.view, ProgramView::Code);
    app.set_page(Page::Files);
    app.request_mode_page();
    assert_eq!(app.nav.page, Page::Program);

    switch_mode(&mut app, PanelMode::Jog);
    app.set_page(Page::Tools);
    app.request_mode_page();
    assert_eq!(app.nav.page, Page::Manual);

    switch_mode(&mut app, PanelMode::Home);
    app.set_page(Page::Tools);
    app.request_mode_page();
    assert_eq!(app.nav.page, Page::Homing);

    switch_mode(&mut app, PanelMode::Auto);
    app.set_page(Page::Tools);
    app.request_mode_page();
    assert_eq!(app.nav.page, Page::Machine);
}

/// The panel moves the mode itself, so leaving EDIT is the last chance to
/// say that AUTO can only start the loaded file.
#[test]
fn leaving_edit_with_unsaved_changes_warns_once() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.program.editor.replace_text("G0 X1\n".into());
    switch_mode(&mut app, PanelMode::Auto);
    assert!(matches!(app.overlay.modal, Some(Modal::UnsavedOnLeave)));
    // KEEP leaves the buffer alone; it is still there when EDIT comes back.
    app.overlay.modal = None;
    assert!(app.program_dirty());
    switch_mode(&mut app, PanelMode::Edit);
    assert!(app.program_dirty());
    // A clean buffer leaves EDIT without a prompt.
    app.discard_editor_changes();
    switch_mode(&mut app, PanelMode::Auto);
    assert!(app.overlay.modal.is_none());
}

#[test]
fn protect_unknown_fails_closed_in_the_ui() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.snapshot.panel.program_protect = false;
    app.snapshot.panel.program_protect_known = false;
    assert!(!app.editing_allowed());
    assert!(app.program_protected());
    app.program.editor.replace_text("G0 X1\n".into());
    let before = app.program.editor.text();
    app.save_program();
    assert_eq!(app.program.editor.text(), before);
    assert!(app.program.editor.dirty);
    assert!(app.overlay.modal.is_none());
}

#[test]
fn editor_preview_updates_only_after_valid_parse() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.program
        .editor
        .replace_text("G0 X0 Y0\nG1 X10 Y10 F300\n".into());
    app.refresh_preview();
    assert!(app.program.preview.error.is_none());
    assert_eq!(app.program.last_valid_preview.segments.len(), 2);
    app.program.editor.replace_text("G0 X0 Y0\nG1 X\n".into());
    app.refresh_preview();
    assert!(app.program.preview.error.is_some());
    assert_eq!(app.program.last_valid_preview.segments.len(), 2);
}

#[test]
fn manual_tool_change_prompt_and_confirmation_flow() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.tools.target = "12".into();
    screens::tools::issue_change(&mut app);
    app.poll_backend();
    assert_eq!(
        app.snapshot.tool_change.status,
        linc_core::model::ToolChangeStatus::ManualPrompt
    );
    let token = app.snapshot.tool_change.token.clone().unwrap();
    app.send(MachineCommand::ConfirmManualToolChange { token });
    app.poll_backend();
    // The simulator models an instantaneous handshake, so it completes at
    // once. The LinuxCNC backend holds the prompt until `iocontrol` drops
    // the request; that level is covered in linc-linuxcnc.
    assert_eq!(
        app.snapshot.tool_change.status,
        linc_core::model::ToolChangeStatus::Complete
    );
    assert_eq!(app.snapshot.hal.tool_change_confirm_seq, 1);
}

#[test]
fn clear_trail_is_view_only_and_sends_backend_request() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    assert!(!app.snapshot.live_trail.is_empty());
    app.clear_trail();
    app.poll_backend();
    assert!(app.snapshot.live_trail.len() <= 1);
    assert_eq!(app.snapshot.program.line_count > 0, true);
}
#[test]
fn jump_to_line_clamps_and_selects() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.program.editor.replace_text("L1\nL2\nL3\nL4\n".into());
    app.jump_to_line(3);
    assert_eq!(app.program.selected_line, 3);
    assert_eq!(app.program.code_scroll_to_line, Some(3));
    assert_eq!(app.program.cursor_line, Some(3));
    app.jump_to_line(0);
    assert_eq!(app.program.selected_line, 1);
    app.jump_to_line(9999);
    assert_eq!(
        app.program.selected_line,
        app.program.editor.line_count().max(1)
    );
}

#[test]
fn a_manual_line_pick_survives_the_running_follow() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.snapshot.task.running = true;
    app.program.follow_current = false;
    app.program.program_was_running = false;
    app.sync_program();
    assert!(app.program.follow_current, "a run start re-arms follow");
    // Ctrl+G or the arrows picked a line: the next poll must not drag it
    // back to the executing one.
    app.program.follow_current = false;
    app.sync_program();
    assert!(!app.program.follow_current, "the operator's pick is kept");
    // The next run re-arms it again.
    app.snapshot.task.running = false;
    app.sync_program();
    app.snapshot.task.running = true;
    app.sync_program();
    assert!(app.program.follow_current);
}

#[test]
fn escape_leaves_the_help_page_before_it_resets_a_running_program() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.poll_backend();
    app.snapshot.task.running = true;
    app.snapshot.task.interp = linc_core::model::InterpState::Reading;
    app.help.query = "G71".into();
    app.set_page(Page::Help);
    let back = app.nav.help_return_page;
    assert_eq!(back, Page::Machine, "Help remembers the page it came from");
    app.escape();
    assert_eq!(
        app.nav.page, back,
        "Esc leaves the Help page for the main view"
    );
    assert!(
        !app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "leaving the Help page must not reset the program"
    );
    app.escape();
    assert!(
        app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "the next Esc on the main view is the program reset"
    );
}

#[test]
fn escape_closes_the_help_dialog_before_resetting_a_running_program() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.poll_backend();
    app.snapshot.task.running = true;
    app.snapshot.task.interp = linc_core::model::InterpState::Reading;
    app.help.open_dialog("g0");
    app.escape();
    assert!(app.help.dialog.is_none(), "Escape closes the Help viewer");
    assert!(
        !app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "the running program behind the Help viewer must not be reset"
    );
}

#[test]
fn escape_leaves_the_diagnostics_page_before_it_resets_a_running_program() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.poll_backend();
    app.set_page(Page::Program);
    app.snapshot.task.running = true;
    app.snapshot.task.interp = linc_core::model::InterpState::Reading;
    app.set_page(Page::Diagnostics);
    app.escape();
    assert_eq!(
        app.nav.page,
        Page::Program,
        "Esc leaves DIAGNOSTICS for the page it came from"
    );
    assert!(
        !app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "looking at a log must not stop the program"
    );
    // The next Esc, on that page, is the one that resets it.
    app.escape();
    assert!(
        app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "Esc still resets once the operator is back on the page"
    );
}

#[test]
fn escape_resets_a_latched_probe_trip_from_the_diagnostics_page() {
    use linc_core::faults::{ProbeTripFault, ProbeTripKind};
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.poll_backend();
    let fault = ProbeTripFault::new(
        ProbeTripKind::JointJog,
        ProbeTripKind::JointJog.message(),
        1,
    );
    // The banner's button has to name the key that does the same thing: the
    // panel keyboard is the operator's only input.
    assert!(
        PROBE_TRIP_ACTION.contains("ESC"),
        "the action button names Esc: {PROBE_TRIP_ACTION}"
    );
    assert!(
        probe_trip_banner_text(&fault).contains("ESC"),
        "the banner names Esc too"
    );
    app.snapshot.probe_trip = Some(fault);
    app.set_page(Page::Program);
    app.set_page(Page::Diagnostics);
    app.escape();
    app.poll_backend();
    assert_eq!(
        app.nav.page,
        Page::Diagnostics,
        "the fault reset must not navigate away from the page the operator is reading"
    );
    assert!(
        !app.snapshot.probe_trip_active(),
        "Esc resets the latched trip from DIAGNOSTICS"
    );
    assert!(
        app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROBE TRIP RESET"),
        "the reset is acknowledged: {:?}",
        app.overlay
            .toasts
            .iter()
            .map(|toast| &toast.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn escape_stops_a_running_program() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.poll_backend();
    app.snapshot.task.running = true;
    app.snapshot.task.interp = linc_core::model::InterpState::Reading;
    app.escape();
    app.poll_backend();
    assert!(!app.snapshot.task.running);
    assert_eq!(
        app.snapshot.task.interp,
        linc_core::model::InterpState::Idle
    );
    assert!(
        app.overlay
            .toasts
            .iter()
            .any(|toast| toast.text == "PROGRAM RESET"),
        "the reset is acknowledged: {:?}",
        app.overlay
            .toasts
            .iter()
            .map(|toast| &toast.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_sidebar_offers_reset_while_a_program_is_active() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.snapshot.task.running = true;
    let shows_reset = |app: &LincApp| {
        app.key_hints().iter().any(|group| {
            group
                .items
                .iter()
                .any(|(key, label)| key == "Esc" && *label == "RESET")
        })
    };
    assert!(shows_reset(&app));
    app.snapshot.task.running = false;
    app.snapshot.task.interp = linc_core::model::InterpState::Idle;
    app.snapshot.program.paused = false;
    assert!(!shows_reset(&app));
}

#[test]
fn servo_load_rows_follow_the_profile_and_the_machine() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    app.profile.config.ui.servo_load = vec!["X".into(), "S".into(), "A".into()];
    app.snapshot.machine.servo_load.insert("X", 12.0);
    app.snapshot.machine.servo_load.insert("A", 4.0);
    app.snapshot.spindles.clear();
    assert_eq!(
        app.servo_load_rows(),
        vec![("X".to_string(), 12.0), ("A".to_string(), 4.0)],
        "S is skipped when the machine has no spindle load"
    );
    app.snapshot.spindles.push(linc_core::model::SpindleState {
        index: 0,
        name: "MAIN SPINDLE".into(),
        commanded_speed: 4200.0,
        actual_speed: 4185.0,
        load: Some(38.0),
        override_factor: 1.0,
        direction: 1,
        on: true,
        live_tool: false,
        live_tool_axial: false,
        live_tool_max_rpm: 0.0,
    });
    assert_eq!(
        app.servo_load_rows(),
        vec![
            ("X".to_string(), 12.0),
            ("S".to_string(), 38.0),
            ("A".to_string(), 4.0)
        ]
    );
    app.profile.config.ui.servo_load.clear();
    assert!(
        app.servo_load_rows().is_empty(),
        "an empty list hides the pane"
    );
}

#[test]
fn stepping_the_selection_clamps_at_both_ends() {
    let (_dir, mut app) = make_app(PanelMode::Auto);
    let text: String = (1..=40).map(|n| format!("L{n}\n")).collect();
    app.program.editor.replace_text(text);
    let count = app.program.editor.line_count();
    app.program.selected_line = 1;
    app.step_selected_line(-1);
    assert_eq!(app.program.selected_line, 1);
    app.step_selected_line(LINE_PAGE);
    assert_eq!(
        app.program.selected_line,
        (1 + LINE_PAGE as usize).min(count)
    );
    app.step_selected_line(10_000);
    assert_eq!(app.program.selected_line, count);
    assert_eq!(app.program.code_scroll_to_line, Some(count));
}

#[test]
fn goto_line_is_bound_to_ctrl_g() {
    let (_dir, app) = make_app(PanelMode::Edit);
    assert!(app.prefs.keymap.goto_line.modifiers.ctrl);
    assert_eq!(app.prefs.keymap.goto_line.key, egui::Key::G);
}

#[test]
fn dro_entry_sets_the_position_it_was_given() {
    let (_dir, mut app) = make_app(PanelMode::Jog);
    app.poll_backend();
    app.offsets.dro_edit = Some(crate::state::DroEdit {
        axis: "X".into(),
        buffer: "10+2.5".into(),
        focus: false,
    });
    assert!(crate::screens::offsets::commit_dro_edit(&mut app));
    app.poll_backend();
    let work = app
        .snapshot
        .positions
        .axes
        .get("X")
        .map(|position| position.work)
        .unwrap_or(f64::NAN);
    assert!(
        (work - 12.5).abs() < 1e-6,
        "DRO X shows {work}, expected 12.5"
    );
}

#[test]
fn the_fps_average_follows_the_frame_time() {
    assert!((fps_step(0.0, 1.0 / 60.0) - 60.0).abs() < 0.01);
    let mut fps = 60.0;
    for _ in 0..40 {
        fps = fps_step(fps, 1.0 / 30.0);
    }
    assert!((fps - 30.0).abs() < 0.5, "{fps}");
    // A wild frame time (a stall) cannot push the reading below zero or to
    // infinity; the clamp keeps it in a sane range.
    assert!(fps_step(60.0, 0.0) > 60.0);
    assert!(fps_step(60.0, 10.0) < 60.0);
}

#[test]
fn page_names_parse_the_way_the_diagnostic_tools_spell_them() {
    assert_eq!(page_from_name("MACHINE"), Some(Page::Machine));
    assert_eq!(page_from_name("program"), Some(Page::Program));
    assert_eq!(page_from_name("G-CODE HELP"), Some(Page::Help));
    assert_eq!(page_from_name("HELP"), Some(Page::Help));
    assert_eq!(page_from_name("Diagnostics"), Some(Page::Diagnostics));
    assert_eq!(page_from_name("NOPE"), None);
}

#[test]
fn a_stale_trail_is_masked_until_the_backend_clears_it() {
    let (_dir, app) = make_app(PanelMode::Jog);
    let mut snapshot = app.snapshot.clone();
    snapshot.tool_tip = Some([1.0, 2.0, 3.0]);
    snapshot.live_trail =
        std::sync::Arc::new(vec![linc_core::model::TrailPoint::new([0.0; 3], false); 5]);
    let tip = Some([1.0, 2.0, 3.0]);
    assert!(cleared_trail_pending(&snapshot, tip), "old points masked");

    // The backend applied the clear: one point left.
    snapshot.live_trail = std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(
        [1.0, 2.0, 3.0],
        false,
    )]);
    assert!(!cleared_trail_pending(&snapshot, tip), "clear accepted");

    // The tool moved: the points are new, not stale.
    snapshot.live_trail =
        std::sync::Arc::new(vec![linc_core::model::TrailPoint::new([0.0; 3], false); 5]);
    snapshot.tool_tip = Some([5.0, 2.0, 3.0]);
    assert!(!cleared_trail_pending(&snapshot, tip), "new motion kept");
}
#[test]
fn the_preview_uses_the_offset_the_program_selects() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    for row in app.snapshot.offsets.offsets.iter_mut() {
        if row.index == 1 {
            row.values.insert("X", -54.35);
            row.values.insert("Y", -49.3);
        }
        if row.index == 2 {
            row.values.insert("X", -100.0);
            row.values.insert("Y", -50.0);
        }
    }
    app.snapshot.offsets.active_index = Some(1);

    // The program selects G54: drawn at the G54 origin, not machine zero.
    app.program
        .editor
        .replace_text("G21 G90 G54\nG0 X0 Y0\n".into());
    app.refresh_preview();
    let end = app
        .program
        .preview
        .segments
        .last()
        .map(|segment| segment.end)
        .unwrap_or_default();
    assert!((end[0] + 54.35).abs() < 1e-6, "G54 X {}", end[0]);
    assert!((end[1] + 49.3).abs() < 1e-6, "G54 Y {}", end[1]);

    // A program that selects G55 is drawn at G55, though G54 is active.
    app.program
        .editor
        .replace_text("G21 G90 G55\nG0 X0 Y0\n".into());
    app.refresh_preview();
    let end = app
        .program
        .preview
        .segments
        .last()
        .map(|segment| segment.end)
        .unwrap_or_default();
    assert!((end[0] + 100.0).abs() < 1e-6, "G55 X {}", end[0]);

    // With no selection the active system is the fallback.
    app.program
        .editor
        .replace_text("G21 G90\nG0 X0 Y0\n".into());
    app.refresh_preview();
    let end = app
        .program
        .preview
        .segments
        .last()
        .map(|segment| segment.end)
        .unwrap_or_default();
    assert!((end[0] + 54.35).abs() < 1e-6, "active X {}", end[0]);
}
#[test]
fn the_view_cycle_key_steps_iso_x_y_z_and_refits() {
    let (_dir, mut app) = make_app(PanelMode::Jog);
    assert!(app.prefs.keymap.view_cycle.modifiers.ctrl);
    assert_eq!(app.prefs.keymap.view_cycle.key, egui::Key::F10);
    let order = [
        linc_view::ViewMode::SideYz,
        linc_view::ViewMode::SideXz,
        linc_view::ViewMode::TopXy,
        linc_view::ViewMode::Iso3d,
    ];
    for expected in order {
        app.program.fit_requested = false;
        app.manual.fit = false;
        app.cycle_toolpath_view();
        assert_eq!(app.program.graph_view, expected, "{expected:?}");
        assert_eq!(app.manual.view, expected, "{expected:?}");
        assert!(app.program.fit_requested, "each view refits: {expected:?}");
        assert!(app.manual.fit, "each view refits: {expected:?}");
    }
}
#[test]
fn run_from_line_uses_the_selected_line_not_the_controller_line() {
    let (_dir, mut app) = make_app(PanelMode::Edit);
    app.snapshot.program.content = Some(std::sync::Arc::new("G0 X0\nG1 X10\nG1 X20\n".to_string()));
    app.snapshot.program.line_count = 3;
    app.snapshot.program.current_line = 3;
    app.program.selected_line = 2;
    let (line, block) = app.run_from_line_choice();
    assert_eq!(line, 2);
    assert_eq!(block, "G1 X10");
}

use linc_core::probe::ProbeCategory;

/// The one wording both calibration routines share under PROTECT.
const CALIBRATION_PROTECT: &str = "PROTECT ON — calibration Run writes controller #3032 and the spindle reference parameters #3007/#3010";

fn probe_raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        )),
        events,
        ..Default::default()
    }
}

/// \`true\` when \`inner\` lies inside \`outer\` up to a rounding pixel.
fn inside(outer: egui::Rect, inner: egui::Rect) -> bool {
    inner.min.x >= outer.min.x - 0.5
        && inner.min.y >= outer.min.y - 0.5
        && inner.max.x <= outer.max.x + 0.5
        && inner.max.y <= outer.max.y + 0.5
}

/// One probe page host: a 1280x720 headless window, a simulator machine in
/// a known state, and a backend that records what the UI actually sent.
struct ProbeHarness {
    _dir: tempfile::TempDir,
    ctx: egui::Context,
    app: LincApp,
    sent: std::rc::Rc<std::cell::RefCell<Vec<MachineCommand>>>,
}

impl ProbeHarness {
    fn new(mode: PanelMode, protect: bool, connected: bool) -> Self {
        Self::ready(mode, protect, connected, MachinePower::On)
    }

    fn ready(mode: PanelMode, protect: bool, connected: bool, power: MachinePower) -> Self {
        let (dir, profile) = fixture();
        let ctx = egui::Context::default();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut sim = linc_core::sim::SimBackend::new(profile.clone(), mode);
        sim.set_power(power);
        sim.set_program_protect(protect, true);
        sim.set_panel_connected(connected);
        let backend = Box::new(RecordingBackend {
            inner: sim,
            sent: sent.clone(),
        });
        // Build the app inside a real 1280x720 pass: \`LincApp::new\` applies
        // the design zoom, and outside a pass egui reports no viewport.
        let mut app_slot = None;
        let mut profile_slot = Some(profile);
        let mut backend_slot = Some(backend);
        let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
            let profile = profile_slot.take().expect("one pass");
            let backend = backend_slot.take().expect("one pass");
            app_slot = Some(LincApp::new(profile, backend, ui.ctx()));
        });
        output.textures_delta.clear();
        let mut app = app_slot.unwrap();
        // The probe tool is in the spindle, so a blocked Run reports the
        // write policy rather than a tool mismatch.
        app.probe.params.probe_tool_number = app.snapshot.tool_change.current_tool;
        Self {
            _dir: dir,
            ctx,
            app,
            sent,
        }
    }

    /// Paint one frame at the 1280x720 baseline, with optional keys.
    fn frame(&mut self, keys: &[egui::Key]) -> Vec<egui::OutputCommand> {
        let keys: Vec<_> = keys
            .iter()
            .map(|key| (*key, egui::Modifiers::NONE))
            .collect();
        self.frame_with(&keys)
    }

    /// Type text at whatever holds the keyboard: what the panel's digits
    /// arrive as, in one event.
    fn type_text(&mut self, text: &str) {
        self.frame_events(vec![egui::Event::Text(text.to_string())]);
    }

    /// The tool-number dialog's text, while it is open.
    fn number_text(&self) -> Option<String> {
        self.app
            .tools
            .number
            .as_ref()
            .map(|entry| entry.text.clone())
    }

    /// Open the TOOLS page, then the tool-number dialog with its own chord.
    fn open_number_dialog(&mut self) {
        self.app.set_page(Page::Tools);
        self.frame(&[]);
        self.frame_with(&[(
            egui::Key::T,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        )]);
        assert!(
            self.app.tools.number.is_some(),
            "Ctrl+T opens the tool-number dialog"
        );
    }

    fn frame_with(&mut self, keys: &[(egui::Key, egui::Modifiers)]) -> Vec<egui::OutputCommand> {
        let events = keys
            .iter()
            .map(|(key, modifiers)| egui::Event::Key {
                key: *key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: *modifiers,
            })
            .collect();
        self.frame_events(events)
    }

    /// Paint one frame with raw events and return what egui asked the
    /// platform to do (clipboard writes prove a TextEdit consumed the cut).
    fn frame_events(&mut self, events: Vec<egui::Event>) -> Vec<egui::OutputCommand> {
        let mut output = self.frame_full(events);
        std::mem::take(&mut output.platform_output.commands)
    }

    /// Paint one frame and keep the whole output: a test that checks what
    /// reached the screen reads the shapes.
    fn frame_full(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.frame_input(probe_raw_input(events))
    }

    /// Paint one frame from a raw input, so a test can change the window
    /// size between frames the way a resize does.
    fn frame_input(&mut self, input: egui::RawInput) -> egui::FullOutput {
        let app = &mut self.app;
        let mut output = self.ctx.run_ui(input, |ui| {
            // The same prologue as \`eframe::App::ui\`, minus the frame.
            theme::apply_design_zoom(ui.ctx(), app.design_scale);
            app.handle_keyboard(ui);
            app.draw(ui);
        });
        // A headless frame has no painter to upload the textures to; the
        // deltas must be cleared, not dropped.
        output.textures_delta.clear();
        output
    }

    /// Draw the MDI pane alone into `slot`, and report the card frames and
    /// every text it painted. Calling the pane directly keeps the slot a
    /// known number instead of the page's computed column.
    fn draw_mdi_pane(&mut self, slot: egui::Rect) -> (Vec<egui::Rect>, Vec<(String, egui::Rect)>) {
        let app = &mut self.app;
        let mut output = self.ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
            theme::apply_design_zoom(ui.ctx(), app.design_scale);
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(slot)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            crate::screens::mdi::pane(app, &mut child, slot.height());
        });
        output.textures_delta.clear();
        (painted_cards(&output), painted_text(&output))
    }

    /// Open the page on \`category\` with a routine selected, bypassing the
    /// entry reset so a test can act on a selection.
    fn enter_probe(&mut self, category: ProbeCategory, operation: &str) {
        self.app.set_page(Page::Probe);
        self.app.probe.category = category;
        self.app.probe.operation = operation.to_string();
        self.app.probe.error = None;
    }

    /// Click at an absolute page position (press and release in one frame).
    fn click_at(&mut self, position: egui::Pos2) -> Vec<egui::OutputCommand> {
        self.frame_events(vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ])
    }

    /// Put one file in NC_FILES, show the Files page and select it.
    fn files_page_with_one_file(&mut self, name: &str) -> std::path::PathBuf {
        // Keep the removable-media poll out of the test; the NC pane is enough.
        self.app.files.usb_root = Some(self.app.files.nc_root.clone());
        let path = self.app.files.nc_root.join(name);
        std::fs::write(&path, "G0 X0\n").unwrap();
        self.app.set_page(Page::Files);
        self.app.files.nc_current = self.app.files.nc_root.clone();
        self.app.files.nc_entries = crate::state::read_dir_entries(&self.app.files.nc_root);
        self.app.files.nc_selected = self
            .app
            .files
            .nc_entries
            .iter()
            .position(|entry| entry.path == path)
            .expect("test file is listed");
        self.app.files.nc_parent_selected = false;
        path
    }

    fn mdi_sent(&self) -> Vec<String> {
        self.sent
            .borrow()
            .iter()
            .filter_map(|command| match command {
                MachineCommand::IssueMdi { block, .. } => Some(block.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every command that writes probe or controller state.
    fn probe_writes(&self) -> Vec<String> {
        self.sent
            .borrow()
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    MachineCommand::IssueMdi { .. }
                        | MachineCommand::ResetProbeCalibration
                        | MachineCommand::ApplyProbeResult { .. }
                )
            })
            .map(|command| format!("{command:?}"))
            .collect()
    }

    fn sent_debug(&self) -> Vec<String> {
        self.sent
            .borrow()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect()
    }
}

// ---- TOOLS page: the typed tool number (Ctrl+T) -------------------

/// Ctrl+T opens a dialog prefilled with the tool in the spindle, and
/// Enter issues exactly what bare `T` issues for a table row.
#[test]
fn ctrl_t_opens_the_tool_number_dialog_and_enter_loads_it() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    assert!(harness.app.tools.number.is_none());
    assert!(!harness.app.overlay_open());

    // egui-winit sets `command` alongside `ctrl` on a real keyboard; a
    // synthetic event has to carry both or the parsed binding misses.
    harness.frame_with(&[(
        egui::Key::T,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    let current = harness.app.snapshot.tool_change.current_tool;
    assert_eq!(
        harness.number_text(),
        Some(current.to_string()),
        "prefilled with the tool in the spindle"
    );
    assert!(
        harness.app.overlay_open(),
        "the dialog makes the table, the cell editor and the shell stand down"
    );

    // The field holds the keyboard from the frame it opens, with the
    // whole value selected: a digit replaces it. No click-to-focus and
    // no Tab on this panel.
    harness.frame(&[]);
    assert!(
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-tool-number"))),
        "the field takes focus on the frame it opens"
    );
    harness.type_text("1");
    assert_eq!(
        harness.number_text(),
        Some("1".to_string()),
        "the first digit replaces the selected value"
    );
    harness.type_text("2");
    assert_eq!(
        harness.number_text(),
        Some("12".to_string()),
        "digits append"
    );

    harness.sent.borrow_mut().clear();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        harness.sent.borrow().as_slice(),
        [MachineCommand::LoadTool { tool: 12 }],
        "Enter issues the same LoadTool as bare T"
    );
    assert!(
        harness.app.tools.number.is_none(),
        "Enter closes the dialog"
    );
}

#[test]
fn ctrl_n_creates_a_new_tool_table_row() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    let before = harness.app.snapshot.tool_table.rows.clone();
    let probe_tool = harness.app.profile.config.probe.tool_number;
    let expected = (1..=u16::MAX)
        .find(|tool| *tool != probe_tool && !before.iter().any(|row| row.tool == *tool))
        .expect("a free tool number");

    harness.sent.borrow_mut().clear();
    harness.frame_with(&[(
        egui::Key::N,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    let sent = harness.sent.borrow();
    assert!(
        sent.iter().any(|command| matches!(
            command,
            MachineCommand::SetToolTableRow { row, .. } if row.tool == expected
        )),
        "{:?}",
        harness.sent_debug()
    );
    drop(sent);

    harness.frame(&[]);
    assert!(
        harness
            .app
            .snapshot
            .tool_table
            .rows
            .iter()
            .any(|row| row.tool == expected),
        "the new row is in the table"
    );
    assert_ne!(
        expected, probe_tool,
        "the probe tool number is never reused"
    );
    let selected = harness
        .app
        .snapshot
        .tool_table
        .rows
        .get(harness.app.tools.selected_row)
        .map(|row| row.tool);
    assert_eq!(selected, Some(expected), "the new row is selected");
    let new_row = harness
        .app
        .snapshot
        .tool_table
        .rows
        .iter()
        .find(|row| row.tool == expected)
        .expect("the new row");
    assert_eq!(new_row.geometry.length, Some(0.0));
    assert_eq!(new_row.geometry.diameter, Some(0.0));
}

#[test]
fn delete_tool_command_removes_the_row() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    let probe_tool = harness.app.profile.config.probe.tool_number;
    let active_tool = harness.app.snapshot.tool_change.current_tool;
    let row = harness
        .app
        .snapshot
        .tool_table
        .rows
        .iter()
        .find(|row| row.tool != probe_tool && row.tool != active_tool)
        .cloned()
        .expect("a non-probe, non-active tool");
    let revision = harness.app.snapshot.tool_table.revision;
    harness.app.send(MachineCommand::DeleteToolTableRow {
        tool: row.tool,
        expected_revision: revision,
    });
    harness.frame(&[]);
    assert!(
        !harness
            .app
            .snapshot
            .tool_table
            .rows
            .iter()
            .any(|existing| existing.tool == row.tool),
        "the row is gone"
    );
}

#[test]
fn renumbering_a_tool_removes_the_old_row() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    let rows = harness.app.snapshot.tool_table.rows.clone();
    let probe_tool = harness.app.profile.config.probe.tool_number;
    let active_tool = harness.app.snapshot.tool_change.current_tool;
    let old = rows
        .iter()
        .find(|row| row.tool != probe_tool && row.tool != active_tool)
        .cloned()
        .expect("a non-probe, non-active tool");
    let new_tool = (1..=u16::MAX)
        .find(|tool| {
            *tool != probe_tool && *tool != active_tool && !rows.iter().any(|row| row.tool == *tool)
        })
        .expect("a free tool number");
    let mut renumbered = old.clone();
    renumbered.tool = new_tool;
    let revision = harness.app.snapshot.tool_table.revision;
    harness.app.send(MachineCommand::SetToolTableRow {
        row: renumbered,
        old_tool: Some(old.tool),
        expected_revision: revision,
    });
    harness.frame(&[]);
    let rows = &harness.app.snapshot.tool_table.rows;
    assert!(
        !rows.iter().any(|row| row.tool == old.tool),
        "the original row is gone"
    );
    assert!(
        rows.iter().any(|row| row.tool == new_tool),
        "the renumbered row is present"
    );
}

#[test]
fn backspace_confirms_tool_delete_and_refuses_the_probe_tool() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    let probe_tool = harness.app.profile.config.probe.tool_number;
    let rows = harness.app.snapshot.tool_table.rows.clone();
    let active_tool = harness.app.snapshot.tool_change.current_tool;
    let target = rows
        .iter()
        .position(|row| row.tool != probe_tool && row.tool != active_tool)
        .expect("a non-probe, non-active tool");
    let target_tool = rows[target].tool;

    harness.app.tools.selected_row = target;
    harness.sent.borrow_mut().clear();
    harness.frame(&[egui::Key::Backspace]);
    assert!(
        matches!(
            harness.app.overlay.modal,
            Some(crate::state::Modal::ConfirmToolDelete { tool, .. })
                if tool == target_tool
        ),
        "Backspace opens a delete confirmation"
    );
    assert!(
        harness.sent.borrow().is_empty(),
        "nothing is deleted before confirmation"
    );

    harness.app.overlay.modal = None;
    // Treat the selected row as the configured probe tool: the fixture
    // table need not carry a T99 row for this refusal.
    harness.app.profile.config.probe.tool_number = target_tool;
    harness.app.tools.selected_row = target;
    harness.frame(&[egui::Key::Backspace]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "the probe tool is never offered for deletion"
    );
    assert!(
        harness
            .app
            .tools
            .error
            .as_deref()
            .is_some_and(|error| error.contains("probe tool")),
        "{:?}",
        harness.app.tools.error
    );

    harness.app.profile.config.probe.tool_number = probe_tool;
    let active = rows
        .iter()
        .position(|row| row.tool == active_tool)
        .expect("the active tool row");
    harness.app.tools.selected_row = active;
    harness.app.tools.error = None;
    harness.frame(&[egui::Key::Backspace]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "the active tool is not deleted"
    );
    assert!(
        harness
            .app
            .tools
            .error
            .as_deref()
            .is_some_and(|error| error.contains("active tool")),
        "{:?}",
        harness.app.tools.error
    );
}

/// A refused number keeps the dialog open, names the problem and sends
/// nothing; 0 is accepted and empties the spindle.
#[test]
fn a_refused_tool_number_stays_in_the_dialog_and_sends_nothing() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.open_number_dialog();
    harness.sent.borrow_mut().clear();

    // The fixture's table is T1 and T12: 2 is a row index, not a tool.
    harness.type_text("2");
    harness.frame(&[egui::Key::Enter]);
    let entry = harness
        .app
        .tools
        .number
        .as_ref()
        .expect("a refused entry keeps the dialog open");
    assert_eq!(entry.text, "2", "the refused text is kept");
    assert_eq!(entry.error.as_deref(), Some("T2 is not in the tool table"));
    assert!(
        harness.sent.borrow().is_empty(),
        "a refused number sends nothing"
    );
    assert!(harness.app.overlay_open());

    // An empty entry is refused the same way, with its own reason.
    harness.app.tools.number.as_mut().unwrap().text.clear();
    harness.frame(&[egui::Key::Enter]);
    let entry = harness.app.tools.number.as_ref().expect("still open");
    assert_eq!(entry.error.as_deref(), Some("type a tool number"));
    assert!(harness.sent.borrow().is_empty());

    // A letter is not a number, and it does not read as 0 either.
    harness.type_text("x");
    harness.frame(&[egui::Key::Enter]);
    let entry = harness.app.tools.number.as_ref().expect("still open");
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("is not a tool number")),
        "a non-numeric entry is refused: {:?}",
        entry.error
    );
    assert!(harness.sent.borrow().is_empty());

    // 0 is accepted: the same command, for an empty spindle.
    harness.app.tools.number.as_mut().unwrap().text = "0".into();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        harness.sent.borrow().as_slice(),
        [MachineCommand::LoadTool { tool: 0 }],
        "0 empties the spindle through the same LoadTool"
    );
    assert!(harness.app.tools.number.is_none());
}

/// Escape closes it and issues nothing; `issue_allowed` is reported
/// inside the dialog rather than silently doing nothing.
#[test]
fn the_tool_number_dialog_reports_the_issue_gate_and_escapes_clean() {
    // Power off: `issue_allowed` refuses every machine command.
    let mut harness = ProbeHarness::ready(PanelMode::Auto, false, true, MachinePower::Off);
    harness.open_number_dialog();
    harness.sent.borrow_mut().clear();
    harness.type_text("1");
    harness.frame(&[egui::Key::Enter]);
    let entry = harness.app.tools.number.as_ref().expect("still open");
    assert_eq!(
        entry.error.as_deref(),
        Some("machine is not ON"),
        "the refusal is the one issue_allowed gives"
    );
    assert!(harness.sent.borrow().is_empty());

    // Escape closes it, sends nothing and gives the page its keys back.
    harness.frame(&[egui::Key::Escape]);
    assert!(
        harness.app.tools.number.is_none(),
        "Escape closes the dialog"
    );
    assert!(harness.sent.borrow().is_empty(), "Escape issues nothing");
    assert!(!harness.app.overlay_open(), "nothing is left open");
    assert!(
        !harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-tool-number"))),
        "the field gives the keyboard back"
    );

    // The dialog belongs to the page: leaving it closes the dialog rather
    // than keeping every key for a window that is not drawn.
    harness.frame_with(&[(
        egui::Key::T,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    assert!(harness.app.tools.number.is_some());
    harness.app.set_page(Page::Machine);
    assert!(
        harness.app.tools.number.is_none(),
        "leaving the TOOLS page closes the dialog"
    );
    assert!(!harness.app.overlay_open());
}

/// The row follows the tool in the spindle: after a change, the operator is on
/// the row they are about to touch off. Browsing by hand is not yanked back
/// while that tool stays the same.
#[test]
fn a_tool_change_moves_the_table_selection_to_the_active_row() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    harness.app.tools.selected_row = 0;

    // The fixture's table is T1, T12: the second row is T12.
    harness
        .app
        .send(MachineCommand::SetCurrentTool { tool: 12 });
    harness.frame(&[]);
    assert_eq!(harness.app.snapshot.tool_change.current_tool, 12);
    assert_eq!(
        harness.app.tools.selected_row, 1,
        "the selection follows the tool in the spindle"
    );

    // A hand-moved cursor survives frames while the tool is unchanged.
    harness.app.tools.selected_row = 0;
    harness.frame(&[]);
    assert_eq!(harness.app.tools.selected_row, 0);

    // The next change moves it again, to the other row.
    harness.app.send(MachineCommand::SetCurrentTool { tool: 1 });
    harness.frame(&[]);
    assert_eq!(harness.app.tools.selected_row, 0);
}

/// Editing the mill's OFFSET cell must write the Z the table loader reads back.
/// The row carries the length in two places — `length` and `values["Z"]` — and
/// the file is written from the values map, so a cell that updated only `length`
/// saved the old Z and the row reverted on the next reload.
#[test]
fn editing_the_offset_cell_writes_the_z_the_table_reads() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.frame(&[]);
    harness.sent.borrow_mut().clear();

    // The fixture's T1 is Z-56: OFFSET is column 2 on a mill (T, TYPE, OFFSET, Ø,
    // DESCRIPTION).
    harness.app.tools.selected_row = 0;
    harness.app.tools.selected_column = 2;
    harness.frame(&[egui::Key::Enter]);
    assert!(harness.app.tools.editing, "Enter opens the cell editor");
    harness.app.tools.edit_buffer = "-50".into();
    harness.frame(&[egui::Key::Enter]);

    let sent = harness.sent.borrow();
    let row = match sent.as_slice() {
        [MachineCommand::SetToolTableRow { row, .. }] => row.clone(),
        other => panic!("expected one tool-table write: {other:?}"),
    };
    assert_eq!(row.geometry.length, Some(-50.0), "the length field");
    assert_eq!(
        row.geometry.values.get("Z").copied(),
        Some(-50.0),
        "the Z the tool table file and G43 read"
    );
}

/// `Z` asks where the tool is and writes the in-spindle tool's offset so the
/// work coordinate reads it: the fixture's T1 carries Z-56 and the work Z reads
/// 0, so saying the tool is at -1.5 moves the offset to -54.5.
#[test]
fn the_tool_position_dialog_writes_the_offset_of_the_tool_in_the_spindle() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Tools);
    harness.app.send(MachineCommand::SetCurrentTool { tool: 1 });
    harness.frame(&[]);
    harness.sent.borrow_mut().clear();

    harness.frame(&[egui::Key::Z]);
    let entry = harness
        .app
        .tools
        .position
        .as_ref()
        .expect("Z opens the tool-position dialog");
    assert_eq!(entry.axis, "Z", "the shortcut opens on the Z axis");
    assert_eq!(entry.buffer, "0.000", "seeded with the work reading");
    assert!(harness.app.overlay_open(), "the dialog owns the keyboard");

    harness.app.tools.position.as_mut().unwrap().buffer = "-1.5".into();
    harness.frame(&[egui::Key::Enter]);
    assert!(
        harness.app.tools.position.is_none(),
        "a confirmed entry closes the dialog"
    );
    let sent = harness.sent.borrow();
    let row = match sent.as_slice() {
        [MachineCommand::SetToolTableRow { row, old_tool, .. }] => {
            assert_eq!(*old_tool, None, "the tool number does not change");
            row.clone()
        }
        other => panic!("expected one tool-table write: {other:?}"),
    };
    assert_eq!(row.tool, 1);
    let z = row.geometry.values.get("Z").copied().unwrap_or_default();
    assert!((z + 54.5).abs() < 1e-9, "Z offset {z} is -56 + 1.5");
    assert_eq!(
        row.geometry.length,
        Some(z),
        "length and the Z column stay the same value"
    );
}

/// An `X`/`Y`/`Z` key picks the axis being entered. Escape cancels, and a
/// blocked write keeps the dialog open with the reason instead of closing on a
/// silent no-op.
#[test]
fn the_tool_position_dialog_switches_axis_and_reports_a_blocked_write() {
    // Power off: the tool table is not writable.
    let mut harness = ProbeHarness::ready(PanelMode::Auto, false, true, MachinePower::Off);
    harness.app.set_page(Page::Tools);
    harness.app.send(MachineCommand::SetCurrentTool { tool: 1 });
    harness.frame(&[]);
    harness.sent.borrow_mut().clear();

    harness.frame(&[egui::Key::Z]);
    assert!(harness.app.tools.position.is_some());
    harness.frame(&[egui::Key::X]);
    let entry = harness.app.tools.position.as_ref().expect("still open");
    assert_eq!(entry.axis, "X", "X picks the axis under the cursor");

    harness.frame(&[egui::Key::Enter]);
    let entry = harness
        .app
        .tools
        .position
        .as_ref()
        .expect("a blocked write keeps the dialog open");
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("must be ON")),
        "the refusal is shown: {:?}",
        entry.error
    );
    assert!(harness.sent.borrow().is_empty(), "nothing was written");

    // Escape closes it and gives the keyboard back.
    harness.frame(&[egui::Key::Escape]);
    assert!(harness.app.tools.position.is_none());
    assert!(!harness.app.overlay_open());
    assert!(
        !harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-tool-position"))),
        "the field gives the keyboard back"
    );
}

struct RecordingBackend {
    inner: linc_core::sim::SimBackend,
    sent: std::rc::Rc<std::cell::RefCell<Vec<MachineCommand>>>,
}

impl UiBackend for RecordingBackend {
    fn snapshot(&mut self) -> MachineSnapshot {
        linc_core::backend::MachineBackend::snapshot(&mut self.inner)
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), linc_core::command::MachineError> {
        self.sent.borrow_mut().push(command.clone());
        linc_core::backend::MachineBackend::send(&mut self.inner, command)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

// PR-03: PROTECT gates what an action writes, not which mode it runs in.

#[test]
fn protect_refuses_a_calibration_run_before_issuing_anything() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    harness.frame(&[]);
    // The refusal is painted before Enter: a calibration Run rewrites #3032.
    let reason = harness
        .app
        .probe
        .layout
        .run_reason_text
        .clone()
        .unwrap_or_default();
    assert_eq!(reason, format!("RUN UNAVAILABLE: {CALIBRATION_PROTECT}"));

    screens::probe::run(&mut harness.app);
    assert_eq!(
        harness.app.probe.error.as_deref(),
        Some(CALIBRATION_PROTECT)
    );
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
    assert!(harness.app.probe.last_run_operation.is_none());

    // With PROTECT off the same Run issues exactly one block.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    harness.frame(&[]);
    screens::probe::run(&mut harness.app);
    assert_eq!(harness.app.probe.error, None);
    let sent = harness.mdi_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("o<probe_cal_round_boss> call"),
        "{}",
        sent[0]
    );
}

#[test]
fn protection_covers_the_spindle_nose_calibration_routine() {
    // spindle_nose is category SurfaceZ but the catalogue marks it
    // calibration: it writes #3010 and #3007, so PROTECT must treat it as a
    // calibration Run even in POSITION mode.
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "spindle_nose");
    harness.app.probe.params.probe_mode = linc_core::probe::ProbeMode::MeasureOnly;
    harness.frame(&[]);
    let reason = harness
        .app
        .probe
        .layout
        .run_reason_text
        .clone()
        .unwrap_or_default();
    assert_eq!(reason, format!("RUN UNAVAILABLE: {CALIBRATION_PROTECT}"));

    screens::probe::run(&mut harness.app);
    assert_eq!(
        harness.app.probe.error.as_deref(),
        Some(CALIBRATION_PROTECT)
    );
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
    assert!(harness.app.probe.last_run_operation.is_none());

    // PROTECT off: the same selection runs, still as a POSITION run.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "spindle_nose");
    harness.frame(&[]);
    screens::probe::run(&mut harness.app);
    assert_eq!(harness.app.probe.error, None);
    let sent = harness.mdi_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("o<probe_spindle_nose> call"),
        "{}",
        sent[0]
    );
}

#[test]
fn an_unclassifiable_routine_block_is_refused_even_with_protect_off() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    // A routine call with no typed parameter block: the mode it inherits
    // from the interpreter cannot be trusted, so it fails closed rather than
    // passing as a measurement.
    crate::screens::probe_view::issue_probe_mdi(
        &mut harness.app,
        "o<probe_z_minus_wco> call".into(),
        "test",
    );
    let error = harness.app.probe.error.clone().unwrap_or_default();
    assert!(error.contains("cannot be classified"), "{error}");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn protect_refuses_the_parameter_update_and_every_data_reset() {
    for (key, modifiers, label) in [
        (egui::Key::U, egui::Modifiers::NONE, "U UPDATE PARAMS"),
        (egui::Key::R, egui::Modifiers::CTRL, "Ctrl+R RESET ALL"),
        (egui::Key::X, egui::Modifiers::CTRL, "Ctrl+X RESET X"),
        (egui::Key::Y, egui::Modifiers::CTRL, "Ctrl+Y RESET Y"),
    ] {
        let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame_with(&[(key, modifiers)]);
        let error = harness.app.probe.error.clone().unwrap_or_default();
        assert!(error.contains("PROTECT ON"), "{label}: {error}");
        assert!(
            harness.probe_writes().is_empty(),
            "{label} issued {:?}",
            harness.sent_debug()
        );

        // PROTECT off: the same key issues one MDI block and no more.
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
        harness.frame_with(&[(key, modifiers)]);
        assert_eq!(harness.app.probe.error, None, "{label}");
        assert_eq!(harness.mdi_sent().len(), 1, "{label}");
    }
}

#[test]
fn the_cut_shortcut_reaches_the_probe_x_data_reset() {
    // `egui-winit` never delivers Ctrl+X as a key event: the page receives
    // `Event::Cut`. This is the event the keyboard X reset really sees.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    harness.frame_events(vec![egui::Event::Cut]);
    let sent = harness.mdi_sent();
    assert_eq!(sent.len(), 1, "{:?}", harness.sent_debug());
    assert!(sent[0].contains("o<x_data_reset> call"), "{}", sent[0]);
    assert_eq!(harness.app.probe.error, None);

    // The same shortcut under PROTECT still takes the write policy.
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    harness.frame_events(vec![egui::Event::Cut]);
    let error = harness.app.probe.error.clone().unwrap_or_default();
    assert!(error.contains("PROTECT ON"), "{error}");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn the_cut_hand_off_is_scoped_to_the_probe_page() {
    // No page other than Probe may receive a synthetic cut chord: the
    // clipboard event belongs to text editing everywhere else.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let mut observed = Vec::new();
    for page in [Page::Machine, Page::Files, Page::Offsets, Page::Probe] {
        harness.app.set_page(page);
        let ctx = harness.ctx.clone();
        let app = &mut harness.app;
        let mut output = ctx.run_ui(probe_raw_input(vec![egui::Event::Cut]), |ui| {
            app.hand_cut_to_page(ui);
            let cut = ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Cut))
            });
            let key = ui.input(|input| {
                input.events.iter().any(|event| {
                    matches!(event, egui::Event::Key { key: egui::Key::X, modifiers, .. } if modifiers.ctrl)
                })
            });
            observed.push((page, cut, key));
        });
        output.textures_delta.clear();
    }
    assert_eq!(
        observed,
        vec![
            (Page::Machine, true, false),
            (Page::Files, true, false),
            (Page::Offsets, true, false),
            (Page::Probe, false, true),
        ]
    );
}

#[test]
fn the_cut_shortcut_never_resets_x_while_the_parameter_editor_is_open() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    // Ctrl+P selects the first parameter, Enter opens the inline editor.
    harness.frame_with(&[(egui::Key::P, egui::Modifiers::CTRL)]);
    harness.frame(&[egui::Key::Enter]);
    // One more frame: the editor takes focus and selects its whole value.
    harness.frame(&[]);
    assert!(harness.app.probe.param_editing, "the editor is open");
    let buffer = harness.app.probe.param_buffer.clone();
    assert!(!buffer.is_empty(), "the editor holds the value");

    let commands = harness.frame_events(vec![egui::Event::Cut]);

    assert!(
        harness.probe_writes().is_empty(),
        "Ctrl+X must not reset X data while the editor is open: {:?}",
        harness.sent_debug()
    );
    assert!(harness.app.probe.param_editing, "the editor stays open");
    assert_eq!(harness.app.probe.error, None);
    // The editor kept the chord: it cut its text to the clipboard.
    assert!(
        commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if *text == buffer)
        ),
        "the editor must still cut its text, got {commands:?}"
    );
    assert!(
        harness.app.probe.param_buffer.is_empty(),
        "the cut emptied the editor"
    );
}

#[test]
fn the_cut_shortcut_still_cuts_text_in_the_gcode_editor() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\n".into());
    harness.app.set_page(Page::Program);
    harness.frame(&[]);

    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    let mut state = egui::text_edit::TextEditState::load(&harness.ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(2),
        )));
    state.store(&harness.ctx, id);

    let commands = harness.frame_events(vec![egui::Event::Cut]);

    assert!(
        commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == "G0")),
        "the editor must cut the selection, got {commands:?}"
    );
    assert_eq!(harness.app.program.editor.text(), " X1\nG1 X2\n");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn a_hovered_toolpath_takes_the_pan_and_zoom_chords() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness.app.set_page(Page::Machine);
    // The widget records hover at the end of a frame; the shell reads it at
    // the top of the next one, which is the frame under test here.
    harness.ctx.data_mut(|data| {
        data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
    });
    // Let the first frame auto-fit, so the pan under test is the key's own.
    harness.frame(&[]);
    let before = harness.app.program.graph_pan;
    harness.ctx.data_mut(|data| {
        data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
    });
    harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
    assert!(
        (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
        "Ctrl+Left pans the view left by one step, {:?} -> {:?}",
        before,
        harness.app.program.graph_pan
    );
    assert_eq!(
        harness.app.nav.focus_region,
        FocusRegion::Page,
        "the hovered view keeps Ctrl+Left from moving the focus ring"
    );
    assert!(
        !harness.app.program.graph_auto_fit,
        "a moved view stops fitting"
    );

    let zoom_before = harness.app.program.graph_zoom;
    let pan_before = harness.app.program.graph_pan;
    harness.ctx.data_mut(|data| {
        data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), true);
    });
    harness.frame_with(&[(egui::Key::Plus, ctrl())]);
    assert!(
        (harness.app.program.graph_zoom - zoom_before * VIEW_ZOOM_STEP).abs() < 1e-6,
        "Ctrl+Plus zooms in one step, got {}",
        harness.app.program.graph_zoom
    );
    assert!(
        (harness.app.program.graph_pan - pan_before * VIEW_ZOOM_STEP).length() < 1e-3,
        "the pan carries the zoom so the card centre stays put, {:?} -> {:?}",
        pan_before,
        harness.app.program.graph_pan
    );
}

/// The CODE layout's card is a thumbnail, not a different view: it used to
/// force TOP, so an oversized editor took the operator's perspective away
/// and nothing but the view cycle brought it back.
#[test]
fn the_code_thumbnail_draws_the_operator_view_and_keeps_its_angles() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X0 Y0 Z0\nG1 X20 Y10 Z-40\n".into());
    harness.app.refresh_preview();
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Code;
    harness.app.program.graph_view = linc_view::ViewMode::Iso3d;
    harness.app.program.graph_yaw = 0.7;
    harness.app.program.graph_pitch = 0.42;
    harness.app.program.fit_requested = true;
    harness.frame(&[]);
    let iso_zoom = harness.app.program.graph_zoom;
    assert!(
        (harness.app.program.graph_yaw - 0.7).abs() < 1e-6
            && (harness.app.program.graph_pitch - 0.42).abs() < 1e-6,
        "drawing the thumbnail kept the operator's angles, got {} / {}",
        harness.app.program.graph_yaw,
        harness.app.program.graph_pitch
    );
    assert_eq!(harness.app.program.graph_view, linc_view::ViewMode::Iso3d);

    // The view the card is given is the view it fits for: the same part
    // fitted in TOP takes a different zoom, because the Z extent has no
    // screen length there.
    harness.app.program.graph_view = linc_view::ViewMode::TopXy;
    harness.app.program.fit_requested = true;
    harness.frame(&[]);
    let top_zoom = harness.app.program.graph_zoom;
    assert!(
        (iso_zoom - top_zoom).abs() > 1e-3,
        "the card fits for the view it is given, ISO {iso_zoom} vs TOP {top_zoom}"
    );
}

/// A card whose rect changes re-fits: the fit belongs to the rect it was
/// computed for.
#[test]
fn the_code_thumbnail_refits_when_the_card_changes_size() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X0 Y0 Z0\nG1 X20 Y10 Z-40\n".into());
    harness.app.refresh_preview();
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Code;
    harness.app.program.graph_view = linc_view::ViewMode::Iso3d;
    harness.frame(&[]);
    // The operator's own zoom: nothing will re-frame it until the card
    // changes size.
    harness.app.program.graph_auto_fit = false;
    harness.app.program.graph_zoom = 6.0;
    harness.frame(&[]);
    assert!(
        (harness.app.program.graph_zoom - 6.0).abs() < 1e-6,
        "an untouched card keeps the operator's zoom"
    );

    let mut input = probe_raw_input(Vec::new());
    input.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(1024.0, 640.0),
    ));
    harness.frame_input(input);
    assert!(
        (harness.app.program.graph_zoom - 6.0).abs() > 0.1,
        "a resized card re-fits its contents, got {}",
        harness.app.program.graph_zoom
    );
}

/// The readout is one line: the axis name at the coordinate's own size, the
/// coordinate beside it, the dimmed machine value at the right end. The name
/// used to sit on a row of its own above the number.
#[test]
fn the_dro_puts_the_axis_name_on_the_coordinate_line() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let texts = painted_text(&output);
    let same_line = |from: egui::Rect, prefix: &str, right_of: f32| {
        texts
            .iter()
            .filter(|(text, rect)| {
                text.starts_with(prefix)
                    && rect.left() >= right_of
                    && (rect.center().y - from.center().y).abs() < 2.0
            })
            .map(|(_, rect)| *rect)
            .min_by(|a, b| a.left().partial_cmp(&b.left()).unwrap())
    };
    for axis in ["X", "Y", "Z"] {
        // The axis names are painted in more than one place on MAIN; the
        // readout line is the one with the coordinate beside it.
        let (label, value) = texts
            .iter()
            .filter(|(text, _)| text == axis)
            .find_map(|(_, rect)| same_line(*rect, "+", rect.right()).map(|value| (*rect, value)))
            .unwrap_or_else(|| panic!("{axis} paints its coordinate beside its name"));
        assert!(
            (label.height() - value.height()).abs() < 0.6,
            "{axis}: the name is set at the coordinate's size, {} vs {}",
            label.height(),
            value.height()
        );
        assert!(
            value.left() >= label.right() - 0.5,
            "{axis}: the name and the coordinate do not overlap"
        );
        let machine = same_line(label, "M ", value.right())
            .unwrap_or_else(|| panic!("{axis} keeps its machine coordinate"));
        assert!(
            machine.left() >= value.right() - 0.5,
            "{axis}: the machine coordinate clears the value, {} vs {}",
            machine.left(),
            value.right()
        );
    }
}

/// A long coordinate on a narrow card drops the machine value whole rather
/// than letting the two touch; the name and the value always have room.
#[test]
fn a_long_coordinate_drops_the_machine_value_instead_of_overlapping() {
    let ctx = egui::Context::default();
    let mut layouts = None;
    let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
        layouts = Some((
            crate::screens::common::dro_line_layout(ui, 400.0, "X", -1234.567, -12.5, false, false),
            crate::screens::common::dro_line_layout(ui, 180.0, "X", -1234.567, -12.5, true, true),
        ));
    });
    output.textures_delta.clear();
    let (wide, narrow) = layouts.expect("one pass");
    assert!(
        wide.machine.is_some(),
        "a wide card keeps the machine coordinate"
    );
    assert!(
        wide.value_x > theme::DRO_INSET,
        "the value starts after the name"
    );
    assert!(
        narrow.machine.is_none(),
        "a narrow card drops the machine coordinate, got {:?}",
        narrow.machine
    );
    assert_eq!(wide.value, "-1234.567");
    assert_eq!(narrow.value, "-1234.567");
    assert_eq!(
        narrow.label, "X DIA",
        "the diameter mark keeps its full label"
    );
    assert_eq!(wide.font_size, theme::font_size(theme::DRO_FONT));
    assert_eq!(narrow.font_size, theme::font_size(theme::DRO_FONT_COMPACT));
}

/// The readout is a third shorter, and the space goes to the toolpath: the
/// graph card starts where the DRO row ends, so the page does not keep an
/// empty gap.
#[test]
fn the_dro_cards_lost_the_second_row_and_the_toolpath_grew() {
    let mut harness = ProbeHarness::new(PanelMode::Jog, false, true);
    harness.app.set_page(Page::Machine);
    harness.frame(&[]);
    let output = harness.frame_full(Vec::new());
    let rects = painted_rects(&output);
    // Each card paints its fill and its border as two rects at one place.
    let mut dro: Vec<egui::Rect> = rects
        .iter()
        .copied()
        .filter(|rect| (rect.height() - theme::DRO_CARD_HEIGHT).abs() < 0.5 && rect.width() > 150.0)
        .collect();
    dro.dedup();
    assert_eq!(dro.len(), 3, "one card per mill axis, got {dro:?}");
    let dro_bottom = dro
        .iter()
        .fold(f32::MIN, |low, rect| low.max(rect.bottom()));
    // The toolpath card is the wide frame under the readout row.
    let graph = rects
        .iter()
        .copied()
        .filter(|rect| rect.width() > 400.0 && rect.top() >= dro_bottom)
        .min_by(|a, b| a.top().partial_cmp(&b.top()).unwrap())
        .expect("the toolpath card is under the readout");
    assert!(
        graph.top() > dro_bottom && graph.top() < dro_bottom + 16.0,
        "the toolpath starts right under the readout row, not below a gap: {} vs {}",
        graph.top(),
        dro_bottom
    );
    assert!(
        graph.height() > 200.0,
        "the toolpath took the space the readout gave up, got {}",
        graph.height()
    );
}

/// The centre of a nav row, from the label the sidebar paints for it.
fn nav_row_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    painted_text(output)
        .into_iter()
        .find(|(text, rect)| text == label && rect.left() < theme::NAV_WIDTH)
        .map(|(_, rect)| rect.center())
        .unwrap_or_else(|| panic!("{label} is painted in the navigation"))
}

/// A point inside the toolpath card, below the line that titles it.
fn toolpath_card_point(output: &egui::FullOutput) -> egui::Pos2 {
    let (_, rect) = painted_text(output)
        .into_iter()
        .find(|(text, _)| text == "TOOLPATH" || text == "PATH")
        .expect("the toolpath card paints its title");
    rect.center() + egui::vec2(0.0, 40.0)
}

fn press_at(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The card's status readout, as painted in one frame.
fn zoom_readout(output: &egui::FullOutput) -> Option<String> {
    painted_text(output)
        .into_iter()
        .map(|(text, _)| text)
        .find(|text| text.contains(" mm"))
}

/// Parse the millimetres out of the card's status readout.
fn readout_mm(readout: &str) -> f32 {
    readout
        .split_whitespace()
        .find_map(|word| word.parse::<f32>().ok())
        .expect("the status line carries a millimetre figure")
}

/// Ctrl+Plus and Ctrl+Minus are the view's even while the G-code editor
/// holds the keyboard: the editor binds neither chord, and that is where the
/// operator is when they want a closer look at the path. Before this, the
/// whole pan/zoom block sat behind "no text field has focus", and the
/// editor takes focus as soon as the Program page opens, so both chords did
/// nothing at all.
#[test]
fn the_zoom_chords_work_while_the_gcode_editor_holds_the_keyboard() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Split;
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.frame_events(Vec::new());
    assert!(
        harness.ctx.memory(|memory| memory.has_focus(id)),
        "the G-code editor holds the keyboard"
    );
    assert!(
        linc_view::toolpath_hovered(&harness.ctx),
        "the pointer is over the toolpath card"
    );

    let before = harness.app.program.graph_zoom;
    harness.frame_with(&[(egui::Key::Plus, ctrl())]);
    assert!(
        (harness.app.program.graph_zoom - before * VIEW_ZOOM_STEP).abs() < 1e-4,
        "Ctrl+Plus zooms one step while the editor has the keyboard, {} -> {}",
        before,
        harness.app.program.graph_zoom
    );
    let before = harness.app.program.graph_zoom;
    harness.frame_with(&[(egui::Key::Minus, ctrl())]);
    assert!(
        (harness.app.program.graph_zoom - before / VIEW_ZOOM_STEP).abs() < 1e-4,
        "Ctrl+Minus zooms one step out, {} -> {}",
        before,
        harness.app.program.graph_zoom
    );
    assert!(
        !harness.app.program.graph_auto_fit,
        "a zoomed view stops refitting, so the step stays on screen"
    );
}

/// Ctrl+0 refits, and the card's status reports the viewport width in mm
/// that the card is drawn at. A status painted before the card would show
/// the previous scale for the fit frame; this one is painted after the
/// projection has been made, so the frame that refits already reads the
/// fitted width.
#[test]
fn ctrl_zero_refits_the_toolpath_and_the_mm_status_follows_it() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Split;
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    // A view the operator has moved: the fit has something to do.
    harness.app.program.graph_auto_fit = false;
    harness.app.program.graph_zoom = 4.0;
    let output = harness.frame_full(Vec::new());
    let zoomed = zoom_readout(&output).expect("the card paints its status line");
    assert!(
        !zoomed.starts_with("ZOOM"),
        "the readout is the viewport width, not the zoom factor: {zoomed:?}"
    );
    let zoomed_mm = readout_mm(&zoomed);
    assert!(zoomed_mm > 0.0, "the width is a real span: {zoomed:?}");

    let output = harness.frame_full(vec![egui::Event::Key {
        key: egui::Key::Num0,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: ctrl(),
    }]);
    let fitted = harness.app.program.graph_zoom;
    assert!(
        (fitted - 4.0).abs() > 0.1,
        "Ctrl+0 refits the card instead of leaving it at the operator's zoom"
    );
    assert!(
        harness.app.program.graph_auto_fit,
        "the fit re-arms the follow-the-program framing"
    );
    // The fit frame is already drawn at the fitted scale, so its status is
    // the fitted width. The span is inverse to the zoom, and the operator's
    // zoom was 4x the fitted one.
    let fitted_readout = zoom_readout(&output).expect("the fitted card paints its status");
    let fitted_mm = readout_mm(&fitted_readout);
    assert!(
        (fitted_mm / zoomed_mm - 4.0 / fitted).abs() < 0.25,
        "the fit frame reports the fitted width: {fitted_readout:?} against {zoomed:?} at fitted zoom {fitted}"
    );
    for _ in 0..2 {
        let output = harness.frame_full(Vec::new());
        assert_eq!(
            zoom_readout(&output).as_deref(),
            Some(fitted_readout.as_str()),
            "the fitted width is what the readout shows from then on"
        );
    }
}

/// The panel keyboard has no Tab key, so a click is how the keyboard leaves
/// the G-code editor. Clicking the toolpath hands it the card, and the pan
/// chords work from there.
#[test]
fn clicking_the_toolpath_hands_it_the_keyboard_and_the_pan_chords_work() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Split;
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    assert!(
        harness.ctx.memory(|memory| memory.has_focus(id)),
        "the editor starts with the keyboard"
    );

    harness.frame_events(vec![press_at(card, true)]);
    harness.frame_events(vec![press_at(card, false)]);
    assert!(
        !harness.ctx.memory(|memory| memory.has_focus(id)),
        "the click took the keyboard away from the editor"
    );

    let before = harness.app.program.graph_pan;
    harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
    assert!(
        (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
        "Ctrl+Left pans the toolpath once the card holds the keyboard, {:?} -> {:?}",
        before,
        harness.app.program.graph_pan
    );
}

/// The line move keeps every chord it had: with the editor focused,
/// Ctrl+Up/Down moves the selected block and Ctrl+Left/Right moves the
/// caret's word — none of them may pan the toolpath.
#[test]
fn a_focused_editor_keeps_ctrl_up_down_and_ctrl_left_right() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Split;
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.frame_events(Vec::new());
    assert!(
        linc_view::toolpath_hovered(&harness.ctx),
        "pointer on the card"
    );

    let pan = harness.app.program.graph_pan;
    harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
    assert_eq!(
        harness.app.program.graph_pan, pan,
        "Ctrl+Left belongs to the editor while it holds the keyboard"
    );
    assert_eq!(
        harness.app.nav.focus_region,
        FocusRegion::Page,
        "and it does not move the focus ring either"
    );

    // A read-only pane has no line move, so use an editable one: the shell
    // only takes the chord when the page allows an edit.
    // Line 2 moves up past line 1: the line move answers Ctrl+Up only for a
    // line that has one above it.
    harness.app.program.selected_line = 2;
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    assert_eq!(
        harness.app.program.graph_pan, pan,
        "Ctrl+Up moves the selected line, not the view"
    );
    assert_eq!(
        harness.app.program.editor.text(),
        "G1 X2\nG0 X1\nG1 X3\n",
        "the selected block moved up, so the chord reached the editor"
    );
}

/// A nav click is the operator leaving the text field behind, so the row
/// that draws its focus border also takes the keyboard back.
///
/// The row of the page already in front is the case that matters: it
/// navigates nowhere, so nothing else would have released the editor, and
/// the pan chords stayed dead while the row drew its focus border. The
/// panel keyboard has no Tab key to move focus with.
#[test]
fn a_navigation_click_releases_the_keyboard_from_the_gcode_editor() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\nG1 X3\n".into());
    harness.app.set_page(Page::Program);
    harness.app.program.view = ProgramView::Split;
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    assert!(harness.ctx.memory(|memory| memory.has_focus(id)));

    let row = nav_row_center(&output, "PROGRAM");
    harness.frame_events(vec![egui::Event::PointerMoved(row)]);
    harness.frame_events(vec![press_at(row, true)]);
    harness.frame_events(vec![press_at(row, false)]);
    assert_eq!(
        harness.app.nav.page,
        Page::Program,
        "the same page, so nothing navigated"
    );
    assert!(
        !harness.ctx.memory(|memory| memory.has_focus(id)),
        "the nav click left the editor holding the keyboard"
    );
    assert_eq!(harness.app.nav.focus_region, FocusRegion::Nav);

    // Back over the toolpath — the row cycled SPLIT to CODE, so it is the
    // thumbnail beside the editor now — the pan chord is live again.
    let output = harness.frame_full(Vec::new());
    let card = toolpath_card_point(&output);
    harness.frame_events(vec![egui::Event::PointerMoved(card)]);
    let before = harness.app.program.graph_pan;
    harness.frame_with(&[(egui::Key::ArrowLeft, ctrl())]);
    assert!(
        (harness.app.program.graph_pan.x - (before.x - VIEW_PAN_STEP)).abs() < 1e-3,
        "Ctrl+Left pans after the nav click, {:?} -> {:?}",
        before,
        harness.app.program.graph_pan
    );
}

#[test]
fn ctrl_left_still_moves_region_focus_away_from_the_toolpath() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness.app.set_page(Page::Machine);
    harness.ctx.data_mut(|data| {
        data.insert_temp(egui::Id::unique(linc_view::TOOLPATH_HOVER_ID), false);
    });
    harness.frame(&[]);
    let before = harness.app.program.graph_pan;
    harness.frame_with(&[(egui::Key::ArrowLeft, egui::Modifiers::CTRL)]);
    assert_eq!(harness.app.nav.focus_region, FocusRegion::Nav);
    assert_eq!(
        harness.app.program.graph_pan, before,
        "no pan without the pointer on the toolpath"
    );
}

#[test]
fn ctrl_arrow_stays_in_a_focused_text_editor() {
    let mut harness = ProbeHarness::new(PanelMode::Edit, false, true);
    harness
        .app
        .program
        .editor
        .replace_text("G0 X1\nG1 X2\n".into());
    harness.app.set_page(Page::Program);
    let id = egui::Id::unique("linc-program-editor");
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.frame_with(&[(egui::Key::ArrowLeft, egui::Modifiers::CTRL)]);
    assert_eq!(
        harness.app.nav.focus_region,
        FocusRegion::Page,
        "Ctrl+Left must stay in the editor instead of moving focus to the nav"
    );
}

#[test]
fn backspace_deletes_a_file_on_the_files_page() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let path = harness.files_page_with_one_file("backspace-delete-test.ngc");

    harness.frame(&[egui::Key::Backspace]);

    match harness.app.overlay.modal {
        Some(crate::state::Modal::ConfirmDelete {
            path: deleted,
            usb: false,
            ..
        }) => assert_eq!(deleted, path),
        ref other => panic!("expected a delete confirmation, got {other:?}"),
    }
    assert!(path.exists(), "nothing is deleted before confirmation");
}

#[test]
fn the_cut_shortcut_does_nothing_on_the_files_page() {
    // Ctrl+X is reserved for text editing: the Files page deletes with
    // Backspace and must not react to the clipboard chord at all.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let path = harness.files_page_with_one_file("cut-does-nothing.ngc");

    let commands = harness.frame_events(vec![egui::Event::Cut]);

    assert!(
        harness.app.overlay.modal.is_none(),
        "Ctrl+X must not open a dialog: {:?}",
        harness.app.overlay.modal
    );
    assert!(path.exists());
    assert!(harness.app.files.rename.is_none());
    // Nothing consumed the Cut event, so egui asked for no clipboard write.
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyText(_))),
        "{commands:?}"
    );
    // And the reserved chord never became a synthetic key.
    assert_eq!(harness.app.probe.error, None);
}

#[test]
fn position_measurement_still_runs_with_protect_on() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    harness.frame(&[]);
    // POSITION: the Run preamble assigns probe parameters, which is setup,
    // not a calibration or result write, so PROTECT must not block it.
    assert!(harness.app.probe.layout.run_reason_text.is_none());
    screens::probe::run(&mut harness.app);
    assert_eq!(harness.app.probe.error, None);
    let sent = harness.mdi_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("o<probe_back_left_top_corner> call"),
        "{}",
        sent[0]
    );
}

#[test]
fn direct_wco_and_edge_angle_runs_keep_their_protect_gates() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    harness.app.probe.params.probe_mode = linc_core::probe::ProbeMode::ApplyWcs;
    harness.app.snapshot.offsets.active_index = Some(1);
    harness.frame(&[]);
    screens::probe::run(&mut harness.app);
    let error = harness.app.probe.error.clone().unwrap_or_default();
    assert!(error.contains("direct WCO"), "{error}");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );

    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
    harness.app.snapshot.offsets.active_index = Some(1);
    harness.frame(&[]);
    screens::probe::run(&mut harness.app);
    let error = harness.app.probe.error.clone().unwrap_or_default();
    assert!(error.contains("edge-angle R0 reset"), "{error}");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn protect_keeps_the_calibration_reset_closed() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, true, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    harness.frame_with(&[(egui::Key::R, egui::Modifiers::CTRL)]);
    let error = harness.app.probe.error.clone().unwrap_or_default();
    assert!(error.contains("PROTECT"), "{error}");
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

// PR-06: every page entry starts with nothing selected.

#[test]
fn files_return_clears_the_probe_selection_but_keeps_the_tab() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
    harness.app.probe.param_focused = true;
    harness.app.probe.param_editing = true;
    harness.app.probe.param_buffer = "12".into();
    harness.app.probe.dro_edit = Some(crate::state::DroEdit {
        axis: "X".into(),
        buffer: "1.0".into(),
        focus: false,
    });

    harness.app.request_files();
    assert_eq!(harness.app.nav.page, Page::Files);
    harness.app.escape();

    assert_eq!(harness.app.nav.page, Page::Probe);
    assert_eq!(
        harness.app.probe.category,
        ProbeCategory::EdgeAngle,
        "the tab the operator left is kept"
    );
    assert!(harness.app.probe.operation.is_empty());
    assert!(!harness.app.probe.param_focused);
    assert!(!harness.app.probe.param_editing);
    assert!(harness.app.probe.dro_edit.is_none());

    // Enter now has nothing to run.
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        harness.app.probe.error.as_deref(),
        Some("select an operation"),
        "{:?}",
        harness.sent_debug()
    );
    assert!(
        harness.probe_writes().is_empty(),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn f8_opens_the_first_probe_tab_with_no_routine_selected() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.app.set_page(Page::Offsets);
    harness.app.probe.category = ProbeCategory::InsideCorners;
    harness.app.probe.operation = "rect_pocket_center_start".into();

    harness.frame(&[egui::Key::F8]);
    assert_eq!(harness.app.nav.page, Page::Probe);
    assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
    assert!(harness.app.probe.operation.is_empty());
}

#[test]
fn paging_probe_tabs_clears_the_selection() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");

    harness.frame(&[egui::Key::PageDown]);
    assert_eq!(harness.app.probe.category, ProbeCategory::InsideCorners);
    assert!(harness.app.probe.operation.is_empty());

    harness.app.probe.operation = "surface_z".into();
    harness.frame(&[egui::Key::PageUp]);
    assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
    assert!(harness.app.probe.operation.is_empty());
}

#[test]
fn escape_returns_to_offsets_and_reentry_never_restores_a_selection() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");

    harness.frame(&[egui::Key::Escape]);
    assert_eq!(harness.app.nav.page, Page::Offsets);

    harness.frame(&[egui::Key::F8]);
    assert_eq!(harness.app.nav.page, Page::Probe);
    assert_eq!(harness.app.probe.category, ProbeCategory::OutsideCorners);
    assert!(harness.app.probe.operation.is_empty());
}

// PR-06: a page restored after its mode went away is still an entry.

#[test]
fn restoring_the_probe_page_after_mode_loss_clears_the_selection() {
    // MDI is not in this list: it opens MAIN, which is not a mode-driven
    // page, so a mode loss there has no page to restore.
    for (mode, mode_page) in [
        (PanelMode::Jog, Page::Manual),
        (PanelMode::Home, Page::Homing),
    ] {
        for loss in [
            PanelModeValidity::Invalid(vec![PanelMode::Auto, mode]),
            PanelModeValidity::Unknown,
        ] {
            let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
            harness.enter_probe(ProbeCategory::EdgeAngle, "top_back_edge_angle");
            harness.app.probe.param_focused = true;
            harness.app.probe.param_editing = true;
            harness.app.probe.param_buffer = "12".into();
            harness.app.probe.dro_edit = Some(crate::state::DroEdit {
                axis: "X".into(),
                buffer: "1.0".into(),
                focus: false,
            });

            // The panel switches to a mode-driven page.
            harness.app.nav.last_mode = Some(PanelModeValidity::Valid(PanelMode::Auto));
            harness.app.on_mode_change(PanelModeValidity::Valid(mode));
            assert_eq!(harness.app.nav.page, mode_page, "{mode:?}");
            assert_eq!(harness.app.nav.last_non_mode_page, Page::Probe, "{mode:?}");

            // ...and the mode goes away: Probe comes back through the entry
            // logic, with the tab kept and nothing selected.
            harness.app.on_mode_change(loss.clone());
            assert_eq!(harness.app.nav.page, Page::Probe, "{mode:?}/{loss:?}");
            assert_eq!(
                harness.app.probe.category,
                ProbeCategory::EdgeAngle,
                "{mode:?}: the tab is kept"
            );
            assert!(harness.app.probe.operation.is_empty(), "{mode:?}");
            assert!(!harness.app.probe.param_focused, "{mode:?}");
            assert!(!harness.app.probe.param_editing, "{mode:?}");
            assert!(harness.app.probe.dro_edit.is_none(), "{mode:?}");

            // Enter has nothing to run.
            harness.frame(&[egui::Key::Enter]);
            assert_eq!(
                harness.app.probe.error.as_deref(),
                Some("select an operation"),
                "{mode:?}"
            );
            assert!(
                harness.probe_writes().is_empty(),
                "{mode:?}/{loss:?}: {:?}",
                harness.sent_debug()
            );
        }
    }
}

#[test]
fn a_valid_mode_takes_its_own_page_so_normal_transitions_never_restore_probe() {
    // Every valid mode drives a page of its own, so the normal transitions
    // (MDI -> AUTO, MANUAL -> EDIT, HOME -> JOG) end on that page and never
    // restore a Probe page. Only a mode LOSS restores, and only that path
    // has to clear the Probe selection.
    // MDI opens MAIN, which is not mode-driven: it never has a page to
    // restore, so only the mode-driven positions are in this table.
    for (from, to, destination) in [
        (PanelMode::Jog, PanelMode::Edit, Page::Program),
        (PanelMode::Home, PanelMode::Jog, Page::Manual),
    ] {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");
        harness.app.probe.param_focused = true;

        harness.app.nav.last_mode = Some(PanelModeValidity::Valid(PanelMode::Auto));
        harness.app.on_mode_change(PanelModeValidity::Valid(from));
        assert!(harness.app.nav.page.is_mode_driven(), "{from:?}");

        // `sync` records the new mode before it calls back in.
        harness.app.nav.last_mode = Some(PanelModeValidity::Valid(from));
        harness.app.on_mode_change(PanelModeValidity::Valid(to));
        assert_eq!(harness.app.nav.page, destination, "{from:?} -> {to:?}");
    }
}

// PR-09 and PR-10: the 1280x720 page keeps its Run context and its targets.

/// Every clickable kind the Probe page painted, as the page recorded it.
fn probe_targets(layout: &crate::screens::probe_view::ProbeLayout) -> Vec<(String, egui::Rect)> {
    let mut targets = Vec::new();
    for (kind, slot) in [
        ("dro", layout.dro_target),
        ("parameter", layout.parameter_target),
        ("calibration card", layout.calibrate_card),
        ("calibration action", layout.calibrate_action),
    ] {
        if let Some(rect) = slot {
            targets.push((kind.to_string(), rect));
        }
    }
    for (index, rect) in &layout.hint_fields {
        targets.push((format!("hint field #{index}"), *rect));
    }
    for (operation, rect) in &layout.routine_cards {
        targets.push((format!("card {operation}"), *rect));
    }
    targets
}

#[test]
fn every_probe_pointer_target_meets_the_44px_minimum() {
    for (state, protect, power) in [
        ("banner-free", false, MachinePower::On),
        ("protect", true, MachinePower::On),
        ("estop", true, MachinePower::Estop),
    ] {
        for (category, operation) in [
            (ProbeCategory::OutsideCorners, "back_left_top_corner"),
            (ProbeCategory::InsideCorners, "back_left_inside_corner"),
            (ProbeCategory::BossAndPocket, "round_boss"),
            (ProbeCategory::RidgeAndValley, "ridge_x"),
            (ProbeCategory::EdgeAngle, "top_back_edge_angle"),
            (ProbeCategory::Calibrate, "cal_round_boss"),
        ] {
            let mut harness = ProbeHarness::ready(PanelMode::Auto, protect, true, power);
            harness.enter_probe(category, operation);
            harness.frame(&[]);
            let layout = harness.app.probe.layout.clone();

            let targets = probe_targets(&layout);
            assert!(
                !targets.is_empty(),
                "{state}/{category:?}: the page recorded no targets"
            );
            for (kind, rect) in targets {
                assert!(
                    rect.width() >= theme::MIN_TARGET && rect.height() >= theme::MIN_TARGET,
                    "{state}/{category:?}: {kind} is {:?}",
                    rect.size()
                );
            }

            if category == ProbeCategory::Calibrate {
                assert!(
                    layout.calibrate_card.is_some(),
                    "{state}: the calibration cards are targets"
                );
                assert!(
                    layout.calibrate_action.is_some(),
                    "{state}: the calibration action is a target"
                );
                continue;
            }
            assert_eq!(
                layout.routine_cards.len(),
                crate::screens::probe_view::card_count(category),
                "{state}/{category:?}: every card must be measured"
            );
            if matches!(
                category,
                ProbeCategory::BossAndPocket | ProbeCategory::RidgeAndValley
            ) {
                assert!(
                    !layout.hint_fields.is_empty(),
                    "{state}/{category:?}: the hint fields are targets"
                );
            }
        }
    }
}

#[test]
fn boss_and_ridge_hint_fields_stack_in_aligned_columns() {
    for category in [ProbeCategory::BossAndPocket, ProbeCategory::RidgeAndValley] {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(category, "");
        harness.frame(&[]);
        let layout = harness.app.probe.layout.clone();
        let labels = &layout.hint_labels;
        let fields = &layout.hint_fields;
        assert_eq!(
            labels.len(),
            fields.len(),
            "{category:?}: every hint field has a label"
        );
        assert!(
            fields.len() >= 2,
            "{category:?}: the unselected tab shows both hint rows"
        );
        let label_left = labels[0].1.left();
        let field_left = fields[0].1.left();
        let field_width = fields[0].1.width();
        let routines_center = layout
            .routine_grid
            .expect("the routine grid was recorded")
            .center()
            .x;
        assert!(
            (fields[0].1.center().x - routines_center).abs() < 0.5,
            "{category:?}: hint inputs must be centred horizontally"
        );
        let mut previous_bottom = f32::NEG_INFINITY;
        for (label, field) in labels.iter().zip(fields) {
            assert_eq!(label.0, field.0, "{category:?}: rows stay paired");
            assert!(
                (label.1.left() - label_left).abs() < 0.5,
                "{category:?}: labels share one column"
            );
            assert!(
                (field.1.left() - field_left).abs() < 0.5,
                "{category:?}: fields share one column"
            );
            assert!(
                (field.1.width() - field_width).abs() < 0.5,
                "{category:?}: fields share one width"
            );
            assert!(
                (label.1.center().y - field.1.center().y).abs() < 0.5,
                "{category:?}: the label is vertically aligned with its field"
            );
            assert!(
                field.1.top() >= previous_bottom,
                "{category:?}: fields are stacked top to bottom"
            );
            assert!(
                inside(layout.page, field.1),
                "{category:?}: hint field {:?} is off the page",
                field.1
            );
            previous_bottom = field.1.bottom();
        }
    }
}

#[test]
fn tall_windows_keep_hints_under_the_routine_cards() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::BossAndPocket, "");
    let mut input = probe_raw_input(Vec::new());
    input.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(1920.0, 1080.0),
    ));
    harness.frame_input(input);
    let layout = harness.app.probe.layout.clone();
    let cards_bottom = layout
        .routine_cards
        .iter()
        .map(|(_, rect)| rect.bottom())
        .fold(f32::NEG_INFINITY, f32::max);
    let hint_top = layout
        .hint_fields
        .first()
        .expect("the hint fields were drawn")
        .1
        .top();
    assert!(
        (hint_top - (cards_bottom + 10.0)).abs() < 1.0,
        "hints start at {hint_top:?}, cards end at {cards_bottom:?}"
    );
}

#[test]
fn calibration_fields_share_one_column() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    let output = harness.frame_full(Vec::new());
    let fields = harness.app.probe.layout.calibrate_fields.clone();
    assert_eq!(
        fields.len(),
        4,
        "offset, diameter, X width and Y width are drawn"
    );
    let left = fields[0].1.left();
    let width = fields[0].1.width();
    for (index, rect) in &fields {
        assert!(
            (rect.left() - left).abs() < 0.5,
            "calibration field {index} starts at {:?}, not {left:?}",
            rect.left()
        );
        assert!(
            (rect.width() - width).abs() < 0.5,
            "calibration field {index} width is {:?}, not {width:?}",
            rect.width()
        );
    }

    let texts = painted_text(&output);
    for (label, index) in [
        ("CALIBRATION OFFSET", 18),
        ("CALIBRATION DIAMETER", 19),
        ("CALIBRATION WIDTH X", 20),
        ("CALIBRATION WIDTH Y", 21),
    ] {
        let (_, label_rect) = texts
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("{label} is not painted"));
        let field = fields
            .iter()
            .find(|(field_index, _)| *field_index == index)
            .map(|(_, rect)| *rect)
            .expect("the calibration field was recorded");
        assert!(
            (label_rect.center().y - field.center().y).abs() < 0.5,
            "{label} is not vertically centred on its field"
        );
        assert!(
            field.left() - label_rect.right() >= 16.0 - 0.5,
            "{label} has too little padding before its field"
        );
    }
}

#[test]
fn calibration_digits_select_the_bottom_row_first() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "");
    harness.frame(&[]);
    harness.frame_with(&[(egui::Key::Num1, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.probe.operation, "cal_square_pocket");
    harness.frame_with(&[(egui::Key::Num2, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.probe.operation, "cal_square_boss");
    harness.frame_with(&[(egui::Key::Num3, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.probe.operation, "cal_round_pocket");
    harness.frame_with(&[(egui::Key::Num4, egui::Modifiers::NONE)]);
    assert_eq!(harness.app.probe.operation, "cal_round_boss");
}

#[test]
fn calibration_arrows_move_from_fields_to_actions_and_back() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    harness.frame(&[]);
    harness.frame_with(&[(
        egui::Key::P,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    assert_eq!(harness.app.probe.param_selected, 19);
    assert!(harness.app.probe.param_focused);

    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.probe.param_selected, 20);
    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.probe.param_selected, 21);
    harness.frame(&[egui::Key::ArrowDown]);
    assert!(!harness.app.probe.param_focused);
    assert_eq!(harness.app.probe.calibrate_action, Some(0));

    harness.frame(&[egui::Key::ArrowRight]);
    assert_eq!(harness.app.probe.calibrate_action, Some(1));
    harness.frame(&[egui::Key::ArrowRight]);
    assert_eq!(harness.app.probe.calibrate_action, Some(2));
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.probe.calibrate_action, Some(1));
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.probe.calibrate_action, Some(0));
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.probe.calibrate_action, None);
    assert!(harness.app.probe.param_focused);
    assert_eq!(harness.app.probe.param_selected, 21);
}

#[test]
fn enter_on_a_focused_calibration_action_runs_it() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    harness.frame(&[]);
    harness.frame_with(&[(
        egui::Key::P,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    harness.frame(&[egui::Key::ArrowDown]);
    harness.frame(&[egui::Key::ArrowDown]);
    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.probe.calibrate_action, Some(0));
    let sent = harness.mdi_sent();
    harness.frame(&[egui::Key::Enter]);
    let after = harness.mdi_sent();
    assert!(
        after.len() > sent.len(),
        "focused CAL ON action must issue MDI: {:?}",
        harness.sent_debug()
    );
    assert!(
        after
            .iter()
            .any(|block| block.contains("o<probe_cal_round_boss> call")),
        "{:?}",
        harness.sent_debug()
    );
}

#[test]
fn probe_labels_hide_internal_numbers_and_empty_apply_prompts() {
    for category in [
        ProbeCategory::OutsideCorners,
        ProbeCategory::InsideCorners,
        ProbeCategory::BossAndPocket,
        ProbeCategory::RidgeAndValley,
        ProbeCategory::EdgeAngle,
    ] {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(category, "");
        let output = harness.frame_full(Vec::new());
        let texts = painted_text(&output);
        for (text, _) in &texts {
            assert!(
                !text.contains("#30"),
                "{category:?}: internal parameter number is painted: {text}"
            );
            assert!(
                !text.contains("ACCENT BORDER"),
                "{category:?}: apply-source legend is painted: {text}"
            );
            assert!(
                !text.contains("NO PROBE RESULT"),
                "{category:?}: empty apply prompt is painted: {text}"
            );
        }
    }

    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::Calibrate, "cal_round_boss");
    let output = harness.frame_full(Vec::new());
    let texts = painted_text(&output);
    for (text, _) in &texts {
        assert!(
            !text.contains("#30"),
            "Calibrate: internal parameter number is painted: {text}"
        );
        assert!(
            !text.contains("CONTROLLER SYNC"),
            "Calibrate: controller-sync label is painted: {text}"
        );
        assert!(
            !text.contains("Ctrl+R RESET CAL"),
            "Calibrate: reset button is painted: {text}"
        );
        assert!(
            !text.contains("RESET CALIBRATION"),
            "Calibrate: reset shortcut is not shortened: {text}"
        );
    }
    assert!(
        texts.iter().any(|(text, _)| text == "CALIBRATION OFFSET"),
        "Calibrate: the offset label remains"
    );
    assert!(
        texts.iter().any(|(text, _)| text == "RESET CAL"),
        "Calibrate: the shortened reset shortcut is painted"
    );
}

#[test]
fn ctrl_p_focuses_the_first_routine_hint_on_boss_and_ridge() {
    for (category, operation, expected) in [
        (ProbeCategory::BossAndPocket, "", 11),
        (ProbeCategory::BossAndPocket, "rect_boss", 12),
        (ProbeCategory::RidgeAndValley, "", 14),
        (ProbeCategory::RidgeAndValley, "ridge_y", 15),
    ] {
        let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
        harness.enter_probe(category, operation);
        harness.frame(&[]);
        harness.frame_with(&[(
            egui::Key::P,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        )]);
        assert_eq!(
            harness.app.probe.param_selected, expected,
            "{category:?}/{operation}: Ctrl+P must start at the routine hint"
        );
        assert!(harness.app.probe.param_focused, "{category:?}");
        assert!(!harness.app.probe.param_editing, "{category:?}");
        assert!(
            harness
                .app
                .probe
                .layout
                .hint_fields
                .iter()
                .any(|(index, _)| *index == expected),
            "{category:?}/{operation}: the focused hint is visible"
        );
    }
}

#[test]
fn arrows_walk_the_stacked_hints_one_row_at_a_time() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::BossAndPocket, "");
    harness.frame(&[]);
    harness.frame_with(&[(
        egui::Key::P,
        egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
    )]);
    assert_eq!(harness.app.probe.param_selected, 11);

    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.probe.param_selected, 12);
    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.probe.param_selected, 13);
    // The hints are one column, so Left/Right do not leave the row.
    harness.frame(&[egui::Key::ArrowRight]);
    assert_eq!(harness.app.probe.param_selected, 13);
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.probe.param_selected, 12);
}

#[test]
fn a_pointer_pick_updates_the_run_context_and_hints_in_the_same_frame() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::BossAndPocket, "round_boss");
    harness.frame(&[]);
    assert_eq!(
        harness.app.probe.layout.hints_drawn.as_deref(),
        Some(&[11usize][..]),
        "the round boss shows only the boss/pocket diameter hint"
    );

    let target = harness
        .app
        .probe
        .layout
        .routine_cards
        .iter()
        .find(|(operation, _)| operation == "rect_boss")
        .map(|(_, rect)| rect.center())
        .expect("the rect boss card was recorded");

    harness.click_at(target);

    assert_eq!(harness.app.probe.operation, "rect_boss");
    let layout = harness.app.probe.layout.clone();
    let name = linc_core::probe::operation("rect_boss").unwrap().name;
    let action = layout.run_action_text.clone().unwrap_or_default();
    assert!(
        action.contains(name),
        "the Run context must name the picked routine in the same frame: {action}"
    );
    assert_eq!(
        layout.hints_drawn.as_deref(),
        Some(&[12usize, 13][..]),
        "the hint row must follow the picked routine in the same frame"
    );
}

#[test]
fn every_parameter_name_fits_inside_its_target() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.enter_probe(ProbeCategory::OutsideCorners, "back_left_top_corner");
    harness.frame(&[]);
    let cell = harness
        .app
        .probe
        .layout
        .parameter_target
        .expect("parameter field drawn")
        .width();
    let mut labels: Vec<&str> = crate::screens::probe_view::COMMON_PARAMS
        .iter()
        .map(|index| crate::screens::probe_view::PARAM_LABELS[*index])
        .collect();
    labels.push("STYLUS DIAM #5410");

    let mut widths = Vec::new();
    let ctx = harness.ctx.clone();
    let mut output = ctx.run_ui(probe_raw_input(Vec::new()), |ui| {
        for label in &labels {
            let width = ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        (*label).to_string(),
                        egui::FontId::monospace(9.0),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            });
            widths.push((label, width));
        }
    });
    output.textures_delta.clear();

    for (label, width) in widths {
        assert!(
            width <= cell - 4.0,
            "{label} needs {width:.1} px of a {cell:.1} px field"
        );
    }
}

#[test]
fn the_action_and_reason_lines_fit_at_1280x720_with_banners() {
    // PROTECT, E-stop (two banners) and banner-free all have to show the
    // action, and the reason when Run is refused, inside the page.
    for (label, protect, power, connected) in [
        ("protect", true, MachinePower::On, true),
        ("estop", true, MachinePower::Estop, true),
        ("offline", true, MachinePower::On, false),
        ("plain", false, MachinePower::On, true),
    ] {
        for (category, operation) in [
            (ProbeCategory::OutsideCorners, "back_left_top_corner"),
            (ProbeCategory::EdgeAngle, "top_back_edge_angle"),
            (ProbeCategory::BossAndPocket, "round_boss"),
            (ProbeCategory::Calibrate, "cal_round_boss"),
        ] {
            let mut harness = ProbeHarness::ready(PanelMode::Auto, protect, connected, power);
            harness.enter_probe(category, operation);
            harness.frame(&[]);
            let layout = harness.app.probe.layout.clone();
            let page = layout.page;
            let action = layout
                .run_action
                .unwrap_or_else(|| panic!("{label}/{category:?}: no action line"));
            assert!(
                inside(page, action),
                "{label}/{category:?}: {action:?} vs {page:?}"
            );
            // A POSITION measurement is available under PROTECT; the
            // Edge Angle R0 reset and a calibration #3032 write are not.
            let blocked = !connected
                || !matches!(power, MachinePower::On)
                || (protect
                    && matches!(
                        category,
                        ProbeCategory::EdgeAngle | ProbeCategory::Calibrate
                    ));
            match (blocked, layout.run_reason) {
                (true, Some(reason)) => {
                    let text = layout.run_reason_text.clone().unwrap_or_default();
                    assert!(text.contains("RUN UNAVAILABLE"), "{label}: {text}");
                    assert!(
                        inside(page, reason),
                        "{label}/{category:?}: reason {reason:?} vs {page:?}"
                    );
                    assert!(
                        action.bottom() <= reason.top(),
                        "{label}/{category:?}: action and reason overlap"
                    );
                }
                (true, None) => {
                    panic!("{label}/{category:?}: a blocked Run must say why")
                }
                (false, Some(reason)) => {
                    panic!("{label}/{category:?}: an available Run shows {reason:?}")
                }
                (false, None) => {}
            }
            if category == ProbeCategory::Calibrate {
                // The Calibrate tab has its own panel instead of the
                // routine columns.
                let panel = layout.calibrate_panel.expect("calibration panel drawn");
                assert!(
                    panel.bottom() <= page.bottom() + 0.5,
                    "{label}/{category:?}: panel {panel:?} vs {page:?}"
                );
                continue;
            }
            let grid = layout.routine_grid.expect("grid drawn");
            assert!(inside(page, grid), "{label}/{category:?}: grid {grid:?}");
            let left = layout.left_column.expect("left column drawn");
            assert!(
                left.bottom() <= page.bottom() + 0.5,
                "{label}/{category:?}: left column {left:?} vs {page:?}"
            );
        }
    }
}

// The MDI pane: the panel position replaces MAIN's program card.

fn mdi_harness() -> ProbeHarness {
    ProbeHarness::new(PanelMode::Mdi, false, true)
}

fn blocks(app: &LincApp) -> Vec<String> {
    app.mdi
        .blocks
        .iter()
        .map(|(block, _)| block.clone())
        .collect()
}

fn history_limit(app: &LincApp) -> usize {
    app.profile.config.mdi.history_limit
}

use linc_core::gcode::SegmentKind;

/// Turn the panel switch the way a frame sees it: the snapshot reports the
/// new position and the mode handler runs against it, as `sync` does.
fn switch_mode(app: &mut LincApp, mode: PanelMode) {
    app.snapshot.panel.mode = PanelModeValidity::Valid(mode);
    app.on_mode_change(PanelModeValidity::Valid(mode));
}

/// The card frames the frame painted: a rounded rect with the border
/// colour, which is what `theme::card` and `common::shell_card` draw.
fn painted_cards(output: &egui::FullOutput) -> Vec<egui::Rect> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Rect(rect) => {
                if rect.stroke.color == theme::BORDER && rect.corner_radius.nw == 6 {
                    out.push(rect.rect);
                }
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, out);
                }
            }
            _ => {}
        }
    }
    let mut rects = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut rects);
    }
    rects
}

/// Every rect the frame painted, for measuring what a card or a row is.
fn painted_rects(output: &egui::FullOutput) -> Vec<egui::Rect> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Rect(rect) => out.push(rect.rect),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, out);
                }
            }
            _ => {}
        }
    }
    let mut rects = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut rects);
    }
    rects
}

/// Every text the frame painted, with the rect it landed in.
fn painted_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(text) => out.push((
                text.galley.text().to_string(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, out);
                }
            }
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut texts);
    }
    texts
}

/// Type one character per frame, the way the panel delivers them.
fn type_text(harness: &mut ProbeHarness, text: &str) {
    for character in text.chars() {
        harness.frame_events(vec![egui::Event::Text(character.to_string())]);
    }
}

/// Where the input's text cursor sits, in characters.
fn mdi_caret(harness: &ProbeHarness) -> usize {
    let id = egui::Id::unique("linc-mdi-input");
    egui::text_edit::TextEditState::load(&harness.ctx, id)
        .and_then(|state| state.cursor.char_range())
        .map(|range| range.primary.index.0)
        .unwrap_or(0)
}

/// Select a character range of the MDI input, the way a drag would.
fn select_mdi_chars(harness: &ProbeHarness, start: usize, end: usize) {
    let id = egui::Id::unique("linc-mdi-input");
    let mut state = egui::text_edit::TextEditState::load(&harness.ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(start),
            egui::text::CCursor::new(end),
        )));
    state.store(&harness.ctx, id);
}

fn mdi_input_focused(harness: &ProbeHarness) -> bool {
    harness
        .ctx
        .memory(|memory| memory.has_focus(egui::Id::unique("linc-mdi-input")))
}

/// What egui-winit delivers on a non-Mac keyboard: Ctrl also sets
/// command, and a binding parsed from "Ctrl+..." requires it.
fn ctrl() -> egui::Modifiers {
    egui::Modifiers::CTRL | egui::Modifiers::COMMAND
}

/// The pane reserves the MODAL band at the bottom of its card and still
/// fills the slot it was given: the column beside it ends on the same line.
#[test]
fn the_mdi_pane_draws_the_modal_band_and_fills_its_slot() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let slot = egui::Rect::from_min_size(egui::pos2(920.0, 90.0), egui::vec2(352.0, 560.0));
    let (cards, texts) = harness.draw_mdi_pane(slot);
    // The pane's own card fills the slot it was given, to the pixel: the
    // column beside it ends on the same line.
    let card = cards
        .iter()
        .find(|rect| (rect.width() - slot.width()).abs() < 0.5)
        .expect("the pane's card is drawn at the slot's width");
    assert!(
        (card.height() - slot.height()).abs() < 0.5,
        "the card fills its slot: {} of {}",
        card.height(),
        slot.height()
    );
    assert!((card.min.y - slot.min.y).abs() < 0.5);
    assert!(texts.iter().any(|(text, _)| text.contains("BLOCKS")));
    // An idle strip draws nothing: the issue keys are already on the rail.
    assert!(
        !texts
            .iter()
            .any(|(text, _)| text.starts_with("ENTER ISSUES")),
        "the idle strip must not repeat the shortcuts rail"
    );
    harness.app.mdi.last_result = Some("G1 X10".into());
    let (_, texts) = harness.draw_mdi_pane(slot);
    let strip = texts
        .iter()
        .find(|(text, _)| text.starts_with("ISSUED"))
        .expect("the result strip is drawn");
    let modal = texts
        .iter()
        .find(|(text, _)| text == "MODAL")
        .expect("the MODAL band is drawn");
    // Inside the card, at its bottom, and under the strip.
    assert!(
        modal.1.min.y > strip.1.max.y,
        "the band sits below the strip"
    );
    assert!(modal.1.min.x >= slot.left() + 12.0 - 0.5);
    assert!(modal.1.max.x <= slot.right() - 12.0 + 0.5);
    assert!(modal.1.max.y <= slot.bottom() - 12.0 + 0.5);
    assert!(
        modal.1.min.y > slot.center().y,
        "the band takes the bottom of the card, not the middle"
    );
}

#[test]
fn a_block_that_leaves_the_machine_envelope_is_refused() {
    // The pane lights the frame red from the same segments; the gate is what
    // stops Enter from sending a move the machine cannot make.
    let mut harness = mdi_harness();
    harness.app.mdi.input = "G0 X1000".into();
    screens::mdi::issue(&mut harness.app);
    let error = harness.app.mdi.last_error.clone().unwrap_or_default();
    assert!(error.contains("travel limit"), "{error}");
    assert!(harness.mdi_sent().is_empty(), "{:?}", harness.sent_debug());

    // The same axis, inside the envelope, still goes out.
    harness.app.mdi.input = "G0 X-100".into();
    harness.app.mdi.last_error = None;
    screens::mdi::issue(&mut harness.app);
    assert_eq!(harness.app.mdi.last_error, None);
    assert_eq!(harness.mdi_sent(), vec!["G0 X-100".to_string()]);
}

#[test]
fn typing_a_block_inserts_the_space_before_each_value_word() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    harness.frame(&[]);
    assert!(mdi_input_focused(&harness), "the pane holds the keyboard");

    type_text(&mut harness, "G1X10");
    assert_eq!(harness.app.mdi.input, "G1 X10");
    assert_eq!(mdi_caret(&harness), 6, "the caret ends after the character");

    // The same rule from a value word, twice in one block.
    let mut second = mdi_harness();
    second.frame(&[]);
    second.frame(&[]);
    type_text(&mut second, "X10Y5");
    assert_eq!(second.app.mdi.input, "X10 Y5");
    assert_eq!(mdi_caret(&second), 6);

    // A block that is already spaced is left exactly as typed.
    let mut third = mdi_harness();
    third.frame(&[]);
    third.frame(&[]);
    type_text(&mut third, "M3 S1200");
    assert_eq!(third.app.mdi.input, "M3 S1200");
}

#[test]
fn a_leading_value_letter_and_a_spaced_one_get_no_extra_space() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    harness.frame(&[]);
    type_text(&mut harness, "X10");
    assert_eq!(
        harness.app.mdi.input, "X10",
        "nothing before the first word"
    );

    // A letter after an existing space is not touched, and a second space is
    // never typed for it.
    type_text(&mut harness, " Y5");
    assert_eq!(harness.app.mdi.input, "X10 Y5");
}

#[test]
fn a_value_letter_inside_a_comment_or_an_expression_is_left_alone() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    harness.frame(&[]);
    type_text(&mut harness, "G1(aX");
    assert_eq!(harness.app.mdi.input, "G1(aX", "a comment is prose");

    let mut brackets = mdi_harness();
    brackets.frame(&[]);
    brackets.frame(&[]);
    type_text(&mut brackets, "G1[#1X");
    assert_eq!(
        brackets.app.mdi.input, "G1[#1X",
        "an expression is not words"
    );

    // Closed again, the rule applies once more.
    type_text(&mut brackets, "]Y");
    assert_eq!(brackets.app.mdi.input, "G1[#1X] Y");
}

#[test]
fn a_recalled_block_is_not_rewritten_until_a_key_is_typed() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G1X10".into());
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.input, "G1X10");
    harness.frame(&[]);
    harness.frame(&[]);
    assert_eq!(
        harness.app.mdi.input, "G1X10",
        "recall keeps the block as it was issued"
    );
    // The first typed character applies the rule at the caret.
    type_text(&mut harness, "Y");
    assert_eq!(harness.app.mdi.input, "G1X10 Y");
    assert_eq!(mdi_caret(&harness), 7);
}

#[test]
fn a_selection_is_replaced_with_the_space_in_front_of_the_word() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    harness.frame(&[]);
    harness.app.mdi.input = "G1X10".into();
    harness.frame(&[]);
    // Replace the `X10` the way Enter-in-a-cell selects a value.
    select_mdi_chars(&harness, 2, 5);
    type_text(&mut harness, "Y");
    assert_eq!(harness.app.mdi.input, "G1 Y");
    assert_eq!(mdi_caret(&harness), 4, "the caret ends after the character");
}

#[test]
fn auto_brings_up_main_from_anywhere() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    assert_eq!(harness.app.nav.page, Page::Machine, "MDI opens MAIN");
    switch_mode(&mut harness.app, PanelMode::Auto);
    assert_eq!(harness.app.nav.page, Page::Machine, "AUTO brings up MAIN");
    assert!(
        !harness.app.mdi_pane_visible(),
        "the pane goes with the mode, so MAIN shows the program card"
    );

    // From any other page too.
    switch_mode(&mut harness.app, PanelMode::Edit);
    harness.app.set_page(Page::Files);
    switch_mode(&mut harness.app, PanelMode::Auto);
    assert_eq!(harness.app.nav.page, Page::Machine, "AUTO brings up MAIN");

    // The other positions keep their mapping.
    harness.app.set_page(Page::Files);
    switch_mode(&mut harness.app, PanelMode::Edit);
    assert_eq!(harness.app.nav.page, Page::Program);
    assert_eq!(harness.app.program.view, ProgramView::Code);
    switch_mode(&mut harness.app, PanelMode::Jog);
    assert_eq!(harness.app.nav.page, Page::Manual);
    switch_mode(&mut harness.app, PanelMode::Home);
    assert_eq!(harness.app.nav.page, Page::Homing);
    switch_mode(&mut harness.app, PanelMode::Mdi);
    assert_eq!(harness.app.nav.page, Page::Machine);
    // AUTO still has no page of its own, so the boot path is unchanged.
    assert!(mode_page(&PanelModeValidity::Valid(PanelMode::Auto)).is_none());
}

#[test]
fn a_block_previews_in_the_machines_distance_mode() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    // No work offset: the block's numbers are the machine's in this test.
    harness.app.program.preview_offset = None;
    // Away from zero, or an absolute move and a relative one would land on
    // the same number and the test would prove nothing.
    if let Some(x) = harness.app.snapshot.positions.axes.get_mut("X") {
        x.machine = -100.0;
        x.work = -100.0;
    }
    harness.app.mdi.input = "G0 X-10".into();
    let absolute = crate::screens::mdi::overlay(&mut harness.app).to_vec();
    let end = absolute.last().expect("a move").end[0];
    assert!((end + 10.0).abs() < 1e-6, "G90 lands on X-10: {end}");

    // The machine sits in G91: the same block is a 10 mm move from where the
    // tool is, not a jump across the part.
    harness.app.snapshot.program.modal = vec![("G91".into(), String::new())];
    let start = harness.app.tool_tip();
    assert!((start[0] + 100.0).abs() < 1e-6, "the tool is at {start:?}");
    let relative = crate::screens::mdi::overlay(&mut harness.app).to_vec();
    let end = relative.last().expect("a move").end[0];
    assert!(
        (end - (start[0] - 10.0)).abs() < 1e-6,
        "G91 moves 10 mm from {}: {end}",
        start[0]
    );
}

#[test]
fn the_overlay_starts_at_the_tool_and_ends_where_the_block_says() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    // A known frame: no work offset, so the block's numbers are the
    // machine's in this test.
    harness.app.program.preview_offset = None;
    let start = harness.app.tool_tip();
    harness.app.mdi.input = "G1 X10".into();
    let segments = crate::screens::mdi::overlay(&mut harness.app).to_vec();
    assert_eq!(segments.len(), 1, "one block, one move");
    assert_eq!(segments[0].start, start, "the move starts at the tool");
    assert_eq!(segments[0].end, [10.0, start[1], start[2]]);
    assert_eq!(
        segments[0].kind,
        SegmentKind::Feed,
        "a fast move is the cut colour, a solid line"
    );

    // A rapid is the other layer: the renderer dashes it.
    harness.app.mdi.input = "G0 X10".into();
    let rapid = crate::screens::mdi::overlay(&mut harness.app).to_vec();
    assert_eq!(rapid[0].kind, SegmentKind::Rapid);
    assert_ne!(rapid[0].kind, segments[0].kind);
    assert_eq!(rapid[0].start, start);
    assert_eq!(rapid[0].end, [10.0, start[1], start[2]]);

    // Nothing to draw for a block the pane would refuse.
    for block in ["", "   ", "G1 X"] {
        harness.app.mdi.input = block.into();
        assert!(
            crate::screens::mdi::overlay(&mut harness.app).is_empty(),
            "{block:?} draws nothing"
        );
    }
}

#[test]
fn the_overlay_is_parsed_once_per_change() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    harness.app.mdi.input = "G1 X10".into();
    crate::screens::mdi::overlay(&mut harness.app);
    let first = harness.app.mdi.overlay.as_ref().expect("cached").generation;
    // Every frame of an unchanged pane keeps the same parse.
    crate::screens::mdi::overlay(&mut harness.app);
    crate::screens::mdi::overlay(&mut harness.app);
    assert_eq!(
        harness.app.mdi.overlay.as_ref().map(|c| c.generation),
        Some(first)
    );
    // A typed character, a moved machine or a changed modal state re-parses.
    harness.app.mdi.input.push('5');
    crate::screens::mdi::overlay(&mut harness.app);
    let typed = harness.app.mdi.overlay.as_ref().expect("cached").generation;
    assert!(typed > first, "the input changed");
    harness.app.mdi.input.pop();
    harness.app.program.preview_offset = Some(linc_core::model::AxisMap::new());
    crate::screens::mdi::overlay(&mut harness.app);
    let modal = harness.app.mdi.overlay.as_ref().expect("cached").generation;
    assert!(modal > typed, "the modal state changed");
}

#[test]
fn ctrl_up_and_down_move_the_selected_block_and_carry_the_selection() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    for block in ["G0 X1", "G1 X2", "G1 X3"] {
        harness.app.mdi.commit(limit, block.into());
    }
    let sent = harness.mdi_sent().len();
    // No selection: the chords do nothing at all.
    harness.app.mdi.selection = None;
    harness.app.mdi.input.clear();
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
    assert_eq!(
        blocks(&harness.app),
        ["G0 X1", "G1 X2", "G1 X3"],
        "nothing moves without a selection"
    );

    // Recall the last line, then move it up: the selection follows.
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.selection, Some(2));
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X3", "G1 X2"]);
    assert_eq!(harness.app.mdi.selection, Some(1));
    assert_eq!(
        harness.app.mdi.input, "G1 X3",
        "the move leaves the input alone"
    );
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G1 X3", "G0 X1", "G1 X2"]);
    assert_eq!(harness.app.mdi.selection, Some(0));
    // At the top: no wrap, no pop.
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G1 X3", "G0 X1", "G1 X2"]);
    assert_eq!(harness.app.mdi.selection, Some(0));
    // Back down to the last line and stop there.
    harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
    harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X2", "G1 X3"]);
    assert_eq!(harness.app.mdi.selection, Some(2));
    harness.frame_with(&[(egui::Key::ArrowDown, ctrl())]);
    assert_eq!(harness.app.mdi.selection, Some(2));
    assert_eq!(harness.app.mdi.len(), 3);

    // The moves never issue anything and never open a dialog.
    assert_eq!(harness.mdi_sent().len(), sent);
    assert!(harness.app.overlay.modal.is_none());
    // Plain arrows still recall.
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.selection, Some(1));
    assert_eq!(harness.app.mdi.input, "G1 X2");
}

#[test]
fn the_moved_block_is_the_one_run_delete_and_save_act_on() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    for block in ["G0 X1", "G1 X2", "G1 X3"] {
        harness.app.mdi.commit(limit, block.into());
    }
    harness.app.mdi.input.clear();
    harness.app.mdi.selection = Some(2);
    harness.frame_with(&[(egui::Key::ArrowUp, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X3", "G1 X2"]);
    assert_eq!(harness.app.mdi.selection, Some(1));

    // Ctrl+R runs the moved block, not the one that was in that slot.
    harness.frame_with(&[(egui::Key::R, ctrl())]);
    assert_eq!(harness.mdi_sent().last().map(String::as_str), Some("G1 X3"));

    // Ctrl+S writes the buffer in the order the operator sees.
    let dir = harness._dir.path().to_path_buf();
    let path = crate::screens::mdi::save_buffer(&harness.app, &dir, "order", false, false)
        .expect("the buffer saves");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "G0 X1\nG1 X3\nG1 X2\n"
    );

    // Ctrl+Bksp deletes the moved line.
    harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
    assert_eq!(blocks(&harness.app), ["G0 X1", "G1 X2"]);
}

#[test]
fn the_mdi_pane_owns_the_program_slot_on_main_and_outlives_the_page() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    assert_eq!(harness.app.nav.page, Page::Machine, "MDI opens MAIN");
    assert!(harness.app.mdi_pane_visible());

    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X0".into());

    // Another page restores the program card and keeps the buffer.
    harness.frame(&[egui::Key::F6]);
    assert_eq!(harness.app.nav.page, Page::Program);
    assert!(!harness.app.mdi_pane_visible());
    harness.frame(&[egui::Key::F5]);
    assert!(harness.app.mdi_pane_visible(), "MAIN shows the pane again");

    // So does leaving the mode and coming back.
    harness
        .app
        .on_mode_change(PanelModeValidity::Valid(PanelMode::Jog));
    assert!(!harness.app.mdi_pane_visible());
    harness
        .app
        .on_mode_change(PanelModeValidity::Valid(PanelMode::Mdi));
    assert_eq!(harness.app.nav.page, Page::Machine);
    assert!(harness.app.mdi_pane_visible());
    assert_eq!(blocks(&harness.app), vec!["G0 X0".to_string()]);
    // auto_focus: the pane takes the keyboard when it appears, so a block
    // can be typed without reaching for the mouse.
    harness.frame(&[]);
    assert!(
        harness
            .ctx
            .memory(|memory| memory.has_focus(egui::Id::unique("linc-mdi-input"))),
        "the input holds the keyboard"
    );

    // Esc clears only the uncommitted input.
    harness.app.mdi.input = "G0 Z5".into();
    harness.frame(&[egui::Key::Escape]);
    assert!(harness.app.mdi.input.is_empty());
    assert_eq!(harness.app.mdi.len(), 1, "Esc never clears the buffer");
}

#[test]
fn enter_issues_a_block_and_a_recalled_line_is_overwritten_in_place() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);

    harness.app.mdi.input = "G0 X1".into();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(harness.mdi_sent(), vec!["G0 X1".to_string()]);
    assert_eq!(blocks(&harness.app), vec!["G0 X1".to_string()]);
    assert!(harness.app.mdi.input.is_empty(), "the input is clear again");

    harness.app.mdi.input = "G1 Z-1 F100".into();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(harness.mdi_sent().len(), 2);
    assert_eq!(harness.app.mdi.len(), 2, "a typed block appends");

    // Recall the first block, edit it, and Enter overwrites that line.
    harness.frame(&[egui::Key::ArrowUp]);
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.selection, Some(0));
    harness.app.mdi.input = "G0 X5".into();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        blocks(&harness.app),
        vec!["G0 X5".to_string(), "G1 Z-1 F100".to_string()]
    );
    assert_eq!(harness.mdi_sent().last().map(String::as_str), Some("G0 X5"));

    // A block that does not parse is not committed and nothing is sent.
    let sent = harness.mdi_sent().len();
    harness.app.mdi.input = "G1 X".into();
    harness.frame(&[egui::Key::Enter]);
    assert_eq!(harness.mdi_sent().len(), sent);
    assert_eq!(harness.app.mdi.len(), 2);
    assert!(harness.app.mdi.last_error.is_some(), "the strip says why");
    let _ = limit;
}

#[test]
fn up_and_down_recall_a_buffer_line_and_keep_the_selection_on_it() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X1".into());
    harness.app.mdi.commit(limit, "G1 X2".into());

    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.input, "G1 X2", "Up starts at the newest");
    assert_eq!(harness.app.mdi.selection, Some(1));
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.input, "G0 X1");
    assert_eq!(harness.app.mdi.selection, Some(0));

    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.mdi.input, "G1 X2");
    assert_eq!(harness.app.mdi.selection, Some(1));
    harness.frame(&[egui::Key::ArrowDown]);
    assert_eq!(harness.app.mdi.selection, None, "past the newest block");
    assert!(harness.app.mdi.input.is_empty());

    // Recall is in place: both lines stay in the listing.
    assert_eq!(harness.app.mdi.len(), 2);
    assert!(harness.mdi_sent().is_empty(), "recall issues nothing");
}

#[test]
fn ctrl_backspace_deletes_the_selected_block_and_otherwise_clears_the_input() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);

    // No buffer line selected: the chord clears the whole input, so the
    // text edit never got it (it would have deleted one word).
    harness.app.mdi.input = "G0 X1 Y2".into();
    harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
    assert!(harness.app.mdi.input.is_empty());
    assert!(harness.mdi_sent().is_empty());

    harness.app.mdi.commit(limit, "G0 X1".into());
    harness.app.mdi.commit(limit, "G1 X2".into());
    harness.frame(&[egui::Key::ArrowUp]);
    assert_eq!(harness.app.mdi.selection, Some(1));
    harness.frame_with(&[(egui::Key::Backspace, ctrl())]);
    assert_eq!(blocks(&harness.app), vec!["G0 X1".to_string()]);
    assert_eq!(
        harness.app.mdi.selection,
        Some(0),
        "the band stays on a line"
    );
    assert!(harness.mdi_sent().is_empty(), "deleting issues nothing");
}

#[test]
fn ctrl_r_issues_the_selected_block_with_no_dialog() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X3".into());
    harness.app.mdi.selection = Some(0);

    harness.frame_with(&[(egui::Key::R, ctrl())]);
    assert_eq!(harness.mdi_sent(), vec!["G0 X3".to_string()]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "Ctrl+R confirms nothing"
    );
    assert_eq!(harness.app.mdi.len(), 1, "the block is already buffered");
}

#[test]
fn a_blocked_run_puts_the_reason_in_the_strip_and_sends_nothing() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X3".into());
    harness.app.mdi.selection = Some(0);

    // Axes unhomed: the run-from-line gate, reported without a dialog.
    harness.app.snapshot.machine.homed.insert("X", false);
    harness.frame_with(&[(egui::Key::R, ctrl())]);
    assert!(harness.mdi_sent().is_empty(), "{:?}", harness.sent_debug());
    assert_eq!(
        harness.app.mdi.last_error.as_deref(),
        Some("home every axis first")
    );
    assert!(harness.app.overlay.modal.is_none());

    // The panel not in MDI blocks it the same way.
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X3".into());
    harness.app.mdi.selection = Some(0);
    screens::mdi::run_selected(&mut harness.app);
    assert!(harness.mdi_sent().is_empty());
    assert_eq!(
        harness.app.mdi.last_error.as_deref(),
        Some("panel mode is not MDI")
    );
}

#[test]
fn ctrl_g_selects_a_buffer_line_and_leaves_the_program_goto_alone() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X1".into());
    harness.app.mdi.commit(limit, "G1 X2".into());

    harness.frame_with(&[(egui::Key::G, ctrl())]);
    assert!(
        matches!(
            harness.app.overlay.modal,
            Some(Modal::GoToLine { mdi: true, .. })
        ),
        "Ctrl+G on the pane targets the buffer"
    );
    // Accepting closes the dialog first, the way the modal handler does.
    harness.app.overlay.modal = None;
    harness.app.go_to_line(99, true);
    assert_eq!(harness.app.mdi.selection, Some(1), "bounded by the buffer");
    assert_eq!(harness.app.mdi.input, "G1 X2");

    // The Program page keeps its own go-to-line.
    harness.app.set_page(Page::Program);
    harness.frame_with(&[(egui::Key::G, ctrl())]);
    assert!(matches!(
        harness.app.overlay.modal,
        Some(Modal::GoToLine { mdi: false, .. })
    ));
    harness.app.go_to_line(1, false);
    assert_eq!(harness.app.program.selected_line, 1);
    assert_eq!(
        harness.app.mdi.selection,
        Some(1),
        "the buffer selection is untouched"
    );
}

#[test]
fn ctrl_s_writes_the_buffer_through_mdi_file() {
    let mut harness = mdi_harness();
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X0 Y0".into());
    harness.app.mdi.commit(limit, "G1 Z-1 F100".into());
    let prefix = harness.app.profile.machine_ini.program_prefix.clone();

    harness.frame_with(&[(egui::Key::S, ctrl())]);
    assert!(
        matches!(harness.app.overlay.modal, Some(Modal::MdiSave(_))),
        "Ctrl+S opens the dialog"
    );
    // The wrapper starts ticked, so the saved program carries the
    // profile's preamble and postamble.
    harness
        .app
        .edit_mdi_save(|dialog| dialog.name = "part".into());
    harness.app.confirm_mdi_save();
    assert!(harness.app.overlay.modal.is_none());
    let wrapped = std::fs::read_to_string(prefix.join("part.ngc")).unwrap();
    let mut lines = wrapped.lines();
    assert_eq!(lines.next(), Some("G17"));
    assert_eq!(lines.last(), Some("M30"));
    assert!(wrapped.contains("G0 X0 Y0\nG1 Z-1 F100\n"));
    assert_eq!(harness.app.mdi.len(), 2, "the buffer is kept after a save");

    // Unticked: the blocks alone.
    harness.app.overlay.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
        name: "plain".into(),
        wrap: false,
        ..Default::default()
    }));
    harness.app.confirm_mdi_save();
    assert_eq!(
        std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
        "G0 X0 Y0\nG1 Z-1 F100\n"
    );

    // An existing name needs the confirm inside the dialog first.
    harness.app.mdi.commit(limit, "G0 Z5".into());
    harness.app.overlay.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
        name: "plain".into(),
        wrap: false,
        ..Default::default()
    }));
    harness.app.confirm_mdi_save();
    assert!(
        matches!(
            harness.app.overlay.modal,
            Some(Modal::MdiSave(crate::state::MdiSaveDialog {
                confirm_overwrite: true,
                ..
            }))
        ),
        "the same dialog asks for the overwrite"
    );
    assert_eq!(
        std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
        "G0 X0 Y0\nG1 Z-1 F100\n",
        "nothing is written before the confirm"
    );
    harness.app.confirm_mdi_save();
    assert!(harness.app.overlay.modal.is_none());
    assert_eq!(
        std::fs::read_to_string(prefix.join("plain.ngc")).unwrap(),
        "G0 X0 Y0\nG1 Z-1 F100\nG0 Z5\n"
    );
}

#[test]
fn protect_refuses_the_mdi_save_before_it_opens_the_dialog() {
    let mut harness = ProbeHarness::new(PanelMode::Mdi, true, true);
    harness.frame(&[]);
    let limit = history_limit(&harness.app);
    harness.app.mdi.commit(limit, "G0 X0".into());
    let prefix = harness.app.profile.machine_ini.program_prefix.clone();

    harness.frame_with(&[(egui::Key::S, ctrl())]);
    assert!(
        harness.app.overlay.modal.is_none(),
        "protect keeps the dialog shut"
    );
    assert!(!prefix.join("part.ngc").exists());

    // A protect that turns on while the dialog is open still refuses.
    harness.app.overlay.modal = Some(crate::state::Modal::MdiSave(crate::state::MdiSaveDialog {
        name: "part".into(),
        ..Default::default()
    }));
    harness.app.confirm_mdi_save();
    assert!(
        harness.app.overlay.modal.is_some(),
        "the dialog stays open with the reason"
    );
    assert!(!prefix.join("part.ngc").exists());
}

#[test]
fn the_parent_row_is_selectable_and_enter_opens_the_parent() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    // Keep the removable-media poll out of the test; the NC pane is enough.
    harness.app.files.usb_root = Some(harness.app.files.nc_root.clone());
    let root = harness.app.files.nc_root.clone();
    let part = root.join("part");
    std::fs::create_dir_all(&part).unwrap();
    std::fs::write(part.join("a.ngc"), "G0 X0\n").unwrap();
    harness.app.set_page(Page::Files);
    harness.app.files.nc_current = part.clone();
    harness.app.files.nc_entries = crate::state::read_dir_entries(&part);
    harness.app.files.nc_selected = 0;
    harness.app.files.nc_parent_selected = false;

    harness.frame(&[]);
    harness.frame(&[egui::Key::ArrowUp]);
    assert!(
        harness.app.files.nc_parent_selected,
        "Up from the first entry selects the .. row inside a directory"
    );
    assert_eq!(harness.app.files.nc_selected, 0);

    harness.frame(&[egui::Key::Enter]);
    assert_eq!(
        harness.app.files.nc_current, root,
        "Enter on the .. row opens the parent directory"
    );
    assert!(!harness.app.files.nc_parent_selected);
}

#[test]
fn nc_files_has_no_parent_row_at_its_root_by_default() {
    let mut harness = ProbeHarness::new(PanelMode::Auto, false, true);
    harness.files_page_with_one_file("part.ngc");
    assert!(
        !harness.app.files.allow_parent_above_root,
        "[files] show_parent_above_root is off in base.toml"
    );
    harness.frame(&[]);
    harness.frame(&[egui::Key::ArrowUp]);
    assert!(
        !harness.app.files.nc_parent_selected,
        "NC_FILES stops at PROGRAM_PREFIX without the profile option"
    );
    assert_eq!(harness.app.files.nc_current, harness.app.files.nc_root);
}

#[test]
fn history_limit_drops_the_oldest_block() {
    let mut mdi = MdiUi::new();
    mdi.commit(2, "G0 X1".into());
    mdi.commit(2, "G1 X2".into());
    mdi.commit(2, "G2 X3".into());
    let texts: Vec<String> = mdi.blocks.iter().map(|(block, _)| block.clone()).collect();
    assert_eq!(texts, vec!["G1 X2".to_string(), "G2 X3".to_string()]);

    // Committing over a selected line rewrites it instead of growing.
    mdi.selection = Some(1);
    mdi.commit(2, "G3 X4".into());
    assert_eq!(mdi.len(), 2);
    assert_eq!(mdi.blocks[1].0, "G3 X4");
}
