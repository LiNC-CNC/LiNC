//! LiNC HMI entry point.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use linc_core::backend::{BackendHandle, HalWriter, MachineBackend};
use linc_core::config::Profile;
use linc_core::model::PanelMode;
use linc_hal::{HalPinValue, HalReader, LincHal};
use linc_linuxcnc::NmlBackend;
use linc_ui::{LincApp, UiBackend};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    Auto,
    Sim,
    Linuxcnc,
}

#[derive(Parser, Debug)]
#[command(name = "linc", version, about = "LiNC — LinuxCNC HMI")]
struct Args {
    /// Machine profile. Defaults to LINC_CONFIG.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Parse, merge and validate the profile without connecting.
    #[arg(long)]
    check_config: bool,
    /// LinuxCNC INI override, as passed by LinuxCNC's `-ini`.
    #[arg(long = "ini")]
    ini: Option<PathBuf>,
    /// Backend transport. Auto uses the simulation backend for sim profiles.
    #[arg(long, value_enum, default_value_t = BackendChoice::Auto)]
    backend: BackendChoice,
    /// Force the self-contained simulation backend.
    #[arg(long)]
    sim: bool,
    /// Do not create the linc.* HAL component (development).
    #[arg(long)]
    no_hal: bool,
    /// Developer/demo: start the simulated panel in this mode.
    #[arg(long, env = "LINC_DEMO_MODE")]
    demo_mode: Option<String>,
    /// Start fullscreen/kiosk.
    #[arg(long)]
    fullscreen: bool,
    /// Do not create the diagnostic socket for linc-mcp.
    #[arg(long)]
    no_diag: bool,
}

/// Install logging. `RUST_LOG` wins; without it LiNC's own crates log at info
/// and everything else at warn.
///
/// The D-Bus stack (zbus, reached through the UDisks2 USB support) is held at
/// warn even when `RUST_LOG` says info: at info it logs every message, every
/// socket read and every property-cache refresh — thousands of records per
/// start, which buries LiNC's own lines. A directive that names the noisy
/// target itself, e.g. `RUST_LOG=zbus::connection=trace`, still wins because
/// the most specific target matches.
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

