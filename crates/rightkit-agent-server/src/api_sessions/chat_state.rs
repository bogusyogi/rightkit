//! The state mark on every chat in the session list.
//!
//! The list used to say only `running` or `idle`. A person scanning it needs to know which chat
//! is waiting on them and why, which finished while they were away, and which stopped. That is
//! derived here from facts the daemon already holds: the live session's pending approvals and
//! questions, whether a turn is running, how the stored conversation last ended, and when the
//! person last opened the chat.
//!
//! "Seen" is the only new stored fact: one small JSON file under the state root, written by
//! `POST /sessions/{id}/seen`. It is deliberately not a column on the session row, so marking a
//! chat as read never changes the row's `updatedAt`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use rightkit_agent_engine_storage::{GoalRow, GoalStatus, SessionTail};
use serde::Serialize;

use crate::api::DaemonState;

/// What the mark beside a chat says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatState {
    /// A turn is running.
    Working,
    /// It wants permission to do something.
    NeedsApproval,
    /// It asked the person a question and is waiting for the reply.
    AskedQuestion,
    /// Its turn ended and the person has not opened the chat since.
    FinishedUnread,
    /// Its turn ended and the person has seen it.
    FinishedSeen,
    /// It failed, was interrupted, or the person stopped it.
    Stopped,
    /// A scheduled run that has not started its turn yet.
    Scheduled,
    /// A chat with no turn yet.
    New,
}

impl ChatState {
    /// Whether this state is the person's move.
    pub(crate) fn needs_you(self) -> bool {
        matches!(self, Self::NeedsApproval | Self::AskedQuestion)
    }
}

/// Everything the mark is derived from. No I/O here, so every rule is unit-tested.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChatFacts<'a> {
    /// The session is held in memory by the daemon.
    pub live: bool,
    pub turn_active: bool,
    pub pending_approvals: usize,
    /// The tool the first pending approval is for, when known.
    pub approval_tool: Option<&'a str>,
    pub pending_questions: usize,
    /// The stored session status (`idle`, `failed`, `interrupted`, `done`, ...).
    pub stored_status: &'a str,
    pub tail: Option<&'a SessionTail>,
    pub seen_at: Option<&'a str>,
    /// The session was created by a schedule.
    pub from_schedule: bool,
}

/// The state and one plain line that says it in words.
pub(crate) fn derive_chat_state(facts: &ChatFacts<'_>) -> (ChatState, String) {
    if facts.pending_approvals > 0 {
        let what = match facts.approval_tool {
            Some("shell") | Some("exec") | Some("process") => "run a command",
            Some("edit_file") | Some("write_file") | Some("multi_edit") | Some("apply_patch") => {
                "edit a file"
            }
            Some(tool) if tool.starts_with("browser") => "use the browser",
            Some(tool) if tool.starts_with("web") || tool == "http_request" => "reach the network",
            _ => "do something",
        };
        return (ChatState::NeedsApproval, format!("Needs approval · {what}"));
    }
    if facts.pending_questions > 0 {
        return (ChatState::AskedQuestion, "Asked you a question".to_string());
    }
    if facts.turn_active {
        return (ChatState::Working, "Working".to_string());
    }
    let tail = facts.tail;
    let last_kind = tail.and_then(|t| t.last_event_kind.as_deref());
    let turn_status = tail.and_then(|t| t.last_turn_status.as_deref());
    match (last_kind, turn_status) {
        (Some("runFailed"), _) | (Some("sessionTurnFinished"), Some("failed")) => {
            return (ChatState::Stopped, "Stopped · it failed".to_string());
        }
        (Some("runInterrupted"), _) => {
            return (ChatState::Stopped, "Stopped · interrupted".to_string());
        }
        (Some("sessionTurnFinished"), Some("cancelled")) => {
            return (ChatState::Stopped, "Stopped by you".to_string());
        }
        _ => {}
    }
    if matches!(
        facts.stored_status,
        "failed" | "error" | "interrupted" | "cancelled"
    ) {
        let why = match facts.stored_status {
            "interrupted" => "interrupted",
            "cancelled" => "stopped",
            _ => "it failed",
        };
        return (ChatState::Stopped, format!("Stopped · {why}"));
    }
    match last_kind {
        None => {
            if facts.from_schedule {
                (ChatState::Scheduled, "Scheduled".to_string())
            } else {
                (ChatState::New, "New".to_string())
            }
        }
        // A message was sent and no turn has ended since. In a live session the turn is
        // about to start; in a stored one it never finished.
        Some("userMessageSubmitted") => {
            if facts.live {
                (ChatState::Working, "Working".to_string())
            } else {
                (ChatState::Stopped, "Stopped · interrupted".to_string())
            }
        }
        _ => {
            if tail.is_some_and(|t| answer_ends_with_a_question(t.last_answer.as_deref())) {
                return (ChatState::AskedQuestion, "Asked you a question".to_string());
            }
            let finished_at = tail.and_then(|t| t.last_event_at.as_deref()).unwrap_or("");
            let seen = facts
                .seen_at
                .is_some_and(|seen| !finished_at.is_empty() && seen >= finished_at);
            if seen {
                (ChatState::FinishedSeen, "Finished".to_string())
            } else {
                (
                    ChatState::FinishedUnread,
                    "Finished · not read yet".to_string(),
                )
            }
        }
    }
}

