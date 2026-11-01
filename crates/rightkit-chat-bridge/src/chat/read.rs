//! `<cli> chat read`: the user and assistant turns of one chat on THIS computer, read from the
//! chat's own transcript. It is the transcript half of the remote `chat_read` tool: the caller
//! (`remote::chat`) has already checked the remote switch and that the chat is selected in
//! Settings; this module only finds the transcript and cuts it into turns.
//!
//! It reuses the transcript readers of `said.rs` (the same files, the same cleaning of text a
//! program put in the user's turn, the same newest-first scan with a byte cap) and adds the
//! assistant side:
//! * Claude: a line with `type == "assistant"` and `message.role == "assistant"`, not
//!   `isSidechain`, `isMeta` or `isApiErrorMessage`. Only its `text` parts count; thinking,
//!   `tool_use` and tool results are never returned.
//! * Codex: `event_msg` with payload type `item_completed` and `item.type == "AgentMessage"`
//!   (text parts, whose `type` is `Text` in current rollouts), or from older Codex `event_msg`
//!   `agent_message` (text in `payload.message`). Only when the searched part has no `event_msg`
//!   turns at all, `response_item` messages with `role == "assistant"` are used instead.
//!
//! * Muse: a `runtime.session` record whose `payload.kind == "run"` and `payload.event.kind ==
//!   "assistant_message_committed"` (text in `payload.event.text`), or `run.terminal.completed`
//!   (text in `payload.text`). The user side is `said::muse_line`. Checked against Muse Code 1.4.11
//!   session files on 2026-10-21 (the `run.terminal.completed` form was not among them).
//!
//! Consecutive assistant blocks with no user turn between them (a Claude transcript writes one
//! line per content block, and a turn with tool calls has several) are one turn.
//!
//! Nothing is written, copied or kept.

use super::harness;
use super::roster::iso_epoch;
use super::said::{
    self, claude_message, claude_transcript, codex_message, codex_rollout, has, muse_message,
    muse_stamp, muse_transcript, strip_reminders, valid_id,
};
use super::store::Store;
use super::tail;
use serde_json::Value;
use std::path::PathBuf;

/// Longest assistant turn kept, in characters, after its blocks are joined; a longer one ends
/// in `TRUNCATED`. A user turn is cut to `said::MAX_CHARS` by the reader it comes from.
const ASSISTANT_CHARS: usize = 6000;
const TRUNCATED: &str = "…(truncated)";
/// Most `response_item` fallback entries kept per `read` call.
const FALLBACK_ITEMS: usize = 400;

/// The turns of the transcripts `read_turns` and `read_registered` have read, per path (see `tail`).
/// A turn comes with whether it is a primary one (Codex `event_msg`; Claude and Muse are all primary).
static INDEX: tail::Index<(Turn, bool)> = tail::new_index();

fn weigh(item: &(Turn, bool)) -> usize {
    128 + item.0.text.len() + item.0.ts.as_ref().map_or(0, String::len)
}

/// Who spoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One turn of a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    /// The transcript's own timestamp text (ISO 8601, UTC), when it has one.
    pub ts: Option<String>,
    pub ts_ms: Option<u64>,
    pub text: String,
    /// The text was cut to its character limit.
    pub truncated: bool,
}

/// The turns found, oldest first.
#[derive(Clone, Debug)]
pub struct Turns {
    /// "claude", "codex" or "muse".
    pub kind: String,
    pub turns: Vec<Turn>,
    /// The scan stopped at its byte cap before it found as many turns as asked for.
    pub scan_cut: bool,
}

/// Why nothing could be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The id is not a chat id (letters, digits, `-` and `_`), or the kind is unknown.
    Invalid(String),
    /// No transcript for that chat on this computer.
    NotFound,
    /// The transcript exists but could not be read.
    Failed(String),
}

/// Which kind of transcript holds `id` here, trying Claude's, then Codex's, then Muse's unless
/// `kind` names one (Muse's only while its switch is on). The transcript must be a regular file,
/// not a link.
pub fn locate(kind: Option<&str>, id: &str) -> Result<(String, PathBuf), ReadError> {
    if !valid_id(id) {
        return Err(ReadError::Invalid(
            "that is not a chat id (letters, digits, - and _ only)".into(),
        ));
    }
    let kinds: &[&str] = match kind {
        Some("claude") => &["claude"],
        Some("codex") => &["codex"],
        Some("muse") => &["muse"],
        None => &["claude", "codex", "muse"],
        Some(other) if super::codex_hook::valid_hook_harness(other) => {
            // A registered harness: its transcript is read from its record's folder.
            let found = Store::open_default()
                .ok()
                .filter(|store| store.harness_chats(other))
                .and_then(|store| harness::transcript_path(&store, other, id));
            return match found {
                Some(path)
                    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) =>
                {
                    Ok((other.to_string(), path))
                }
                _ => Err(ReadError::NotFound),
            };
        }
        Some(other) => {
            return Err(ReadError::Invalid(format!(
                "unknown chat kind \"{other}\"; use claude, codex or muse"
            )));
        }
    };
    let muse_on = super::store::Store::open_default().is_ok_and(|store| store.muse_chats());
    for kind in kinds {
        let path = match *kind {
            "claude" => claude_transcript(id),
            "codex" => codex_rollout(id),
            _ if muse_on => muse_transcript(id),
            _ => None,
        };
        if let Some(path) = path
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
        {
            return Ok((kind.to_string(), path));
        }
    }
    // With no kind named, a registered harness's chat is found last.
    if kind.is_none()
        && let Ok(store) = Store::open_default()
    {
        for other in harness::kinds(&store) {
            if store.harness_chats(&other)
                && let Some(path) = harness::transcript_path(&store, &other, id)
                && std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
            {
                return Ok((other, path));
            }
        }
    }
    Err(ReadError::NotFound)
}

