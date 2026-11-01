#![allow(clippy::items_after_test_module)]

//! Tool-dispatch helper functions used by the session loop.

use crate::llm::LlmResponse;
use std::path::{Component, Path, PathBuf};

/// Stable identity key for a tool's target path, used to decide whether two
/// write calls touch the same file. Resolves relative paths against the
/// workspace root and collapses `.`/`..` lexically (no filesystem access - the
/// file may not exist yet). Lower-cased so case-only differences on
/// case-insensitive filesystems (Windows/macOS) are treated as the SAME file -
/// erring toward sequential execution, never toward a false "disjoint".
#[allow(dead_code)]
pub(crate) fn lexical_path_key(workspace_root: &Path, path: &str) -> String {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace_root.join(raw)
    };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    let mut buf = PathBuf::new();
    for c in out {
        buf.push(c);
    }
    buf.to_string_lossy().to_lowercase().replace('\\', "/")
}

/// Parse, apply only schema-proven repairs, then validate tool-call arguments.
/// Empty/whitespace is a legitimate no-arg call. Repairs never invent values:
/// they are restricted to an encoded JSON object, singleton arrays, primitive
/// string coercions, & omission of optional nulls. Invalid calls never execute.
#[allow(dead_code)]
pub(crate) fn parse_tool_arguments(
    arguments_json: &str,
    schema: Option<&serde_json::Value>,
) -> (serde_json::Value, Option<String>) {
    parse_tool_arguments_attributed(arguments_json, schema, None, None)
}

/// [`parse_tool_arguments`] plus repair-telemetry attribution
/// (master-change 5.3): every repair applied and every validation failure is
/// recorded with tool, repair kind, bounded validator-error kind, success,
/// and the daemon-injected provider/model attribution. The unattributed
/// legacy entry point records with `None` attribution.
pub(crate) fn parse_tool_arguments_attributed(
    arguments_json: &str,
    schema: Option<&serde_json::Value>,
    tool: Option<&str>,
    attribution: Option<&(Option<String>, Option<String>)>,
) -> (serde_json::Value, Option<String>) {
    let record = |repair_kind: Option<&str>, validator_error: Option<&str>, success: bool| {
        rightkit_logs::agent_telemetry::record_repair_event(
            rightkit_logs::agent_telemetry::RepairEvent {
                provider: attribution.and_then(|(provider, _)| provider.clone()),
                model: attribution.and_then(|(_, model)| model.clone()),
                tool: tool.map(str::to_string),
                repair_kind: repair_kind.map(str::to_string),
                validator_error: validator_error.map(str::to_string),
                success,
            },
        );
    };
    let trimmed = arguments_json.trim();
    if trimmed.is_empty() {
        return (serde_json::Value::Null, None);
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(mut value) => {
            let mut repairs = Vec::new();
            if let Some(schema) = schema {
                repair_value(&mut value, schema, &mut repairs);
                if let Err(error) = validate_value(&value, schema, "$") {
                    // The repaired payload still failed validation: record
                    // one failure event per repair kind plus the bounded
                    // validator-error kind.
                    for repair in &repairs {
                        record(Some(repair), Some(&validator_error_kind(&error)), false);
                    }
                    if repairs.is_empty() {
                        record(None, Some(&validator_error_kind(&error)), false);
                    }
                    return (
                        serde_json::Value::Null,
                        Some(validation_error(error, schema)),
                    );
                }
            }
            if !repairs.is_empty() {
                // Telemetry intentionally excludes argument bodies. Provider &
                // model attribution belongs at each adapter boundary; the
                // engine seam records the deterministic repair kinds it
                // applied plus the injected attribution.
                for repair in &repairs {
                    record(Some(repair), None, true);
                }
                tracing::info!(repair_kinds = %repairs.join(","), "tool argument repair applied");
            }
            (value, None)
        }
        Err(e) => {
            record(None, Some("not_valid_json"), false);
            let msg = schema.map_or_else(
                || format!("Invalid tool arguments: not valid JSON ({e}). Re-issue this call with a single well-formed JSON object."),
                |schema| validation_error(format!("not valid JSON ({e})"), schema),
            );
            (serde_json::Value::Null, Some(msg))
        }
    }
}

