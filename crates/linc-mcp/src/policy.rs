//! Write policy and tool annotations.
//!
//! No tool can move a machine or touch a real panel. The only write surface is
//! a simulator panel, and it stays closed until the operator starts the MCP
//! server with `--allow-sim-control --sim` *and* the panel service with
//! `--allow-sim-control`. Writes are refused by name, so adding one without a
//! policy decision is a compile-time-visible change to [`crate::tools::ToolClass`].

use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Simulator panel control. Reads are always allowed.
    pub allow_sim_control: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_sim_control: false,
        }
    }
}

impl Policy {
    /// Refuse a write unless the operator opted in. The returned error is the
    /// one an agent should see, not a panic.
    pub fn check_write(&self, tool: &str) -> Result<(), linc_core::diag::DiagError> {
        if !self.allow_sim_control {
            return Err(linc_core::diag::DiagError::new(
                "WRITE_DISABLED",
                format!(
                    "{tool} changes a simulated panel; restart linc-mcp with --allow-sim-control --sim \
                     and linc-panel with --allow-sim-control"
                ),
                false,
            ));
        }
        Ok(())
    }
}

/// MCP annotations per tool class (§8.3). Returned with every tool.
pub fn annotations(class: crate::tools::ToolClass) -> serde_json::Value {
    if class.read_only() {
        json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        })
    } else {
        json!({
            "readOnlyHint": false,
            "destructiveHint": class.destructive(),
            "idempotentHint": false,
            "openWorldHint": false,
        })
    }
}
