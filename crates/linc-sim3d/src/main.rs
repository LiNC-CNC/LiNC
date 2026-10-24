//! The linc-sim3d command line.
//!
//! Three modes, one binary:
//!
//! - no flag opens the 3D window on the model, driven by the panel's jog
//!   controls: the machine, the workpieces, the probe at the live tip.
//! - --hal publishes the model on the linc-sim3d HAL component and samples it,
//!   which is what hal/linc-sim3d.hal starts with loadusr. On a host with a
//!   display it opens the 3D window as well, so one process owns the component
//!   and the view; closing the window keeps sampling.
//! - --selftest runs headless with no HAL and checks the model against a
//!   synthetic box, printing one PASS/FAIL line per check.
//!
//! --config defaults to configs/sim3d.toml, resolved from the working
//! directory. Relative paths inside the config resolve against its own
//! directory, not against the working directory.
//!
//! Whether a --hal rig shows the window is decided once, by
//! [linc_sim3d::viewer::window_mode]: --window forces it, --no-window forbids
//! it, LINC_SIM3D_WINDOW=0/1 does the same from the environment, and otherwise
//! a DISPLAY or WAYLAND_DISPLAY means yes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;

use linc_sim3d::config::Sim3dConfig;
use linc_sim3d::engine::{self, MockPositionSource, PositionSource, Positions};
use linc_sim3d::hal::Rig;
use linc_sim3d::model::{Action, CollisionTarget, SharedState, SimModel, Snapshot};
use linc_sim3d::sink::{MockSignal, Sim3dSignal};
use linc_sim3d::types::Transform;
use linc_sim3d::ui::{Panel, SourceKind};
use linc_sim3d::viewer::{self, Live, Scene, Viewer, WindowFlag, WindowMode};

/// The 20 x 20 x 10 mm box the self test probes, imported through the real
/// STEP path so the test covers the importer too.
const SYNTHETIC_BOX_STEP: &str = include_str!("../tests/fixtures/box.step");

#[derive(Debug, Parser)]
#[command(
    name = "linc-sim3d",
    version,
    about = "A 3D machine simulator for testing touch probing (no material removal)"
)]
struct Args {
    /// Simulator config; paths inside it resolve against its own directory.
    #[arg(long, value_name = "PATH", default_value_os_t = Sim3dConfig::default_path())]
    config: PathBuf,

    /// Publish the linc-sim3d HAL component and sample it. Opens the 3D window
    /// too when this host has a display.
    #[arg(long, conflicts_with = "selftest")]
    hal: bool,

    /// Headless: probe a synthetic box and print one PASS/FAIL line per check.
    #[arg(long)]
    selftest: bool,

    /// With --hal: open the 3D window even when no display was detected; a
    /// window that cannot open is then an error, not a silent fallback.
    #[arg(long, conflicts_with_all = ["no_window", "selftest"])]
    window: bool,

    /// With --hal: never open a window, whatever the environment says.
    #[arg(long, requires = "hal", conflicts_with = "selftest")]
    no_window: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if args.hal {
        run_hal_mode(&args)
    } else if args.selftest {
        run_selftest(&args.config)
    } else {
        match run_window(&args.config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("linc-sim3d: {error:#}");
                ExitCode::FAILURE
            }
        }
    }
}

// ------------------------------------------------------------------- --hal

/// What the command line asked for, as the window policy sees it.
fn window_flag(args: &Args) -> WindowFlag {
    if args.no_window {
        WindowFlag::Off
    } else if args.window {
        WindowFlag::Force
    } else {
        WindowFlag::Unset
    }
}

/// The --hal mode: one process owns the HAL component and, when it can, the
/// window on the same state.
///
/// The component and the sampling thread come from [Rig]; the window only reads
/// the snapshot. Closing the window therefore cannot drop probe-out: the engine
/// keeps writing it until SIGINT or SIGTERM.
fn run_hal_mode(args: &Args) -> ExitCode {
    let mode = viewer::window_mode(
        window_flag(args),
        std::env::var("LINC_SIM3D_WINDOW").ok().as_deref(),
        viewer::display_available(),
    );

    let mut rig = match Rig::start(&args.config) {
        Ok(rig) => rig,
        Err(error) => {
            // One line, no panic, no window: this runs under loadusr, where the
            // HAL startup fails and LinuxCNC reads stderr.
            eprintln!(
                "linc-sim3d: cannot run the HAL component (needs a running LinuxCNC HAL, \
                 liblinuxcnchal): {error:#}"
            );
            return ExitCode::FAILURE;
        }
    };

    if mode.wants_window() {
        match open_rig_window(&rig, &args.config) {
            Ok(()) => {
                if rig.stop_requested() {
                    // SIGINT/SIGTERM closed the window: fall through to the
                    // normal shutdown instead of waiting for a second signal.
                    log::info!("linc-sim3d: stop signal with the window open; shutting down");
                } else {
                    eprintln!(
                        "linc-sim3d: window closed; still sampling the {} HAL component until SIGINT/SIGTERM",
                        rig.name()
                    );
                }
            }
            Err(error) => {
                if mode == WindowMode::Required {
                    eprintln!("linc-sim3d: cannot open the window: {error:#}");
                    rig.stop();
                    return ExitCode::FAILURE;
                }
                eprintln!("linc-sim3d: no window on this host ({error:#}); sampling headless");
            }
        }
    }

    rig.wait_for_stop();
    rig.stop();
    ExitCode::SUCCESS
}

