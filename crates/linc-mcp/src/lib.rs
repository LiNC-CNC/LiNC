//! MCP adapter over the LiNC diagnostic socket.
//!
//! The MCP transport is JSON-RPC 2.0 over stdio, one object per line, which is
//! what an MCP client speaks. Nothing here depends on the UI, HAL or LinuxCNC:
//! every piece of machine state comes from `linc-diag`.

pub mod audit;
pub mod policy;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod tools;

pub use server::serve;

pub const SERVER_NAME: &str = "linc";
pub const SERVER_TITLE: &str = "LiNC diagnostics";
pub const PROTOCOL_VERSION: &str = "2024-11-05";

pub const INSTRUCTIONS: &str = "\
Read-only LiNC diagnostics. Real-machine control is disabled by default.\n\
Check `stale` and `age_ms` before trusting state; a stale snapshot fails closed\n\
for any write decision. The panel and its hardware E-stop remain authoritative.\n\
Use linc_why_blocked before suggesting an action, and prefer linc_get_state_summary\n\
for an operator-level view.";

#[cfg(test)]
mod tests {
    use super::*;
    use linc_core::diag::{BuildInfo, Bus, ProcessInfo};
    use linc_core::model::{MachineSnapshot, MachineVariant};
    use serde_json::{json, Value};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    fn session(name: &str) -> (server::Session, linc_diag::ServerHandle) {
        let dir = std::env::temp_dir().join(format!("linc-mcp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bus = Arc::new(Mutex::new(Bus::new(MachineSnapshot::empty(
            MachineVariant::Mill,
        ))));
        let process = ProcessInfo {
            pid: std::process::id(),
            role: "hmi".into(),
            build: BuildInfo {
                version: "test".into(),
                ..BuildInfo::default()
            },
        };
        let source = Arc::new(linc_diag::BusSource::new(bus, process));
        let handle = linc_diag::serve(dir, "hmi", source).expect("serve");
        let client = linc_diag::Client::connect(&handle.socket).expect("connect");
        let session = server::Session::new(client, None, policy::Policy::default(), audit::Audit::new(None));
        (session, handle)
    }

    fn round_trip(session: &mut server::Session, requests: &[Value]) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut out = Vec::new();
        crate::serve(Cursor::new(input), &mut out, session).expect("serve");
        String::from_utf8(out)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json reply"))
            .collect()
    }

    #[test]
    fn mcp_answers_initialize_list_and_tool_calls() {
        let (mut session, handle) = session("protocol");
        let replies = round_trip(
            &mut session,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"linc_get_state_summary","arguments":{}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}),
                json!({"jsonrpc":"2.0","id":5,"method":"prompts/list"}),
                json!({"jsonrpc":"2.0","id":6,"method":"ping"}),
            ],
        );
        assert_eq!(replies.len(), 6, "one reply per request: {replies:?}");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "linc");
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        let tools = replies[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "linc_get_snapshot"));
        assert!(tools.iter().any(|tool| tool["name"] == "linc_screenshot"));
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);

        let content = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
        let envelope: Value = serde_json::from_str(content).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["data"]["machine"], "", "summary of an empty machine");
        assert!(envelope["data"]["blockers"].as_array().is_some());

        assert!(replies[3]["result"]["resources"].as_array().unwrap().len() >= 5);
        assert!(replies[4]["result"]["prompts"].as_array().unwrap().len() >= 2);
        assert_eq!(replies[5]["result"], json!({}));

        // A notification must not produce a reply.
        let replies = round_trip(
            &mut session,
            &[json!({"jsonrpc":"2.0","method":"notifications/initialized"})],
        );
        assert!(replies.is_empty());

        // An unknown method is answered, not fatal.
        let replies = round_trip(
            &mut session,
            &[json!({"jsonrpc":"2.0","id":9,"method":"nope/nope"})],
        );
        assert_eq!(replies[0]["error"]["code"], -32601);
        drop(handle);
    }

    #[test]
    fn tools_are_read_only_by_default_and_unknown_names_fail_cleanly() {
        let (mut session, handle) = session("readonly");
        assert!(!session.policy.allow_writes);
        assert!(!tools::is_gated("linc_get_snapshot"));

        let replies = round_trip(
            &mut session,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"linc_set_something","arguments":{}}
            })],
        );
        assert_eq!(replies[0]["result"]["isError"], true);
        let text = replies[0]["result"]["content"][0]["text"].as_str().unwrap();
        let error: Value = serde_json::from_str(text).unwrap();
        assert_eq!(error["code"], "NOT_FOUND");
        assert!(error["audit_id"].as_str().is_some(), "every call is audited: {error}");
        drop(handle);
    }

    #[test]
    fn history_diff_and_timeline_answer_from_the_bus() {
        let dir = std::env::temp_dir().join(format!("linc-mcp-{}-history", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bus = Arc::new(Mutex::new(Bus::new(MachineSnapshot::empty(
            MachineVariant::Mill,
        ))));
        let mut first = MachineSnapshot::empty(MachineVariant::Mill);
        first.machine_name = "Before".into();
        bus.lock().unwrap().publish_machine(first);
        let from = bus.lock().unwrap().sequence();
        let mut second = MachineSnapshot::empty(MachineVariant::Mill);
        second.machine_name = "After".into();
        second.machine.estop = true;
        bus.lock().unwrap().publish_machine(second);

        let process = ProcessInfo {
            pid: std::process::id(),
            role: "hmi".into(),
            build: BuildInfo {
                version: "test".into(),
                ..BuildInfo::default()
            },
        };
        let source = Arc::new(linc_diag::BusSource::new(bus, process));
        let handle = linc_diag::serve(dir, "hmi", source).expect("serve");
        let client = linc_diag::Client::connect(&handle.socket).expect("connect");
        let mut session =
            server::Session::new(client, None, policy::Policy::default(), audit::Audit::new(None));

        let replies = round_trip(
            &mut session,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"linc_diff_snapshots","arguments":{"from_sequence":from}}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"linc_get_timeline","arguments":{"limit":10}}}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"linc_get_snapshot_at","arguments":{"sequence":from}}}),
            ],
        );
        for reply in &replies {
            assert_eq!(reply["result"]["isError"], false, "{reply}");
        }
        let diff = json_from(&replies[0]);
        let changes = diff["data"]["changes"].as_array().unwrap();
        assert!(
            changes.iter().any(|change| change["path"] == "machine_name"),
            "{changes:?}"
        );
        assert!(json_from(&replies[1])["data"]["timeline"].is_array());
        assert_eq!(
            json_from(&replies[2])["data"]["machine"]["machine_name"],
            "Before"
        );
        drop(handle);
    }

    fn json_from(reply: &Value) -> Value {
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn a_dropped_diagnostic_server_surfaces_as_an_error_not_a_panic() {
        let (mut session, handle) = session("dropped");
        drop(handle);
        let replies = round_trip(
            &mut session,
            &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"linc_ping","arguments":{}}})],
        );
        assert_eq!(replies[0]["result"]["isError"], true);
        let text = replies[0]["result"]["content"][0]["text"].as_str().unwrap();
        let error: Value = serde_json::from_str(text).unwrap();
        assert_eq!(error["code"], "LINC_NOT_RUNNING");
    }
}
