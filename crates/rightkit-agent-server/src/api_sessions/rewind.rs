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
//! - **files**: every not-yet-reverted workspace change this session's runs
//!   captured at or after that prompt is undone newest-first through the
//!   run-change store's `undo`, which is an ordinary FileService mutation
//!   guarded by EFF-008 fingerprints: bytes that changed since the agent wrote
//!   them are never overwritten and are reported as conflicts.
//! - **both**.

use std::collections::HashMap;

use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};

use crate::api::{lock_recover, now_iso, touch_session, DaemonState};
use crate::api_run_changes::{shared_run_changes_store, UndoRequest};

/// A rewind command for the engine task (which owns the engine transcript).
pub(crate) struct RewindRequest {
    target: Option<RewindTarget>,
    resp: tokio::sync::oneshot::Sender<Option<String>>,
}

#[derive(Clone)]
struct RewindTarget {
    text: String,
    nth_from_end: usize,
}

impl RewindRequest {
    /// Legacy `POST /sessions/{id}/rewind`: drop the last user message.
    pub(crate) fn last_user(resp: tokio::sync::oneshot::Sender<Option<String>>) -> Self {
        Self { target: None, resp }
    }

    fn to_point(point: &RewindPoint, resp: tokio::sync::oneshot::Sender<Option<String>>) -> Self {
        Self {
            target: Some(RewindTarget {
                text: point.text.clone(),
                nth_from_end: point.nth_from_end,
            }),
            resp,
        }
    }

    /// Run on the engine task: truncate the engine transcript and answer.
    pub(crate) fn apply(self, engine: &mut rightkit_agent_engine_core::SessionEngine) {
        let removed = match &self.target {
            None => engine.trim_before_last_user(),
            Some(target) => engine.trim_before_user_matching(
                |content| clean(content) == target.text,
                target.nth_from_end,
            ),
        };
        let _ = self.resp.send(removed);
    }
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

/// Prior user prompts of `session_id`, oldest first.
pub(crate) fn rewind_points(
    state: &DaemonState,
    session_id: &str,
) -> Result<Vec<RewindPoint>, String> {
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

    // Conversation first: if the prompt is no longer in context nothing else
    // has been touched yet.
    if matches!(mode, RewindMode::Conversation | RewindMode::Both) {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        live.rewind_tx
            .send(RewindRequest::to_point(point, resp_tx))
            .await
            .map_err(|_| "The session engine is not accepting rewinds.".to_string())?;
        let removed = resp_rx.await.unwrap_or(None);
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
                transcript.truncate(position);
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
        let store = shared_run_changes_store();
        for (run_id, entry) in store.session_entries_since(session_id, &point.created_at) {
            let response = store.undo(
                &run_id,
                UndoRequest {
                    run_id: run_id.clone(),
                    workspace_root: entry.workspace_root.clone(),
                    mutation_id: entry.mutation_id.clone(),
                    observed_fingerprint: entry.fingerprint.clone(),
                },
            );
            if response.applied {
                if !report.files_restored.contains(&entry.path) {
                    report.files_restored.push(entry.path.clone());
                }
            } else {
                report.file_conflicts.push((
                    entry.path.clone(),
                    response
                        .conflict_reason
                        .unwrap_or_else(|| "could not be restored".to_string()),
                ));
            }
        }
    }
    Ok(report)
}
