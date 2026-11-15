//! Cross-vendor history repair for the OpenAI-compatible and Responses routes.
//!
//! A session can change model part-way (or run roles on different vendors), so the history a route
//! receives may hold turns another vendor produced. This module is one pure function over the
//! OpenAI-shaped wire messages (`Vec<serde_json::Value>`, the form `transcript_to_wire` produces and
//! both request builders already hold). It never reorders messages.
//!
//! - Reasoning text is dropped when the target cannot take it back, or when the turn can be shown to
//!   come from another model.
//! - Tool-call ids a target may reject are mapped to a stable short id, in the call and in its result.
//! - Tool calls with no result get a synthetic "interrupted" result.
//! - Images are replaced by a text placeholder when the target is known not to accept images.

use serde_json::{json, Value};

/// Text put where an image was removed.
pub const IMAGE_OMITTED_PLACEHOLDER: &str = "[image omitted: this model does not accept images]";

/// Same wording the Anthropic path uses for a call that never got a result.
pub const INTERRUPTED_RESULT: &str = "Tool call produced no recorded result (interrupted).";

const MAX_TOOL_CALL_ID_LEN: usize = 64;

/// What the route being built can accept.
#[derive(Debug, Clone, Copy)]
pub struct TransformTarget<'a> {
    /// Model id the request names.
    pub model: &'a str,
    /// Whether earlier reasoning text may be sent back on this route.
    pub accepts_reasoning: bool,
    /// `Some(false)` only when the model is known to take no images; `None` when unknown (images stay).
    pub accepts_images: Option<bool>,
}

/// Apply every repair to `messages` and return the list to put on the wire.
pub fn transform_history(messages: Vec<Value>, target: &TransformTarget<'_>) -> Vec<Value> {
    let mut messages = messages;
    drop_foreign_reasoning(&mut messages, target);
    if target.accepts_images == Some(false) {
        for message in messages.iter_mut() {
            downgrade_images(message);
        }
    }
    for message in messages.iter_mut() {
        normalise_ids(message);
        if let Some(object) = message.as_object_mut() {
            // The origin tag is for this function only; no route accepts it.
            object.remove("origin");
        }
    }
    close_orphaned_calls(messages)
}

/// A tool-call id every known route accepts: non-empty, `[A-Za-z0-9_-]`, at most 64 characters.
pub fn is_acceptable_tool_call_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TOOL_CALL_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Deterministic id for an offending one; acceptable ids come back unchanged.
pub fn normalise_tool_call_id(id: &str) -> String {
    if is_acceptable_tool_call_id(id) {
        return id.to_string();
    }
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(id.as_bytes());
    let digest = hasher.finalize().to_string();
    let short: String = digest
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(24)
        .collect();
    format!("call_{short}")
}

/// Map every tool-call id in `messages` a route may reject (see [`is_acceptable_tool_call_id`]),
/// on the call and on its result alike. For routes that do not run the full [`transform_history`].
pub fn normalise_wire_ids(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        normalise_ids(message);
    }
}

fn normalise_ids(message: &mut Value) {
    if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for call in calls {
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                let mapped = normalise_tool_call_id(id);
                call["id"] = Value::String(mapped);
            }
        }
    }
    if let Some(id) = message.get("tool_call_id").and_then(Value::as_str) {
        let mapped = normalise_tool_call_id(id);
        message["tool_call_id"] = Value::String(mapped);
    }
}

/// Ids another vendor issues in a shape this route's models never produce (Anthropic `toolu_`).
fn has_foreign_call_id(message: &Value) -> bool {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| {
            calls.iter().any(|call| {
                call.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("toolu_") || id.starts_with("srvtoolu_"))
            })
        })
}

/// Reasoning rule. With an `origin` tag on the message (`{"provider","model"}`) the message keeps its
/// reasoning only when the model matches the target. Without a tag, reasoning stays (today's behaviour,
/// needed by routes that want it echoed on tool-call turns) except for assistant turns at or before the
/// last turn whose tool-call ids are in another vendor's shape: those predate a vendor switch.
fn drop_foreign_reasoning(messages: &mut [Value], target: &TransformTarget<'_>) {
    let last_foreign = messages.iter().rposition(|m| {
        m.get("role").and_then(Value::as_str) == Some("assistant") && has_foreign_call_id(m)
    });
    for (index, message) in messages.iter_mut().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let keep = if !target.accepts_reasoning {
            false
        } else if let Some(origin_model) = message
            .get("origin")
            .and_then(|o| o.get("model"))
            .and_then(Value::as_str)
        {
            origin_model == target.model
        } else {
            last_foreign.is_none_or(|last| index > last)
        };
        if !keep {
            if let Some(object) = message.as_object_mut() {
                object.remove("reasoning");
                object.remove("reasoning_content");
            }
        }
    }
}

fn is_image_part(part: &Value) -> bool {
    matches!(
        part.get("type").and_then(Value::as_str),
        Some("image_url") | Some("image") | Some("input_image")
    )
}

fn downgrade_images(message: &mut Value) {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    if role != "user" && role != "tool" {
        return;
    }
    let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    if !parts.iter().any(is_image_part) {
        return;
    }
    let mut out: Vec<Value> = Vec::with_capacity(parts.len());
    let mut last_was_placeholder = false;
    for part in parts.drain(..) {
        if is_image_part(&part) {
            if !last_was_placeholder {
                out.push(json!({"type": "text", "text": IMAGE_OMITTED_PLACEHOLDER}));
            }
            last_was_placeholder = true;
        } else {
            last_was_placeholder =
                part.get("text").and_then(Value::as_str) == Some(IMAGE_OMITTED_PLACEHOLDER);
            out.push(part);
        }
    }
    *parts = out;
}

fn role_of(message: &Value) -> &str {
    message.get("role").and_then(Value::as_str).unwrap_or("")
}

/// For each assistant turn with tool calls, answer every call that has no result before the next
/// assistant or user turn. System messages are transparent. The synthetic result goes after the last
/// existing result of the span (or straight after the assistant turn), so order is preserved.
fn close_orphaned_calls(messages: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        out.push(message.clone());
        index += 1;
        if role_of(message) != "assistant" {
            continue;
        }
        let call_ids: Vec<String> = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if call_ids.is_empty() {
            continue;
        }
        // Span of the following messages that can still answer these calls.
        let mut end = index;
        let mut answered: Vec<&str> = Vec::new();
        let mut last_tool = None;
        while end < messages.len() {
            match role_of(&messages[end]) {
                "assistant" | "user" => break,
                "tool" => {
                    if let Some(id) = messages[end].get("tool_call_id").and_then(Value::as_str) {
                        answered.push(id);
                    }
                    last_tool = Some(end);
                }
                _ => {}
            }
            end += 1;
        }
        let missing: Vec<&String> = call_ids
            .iter()
            .filter(|id| !answered.contains(&id.as_str()))
            .collect();
        let synthetic = |id: &str| {
            json!({
                "role": "tool",
                "tool_call_id": id,
                "content": INTERRUPTED_RESULT,
                "is_error": true,
            })
        };
        match last_tool {
            // Copy the existing results through the last one, then the synthetic ones.
            Some(last) => {
                while index <= last {
                    out.push(messages[index].clone());
                    index += 1;
                }
                out.extend(missing.iter().map(|id| synthetic(id)));
            }
            None => out.extend(missing.iter().map(|id| synthetic(id))),
        }
    }
    out
}
