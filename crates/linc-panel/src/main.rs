//! `linc-panel` — LiNC-owned CNC control panel service.
//!
//! Owns the serial port (or simulation window) and creates `cnc_panel.*` HAL
//! pins. The HMI is a separate process; an HMI crash cannot remove panel
//! control.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use linc_core::config::Profile;
use linc_core::model::{PanelMode, PanelSnapshot};
use linc_core::sim_ipc::SimPanelIpc;

#[derive(Parser, Debug)]
#[command(name = "linc-panel", version, about = "LiNC CNC control panel service")]
struct Args {
    /// Machine profile. Defaults to LINC_CONFIG.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Force the simulation driver.
    #[arg(long)]
    sim: bool,
    /// Show the simulator panel window (simulation only).
    #[arg(long)]
    window: bool,
    /// Never show the simulator panel window.
    #[arg(long)]
    no_window: bool,
    /// Show the simulation-only PROBE CONTACT debug key in the panel window.
    #[arg(long)]
    probe_key: bool,
    /// Do not create the cnc_panel HAL component (development only).
    #[arg(long)]
    no_hal: bool,
    /// Do not create the diagnostic socket for linc-mcp.
    #[arg(long)]
    no_diag: bool,
    /// Let linc-mcp drive this simulator through the diagnostic socket. The
    /// panel and its E-stop stay authoritative; a real panel ignores this.
    #[arg(long)]
    allow_sim_control: bool,
    /// Do not exchange standalone simulation state files; use only HAL.
    #[arg(long)]
    no_ipc: bool,
    /// Run exactly one cycle and exit.
    #[arg(long)]
    once: bool,
}

/// Install logging. `RUST_LOG` wins; without it LiNC's own crates log at info
/// and everything else at warn. The D-Bus stack (zbus) is held at warn even
/// when `RUST_LOG` says info, because at info it logs every message and every
/// property-cache refresh; a directive that names the noisy target still wins.
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "warn,linc_app=info,linc_core=info,linc_ui=info,linc_view=info,\
         linc_hal=info,linc_panel=info,linc_linuxcnc=info",
    ));
    for target in ["zbus", "zbus_macros", "zbus_xdg", "udisks2", "tracing"] {
        builder.filter_module(target, log::LevelFilter::Warn);
    }
    builder.init();
}

/// `--probe-key` also arrives from the environment.
///
/// The simulation starts this service from HAL (loadusr -Wn cnc_panel
/// linc-panel --no-ipc), and a loadusr line cannot pass a flag, so the
/// launcher exports LINC_PROBE_KEY instead (scripts/start-sim.sh does).
/// The flag still wins when both are given.
fn probe_key_requested(flag: bool) -> bool {
    resolve_probe_key(flag, std::env::var("LINC_PROBE_KEY").ok().as_deref())
}

/// The flag wins; the environment can only ask for the key, never take it
/// away. Split out from the process environment so both halves are testable.
fn resolve_probe_key(flag: bool, env: Option<&str>) -> bool {
    flag || truthy(env)
}

/// A launcher's boolean environment value. Anything but an explicit yes reads
/// as no, so a typo leaves the debug key hidden rather than in the way.
fn truthy(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|value| {
        ["1", "true", "yes", "on"]
            .iter()
            .any(|word| word.eq_ignore_ascii_case(value))
    })
}

