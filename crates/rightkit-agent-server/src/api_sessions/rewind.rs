//! Rewind to an earlier user message (Claude-Code style `Esc Esc`).
//!
//! Three restorations, chosen by the caller:
//!
//! - **conversation**: the active context is truncated to just before the
//!   chosen prompt (the engine transcript and the shared live transcript);
//!   the prompt text is handed back so the client can put it in the composer.
//!   The durable event journal is append-only and is never edited: a
//!   `humanSteeringInjected` event with `injectedBy = "rewind"` is appended as
//!   the rewind marker.
//! - **files**: every file the rewound turns changed is restored from the
//!   `crates/history` snapshots the engine's write path wrote (sessions rules
//!   40 and 41) to its content before the first rewound turn. A file another
//!   process changed since is left alone and reported as a conflict.
//! - **both**.

use std::collections::HashMap;

use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};

use crate::api::{lock_recover, now_iso, touch_session, DaemonState};

/// A rewind (or redo) command for the engine task (which owns the engine transcript).
pub(crate) struct RewindRequest {
    target: Option<RewindTarget>,
    resp: tokio::sync::oneshot::Sender<Option<String>>,
    /// Rewind to a point: where the removed messages go, kept for redo.
    tail: Option<tokio::sync::oneshot::Sender<Vec<TranscriptMessage>>>,
    /// Redo: the messages to put back (the command then removes nothing).
    restore: Option<Vec<TranscriptMessage>>,
}

#[derive(Clone)]
struct RewindTarget {
    text: String,
    nth_from_end: usize,
}

impl RewindRequest {
    /// Legacy `POST /sessions/{id}/rewind`: drop the last user message.
    pub(crate) fn last_user(resp: tokio::sync::oneshot::Sender<Option<String>>) -> Self {
        Self {
            target: None,
            resp,
            tail: None,
            restore: None,
        }
    }

    fn to_point(
        point: &RewindPoint,
        resp: tokio::sync::oneshot::Sender<Option<String>>,
        tail: tokio::sync::oneshot::Sender<Vec<TranscriptMessage>>,
    ) -> Self {
        Self {
            target: Some(RewindTarget {
                text: point.text.clone(),
                nth_from_end: point.nth_from_end,
            }),
            resp,
            tail: Some(tail),
            restore: None,
        }
    }

    fn redo(
        messages: Vec<TranscriptMessage>,
        resp: tokio::sync::oneshot::Sender<Option<String>>,
    ) -> Self {
        Self {
            target: None,
            resp,
            tail: None,
            restore: Some(messages),
        }
    }

    /// Run on the engine task: truncate (or restore) the engine transcript and answer.
    pub(crate) fn apply(self, engine: &mut rightkit_agent_engine_core::SessionEngine) {
        if let Some(messages) = self.restore {
            engine.restore_messages(messages);
            let _ = self.resp.send(Some(String::new()));
            return;
        }
        let removed = match &self.target {
            None => engine.trim_before_last_user(),
            Some(target) => {
                let tail = engine.take_tail_before_user_matching(
                    |content| clean(content) == target.text,
                    target.nth_from_end,
                );
                let text = tail.as_ref().and_then(|tail| match tail.first() {
                    Some(TranscriptMessage::User { content, .. }) => Some(content.clone()),
                    _ => None,
                });
                if let (Some(tail), Some(sender)) = (tail, self.tail) {
                    let _ = sender.send(tail);
                }
                text
            }
        };
        let _ = self.resp.send(removed);
    }
}

/// What the most recent undo removed, so `POST /sessions/{id}/redo` can put it
/// back. One per session; a new rewind replaces it; a new turn voids it.
struct RedoStash {
    /// Engine-side messages the rewind removed (the chosen prompt onward).
    engine_tail: Vec<TranscriptMessage>,
    /// The same removal from the shared live transcript.
    live_tail: Vec<TranscriptMessage>,
    /// Length of the live transcript right after the undo: a redo is only
    /// valid while it is unchanged, i.e. while no new turn has run.
    live_len_after: usize,
    text: String,
}

fn redo_stashes() -> &'static std::sync::Mutex<HashMap<String, RedoStash>> {
    static STASHES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, RedoStash>>> =
        std::sync::OnceLock::new();
    STASHES.get_or_init(Default::default)
}