fn validator_error_kind(error: &str) -> String {
    rightkit_logs::agent_telemetry::validator_error_kind(error)
}

fn validation_error(error: String, schema: &serde_json::Value) -> String {
    let schema_hint = serde_json::to_string(schema)
        .map(|encoded| format!("\nExpected input schema:\n{encoded}"))
        .unwrap_or_default();
    format!(
        "Invalid tool arguments: {error}. Re-issue this call with values matching the schema.{schema_hint}"
    )
}

fn repair_value(
    value: &mut serde_json::Value,
    schema: &serde_json::Value,
    repairs: &mut Vec<&'static str>,
) {
    if schema_type(schema) == Some("object") {
        if let serde_json::Value::String(encoded) = value {
            if let Ok(decoded) = serde_json::from_str::<serde_json::Value>(encoded) {
                if decoded.is_object() {
                    *value = decoded;
                    repairs.push("json_string_to_object");
                }
            }
        }
        let required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            if let Some(properties) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                for (name, property_schema) in properties {
                    let optional = !required.contains(&name.as_str());
                    if object.get(name).is_some_and(serde_json::Value::is_null) && optional {
                        object.remove(name);
                        repairs.push("optional_null_omitted");
                    }
                    if let Some(property) = object.get_mut(name) {
                        repair_value(property, property_schema, repairs);
                    }
                }
            }
        }
        return;
    }

    if schema_type(schema) == Some("array") && !value.is_array() {
        let original = std::mem::take(value);
        *value = serde_json::Value::Array(vec![original]);
        repairs.push("singleton_to_array");
    }
    if schema_type(schema) == Some("array") {
        if let (Some(items), Some(values)) = (schema.get("items"), value.as_array_mut()) {
            for item in values {
                repair_value(item, items, repairs);
            }
        }
        return;
    }

    let Some(string) = value.as_str().map(str::to_string) else {
        return;
    };
    match schema_type(schema) {
        Some("boolean") => match string.as_str() {
            "true" => {
                *value = serde_json::Value::Bool(true);
                repairs.push("string_to_boolean");
            }
            "false" => {
                *value = serde_json::Value::Bool(false);
                repairs.push("string_to_boolean");
            }
            _ => {}
        },
        Some("integer") => {
            if let Ok(number) = string.parse::<i64>() {
                *value = serde_json::Value::Number(number.into());
                repairs.push("string_to_integer");
            }
        }
        Some("number") => {
            if let Ok(number) = string.parse::<f64>() {
                if let Some(number) = serde_json::Number::from_f64(number) {
                    *value = serde_json::Value::Number(number);
                    repairs.push("string_to_number");
                }
            }
        }
        _ => {}
    }
}

fn validate_value(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    if let Some(expected) = schema_type(schema) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            _ => true,
        };
        if !matches {
            return Err(format!("{path} must be {expected}"));
        }
    }
    if let Some(object) = value.as_object() {
        for required in schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(format!("{path}.{required} is required"));
            }
        }
        if let Some(properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            for (name, child_schema) in properties {
                if let Some(child) = object.get(name) {
                    validate_value(child, child_schema, &format!("{path}.{name}"))?;
                }
            }
        }
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (index, child) in values.iter().enumerate() {
            validate_value(child, items, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn schema_type(schema: &serde_json::Value) -> Option<&str> {
    schema.get("type").and_then(serde_json::Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::parse_tool_arguments;

    #[test]
    fn repairs_only_schema_proven_values_before_validation() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["count", "enabled", "paths"],
            "properties": {
                "count": { "type": "integer" },
                "enabled": { "type": "boolean" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "optional": { "type": "string" }
            }
        });
        let (args, error) = parse_tool_arguments(
            r#""{\"count\":\"2\",\"enabled\":\"true\",\"paths\":\"src/lib.rs\",\"optional\":null}""#,
            Some(&schema),
        );
        assert!(error.is_none(), "{error:?}");
        assert_eq!(args["count"], 2);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["paths"], serde_json::json!(["src/lib.rs"]));
        assert!(args.get("optional").is_none());
    }

    #[test]
    fn rejects_values_that_remain_schema_invalid_after_repair() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["count"],
            "properties": { "count": { "type": "integer" } }
        });
        let (args, error) = parse_tool_arguments(r#"{"count":"two"}"#, Some(&schema));
        assert!(args.is_null());
        assert!(error.unwrap().contains("$.count must be integer"));
    }
}

