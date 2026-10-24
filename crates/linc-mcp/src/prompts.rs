//! MCP prompts: guided diagnostic sequences an agent can start from.

use serde_json::{json, Value};

pub fn list() -> Value {
    json!({
        "prompts": [
            {
                "name": "linc_diagnose_program_start",
                "title": "Why won't the program start?",
                "description": "Collect state, blockers, program and panel status, then explain the block.",
                "arguments": [
                    { "name": "symptom", "required": false, "description": "What the operator expected to happen." }
                ],
            },
            {
                "name": "linc_diagnose_panel",
                "title": "Is the panel link healthy?",
                "description": "Check panel connection, communication errors, protect state and recent panel events.",
                "arguments": [],
            }
        ]
    })
}

pub fn get(params: &Value) -> Result<Value, crate::server::JsonRpcError> {
    let name =
        params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| crate::server::JsonRpcError {
                code: -32602,
                message: "prompts/get needs a name".into(),
            })?;
    let symptom = params
        .get("arguments")
        .and_then(|args| args.get("symptom"))
        .and_then(Value::as_str)
        .unwrap_or("the program does not start");
    let text = match name {
        "linc_diagnose_program_start" => format!(
            "The operator says: {symptom}.\n\n\
             Run this sequence and explain the result with evidence:\n\
             1. linc_health_check — freshness, panel link, drops.\n\
             2. linc_why_blocked — every blocker with its code and remediation.\n\
             3. linc_get_state_summary — mode, interpreter, power, homing, program.\n\
             4. linc_get_program — is a program loaded, is it dirty, is there a syntax error?\n\
             5. linc_get_panel_state is covered by the summary; check `protect` and `panel_online`.\n\
             Do not suggest an action that the gate report still blocks. Quote `stale` and `age_ms`."
        ),
        "linc_diagnose_panel" => "\
             Check the panel link with evidence:\n\
             1. linc_health_check — panel findings.\n\
             2. linc_get_events with kind \"panel\" — when the link changed.\n\
             3. linc_get_logs with level \"warn\" — link and frame errors.\n\
             4. linc_get_state_summary — `panel_online`, `protect`, `mode`."
            .into(),
        other => {
            return Err(crate::server::JsonRpcError {
                code: -32602,
                message: format!("unknown prompt {other:?}"),
            })
        }
    };
    Ok(json!({
        "description": "LiNC diagnostic guidance",
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}