const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
const SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";

/// The single wrapper for host-injected reminders. Every injection site builds
/// its block here, so `strip_system_reminders` always matches the exact bytes
/// that were appended to the user turn.
pub(crate) fn wrap_system_reminder(body: &str) -> String {
    format!("{SYSTEM_REMINDER_OPEN}\n{body}\n{SYSTEM_REMINDER_CLOSE}")
}

/// Remove host-injected `<system-reminder>…</system-reminder>` blocks, leaving
/// the user's own text. An unclosed block drops the remainder.
pub(crate) fn strip_system_reminders(content: &str) -> String {
    let mut remaining = content;
    let mut out = String::new();
    while let Some(start) = remaining.find(SYSTEM_REMINDER_OPEN) {
        out.push_str(&remaining[..start]);
        let after = &remaining[start + SYSTEM_REMINDER_OPEN.len()..];
        match after.find(SYSTEM_REMINDER_CLOSE) {
            Some(end) => remaining = &after[end + SYSTEM_REMINDER_CLOSE.len()..],
            None => return out,
        }
    }
    out.push_str(remaining);
    out
}

/// What the user typed: provider-only `<system-reminder>` blocks removed.
fn clean(content: &str) -> String {
    strip_system_reminders(content).trim().to_string()
}

/// One prior user prompt of a session, as offered by the rewind picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPoint {
    pub text: String,
    /// Which occurrence of this exact text, counting from the newest (0 = the
    /// latest), so repeated identical prompts stay addressable.
    pub nth_from_end: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindMode {
    Conversation,
    Files,
    Both,
}

impl RewindMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Files => "files",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RewindReport {
    /// The chosen prompt, for the composer.
    pub text: String,
    pub conversation_rewound: bool,
    /// Workspace paths restored (create undone, edit reverted, delete recreated).
    pub files_restored: Vec<String>,
    /// `(path, reason)` for files left untouched because they changed since
    /// the agent wrote them (or could not be restored).
    pub file_conflicts: Vec<(String, String)>,
    /// `(shell id, command)` of background commands still running at a file rewind: they are left
    /// alone and what they change is not covered by it.
    pub background_shells_running: Vec<(String, String)>,
}

fn locate(messages: &[TranscriptMessage], text: &str, nth_from_end: usize) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| {
            matches!(message, TranscriptMessage::User { content, .. } if clean(content) == text)
        })
        .nth(nth_from_end)
        .map(|(position, _)| position)
}

/// A forked or otherwise dormant session is saved but not live; bring it back before rewinding it
/// (the same resume a new message triggers), so `/rewind` works on a session the TUI just forked.
async fn ensure_live(state: &DaemonState, session_id: &str) {
    if state.get_session(session_id).is_none() {
        let _ = state.resume_session(session_id).await;
    }
}

/// Prior user prompts of `session_id`, oldest first.
pub(crate) async fn rewind_points(
    state: &DaemonState,
    session_id: &str,
) -> Result<Vec<RewindPoint>, String> {
    ensure_live(state, session_id).await;
    let live = state
        .get_session(session_id)
        .ok_or_else(|| format!("session '{session_id}' not found"))?;
    let transcript = lock_recover(&live.transcript, "session.transcript");
    let mut rows: Vec<(String, String)> = transcript
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::User {
                content,
                created_at,
                pinned: false,
                ..
            } => {
                let text = clean(content);
                (!text.is_empty()).then(|| (text, created_at.clone()))
            }
            _ => None,
        })
        .collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut points = Vec::with_capacity(rows.len());
    while let Some((text, created_at)) = rows.pop() {
        let nth = seen.entry(text.clone()).or_default();
        points.push(RewindPoint {
            text,
            nth_from_end: *nth,
            created_at,
        });
        *nth += 1;
    }
    points.reverse();
    Ok(points)
}