pub(crate) fn strip_safety_terminated_tool_calls(response: &mut LlmResponse) -> bool {
    let Some(reason) = response.finish_reason.as_deref() else {
        return false;
    };
    if !is_safety_finish_reason(reason) || response.tool_calls.is_empty() {
        return false;
    }

    response.tool_calls.clear();
    let note = format!(
        "\n\n[Stopped by provider safety policy before tool execution: {reason}. No requested tool calls were run.]"
    );
    if response.text.trim().is_empty() {
        response.text = note.trim().to_string();
    } else {
        response.text.push_str(&note);
    }
    true
}

fn is_safety_finish_reason(reason: &str) -> bool {
    let normalized = reason.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "content_filter" | "safety" | "safety_stop" | "policy" | "policy_violation" | "refusal"
    ) || normalized.contains("safety")
        || normalized.contains("content_filter")
        || normalized.contains("policy")
}

// ── EFF-009: incomplete-generation gate ────────────────────────────────────
//
// Canon: only a COMPLETE model generation may enter approval/effect dispatch.
// Syntactically valid JSON is not sufficient proof of completeness — a
// provider can terminate mid-tool-call (or mid-turn) and still hand back a
// tool call whose arguments happen to parse. This is a typed classification
// consulted immediately in front of dispatch, mirroring
// `strip_safety_terminated_tool_calls` above but keyed on truncation rather
// than safety refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationCompleteness {
    /// The provider reports (or implies, by absence of a truncation signal)
    /// that generation ran to a natural stop. Tool calls may proceed.
    Complete,
    /// The provider's own termination signal says output was cut off before
    /// generation finished (length/max-token/output-limit family). Any tool
    /// calls attached to this turn are provider-truncated artifacts, not
    /// deliberate complete calls, regardless of whether their JSON parses.
    IncompleteGeneration,
    /// Generation was not reported as truncated, but at least one attached
    /// tool call fails structural well-formedness (missing id/name, or
    /// arguments that are not valid JSON). Treated the same as incomplete:
    /// never dispatched.
    Malformed,
}

/// Provider terminal reasons which mean output was cut short, not completed.
/// Duplicated intentionally from `host_providers`' wire-level
/// `is_incomplete_finish_reason`: this crate does not depend on the
/// providers crate, and the classification belongs at the dispatch seam
/// regardless of which adapter produced the turn (including scripted/stub
/// turns used in tests, which never go through provider wire parsing).
fn is_incomplete_finish_reason(reason: &str) -> bool {
    matches!(
        reason.trim().to_ascii_lowercase().as_str(),
        "length" | "max_tokens" | "max_output_tokens" | "token_limit" | "output_limit"
    )
}

fn tool_call_is_malformed(call: &rightkit_agent_engine_protocol::ToolCall) -> bool {
    call.id.trim().is_empty()
        || call.name.trim().is_empty()
        || serde_json::from_str::<serde_json::Value>(&call.arguments_json).is_err()
}