/// Whether a turn's last answer leaves a question with the person: its last line of prose ends
/// with a question mark. Code blocks and trailing blank lines are skipped, so a snippet ending
/// in `?` does not count and an answer that ends "Shall I go ahead?" does.
pub(crate) fn answer_ends_with_a_question(answer: Option<&str>) -> bool {
    let Some(answer) = answer else {
        return false;
    };
    let mut in_code = false;
    let mut last_prose: Option<&str> = None;
    for line in answer.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || trimmed.is_empty() {
            continue;
        }
        last_prose = Some(trimmed);
    }
    last_prose.is_some_and(|line| {
        line.trim_end_matches(['*', '_', '"', '\'', ')', '`', ' '])
            .ends_with('?')
    })
}

/// The `chatState` object on a session row.
pub(crate) fn chat_state_json(facts: &ChatFacts<'_>) -> serde_json::Value {
    let (state, detail) = derive_chat_state(facts);
    serde_json::json!({
        "state": state,
        "detail": detail,
        "needsYou": state.needs_you(),
        "pendingApprovals": facts.pending_approvals,
        "pendingQuestions": facts.pending_questions,
        "since": facts.tail.and_then(|t| t.last_event_at.clone()),
        "seenAt": facts.seen_at,
    })
}

/// Whether a session id was minted by the scheduler (`bot-<schedule>-<millis>`).
pub(crate) fn session_is_from_schedule(session_id: &str) -> bool {
    session_id
        .strip_prefix("bot-")
        .and_then(|rest| rest.rsplit_once('-'))
        .is_some_and(|(schedule, millis)| {
            !schedule.is_empty() && !millis.is_empty() && millis.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The `goal` object on a session row: the goal's text and where it stands, or `null`.
pub(crate) fn goal_json(goal: Option<&GoalRow>) -> serde_json::Value {
    let Some(goal) = goal else {
        return serde_json::Value::Null;
    };
    let total = goal.steps.len();
    let done = goal
        .steps
        .iter()
        .filter(|step| goal_step_is_done(step))
        .count();
    serde_json::json!({
        "text": goal.text,
        "status": goal.status.as_str(),
        "phase": goal_phase(goal, done, total),
        "now": goal.now,
        "next": goal.next,
        "stepsDone": done,
        "stepsTotal": total,
        // The goal's own verification record, as stored (for example `pending`, `verified`).
        // This is the only review verdict the engine keeps; a plain turn has none.
        "verification": goal.verification_state,
        "startedAt": goal.started_at,
    })
}

fn goal_step_is_done(step: &rightkit_agent_engine_protocol::event::GoalStepV1) -> bool {
    serde_json::to_value(step)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .or_else(|| value.get("state"))
                .and_then(|s| s.as_str())
                .map(|s| matches!(s, "done" | "completed" | "complete" | "verified"))
        })
        .unwrap_or(false)
}

/// One word for where a goal stands, in the order a person expects: plan, build, verify, done.
/// Derived only from what the goal record holds; nothing is guessed.
fn goal_phase(goal: &GoalRow, done: usize, total: usize) -> &'static str {
    match goal.status {
        GoalStatus::Achieved => return "done",
        GoalStatus::Blocked | GoalStatus::Unmet => return "blocked",
        GoalStatus::Paused | GoalStatus::BudgetLimited | GoalStatus::UsageLimited => {
            return "paused"
        }
        GoalStatus::Active => {}
    }
    let verifying = matches!(
        goal.verification_state.as_str(),
        "running" | "verifying" | "pending-verdict" | "in-progress"
    );
    if verifying || (total > 0 && done == total) {
        "verify"
    } else if total == 0 {
        "plan"
    } else {
        "build"
    }
}

