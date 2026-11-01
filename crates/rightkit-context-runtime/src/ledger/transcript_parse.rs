//! Provider history parsers for the Ledger transcript lane (pure: no storage, no filesystem).
//!
//! Two authorized sources are understood, and nothing else:
//!
//! * Codex rollout files (`rollout-*.jsonl`): one JSON object per line, `{timestamp, ordinal,
//!   type, payload}`. `session_meta` names the conversation; `response_item` rows carry
//!   `message`, `function_call`/`custom_tool_call` and their outputs. Format taken from
//!   `membrane-transcript` (`adapters::codex_events`) and from reading the key names of local
//!   rollouts (keys only, never content).
//! * A ChatGPT data export (`conversations*.json`): an array of conversations, each with a
//!   `mapping` of node id to `{id, message, parent, children}` and a `current_node`. The mapping is
//!   a tree: every node is kept (edited and regenerated answers are sibling branches) and the
//!   nodes on the path to `current_node` are flagged. Attachment parts become references
//!   (`[attachment: <pointer>]`), never bytes.
//!
//! Privacy is applied here, before anything reaches storage: secret shapes are redacted
//! (`rightkit_agent_transcript::redact`), private reasoning, system/developer instructions, injected
//! context, `base_instructions`, account ids and absolute paths are never read into a row.
use rightkit_agent_transcript::redact::redact;
use rightkit_fs::digest::sha256_bytes;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const PARSER_VERSION: &str = "ledger.transcript-parse.v1";
/// Longest stored text of a user or assistant message; the rest is cut and flagged.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Longest stored text of a tool call or result.
pub const MAX_TOOL_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct ParsedMessage {
    pub provider_msg_id: String,
    pub parent_msg_id: Option<String>,
    /// `user`, `assistant` or `tool`.
    pub role: &'static str,
    /// `text`, `tool_call` or `tool_result`. Only `text` is searchable.
    pub kind: &'static str,
    pub ordinal: i64,
    pub created_ms: i64,
    /// Redacted and bounded.
    pub text: String,
    pub truncated: bool,
    /// ChatGPT: the node lies on the path to `current_node`. Codex: always true.
    pub on_current_path: bool,
    /// SHA-256 of the exact source bytes this message was read from (a Codex line, or the
    /// canonical JSON of the ChatGPT message object).
    pub raw_sha256: String,
    pub byte_start: i64,
    pub byte_end: i64,
    /// Where in the source file the message lives: `line:<n>` or `<conversation>/<node>`.
    pub locator: String,
}

impl ParsedMessage {
    /// Revision identity: equal text under one message id is one revision; changed text is a new
    /// one. Never derived from the id, so edits stay distinct and equal text in two messages
    /// never merges them (the message key carries the identity).
    pub fn revision(&self) -> String {
        sha256_bytes(format!("{}\0{}\0{}", self.role, self.kind, self.text).as_bytes())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub branch: Option<String>,
    /// Last path component of the working folder, never the full path.
    pub workspace: Option<String>,
    pub parent_conv_id: Option<String>,
    pub current_msg_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Skipped {
    pub reasoning: usize,
    pub instructions: usize,
    pub hidden: usize,
    pub unsupported: usize,
    pub bad_lines: usize,
}

impl Skipped {
    pub fn add(&mut self, other: &Skipped) {
        self.reasoning += other.reasoning;
        self.instructions += other.instructions;
        self.hidden += other.hidden;
        self.unsupported += other.unsupported;
        self.bad_lines += other.bad_lines;
    }
}

/// Redact, strip NUL, trim and bound. Returns the text and whether it was cut.
pub fn clean_text(raw: &str, limit: usize) -> (String, bool) {
    let cleaned = raw.replace('\0', "");
    let redacted = redact(&cleaned);
    let trimmed = redacted.trim();
    if trimmed.len() <= limit {
        return (trimmed.to_string(), false);
    }
    let mut end = limit;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    (trimmed[..end].to_string(), true)
}

/// Unix milliseconds of an RFC 3339 timestamp, 0 when absent or malformed.
fn rfc3339_ms(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_str)
        .and_then(rightkit_time::DateTime::parse_rfc3339)
        .map(|at| at.unix_seconds().saturating_mul(1000))
        .unwrap_or(0)
}

/// Unix milliseconds of ChatGPT's float-seconds timestamps.
fn float_secs_ms(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_f64)
        .filter(|secs| secs.is_finite() && *secs >= 0.0)
        .map(|secs| (secs * 1000.0) as i64)
        .unwrap_or(0)
}

fn last_component(path: &str) -> Option<String> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|part| !part.is_empty())
        .map(|part| part.chars().take(120).collect())
}

// ---- Codex ----

pub enum CodexLine {
    Meta(ConversationMeta),
    Message(ParsedMessage),
    Skip,
}

/// The conversation id a rollout file name carries: `rollout-<timestamp>-<uuid>.jsonl`.
pub fn codex_id_from_file_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".jsonl")?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    (id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        .then(|| id.to_string())
}

