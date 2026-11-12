//! Host adapters: Claude Code, Codex & CodeRight absorbed-stream rows.
//!
//! Each adapter takes a single JSONL row (`serde_json::Value`) and returns the
//! normalized [`RawEvent`]s found inside it. All hosts share one shape so
//! downstream projections ingest them identically.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::canonical::py_json_dumps;

static CODEX_CONTROL_ENVELOPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<(?:subagent_notification|codex_internal_context|codex_delegation|turn_aborted|heartbeat)(?:\s[^>]*)?>.*?</(?:subagent_notification|codex_internal_context|codex_delegation|turn_aborted|heartbeat)>",
    )
    .expect("valid Codex control-envelope regex")
});
static CODEX_IMAGE_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?image\b[^>]*>?").expect("valid Codex image-marker regex"));

const INJECTED_CONTEXT_PREFIXES: &[&str] = &[
    "# AGENTS.md instructions",
    "# CLAUDE.md instructions",
    "# Context from my IDE setup:",
    "# Files mentioned by the user",
    "<environment_context",
    "<in-app-browser-context",
    "<permissions instructions",
    "<recommended_plugins",
    "<summary>",
    "<command-name",
    "<command-message",
    "<local-command-stdout",
    "<task-notification",
    "<launch-selected-element",
    "[Request interrupted",
];

fn injected_context(text: &str) -> bool {
    let text = text.trim_start();
    INJECTED_CONTEXT_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn codex_user_text(text: String) -> String {
    let text = CODEX_CONTROL_ENVELOPE.replace_all(&text, "");
    CODEX_IMAGE_MARKER.replace_all(&text, "").trim().to_owned()
}

/// A normalized pre-canonicalization event emitted by a host adapter.
#[derive(Debug, Clone, Default)]
pub struct RawEvent {
    pub kind: String,
    pub role: Option<String>,
    pub timestamp: Option<String>,
    pub text: String,
    pub tool: Option<String>,
    pub call_id: Option<String>,
    pub is_error: bool,
    pub synthetic: bool,
    pub meta: bool,
    pub private_reasoning_omitted: bool,
    /// Explicit redaction override; `None` derives from redaction markers.
    pub redacted: Option<bool>,
    pub is_sidechain: Option<bool>,
    // Optional thread/scope passthrough (generic frozen snapshots).
    pub agent_role: Option<String>,
    pub thread_source: Option<String>,
    pub parent_thread_id: Option<String>,
    pub cwd: Option<String>,
    pub repo: Option<String>,
    pub session_id: Option<String>,
}

impl RawEvent {
    fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            ..Default::default()
        }
    }
}

fn as_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

// ---- Claude Code adapter ----

/// Claude Code rows: `{"type":"user"|"assistant","message":{...}}` with
/// `message.content` either a string or a block list (`text`, `thinking`,
/// `tool_use` on assistant, `tool_result` on user rows).
pub fn claude_events(obj: &Value) -> Vec<RawEvent> {
    let row_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let timestamp = as_str(obj.get("timestamp"));

    if row_type != "user" && row_type != "assistant" {
        if matches!(
            row_type,
            "queue-operation" | "file-history-snapshot" | "summary"
        ) {
            let mut ev = RawEvent::new("meta");
            ev.timestamp = timestamp;
            ev.text = obj
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(row_type)
                .to_string();
            ev.meta = true;
            ev.synthetic = true;
            return vec![ev];
        }
        return Vec::new();
    }

    if obj
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Vec::new();
    }
    if row_type == "user"
        && (obj.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
            || obj
                .get("isCompactSummary")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || obj.get("toolUseResult").is_some()
            || obj
                .get("origin")
                .and_then(Value::as_object)
                .is_some_and(|origin| origin.get("kind").and_then(Value::as_str) != Some("human"))
            || (obj.get("promptSource").and_then(Value::as_str) == Some("sdk")
                && obj.get("entrypoint").and_then(Value::as_str) == Some("sdk-cli")))
    {
        return Vec::new();
    }

    let Some(message) = obj.get("message").filter(|m| m.is_object()) else {
        return Vec::new();
    };
    let Some(content) = message.get("content") else {
        return Vec::new();
    };

    let mut events = Vec::new();

    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let mut ev = RawEvent::new(&format!("{row_type}_message"));
            ev.role = Some(row_type.to_string());
            ev.timestamp = timestamp;
            ev.text = trimmed.to_string();
            if row_type == "user" && injected_context(&ev.text) {
                return Vec::new();
            }
            events.push(ev);
        }
        return events;
    }

    let Some(blocks) = content.as_array() else {
        return events;
    };

    for block in blocks {
        let Some(block) = block.as_object() else {
            continue;
        };
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    let mut ev = RawEvent::new(&format!("{row_type}_message"));
                    ev.role = Some(row_type.to_string());
                    ev.timestamp = timestamp.clone();
                    ev.text = text.to_string();
                    if row_type != "user" || !injected_context(&ev.text) {
                        events.push(ev);
                    }
                }
            }
            "thinking" => {
                let mut ev = RawEvent::new("thinking");
                ev.role = Some(row_type.to_string());
                ev.timestamp = timestamp.clone();
                ev.private_reasoning_omitted = true;
                ev.meta = true;
                events.push(ev);
            }
            "tool_use" if row_type == "assistant" => {
                let mut ev = RawEvent::new("tool_call");
                ev.role = Some("assistant".to_string());
                ev.timestamp = timestamp.clone();
                ev.tool = Some(
                    block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                );
                ev.call_id = as_str(block.get("id"));
                let empty_input: Value = Value::Object(serde_json::Map::new());
                ev.text = py_json_dumps(block.get("input").unwrap_or(&empty_input));
                events.push(ev);
            }
            "tool_result" if row_type == "user" => {
                let content_value = match block.get("content") {
                    None | Some(Value::Null) => Value::String(String::new()),
                    Some(v @ Value::String(_)) => v.clone(),
                    Some(other) => Value::String(py_json_dumps(other)),
                };
                let mut ev = RawEvent::new("tool_result");
                ev.role = Some("user".to_string());
                ev.timestamp = timestamp.clone();
                ev.call_id = as_str(block.get("tool_use_id"));
                ev.text = content_value.as_str().unwrap_or_default().to_string();
                ev.is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                events.push(ev);
            }
            _ => {}
        }
    }
    events
}

