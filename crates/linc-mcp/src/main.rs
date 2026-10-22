//! `linc-mcp` — MCP server for LiNC diagnostics.
//!
//! Speaks MCP on stdio and the diagnostic protocol on the Unix socket. It never
//! links HAL, LinuxCNC, the panel driver or the UI.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "linc-mcp", version, about = "MCP server for LiNC diagnostics")]
struct Args {
    /// Override the `linc` diagnostic socket.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Override the `linc-panel` diagnostic socket.
    #[arg(long)]
    panel_socket: Option<PathBuf>,
    /// Append audit records to this file in addition to stderr.
    #[arg(long)]
    audit: Option<PathBuf>,
    /// Allow tools that change a simulator panel. Off by default; the panel
    /// service must also be started with --allow-sim-control.
    #[arg(long)]
    allow_sim_control: bool,
    /// Required together with --allow-sim-control: state that this session is a
    /// simulator, so no tool may touch a real panel.
    #[arg(long)]
    sim: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,linc_mcp=info"))
        .init();
    let args = Args::parse();
    if args.allow_sim_control && !args.sim {
        bail!("--allow-sim-control requires --sim: simulator control is never enabled for a real machine");
    }
    let socket = args
        .socket
        .clone()
        .or_else(|| linc_diag::discovery::discover("linc", None));
    let socket = socket.context(
        "no linc diagnostic socket found: start linc, or pass --socket /path/to/linc.sock",
    )?;
    let client = linc_diag::Client::connect(&socket)
        .with_context(|| format!("cannot connect to {}", socket.display()))?;
    // The panel is optional: its tools answer PANEL_NOT_RUNNING when it is absent.
    let panel = args
        .panel_socket
        .clone()
        .or_else(|| linc_diag::discovery::discover("panel", None))
        .and_then(|path| linc_diag::Client::connect(&path).ok());
    let policy = linc_mcp::policy::Policy {
        allow_sim_control: args.allow_sim_control && args.sim,
    };
    if policy.allow_sim_control {
        log::warn!("simulator control enabled (--allow-sim-control --sim); every call is audited");
    }
    linc_diag::crash::install_hook(
        "mcp",
        linc_core::diag::ProcessInfo {
            pid: std::process::id(),
            role: "mcp".into(),
            build: linc_core::diag::BuildInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                backend: "mcp".into(),
                ..linc_core::diag::BuildInfo::default()
            },
        },
    );
    let audit = linc_mcp::audit::Audit::new(args.audit.clone());
    let mut session = linc_mcp::server::Session::new(client, panel, policy, audit);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    linc_mcp::serve(stdin.lock(), stdout.lock(), &mut session)?;
    Ok(())
}
