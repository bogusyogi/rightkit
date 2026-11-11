//! Durable execution journal for agent runs.
//!
//! When an agent turn starts, a journal file is opened and entries are appended
//! as JSON lines. On clean completion, the file is deleted. If the daemon finds
//! a journal file on startup, it knows that run was interrupted mid-flight.
//!
//! Journal path: `~/.coderight/journals/<session_id>.journal`
//! Format: newline-delimited JSON, one entry per line.

use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedRun {
    pub turn_id: String,
    pub started_at: String,
    pub user_message_preview: Option<String>,
}

pub struct ExecutionJournal {
    path: PathBuf,
    file: Option<fs::File>,
}

impl ExecutionJournal {
    /// Locate the journals directory under `~/.coderight/journals/`.
    pub fn journals_dir() -> Option<PathBuf> {
        rightkit_agent_config::state_root().map(|root| root.join("journals"))
    }

    fn path_for(journals_dir: &Path, session_id: &str) -> PathBuf {
        journals_dir.join(format!("{session_id}.journal"))
    }

    /// Open (create) a journal for this session, creating the directory if needed.
    pub async fn open(journals_dir: &Path, session_id: &str) -> Self {
        let path = Self::path_for(journals_dir, session_id);
        let file = async {
            fs::create_dir_all(journals_dir).await.ok()?;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .ok()
        }
        .await;
        Self { path, file }
    }

    /// Append a JSON entry (adds a newline).
    pub async fn append(&mut self, entry: serde_json::Value) {
        if let Some(f) = &mut self.file {
            let mut line = serde_json::to_string(&entry).unwrap_or_default();
            line.push('\n');
            let _ = f.write_all(line.as_bytes()).await;
        }
    }

    /// Close and delete the journal (clean completion).
    pub async fn close(&mut self) {
        self.file = None;
        let _ = fs::remove_file(&self.path).await;
    }

    /// Read all entries from an existing journal. Returns empty vec if none exists.
    pub async fn replay(journals_dir: &Path, session_id: &str) -> Vec<serde_json::Value> {
        let path = Self::path_for(journals_dir, session_id);
        let text = match fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Return runs that started but never recorded a matching `run_end`.
    /// These are not auto-retried because a crash can happen after filesystem or
    /// shell side effects. The caller can surface them as recovery context.
    pub async fn interrupted_runs(journals_dir: &Path, session_id: &str) -> Vec<InterruptedRun> {
        let mut starts: Vec<InterruptedRun> = Vec::new();
        for entry in Self::replay(journals_dir, session_id).await {
            let Some(kind) = entry.get("kind").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(turn_id) = entry.get("turn_id").and_then(|v| v.as_str()) else {
                continue;
            };
            match kind {
                "run_start" => starts.push(InterruptedRun {
                    turn_id: turn_id.to_string(),
                    started_at: entry
                        .get("ts")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    user_message_preview: entry
                        .get("user_message_preview")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                }),
                "run_end" => starts.retain(|run| run.turn_id != turn_id),
                _ => {}
            }
        }
        starts
    }
}

/// A UI-surfaceable recovery marker for a session with a leftover journal:
/// the most recent interrupted turn's id, start time, and prompt preview.
/// Serialized onto the session row so the client can badge "recoverable"
/// BEFORE the session is reopened (the resume-time recovery note in the
/// transcript only appears once the agent is already re-running).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryMarker {
    pub turn_id: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
}

/// The recovery marker for one session, or `None` when its journal is absent
/// or has no interrupted run (clean completion deletes the file). Reads the
/// existing `interrupted_runs` payload -- the same source the resume-time
/// recovery note uses -- so the row and the note never disagree.
pub async fn recovery_marker_for(journals_dir: &Path, session_id: &str) -> Option<RecoveryMarker> {
    ExecutionJournal::interrupted_runs(journals_dir, session_id)
        .await
        .pop()
        .map(|run| RecoveryMarker {
            turn_id: run.turn_id,
            started_at: run.started_at,
            prompt_preview: run.user_message_preview,
        })
}

