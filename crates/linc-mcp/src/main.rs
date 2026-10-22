//! `linc-mcp` — MCP server for LiNC diagnostics.
//!
//! Speaks MCP on stdio and the diagnostic protocol on the Unix socket. It never
//! links HAL, LinuxCNC, the panel driver or the UI.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "linc-mcp", version, about = "MCP server for LiNC diagnostics")]
struct Args {
    /// Override the `linc` diagnostic socket.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Override the `linc-panel` diagnostic socket (reserved; panel tools come later).
    #[arg(long)]
    panel_socket: Option<PathBuf>,
    /// Append audit records to this file in addition to stderr.
    #[arg(long)]
    audit: Option<PathBuf>,
    /// Allow tools that change the simulator. Off by default.
    #[arg(long)]
    allow_writes: bool,
    /// Required together with --allow-writes.
    #[arg(long)]
    sim: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,linc_mcp=info"))
        .init();
    let args = Args::parse();
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
        allow_writes: args.allow_writes,
        allow_sim_writes: args.allow_writes && args.sim,
    };
    if args.allow_writes {
        log::warn!("write tools enabled (sim only); every call is audited");
    }
    let audit = linc_mcp::audit::Audit::new(args.audit.clone());
    let mut session = linc_mcp::server::Session::new(client, panel, policy, audit);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    linc_mcp::serve(stdin.lock(), stdout.lock(), &mut session)?;
    Ok(())
}