fn window_icon() -> Option<egui::IconData> {
    const ICON: &[u8] = include_bytes!("../../../assets/branding/linc-icon-256.png");
    let image = image::load_from_memory(ICON).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn main() -> Result<()> {
    init_logging();
    let args = parse_args();
    let config_path = args
        .config
        .clone()
        .or_else(|| Profile::load_path().ok())
        .context("no profile supplied: use --config <profile.toml> or LINC_CONFIG")?;
    let profile = Profile::load(&config_path, args.ini.clone())
        .with_context(|| format!("cannot load profile {}", config_path.display()))?;

    if args.check_config {
        println!("LiNC configuration OK");
        for (key, value) in profile.summary() {
            println!("{key:16} {value}");
        }
        return Ok(());
    }

    let backend_choice = if args.sim {
        BackendChoice::Sim
    } else {
        args.backend
    };
    let use_linuxcnc = match backend_choice {
        BackendChoice::Linuxcnc => true,
        BackendChoice::Sim => false,
        BackendChoice::Auto => !profile.is_sim(),
    };
    let demo_mode = parse_demo_mode(args.demo_mode.as_deref()).unwrap_or(PanelMode::Auto);

    let status_hz = profile.config.backend.status_hz;
    let status_source = profile.status_source;
    let backend: Box<dyn MachineBackend> = if use_linuxcnc {
        Box::new(
            NmlBackend::connect(profile.clone())
                .context("cannot open the LinuxCNC NML channels")?,
        )
    } else {
        Box::new(linc_core::sim::SimBackend::new_connected(
            profile.clone(),
            demo_mode,
        ))
    };

    let hal = if args.no_hal {
        None
    } else {
        match LincHal::new() {
            Ok(hal) => Some(Box::new(hal) as Box<dyn HalWriter>),
            Err(error) => {
                log::warn!("linc.* HAL outputs unavailable: {error}");
                None
            }
        }
    };
    let handle: Box<dyn UiBackend> =
        Box::new(BackendHandle::spawn(backend, hal, status_hz, status_source));

    // Diagnostic socket for linc-mcp. It is created before the window so an
    // agent can attach as soon as the process exists; it carries state only, so
    // a failure here never stops the HMI.
    let diag_bus = std::sync::Arc::new(std::sync::Mutex::new(linc_core::diag::Bus::new(
        linc_core::model::MachineSnapshot::empty(profile.variant),
    )));
    let _diag = if args.no_diag {
        None
    } else {
        let process = linc_core::diag::ProcessInfo {
            pid: std::process::id(),
            role: "hmi".into(),
            build: linc_core::diag::BuildInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                git_commit: option_env!("LINC_GIT_COMMIT").unwrap_or("").into(),
                git_dirty: option_env!("LINC_GIT_DIRTY").is_some(),
                profile: profile.config.machine.name.clone(),
                backend: if use_linuxcnc {
                    "linuxcnc".into()
                } else {
                    "sim".into()
                },
            },
        };
        let config = serde_json::json!({
            "config_path": config_path.display().to_string(),
            "ini": profile.config.machine.linuxcnc_ini,
            "summary": profile.summary(),
            "hal": {
                "component": profile.config.hal.linc_component,
                "panel_component": profile.config.hal.panel_component,
                "allowed_outputs": profile.config.hal.allowed_outputs,
                "write_policy": profile.config.hal.write_policy,
            },
        });
        // A crash record is the only diagnostic that survives the process: the
        // hook writes it before the panic propagates.
        linc_diag::crash::install_hook("hmi", process.clone());
        let source = std::sync::Arc::new(
            linc_diag::BusSource::new(diag_bus.clone(), process).with_config(config),
        );
        match linc_diag::serve(linc_diag::discovery::runtime_dir(), "hmi", source) {
            Ok(handle) => {
                log::info!("diagnostic socket at {}", handle.socket.display());
                Some(handle)
            }
            Err(error) => {
                log::warn!("diagnostic socket unavailable: {error}");
                None
            }
        }
    };

    let title = profile.config.ui.window_title.clone();
    // The shell is designed at 1280x720 and scales up from that baseline.
    let size = [1280.0, 720.0];
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([1280.0, 720.0])
        .with_title(title.clone());
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let window_mode = if args.fullscreen {
        WindowMode::Fullscreen
    } else {
        resolve_window_mode(
            &profile.config.ui.window_mode,
            &profile.config.machine.kind,
        )
    };
    // Fullscreen has no frame to decorate either way.
    let decorated = window_mode != WindowMode::Fullscreen && profile.config.ui.decorations;
    viewport = viewport.with_decorations(decorated);
    match window_mode {
        WindowMode::Windowed => {}
        WindowMode::Maximized => viewport = viewport.with_maximized(true),
        WindowMode::Fullscreen => viewport = viewport.with_fullscreen(true),
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let hal_enabled = !args.no_hal;
    let result = eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            let mut app = LincApp::new(profile.clone(), handle, &cc.egui_ctx);
            app.set_diag_bus(diag_bus.clone());
            Ok(Box::new(MachineOffGuard::new(app, hal_enabled, window_mode)))
        }),
    );
    result.map_err(|error| anyhow::anyhow!("UI error: {error}"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowMode {
    Windowed,
    Maximized,
    Fullscreen,
}

/// Resolve `[ui] window_mode`. `"auto"` keeps the historical behaviour: a real
/// machine takes the whole screen, a simulation opens a window. A control panel
/// that must leave the window manager's decorations off sets `"maximized"` and
/// `decorations = false` in its profile.
fn resolve_window_mode(mode: &str, machine_kind: &str) -> WindowMode {
    match mode {
        "windowed" => WindowMode::Windowed,
        "maximized" => WindowMode::Maximized,
        "fullscreen" => WindowMode::Fullscreen,
        _ if machine_kind == "real" => WindowMode::Fullscreen,
        _ => WindowMode::Windowed,
    }
}

/// HAL pin the cabinet's machine-on switch drives; see the machine's post-GUI
/// file, which extends `sig_rio_machine-on-switch_bit` into it.
const MACHINE_ON_PIN: &str = "linc.machine-on";

/// Ends the session when the cabinet is switched off.
///
/// The read lives here rather than in `linc-ui`, which must never touch HAL;
/// `linc-ui` only exposes the forced-quit entry point. A session that never
/// ends is also a Pi that never powers off, which is why this exists at all.
///
/// One falling edge is not a shutdown: the off half of the switch cycle that
/// re-powers a machine a probe trip has already switched off. `halui` needs a
/// rising edge on `halui.machine.on` to power back on, so recovery is toggling
/// the same switch; ending LiNC there would take the HMI away from the latched
/// fault before the operator could reset it.
struct MachineOffGuard {
    inner: LincApp,
    reader: Option<HalReader>,
    last: Option<bool>,
    /// How the window should sit, re-asserted after creation.
    window: WindowMode,
    /// Frames spent re-asserting the window state; bounded so a compositor that
    /// disagrees does not get a command every frame forever.
    window_attempts: u8,
}

impl MachineOffGuard {
    fn new(inner: LincApp, hal_enabled: bool, window: WindowMode) -> Self {
        let reader = if hal_enabled {
            match HalReader::new_with_role("ui") {
                Ok(reader) => Some(reader),
                Err(error) => {
                    log::warn!("machine-on input unavailable: {error}");
                    None
                }
            }
        } else {
            None
        };
        Self {
            inner,
            reader,
            last: None,
            window,
            window_attempts: 0,
        }
    }

    /// with_maximized / with_fullscreen are applied at window creation, which
    /// xfwm4 (this machine's window manager) ignores for an undecorated window:
    /// the panel came up 1280x720 inside a borderless frame. Ask again once the
    /// window exists, and size it to the monitor as a fallback that does not
    /// depend on the window manager at all.
    fn reassert_window_state(&mut self, ctx: &egui::Context) {
        if self.window == WindowMode::Windowed || self.window_attempts >= 30 {
            return;
        }
        self.window_attempts += 1;
        if let Some(size) = ctx.input(|input| input.viewport().monitor_size) {
            if self.window == WindowMode::Maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::ZERO));
            }
        }
        match self.window {
            WindowMode::Maximized => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true)),
            WindowMode::Fullscreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true))
            }
            WindowMode::Windowed => {}
        }
    }

    fn poll_machine_on(&mut self) {
        let Some(reader) = &self.reader else { return };
        let Some(HalPinValue::Bit(now)) = reader.read(MACHINE_ON_PIN) else {
            return;
        };
        let probe_trip_switched_machine_off = self.inner.snapshot.probe_trip_switched_machine_off();
        if machine_off_ends_session(self.last, now, probe_trip_switched_machine_off) {
            self.inner.request_machine_off();
        } else if machine_off_edge(self.last, now) {
            log::info!("machine-on dropped during a probe-trip machine-off; session kept");
        }
        self.last = Some(now);
    }
}

