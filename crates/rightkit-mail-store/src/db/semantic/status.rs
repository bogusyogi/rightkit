//! What the Settings panel and the search box need to know about semantic
//! search: one state, per-account progress, and nothing that came from a
//! message. The state is derived from plain inputs, so it is testable without
//! a model, a runtime or an app.

/// The one state shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticState {
    Off,
    ModelMissing,
    RuntimeMissing,
    Indexing,
    Ready,
    Error,
}

/// What the background job last reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobReport {
    /// Nothing wrong (not started, running, or finished).
    Fine,
    /// The job stopped with a short code (for example `embedder_load`).
    Failed(String),
}

/// Indexed and total messages of one account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountProgress {
    pub account_id: String,
    pub indexed: i64,
    pub total: i64,
    pub done: bool,
}

/// Combine the switch, the machine's readiness, the job report and the
/// progress into the state. Order matters: off beats everything, a missing
/// runtime beats a missing model (it blocks search even with a model), and a
/// failed job shows as an error only when the machine is otherwise ready.
pub fn derive_state(
    enabled: bool,
    runtime_available: bool,
    model_installed: bool,
    job: &JobReport,
    accounts: &[AccountProgress],
) -> SemanticState {
    if !enabled {
        return SemanticState::Off;
    }
    if !runtime_available {
        return SemanticState::RuntimeMissing;
    }
    if !model_installed {
        return SemanticState::ModelMissing;
    }
    if matches!(job, JobReport::Failed(_)) {
        return SemanticState::Error;
    }
    if accounts.iter().all(|a| a.done) {
        SemanticState::Ready
    } else {
        SemanticState::Indexing
    }
}

/// Progress of every account, oldest first.
pub fn all_progress(conn: &Connection) -> Result<Vec<AccountProgress>, String> {
    let ids: Vec<String> = crate::db::query_map_collect(
        conn,
        "SELECT id FROM accounts ORDER BY created_at, id",
        [],
        "semantic_status_accounts",
        |r| r.get(0),
    )?;
    ids.into_iter()
        .map(|account_id| {
            let p = index::index_progress(conn, &account_id)?;
            Ok(AccountProgress {
                account_id,
                indexed: p.indexed_count,
                total: p.total_messages,
                done: p.done,
            })
        })
        .collect()
}
