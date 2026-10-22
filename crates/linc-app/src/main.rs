//! LiNC HMI entry point.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use linc_core::backend::{BackendHandle, HalWriter, MachineBackend};
use linc_core::config::Profile;
use linc_core::model::PanelMode;
use linc_hal::LincHal;
use linc_linuxcnc::PythonBridgeBackend;
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

fn main() -> Result<()> {    init_logging();
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
            PythonBridgeBackend::spawn(profile.clone())
                .context("cannot start the LinuxCNC bridge")?,
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
                backend: if use_linuxcnc { "linuxcnc".into() } else { "sim".into() },
            },
        };
        let config = serde_json::json!({
            "config_path": config_path.display().to_string(),
            "ini": profile.config.machine.linuxcnc_ini,
            "summary": profile.summary(),
        });
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
    if args.fullscreen || profile.config.machine.kind == "real" {
        viewport = viewport.with_fullscreen(args.fullscreen);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            let mut app = LincApp::new(profile.clone(), handle, &cc.egui_ctx);
            app.set_diag_bus(diag_bus.clone());
            Ok(Box::new(app))
        }),
    );
    result.map_err(|error| anyhow::anyhow!("UI error: {error}"))?;
    Ok(())
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
