//! Append-only audit trail for tool calls.
//!
//! One JSON object per line: timestamp, tool, argument keys (not values — they
//! can be large), outcome and the audit id returned to the client. The MCP
//! server's stdout is the transport, so the default sink is stderr; `--audit`
//! also writes a file.

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

pub struct Audit {
    path: Option<PathBuf>,
    sequence: u64,
}

impl Audit {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, sequence: 0 }
    }

    /// Record a call and return the id reported to the client.
    pub fn record(&mut self, tool: &str, args: &Value, outcome: &str, duration_ms: u128) -> String {
        self.sequence += 1;
        let id = format!("audit-{}-{}", linc_core::diag::now_ms(), self.sequence);
        let keys: Vec<String> = args
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        let record = json!({
            "audit_id": id,
            "at_ms": linc_core::diag::now_ms(),
            "tool": tool,
            "argument_keys": keys,
            "outcome": outcome,
            "duration_ms": duration_ms,
        });
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{record}");
            }
        }
        eprintln!("linc-mcp audit: {record}");
        id
    }
}