/// Classify a completed turn's readiness for approval/effect dispatch.
/// Pure function: does not mutate `response`.
pub(crate) fn classify_generation_completeness(response: &LlmResponse) -> GenerationCompleteness {
    if let Some(reason) = response.finish_reason.as_deref() {
        if is_incomplete_finish_reason(reason) {
            return GenerationCompleteness::IncompleteGeneration;
        }
    }
    if response.tool_calls.iter().any(tool_call_is_malformed) {
        return GenerationCompleteness::Malformed;
    }
    GenerationCompleteness::Complete
}

/// Enforce the EFF-009 gate: only `Complete` turns may keep their tool calls.
/// An `IncompleteGeneration`/`Malformed` turn has its tool calls cleared
/// before this response ever reaches the approval/dispatch loop, so zero
/// approval prompt, zero `ToolCallStarted`, and zero downstream effect are
/// possible for that turn. Returns the classification that was applied, so
/// callers can log/trace it; returns `None` when the turn already had no
/// tool calls (nothing to gate).
pub(crate) fn enforce_generation_completeness_gate(
    response: &mut LlmResponse,
) -> Option<GenerationCompleteness> {
    if response.tool_calls.is_empty() {
        return None;
    }
    let classification = classify_generation_completeness(response);
    if classification == GenerationCompleteness::Complete {
        return None;
    }
    response.tool_calls.clear();
    let reason_note = match classification {
        GenerationCompleteness::IncompleteGeneration => {
            let reason = response.finish_reason.as_deref().unwrap_or("output_limit");
            format!(
                "\n\n[Provider generation was incomplete ({reason}); requested tool calls were withheld from dispatch, not run.]"
            )
        }
        GenerationCompleteness::Malformed => "\n\n[One or more requested tool calls were malformed; withheld from dispatch, not run.]"
            .to_string(),
        GenerationCompleteness::Complete => unreachable!(),
    };
    if response.text.trim().is_empty() {
        response.text = reason_note.trim().to_string();
    } else {
        response.text.push_str(&reason_note);
    }
    Some(classification)
}

/// EFF-002: canonical form of the payload a tool will actually execute.
///
/// Keys are sorted recursively so that two payloads differing only in map
/// ordering produce the same bytes; anything else - an added, removed or
/// altered argument - changes them. Values are never truncated or elided,
/// because a digest over a lossy projection would let a real change slip
/// through unnoticed.
pub fn canonical_effect_payload(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                out.push('{');
                for (index, key) in keys.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String((*key).clone()).to_string());
                    out.push(':');
                    write(&map[*key], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// EFF-002: the identity an approval binds to.
///
/// Derived from the tool name plus the canonical POST-transform payload, so
/// approving one concrete effect cannot authorize a different one. The digest
/// is a SHA-256 over that canonical form and is recomputed immediately before
/// execution; a mismatch means the payload changed after approval and the old
/// approval can no longer authorize it.
///
/// Note it deliberately does NOT include the tool-call id: a retry or replay
/// of the SAME normalized effect keeps its identity, which is what lets an
/// approval survive a benign retry without authorizing anything new.
pub fn effect_payload_digest(tool: &str, args: &serde_json::Value) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(tool.as_bytes());
    hasher.update(&[0_u8]);
    hasher.update(canonical_effect_payload(args).as_bytes());
    format!("sha256:{}", hasher.finalize())
}