/// True only on a genuine high-to-low transition. `previous` stays `None` until
/// the first successful read, so a pin that is low from the start — an
/// unwired input in simulation — never quits the session.
fn machine_off_edge(previous: Option<bool>, now: bool) -> bool {
    previous == Some(true) && !now
}

/// A switch-off ends the session unless a probe trip has already switched the
/// machine off and the operator is cycling the switch to re-power it.
fn machine_off_ends_session(
    previous: Option<bool>,
    now: bool,
    probe_trip_switched_machine_off: bool,
) -> bool {
    machine_off_edge(previous, now) && !probe_trip_switched_machine_off
}

impl eframe::App for MachineOffGuard {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.poll_machine_on();
        self.reassert_window_state(ui.ctx());
        eframe::App::ui(&mut self.inner, ui, frame);
    }
}

#[cfg(test)]
mod machine_off_tests {
    use super::{machine_off_edge, machine_off_ends_session, resolve_window_mode, WindowMode};

    #[test]
    fn window_mode_resolves_auto_by_machine_kind() {
        assert_eq!(resolve_window_mode("auto", "real"), WindowMode::Fullscreen);
        assert_eq!(resolve_window_mode("auto", "sim"), WindowMode::Windowed);
        assert_eq!(resolve_window_mode("maximized", "real"), WindowMode::Maximized);
        assert_eq!(resolve_window_mode("windowed", "real"), WindowMode::Windowed);
        assert_eq!(resolve_window_mode("fullscreen", "sim"), WindowMode::Fullscreen);
    }

    #[test]
    fn quits_only_on_a_falling_edge() {
        assert!(machine_off_edge(Some(true), false));
        assert!(!machine_off_edge(Some(false), false));
        assert!(!machine_off_edge(Some(false), true));
        assert!(!machine_off_edge(Some(true), true));
    }

    #[test]
    fn an_unwired_pin_never_quits() {
        // Low from the very first read: no edge, no quit.
        assert!(!machine_off_edge(None, false));
        // High on the first read only records the baseline.
        assert!(!machine_off_edge(None, true));
    }

    #[test]
    fn a_probe_trip_machine_off_survives_the_recovery_toggle() {
        // The trip switched the machine off through halui; halui needs a rising
        // edge to power back on, so the operator cycles the switch. The off
        // half must keep the session alive for the reset.
        assert!(!machine_off_ends_session(Some(true), false, true));
        // The switch-off after the trip is reset is a real shutdown again.
        assert!(machine_off_ends_session(Some(true), false, false));
        // The trip latch suppresses only the falling edge.
        assert!(!machine_off_ends_session(Some(false), true, true));
        assert!(!machine_off_ends_session(None, false, true));
    }
}

fn parse_args() -> Args {
    // LinuxCNC invokes a display as `<display> -ini <file>`. Clap treats
    // `-ini` as bundled short flags, so normalize the single-dash spelling.
    let mut raw: Vec<OsString> = std::env::args_os().collect();
    for arg in &mut raw {
        if arg == "-ini" {
            *arg = OsString::from("--ini");
        }
    }
    Args::parse_from(raw)
}

fn parse_demo_mode(value: Option<&str>) -> Option<PanelMode> {
    let value = value?;
    Some(match value.to_ascii_lowercase().as_str() {
        "edit" => PanelMode::Edit,
        "auto" => PanelMode::Auto,
        "mdi" => PanelMode::Mdi,
        "jog" => PanelMode::Jog,
        "inc" => PanelMode::Inc,
        "mpg" => PanelMode::Mpg,
        "home" => PanelMode::Home,
        "proghome" | "prog-home" => PanelMode::ProgHome,
        _ => return None,
    })
}
