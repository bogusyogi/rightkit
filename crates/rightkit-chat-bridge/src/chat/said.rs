//! `<cli> chat said`: what a chat's user said in that chat, read from its own transcript.
//! "Said" covers two kinds of user turn: text the user typed, and the user's answers to
//! pop-up questions (Claude's AskUserQuestion, Codex's request_user_input_async), each
//! labelled with its question. A relay from a lead chat is acted on; `said` confirms the
//! relay against the user's own words, it is not a gate. Only a contradiction is grounds to
//! ask the user; an answer that `said` does not show is not.
//!
//! The text comes from the sender's transcript on its own computer: this computer's files
//! directly, a linked computer's through `<cli> chat said --local` over the existing ssh link
//! (the same remote-command path `peers` uses). Nothing is written, copied or kept; the CLI
//! prints at most the last N messages.
//!
//! Sources (the field names relied on):
//! * Claude: `<claude config dir>/projects/<cwd slug>/<sessionId>.jsonl`, found by the
//!   file name under `projects/`. A typed message is a line with `type == "user"`,
//!   `message.role == "user"`, not `isMeta`, `isSidechain`, `isCompactSummary` or
//!   `isVisibleInTranscriptOnly`, no `toolUseResult`, `origin.kind` absent or `human` (a
//!   peer or system origin is not typed), and content that is a string or has text parts and
//!   no `tool_result` part. Text that is only `<system-reminder>` blocks, a `<command-...>` /
//!   `<local-command-...>` wrapper, a cross-session message, a the host app header, a task
//!   notification or an interruption marker is dropped.
//!   An answer to a pop-up question is a `user` line whose content has a `tool_result` part
//!   and whose `toolUseResult.answers` is an object: question text to answer, a string (or
//!   an array of strings for a multi-select question). The questions are listed in
//!   `toolUseResult.questions[].question`, which gives the order. Each answer is one message
//!   of kind `answer`, at the line's `timestamp`; an answer `[User dismissed ...]` is kind
//!   `dismissed` (the user closed the question without answering). Checked against 57 answer
//!   records in Claude transcripts on 2026-10-16.
//! * Codex: the thread's rollout file (`threads.rollout_path` in the Codex state database,
//!   as `deliver_codex` finds it). A typed message is `event_msg` with payload type
//!   `item_completed` and `item.type == "UserMessage"` (text in `item.content[].text`; the
//!   form current Codex writes, checked against real rollouts on 2026-10-15), or, from older
//!   Codex, `event_msg` with payload type `user_message` (text in `payload.message`). Only when
//!   the searched part of the file has neither, `response_item` payloads with `type == "message"` and `role ==
//!   "user"` (text in `content[].text`) are used instead, minus context Codex injects
//!   (environment context, AGENTS.md text, hook output) and text that carries the host app
//!   header.
//!   An answer to a pop-up question (`request_user_input_async`, answered asynchronously) is a
//!   `response_item` `message` with `role == "user"` whose text starts with
//!   `<send_user_message_question_reply>` and holds a JSON array of
//!   `{questionItemId, question, answer}`. Each element is one message of kind `answer`, at
//!   the line's `timestamp`. Checked against 9 answer records on 2026-10-16.
//!
//! * Muse: the session's `session.jsonl` (`muse::transcript_path`; JSON lines with a `payload_type`,
//!   a `payload` and `recorded_at` in microseconds). A typed message is the prompt of a run that
//!   started: a `runtime.session` record whose `payload.kind == "run"` and `payload.event.kind ==
//!   "started"` (text in `payload.event.prompt`, the whole text, where the `runtime.user_intent.accepted`
//!   record of the same turn can hold only a "[Pasted Content N chars]" placeholder), or a
//!   `turn.input.user` record (text in `payload.prompt`). Checked against 81 Muse Code 1.4.11
//!   session files on 2026-10-21; none had `turn.input.user`, so that form is read as the one
//!   the task description names.
//!
//! Turns `said` cannot show are counted, not dropped silently: a user turn whose content has
//! a non-text part (an image, a file) and which either shows no text or shows text beside it
//! is counted in `omitted` (`omittedTurns` in `--json`, "N non-text user turns omitted" in
//! text). Program-written text (headers, reminders, notifications) is not a user turn and is
//! not counted. The count covers only the transcript part that was searched.
//!
//! Automated turns are not what the user typed and are dropped: task notifications, peer and
//! system origins, the host app's own headers, and Codex automation heartbeats (`<heartbeat>`,
//! `<automation_id>`) and Claude's own automatic continuations ("I hit my usage limit while you
//! were working ...", "The app was quit while you were working ..."; seen with `origin` human or
//! none). Scheduled-task and `/loop` wake prompts: none was found in 2026-10-22's transcripts, so
//! whether they carry a marker is still unverified. A queued copy of a message (written when the user sent it mid-turn) is
//! counted once, with its user line. `--limit N` counts the messages shown (typed messages and
//! pop-up answers), newest first, exactly.
//!
//! The file is read from its end, newest line first, and the search stops once N messages
//! are found or `SCAN_CAP` bytes have been read, so a transcript of hundreds of megabytes
//! costs only its tail. A transcript that grows is not read again whole: the parsed lines are kept
//! in memory per path and only the appended bytes are read (`tail`, #449). Each message is cut to
//! `MAX_CHARS` characters.
//!
//! `stamp_for` reuses these readers on the SENDING side: every message the host app sends from
//! a chat carries that chat's newest 3 user turns (`envelope::UserWords`, at most 1024 bytes
//! each, 3 KiB in all) as the envelope's `said` field, filled from the transcript and never from
//! message text, so a receiver can read the user's exact words with no lookup, also for a relay
//! that `said <message-id>` cannot follow. A same-computer chat can check any other local chat
//! with `said claude:<id>`, `codex:<id>`, `muse:<id>` or a bare id (`said` below). Policy key
//! `stampUserWords` (default on) turns the stamp off.
//!
//! The Chat switch applies: with Chat off on a computer nothing is read there.

