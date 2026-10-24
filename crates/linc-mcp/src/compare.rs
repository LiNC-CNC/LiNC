//! Compare the panel service's own view with the HMI's copy of it.
//!
//! The HMI does not read the panel socket: it gets its panel snapshot from the
//! backend (HAL or the simulation IPC files). A disagreement between the two is
//! how a stale copy, a lost IPC write or a HAL wiring mistake shows up. The
//! comparison is field by field and intentionally small: only values an agent
//! can act on, not the whole snapshot.

use linc_core::model::PanelModeValidity;
use serde_json::{json, Value};

/// Fields both views carry, compared after normalisation.
const FIELDS: &[&str] = &[
    "connected",
    "communication_error",
    "mode",
    "feed_override",
    "rapid_override",
    "spindle_override",
    "protect",
    "estop",
    "power_on",
    "current_tool",
    "next_tool",
    "buttons",
    "sseg_tool",
    "sseg_next_tool",
    "sseg_status",
];

pub(crate) fn mode_text(value: &Value) -> String {
    match serde_json::from_value::<PanelModeValidity>(value.clone()) {
        Ok(PanelModeValidity::Valid(mode)) => mode.label().into(),
        Ok(PanelModeValidity::Invalid(_)) => "INVALID".into(),
        Ok(PanelModeValidity::Unknown) | Err(_) => "UNKNOWN".into(),
    }
}

fn protect_text(snapshot: &Value) -> String {
    let known = snapshot["program_protect_known"].as_bool().unwrap_or(false);
    if !known {
        "UNKNOWN".into()
    } else if snapshot["program_protect"].as_bool().unwrap_or(false) {
        "ON".into()
    } else {
        "OFF".into()
    }
}

fn normalized(field: &str, snapshot: &Value) -> Value {
    match field {
        "mode" => json!(mode_text(&snapshot["mode"])),
        "protect" => json!(protect_text(snapshot)),
        "buttons" => json!(format!(
            "0x{:016x}",
            snapshot["buttons"].as_u64().unwrap_or_default()
        )),
        other => snapshot.get(other).cloned().unwrap_or(Value::Null),
    }
}

fn side(envelope: &Value) -> Value {
    json!({
        "sequence": envelope.get("sequence").cloned().unwrap_or(Value::Null),
        "age_ms": envelope.get("age_ms").cloned().unwrap_or(Value::Null),
        "stale": envelope.get("stale").cloned().unwrap_or(Value::Null),
    })
}

/// `hmi` is a `diag.snapshot` envelope, `panel` a `diag.panel` envelope.
pub fn compare_views(hmi: &Value, panel: &Value) -> Value {
    let hmi_snapshot = &hmi["data"]["machine"]["panel"];
    let panel_snapshot = &panel["data"]["snapshot"];
    let mut differences = Vec::new();
    for field in FIELDS {
        let left = normalized(field, hmi_snapshot);
        let right = normalized(field, panel_snapshot);
        // The HMI backend reports `None` for a power state it does not model;
        // that is not a disagreement with a simulator that does.
        if field == &"power_on" && (left.is_null() || right.is_null()) {
            continue;
        }
        if left != right {
            differences.push(json!({
                "field": field,
                "hmi": left,
                "panel": right,
            }));
        }
    }
    json!({
        "same": differences.is_empty(),
        "differences": differences,
        "fields_compared": FIELDS.len(),
        "hmi": side(hmi),
        "panel": side(panel),
        "note": "the HMI copies the panel on its own poll cycle; a difference right after a change is expected",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hmi(mode: Value, estop: bool, buttons: u64) -> Value {
        json!({
            "data": { "machine": { "panel": {
                "connected": true,
                "communication_error": false,
                "mode": mode,
                "feed_override": 1.0,
                "rapid_override": 1.0,
                "spindle_override": 1.0,
                "program_protect": false,
                "program_protect_known": true,
                "estop": estop,
                "power_on": null,
                "current_tool": 12,
                "next_tool": 0,
                "buttons": buttons,
                "sseg_tool": 12,
                "sseg_next_tool": 0,
                "sseg_status": 54,
            } } },
            "sequence": 4, "age_ms": 20, "stale": false,
        })
    }

    fn panel(mode: Value, estop: bool, buttons: u64) -> Value {
        json!({
            "data": { "snapshot": {
                "connected": true,
                "communication_error": false,
                "mode": mode,
                "feed_override": 1.0,
                "rapid_override": 1.0,
                "spindle_override": 1.0,
                "program_protect": false,
                "program_protect_known": true,
                "estop": estop,
                "power_on": true,
                "current_tool": 12,
                "next_tool": 0,
                "buttons": buttons,
                "sseg_tool": 12,
                "sseg_next_tool": 0,
                "sseg_status": 54,
            } },
            "sequence": 7, "age_ms": 5, "stale": false,
        })
    }

    #[test]
    fn identical_views_have_no_differences() {
        let auto = json!({ "Valid": "Auto" });
        let report = compare_views(&hmi(auto.clone(), false, 0), &panel(auto, false, 0));
        assert_eq!(report["same"], true, "{report}");
        assert_eq!(report["differences"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_stale_copy_shows_the_field_and_both_values() {
        let report = compare_views(
            &hmi(json!({ "Valid": "Auto" }), false, 0),
            &panel(json!({ "Valid": "Jog" }), true, 1 << 63),
        );
        assert_eq!(report["same"], false);
        let fields: Vec<&str> = report["differences"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|difference| difference["field"].as_str())
            .collect();
        assert_eq!(fields, vec!["mode", "estop", "buttons"], "{report}");
        assert_eq!(report["differences"][0]["hmi"], "AUTO");
        assert_eq!(report["differences"][0]["panel"], "JOG");
        assert_eq!(report["hmi"]["sequence"], 4);
        assert_eq!(report["panel"]["sequence"], 7);
    }

    #[test]
    fn an_unknown_protect_input_is_not_reported_as_off() {
        let mut panel_snapshot = panel(json!({ "Valid": "Auto" }), false, 0);
        panel_snapshot["data"]["snapshot"]["program_protect_known"] = json!(false);
        let report = compare_views(&hmi(json!({ "Valid": "Auto" }), false, 0), &panel_snapshot);
        assert_eq!(report["differences"][0]["field"], "protect");
        assert_eq!(report["differences"][0]["panel"], "UNKNOWN");
    }
}