/// The last `limit` turns of the transcript at `path`, oldest first.
pub fn read_turns(kind: &str, path: &std::path::Path, limit: usize) -> Result<Turns, ReadError> {
    if !matches!(kind, "claude" | "codex" | "muse") {
        return read_registered(kind, path, limit.max(1));
    }
    let limit = limit.max(1);
    // Newest first. `primary` holds the turns the transcript's own turn events gave;
    // `fallback` the Codex `response_item` form, used only when `primary` stays empty.
    let mut primary: Vec<Turn> = Vec::new();
    let mut fallback: Vec<Turn> = Vec::new();
    let mut turns_seen = 0usize;
    let parse = |line: &[u8]| -> Option<(Turn, bool)> {
        match kind {
            "muse" => muse_turn(line).map(|turn| (turn, true)),
            "claude" => claude_turn(line).map(|turn| (turn, true)),
            _ => codex_turn(line),
        }
    };
    let cut = tail::scan_rev(&INDEX, kind, path, &parse, &weigh, &mut |item: &(
        Turn,
        bool,
    )| {
        let (turn, is_primary) = item;
        if kind != "codex" || *is_primary {
            turns_seen += starts_turn(&primary, turn) as usize;
            if turns_seen > limit {
                return false;
            }
            primary.push(turn.clone());
        } else if fallback.len() < FALLBACK_ITEMS {
            fallback.push(turn.clone());
        }
        true
    })
    .map_err(|e| ReadError::Failed(format!("couldn't read the chat's transcript: {e}")))?;
    let mut items = if primary.is_empty() {
        fallback
    } else {
        primary
    };
    items.reverse();
    let mut turns = merge(items);
    // The newest `limit` turns (the fallback form was not counted while scanning).
    if turns.len() > limit {
        turns.drain(..turns.len() - limit);
    }
    Ok(Turns {
        kind: kind.to_string(),
        turns,
        scan_cut: cut,
    })
}

/// The last `limit` turns of a registered harness's transcript, oldest first. Lines are read
/// as Claude's or Codex's transcript lines where they match, else in the common shape
/// (`{"role", "content"}`, or under `message`). When no line parses, the format is unsupported.
fn read_registered(kind: &str, path: &std::path::Path, limit: usize) -> Result<Turns, ReadError> {
    let mut newest_first: Vec<Turn> = Vec::new();
    let mut turns_seen = 0usize;
    let parse = |line: &[u8]| -> Option<(Turn, bool)> {
        claude_turn(line)
            .or_else(|| codex_turn(line).map(|(turn, _)| turn))
            .or_else(|| generic_turn(line))
            .map(|turn| (turn, true))
    };
    let cut = tail::scan_rev(&INDEX, kind, path, &parse, &weigh, &mut |item: &(
        Turn,
        bool,
    )| {
        let turn = &item.0;
        turns_seen += starts_turn(&newest_first, turn) as usize;
        if turns_seen > limit {
            return false;
        }
        newest_first.push(turn.clone());
        true
    })
    .map_err(|e| ReadError::Failed(format!("couldn't read the chat's transcript: {e}")))?;
    if newest_first.is_empty() {
        return Err(ReadError::Failed(format!(
            "unsupported format: no turns in the {kind} chat's transcript could be read"
        )));
    }
    newest_first.reverse();
    let mut turns = merge(newest_first);
    if turns.len() > limit {
        turns.drain(..turns.len() - limit);
    }
    Ok(Turns {
        kind: kind.to_string(),
        turns,
        scan_cut: cut,
    })
}

/// A line in the common transcript shape: `role` and `content` (text, or parts with `text`), at
/// the top or under `message`.
fn generic_turn(line: &[u8]) -> Option<Turn> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let node = if value["message"].is_object() {
        &value["message"]
    } else {
        &value
    };
    let role = match node["role"].as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let text = match &node["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str().or_else(|| p.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    turn(role, &value, text)
}