use super::deliver_codex::rollout_path_of;
use super::links::{self, Link};
use super::roster::{self, iso_epoch};
use super::store::Store;
use super::tail;
use super::{CLI_CALLER_ID, ChatError, Identity};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The parsed lines of the transcripts `read_file` has read (see `tail`).
static INDEX: tail::Index<Parsed> = tail::new_index();

/// Messages printed when `--limit` is not given.
pub const DEFAULT_LIMIT: usize = 10;
/// Most messages one call returns.
pub const MAX_LIMIT: usize = 50;
/// Longest message kept, in characters; a longer one ends in `TRUNCATED`.
pub const MAX_CHARS: usize = 4000;
const TRUNCATED: &str = "…(truncated)";

/// What kind of user turn a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Text the user typed.
    Typed,
    /// The user's answer to a pop-up question; `Said::question` holds the question.
    Answer,
    /// A pop-up question the user closed without answering; `Said::question` holds it.
    Dismissed,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Typed => "typed",
            Kind::Answer => "answer",
            Kind::Dismissed => "dismissed",
        }
    }
}

/// One message the user typed, or one answer to a pop-up question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub kind: Kind,
    /// For an answer or a dismissed question: the question's text, cut to `MAX_CHARS`.
    pub question: Option<String>,
    /// The transcript's own timestamp text (ISO 8601, UTC), when it has one.
    pub ts: Option<String>,
    pub ts_ms: Option<u64>,
    pub text: String,
    /// The text was longer than `MAX_CHARS` and was cut.
    pub truncated: bool,
}

impl Said {
    fn to_json(&self) -> Value {
        json!({"kind": self.kind.as_str(), "question": self.question,
               "ts": self.ts, "tsMs": self.ts_ms, "text": self.text,
               "truncated": self.truncated})
    }

    fn from_json(value: &Value) -> Option<Said> {
        let text = value["text"].as_str()?;
        let (text, cut) = cap(text);
        let kind = match value["kind"].as_str()? {
            "typed" => Kind::Typed,
            "answer" => Kind::Answer,
            "dismissed" => Kind::Dismissed,
            _ => return None,
        };
        Some(Said {
            kind,
            question: value["question"].as_str().map(str::to_string),
            ts: value["ts"].as_str().map(str::to_string),
            ts_ms: value["tsMs"].as_u64(),
            text,
            truncated: cut || value["truncated"].as_bool() == Some(true),
        })
    }
}

/// Why nothing could be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaidError {
    /// Not available, by a fact the caller can act on: no route, no transcript, Chat off.
    /// The CLI exits 2.
    Refused(String),
    /// Something broke while reading or reaching the computer.
    Failed(String),
}

