//! Session state helpers — small helpers for session persistence and status.
//! Extracted from `api_sessions.rs` to keep handlers focused.

use crate::api::{DaemonState, LiveSession};
use std::sync::Arc;

/// Check if a persisted session row exists.
pub(crate) async fn persisted_session_exists(state: &DaemonState, id: &str) -> bool {
    if let Some(storage) = &state.storage {
        let storage = storage.clone();
        let id = id.to_string();
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
    } else {
        false
    }
}

/// Load a persisted session row, if it exists.
pub(crate) async fn persisted_session_row(
    state: &DaemonState,
    id: &str,
) -> Option<rightkit_agent_engine_storage::SessionRow> {
    if let Some(storage) = &state.storage {
        let storage = storage.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || storage.blocking_lock().get_session(&id).ok().flatten())
            .await
            .unwrap_or(None)
    } else {
        None
    }
}

/// Get all descendant live sessions for a parent.
pub(crate) fn descendant_live_sessions(
    state: &DaemonState,
    parent_id: &str,
) -> Vec<Arc<LiveSession>> {
    crate::api::lock_recover(&state.sessions, "sessions")
        .values()
        .filter(|s| s.parent_session_id.as_deref() == Some(parent_id))
        .cloned()
        .collect()
}
