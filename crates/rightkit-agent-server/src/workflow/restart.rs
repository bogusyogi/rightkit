//! WP-03b — Restart recovery for the autonomous workflow control plane.
//!
//! Recovery is the **only** path that rebuilds lifecycle state on a cold
//! start; the live path (executor + supervisor) trusts the journal as the
//! single source of truth and rebuilds state incrementally. Recovery
//! therefore has to walk the same deterministic reducer over the same
//! journal events and end at the same [`LifecycleState`] — that is the
//! "WP-03 invariant #3" the supervisor module documents and that the
//! cross-module test suite asserts.
//!
//! Invariants this module enforces:
//!
//! 1. **Active runs only.** `rehydrate` only processes runs whose
//!    `lifecycle_phase` is not `succeeded` / `failed` / `cancelled`.
//!    Terminal runs are not rebuilt — they are already in their final
//!    state and re-running them would re-issue `run_resumed` events
//!    that confuse downstream watchers.
//! 2. **Lease hygiene.** A lease whose holder matches a "dead PID" is
//!    released in one recovery command. The dead-PID check uses a
//!    process-liveness probe on Unix.
//! 3. **Single-source replay.** `replay_single` and `rehydrate` both
//!    call the pure [`reduce`] function from `rightkit_workflow` —
//!    never a parallel state machine. That is the property the
//!    cross-module test asserts: replay-twice must equal replay-once.
//! 4. **All clock reads via the trait.** No `std::time::SystemTime` is
//!    reached from this module; every `created_at` is taken from the
//!    [`Clock`] injected by the caller.
//! 5. **No free-form unwraps.** Production code returns
//!    [`SupervisorError`] for every failure path; tests may use
//!    `expect` for fixture setup.

#![forbid(unsafe_code)]
#![allow(clippy::needless_return)]

use crate::workflow::triggers::format_unix_ms_as_iso;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use rightkit_agent_engine_storage::{
    JournalAppend, LeaseRelease, ProjectionUpdate, Storage, StorageError, WorkflowCommandRow,
    WorkflowJournalRow, WorkflowJournalWriter, WorkflowRepository, WorkflowRunLeaseRow,
    WorkflowRunRow,
};
use rightkit_workflow::control_plane::domain::{
    run_id as domain_run_id, workflow_id as domain_workflow_id,
    workflow_version_id as domain_workflow_version_id, LifecyclePhase, LifecycleState,
};
use rightkit_workflow::control_plane::prelude::{Clock, Revision};
use rightkit_workflow::control_plane::reducer::{reduce, Command, ReducerOutcome};

use crate::api::SharedStorage;
use crate::workflow::supervisor::SupervisorError;

// ─── Outcome enum ───────────────────────────────────────────────────────────

/// One recovery-worker outcome. Variants:
///
/// * `None` — no active runs were found; the worker has nothing to do.
/// * `Rehydrated { run_ids }` — the rehydrate pass processed these runs.
///   The runs were replayed through the reducer, had their leases
///   audited, and received a `run_resumed` journal event.
/// * `Replayed { state }` — a single-run replay produced this lifecycle
///   state. Returned by [`RecoveryWorker::replay_single`].
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryOutcome {
    None,
    Rehydrated { run_ids: Vec<String> },
    Replayed { state: LifecycleState },
}

// ─── Recovery worker ────────────────────────────────────────────────────────

/// Recovery worker. Stateless apart from the injected `Arc<Mutex<Storage>>`
/// and `Arc<dyn Clock>`. Cheap to construct; clone-able by sharing those
/// `Arc`s.
///
/// The worker is the **only** path that performs a "cold start" rebuild
/// of workflow lifecycle state. Its two public methods ([`rehydrate`]
/// and [`replay_single`]) wrap the same private replay helper, so the
/// single-replay and multi-replay code paths cannot diverge.
pub struct RecoveryWorker {
    storage: SharedStorage,
    clock: Arc<dyn Clock>,
}

impl RecoveryWorker {
    /// Build a new recovery worker.
    pub fn new(storage: SharedStorage, clock: Arc<dyn Clock>) -> Self {
        Self { storage, clock }
    }