/// The typed messages and pop-up answers of one chat, oldest first.
#[derive(Debug, Clone)]
pub struct Found {
    /// The computer the chat is on, as this computer names it.
    pub device: String,
    /// "claude", "codex" or "muse".
    pub kind: String,
    pub session: String,
    /// The received message the chat was found from, when it was named by one.
    pub message_id: Option<String>,
    pub messages: Vec<Said>,
    /// User turns in the searched part that `said` cannot show (non-text content such as
    /// images or files). Counted so the output can say so.
    pub omitted: usize,
    /// The search stopped at `SCAN_CAP` bytes before finding as many as asked for.
    pub scan_cut: bool,
}

impl Found {
    pub fn to_json(&self) -> Value {
        json!({"device": self.device, "kind": self.kind, "session": self.session,
               "messageId": self.message_id,
               "messages": self.messages.iter().map(Said::to_json).collect::<Vec<_>>(),
               "omittedTurns": self.omitted,
               "scanCut": self.scan_cut})
    }
}

/// A chat id is letters, digits, `-` and `_` (a Claude session or Codex thread id): never a
/// path, so a remote caller cannot name a file outside the transcript folders.
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `text` cut to `MAX_CHARS` characters, with whether it was.
pub(super) fn cap(text: &str) -> (String, bool) {
    if text.chars().count() <= MAX_CHARS {
        return (text.to_string(), false);
    }
    let mut cut: String = text.chars().take(MAX_CHARS).collect();
    cut.push_str(TRUNCATED);
    (cut, true)
}

/// `text` without its `<system-reminder>…</system-reminder>` blocks (an unclosed one is
/// dropped with everything after it).
pub(super) fn strip_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Whether `text` starts like something a program put in the user's turn.
fn starts_like(text: &str, prefixes: &[&str]) -> bool {
    let lower: String = text.chars().take(120).collect::<String>().to_lowercase();
    prefixes.iter().any(|p| lower.starts_with(p))
}

/// The start of a header this bridge writes: `[<product> chat:`, lower case.
fn header_marker() -> String {
    format!("[{}:", super::brand::brand().chat_marker())
}

pub(super) fn claude_injected(text: &str) -> bool {
    let marker = header_marker();
    starts_like(
        text,
        &[
            "<cross-session-message",
            "another claude session sent a message",
            marker.as_str(),
            "<task-notification",
            "<command-",
            "<local-command-",
            "<bash-",
            "<heartbeat",
            "<automation",
            "caveat: the messages below were generated by the user while running local commands",
            "[request interrupted by user",
            // Claude's own automatic continuations, seen in real transcripts with `origin`
            // `human` or none: not words the user typed.
            "i hit my usage limit while you were working",
            "the app was quit while you were working",
        ],
    )
}

pub(super) fn muse_injected(text: &str) -> bool {
    let marker = header_marker();
    let delivered = format!(
        "{} delivered the following",
        super::brand::brand().chat_marker()
    );
    starts_like(
        text,
        &[
            marker.as_str(),
            delivered.as_str(),
            "<heartbeat",
            "<automation",
            "<system-reminder",
        ],
    )
}

pub(super) fn codex_injected(text: &str) -> bool {
    let marker = header_marker();
    starts_like(
        text,
        &[
            marker.as_str(),
            "<heartbeat",
            "<automation",
            "<environment_context>",
            "# agents.md instructions",
            "<user_instructions>",
            "<turn_aborted>",
            "<hook",
            "<skill",
            "<permissions",
            "<subagent_notification",
        ],
    )
}

/// A message from a transcript line's text, if it survives the cleaning.
fn said_from(line: &Value, text: &str, injected: fn(&str) -> bool) -> Option<Said> {
    let text = strip_reminders(text);
    let text = text.trim();
    if text.is_empty() || injected(text) {
        return None;
    }
    let ts = line["timestamp"].as_str().map(str::to_string);
    let ts_ms = ts.as_deref().and_then(iso_epoch).map(|s| s * 1000);
    let (text, truncated) = cap(text);
    Some(Said {
        kind: Kind::Typed,
        question: None,
        ts,
        ts_ms,
        text,
        truncated,
    })
}

/// One answer to a pop-up question, at the timestamp of the line that carries it. `answer`
/// is the answer text (several chosen labels joined by ", "). A dismissal is kind `dismissed`
/// and says so, rather than presenting the placeholder text as an answer.
fn answer_said(line: &Value, question: &str, answer: &str) -> Said {
    let dismissed = answer.trim_start().starts_with("[User dismissed");
    let body = if dismissed {
        format!("Pop-up question dismissed without an answer: \"{question}\"")
    } else {
        format!("Answer to pop-up question \"{question}\": {answer}")
    };
    let ts = line["timestamp"].as_str().map(str::to_string);
    let ts_ms = ts.as_deref().and_then(iso_epoch).map(|s| s * 1000);
    let (text, truncated) = cap(&body);
    Said {
        kind: if dismissed {
            Kind::Dismissed
        } else {
            Kind::Answer
        },
        question: Some(cap(question).0),
        ts,
        ts_ms,
        text,
        truncated,
    }
}

