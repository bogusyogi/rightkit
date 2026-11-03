//! What the Settings panel and the search box need to know about semantic
//! search: one state, per-account progress, and nothing that came from a
//! message. The state is derived from plain inputs, so it is testable without
//! a model, a runtime or an app.

use rightkit_db::rusqlite::Connection;

use super::index;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::semantic::embedder::STORED_DIM;
    use crate::db::semantic::index::run_index_step;
    use crate::db::semantic::index::test_support::{conn_with_schema, seed_messages, FakeEmbedder};

    fn account(done: bool) -> AccountProgress {
        AccountProgress {
            account_id: "a".into(),
            indexed: 0,
            total: 1,
            done,
        }
    }

    #[test]
    fn off_beats_everything() {
        let failed = JobReport::Failed("x".into());
        let s = derive_state(false, false, false, &failed, &[account(false)]);
        assert_eq!(s, SemanticState::Off);
    }

    #[test]
    fn missing_runtime_beats_missing_model() {
        let s = derive_state(true, false, false, &JobReport::Fine, &[]);
        assert_eq!(s, SemanticState::RuntimeMissing);
        let s = derive_state(true, true, false, &JobReport::Fine, &[]);
        assert_eq!(s, SemanticState::ModelMissing);
    }

    #[test]
    fn failed_job_is_an_error_only_when_ready_to_run() {
        let failed = JobReport::Failed("embedder_load".into());
        let s = derive_state(true, true, true, &failed, &[account(true)]);
        assert_eq!(s, SemanticState::Error);
        let s = derive_state(true, false, true, &failed, &[]);
        assert_eq!(s, SemanticState::RuntimeMissing);
    }

    #[test]
    fn indexing_until_every_account_is_done() {
        let fine = JobReport::Fine;
        let s = derive_state(true, true, true, &fine, &[account(true), account(false)]);
        assert_eq!(s, SemanticState::Indexing);
        let s = derive_state(true, true, true, &fine, &[account(true)]);
        assert_eq!(s, SemanticState::Ready);
        let s = derive_state(true, true, true, &fine, &[]);
        assert_eq!(s, SemanticState::Ready, "no accounts: nothing to index");
    }

    #[test]
    fn state_serializes_as_snake_case() {
        let json = serde_json::to_string(&SemanticState::RuntimeMissing).unwrap();
        assert_eq!(json, "\"runtime_missing\"");
    }

    #[test]
    fn progress_counts_each_account_and_never_carries_content() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 3);
        seed_messages(&conn, "a2", 20);
        let e = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        run_index_step(&conn, &conn, "a1", &e).unwrap();
        run_index_step(&conn, &conn, "a2", &e).unwrap();
        let progress = all_progress(&conn).unwrap();
        assert_eq!(progress.len(), 2);
        let a1 = progress.iter().find(|p| p.account_id == "a1").unwrap();
        assert_eq!((a1.indexed, a1.total, a1.done), (3, 3, true));
        let a2 = progress.iter().find(|p| p.account_id == "a2").unwrap();
        assert_eq!((a2.indexed, a2.total, a2.done), (16, 20, false));
    }
}