#[cfg(test)]
mod eff002_payload_binding_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_identity() {
        let a = json!({"path": "a.rs", "content": "x"});
        let b = json!({"content": "x", "path": "a.rs"});
        assert_eq!(
            effect_payload_digest("write_file", &a),
            effect_payload_digest("write_file", &b),
            "map ordering is not a semantic difference"
        );
    }

    #[test]
    fn changed_path_changes_identity() {
        let approved = json!({"path": "safe.rs", "content": "x"});
        let mutated = json!({"path": "/etc/passwd", "content": "x"});
        assert_ne!(
            effect_payload_digest("write_file", &approved),
            effect_payload_digest("write_file", &mutated),
            "a redirected path must not inherit the original approval"
        );
    }

    #[test]
    fn changed_argv_changes_identity() {
        let approved = json!({"command": "ls -la"});
        let mutated = json!({"command": "rm -rf /"});
        assert_ne!(
            effect_payload_digest("shell", &approved),
            effect_payload_digest("shell", &mutated),
            "a rewritten command must not inherit the original approval"
        );
    }

    #[test]
    fn same_tool_name_with_different_args_is_a_different_effect() {
        let a = json!({"path": "a.rs"});
        let b = json!({"path": "b.rs"});
        assert_ne!(
            effect_payload_digest("read_file", &a),
            effect_payload_digest("read_file", &b)
        );
    }

    #[test]
    fn different_tools_with_identical_args_are_different_effects() {
        let args = json!({"command": "echo hi"});
        assert_ne!(
            effect_payload_digest("shell", &args),
            effect_payload_digest("powershell", &args),
            "the tool is part of the effect identity"
        );
    }

    #[test]
    fn identity_survives_retry_of_the_same_normalized_effect() {
        let args = json!({"path": "a.rs", "content": "x"});
        let first = effect_payload_digest("write_file", &args);
        let retried = effect_payload_digest("write_file", &args.clone());
        assert_eq!(
            first, retried,
            "a benign retry of the same effect keeps its approval identity"
        );
    }

    #[test]
    fn digest_does_not_leak_payload_content() {
        let args =
            json!({"command": concat!("curl -H 'Authorization: Bearer sk", "-secret-value'")});
        let digest = effect_payload_digest("shell", &args);
        assert!(digest.starts_with("sha256:"));
        assert!(
            !digest.contains(concat!("sk", "-secret-value")) && !digest.contains("Authorization"),
            "the digest must not carry payload content or credentials"
        );
        // A SHA-256 hex digest is fixed width regardless of payload size, so
        // it cannot encode the content it covers.
        assert_eq!(digest.len(), "sha256:".len() + 64);
    }

    #[test]
    fn nested_structure_changes_are_detected() {
        let approved = json!({"edits": [{"path": "a.rs", "new": "safe"}]});
        let mutated = json!({"edits": [{"path": "a.rs", "new": "malicious"}]});
        assert_ne!(
            effect_payload_digest("multi_edit", &approved),
            effect_payload_digest("multi_edit", &mutated),
            "a change nested inside an array must still change identity"
        );
    }
}

/// EFF-001: the ordered admission stages every effect-producing surface must
/// traverse. Recorded so convergence is observable rather than asserted in
/// prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectAdmissionStage {
    Validate,
    Transform,
    Normalize,
    Policy,
    Approval,
    Resource,
    Execute,
    Verify,
    Settle,
}

impl EffectAdmissionStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Validate => "validate",
            Self::Transform => "transform",
            Self::Normalize => "normalize",
            Self::Policy => "policy",
            Self::Approval => "approval",
            Self::Resource => "resource",
            Self::Execute => "execute",
            Self::Verify => "verify",
            Self::Settle => "settle",
        }
    }

    /// The canonical order. A surface may legitimately SKIP a stage that does
    /// not apply to it (a read-only call needs no approval), but it may never
    /// reorder them, and it may never reach `Execute` after a denial.
    pub fn canonical_order() -> [EffectAdmissionStage; 9] {
        [
            Self::Validate,
            Self::Transform,
            Self::Normalize,
            Self::Policy,
            Self::Approval,
            Self::Resource,
            Self::Execute,
            Self::Verify,
            Self::Settle,
        ]
    }
}

/// Records the admission stages one effect traversed, so a fixture can prove
/// that different surfaces converge on the same authority.
#[derive(Debug, Default)]
pub struct EffectAdmissionTrace {
    stages: std::sync::Mutex<Vec<(String, EffectAdmissionStage)>>,
}