/// What one transcript line says about the user: its messages (several for an answer to a
/// multi-question pop-up), and whether it is a user turn `said` cannot show.
#[derive(Clone, Debug, Default)]
pub(super) struct Parsed {
    pub messages: Vec<Said>,
    /// Claude only: the line is a queued copy of a message (a `queue-operation` enqueue).
    pub queued: bool,
    /// A user turn with non-text content that `said` cannot show (see the module doc).
    pub omitted: bool,
    /// Codex only: the message came from the `response_item` fallback, not a primary source.
    pub fallback: bool,
}

/// The answers to a Claude pop-up question in one user line, in question order. Claude keeps
/// them in `toolUseResult.answers`: question text to answer (a string, or an array of
/// strings for a multi-select question).
fn claude_answers(value: &Value) -> Vec<Said> {
    let Some(result) = value.get("toolUseResult").filter(|r| r.is_object()) else {
        return Vec::new();
    };
    let Some(answers) = result["answers"].as_object() else {
        return Vec::new();
    };
    let mut order: Vec<&str> = result["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| q["question"].as_str())
        .filter(|q| answers.contains_key(*q))
        .collect();
    for key in answers.keys() {
        if !order.contains(&key.as_str()) {
            order.push(key.as_str());
        }
    }
    order
        .into_iter()
        .filter_map(|question| {
            let answer = match answers.get(question)? {
                Value::String(text) => text.clone(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => return None,
            };
            Some(answer_said(value, question, &answer))
        })
        .collect()
}

pub(super) fn has(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len()).any(|w| w == needle)
}

/// Everything one Claude transcript line says about the user.
pub(super) fn claude_line(line: &[u8]) -> Parsed {
    let mut out = Parsed::default();
    // Most lines are assistant output or tool results; skip them without parsing.
    let queued = has(line, b"\"queue-operation\"");
    if !queued && !has(line, b"\"user\"") {
        return out;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return out;
    };
    // A message the user sends while the chat is busy is recorded first as a queue
    // `enqueue` line holding the text; when the chat takes it in mid-turn, no `user` line
    // follows (found by the 2026-10-15 council, F-2). Peer and task messages are queued the
    // same way and are dropped by the text rules below.
    if value["type"] == "queue-operation" {
        if value["operation"] == "enqueue"
            && let Some(text) = value["content"].as_str()
        {
            out.queued = true;
            out.messages
                .extend(said_from(&value, text, claude_injected));
        }
        return out;
    }
    if value["type"] != "user" || value["message"]["role"] != "user" {
        return out;
    }
    let flag = |key: &str| value[key].as_bool() == Some(true);
    if flag("isMeta")
        || flag("isSidechain")
        || flag("isCompactSummary")
        || flag("isVisibleInTranscriptOnly")
    {
        return out;
    }
    // A tool result: the answer to a pop-up question carries the answers; any other tool
    // result is not something the user typed.
    if let Value::Array(parts) = &value["message"]["content"]
        && parts.iter().any(|p| p["type"] == "tool_result")
    {
        out.messages = claude_answers(&value);
        return out;
    }
    if value.get("toolUseResult").is_some_and(|v| !v.is_null()) {
        return out;
    }
    // A peer, a task or the system wrote it, not the user. An older transcript has no
    // `origin`; the text rules below still apply to it.
    if let Some(kind) = value["origin"]["kind"].as_str()
        && kind != "human"
    {
        return out;
    }
    let (text, attachments) = match &value["message"]["content"] {
        Value::String(text) => (text.clone(), false),
        Value::Array(parts) => (
            parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            parts.iter().any(|p| p["type"] != "text"),
        ),
        _ => return out,
    };
    let message = said_from(&value, &text, claude_injected);
    out.omitted = attachments && (message.is_some() || strip_reminders(&text).trim().is_empty());
    out.messages.extend(message);
    out
}

/// The user message in one Claude transcript line, if the line is one the user typed.
/// Answers to pop-up questions are not returned here; `claude_line` has them.
pub(super) fn claude_message(line: &[u8]) -> Option<Said> {
    claude_line(line)
        .messages
        .into_iter()
        .find(|m| m.kind == Kind::Typed)
}

/// The reply to pop-up questions Codex asked with `request_user_input_async`, if `text` is
/// one: `<send_user_message_question_reply>` and a JSON array of `{questionItemId, question,
/// answer}`. Anything else is not a reply, and `None` lets the caller treat it as text.
fn codex_replies(line: &Value, text: &str) -> Option<Vec<Said>> {
    const OPEN: &str = "<send_user_message_question_reply>";
    const CLOSE: &str = "</send_user_message_question_reply>";
    let rest = text.trim_start().strip_prefix(OPEN)?;
    let body = rest.split(CLOSE).next()?;
    let items: Vec<Value> = serde_json::from_str(body.trim()).ok()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let question = item["question"].as_str()?;
                let answer = match &item["answer"] {
                    Value::String(text) => text.clone(),
                    Value::Array(list) => list
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    _ => return None,
                };
                Some(answer_said(line, question, &answer))
            })
            .collect(),
    )
}