/// The window over a live rig: it reads the snapshot the engine publishes and
/// never touches the component.
fn open_rig_window(rig: &Rig, config_path: &Path) -> anyhow::Result<()> {
    if !rig.shared().snapshot().faults.is_empty() {
        log::warn!("linc-sim3d: the model carries faults; they are listed in the panel");
    }
    let shared = Arc::clone(rig.shared());
    let scene = Scene::build(&shared.read_model());
    let panel = Panel::new(
        rig.config().clone(),
        config_path.to_path_buf(),
        SourceKind::Hal,
        MockPositionSource::new(),
    );
    present(
        window_title(&shared),
        Window {
            shared,
            panel,
            viewer: Viewer::new(scene),
            stop: Some(rig.stop_flag()),
        },
    )
}

// ---------------------------------------------------------------- --window

fn run_window(config_path: &Path) -> anyhow::Result<()> {
    let config = Sim3dConfig::load(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let model = SimModel::from_config(&config)
        .with_context(|| format!("building the model from {}", config_path.display()))?;
    let shared = Arc::new(SharedState::new(model));

    // The standalone window drives the machine itself: the jog source is the
    // engine's position source, and nothing here owns a HAL component.
    let jog = MockPositionSource::new();
    let panel = Panel::new(
        config.clone(),
        config_path.to_path_buf(),
        SourceKind::Ui,
        jog.clone(),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let interval = Duration::from_secs_f64((config.hal.sample_ms / 1000.0).max(0.0005));
    let engine_shared = Arc::clone(&shared);
    let engine_stop = Arc::clone(&stop);
    let position_source: Box<dyn PositionSource> = Box::new(jog);
    let signal: Box<dyn Sim3dSignal> = Box::new(MockSignal::new());
    let engine_thread = std::thread::Builder::new()
        .name("linc-sim3d-engine".to_string())
        .spawn(move || {
            engine::run_engine(
                engine_shared,
                position_source,
                signal,
                interval,
                engine_stop,
            );
        })
        .context("starting the sampling thread")?;

    let scene = Scene::build(&shared.read_model());
    let title = window_title(&shared);
    let result = present(
        title,
        Window {
            shared: Arc::clone(&shared),
            panel,
            viewer: Viewer::new(scene),
            stop: None,
        },
    );

    stop.store(true, Ordering::Relaxed);
    let _ = engine_thread.join();
    result
}

fn window_title(shared: &SharedState) -> String {
    format!("linc-sim3d - {}", shared.read_model().machine().name.trim())
}

/// Run the window until it is closed.
///
/// One place for the eframe setup, so both modes present the same window and a
/// failure to open one surfaces the same way.
fn present(title: String, window: Window) -> anyhow::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([900.0, 600.0])
        .with_title(title.clone());
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(&title, options, Box::new(move |_cc| Ok(Box::new(window))))
        .map_err(|error| anyhow::anyhow!("window: {error}"))
}

/// The window: the side panel owns the model changes, the view draws them.
struct Window {
    shared: Arc<SharedState>,
    panel: Panel,
    viewer: Viewer,
    /// Set for a --hal rig: SIGINT/SIGTERM closes the window, so the
    /// process shuts down instead of waiting for a click.
    stop: Option<Arc<AtomicBool>>,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Esc closes the view, and so does a SIGINT/SIGTERM: eframe returns from
        // run_native either way, which is the same path a window manager's close
        // button takes. After a close the --hal rig keeps sampling; after a stop
        // signal it shuts down. The frame loop already repaints often enough for
        // that to land well inside a second.
        let stop_requested = self
            .stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Relaxed));
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if viewer::close_request(stop_requested, escape).is_some() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let mut model_changed = false;
        egui::containers::Panel::right("linc-sim3d-panel")
            .resizable(true)
            .default_size(380.0)
            .size_range(egui::Rangef::new(300.0, 620.0))
            .show(ui, |ui| {
                model_changed = self.panel.ui(ui, &self.shared, &mut self.viewer);
            });
        if model_changed {
            let scene = Scene::build(&self.shared.read_model());
            self.viewer.set_scene(scene);
        }

        let snapshot = self.shared.snapshot();
        let tool = self
            .shared
            .read_model()
            .active_tool(snapshot.tool_number, 0.0);
        let marker = if snapshot.probe_touching {
            snapshot.probe_point
        } else {
            snapshot.collision_point
        };
        let live = Live {
            tip: snapshot.tip,
            tool,
            colliding: snapshot.collision,
            marker,
        };
        egui::containers::CentralPanel::default().show(ui, |ui| {
            self.viewer.show(ui, &live);
        });
        // The snapshot changes under the view, so redraw even without input.
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
}