    /// Cold-start rehydrate: list every active run (`lifecycle_phase`
    /// not in `{succeeded, failed, cancelled}`), replay each run's
    /// journal through the reducer, audit its lease, and append a
    /// `run_resumed` journal event.
    ///
    /// Returns [`RecoveryOutcome::Rehydrated`] with the list of runs
    /// that were processed. Runs whose per-run processing fails are
    /// logged and skipped — a poisoned run must not block the others.
    /// Returns [`RecoveryOutcome::None`] when there are no active runs.
    pub async fn rehydrate(&self) -> Result<RecoveryOutcome, SupervisorError> {
        let storage = self.storage.clone();
        // 1. List active runs. We deliberately do not include the
        //    terminal phases — see invariant #1 in the module docs.
        let active_run_ids: Vec<String> =
            tokio::task::spawn_blocking(move || -> Result<Vec<String>, SupervisorError> {
                let storage_guard = storage.blocking_lock();
                let conn = storage_guard.conn();
                let mut stmt = conn
                    .prepare(
                        "SELECT id
                           FROM workflow_runs
                          WHERE lifecycle_phase NOT IN ('succeeded', 'failed', 'cancelled')
                          ORDER BY started_at ASC
                          LIMIT 100",
                    )
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;
                let ids: Vec<String> = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(StorageError::from)?
                    .collect::<rightkit_db::rusqlite::Result<Vec<_>>>()
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;
                Ok(ids)
            })
            .await
            .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        if active_run_ids.is_empty() {
            debug!(
                target: "coderight.workflow.recovery",
                "recovery: no active runs found"
            );
            return Ok(RecoveryOutcome::None);
        }

        // 2. Process each active run. Per-run failures are logged and
        //    skipped so one bad run does not block the others.
        let mut processed: Vec<String> = Vec::with_capacity(active_run_ids.len());
        for run_id in &active_run_ids {
            match self.process_active_run(run_id).await {
                Ok(()) => processed.push(run_id.clone()),
                Err(e) => warn!(
                    target: "coderight.workflow.recovery",
                    run_id = %run_id,
                    error = %e,
                    "recovery: failed to process active run; continuing"
                ),
            }
        }

        info!(
            target: "coderight.workflow.recovery",
            count = processed.len(),
            "recovery: rehydrate pass complete"
        );
        Ok(RecoveryOutcome::Rehydrated { run_ids: processed })
    }

    /// Single-run replay: replay one run's journal through the reducer
    /// and return the resulting [`LifecycleState`]. Used by the daemon
    /// to surface a run's current state without going through the
    /// multi-run rehydrate path, and by the test suite to assert the
    /// reducer replay is deterministic.
    pub async fn replay_single(&self, run_id: &str) -> Result<RecoveryOutcome, SupervisorError> {
        let storage = self.storage.clone();
        let run_id_owned = run_id.to_string();

        let state: LifecycleState =
            tokio::task::spawn_blocking(move || -> Result<LifecycleState, SupervisorError> {
                let storage_guard = storage.blocking_lock();
                let conn = storage_guard.conn();
                let repo = WorkflowRepository::new(conn);
                let run_row: WorkflowRunRow = repo
                    .find_run(&run_id_owned)
                    .map_err(SupervisorError::Storage)?
                    .ok_or_else(|| {
                        SupervisorError::NotFound(format!("workflow_run {run_id_owned}"))
                    })?;
                let journal = repo
                    .replay_journal(&run_id_owned)
                    .map_err(SupervisorError::Storage)?;
                Ok(replay_journal_through_reducer(&run_row, &journal))
            })
            .await
            .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        Ok(RecoveryOutcome::Replayed { state })
    }

