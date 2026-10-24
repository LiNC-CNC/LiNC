//! Conditional waits for the diagnostic test harness.
//!
//! A condition is an allowlisted path, one operator and a scalar. There is no
//! JSONPath, no regex and no evaluation: an agent can only ask whether a named
//! piece of state reached a value, which is what makes a wait reproducible.
//!
//! The vocabulary is deliberately the state an operator would watch: the panel's
//! link and controls, the task's power and interpreter, the program and the
//! alarms. Every path is listed in [`PATHS`]; adding one is a code change.

use std::time::{Duration, Instant};

use linc_core::diag::DiagError;
use serde_json::Value;

/// State a condition may read. `panel.*` and `sim.*` come from the panel
/// service's socket; the rest come from the HMI's snapshot.
pub const PATHS: &[&str] = &[
    "panel.connected",
    "panel.communication_error",
    "panel.estop",
    "panel.protect",
    "panel.mode",
    "panel.power",
    "panel.buttons",
    "panel.current_tool",
    "panel.next_tool",
    "panel.sseg_status",
    "sim.connected",
    "sim.error",
    "sim.estop",
    "sim.power",
    "sim.mode",
    "sim.program_protect",
    "sim.faults",
    "machine.estop",
    "machine.on",
    "machine.fault",
    "machine.homing",
    "machine.nml_command_connected",
    "machine.nml_status_connected",
    "task.power",
    "task.mode",
    "task.interp",
    "task.paused",
    "task.running",
    "task.motion_active",
    "program.loaded",
    "program.running",
    "program.paused",
    "program.dirty",
    "program.current_line",
    "program.motion_line",
    "program.progress",
    "alarms.length",
    "ui.page",
];

const OPS: [&str; 7] = ["eq", "ne", "lt", "lte", "gt", "gte", "contains"];

#[derive(Clone, Debug, PartialEq)]
pub struct Condition {
    path: String,
    op: String,
    value: Value,
}

impl Condition {
    pub fn parse(args: &Value) -> Result<Self, DiagError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| DiagError::invalid_argument("a condition needs a path"))?;
        if !PATHS.contains(&path) {
            return Err(DiagError::invalid_argument(format!(
                "unknown condition path {path:?}; allowed paths: {}",
                PATHS.join(", ")
            )));
        }
        let op = args.get("op").and_then(Value::as_str).unwrap_or("eq");
        if !OPS.contains(&op) {
            return Err(DiagError::invalid_argument(format!(
                "unknown operator {op:?}; allowed: {}",
                OPS.join(", ")
            )));
        }
        let value = args
            .get("value")
            .cloned()
            .ok_or_else(|| DiagError::invalid_argument("a condition needs a value"))?;
        Ok(Self {
            path: path.into(),
            op: op.into(),
            value,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn matches(&self, actual: &Value) -> Result<bool, DiagError> {
        match self.op.as_str() {
            "eq" => Ok(values_equal(actual, &self.value)),
            "ne" => Ok(!values_equal(actual, &self.value)),
            "contains" => Ok(contains(actual, &self.value)),
            "lt" | "lte" | "gt" | "gte" => {
                let left = as_number(actual).ok_or_else(|| {
                    DiagError::invalid_argument(format!("{} is not numeric", self.path))
                })?;
                let right = as_number(&self.value).ok_or_else(|| {
                    DiagError::invalid_argument(format!("{} needs a numeric value", self.path))
                })?;
                Ok(match self.op.as_str() {
                    "lt" => left < right,
                    "lte" => left <= right,
                    "gt" => left > right,
                    _ => left >= right,
                })
            }
            other => Err(DiagError::invalid_argument(format!(
                "unknown operator {other:?}"
            ))),
        }
    }
}

/// Strings compare without ASCII case, so `ON` matches the serialized `On` and
/// a mode label matches in either case. Everything else is exact.
fn values_equal(actual: &Value, expected: &Value) -> bool {
    match (actual.as_str(), expected.as_str()) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => actual == expected,
    }
}

fn contains(actual: &Value, expected: &Value) -> bool {
    match actual {
        Value::String(haystack) => expected
            .as_str()
            .map(|needle| haystack.to_lowercase().contains(&needle.to_lowercase()))
            .unwrap_or(false),
        Value::Array(items) => items.iter().any(|item| values_equal(item, expected)),
        _ => false,
    }
}

