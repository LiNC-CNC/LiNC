//! The MCP stdio loop.
//!
//! Reads newline-delimited JSON-RPC from `reader`, writes replies to `writer`.
//! stdout belongs to the transport, so diagnostics go to stderr.

use std::io::{BufRead, Write};
use std::time::Instant;

use linc_core::diag::DiagError;
use serde_json::{json, Value};

use crate::audit::Audit;
use crate::policy::Policy;

pub struct Session {
    pub client: linc_diag::Client,
    pub policy: Policy,
    pub audit: Audit,
}

impl Session {
    pub fn new(client: linc_diag::Client, policy: Policy, audit: Audit) -> Self {
        Self {
            client,
            policy,
            audit,
        }
    }
}

/// Serve until `reader` is exhausted. A protocol error is answered, never fatal.
pub fn serve<R: BufRead, W: Write>(reader: R, mut writer: W, session: &mut Session) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::warn!("stdin read failed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") },
                });
                writeln!(writer, "{reply}")?;
                writer.flush()?;
                continue;
            }
        };
        if let Some(reply) = handle(request, session) {
            writeln!(writer, "{reply}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn handle(request: Value, session: &mut Session) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    // Notifications have no id and must not be answered.
    let is_notification = id.is_null();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    if is_notification {
        return None;
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": crate::PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false },
            },
            "serverInfo": {
                "name": crate::SERVER_NAME,
                "title": crate::SERVER_TITLE,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": crate::INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(crate::tools::list()),
        "tools/call" => call_tool(session, &params),
        "resources/list" => Ok(crate::resources::list()),
        "resources/read" => crate::resources::read(&mut session.client, &params),
        "prompts/list" => Ok(crate::prompts::list()),
        "prompts/get" => crate::prompts::get(&params),
        "logging/setLevel" => Ok(json!({})),
        other => Err(JsonRpcError::method_not_found(other)),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        }),
    })
}

pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
}

impl From<DiagError> for JsonRpcError {
    fn from(error: DiagError) -> Self {
        Self {
            code: -32603,
            message: format!("{}: {}", error.code, error.message),
        }
    }
}

fn call_tool(session: &mut Session, params: &Value) -> Result<Value, JsonRpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| JsonRpcError {
            code: -32602,
            message: "tools/call needs a tool name".into(),
        })?;
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if crate::tools::is_write(name) {
        if let Err(error) = session.policy.check_write(name) {
            return Ok(tool_error(&error));
        }
    }
    let started = Instant::now();
    let result = crate::tools::call(&mut session.client, name, &args);
    let duration_ms = started.elapsed().as_millis();
    let audit_id = match &result {
        Ok(_) => session.audit.record(name, &args, "ok", duration_ms),
        Err(error) => session
            .audit
            .record(name, &args, &format!("error:{}", error.code), duration_ms),
    };
    match result {
        Ok(envelope) => {
            let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
            Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "audit_id": audit_id,
                "isError": false,
            }))
        }
        Err(error) => {
            let mut payload = serde_json::to_value(&error).unwrap_or_else(|_| json!({}));
            if let Some(object) = payload.as_object_mut() {
                object.insert("audit_id".into(), json!(audit_id));
            }
            Ok(json!({
                "content": [{ "type": "text", "text": payload.to_string() }],
                "isError": true,
            }))
        }
    }
}

fn tool_error(error: &DiagError) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string(error).unwrap_or_default() }],
        "isError": true,
    })
}
