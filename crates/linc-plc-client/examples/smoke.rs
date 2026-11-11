//! Load a project into a running daemon, start it, and print one snapshot.
//!
//!     cargo run -p linc-plc-client --example smoke -- configs/plc-sim.json
//!
//! The socket defaults to the session's runtime path, as the HMI uses.

use std::path::PathBuf;

use linc_plc_client::{default_socket_path, Client};
use linc_plc_core::protocol::{Request, Response, RunState};
use linc_plc_core::Project;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "configs/plc-sim.json".to_string())
        .into();
    let socket = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    let text = std::fs::read_to_string(&project_path)?;
    let project: Project = serde_json::from_str(&text)?;
    let mut client = Client::connect(&socket)?;
    println!("hello: {:?}", client.hello()?);

    match client.call(&Request::Load {
        project: project.clone(),
    })? {
        Response::Loaded { hash, diagnostics } => {
            println!(
                "loaded {} rung section(s), hash {hash}, {} diagnostics",
                project.sections.len(),
                diagnostics.len()
            );
            for item in diagnostics.iter().take(4) {
                println!("  {:?} {}", item.severity, item.message);
            }
        }
        other => println!("load replied {other:?}"),
    }
    println!("start: {:?}", client.call(&Request::Start)?);
    // `--force name=true` sets an input for the run, which is how a headless
    // check drives a rung; the daemon must allow forces.
    for argument in std::env::args().skip(3) {
        if let Some((name, value)) = argument.split_once('=') {
            let value = matches!(value, "true" | "1" | "on");
            println!(
                "force {name}={value}: {:?}",
                client.call(&Request::Force {
                    name: name.to_string(),
                    value,
                    ttl_ms: 0,
                })?
            );
        }
    }

    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(300));
        match client.call(&Request::Snapshot)? {
            Response::State(state) => {
                println!(
                    "state {:?} scans {} words {}",
                    state.state,
                    state.scan_count,
                    state.words.len()
                );
                for word in state
                    .words
                    .iter()
                    .filter(|word| word.name == "MW0" || word.name == "TON0.ET")
                {
                    println!("  {} = {}", word.name, word.value);
                }
                for var in state.vars.iter().take(6) {
                    println!("  {} = {}", var.name, var.value);
                }
                let _ = RunState::Running;
            }
            other => println!("snapshot replied {other:?}"),
        }
    }
    if let Ok(Response::Trend(data)) = client.call(&Request::Trend) {
        println!(
            "trend: {} names, {} samples, first row {:?}",
            data.names.len(),
            data.samples.len(),
            data.samples.last()
        );
    }
    let _ = client.call(&Request::Stop);
    Ok(())
}
