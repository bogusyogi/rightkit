//! Tool-dispatch helper functions used by the session loop.

use crate::llm::LlmResponse;

/// Parse, apply only schema-proven repairs, then validate tool-call arguments.
/// Empty/whitespace is a legitimate no-arg call. Repairs never invent values:
/// they are restricted to an encoded JSON object or array, a renamed edit key,
/// an index-keyed object, singleton arrays, primitive string coercions, & omission
/// of optional nulls. Invalid calls never execute.
///
/// Every repair applied and every validation failure is recorded
/// (master-change 5.3) with tool, repair kind, bounded validator-error kind,
/// success, and the daemon-injected provider/model attribution (`None` when a
/// caller has none).
pub(crate) fn parse_tool_arguments_attributed(
    arguments_json: &str,
    schema: Option<&serde_json::Value>,
    tool: Option<&str>,
    attribution: Option<&(Option<String>, Option<String>)>,
) -> (serde_json::Value, Option<String>) {
    let record = |repair_kind: Option<&str>, validator_error: Option<&str>, success: bool| {
        rightkit_agent_telemetry::record_repair_event(rightkit_agent_telemetry::RepairEvent {
            provider: attribution.and_then(|(provider, _)| provider.clone()),
            model: attribution.and_then(|(_, model)| model.clone()),
            tool: tool.map(str::to_string),
            repair_kind: repair_kind.map(str::to_string),
            validator_error: validator_error.map(str::to_string),
            success,
        });
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
    rightkit_agent_telemetry::validator_error_kind(error)
}

fn validation_error(error: String, schema: &serde_json::Value) -> String {
    let schema_hint = serde_json::to_string(schema)
        .map(|encoded| format!("\nExpected input schema:\n{encoded}"))
        .unwrap_or_default();
    format!(
        "Invalid tool arguments: {error}. Re-issue this call with values matching the schema.{schema_hint}"
    )
}

/// Field-name spellings models send for `multi_edit`/`edit_file` style objects.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("old", "old_string"),
    ("oldString", "old_string"),
    ("old_str", "old_string"),
    ("new", "new_string"),
    ("newString", "new_string"),
    ("new_str", "new_string"),
];

/// JSON-type description of a rejected value: types, lengths and key names only, never content.
fn value_shape(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(_) => "a boolean".to_string(),
        serde_json::Value::Number(_) => "a number".to_string(),
        serde_json::Value::String(text) => {
            let lead = text.trim_start().chars().next();
            let lead = match lead {
                Some(c @ ('{' | '[')) => format!(" starting with \"{c}\""),
                _ => String::new(),
            };
            format!("a string of {} chars{lead}", text.chars().count())
        }
        serde_json::Value::Array(items) => format!("an array of {} items", items.len()),
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map
                .keys()
                .take(8)
                .map(|k| {
                    k.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                        .take(40)
                        .collect::<String>()
                })
                .collect();
            let more = if map.len() > 8 { ",..." } else { "" };
            format!("an object with keys {}{more}", keys.join(","))
        }
    }
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
        // Models spell the edit fields `old`/`new` or camelCase; rename only when the schema
        // names the target property and the object lacks it, so no content is guessed.
        if let (Some(object), Some(properties)) = (
            value.as_object_mut(),
            schema
                .get("properties")
                .and_then(serde_json::Value::as_object),
        ) {
            for (alias, target) in KEY_ALIASES {
                if properties.contains_key(*target)
                    && !object.contains_key(*target)
                    && object.contains_key(*alias)
                {
                    if let Some(moved) = object.remove(*alias) {
                        object.insert((*target).to_string(), moved);
                        repairs.push("key_alias_renamed");
                    }
                }
            }
        }
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

    // A model sometimes sends an array as a JSON-encoded string (`"edits": "[{...}]"`). Decode
    // it, as an encoded object is decoded above; wrapping the string as one item turned it into
    // `["[{...}]"]` and the call was refused ("$.edits[0] must be object": 19 Sonnet
    // `multi_edit` calls in the task-set run on build 96).
    if schema_type(schema) == Some("array") {
        if let serde_json::Value::String(encoded) = value {
            if let Ok(decoded) = serde_json::from_str::<serde_json::Value>(encoded) {
                if decoded.is_array() {
                    *value = decoded;
                    repairs.push("json_string_to_array");
                }
            }
        }
    }
    // An object keyed "0","1",... is an array a model spelled as a map; order by index.
    if schema_type(schema) == Some("array") {
        if let serde_json::Value::Object(map) = value {
            let mut indexed: Vec<(usize, serde_json::Value)> = Vec::new();
            for (key, item) in map.iter() {
                match key.parse::<usize>() {
                    Ok(index) => indexed.push((index, item.clone())),
                    Err(_) => {
                        indexed.clear();
                        break;
                    }
                }
            }
            if !indexed.is_empty() && indexed.len() == map.len() {
                indexed.sort_by_key(|(index, _)| *index);
                *value = serde_json::Value::Array(indexed.into_iter().map(|(_, v)| v).collect());
                repairs.push("numeric_keys_object_to_array");
            }
        }
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
            return Err(format!(
                "{path} must be {expected} (got {})",
                value_shape(value)
            ));
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
                return Err(format!(
                    "{path}.{required} is required (value is {})",
                    value_shape(value)
                ));
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
    /// The stage's name on the qualification trace.
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
}

/// Records the admission stages one effect traversed, so a fixture can prove
/// that different surfaces converge on the same authority.
#[derive(Debug, Default)]
pub struct EffectAdmissionTrace {
    stages: std::sync::Mutex<std::collections::VecDeque<(String, EffectAdmissionStage)>>,
}

/// Upper bound on retained trace entries; the oldest are dropped first, so a
/// call whose entries were evicted reports no stages.
const ADMISSION_TRACE_CAPACITY: usize = 16_384;

/// Production trace sink shared by every entrypoint.  Keeping the sink here
/// makes the dispatch seam observable without giving any host a second
/// admission implementation.  Runtime qualification reads this same trace;
/// callers never seed stage labels into it.
static PRODUCTION_ADMISSION_TRACE: std::sync::OnceLock<EffectAdmissionTrace> =
    std::sync::OnceLock::new();

pub(crate) fn production_admission_trace() -> &'static EffectAdmissionTrace {
    PRODUCTION_ADMISSION_TRACE.get_or_init(EffectAdmissionTrace::default)
}

/// The labels of the stages recorded for `call_id`, in recorded order.
pub(crate) fn admission_stage_labels(call_id: &str) -> Vec<&'static str> {
    production_admission_trace()
        .stages_for(call_id)
        .into_iter()
        .map(EffectAdmissionStage::label)
        .collect()
}

pub(crate) fn record_production_admission_stage(call_id: &str, stage: EffectAdmissionStage) {
    production_admission_trace().record(call_id, stage);
}

impl EffectAdmissionTrace {
    pub fn record(&self, call_id: &str, stage: EffectAdmissionStage) {
        if let Ok(mut stages) = self.stages.lock() {
            stages.push_back((call_id.to_string(), stage));
            while stages.len() > ADMISSION_TRACE_CAPACITY {
                stages.pop_front();
            }
        }
    }

    /// The stages recorded for `call_id`, in recorded order.
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
}