// ------------------------------------------------------------- --selftest

fn run_selftest(config_path: &Path) -> ExitCode {
    match selftest(config_path) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(failures) => {
            println!("FAILED {failures} check(s)");
            ExitCode::FAILURE
        }
        Err(error) => {
            println!("FAIL  setup: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// One report line per check, counting the failures.
struct Report {
    failures: usize,
    checks: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: 0,
            checks: 0,
        }
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        self.checks += 1;
        if !ok {
            self.failures += 1;
        }
        println!(
            "{}  {}  ({})",
            if ok { "PASS" } else { "FAIL" },
            name,
            detail.as_ref()
        );
    }
}

fn selftest(config_path: &Path) -> anyhow::Result<usize> {
    let config = Sim3dConfig::load(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut model = SimModel::from_config(&config)
        .with_context(|| format!("building the model from {}", config_path.display()))?;
    let probe_tool = config.probe.tool_number;
    let probe_radius = model.probe().tip_radius;
    let travel = model.machine().travel;
    let cutter_tool = first_cutter(&model, probe_tool);

    // The synthetic box, through the real importer. Without a STEP backend the
    // first configured workpiece stands in; with neither there is nothing to
    // probe and the test says so instead of pretending.
    let box_path = synthetic_box()?;
    let index = match model.apply(Action::AddWorkpiece {
        path: box_path.clone(),
        transform: Transform::identity(),
    }) {
        Ok(()) => model.workpieces().len() - 1,
        Err(error) => {
            if model.workpieces().is_empty() {
                anyhow::bail!(
                    "cannot import {} ({error}); no configured workpiece to fall back on",
                    box_path.display()
                );
            }
            println!(
                "note  cannot import {}: {error}; using the first configured workpiece",
                box_path.display()
            );
            0
        }
    };

    // Centre the part in travel and lift it clear of the table, so the probe
    // starts above it and the machine envelope is never the reason for a hit.
    let raw = model.workpieces()[index].raw.bounds;
    let desired = [
        (travel.min[0] + travel.max[0]) * 0.5,
        (travel.min[1] + travel.max[1]) * 0.5,
        travel.min[2] + 30.0 + raw.size()[2] * 0.5,
    ];
    let center = raw.center();
    let transform = Transform::identity().with_translation([
        desired[0] - center[0],
        desired[1] - center[1],
        desired[2] - center[2],
    ]);
    model
        .apply(Action::SetTransform { index, transform })
        .with_context(|| format!("placing workpiece {index} for the self test"))?;

    let placed = model.workpieces()[index].mesh.bounds;
    let top = placed.max[2];
    let spot = [
        (placed.min[0] + placed.max[0]) * 0.5,
        (placed.min[1] + placed.max[1]) * 0.5,
    ];
    let expected_z = top + probe_radius;
    let step = 0.5;
    let sweep_start = expected_z + 6.0;
    println!(
        "box  {} triangles, x {:.1}..{:.1}, y {:.1}..{:.1}, z {:.1}..{:.1} mm",
        model.workpieces()[index].mesh.triangle_count(),
        placed.min[0],
        placed.max[0],
        placed.min[1],
        placed.max[1],
        placed.min[2],
        placed.max[2],
    );
    println!(
        "probe T{} ball radius {:.3} mm (offset {:.3} mm), cutter T{} radius {:.3} mm \
         (offset {:.3} mm), expected trigger Z {:.3} mm",
        probe_tool,
        probe_radius,
        model.active_tool(probe_tool, 0.0).z_offset,
        cutter_tool,
        model.active_tool(cutter_tool, 0.0).tip_radius,
        model.active_tool(cutter_tool, 0.0).z_offset,
        expected_z,
    );

    // The engine builds the tip as z + the active tool's z offset, so the
    // sweep is written in tip space and converted back here.
    let probe_offset = model.active_tool(probe_tool, 0.0).z_offset;
    let cutter_offset = model.active_tool(cutter_tool, 0.0).z_offset;
    let shared = SharedState::new(model);
    let signal = MockSignal::new();
    let probe_at = |tip_z: f64| Positions {
        x: spot[0],
        y: spot[1],
        z: tip_z - probe_offset,
        tool_offset_z: 0.0,
        tool_number: probe_tool,
    };
    let cutter_at = |tip_z: f64| Positions {
        x: spot[0],
        y: spot[1],
        z: tip_z - cutter_offset,
        tool_offset_z: 0.0,
        tool_number: cutter_tool,
    };

    let mut report = Report::new();
    let start = engine::step_once(&shared, &probe_at(sweep_start), &signal);
    report.check(
        "1 no contact at the start",
        !start.probe_touching,
        format!("tip z {:.3} mm, ball clear of the part", sweep_start),
    );

    let mut contact_z: Option<f64> = None;
    let mut contact_snapshot: Option<Snapshot> = None;
    let mut z = sweep_start;
    while z >= expected_z - 2.0 {
        let snapshot = engine::step_once(&shared, &probe_at(z), &signal);
        if snapshot.probe_touching && contact_z.is_none() {
            contact_z = Some(z);
            contact_snapshot = Some(snapshot);
        }
        z -= step;
    }
    let measured = contact_z;
    println!(
        "measured trigger Z {}",
        match measured {
            Some(value) => format!("{value:.3} mm"),
            None => "never triggered".to_string(),
        }
    );
    report.check(
        "2 contact starts within one sample of the expected Z",
        measured.is_some_and(|value| (value - expected_z).abs() <= step + 1e-9),
        format!("expected {expected_z:.3} mm, sample step {step:.3} mm"),
    );

    let retract = engine::step_once(&shared, &probe_at(sweep_start), &signal);
    report.check(
        "3 contact clears on retract",
        contact_snapshot.is_some() && !retract.probe_touching,
        format!("back at tip z {sweep_start:.3} mm"),
    );

    let same = measured.unwrap_or(expected_z);
    let cutter = engine::step_once(&shared, &cutter_at(same), &signal);
    report.check(
        "4a a cutter at the contact position reports collision",
        cutter.collision,
        format!(
            "cutter T{} at tip z {same:.3} mm: {}",
            cutter_tool,
            linc_sim3d::ui::target_label(cutter.collision_target)
        ),
    );
    let probe = engine::step_once(&shared, &probe_at(same), &signal);
    report.check(
        "4b the probe at the same position reports no collision",
        !probe.collision,
        "probing is never reported as a crash",
    );

    let outside = Positions {
        x: travel.max[0] + 20.0,
        y: spot[1],
        z: same - cutter_offset,
        tool_offset_z: 0.0,
        tool_number: cutter_tool,
    };
    let envelope = engine::step_once(&shared, &outside, &signal);
    report.check(
        "5 the machine envelope fires outside travel",
        envelope.collision && envelope.collision_target == Some(CollisionTarget::Machine),
        format!(
            "tip x {:.1} mm is past the travel maximum {:.1} mm",
            outside.x, travel.max[0]
        ),
    );

    let faults = shared.read_model().faults().to_vec();
    if !faults.is_empty() {
        println!("note  model faults: {}", faults.join("; "));
    }
    Ok(report.failures)
}

/// The first tool that is a cutter with a real diameter, so the collision check
/// has something to swing.
fn first_cutter(model: &SimModel, probe_tool: u16) -> u16 {
    for tool in 1..=120u16 {
        if tool == probe_tool {
            continue;
        }
        let active = model.active_tool(tool, 0.0);
        if !active.is_probe() && active.tip_radius > 0.0 {
            return tool;
        }
    }
    probe_tool
}

/// Write the embedded box STEP to a stable temporary path.
///
/// A stable path lets the importer's disk cache hit on later runs; the content
/// is compared first so a changed fixture is rewritten.
fn synthetic_box() -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join("linc-sim3d-selftest");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("box.step");
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref() != Some(SYNTHETIC_BOX_STEP) {
        std::fs::write(&path, SYNTHETIC_BOX_STEP)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}
