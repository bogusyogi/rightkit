//! AI chat in the launcher . A question typed in the launcher is answered by
//! the resident engine's `launcher.ask` (the same command the Mac calls), which runs one
//! answer-only, tools-off call on the person's own Claude Code. Only the Ask row set to
//! "inLauncher" (`launcherAskTarget`) gates it; `agentRequests` does not apply. A refusal is shown
//! as the engine words it.
//!
//! The history lives in `ai-chat.json` in the launcher folder: newest first, the pinned
//! ones kept however old, the rest capped at `KEEP`.

use crate::host::{self, EngineFailure as Failure};
use rightkit_launcher::config::quoted;
use rightkit_launcher::json::{self, Value};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The history file, in the launcher's folder.
const FILE: &str = "ai-chat.json";
/// Most unpinned answers kept; pinned ones are not counted.
pub const KEEP: usize = 100;
/// Most characters kept of one answer.
const ANSWER_MAX: usize = 8 * 1024;
/// Most bytes read from the history file or the CLI's answer.
const READ_MAX: usize = 8 * 1024 * 1024;
/// The engine's own limit is 180 s; this ends the wait a little after it.
const TIME_LIMIT: Duration = Duration::from_secs(200);

/// One question and its answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub question: String,
    pub answer: String,
    /// When it was asked, in milliseconds since 1970.
    pub at: u64,
    pub pinned: bool,
}

/// The history in `dir`, newest first; empty when there is none or it cannot be read.
pub fn load(dir: &Path) -> Vec<Entry> {
    let Ok(bytes) = std::fs::read(dir.join(FILE)) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = json::parse(&bytes, READ_MAX) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = items
        .iter()
        .filter_map(|item| {
            Some(Entry {
                id: item.get("id")?.as_str()?.to_string(),
                question: item.get("question")?.as_str()?.to_string(),
                answer: item.get("answer")?.as_str()?.to_string(),
                at: item.get("at")?.as_f64()?.max(0.0) as u64,
                pinned: item.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.at));
    entries
}

/// Writes the history. False when the folder or the file could not be written.
pub fn save(dir: &Path, entries: &[Entry]) -> bool {
    let items: Vec<String> = entries
        .iter()
        .map(|entry| {
            format!(
                "{{\"id\":{},\"question\":{},\"answer\":{},\"at\":{},\"pinned\":{}}}",
                quoted(&entry.id),
                quoted(&entry.question),
                quoted(&entry.answer),
                entry.at,
                entry.pinned
            )
        })
        .collect();
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    rightkit_fs::atomic_write(&dir.join(FILE), format!("[{}]", items.join(",")).as_bytes()).is_ok()
}

/// Puts a new answer first and drops the oldest unpinned ones beyond `KEEP`.
pub fn record(entries: &mut Vec<Entry>, question: &str, answer: &str) -> Entry {
    let at = now_millis();
    let entry = Entry {
        id: format!("chat-{at}"),
        question: question.chars().take(400).collect(),
        answer: answer.chars().take(ANSWER_MAX).collect(),
        at,
        pinned: false,
    };
    entries.insert(0, entry.clone());
    let mut unpinned = 0;
    entries.retain(|item| {
        if item.pinned {
            return true;
        }
        unpinned += 1;
        unpinned <= KEEP
    });
    entry
}

/// An engine answer: the text and, only when the engine supplied one, the provider's label. The
/// label is shown with a fresh answer and is not saved (the history file's schema is unchanged).
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub answer: String,
    pub provider: Option<String>,
}

/// Asks the resident engine's `launcher.ask` one question and returns the answer, or why there
/// is none. The engine answers with the person's own Claude Code, tools off; it needs only the
/// `launcherAskTarget` setting (the launcher asks only while it is "inLauncher"), not `agentRequests`.
pub fn ask(question: &str) -> Result<Reply, String> {
    let reply = host::engine_call_within(
        "launcher.ask",
        serde_json::json!({ "question": question }),
        TIME_LIMIT,
    )
    .map_err(|failure| match failure {
        Failure::Unreachable(_) => host::t(
            "The assistant's engine is not running, so the question cannot reach your AI provider. Try again in a moment.",
        )
        .to_string(),
        Failure::Unknown(_) => host::t("The assistant did not answer in time.").to_string(),
        // `no_provider` carries the setup steps in its message; every refusal is shown as the engine words it.
        Failure::Refused { message, .. } => message,
    })?;
    let text = reply
        .get("answer")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(host::t("The assistant gave no answer text.").to_string());
    }
    let provider = reply
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    Ok(Reply {
        answer: text.to_string(),
        provider,
    })
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