/// Restore the session to just before `point` (see the module docs).
pub(crate) async fn rewind_to_message(
    state: &DaemonState,
    session_id: &str,
    point: &RewindPoint,
    mode: RewindMode,
) -> Result<RewindReport, String> {
    ensure_live(state, session_id).await;
    let live = state
        .get_session(session_id)
        .ok_or_else(|| format!("session '{session_id}' not found"))?;
    if live.live_turn_handle.is_active() {
        return Err("A turn is running; stop it before rewinding.".to_string());
    }
    touch_session(&live);
    let mut report = RewindReport {
        text: point.text.clone(),
        ..RewindReport::default()
    };

    let mut engine_tail: Vec<TranscriptMessage> = Vec::new();
    let mut live_tail: Vec<TranscriptMessage> = Vec::new();
    // Conversation first: if the prompt is no longer in context nothing else
    // has been touched yet.
    if matches!(mode, RewindMode::Conversation | RewindMode::Both) {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        let (tail_tx, tail_rx) = tokio::sync::oneshot::channel::<Vec<TranscriptMessage>>();
        live.rewind_tx
            .send(RewindRequest::to_point(point, resp_tx, tail_tx))
            .await
            .map_err(|_| "The session engine is not accepting rewinds.".to_string())?;
        let removed = resp_rx.await.unwrap_or(None);
        engine_tail = tail_rx.await.unwrap_or_default();
        if removed.is_none() {
            return Err(
                "That message is no longer in the active context (compacted or already rewound). \
                 Nothing was changed."
                    .to_string(),
            );
        }
        {
            let mut transcript = lock_recover(&live.transcript, "session.transcript");
            if let Some(position) = locate(&transcript, &point.text, point.nth_from_end) {
                live_tail = transcript.split_off(position);
            }
        }
        report.conversation_rewound = true;
        // Append-only journal: record the rewind, never delete history.
        let preview: String = point.text.chars().take(80).collect();
        let marker = serde_json::json!({
            "rewind": {
                "mode": mode.as_str(),
                "messageCreatedAt": point.created_at,
                "nthFromEnd": point.nth_from_end,
            }
        });
        live.conduit
            .emit(|seq| ProtocolEvent::HumanSteeringInjected {
                session_id: session_id.to_string(),
                seq,
                created_at: now_iso(),
                injected_by: "rewind".to_string(),
                content_preview: format!("rewind to: {preview}"),
                text: Some(marker.to_string()),
            });
    }

    if matches!(mode, RewindMode::Files | RewindMode::Both) {
        // Sessions rule 40: restore from the history snapshots the engine's
        // write path wrote, every file the rewound turns changed back to its
        // content before the first rewound turn. What could not be restored is
        // reported by name, never skipped in silence.
        let Some(cutoff_ns) = rightkit_agent_engine_history::rfc3339_utc_to_ns(&point.created_at)
        else {
            return Err(format!(
                "The rewind point has no usable timestamp ({:?}); no file was touched.",
                point.created_at
            ));
        };
        // A background command may have written after its turn ended: give what it changed so
        // far a restore point before restoring.
        rightkit_agent_coding_tools::shell::settle_background_file_tracking(session_id).await;
        let workspace_root = live.workspace_root.clone();
        let session = session_id.to_string();
        let restored = tokio::task::spawn_blocking(move || {
            rightkit_agent_engine_history::HistoryStore::new(
                std::path::Path::new(&workspace_root),
                rightkit_agent_config::state_dir_name(),
                &session,
            )
            .restore_since(std::path::Path::new(&workspace_root), cutoff_ns)
        })
        .await
        .map_err(|error| format!("file restore task failed: {error}"))?;
        report.files_restored = restored.restored;
        report.file_conflicts = restored.failed;
        let mut running: Vec<(String, String)> =
            lock_recover(&live.background_shells, "session.background_shells")
                .iter()
                .filter(|(_, shell)| {
                    shell.status()
                        == rightkit_agent_coding_tools::background_shell::BgStatus::Running
                })
                .map(|(id, shell)| (id.clone(), shell.command.clone()))
                .collect();
        running.sort();
        report.background_shells_running = running;
    } else {
        // A conversation-only rewind must not leave an older file stash that a
        // redo would apply to the wrong state.
        let workspace_root = live.workspace_root.clone();
        let session = session_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            rightkit_agent_engine_history::HistoryStore::new(
                std::path::Path::new(&workspace_root),
                rightkit_agent_config::state_dir_name(),
                &session,
            )
            .discard_redo()
        })
        .await;
    }
    // Keep what this undo removed so `redo` can put it back (one per session).
    let live_len_after = lock_recover(&live.transcript, "session.transcript").len();
    lock_recover(redo_stashes(), "redo stashes").insert(
        session_id.to_string(),
        RedoStash {
            engine_tail,
            live_tail,
            live_len_after,
            text: point.text.clone(),
        },
    );
    Ok(report)
}

