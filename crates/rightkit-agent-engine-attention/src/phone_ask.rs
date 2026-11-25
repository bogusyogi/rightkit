//! A phone's question and its lifecycle: the checks on what the phone sends, the key a question is kept
//! under, whose task a task is, the cancel that beat its start, and the attempt number of an Inbox action.
//!
//! The host builds the task and talks to its journal; this module holds the neutral rules around that, so
//! another host's phone thread behaves the same. Parameter errors are returned as the plain message of a
//! bad-input refusal.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Longest question, in bytes.
pub const QUESTION_MAX: usize = 4096;

/// A plain token: 1 to `max` characters of letters, digits, `-` and `_`.
pub fn plain_token(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `question`: trimmed, 1 to [`QUESTION_MAX`] bytes.
pub fn question_param(params: &Value) -> Result<&str, String> {
    params
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty() && q.len() <= QUESTION_MAX)
        .ok_or_else(|| format!("`question` must be 1 to {QUESTION_MAX} bytes"))
}

/// `clientMsgId`: 1 to 64 letters, digits, `-` or `_`.
pub fn client_param(params: &Value) -> Result<&str, String> {
    params
        .get("clientMsgId")
        .and_then(Value::as_str)
        .filter(|id| plain_token(id, 64))
        .ok_or_else(|| "`clientMsgId` must be 1 to 64 letters, digits, - or _".to_string())
}

/// `taskId`: at most 80 bytes of letters, digits, `-`, `_` and `:`, not empty.
pub fn task_id_param(params: &Value) -> Result<&str, String> {
    params
        .get("taskId")
        .and_then(Value::as_str)
        .filter(|id| {
            id.len() <= 80
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_:".contains(c))
        })
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "`taskId` is missing or malformed".to_string())
}

/// The idempotency key of a phone's ask. Built by the host, so a result can tell the asking device's tasks by
/// its prefix: a phone cannot forge another device's prefix (the device id is the registry's).
pub fn ask_key(device_id: &str, client_id: &str) -> String {
    format!("phone-ask:{device_id}:{client_id}")
}

/// Whether a task whose kind is (`kind_is_ask`) and whose idempotency key is `key` is one of THIS device's questions.
pub fn is_own_question(kind_is_ask: bool, key: Option<&str>, device_id: &str) -> bool {
    kind_is_ask && key.is_some_and(|key| key.starts_with(&ask_key(device_id, "")))
}

// ---- a cancel that beat its start ---------------------------------------------------------------------------

/// How long a cancel that found no question waits for the start it raced.
const CANCELLED_EARLY_TTL: Duration = Duration::from_secs(600);
/// Most such cancels held at once.
const CANCELLED_EARLY_MAX: usize = 64;

fn cancelled_early() -> &'static Mutex<Vec<(String, Instant)>> {
    static SET: OnceLock<Mutex<Vec<(String, Instant)>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(Vec::new()))
}

/// Questions the phone cancelled by `clientMsgId` before the start reached this computer (Stop pressed while
/// the start was still on its way). Remembers `key`, with when.
pub fn remember_cancelled_early(key: &str) {
    let mut set = cancelled_early().lock().unwrap_or_else(|e| e.into_inner());
    set.retain(|(_, at)| at.elapsed() < CANCELLED_EARLY_TTL);
    if set.len() >= CANCELLED_EARLY_MAX {
        set.remove(0);
    }
    set.push((key.to_string(), Instant::now()));
}

/// Whether `key` was cancelled before it started; a start for it is refused `cancelled`, so the race closes.
pub fn was_cancelled_early(key: &str) -> bool {
    let mut set = cancelled_early().lock().unwrap_or_else(|e| e.into_inner());
    set.retain(|(_, at)| at.elapsed() < CANCELLED_EARLY_TTL);
    set.iter().any(|(held, _)| held == key)
}

/// The answer to a cancel by `clientMsgId` when no such question exists yet.
pub fn cancelled_before_start(client_id: &str) -> Value {
    json!({ "clientMsgId": client_id, "status": "cancelled", "finished": true })
}

// ---- the attempt number of an Inbox action -------------------------------------------------------------------

/// The most attempts one action is offered (the Inbox allows a retry of 1 to 1000).
pub const MAX_RETRY: i64 = 1000;

/// `params` with only `id`, `action` and `retry` (set to `retry`).
pub fn with_retry(params: &Value, retry: i64) -> Value {
    let mut clean = serde_json::Map::new();
    for key in ["id", "action"] {
        if let Some(value) = params.get(key) {
            clean.insert(key.to_string(), value.clone());
        }
    }
    clean.insert("retry".to_string(), json!(retry));
    Value::Object(clean)
}

/// Why no next attempt number could be given.
#[derive(Debug)]
pub enum RetryError<E> {
    /// The journal lookup failed.
    Lookup(E),
    /// An earlier attempt is still going, or ended `uncertain` (`act_in_progress`).
    InProgress,
    /// More than [`MAX_RETRY`] failed attempts (`too_many_attempts`).
    TooMany,
}

impl<E> RetryError<E> {
    /// The refusal code and message of the two refusals, `None` for a failed lookup.
    pub fn refusal(&self) -> Option<(&'static str, &'static str)> {
        match self {
            RetryError::Lookup(_) => None,
            RetryError::InProgress => Some((
                "act_in_progress",
                "that action was already started and has not ended; look at the item again in a moment",
            )),
            RetryError::TooMany => Some((
                "too_many_attempts",
                "that action has failed too many times to offer it again",
            )),
        }
    }
}

/// The number of the next attempt at an item's action: 1 for the first, one more after each earlier attempt
/// that `failed` or was `cancelled` (the item stays open then). An earlier attempt that is still going, or
/// ended `uncertain`, is not retried: nothing a second run does could be told apart from the first's. The
/// idempotency key of an attempt names the item, the action and the number
/// (`inbox.act:<item>:<action>:<retry>`). `task_for_key` and `status_of` read the host's journal:
/// the task id of a key, and a task's status word.
pub fn next_retry<E>(
    item_id: &str,
    action_id: &str,
    mut task_for_key: impl FnMut(&str) -> Result<Option<String>, E>,
    mut status_of: impl FnMut(&str) -> Result<Option<String>, E>,
) -> Result<i64, RetryError<E>> {
    for retry in 1..=MAX_RETRY {
        let key = format!("inbox.act:{item_id}:{action_id}:{retry}");
        let Some(task) = task_for_key(&key).map_err(RetryError::Lookup)? else {
            return Ok(retry);
        };
        let status = status_of(&task).map_err(RetryError::Lookup)?;
        if !matches!(status.as_deref(), Some("failed" | "cancelled")) {
            return Err(RetryError::InProgress);
        }
    }
    Err(RetryError::TooMany)
}
