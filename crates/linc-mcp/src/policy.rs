//! Write policy and tool annotations.
//!
//! Every tool is read-only in this phase; the gate exists so a write cannot be
//! added by accident without also adding an annotation and an audit entry.

use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Reads are always allowed. Writes need `--allow-writes` and a simulator.
    pub allow_writes: bool,
    pub allow_sim_writes: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_writes: false,
            allow_sim_writes: false,
        }
    }
}

impl Policy {
    /// Refuse a write unless the operator opted in. The returned error is the
    /// one an agent should see, not a panic.
    pub fn check_write(&self, tool: &str) -> Result<(), linc_core::diag::DiagError> {
        if !self.allow_writes || !self.allow_sim_writes {
            return Err(linc_core::diag::DiagError::new(
                "WRITE_DISABLED",
                format!("{tool} is a write; restart linc-mcp with --allow-writes --sim"),
                false,
            ));
        }
        Ok(())
    }
}

/// MCP annotations per tool class (§8.3). Returned with every tool.
pub fn annotations(read_only: bool) -> serde_json::Value {
    if read_only {
        json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        })
    } else {
        json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        })
    }
}