/// Everything one Codex rollout line says about the user. Its primary messages come from
/// `event_msg` lines and pop-up replies; `fallback` marks the `response_item` text.
pub(super) fn codex_line(line: &[u8]) -> Parsed {
    let mut out = Parsed::default();
    if !has(line, b"user_message") && !has(line, b"UserMessage") && !has(line, b"\"user\"") {
        return out;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return out;
    };
    let payload = &value["payload"];
    match value["type"].as_str() {
        Some("event_msg")
            if payload["type"] == "item_completed" && payload["item"]["type"] == "UserMessage" =>
        {
            let Some(parts) = payload["item"]["content"].as_array() else {
                return out;
            };
            let text = parts
                .iter()
                .filter(|p| matches!(p["type"].as_str(), Some("text" | "input_text")))
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let attachments = parts
                .iter()
                .any(|p| !matches!(p["type"].as_str(), Some("text" | "input_text")));
            let message = said_from(&value, &text, codex_injected);
            out.omitted =
                attachments && (message.is_some() || strip_reminders(&text).trim().is_empty());
            out.messages.extend(message);
        }
        Some("event_msg") if payload["type"] == "user_message" => {
            let Some(text) = payload["message"].as_str() else {
                return out;
            };
            out.messages.extend(said_from(&value, text, codex_injected));
        }
        Some("response_item") if payload["type"] == "message" && payload["role"] == "user" => {
            let (text, attachments) = match &payload["content"] {
                Value::String(text) => (text.clone(), false),
                Value::Array(parts) => (
                    parts
                        .iter()
                        .filter(|p| matches!(p["type"].as_str(), Some("input_text" | "text")))
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    parts
                        .iter()
                        .any(|p| !matches!(p["type"].as_str(), Some("input_text" | "text"))),
                ),
                _ => return out,
            };
            if let Some(replies) = codex_replies(&value, &text) {
                out.messages = replies;
                return out;
            }
            out.fallback = true;
            let message = said_from(&value, &text, codex_injected);
            out.omitted =
                attachments && (message.is_some() || strip_reminders(&text).trim().is_empty());
            out.messages.extend(message);
        }
        _ => {}
    }
    out
}

/// The time of a Muse record (`recorded_at`, microseconds): ISO text and milliseconds.
pub(super) fn muse_stamp(line: &Value) -> (Option<String>, Option<u64>) {
    let ms = line["recorded_at"].as_u64().map(|us| us / 1000);
    (ms.map(|ms| crate::chat::rfc3339(ms / 1000)), ms)
}

/// Everything one Muse `session.jsonl` line says about the user (see the module doc for the
/// records). All of it is primary: a Muse transcript has no second source to fall back to.
pub(super) fn muse_line(line: &[u8]) -> Parsed {
    let mut out = Parsed::default();
    if !has(line, b"turn.input.user") && !has(line, b"\"started\"") {
        return out;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return out;
    };
    let payload = &value["payload"];
    let text = match value["payload_type"].as_str() {
        Some("turn.input.user") => payload["prompt"].as_str(),
        Some("runtime.session")
            if payload["kind"] == "run" && payload["event"]["kind"] == "started" =>
        {
            payload["event"]["prompt"].as_str()
        }
        _ => None,
    };
    let Some(text) = text else {
        return out;
    };
    let cleaned = strip_reminders(text);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || muse_injected(cleaned) {
        return out;
    }
    let (ts, ts_ms) = muse_stamp(&value);
    let (text, truncated) = cap(cleaned);
    out.messages.push(Said {
        kind: Kind::Typed,
        question: None,
        ts,
        ts_ms,
        text,
        truncated,
    });
    out
}