fn as_number(value: &Value) -> Option<f64> {
    value.as_f64()
}

pub struct WaitOutcome {
    pub matched: bool,
    pub elapsed_ms: u128,
    pub samples: u64,
    /// The last value read, so a failed wait reports what it saw.
    pub value: Value,
}

/// Poll `sample` until the condition matches or the timeout expires.
pub fn wait_for<F>(
    condition: &Condition,
    timeout: Duration,
    interval: Duration,
    mut sample: F,
) -> Result<WaitOutcome, DiagError>
where
    F: FnMut() -> Result<Value, DiagError>,
{
    let started = Instant::now();
    let mut samples = 0u64;
    loop {
        let value = sample()?;
        samples += 1;
        if condition.matches(&value)? {
            return Ok(WaitOutcome {
                matched: true,
                elapsed_ms: started.elapsed().as_millis(),
                samples,
                value,
            });
        }
        if started.elapsed() >= timeout {
            return Ok(WaitOutcome {
                matched: false,
                elapsed_ms: started.elapsed().as_millis(),
                samples,
                value,
            });
        }
        std::thread::sleep(
            interval
                .min(timeout.saturating_sub(started.elapsed()))
                .max(Duration::from_millis(1)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn condition(path: &str, op: &str, value: Value) -> Condition {
        Condition::parse(&json!({ "path": path, "op": op, "value": value })).expect("condition")
    }

    #[test]
    fn only_allowlisted_paths_and_operators_parse() {
        let error = Condition::parse(&json!({ "path": "machine.secret", "value": 1 }))
            .expect_err("unknown path");
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert!(error.message.contains("allowed paths"));

        let error =
            Condition::parse(&json!({ "path": "machine.estop", "op": "regex", "value": 1 }))
                .expect_err("unknown operator");
        assert_eq!(error.code, "INVALID_ARGUMENT");

        let parsed = Condition::parse(&json!({ "path": "machine.estop", "value": true }))
            .expect("default operator");
        assert_eq!(parsed.op, "eq");
    }

    #[test]
    fn comparisons_cover_labels_numbers_and_collections() {
        assert!(condition("panel.mode", "eq", json!("auto"))
            .matches(&json!("AUTO"))
            .unwrap());
        assert!(condition("task.power", "eq", json!("ON"))
            .matches(&json!("On"))
            .unwrap());
        assert!(condition("program.current_line", "gte", json!(10))
            .matches(&json!(12))
            .unwrap());
        assert!(!condition("program.current_line", "lt", json!(10))
            .matches(&json!(12))
            .unwrap());
        assert!(condition("task.mode", "contains", json!("ut"))
            .matches(&json!("Auto"))
            .unwrap());
        assert!(condition("alarms.length", "eq", json!(2))
            .matches(&json!(2))
            .unwrap());
        assert!(condition("panel.mode", "eq", json!("AUTO"))
            .matches(&json!("JOG"))
            .map(|matched| !matched)
            .unwrap());
    }

    #[test]
    fn ordering_a_non_numeric_path_fails_with_its_name() {
        let error = condition("ui.page", "gt", json!(1))
            .matches(&json!("PROGRAM"))
            .expect_err("strings have no order");
        assert!(error.message.contains("ui.page"));
    }

    #[test]
    fn wait_for_stops_when_the_value_arrives() {
        let mut calls = 0;
        let outcome = wait_for(
            &condition("program.current_line", "gte", json!(3)),
            Duration::from_millis(200),
            Duration::from_millis(1),
            || {
                calls += 1;
                Ok(json!(calls))
            },
        )
        .expect("wait");
        assert!(outcome.matched);
        assert_eq!(outcome.samples, 3);
        assert_eq!(outcome.value, json!(3));
    }

    #[test]
    fn a_timeout_returns_the_last_value_instead_of_an_error() {
        let outcome = wait_for(
            &condition("machine.estop", "eq", json!(false)),
            Duration::from_millis(20),
            Duration::from_millis(5),
            || Ok(json!(true)),
        )
        .expect("wait");
        assert!(!outcome.matched);
        assert!(outcome.samples >= 1);
        assert_eq!(outcome.value, json!(true));
        assert!(outcome.elapsed_ms >= 20);
    }
}