fn main() -> Result<()> {
    init_logging();
    let mut args = Args::parse();
    args.probe_key = probe_key_requested(args.probe_key);
    let config_path = args
        .config
        .or_else(|| Profile::load_path().ok())
        .context("no profile: use --config or LINC_CONFIG")?;
    let profile = Profile::load(&config_path, None)
        .with_context(|| format!("cannot load profile {}", config_path.display()))?;
    if let Ok(path) = std::env::var("LINC_PANEL_PID_FILE") {
        if !path.is_empty() {
            let _ = std::fs::write(path, format!("{}\n", std::process::id()));
        }
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone());
    if args.probe_key && !args.sim && profile.config.panel.kind != "simulated" {
        log::warn!("the PROBE CONTACT key is ignored: this panel is not a simulator");
    }
    let simulated = args.sim || profile.config.panel.kind == "simulated";
    let cycle = Duration::from_millis(
        profile
            .config
            .panel
            .link
            .cycle_time_ms
            .unwrap_or(if simulated { 20 } else { 50 })
            .max(if simulated { 10 } else { 20 }),
    );

    log::info!(
        "linc-panel: profile={} driver={} simulated={} cycle={:?}",
        profile.config.profile,
        profile.config.panel.driver,
        simulated,
        cycle
    );

    // The simulator handle exists before the diagnostic socket, because the
    // socket's control surface is a view of this one state. The window and the
    // panel loop share it, so a tool's change is an operator's change.
    let sim_handle = if simulated {
        Some(linc_panel::sim::SimPanelHandle::new())
    } else {
        None
    };
    if args.allow_sim_control && !simulated {
        log::warn!("--allow-sim-control ignored: this panel is not a simulator");
    }
    let sim_control: Option<Arc<dyn linc_diag::SimControl>> =
        match (&sim_handle, args.allow_sim_control, simulated) {
            (Some(handle), true, true) => {
                log::warn!("simulator control armed for linc-mcp (--allow-sim-control)");
                Some(Arc::new(linc_panel::sim::SimControlState::new(
                    handle.clone(),
                )))
            }
            _ => None,
        };

    // Panel diagnostic socket. The panel owns its own state, so it publishes
    // its own socket instead of routing through the HMI. A failure here never
    // stops the panel loop.
    let panel_bus = Arc::new(Mutex::new(linc_diag::PanelBus::new()));
    let _panel_diag = if args.no_diag {
        None
    } else {
        let process = linc_core::diag::ProcessInfo {
            pid: std::process::id(),
            role: "panel".into(),
            build: linc_core::diag::BuildInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                git_commit: option_env!("LINC_GIT_COMMIT").unwrap_or("").into(),
                git_dirty: option_env!("LINC_GIT_DIRTY").is_some(),
                profile: profile.config.machine.name.clone(),
                backend: if simulated {
                    "sim".into()
                } else {
                    "serial".into()
                },
            },
        };
        linc_diag::crash::install_hook("panel", process.clone());
        let source = Arc::new(linc_diag::PanelSource::new(
            panel_bus.clone(),
            process,
            sim_control,
        ));
        match linc_diag::serve(linc_diag::discovery::runtime_dir(), "panel", source) {
            Ok(handle) => {
                log::info!("panel diagnostic socket at {}", handle.socket.display());
                Some(handle)
            }
            Err(error) => {
                log::warn!("panel diagnostic socket unavailable: {error}");
                None
            }
        }
    };

    if simulated {
        run_simulated(
            profile,
            cycle,
            args.window,
            args.no_window,
            args.probe_key,
            args.no_hal,
            args.no_ipc,
            args.once,
            shutdown,
            panel_bus,
            sim_handle.context("simulator handle")?,
        )
    } else {
        run_physical(profile, cycle, args.no_hal, args.once, shutdown, panel_bus)
    }
}

