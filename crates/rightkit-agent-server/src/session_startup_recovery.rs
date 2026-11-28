//! Daemon-start reconciliation of session status (sessions rules 4 and 34).
//!
//! There is no turn journal. The crash record is `sessions.status`: a session
//! that was running when the daemon died is stamped `interrupted`, and
//! `resume_session` adds the recovery note for it. A status that already
//! records an end (an idle chat, a dormant or archived one, or a child that
//! finished as `completed`, `failed` or `cancelled`) is left alone.

/// A session status that records a settled state: nothing was running.
pub(crate) fn status_is_settled(status: &str) -> bool {
    matches!(
        status,
        "dormant"
            | "done"
            | "deleted"
            | "archived"
            | "idle"
            | ""
            | "completed"
            | "failed"
            | "cancelled"
            | "interrupted"
    )
}

/// Stamp every session whose status is not settled as `interrupted`.
///
/// The write is conditional on the row still being exactly what the scan read
/// (same status and `updated_at`, and older than `scan_started`), so a turn
/// accepted while the scan runs and persisted as `running` is never replaced by
/// a stale `interrupted` (ra-008). Returns how many rows were stamped.
pub(crate) fn mark_unsettled_sessions_interrupted(
    store: &rightkit_agent_engine_storage::Storage,
    scan_started: &str,
    now: &str,
) -> usize {
    mark_unsettled_sessions_interrupted_where(store, scan_started, now, |_| true)
}

/// [`mark_unsettled_sessions_interrupted`] for the sessions `abandoned` says nobody is running.
fn mark_unsettled_sessions_interrupted_where(
    store: &rightkit_agent_engine_storage::Storage,
    scan_started: &str,
    now: &str,
    abandoned: impl Fn(&str) -> bool,
) -> usize {
    let Ok(sessions) = store.list_sessions() else {
        return 0;
    };
    let mut stamped = 0;
    for session in sessions {
        if status_is_settled(&session.status) || !abandoned(&session.id) {
            continue;
        }
        if store
            .update_session_status_if_unchanged(
                &session.id,
                "interrupted",
                now,
                &session.status,
                &session.updated_at,
                Some(scan_started),
            )
            .unwrap_or(false)
        {
            stamped += 1;
        }
    }
    stamped
}

/// In-process host start: stamp the sessions a killed in-process run left `running` as
/// `interrupted`, as the daemon does at its own start. A one-shot run has no daemon to do it
/// later, so the session would read `running` forever. Other in-process hosts may be alive on
/// the same account (issue 510): a session is stamped only when no live process holds its lease
/// (`session_lease`), so a session another run is working on is never touched and a crashed one
/// is settled by the next run that starts, whatever else is running. Returns how many rows were
/// stamped.
pub(crate) fn recover_for_inline_host(store: &rightkit_agent_engine_storage::Storage) -> usize {
    crate::session_lease::prune_stale();
    let scan_started = rightkit_agent_engine_protocol::now_iso();
    mark_unsettled_sessions_interrupted_where(
        store,
        &scan_started,
        &rightkit_agent_engine_protocol::now_iso(),
        crate::session_lease::is_free,
    )
}