// ── "seen" ────────────────────────────────────────────────────────────────────

const SEEN_FILE: &str = "session-seen.json";

fn seen_cache() -> &'static Mutex<Option<HashMap<String, String>>> {
    static CACHE: OnceLock<Mutex<Option<HashMap<String, String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn seen_path() -> Option<PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join(SEEN_FILE))
}

fn load_seen_from_disk() -> HashMap<String, String> {
    seen_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// When each chat was last opened, by session id.
pub(crate) fn seen_snapshot() -> HashMap<String, String> {
    let mut cache = seen_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.get_or_insert_with(load_seen_from_disk).clone()
}

/// Record that a chat was opened now. Returns the time recorded.
pub(crate) fn mark_seen(session_id: &str) -> String {
    let now = rightkit_agent_engine_protocol::now_iso();
    let snapshot = {
        let mut cache = seen_cache().lock().unwrap_or_else(|e| e.into_inner());
        let map = cache.get_or_insert_with(load_seen_from_disk);
        map.insert(session_id.to_string(), now.clone());
        map.clone()
    };
    persist_seen(&snapshot);
    now
}

/// Drop a deleted chat's marker.
pub(crate) fn forget_seen(session_id: &str) {
    let snapshot = {
        let mut cache = seen_cache().lock().unwrap_or_else(|e| e.into_inner());
        let map = cache.get_or_insert_with(load_seen_from_disk);
        if map.remove(session_id).is_none() {
            return;
        }
        map.clone()
    };
    persist_seen(&snapshot);
}

fn persist_seen(map: &HashMap<String, String>) {
    let Some(path) = seen_path() else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(map) else {
        return;
    };
    // Write beside the file and rename, so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `POST /sessions/{id}/seen`: the person opened this chat. Idempotent.
pub(crate) async fn mark_session_seen(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let known = state.list_sessions().iter().any(|s| s.id == id)
        || match &state.storage {
            Some(storage) => {
                let storage = storage.clone();
                let id = id.clone();
                tokio::task::spawn_blocking(move || {
                    storage
                        .blocking_lock()
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
                .unwrap_or(false)
            }
            None => false,
        };
    if !known {
        return Err(StatusCode::NOT_FOUND);
    }
    let seen_at = tokio::task::spawn_blocking(move || mark_seen(&id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "seenAt": seen_at })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(kind: &str, status: Option<&str>, at: &str, answer: Option<&str>) -> SessionTail {
        SessionTail {
            last_event_kind: Some(kind.to_string()),
            last_event_at: Some(at.to_string()),
            last_turn_status: status.map(str::to_string),
            last_answer: answer.map(str::to_string),
        }
    }

    fn state_of(facts: ChatFacts<'_>) -> ChatState {
        derive_chat_state(&facts).0
    }

    #[test]
    fn an_approval_outranks_everything_and_names_what_it_is_for() {
        let facts = ChatFacts {
            live: true,
            turn_active: true,
            pending_approvals: 1,
            approval_tool: Some("shell"),
            pending_questions: 1,
            ..Default::default()
        };
        let (state, detail) = derive_chat_state(&facts);
        assert_eq!(state, ChatState::NeedsApproval);
        assert_eq!(detail, "Needs approval · run a command");
        assert!(state.needs_you());
    }

    #[test]
    fn a_pending_question_is_a_different_state_from_an_approval() {
        let facts = ChatFacts {
            live: true,
            turn_active: true,
            pending_questions: 1,
            ..Default::default()
        };
        assert_eq!(state_of(facts), ChatState::AskedQuestion);
    }

    #[test]
    fn a_running_turn_is_working() {
        let facts = ChatFacts {
            live: true,
            turn_active: true,
            ..Default::default()
        };
        assert_eq!(state_of(facts), ChatState::Working);
    }

    #[test]
    fn a_finished_turn_is_unread_until_seen_after_it_finished() {
        let done = tail(
            "sessionTurnFinished",
            Some("completed"),
            "2026-11-12T10:00:00.000Z",
            Some("Done. 44 tests pass."),
        );
        let unread = ChatFacts {
            tail: Some(&done),
            stored_status: "idle",
            ..Default::default()
        };
        assert_eq!(state_of(unread), ChatState::FinishedUnread);
        let seen_before = ChatFacts {
            tail: Some(&done),
            stored_status: "idle",
            seen_at: Some("2026-11-12T09:59:59.000Z"),
            ..Default::default()
        };
        assert_eq!(state_of(seen_before), ChatState::FinishedUnread);
        let seen_after = ChatFacts {
            tail: Some(&done),
            stored_status: "idle",
            seen_at: Some("2026-11-12T10:00:01.000Z"),
            ..Default::default()
        };
        assert_eq!(state_of(seen_after), ChatState::FinishedSeen);
    }

    #[test]
    fn a_turn_that_ends_on_a_question_waits_for_a_reply() {
        let asked = tail(
            "sessionTurnFinished",
            Some("completed"),
            "2026-11-12T10:00:00.000Z",
            Some("I found two ways to do this.\n\nWhich one do you want?"),
        );
        let facts = ChatFacts {
            tail: Some(&asked),
            stored_status: "idle",
            seen_at: Some("2026-11-12T11:00:00.000Z"),
            ..Default::default()
        };
        let (state, detail) = derive_chat_state(&facts);
        assert_eq!(state, ChatState::AskedQuestion);
        assert_eq!(detail, "Asked you a question");
    }

    #[test]
    fn question_detection_reads_prose_not_code() {
        assert!(answer_ends_with_a_question(Some("Shall I go ahead?")));
        assert!(answer_ends_with_a_question(Some(
            "**Shall I go ahead?**\n\n"
        )));
        assert!(!answer_ends_with_a_question(Some("Done.")));
        assert!(!answer_ends_with_a_question(Some(
            "Here is the check:\n```\nif x == nil ?\n```"
        )));
        assert!(answer_ends_with_a_question(Some(
            "```\nlet x = 1;\n```\nIs that what you meant?"
        )));
        assert!(!answer_ends_with_a_question(None));
        assert!(!answer_ends_with_a_question(Some("")));
    }

    #[test]
    fn failed_cancelled_and_interrupted_turns_are_stopped() {
        for (kind, status, words) in [
            ("sessionTurnFinished", Some("failed"), "Stopped · it failed"),
            ("sessionTurnFinished", Some("cancelled"), "Stopped by you"),
            ("runInterrupted", None, "Stopped · interrupted"),
            ("runFailed", None, "Stopped · it failed"),
        ] {
            let t = tail(kind, status, "2026-11-12T10:00:00.000Z", None);
            let facts = ChatFacts {
                tail: Some(&t),
                ..Default::default()
            };
            let (state, detail) = derive_chat_state(&facts);
            assert_eq!(state, ChatState::Stopped, "{kind} {status:?}");
            assert_eq!(detail, words);
        }
        let stored = ChatFacts {
            stored_status: "interrupted",
            ..Default::default()
        };
        assert_eq!(state_of(stored), ChatState::Stopped);
    }

    #[test]
    fn a_message_with_no_finished_turn_is_working_when_live_and_stopped_when_stored() {
        let sent = tail(
            "userMessageSubmitted",
            None,
            "2026-11-12T10:00:00.000Z",
            None,
        );
        let live = ChatFacts {
            live: true,
            tail: Some(&sent),
            ..Default::default()
        };
        assert_eq!(state_of(live), ChatState::Working);
        let stored = ChatFacts {
            tail: Some(&sent),
            ..Default::default()
        };
        assert_eq!(state_of(stored), ChatState::Stopped);
    }

    #[test]
    fn a_chat_with_no_turn_is_new_or_scheduled() {
        assert_eq!(state_of(ChatFacts::default()), ChatState::New);
        let scheduled = ChatFacts {
            from_schedule: true,
            ..Default::default()
        };
        assert_eq!(state_of(scheduled), ChatState::Scheduled);
        assert!(session_is_from_schedule("bot-nightly-audit-1794480000000"));
        assert!(!session_is_from_schedule("bot-"));
        assert!(!session_is_from_schedule("bot-nightly"));
        assert!(!session_is_from_schedule("w15twice"));
    }

    #[test]
    fn the_row_object_carries_the_state_in_snake_case_and_in_words() {
        let facts = ChatFacts {
            live: true,
            turn_active: true,
            pending_approvals: 1,
            approval_tool: Some("edit_file"),
            ..Default::default()
        };
        let json = chat_state_json(&facts);
        assert_eq!(json["state"], "needs_approval");
        assert_eq!(json["detail"], "Needs approval · edit a file");
        assert_eq!(json["needsYou"], true);
    }
}