/// The user message in one Muse line.
pub(super) fn muse_message(line: &[u8]) -> Option<Said> {
    muse_line(line).messages.into_iter().next()
}

/// The user message in one Codex rollout line, and whether it came from the primary
/// source (`event_msg` `user_message`) rather than the `response_item` fallback. Pop-up
/// answers are not returned here; `codex_line` has them.
pub(super) fn codex_message(line: &[u8]) -> Option<(Said, bool)> {
    let parsed = codex_line(line);
    let primary = !parsed.fallback;
    parsed
        .messages
        .into_iter()
        .find(|m| m.kind == Kind::Typed)
        .map(|m| (m, primary))
}

/// The Claude transcript of a session: `<config dir>/projects/*/<id>.jsonl` in any Claude
/// profile folder, the newest when more than one folder has it.
pub(super) fn claude_transcript(id: &str) -> Option<PathBuf> {
    let name = format!("{id}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = super::deliver_claude::sessions_dirs()
        .into_iter()
        .filter_map(|(dir, _)| dir.parent().map(|p| p.join("projects")))
        .filter_map(|projects| std::fs::read_dir(projects).ok())
        .flat_map(|listing| listing.flatten());
    for entry in entries {
        let path = entry.path().join(&name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

pub(super) fn codex_rollout(id: &str) -> Option<PathBuf> {
    rollout_path_of(id).filter(|p| p.is_file())
}

pub(super) fn muse_transcript(id: &str) -> Option<PathBuf> {
    super::muse::transcript_path(id)
}

/// The last `limit` messages typed in the chat `session` of `kind` ("claude", "codex", "muse", or
/// none to try Claude's transcripts, then Codex's, then Muse's) on THIS computer, oldest first.
/// Blocking file reads: call it off a UI thread.
pub fn read_local(
    store: &Store,
    me: &Identity,
    kind: Option<&str>,
    session: &str,
    limit: usize,
) -> Result<Found, SaidError> {
    if !store.chat_enabled() {
        return Err(SaidError::Refused(format!("Chat is off on {}", me.alias)));
    }
    if !valid_id(session) {
        return Err(SaidError::Refused(
            "that is not a chat id (letters, digits, - and _ only)".into(),
        ));
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let named = kind.is_some();
    let kinds: &[&str] = match kind {
        Some("claude") => &["claude"],
        Some("codex") => &["codex"],
        Some("muse") => &["muse"],
        None => &["claude", "codex", "muse"],
        Some(other) if super::codex_hook::valid_hook_harness(other) => {
            return Err(SaidError::Refused(format!(
                "The app has no reader for {other} chats' history, so it cannot show what the user typed there"
            )));
        }
        Some(other) => {
            return Err(SaidError::Refused(format!(
                "unknown chat kind \"{other}\"; use claude, codex or muse"
            )));
        }
    };
    for kind in kinds {
        let path = match *kind {
            "claude" => claude_transcript(session),
            "codex" => codex_rollout(session),
            _ if store.muse_chats() => muse_transcript(session),
            _ if named => {
                return Err(SaidError::Refused(format!(
                    "Muse chats are off on {}",
                    me.alias
                )));
            }
            _ => None,
        };
        if let Some(path) = path {
            return read_file(me, kind, session, &path, limit);
        }
    }
    Err(SaidError::Refused(format!(
        "no {} transcript for chat {session} on {}",
        kind.unwrap_or("Claude, Codex or Muse"),
        me.alias
    )))
}

fn read_file(
    me: &Identity,
    kind: &str,
    session: &str,
    path: &Path,
    limit: usize,
) -> Result<Found, SaidError> {
    let mut typed: Vec<Said> = Vec::new();
    // Claude: indices into `typed` of user lines no queued copy has been matched to yet.
    let mut open_user: Vec<usize> = Vec::new();
    let mut fallback: Vec<Said> = Vec::new();
    // Omitted turns, primary lines and Codex fallback lines counted apart (see below).
    let mut omitted = 0usize;
    let mut omitted_fallback = 0usize;
    let parse = |line: &[u8]| -> Option<Parsed> {
        let parsed = match kind {
            "claude" => claude_line(line),
            "muse" => muse_line(line),
            _ => codex_line(line),
        };
        // A line with no message and no omitted turn changes nothing below.
        (!parsed.messages.is_empty() || parsed.omitted).then_some(parsed)
    };
    let weigh = |parsed: &Parsed| {
        128 + parsed
            .messages
            .iter()
            .map(|m| m.text.len() + m.question.as_ref().map_or(0, String::len))
            .sum::<usize>()
    };
    // The transcript's parsed lines are kept per path and read again only where the file grew.
    let cut = tail::scan_rev(
        &INDEX,
        kind,
        path,
        &parse,
        &weigh,
        &mut |parsed: &Parsed| {
            // A line's messages are pushed last first, so the final reverse keeps them in order.
            if kind == "claude" {
                omitted += usize::from(parsed.omitted);
                for message in parsed.messages.iter().cloned().rev() {
                    if parsed.queued {
                        // A message the user sent while the chat was busy is written twice: a queued
                        // copy, then the user line when the chat takes it. The queued copy is the
                        // same message, so it is skipped when its user line is already kept.
                        let matched = open_user
                            .iter()
                            .rposition(|&i| typed[i].text == message.text);
                        if let Some(pos) = matched {
                            open_user.remove(pos);
                            continue;
                        }
                    } else if message.kind == Kind::Typed {
                        open_user.push(typed.len());
                    }
                    typed.push(message);
                }
            } else if parsed.fallback {
                omitted_fallback += usize::from(parsed.omitted);
                if fallback.len() < limit {
                    fallback.extend(parsed.messages.iter().cloned().rev());
                }
            } else {
                omitted += usize::from(parsed.omitted);
                typed.extend(parsed.messages.iter().cloned().rev());
            }
            typed.len() < limit
        },
    )
    .map_err(|e| SaidError::Failed(format!("couldn't read the chat's transcript: {e}")))?;
    let use_fallback = typed.is_empty();
    let mut messages = if use_fallback { fallback } else { typed };
    // A Codex fallback turn is the same turn as a primary one when both are in the file, so the
    // larger count is taken rather than the sum.
    let omitted = if use_fallback {
        omitted.max(omitted_fallback)
    } else {
        omitted
    };
    messages.reverse();
    // One line can hold several answers, so keep the newest `limit`.
    if messages.len() > limit {
        messages.drain(..messages.len() - limit);
    }
    Ok(Found {
        device: me.alias.clone(),
        kind: kind.to_string(),
        session: session.to_string(),
        message_id: None,
        messages,
        omitted,
        scan_cut: cut,
    })
}

/// `read_local` for the other side of a link: runs `<cli> chat said --local` there and
/// reads its one JSON line.
fn read_remote(
    link: &Link,
    kind: Option<&str>,
    session: &str,
    limit: usize,
) -> Result<Found, SaidError> {
    if !valid_id(session) {
        return Err(SaidError::Refused(
            "that is not a chat id (letters, digits, - and _ only)".into(),
        ));
    }
    let target = match kind {
        Some(kind) => format!("{kind}:{session}"),
        None => session.to_string(),
    };
    let answer = links::said(link, &target, limit.clamp(1, MAX_LIMIT)).map_err(|e| {
        if e.contains("unknown chat command") {
            SaidError::Failed(format!(
                "{}'s {} is too old for `{} chat said`; update it there",
                link.device,
                super::brand::product(),
                super::brand::brand().cli
            ))
        } else {
            SaidError::Failed(e)
        }
    })?;
    if answer["ok"].as_bool() != Some(true) {
        let why = answer["error"].as_str().unwrap_or("it gave no reason");
        let text = format!("{}: {why}", link.device);
        return Err(if answer["refused"].as_bool() == Some(true) {
            SaidError::Refused(text)
        } else {
            SaidError::Failed(text)
        });
    }
    let messages = answer["messages"]
        .as_array()
        .ok_or_else(|| SaidError::Failed(format!("{} printed no messages list", link.device)))?
        .iter()
        .filter_map(Said::from_json)
        .collect();
    Ok(Found {
        device: link.device.clone(),
        kind: answer["kind"].as_str().unwrap_or("").to_string(),
        session: session.to_string(),
        message_id: None,
        messages,
        omitted: answer["omittedTurns"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| {
                SaidError::Failed("the other computer's answer has no omitted-turn count".into())
            })?,
        scan_cut: answer["scanCut"].as_bool() == Some(true),
    })
}

/// The user's latest turns in the sending chat `session` (its harness `kind`, "" when unknown) on
/// THIS computer, for `Envelope::stamped`: the same transcript readers `said` uses, the
/// newest `MAX_STAMP_TURNS` typed messages and pop-up answers, oldest first. Called by the
/// sending the host app for every message it sends; the text of a message never reaches it. Empty
/// (never an error) when the switch is off, the chat is not a chat (a plain terminal), or no
/// transcript can be read, so a send is never blocked or slowed past the tail read.
pub fn stamp_for(
    store: &Store,
    me: &Identity,
    kind: &str,
    session: &str,
) -> Vec<super::envelope::UserWords> {
    if session == CLI_CALLER_ID || !store.stamp_user_words() {
        return Vec::new();
    }
    let kind = (!kind.is_empty()).then_some(kind);
    let Ok(found) = read_local(store, me, kind, session, super::envelope::MAX_STAMP_TURNS) else {
        return Vec::new();
    };
    found
        .messages
        .into_iter()
        .filter(|m| m.kind != Kind::Dismissed)
        .map(|m| super::envelope::UserWords {
            ts: m.ts.unwrap_or_default(),
            kind: m.kind.as_str().to_string(),
            text: match (m.kind, m.question) {
                (Kind::Answer, Some(question)) => format!("[answer to \"{question}\"] {}", m.text),
                _ => m.text,
            },
        })
        .collect()
}

fn clean_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The typed messages of the chat `target` names: a received message's id (its reply
/// route says which chat sent it), a chat id (`claude:<id>`, `codex:<id>`, `muse:<id>`, a bare id of a
/// chat on this computer, `<device>:<id>`), or `"<chat> on <device>"` as `peers` prints it.
/// Blocking (files, and ssh for a linked computer): call it off a UI thread.
pub fn said(store: &Store, me: &Identity, target: &str, limit: usize) -> Result<Found, SaidError> {
    if !store.chat_enabled() {
        return Err(SaidError::Refused(format!("Chat is off on {}", me.alias)));
    }
    let target = target.trim();
    let limit = limit.clamp(1, MAX_LIMIT);
    // A received message: the route stored when it was delivered names the sender.
    if clean_id(target)
        && let Some(route) = store.reply_route(target)
    {
        if route.from_session == CLI_CALLER_ID {
            return Err(SaidError::Refused(
                "that message was sent from a plain terminal, not from a chat, so there is no \
                 chat transcript to read"
                    .into(),
            ));
        }
        let address = super::reply_address(store, me, &route.from_device, &route.from_session)
            .map_err(SaidError::Refused)?;
        let mut found = match address.strip_suffix(&format!(":{}", route.from_session)) {
            Some(device) => {
                let link = links::find(store, device)
                    .ok_or_else(|| SaidError::Refused(format!("no link named {device}")))?;
                read_remote(&link, None, &route.from_session, limit)?
            }
            None => read_local(store, me, None, &route.from_session, limit)?,
        };
        found.message_id = Some(target.to_string());
        return Ok(found);
    }
    // An exact local chat: `claude:<id>`, `codex:<id>` or `muse:<id>`.
    if let Some((kind, id)) = target.split_once(':')
        && matches!(kind, "claude" | "codex" | "muse")
    {
        return read_local(store, me, Some(kind), id, limit);
    }
    if clean_id(target) {
        // No route. Maybe a chat id on this computer; else the route is gone.
        let peers = super::local_peers(store, me);
        if let Ok(peer) = roster::resolve(&peers, target) {
            return read_local(store, me, Some(peer.kind.as_str()), &peer.session, limit);
        }
        return Err(SaidError::Refused(format!(
            "no route for message id {target} on {} (routes are kept 30 days, and this computer \
             only has them for messages it received), and no chat here has that id. If you know \
             the sender, name its chat: {} chat said \"<chat> on <device>\"",
            me.alias,
            super::brand::brand().cli
        )));
    }
    let peer = super::resolve_peer(store, me, target).map_err(|e| match e {
        ChatError::NotFound(_) | ChatError::Invalid(_) => SaidError::Refused(e.to_string()),
        other => SaidError::Failed(other.to_string()),
    })?;
    if peer.local {
        read_local(store, me, Some(peer.kind.as_str()), &peer.session, limit)
    } else {
        let link = links::find(store, &peer.device)
            .ok_or_else(|| SaidError::Refused(format!("no link named {}", peer.device)))?;
        read_remote(&link, Some(peer.kind.as_str()), &peer.session, limit)
    }
}