/// Whether `next` (older than everything in `newest_first`) starts a new turn there.
fn starts_turn(newest_first: &[Turn], next: &Turn) -> bool {
    newest_first
        .last()
        .is_none_or(|last| last.role != next.role)
}

/// Joins runs of assistant blocks into one turn, drops a user turn that repeats the one before
/// it (a queued message appears as its enqueue line, then as the user line), and cuts long
/// assistant turns. `items` is oldest first.
fn merge(items: Vec<Turn>) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for item in items {
        match out.last_mut() {
            Some(last) if last.role == Role::Assistant && item.role == Role::Assistant => {
                last.text.push_str("\n\n");
                last.text.push_str(&item.text);
                last.truncated |= item.truncated;
            }
            Some(last)
                if last.role == Role::User && item.role == Role::User && last.text == item.text => {
            }
            _ => out.push(item),
        }
    }
    for turn in &mut out {
        if turn.role == Role::Assistant && turn.text.chars().count() > ASSISTANT_CHARS {
            let mut cut: String = turn.text.chars().take(ASSISTANT_CHARS).collect();
            cut.push_str(TRUNCATED);
            turn.text = cut;
            turn.truncated = true;
        }
    }
    out
}

fn stamp(line: &Value) -> (Option<String>, Option<u64>) {
    let ts = line["timestamp"].as_str().map(str::to_string);
    let ms = ts.as_deref().and_then(iso_epoch).map(|s| s * 1000);
    (ts, ms)
}

fn turn(role: Role, line: &Value, text: String) -> Option<Turn> {
    let text = strip_reminders(&text);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (ts, ts_ms) = stamp(line);
    Some(Turn {
        role,
        ts,
        ts_ms,
        text: text.to_string(),
        truncated: false,
    })
}

fn from_said(message: said::Said) -> Turn {
    Turn {
        role: Role::User,
        ts: message.ts,
        ts_ms: message.ts_ms,
        text: message.text,
        truncated: message.truncated,
    }
}

/// The turn in one Claude transcript line: what the user typed, or an assistant's text.
fn claude_turn(line: &[u8]) -> Option<Turn> {
    if let Some(said) = claude_message(line) {
        return Some(from_said(said));
    }
    if !has(line, b"\"assistant\"") {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    if value["type"] != "assistant" || value["message"]["role"] != "assistant" {
        return None;
    }
    let flag = |key: &str| value[key].as_bool() == Some(true);
    if flag("isSidechain") || flag("isMeta") || flag("isApiErrorMessage") {
        return None;
    }
    let text = match &value["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    turn(Role::Assistant, &value, text)
}

/// The turn in one Muse `session.jsonl` line: what the user typed, or an assistant's text.
fn muse_turn(line: &[u8]) -> Option<Turn> {
    if let Some(said) = muse_message(line) {
        return Some(from_said(said));
    }
    if !has(line, b"assistant_message_committed") && !has(line, b"run.terminal.completed") {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    let payload = &value["payload"];
    let text = match value["payload_type"].as_str()? {
        "runtime.session"
            if payload["kind"] == "run"
                && payload["event"]["kind"] == "assistant_message_committed" =>
        {
            payload["event"]["text"].as_str()?
        }
        "run.terminal.completed" => payload["text"].as_str()?,
        _ => return None,
    };
    let mut reply = turn(Role::Assistant, &value, text.to_string())?;
    (reply.ts, reply.ts_ms) = muse_stamp(&value);
    Some(reply)
}

/// Text parts whose `type` is `text`, `input_text`, `output_text` in any letter case.
fn text_parts(parts: &Value) -> String {
    parts
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| {
                    p["type"].as_str().is_some_and(|t| {
                        matches!(
                            t.to_ascii_lowercase().as_str(),
                            "text" | "input_text" | "output_text"
                        )
                    })
                })
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The turn in one Codex rollout line, and whether it came from the primary source (`event_msg`)
/// rather than the `response_item` fallback.
fn codex_turn(line: &[u8]) -> Option<(Turn, bool)> {
    if let Some((said, primary)) = codex_message(line) {
        return Some((from_said(said), primary));
    }
    if !has(line, b"AgentMessage") && !has(line, b"agent_message") && !has(line, b"\"assistant\"") {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    let payload = &value["payload"];
    let (text, primary) = match value["type"].as_str()? {
        "event_msg"
            if payload["type"] == "item_completed" && payload["item"]["type"] == "AgentMessage" =>
        {
            (text_parts(&payload["item"]["content"]), true)
        }
        "event_msg" if payload["type"] == "agent_message" => {
            (payload["message"].as_str()?.to_string(), true)
        }
        "response_item" if payload["type"] == "message" && payload["role"] == "assistant" => {
            let text = match &payload["content"] {
                Value::String(text) => text.clone(),
                other => text_parts(other),
            };
            (text, false)
        }
        _ => return None,
    };
    turn(Role::Assistant, &value, text).map(|t| (t, primary))
}
