//! `agy --output-format stream-json` event normalization.
//!
//! One NDJSON object per stdout line, discriminated by the top-level `event`
//! field. The observed v1.2.16 frame families (fixture: a single tiny `-p`
//! turn checked in under `crates/driver_host/tests/fixtures/`):
//!
//! - `{"event":"init","conversation_id":"...","init":{...}}` — session
//!   handshake carrying the vendor conversation id used for `--conversation`
//!   resume.
//! - `{"event":"step_update","step_update":{"conversation_id":"...",
//!   "step_index":N,"state":"ACTIVE|DONE","step_type":"user_input|
//!   agent_response|...","text_delta":"..."}}` — per-step progress.
//!   `agent_response` steps carry the streamed assistant text in
//!   `text_delta` while `ACTIVE`; `DONE` marks step settlement.
//! - `{"event":"result","result":{"conversation_id":"...","status":"SUCCESS",
//!   "response":"...","num_turns":N,"usage":{...}}}` — terminal frame.
//!   `status` is `SUCCESS` on completion; failure/cancellation statuses map
//!   to `TurnFailed`/`TurnInterrupted`.
//!
//! Fail-closed posture matches the Claude parser: a line that is not JSON or
//! lacks the `event` discriminant is an error, and an unrecognized `event`
//! family is `UnknownRequiredEvent` — the consumer decides whether an unknown
//! vendor addition is fatal. Unknown `step_update` `step_type`s are treated
//! as vendor bookkeeping and skipped: they describe presentation progress,
//! not effects, and new step kinds must not silently masquerade as output.

use serde_json::Value;

use crate::driver_host::{
    event_key, invalid, stable_event_key, DriverHostError, DriverKind, NormalizedEvent,
    NormalizedEventKind,
};

/// Parse one `agy` stream-json NDJSON line into the shared normalized event
/// vocabulary. Returns `Ok(None)` for bookkeeping frames that carry no
/// transcript-relevant content (step settlement, user echoes, progress
/// notices).
pub fn parse_agy_stream_line(
    line: &str,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Antigravity, error.to_string()))?;
    let event = value
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(DriverKind::Antigravity, "missing event"))?;
    let kind = match event {
        "init" => NormalizedEventKind::SessionStarted {
            vendor_session_id: value
                .get("conversation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(DriverKind::Antigravity, "init missing conversation_id"))?
                .to_string(),
            init: None,
        },
        "step_update" => match normalize_step_update(&value)? {
            Some(kind) => kind,
            None => return Ok(None),
        },
        "result" => normalize_result(&value)?,
        unknown => {
            return Err(DriverHostError::UnknownRequiredEvent {
                driver: DriverKind::Antigravity,
                method: unknown.into(),
            })
        }
    };
    Ok(Some(NormalizedEvent {
        event_key: stable_event_key(DriverKind::Antigravity, event, &value, source_offset),
        driver: DriverKind::Antigravity,
        source_offset,
        vendor_method: event.into(),
        kind,
    }))
}

fn normalize_step_update(value: &Value) -> Result<Option<NormalizedEventKind>, DriverHostError> {
    let step = value
        .get("step_update")
        .ok_or_else(|| invalid(DriverKind::Antigravity, "step_update missing payload"))?;
    let step_type = step.get("step_type").and_then(Value::as_str).unwrap_or("");
    match step_type {
        "agent_response" => {
            // Streamed assistant text arrives on ACTIVE steps; the DONE frame
            // repeats trailing text and carries duration/usage bookkeeping.
            if step.get("state").and_then(Value::as_str) == Some("ACTIVE") {
                if let Some(text) = step.get("text_delta").and_then(Value::as_str) {
                    return Ok(Some(NormalizedEventKind::AssistantDelta {
                        text: text.to_string(),
                    }));
                }
            }
            Ok(None)
        }
        // Tool-call steps surface the vendor's own tool surface. The field
        // names are projected defensively: `call_id`/`id` for identity,
        // `tool_name`/`name` for the tool, `arguments`/`input` for the call,
        // `result`/`output` for a settled result. A step type that names a
        // tool but matches no known payload still yields no event rather than
        // a fabricated transcript row.
        other if other.contains("tool") => Ok(normalize_tool_step(step)),
        // `user_input`, `thinking`, plan/notice steps and any future
        // bookkeeping kind: presentation progress, not transcript content.
        _ => Ok(None),
    }
}

fn normalize_tool_step(step: &Value) -> Option<NormalizedEventKind> {
    let call_id = step
        .get("call_id")
        .or_else(|| step.get("tool_call_id"))
        .or_else(|| step.get("id"))
        .and_then(Value::as_str)?;
    let state = step
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("ACTIVE");
    if state == "DONE" {
        return Some(NormalizedEventKind::ToolResult {
            call_id: call_id.to_string(),
            is_error: step
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            content: step
                .get("result")
                .or_else(|| step.get("output"))
                .cloned()
                .unwrap_or(Value::Null),
        });
    }
    Some(NormalizedEventKind::ToolCallStarted {
        call_id: call_id.to_string(),
        tool_name: step
            .get("tool_name")
            .or_else(|| step.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string(),
        arguments: step
            .get("arguments")
            .or_else(|| step.get("input"))
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn normalize_result(value: &Value) -> Result<NormalizedEventKind, DriverHostError> {
    let result = value
        .get("result")
        .ok_or_else(|| invalid(DriverKind::Antigravity, "result missing payload"))?;
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    if status == "SUCCESS" {
        let usage = result.get("usage");
        return Ok(match usage {
            Some(usage) => NormalizedEventKind::TurnCompletedWithUsage {
                prompt_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                completion_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                cache_read_tokens: usage
                    .get("cache_read_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                cache_creation_tokens: usage
                    .get("cache_creation_input_tokens")
                    .or_else(|| usage.get("cache_creation_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
            },
            None => NormalizedEventKind::TurnCompleted,
        });
    }
    if status.contains("CANCEL") || status.contains("INTERRUPT") {
        return Ok(NormalizedEventKind::TurnInterrupted);
    }
    let detail = result
        .get("error")
        .or_else(|| result.get("message"))
        .and_then(Value::as_str)
        .or_else(|| result.get("response").and_then(Value::as_str));
    Ok(NormalizedEventKind::TurnFailed {
        message: match detail {
            Some(detail) => format!("agy turn {status}: {detail}"),
            None => format!("agy turn ended with status {status}"),
        },
    })
}

/// Test-only hook: expose the raw `result.response` fallback so a turn that
/// streamed no deltas still yields its final text to the presentation layer.
#[allow(dead_code)]
pub(crate) fn result_response_text(value: &Value) -> Option<&str> {
    value
        .get("result")
        .and_then(|result| result.get("response"))
        .and_then(Value::as_str)
}