// ---- Codex adapter ----

fn codex_text_blocks(content: &Value, allowed: &[&str]) -> Vec<String> {
    if let Some(s) = content.as_str() {
        return vec![s.to_string()];
    }
    let Some(items) = content.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|b| {
            let b = b.as_object()?;
            if !allowed.contains(&b.get("type").and_then(Value::as_str)?) {
                return None;
            }
            b.get("text").and_then(Value::as_str).map(str::to_string)
        })
        .collect()
}

/// Codex rows: `{"type":"response_item","timestamp":"...","payload":{...}}`
/// with payload types `message`, `function_call`/`custom_tool_call`,
/// `function_call_output`/`custom_tool_call_output`, `reasoning` (private,
/// omitted), and `event_msg`.
pub fn codex_events(obj: &Value) -> Vec<RawEvent> {
    if obj.get("type").and_then(Value::as_str) != Some("response_item") {
        return Vec::new();
    }
    let Some(payload) = obj.get("payload").filter(|p| p.is_object()) else {
        return Vec::new();
    };
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let timestamp = as_str(obj.get("timestamp"));

    match kind {
        "message" => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            if role != "user" && role != "assistant" {
                return Vec::new();
            }
            let blocks = codex_text_blocks(
                payload.get("content").unwrap_or(&Value::Null),
                &["input_text", "output_text", "text"],
            );
            let text = blocks.join("\n");
            let text = if role == "user" {
                codex_user_text(text)
            } else {
                text
            };
            if text.trim().is_empty() {
                return Vec::new();
            }
            if role == "user" && injected_context(&text) {
                return Vec::new();
            }
            let mut ev = RawEvent::new(&format!("{role}_message"));
            ev.role = Some(role.to_string());
            ev.timestamp = timestamp;
            ev.text = text;
            vec![ev]
        }
        "function_call" | "custom_tool_call" => {
            let name = payload
                .get("name")
                .or_else(|| payload.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let mut ev = RawEvent::new("tool_call");
            ev.role = Some("assistant".to_string());
            ev.timestamp = timestamp;
            ev.tool = Some(name.to_string());
            ev.call_id = as_str(payload.get("call_id"));
            // Python used str(arguments): strings pass through verbatim; any
            // other JSON shape serializes canonically here (deterministic).
            ev.text = match &arguments {
                Value::String(s) => s.clone(),
                other => py_json_dumps(other),
            };
            vec![ev]
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = payload
                .get("output")
                .or_else(|| payload.get("content"))
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let mut ev = RawEvent::new("tool_result");
            ev.role = Some("user".to_string());
            ev.timestamp = timestamp;
            ev.call_id = as_str(payload.get("call_id"));
            ev.text = match &output {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                // Deterministic canonical serialization (Python used str(),
                // whose dict repr is not reproducible across runtimes).
                other => py_json_dumps(other),
            };
            ev.is_error = payload
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            vec![ev]
        }
        "reasoning" => {
            let mut ev = RawEvent::new("thinking");
            ev.role = Some("assistant".to_string());
            ev.timestamp = timestamp;
            ev.private_reasoning_omitted = true;
            ev.meta = true;
            vec![ev]
        }
        "event_msg" => {
            let mut ev = RawEvent::new("meta");
            ev.timestamp = timestamp;
            ev.text = payload
                .get("type")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("event_msg")
                .to_string();
            ev.meta = true;
            ev.synthetic = true;
            vec![ev]
        }
        _ => Vec::new(),
    }
}