    /// Process one active run: replay its journal, audit its lease for
    /// a dead holder, and append a `run_resumed` journal event.
    async fn process_active_run(&self, run_id: &str) -> Result<(), SupervisorError> {
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id_owned = run_id.to_string();

        tokio::task::spawn_blocking(move || -> Result<(), SupervisorError> {
            let storage_guard = storage.blocking_lock();
            let conn = storage_guard.conn();
            let repo = WorkflowRepository::new(conn);

            // 1. Load journal + run row.
            let journal: Vec<WorkflowJournalRow> = repo
                .replay_journal(&run_id_owned)
                .map_err(SupervisorError::Storage)?;
            let run_row: WorkflowRunRow = repo
                .find_run(&run_id_owned)
                .map_err(SupervisorError::Storage)?
                .ok_or_else(|| SupervisorError::NotFound(format!("workflow_run {run_id_owned}")))?;

            // 2. Rebuild the lifecycle state through the pure reducer.
            //    The reducer is the only authority for state transitions,
            //    so the rebuild is identical to the live path's view of
            //    the same journal.
            let _state = replay_journal_through_reducer(&run_row, &journal);

            // 3. Audit the lease. Recovery records a dead holder in its
            // command payload and clears that exact fenced lease atomically
            // with the lifecycle retry/resume.
            let lease: Option<WorkflowRunLeaseRow> = repo
                .find_run_lease(&run_id_owned)
                .map_err(SupervisorError::Storage)?;
            let dead_lease = lease.as_ref().filter(|row| {
                tokio::runtime::Handle::current().block_on(is_dead_holder(&row.lease_holder))
            });
            let now_ms = clock.now_unix_ms();
            let now_iso = format_unix_ms_as_iso(now_ms);
            let command_kind = if run_row.lifecycle_phase == "suspended" {
                "resume"
            } else {
                "retry"
            };
            let payload = serde_json::json!({
                "recovery": true,
                "lease_lost": dead_lease.is_some(),
                "dead_holder": dead_lease.map(|row| row.lease_holder.as_str()),
            })
            .to_string();
            let event = JournalAppend {
                run_id: &run_id_owned,
                event_kind: command_kind,
                node_id: None,
                payload_json: &payload,
                created_at: &now_iso,
            };
            let projection = ProjectionUpdate {
                run_id: &run_id_owned,
                run_phase: Some("running"),
                release_run_lease: dead_lease.map(|row| LeaseRelease {
                    run_id: &run_id_owned,
                    holder: &row.lease_holder,
                    fence: row.lease_fence,
                }),
                ..Default::default()
            };
            let command_id = format!("wfc_{}", rightkit_agent_engine_protocol::uuid_v7());
            let command = WorkflowCommandRow {
                run_id: run_id_owned.clone(),
                command_id: command_id.clone(),
                expected_revision: run_row.lifecycle_revision,
                command_kind: command_kind.to_string(),
                payload_json: payload.clone(),
                created_at: now_iso.clone(),
            };
            let reducer_command = Command {
                command_id,
                kind: command_kind.to_string(),
                run_id: domain_run_id(&run_id_owned),
                expected_revision: Revision(run_row.lifecycle_revision.max(0) as u64),
                payload_json: payload.clone(),
            };
            let snapshot = LifecycleState {
                workflow_id: domain_workflow_id(&run_row.workflow_id),
                version_id: domain_workflow_version_id(&run_row.workflow_version_id),
                revision: Revision(run_row.lifecycle_revision.max(0) as u64),
                phase: lifecycle_phase_from_str(&run_row.lifecycle_phase),
            };
            WorkflowJournalWriter::new(conn)
                .apply_command_from_snapshot(
                    &snapshot,
                    &reducer_command,
                    &command,
                    "{}",
                    &event,
                    &projection,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .map_err(SupervisorError::Storage)?;

            Ok(())
        })
        .await
        .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        Ok(())
    }
}

// ─── Free helpers ───────────────────────────────────────────────────────────

/// Replay `journal` through the pure reducer and return the resulting
/// [`LifecycleState`]. The starting state is `(workflow_id from run,
/// version_id from run, Revision(0), LifecyclePhase::Pending)` — the
/// run's identity is read from the `workflow_runs` row, and the
/// reducer's job is to advance `revision` (and only `revision` in the
/// current skeleton reducer) per event.
///
/// This helper is the single source of truth for "what state does this
/// run end up in after replay". It is wrapped by both `rehydrate` and
/// `replay_single` so the live and recovery paths cannot diverge.
fn replay_journal_through_reducer(
    run: &WorkflowRunRow,
    journal: &[WorkflowJournalRow],
) -> LifecycleState {
    let mut state = LifecycleState {
        workflow_id: domain_workflow_id(&run.workflow_id),
        version_id: domain_workflow_version_id(&run.workflow_version_id),
        revision: Revision::ZERO,
        phase: LifecyclePhase::Pending,
    };

    for ev in journal {
        // The journal writer assigns the *post*-event revision to the
        // row, so the command we feed the reducer must claim the
        // *pre*-event revision as `expected_revision`.
        let expected = Revision(ev.revision.saturating_sub(1).max(0) as u64);
        let cmd = Command {
            command_id: format!("cmd-{}", ev.seq),
            kind: ev.event_kind.clone(),
            run_id: domain_run_id(&ev.run_id),
            expected_revision: expected,
            payload_json: ev.payload_json.clone(),
        };
        match reduce(&state, &cmd) {
            ReducerOutcome::Applied { next_state, .. } => state = *next_state,
            // A reducer rejection during replay means the journal
            // contains an event that the current state machine cannot
            // accept (e.g. a `run_failed` event against a run whose
            // phase is already terminal). The recovery worker cannot
            // fix that — it must surface the inconsistency to the
            // caller. We stop at this event so the returned state is
            // the last successfully-replayed position.
            ReducerOutcome::Rejected {
                reason,
                observed_revision,
            } => {
                warn!(
                    target: "coderight.workflow.recovery",
                    run_id = %run.workflow_id,
                    seq = ev.seq,
                    event_kind = %ev.event_kind,
                    ?reason,
                    observed_revision = observed_revision.0,
                    "recovery: reducer rejected replay; halting"
                );
                break;
            }
            ReducerOutcome::Duplicate { .. } => {
                // Idempotent replay — the reducer saw a duplicate
                // command id, which means the journal has been re-fed
                // in some prior cycle. Continue past it.
            }
        }
    }

    state
}

fn lifecycle_phase_from_str(phase: &str) -> LifecyclePhase {
    match phase {
        "pending" => LifecyclePhase::Pending,
        "running" => LifecyclePhase::Running,
        "suspended" => LifecyclePhase::Suspended,
        "cancelled" => LifecyclePhase::Cancelled,
        "succeeded" => LifecyclePhase::Succeeded,
        "failed" => LifecyclePhase::Failed,
        _ => LifecyclePhase::Degraded,
    }
}

/// Check process-backed lease holders without treating opaque logical
/// holders as dead. Runtime holders use `pid_<n>` (or a bare numeric PID).
/// A managed `ps` probe performs a permission-safe existence check on Unix;
/// non-zero exit or empty output means definitive death.
async fn is_dead_holder(holder: &str) -> bool {
    let raw = holder
        .strip_prefix("pid_")
        .or_else(|| holder.strip_prefix("pid:"))
        .unwrap_or(holder);
    let Ok(pid) = raw.parse::<i32>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // `ps` observes existence without signalling another process or
        // misclassifying an EPERM result as a dead holder. Route it through
        // the managed process seam, and fail closed on every probe failure.
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: ".".to_string(),
            sandbox_profile: Some("workflow-process-probe".to_string()),
            sandbox_required: false,
            network_required: false,
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: "ps".to_string(),
            argv: vec![
                "-p".to_string(),
                pid.to_string(),
                "-o".to_string(),
                "pid=".to_string(),
            ],
            cwd: context.workspace_root.clone(),
            context,
            env: Vec::new(),
        };
        let Ok(mut child) =
            rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec).await
        else {
            return false;
        };
        let Some(mut stdout) = child.take_stdout() else {
            let _ = child.cancel().await;
            return false;
        };
        let mut output = Vec::with_capacity(32);
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stdout.read_to_end(&mut output),
        )
        .await;
        let status = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
        match (read, status) {
            (Ok(Ok(_)), Ok(Ok(code))) => code != 0 || output.is_empty(),
            _ => {
                let _ = child.cancel().await;
                false
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rightkit_agent_engine_storage::{JournalAppend, ProjectionUpdate};
    use rightkit_agent_engine_storage::{Storage, WorkflowJournalWriter, WorkflowRepository};
    use rightkit_db::rusqlite::{params, Connection};
    use rightkit_workflow::control_plane::clock::TestClock;

    fn open_storage() -> SharedStorage {
        SharedStorage::new(Storage::open_in_memory().expect("in-memory storage"))
    }

    fn seed_run(conn: &Connection, run_id: &str) {
        conn.execute(
            "INSERT INTO workflow_runs(
                id, workflow_id, workflow_version_id,
                lifecycle_phase, lifecycle_revision,
                initiated_by_kind, input_json, started_at, runs_root_durable
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                "wfd-test",
                "wfv-test",
                "queued",
                0_i64,
                "manual",
                "{}",
                "2026-08-07T00:00:00Z",
                1_i64,
            ],
        )
        .expect("seed workflow_runs row");
    }

    /// Append `n` journal events for `run_id` through the public
    /// `WorkflowJournalWriter`. Each event advances the run's
    /// `lifecycle_revision` by one (the writer auto-increments when the
    /// projection omits `run_lifecycle_revision`) and leaves the
    /// lifecycle phase as whatever the seeded row had.
    fn append_journal_events(conn: &Connection, run_id: &str, n: usize) {
        let writer = WorkflowJournalWriter::new(conn);
        // Keep fixture history within closed reducer vocabulary; `advance`
        // belonged to the pre-reducer skeleton.
        let kinds = ["admit", "start", "pause", "resume"];
        for index in 0..n {
            writer
                .append_with_projection(
                    &JournalAppend {
                        run_id,
                        event_kind: kinds[index % kinds.len()],
                        node_id: None,
                        payload_json: "{}",
                        created_at: "2026-08-07T00:00:01Z",
                    },
                    &ProjectionUpdate {
                        run_id,
                        ..Default::default()
                    },
                )
                .expect("append journal event");
        }
    }

    /// `recovery_replays_journal_deterministically`: seed four journal
    /// events, replay twice, and assert the resulting `LifecycleState`s
    /// are equal. This is the property WP-03 invariant #3 depends on —
    /// recovery must reach the same state the live path would have.
    #[tokio::test(flavor = "current_thread")]
    async fn recovery_replays_journal_deterministically() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        let run_id = "wfr_recovery";
        seed_run(conn, run_id);
        append_journal_events(conn, run_id, 4);

        // Sanity: the seeded journal must contain exactly four events.
        let repo = WorkflowRepository::new(conn);
        let journal_rows = repo.replay_journal(run_id).expect("replay journal");
        assert_eq!(
            journal_rows.len(),
            4,
            "test fixture must seed exactly 4 journal events"
        );
        drop(storage_guard);

        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_000_000));
        let worker = RecoveryWorker::new(storage.clone(), clock);

        // Replay the run twice and assert the resulting `LifecycleState`s
        // are byte-for-byte equal. This is the determinism property.
        let r1 = match worker.replay_single(run_id).await.expect("replay 1") {
            RecoveryOutcome::Replayed { state } => state,
            other => panic!("expected RecoveryOutcome::Replayed; got {other:?}"),
        };
        let r2 = match worker.replay_single(run_id).await.expect("replay 2") {
            RecoveryOutcome::Replayed { state } => state,
            other => panic!("expected RecoveryOutcome::Replayed; got {other:?}"),
        };

        assert_eq!(
            r1, r2,
            "replay must be deterministic across calls (WP-03 invariant #3)"
        );
        assert_eq!(
            r1.revision,
            Revision(journal_rows.len() as u64),
            "final revision must equal the number of journal events replayed"
        );
        assert_eq!(
            r1.workflow_id,
            domain_workflow_id("wfd-test"),
            "workflow_id must be lifted from the workflow_runs row"
        );
        assert_eq!(
            r1.version_id,
            domain_workflow_version_id("wfv-test"),
            "version_id must be lifted from the workflow_runs row"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_holder_probe_distinguishes_opaque_live_and_dead() {
        // Probe is async because production uses managed process I/O.
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            assert!(!is_dead_holder("supervisor").await);
            assert!(!is_dead_holder(&format!("pid:{}", std::process::id())).await);
            assert!(is_dead_holder("pid:99999999").await);
            assert!(!is_dead_holder("pid:not-a-pid").await);
        });
    }
}
