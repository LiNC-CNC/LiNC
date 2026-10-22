//! MCP resources: read-only views of the same state, addressable by URI.

use serde_json::{json, Value};

use crate::server::JsonRpcError;

const RESOURCES: &[(&str, &str, &str)] = &[
    ("linc://state/summary", "LiNC state summary", "Operator-level state and blockers."),
    ("linc://ui", "LiNC UI state", "Page, mode, views and selection."),
    ("linc://snapshot", "LiNC machine snapshot", "Full snapshot without the live trail."),
    ("linc://offsets", "LiNC work offsets", "All systems and the active one."),
    ("linc://tools", "LiNC tool table", "Rows, columns and tool change state."),
    ("linc://program", "LiNC program", "Loaded program metadata and text head."),
    ("linc://logs", "LiNC logs", "Recent log records."),
];

pub fn list() -> Value {
    json!({
        "resources": RESOURCES
            .iter()
            .map(|(uri, name, description)| json!({
                "uri": uri,
                "name": name,
                "description": description,
                "mimeType": "application/json",
            }))
            .collect::<Vec<_>>()
    })
}

pub fn read(client: &mut linc_diag::Client, params: &Value) -> Result<Value, JsonRpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| JsonRpcError {
            code: -32602,
            message: "resources/read needs a uri".into(),
        })?;
    let envelope = match uri {
        "linc://state/summary" => client.call("diag.summary", Value::Null),
        "linc://ui" => client.call("diag.ui", Value::Null),
        "linc://snapshot" => client.call("diag.snapshot", json!({ "include_trail": false })),
        "linc://offsets" => client.call("diag.offsets", Value::Null),
        "linc://tools" => client.call("diag.tools", Value::Null),
        "linc://program" => client.call("diag.program", json!({ "line_count": 100 })),
        "linc://logs" => client.call("diag.logs", json!({ "limit": 200 })),
        other => {
            return Err(JsonRpcError {
                code: -32002,
                message: format!("unknown resource {other:?}"),
            })
        }
    };
    let envelope: Value = envelope.map_err(JsonRpcError::from)?;
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": serde_json::to_string_pretty(&envelope).unwrap_or_default(),
        }]
    }))
}