/// Reconcile the persistent recovery marker on a session row with the
/// on-disk journal state. Reads the marker the same way `recovery_marker_for`
/// does (so the row never disagrees with the resume-time recovery note), then
/// either stamps the serialized marker onto `recovery_marker` or clears it.
///
/// Idempotent: callers may invoke after every `ExecutionJournal::open()`
/// (mid-life crash sets the marker), after every `close()` (clean completion
/// clears it), and at daemon startup (reconcile drift from the previous
/// lifecycle / 7-day archive). Caller passes the daemon's `SharedStorage`
/// so the writer mutex path is used — never raw `Storage`.
pub async fn reconcile_persistent_marker(
    storage: &crate::api::SharedStorage,
    journals_dir: &Path,
    session_id: &str,
) {
    let marker = recovery_marker_for(journals_dir, session_id).await;
    let json = marker.as_ref().and_then(|m| serde_json::to_string(m).ok());
    let now = crate::api::now_iso();
    let s = storage.lock().await;
    let _ = s.update_session_recovery_marker(session_id, json.as_deref(), &now);
}

/// Bulk startup housekeeping: walk every persisted session and reconcile
/// `recovery_marker` against the on-disk journal. Handles both directions
/// in one pass — `Some(marker)` when a leftover journal exists,
/// `None` when the journal was archived (7-day) or cleanly closed in a
/// previous daemon lifecycle that this daemon never observed.
///
/// Runs as part of the startup housekeeping task; cheap enough (one stat
/// per session + one write per dirty row) that a daemon with thousands of
/// sessions still completes it within the readiness budget.
pub async fn persist_recovery_markers(storage: &crate::api::SharedStorage, journals_dir: &Path) {
    let session_ids: Vec<String> = {
        let s = storage.lock().await;
        s.list_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.id)
            .collect()
    };
    for id in session_ids {
        reconcile_persistent_marker(storage, journals_dir, &id).await;
    }
}

/// Scan the daemon's journals directory and log any interrupted runs found.
/// Called once at startup to surface leftover journals. Takes the dir from
/// the daemon state so auxiliary (`--db`) daemons scan their own journals,
/// never the primary daemon's `~/.coderight/journals`.
pub async fn detect_interrupted_journals(dir: &Path) {
    let mut entries = match fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    // Journals older than this are debris from long-gone runs (killed daemons,
    // crashed sessions nobody resumed). They are archived — not deleted — so
    // the journals dir stays bounded and startup reporting stays meaningful.
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let archive_dir = dir.join("archive");
    let mut count = 0usize;
    let mut archived = 0usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let fname = name.to_string_lossy().to_string();
        if !fname.ends_with(".journal") {
            continue;
        }
        let is_stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|age| age > STALE_AFTER)
            .unwrap_or(false);
        if is_stale
            && fs::create_dir_all(&archive_dir).await.is_ok()
            && fs::rename(entry.path(), archive_dir.join(&fname))
                .await
                .is_ok()
        {
            archived += 1;
            continue;
        }
        let session_id = fname.trim_end_matches(".journal");
        eprintln!("[journal] found interrupted journal for session {session_id}");
        count += 1;
    }
    if archived > 0 {
        eprintln!("[journal] archived {archived} stale journal(s) (> 7 days old)");
    }
    if count > 0 {
        eprintln!("[journal] {count} interrupted session(s) detected at startup");
    }
}

/// Convenience: build a timestamped journal entry.
pub fn entry(
    kind: &str,
    session_id: &str,
    turn_id: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "ts": crate::api::now_iso(),
        "kind": kind,
        "session_id": session_id,
        "turn_id": turn_id,
    });
    if let (Some(obj), Some(ext)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in ext {
            obj.insert(k.clone(), val.clone());
        }
    }
    v
}

pub fn text_preview(text: &str, limit: usize) -> String {
    let mut preview = text.chars().take(limit).collect::<String>();
    if text.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}
