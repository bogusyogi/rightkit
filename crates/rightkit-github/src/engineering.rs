//! Engineering diagnostics over imported JSON evidence: bounded file reads, the imported-evidence
//! envelope, and the work-plan, review-loop and delivery assessments. Assessment is separate from
//! execution and authority. Enabled by the `observer` feature.
//!
//! Result `kind` ids read `<prefix>-<name>`; the app passes the prefix its stored results already
//! carry.

use std::{fs::File, io::Read, path::Path};

use rightkit_agent_orchestration::{
    delivery::{assess_delivery, DeliveryAssessmentRequest},
    workplan::{parse_work_plan_json, validate_work_plan},
};
use rightkit_review::code_review::rounds::{assess_review_loop, ReviewLoopInput};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Default cap on an input file.
pub const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
/// Cap on a raw provider report file.
pub const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;

/// A command failure with the exit code the app should use.
#[derive(Debug)]
pub struct CommandError {
    /// Exit code: 1 policy, 2 incomplete, 3 internal, 4 usage, 5 integrity.
    pub code: i32,
    /// What went wrong.
    pub message: String,
}

impl CommandError {
    /// Bad input (exit 4).
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
        }
    }

    /// The command could not finish (exit 2).
    pub fn incomplete(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    /// An I/O or serialisation failure (exit 3).
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }

    /// The caller cancelled (exit 2).
    pub fn cancelled() -> Self {
        Self {
            code: 2,
            message: "CANCELLED: task cancelled".into(),
        }
    }

    /// Input bytes do not match what they claim (exit 5).
    pub fn integrity(message: impl Into<String>) -> Self {
        Self {
            code: 5,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

/// A command result: JSON, or the failure.
pub type CommandResult = Result<Value, CommandError>;

/// An I/O or serialisation failure as an internal error.
pub fn io_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::internal(error.to_string())
}

/// Read a regular file of at most `limit` bytes.
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, CommandError> {
    let file = File::open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(CommandError::usage("input must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(CommandError::usage(format!("input exceeds {limit} bytes")));
    }
    Ok(bytes)
}

/// Parse JSON input, mapping a failure to a usage error.
pub fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CommandError> {
    serde_json::from_slice(bytes)
        .map_err(|error| CommandError::usage(format!("invalid engineering input: {error}")))
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn digest(bytes: &[u8]) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(bytes))
}

/// The evidence envelope for an imported assertion. Hashes bind bytes, not truth; they do not
/// upgrade imported assertions to proof.
pub fn imported(kind: &str, bytes: &[u8], assessment: Value) -> Value {
    json!({
        "schemaVersion": 1,
        "kind": kind,
        "provenance": {"origin": "imported", "inputDigest": digest(bytes), "authenticated": false},
        "effectAuthority": "none",
        "assessment": assessment
    })
}

/// Validate a work plan file: `<prefix>-work-plan-assessment`. An invalid plan is a result with
/// `valid: false`, not an error.
pub fn work_plan(prefix: &str, input: &Path) -> CommandResult {
    let bytes = read_bounded(input, MAX_INPUT_BYTES)?;
    let kind = format!("{prefix}-work-plan-assessment");
    let result = parse_work_plan_json(&bytes).and_then(|plan| validate_work_plan(&plan));
    match result {
        Ok(assessment) => Ok(imported(&kind, &bytes, json!(assessment))),
        Err(error) => {
            let mut value = imported(&kind, &bytes, json!({"error": error}));
            value["valid"] = json!(false);
            Ok(value)
        }
    }
}

/// Assess a review loop file: `<prefix>-review-loop-assessment`.
pub fn review_loop(prefix: &str, input: &Path) -> CommandResult {
    let bytes = read_bounded(input, MAX_INPUT_BYTES)?;
    let parsed: ReviewLoopInput = parse(&bytes)?;
    let assessment =
        assess_review_loop(&parsed).map_err(|error| CommandError::usage(error.to_string()))?;
    Ok(imported(
        &format!("{prefix}-review-loop-assessment"),
        &bytes,
        json!(assessment),
    ))
}

/// Assess one imported delivery observation: `<prefix>-delivery-assessment`. Never merges.
pub fn delivery_assess(prefix: &str, input: &Path) -> CommandResult {
    let bytes = read_bounded(input, MAX_INPUT_BYTES)?;
    let request: DeliveryAssessmentRequest = parse(&bytes)?;
    let assessment =
        assess_delivery(&request).map_err(|error| CommandError::usage(error.to_string()))?;
    Ok(imported(
        &format!("{prefix}-delivery-assessment"),
        &bytes,
        json!(assessment),
    ))
}

/// Exit status of a diagnostic result: it reflects the verdict, never authorisation to act.
/// `None` for a result that is not one of this module's kinds (or lacks the fields).
pub fn diagnostic_exit_code(prefix: &str, value: &Value) -> Option<i32> {
    let kind = value.get("kind")?.as_str()?;
    let name = kind.strip_prefix(prefix)?.strip_prefix('-')?;
    match name {
        "work-plan-assessment" => Some(i32::from(value["valid"] == false)),
        "review-loop-assessment" => Some(match value["assessment"]["status"].as_str()? {
            "approved" => 0,
            "needs_revision" | "exhausted" => 1,
            _ => 2,
        }),
        "delivery-assessment" | "delivery-observation" => {
            Some(match value["assessment"]["status"].as_str()? {
                "ready_for_handoff" | "ready_for_merge_request" | "observed_merged" => 0,
                "closed_without_merge" => 1,
                _ => 2,
            })
        }
        "quality-observation" => Some(match value["assessment"]["state"].as_str()? {
            "measured" => 0,
            "policyAttention" => 1,
            _ => 2,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rk-github-eng-{name}-{}", std::process::id()))
    }

    #[test]
    fn reads_are_bounded_and_regular_only() {
        let file = scratch("bounded");
        std::fs::write(&file, b"12345").unwrap();
        assert_eq!(read_bounded(&file, 5).unwrap(), b"12345");
        assert_eq!(read_bounded(&file, 4).unwrap_err().code, 4);
        assert_eq!(read_bounded(&std::env::temp_dir(), 5).unwrap_err().code, 4);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn imported_envelope_binds_bytes_and_grants_no_authority() {
        let value = imported("x-kind", b"abc", json!({"ok": true}));
        assert_eq!(value["effectAuthority"], "none");
        assert_eq!(value["provenance"]["authenticated"], false);
        assert_eq!(
            value["provenance"]["inputDigest"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn exit_codes_follow_the_verdict_for_the_apps_prefix_only() {
        let ok = json!({"kind": "demo-delivery-assessment", "assessment": {"status": "ready_for_handoff"}});
        assert_eq!(diagnostic_exit_code("demo", &ok), Some(0));
        assert_eq!(diagnostic_exit_code("other", &ok), None);
        let bad = json!({"kind": "demo-work-plan-assessment", "valid": false});
        assert_eq!(diagnostic_exit_code("demo", &bad), Some(1));
        let loopy =
            json!({"kind": "demo-review-loop-assessment", "assessment": {"status": "exhausted"}});
        assert_eq!(diagnostic_exit_code("demo", &loopy), Some(1));
        assert_eq!(
            diagnostic_exit_code("demo", &json!({"kind": "demo-other"})),
            None
        );
    }

    #[test]
    fn an_unreadable_work_plan_is_an_error_but_an_invalid_one_is_a_result() {
        let file = scratch("plan");
        std::fs::write(&file, b"not json").unwrap();
        let value = work_plan("demo", &file).unwrap();
        assert_eq!(value["valid"], false);
        assert_eq!(value["kind"], "demo-work-plan-assessment");
        let _ = std::fs::remove_file(&file);
        assert!(work_plan("demo", &scratch("absent")).is_err());
    }
}