/// Why a redo was refused. Both map to HTTP 409 with a stable `error` code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedoRefusal {
    /// No undo is waiting to be redone.
    NothingToRedo,
    /// A new turn has run since the undo, so it can no longer be redone.
    TurnSinceUndo,
    /// A turn is running right now.
    TurnRunning,
    /// The session or its engine is gone.
    Unavailable(String),
}

impl RedoRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NothingToRedo => "nothing_to_redo",
            Self::TurnSinceUndo => "turn_since_undo",
            Self::TurnRunning => "turn_running",
            Self::Unavailable(_) => "redo_unavailable",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::NothingToRedo => "There is no undo to redo.".to_string(),
            Self::TurnSinceUndo => {
                "A new turn has run since the undo, so it can no longer be redone.".to_string()
            }
            Self::TurnRunning => "A turn is running; stop it before redoing.".to_string(),
            Self::Unavailable(reason) => reason.clone(),
        }
    }
}

/// Re-apply the most recent undo: the removed messages go back into the engine
/// and the shared transcript, and the files return to the state the undo
/// replaced (see `HistoryStore::redo_since`). Refused once a new turn has run.
pub(crate) async fn redo_last_undo(
    state: &DaemonState,
    session_id: &str,
) -> Result<RewindReport, RedoRefusal> {
    ensure_live(state, session_id).await;
    let live = state
        .get_session(session_id)
        .ok_or_else(|| RedoRefusal::Unavailable(format!("session '{session_id}' not found")))?;
    if live.live_turn_handle.is_active() {
        return Err(RedoRefusal::TurnRunning);
    }
    let stash = {
        let mut stashes = lock_recover(redo_stashes(), "redo stashes");
        let Some(stash) = stashes.remove(session_id) else {
            return Err(RedoRefusal::NothingToRedo);
        };
        stash
    };
    let live_len = lock_recover(&live.transcript, "session.transcript").len();
    if live_len != stash.live_len_after {
        // The stash is spent: a later turn made the undone state unreachable.
        let workspace_root = live.workspace_root.clone();
        let session = session_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            rightkit_agent_engine_history::HistoryStore::new(
                std::path::Path::new(&workspace_root),
                rightkit_agent_config::state_dir_name(),
                &session,
            )
            .discard_redo()
        })
        .await;
        return Err(RedoRefusal::TurnSinceUndo);
    }
    touch_session(&live);
    let mut report = RewindReport {
        text: stash.text.clone(),
        ..RewindReport::default()
    };
    if !stash.engine_tail.is_empty() {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        live.rewind_tx
            .send(RewindRequest::redo(stash.engine_tail, resp_tx))
            .await
            .map_err(|_| {
                RedoRefusal::Unavailable("The session engine is not accepting redo.".into())
            })?;
        resp_rx
            .await
            .map_err(|_| RedoRefusal::Unavailable("The session engine did not answer.".into()))?;
        lock_recover(&live.transcript, "session.transcript").extend(stash.live_tail);
        report.conversation_rewound = true;
    }
    let workspace_root = live.workspace_root.clone();
    let session = session_id.to_string();
    let redone = tokio::task::spawn_blocking(move || {
        rightkit_agent_engine_history::HistoryStore::new(
            std::path::Path::new(&workspace_root),
            rightkit_agent_config::state_dir_name(),
            &session,
        )
        .redo_since()
    })
    .await
    .map_err(|error| RedoRefusal::Unavailable(format!("file redo task failed: {error}")))?;
    report.files_restored = redone.restored;
    report.file_conflicts = redone.failed;
    let preview: String = stash.text.chars().take(80).collect();
    live.conduit
        .emit(|seq| ProtocolEvent::HumanSteeringInjected {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            injected_by: "redo".to_string(),
            content_preview: format!("redo: {preview}"),
            text: Some(
                serde_json::json!({"redo": {"filesRestored": report.files_restored.len()}})
                    .to_string(),
            ),
        });
    Ok(report)
}