fn text_blocks(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => {
                    block.get("text").and_then(Value::as_str)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Context the Codex CLI injects into a user turn; not something the user wrote.
fn injected(text: &str) -> bool {
    let head = text.trim_start();
    head.starts_with("<environment_context>")
        || head.starts_with("<user_instructions>")
        || head.starts_with("<permissions instructions>")
        || head.starts_with("# AGENTS.md instructions")
        || head.starts_with("<INSTRUCTIONS>")
}

/// Parse one complete rollout line. `line` excludes the newline; `byte_start` is its offset in the
/// file and `line_no` its 1-based number. Malformed JSON counts in `skipped.bad_lines`.
pub fn codex_line(line: &[u8], byte_start: i64, line_no: i64, skipped: &mut Skipped) -> CodexLine {
    let Ok(row) = serde_json::from_slice::<Value>(line) else {
        skipped.bad_lines += 1;
        return CodexLine::Skip;
    };
    let Some(payload) = row.get("payload").filter(|p| p.is_object()) else {
        return CodexLine::Skip;
    };
    let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "session_meta" {
        // Only these keys are read: never `base_instructions`, `creator_*`, `cwd` beyond its last
        // component, or `runtime_workspace_roots`.
        let id = payload
            .get("id")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let git = payload.get("git");
        return CodexLine::Meta(ConversationMeta {
            id,
            title: String::new(),
            created_ms: rfc3339_ms(payload.get("timestamp").or_else(|| row.get("timestamp"))),
            updated_ms: 0,
            branch: git
                .and_then(|g| g.get("branch"))
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty())
                .map(|b| b.chars().take(200).collect()),
            workspace: payload
                .get("cwd")
                .and_then(Value::as_str)
                .and_then(last_component),
            parent_conv_id: payload
                .get("forked_from_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            current_msg_id: None,
        });
    }
    if kind != "response_item" {
        return CodexLine::Skip;
    }
    let ordinal = row
        .get("ordinal")
        .and_then(Value::as_i64)
        .unwrap_or(line_no);
    let created_ms = rfc3339_ms(row.get("timestamp"));
    let raw_sha256 = sha256_bytes(line);
    let byte_end = byte_start + line.len() as i64;
    let locator = format!("line:{line_no}");
    let own_id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let build = |id: String, role: &'static str, kind: &'static str, raw: &str, limit: usize| {
        let (text, truncated) = clean_text(raw, limit);
        ParsedMessage {
            provider_msg_id: id,
            parent_msg_id: None,
            role,
            kind,
            ordinal,
            created_ms,
            text,
            truncated,
            on_current_path: true,
            raw_sha256: raw_sha256.clone(),
            byte_start,
            byte_end,
            locator: locator.clone(),
        }
    };
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let role = match payload.get("role").and_then(Value::as_str) {
                Some("user") => "user",
                Some("assistant") => "assistant",
                // developer and system turns are instructions, not conversation.
                _ => {
                    skipped.instructions += 1;
                    return CodexLine::Skip;
                }
            };
            let raw = text_blocks(payload.get("content").unwrap_or(&Value::Null));
            if raw.trim().is_empty() {
                return CodexLine::Skip;
            }
            if role == "user" && injected(&raw) {
                skipped.instructions += 1;
                return CodexLine::Skip;
            }
            let id = own_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("ord:{ordinal}"));
            let message = build(id, role, "text", &raw, MAX_TEXT_BYTES);
            if message.text.is_empty() {
                return CodexLine::Skip;
            }
            CodexLine::Message(message)
        }
        "function_call" | "custom_tool_call" => {
            let name = payload
                .get("name")
                .or_else(|| payload.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let args = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .map(|c| format!("{c}:call"))
                .unwrap_or_else(|| format!("ord:{ordinal}"));
            CodexLine::Message(build(
                id,
                "assistant",
                "tool_call",
                &format!("{name}\n{args}"),
                MAX_TOOL_BYTES,
            ))
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = payload
                .get("output")
                .or_else(|| payload.get("content"))
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .map(|c| format!("{c}:out"))
                .unwrap_or_else(|| format!("ord:{ordinal}"));
            CodexLine::Message(build(id, "tool", "tool_result", &output, MAX_TOOL_BYTES))
        }
        "reasoning" => {
            skipped.reasoning += 1;
            CodexLine::Skip
        }
        _ => CodexLine::Skip,
    }
}

// ---- ChatGPT export ----