// ---- Absorbed-stream adapter (an agent server's exported session events) ----

const GENERIC_KINDS: &[&str] = &[
    "user_message",
    "assistant_message",
    "tool_call",
    "tool_result",
    "thinking",
    "meta",
];

/// The row type an agent server's exporter writes for `host` (`<host>_event_v1`), or `None` when
/// `host` is not a plain lowercase name (letters, digits, `_` and `-`), so no row matches.
pub fn host_event_type(host: &str) -> Option<String> {
    let plain = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    plain.then(|| format!("{host}_event_v1"))
}

/// Reads one already-normalized event from a CodeRight runtime export row:
/// `{"type":"coderight_event_v1","host":"coderight",…}`. The same as
/// [`host_events`] for the host `coderight`.
pub fn coderight_events(obj: &Value) -> Vec<RawEvent> {
    host_events("coderight", obj)
}

/// Reads one already-normalized event from an agent server's runtime export row for `host`:
/// `{"type":"<host>_event_v1","host":"<host>","sessionId":"…",
/// "sourceSeq":N,"eventId":"…","occurredAt":"…","event":{…}}` (the exporter names the row
/// after the app that hosts the agent server). A row of another host, or of no host, yields
/// nothing.
///
/// These rows never arrive through file transcripts — they are produced by the agent server's
/// durable-event exporter and consumed from the absorbed store — so the row carries canonical
/// identity the generic `adapt_event_v1` shape lacks (`sessionId`/`sourceSeq`/`eventId` at
/// row level).
pub fn host_events(host: &str, obj: &Value) -> Vec<RawEvent> {
    let Some(row_type) = host_event_type(host) else {
        return Vec::new();
    };
    if obj.get("type").and_then(Value::as_str) != Some(row_type.as_str()) {
        return Vec::new();
    }
    if obj.get("host").and_then(Value::as_str) != Some(host) {
        return Vec::new();
    }
    let Some(event) = obj.get("event").filter(|e| e.is_object()) else {
        return Vec::new();
    };
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    if !GENERIC_KINDS.contains(&kind) {
        return Vec::new();
    }

    let mut ev = RawEvent::new(kind);
    ev.role = as_str(event.get("role"));
    ev.timestamp = as_str(event.get("timestamp"));
    ev.text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    ev.tool = as_str(event.get("tool"));
    ev.call_id = as_str(event.get("call_id"));
    ev.is_error = event
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ev.synthetic = event
        .get("synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ev.meta = event.get("meta").and_then(Value::as_bool).unwrap_or(false);
    ev.private_reasoning_omitted = event
        .get("private_reasoning_omitted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ev.redacted = event.get("redacted").and_then(Value::as_bool);
    ev.is_sidechain = event.get("is_sidechain").and_then(Value::as_bool);
    ev.agent_role = as_str(event.get("agentRole"));
    ev.thread_source = as_str(event.get("threadSource"));
    ev.parent_thread_id = as_str(event.get("parentThreadId"));
    ev.cwd = as_str(event.get("cwd"));
    ev.repo = as_str(event.get("repo"));
    // Row-level identity: the exporter binds the session on the row so a
    // store replay cannot silently attribute the event to another session.
    ev.session_id = as_str(event.get("sessionId")).or_else(|| as_str(obj.get("sessionId")));
    vec![ev]
}

// ---- Host detection ----

/// Detect the host by scanning the first (up to) 20 rows: Codex rows carry
/// `type` in `{session_meta, response_item, event_msg}` with a nested
/// `payload`; Claude Code rows carry `type` in `{user, assistant,
/// queue-operation}`. Falls back to `claude_code`.
pub fn detect_host(path: &Path) -> std::io::Result<&'static str> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    for line in std::io::BufRead::lines(reader).take(20) {
        let line = line?;
        let Ok(obj) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(map) = obj.as_object() else { continue };
        match map.get("type").and_then(Value::as_str) {
            Some("session_meta" | "response_item" | "event_msg") => {
                return Ok("codex");
            }
            Some("user" | "assistant" | "queue-operation") => {
                return Ok("claude_code");
            }
            _ => {}
        }
    }
    Ok("claude_code")
}

/// Public dispatch: route one decoded row through the right adapter.
/// `coderight_event_v1` rows always take the native path regardless of the
/// caller's host label (the row self-declares and self-verifies), and so does a
/// `<host>_event_v1` row of the caller's own `host` (another app's agent server).
pub fn iter_events_for_host(host: &str, obj: &Value) -> Vec<RawEvent> {
    let row_type = obj.get("type").and_then(Value::as_str);
    if row_type == Some("coderight_event_v1") {
        return coderight_events(obj);
    }
    if row_type.is_some() && row_type.map(str::to_string) == host_event_type(host) {
        return host_events(host, obj);
    }
    match host {
        "claude_code" => claude_events(obj),
        "codex" => codex_events(obj),
        _ => Vec::new(),
    }
}