/// Production trace sink shared by every entrypoint.  Keeping the sink here
/// makes the dispatch seam observable without giving any host a second
/// admission implementation.  Runtime qualification reads this same trace;
/// callers never seed stage labels into it.
static PRODUCTION_ADMISSION_TRACE: std::sync::OnceLock<EffectAdmissionTrace> =
    std::sync::OnceLock::new();

pub(crate) fn production_admission_trace() -> &'static EffectAdmissionTrace {
    PRODUCTION_ADMISSION_TRACE.get_or_init(EffectAdmissionTrace::default)
}

pub(crate) fn record_production_admission_stage(call_id: &str, stage: EffectAdmissionStage) {
    production_admission_trace().record(call_id, stage);
}

impl EffectAdmissionTrace {
    pub fn record(&self, call_id: &str, stage: EffectAdmissionStage) {
        if let Ok(mut stages) = self.stages.lock() {
            stages.push((call_id.to_string(), stage));
        }
    }

    pub fn stages_for(&self, call_id: &str) -> Vec<EffectAdmissionStage> {
        self.stages
            .lock()
            .map(|stages| {
                stages
                    .iter()
                    .filter(|(id, _)| id == call_id)
                    .map(|(_, stage)| *stage)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True when the recorded stages appear in canonical order (skips allowed).
    pub fn is_canonically_ordered(&self, call_id: &str) -> bool {
        let observed = self.stages_for(call_id);
        observed.windows(2).all(|pair| pair[0] <= pair[1])
    }

    pub fn reached_execute(&self, call_id: &str) -> bool {
        self.stages_for(call_id)
            .contains(&EffectAdmissionStage::Execute)
    }
}

#[cfg(test)]
mod eff001_admission_convergence_tests {
    use super::*;

    #[test]
    fn stage_order_is_fixed_and_denial_never_reaches_execute() {
        let trace = EffectAdmissionTrace::default();

        // An approved mutating effect traverses the full authority.
        for stage in EffectAdmissionStage::canonical_order() {
            trace.record("approved", stage);
        }
        assert!(trace.is_canonically_ordered("approved"));
        assert!(trace.reached_execute("approved"));

        // A denied effect stops at Policy and must never reach Execute.
        trace.record("denied", EffectAdmissionStage::Validate);
        trace.record("denied", EffectAdmissionStage::Transform);
        trace.record("denied", EffectAdmissionStage::Normalize);
        trace.record("denied", EffectAdmissionStage::Policy);
        trace.record("denied", EffectAdmissionStage::Settle);
        assert!(trace.is_canonically_ordered("denied"));
        assert!(
            !trace.reached_execute("denied"),
            "denied work must never reach the effect"
        );

        // A read-only effect legitimately skips Approval, but still may not
        // reorder the stages it does traverse.
        trace.record("readonly", EffectAdmissionStage::Validate);
        trace.record("readonly", EffectAdmissionStage::Policy);
        trace.record("readonly", EffectAdmissionStage::Resource);
        trace.record("readonly", EffectAdmissionStage::Execute);
        trace.record("readonly", EffectAdmissionStage::Settle);
        assert!(trace.is_canonically_ordered("readonly"));
    }

    #[test]
    fn out_of_order_admission_is_detected() {
        let trace = EffectAdmissionTrace::default();
        // Executing before policy is exactly the convergence failure this
        // instrument exists to catch.
        trace.record("bad", EffectAdmissionStage::Validate);
        trace.record("bad", EffectAdmissionStage::Execute);
        trace.record("bad", EffectAdmissionStage::Policy);
        assert!(
            !trace.is_canonically_ordered("bad"),
            "execute-before-policy must be reported as out of order"
        );
    }

    #[test]
    fn traces_are_isolated_per_call() {
        let trace = EffectAdmissionTrace::default();
        trace.record("a", EffectAdmissionStage::Execute);
        trace.record("b", EffectAdmissionStage::Policy);
        assert!(trace.reached_execute("a"));
        assert!(!trace.reached_execute("b"));
    }
}