/// One conversation of a ChatGPT export. `None` when the object has no usable conversation id.
pub fn chatgpt_conversation(
    conversation: &Value,
    skipped: &mut Skipped,
) -> Option<(ConversationMeta, Vec<ParsedMessage>)> {
    let id = conversation
        .get("conversation_id")
        .or_else(|| conversation.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_string();
    let mapping = conversation.get("mapping").and_then(Value::as_object)?;
    let current = conversation
        .get("current_node")
        .and_then(Value::as_str)
        .map(str::to_string);
    // Nodes on the path from `current_node` to the root.
    let mut on_path = BTreeSet::new();
    let mut cursor = current.clone();
    while let Some(node) = cursor {
        if !on_path.insert(node.clone()) {
            break;
        }
        cursor = mapping
            .get(&node)
            .and_then(|n| n.get("parent"))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    // Depth-first order from the roots, children in their listed order, so ordinals are stable
    // for an unchanged tree and siblings (edits, regenerations) sit next to each other.
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut roots: Vec<&String> = mapping
        .iter()
        .filter(|(_, node)| node.get("parent").is_none_or(Value::is_null))
        .map(|(key, _)| key)
        .collect();
    roots.sort();
    let mut stack: Vec<String> = roots.into_iter().rev().cloned().collect();
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        order.push(node.clone());
        if let Some(children) = mapping
            .get(&node)
            .and_then(|n| n.get("children"))
            .and_then(Value::as_array)
        {
            for child in children.iter().rev().filter_map(Value::as_str) {
                stack.push(child.to_string());
            }
        }
    }
    // A node unreachable from a root (a dangling parent) is still a message: append it.
    for key in mapping.keys() {
        if seen.insert(key.clone()) {
            order.push(key.clone());
        }
    }
    let mut messages = Vec::new();
    let mut by_node: BTreeMap<String, usize> = BTreeMap::new();
    for (ordinal, node_id) in order.iter().enumerate() {
        let node = &mapping[node_id];
        let Some(message) = node.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let role = match message
            .get("author")
            .and_then(|a| a.get("role"))
            .and_then(Value::as_str)
        {
            Some("user") => "user",
            Some("assistant") => "assistant",
            Some("tool") => "tool",
            _ => {
                skipped.instructions += 1;
                continue;
            }
        };
        let metadata = message.get("metadata");
        if metadata
            .and_then(|m| m.get("is_visually_hidden_from_conversation"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            skipped.hidden += 1;
            continue;
        }
        let content = message.get("content").unwrap_or(&Value::Null);
        let content_type = content
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let (kind, raw) = match content_type {
            "text" | "multimodal_text" => {
                let parts = content
                    .get("parts")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| match part {
                                Value::String(text) => Some(text.clone()),
                                Value::Object(_) => {
                                    if let Some(pointer) =
                                        part.get("asset_pointer").and_then(Value::as_str)
                                    {
                                        Some(format!("[attachment: {pointer}]"))
                                    } else {
                                        part.get("text").and_then(Value::as_str).map(str::to_string)
                                    }
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                (
                    if role == "tool" {
                        "tool_result"
                    } else {
                        "text"
                    },
                    parts,
                )
            }
            "code" => (
                "tool_call",
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            "execution_output" => (
                "tool_result",
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            "thoughts" | "reasoning_recap" => {
                skipped.reasoning += 1;
                continue;
            }
            "user_editable_context" | "system_error" => {
                skipped.instructions += 1;
                continue;
            }
            _ => {
                skipped.unsupported += 1;
                continue;
            }
        };
        if raw.trim().is_empty() {
            continue;
        }
        let limit = if kind == "text" {
            MAX_TEXT_BYTES
        } else {
            MAX_TOOL_BYTES
        };
        let (text, truncated) = clean_text(&raw, limit);
        if text.is_empty() {
            continue;
        }
        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .unwrap_or(node_id)
            .to_string();
        by_node.insert(node_id.clone(), messages.len());
        messages.push(ParsedMessage {
            provider_msg_id: message_id,
            parent_msg_id: node
                .get("parent")
                .and_then(Value::as_str)
                .map(str::to_string),
            role,
            kind,
            ordinal: ordinal as i64,
            created_ms: float_secs_ms(message.get("create_time")),
            text,
            truncated,
            on_current_path: on_path.contains(node_id),
            raw_sha256: sha256_bytes(message.to_string().as_bytes()),
            byte_start: 0,
            byte_end: 0,
            locator: format!("{id}/{node_id}"),
        });
    }
    let current_msg_id = current
        .as_deref()
        .and_then(|node| by_node.get(node))
        .map(|index| messages[*index].provider_msg_id.clone());
    let meta = ConversationMeta {
        id,
        title: conversation
            .get("title")
            .and_then(Value::as_str)
            .map(|t| clean_text(t, 300).0)
            .unwrap_or_default(),
        created_ms: float_secs_ms(conversation.get("create_time")),
        updated_ms: float_secs_ms(conversation.get("update_time")),
        branch: None,
        workspace: None,
        parent_conv_id: None,
        current_msg_id,
    };
    Some((meta, messages))
}

/// Searchable chunks of a message text: at most ~4000 bytes each, cut at whitespace where one
/// exists, always at a character boundary. Returns `(chunk index, byte offset, text)`.
pub fn chunks(text: &str) -> Vec<(usize, usize, &str)> {
    const TARGET: usize = 4000;
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + TARGET).min(text.len());
        if end < text.len() {
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if let Some(space) = text[start..end].rfind(char::is_whitespace) {
                if space > TARGET / 2 {
                    end = start + space;
                }
            }
        }
        if end == start {
            end = (start + 1).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
        }
        out.push((out.len(), start, &text[start..end]));
        start = end;
    }
    out
}
