//! `linc-plc`: the LiNC ladder PLC daemon.
//!
//! Loaded by LinuxCNC through `loadusr -W linc-plc --project <path>`, or run by
//! hand for tests. SIGINT and SIGTERM set the shutdown flag [`linc_plc::run`]
//! watches; the daemon then drives every output false, raises the fault pin,
//! removes its socket and unregisters the HAL component.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Parser;

use linc_plc::Config;

#[derive(Parser)]
#[command(
    name = "linc-plc",
    version,
    about = "LiNC ladder PLC service",
    long_about = "Runs one ladder project as a userspace PLC with its own \
                  linc_plc.* HAL component and serves the LiNC UI over a Unix socket."
)]
struct Cli {
    /// Ladder project JSON; read at startup to bind the HAL pin map.
    #[arg(long, env = "LINC_PLC_PROJECT", value_name = "PATH")]
    project: PathBuf,

    /// Unix socket to serve [default: linc_plc_client::default_socket_path()]
    #[arg(long, env = "LINC_PLC_SOCKET", value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Do not create the HAL component or any pin.
    #[arg(long)]
    no_hal: bool,

    /// Target scan period, in microseconds.
    #[arg(long, default_value_t = 2000, value_name = "US")]
    scan_us: u64,

    /// Accept Force requests (commissioning and tests only).
    #[arg(long)]
    allow_force: bool,
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        if let Err(error) = signal_hook::flag::register(signal, Arc::clone(&shutdown)) {
            log::error!("linc-plc: cannot install the signal handler: {error}");
            return ExitCode::FAILURE;
        }
    }

    let config = Config {
        project: cli.project,
        socket: cli
            .socket
            .unwrap_or_else(linc_plc_client::default_socket_path),
        no_hal: cli.no_hal,
        scan_us: cli.scan_us,
        allow_force: cli.allow_force,
    };

    match linc_plc::run(config, shutdown) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("linc-plc: {error}");
            ExitCode::FAILURE
        }
    }
}