fn run_physical(
    profile: Profile,
    cycle: Duration,
    no_hal: bool,
    once: bool,
    shutdown: Arc<AtomicBool>,
    panel_bus: Arc<Mutex<linc_diag::PanelBus>>,
) -> Result<()> {
    let port = profile
        .config
        .panel
        .link
        .port
        .clone()
        .unwrap_or_else(|| "/dev/ttyUSB0".into());
    let baud = profile.config.panel.link.baud.unwrap_or(115200);
    let mut driver = linc_panel::CncPanelDriver::new(port, baud, cycle);
    driver.set_frame_tap(panel_bus.clone());
    run_loop(
        &mut driver,
        cycle,
        no_hal,
        once,
        shutdown,
        None,
        Some(panel_bus),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_simulated(
    profile: Profile,
    cycle: Duration,
    window: bool,
    no_window: bool,
    probe_key: bool,
    no_hal: bool,
    no_ipc: bool,
    once: bool,
    shutdown: Arc<AtomicBool>,
    panel_bus: Arc<Mutex<linc_diag::PanelBus>>,
    handle: linc_panel::sim::SimPanelHandle,
) -> Result<()> {
    let ipc = if no_ipc {
        None
    } else {
        Some(SimPanelIpc::from_env())
    };
    // The simulator starts in the state a real cabinet is found in before the
    // operator powers it up: E-stop latched and power off, panel connected.
    // Release the mushroom and press POWER ON to bring the machine up.
    handle.set(|state| {
        state.mode = PanelMode::Auto;
        state.connected = true;
        state.power = false;
        state.power_on_pulse = false;
        state.power_off_pulse = false;
        state.estop = true;
        state.sseg_status = 54;
        state.last_cycle_ms = cycle.as_secs_f64() * 1000.0;
    });
    // A launcher can start the cabinet further along the power-up sequence than
    // a real one is found in: `LINC_SIM_READY=estop` releases the mushroom and
    // `power` also presses POWER ON. The pulses are the ones the window
    // buttons raise, so HAL sees an operator's press.
    let start = linc_panel::sim::SimStart::from_env();
    if start != linc_panel::sim::SimStart::Cold {
        log::info!("simulated panel starts ready: {start:?} (LINC_SIM_READY)");
        handle.set(|state| state.apply_start(start));
    }
    let mut driver = linc_panel::SimPanelDriver::new(handle.clone(), cycle);
    let show_window =
        !no_window && (window || profile.config.simulation.panel_window.unwrap_or(false));
    if show_window {
        let title = profile
            .config
            .simulation
            .panel_title
            .clone()
            .unwrap_or_else(|| "CNC Control Panel Simulator".into());
        let panel_scale = profile.config.simulation.panel_scale.unwrap_or(1.0) as f32;
        // The profile chooses the face: gui = "lathe" / panel.driver =
        // "lathe-sim" draws the lathe panel, everything else the mill panel.
        let face = linc_panel::sim::PanelFace::for_profile(
            &profile.config.machine.gui,
            &profile.config.panel.driver,
            profile
                .config
                .lathe
                .as_ref()
                .and_then(|lathe| lathe.sub_spindle)
                .unwrap_or(false),
        );
        let loop_handle = handle.clone();
        let loop_shutdown = shutdown.clone();
        let loop_ipc = ipc.clone();
        let loop_bus = panel_bus.clone();
        let run_thread = thread::spawn(move || {
            let mut driver = linc_panel::SimPanelDriver::new(loop_handle, cycle);
            if let Err(error) = run_loop(
                &mut driver,
                cycle,
                no_hal,
                false,
                loop_shutdown,
                loop_ipc,
                Some(loop_bus),
            ) {
                log::error!("simulator panel loop stopped: {error:#}");
            }
        });
        let window_result = linc_panel::sim::simulator_window(
            handle,
            &title,
            panel_scale,
            probe_key,
            !no_hal,
            face,
            shutdown.clone(),
        );
        shutdown.store(true, Ordering::Relaxed);
        let _ = run_thread.join();
        window_result.map_err(|error| anyhow::anyhow!("simulator window: {error}"))?;
        Ok(())
    } else {
        run_loop(
            &mut driver,
            cycle,
            no_hal,
            once,
            shutdown,
            ipc,
            Some(panel_bus),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    driver: &mut dyn linc_panel::PanelDriver,
    cycle: Duration,
    no_hal: bool,
    once: bool,
    shutdown: Arc<AtomicBool>,
    mut ipc: Option<SimPanelIpc>,
    panel_bus: Option<Arc<Mutex<linc_diag::PanelBus>>>,
) -> Result<()> {
    let mut hal = if no_hal {
        None
    } else {
        match linc_panel::CncPanelHal::new() {
            Ok(hal) => Some(hal),
            Err(error) => {
                log::warn!("cnc_panel HAL unavailable: {error}");
                None
            }
        }
    };
    let mut last_snapshot = PanelSnapshot::default();
    let mut last_connection: Option<bool> = None;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let snapshot = driver.poll().unwrap_or_else(|error| {
            log::warn!("panel poll failed: {error}");
            PanelSnapshot::default()
        });
        if let Some(bus) = panel_bus.as_ref() {
            if let Ok(mut bus) = bus.lock() {
                bus.publish(snapshot.clone());
            }
        }
        if let Some(ipc) = ipc.as_ref() {
            if let Err(error) = ipc.publish(&snapshot) {
                log::warn!("cannot publish simulation panel state: {error}");
            }
        }

        let mut display = hal
            .as_ref()
            .map(linc_panel::CncPanelHal::read_display)
            .unwrap_or_else(|| linc_panel::PanelDisplay {
                leds: snapshot.leds,
                sseg_tool: snapshot.sseg_tool,
                sseg_next_tool: snapshot.sseg_next_tool,
                sseg_status: snapshot.sseg_status,
                vel: [0.0; 3],
                homing: false,
            });
        if let Some(ipc) = ipc.as_mut() {
            if let Some(from_hmi) = ipc.read_display() {
                display = from_hmi;
            }
        }
        if hal.is_some() {
            driver.observe_display(&display);
        }
        let outputs = linc_panel::PanelOutputs {
            buttons: snapshot.buttons,
            mode: snapshot.mode.clone(),
            feed_override: snapshot.feed_override,
            rapid_override: snapshot.rapid_override,
            spindle_override: snapshot.spindle_override,
            program_protect: snapshot.program_protect,
            estop: snapshot.estop,
            probe: snapshot.probe,
            home: snapshot.home,
            proghome: snapshot.proghome,
            step: snapshot.step,
            power_on: snapshot.power_on,
            power_on_pulse: snapshot.power_on_pulse,
            power_off_pulse: snapshot.power_off_pulse,
            estop_reset_pulse: snapshot.estop_reset_pulse,
            display,
        };
        if let Some(hal) = hal.as_mut() {
            hal.apply_outputs(&outputs);
            hal.set_connected(snapshot.connected, snapshot.communication_error);
        }
        if let Err(error) = driver.write(&outputs) {
            log::warn!("panel write failed: {error}");
        }
        if last_connection != Some(snapshot.connected) {
            if snapshot.connected {
                log::info!("panel online");
            } else {
                log::warn!("panel offline");
            }
            last_connection = Some(snapshot.connected);
        }
        last_snapshot = snapshot;
        if once {
            break;
        }
        thread::sleep(cycle);
    }
    log::info!(
        "panel service stopped (last mode {}, connected={})",
        last_snapshot.mode.label(),
        last_snapshot.connected
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A launcher reads as yes only when it says so: an unset or misspelled
    /// value leaves the debug key hidden rather than in the way.
    #[test]
    fn the_probe_key_environment_value_is_read_conservatively() {
        for yes in ["1", "true", "yes", "on", " TRUE ", "Yes"] {
            assert!(truthy(Some(yes)), "{yes:?}");
        }
        for no in ["", "0", "false", "off", "no", "probe", "2"] {
            assert!(!truthy(Some(no)), "{no:?}");
        }
        assert!(!truthy(None), "unset");
    }

    /// The flag alone still works, the environment can ask for the key, and
    /// neither can take it away from the other.
    #[test]
    fn the_flag_and_the_environment_both_ask_for_the_probe_key() {
        assert!(resolve_probe_key(true, None), "flag only");
        assert!(resolve_probe_key(true, Some("0")), "flag beats a zero");
        assert!(resolve_probe_key(false, Some("1")), "environment only");
        assert!(!resolve_probe_key(false, None), "neither");
    }
}
