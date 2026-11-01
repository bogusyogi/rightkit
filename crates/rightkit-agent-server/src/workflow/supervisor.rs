//! WP-03a — autonomous workflow control plane (ADR-0021). RunSupervisor owns lease fencing and queue discipline.
//!
//! This module is the **only** path that mutates `workflow_run_lease` and is
//! the terminal run-command authority. Every SQLite access is funneled through
//! [`tokio::task::spawn_blocking`] so the supervisor never stalls an async
//! task on a blocking lock. The lease `fence` is the monotonic generation
//! counter that protects against stolen leases: a supervisor who has been
//! holding a lease for a run, dies, and lets another supervisor reclaim it
//! will see a fence bump that aborts any stale `release_run` it might attempt
//! afterwards.

#![forbid(unsafe_code)]

use crate::workflow::triggers::format_unix_ms_as_iso;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rightkit_db::rusqlite::{params, Connection};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::api::SharedStorage;
use rightkit_agent_engine_storage::{
    JournalAppend, LeaseRelease, ProjectionUpdate, Storage, StorageError, WorkflowJournalWriter,
    WorkflowNodeStateRow, WorkflowRepository, WorkflowRunLeaseRow, WorkflowRunRow,
    WorkflowVersionRow,
};
use rightkit_workflow::control_plane::prelude::FailureClass;
use rightkit_workflow::control_plane::prelude::{
    Clock, CompiledPlan, FeatureGate, LifecyclePhase, LifecycleState, Prefix, Revision, WorkflowId,
    WorkflowVersionId,
};
use rightkit_workflow::control_plane::reducer::Command;

/// Lease TTL knobs supplied by the daemon. `initial_secs` is what a fresh claim
/// writes into `workflow_run_lease.lease_expires_at`; `renewal_secs` is the
/// shorter interval at which a long-running executor re-extends the lease via
/// a follow-up `claim_run` call (WP-04 / WP-05 territory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorLeaseTtl {
    pub initial_secs: u64,
    pub renewal_secs: u64,
}

/// Outcome of one supervisor tick. The daemon only acts on `Claimed`; the
/// other two variants are observational.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOnceOutcome {
    /// Nothing to claim.
    Empty,
    /// One or more runs were claimed and are now leased to this supervisor.
    Claimed { run_ids: Vec<String> },
    /// The feature gate is off. Returned as a distinct outcome so a production
    /// daemon can never accidentally enter the claim path.
    Disabled,
}

/// Everything a worker needs to dispatch the next node for the claimed run.
#[derive(Debug, Clone)]
pub struct ClaimOutcome {
    pub run_id: String,
    pub lifecycle_snapshot: LifecycleState,
    pub plan: CompiledPlan,
    pub node_states: Vec<WorkflowNodeStateRow>,
    pub lease_fence: i64,
}

/// Typed failures the supervisor can surface. The `Storage` variant wraps any
/// underlying [`rightkit_agent_engine_storage::StorageError`] so callers can downcast as
/// needed; the others cover supervisor-only invariants.
#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("fence conflict for run {run_id}: expected <= {expected}, found {found}")]
    FenceConflict {
        run_id: String,
        expected: i64,
        found: i64,
    },
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid claim window: {0}")]
    InvalidClaim(String),
    #[error("supervisor internal error: {0}")]
    Internal(String),
}

/// The workflow lease-and-queue authority. Construction is cheap; the
/// `fence_state` map starts empty and is populated lazily on each successful
/// claim. Cloning a `RunSupervisor` is not supported — wrap it in an `Arc` if
/// the daemon needs to share it across tasks.
///
/// `storage` is wrapped in a `std::sync::Mutex` so the supervisor can hand
/// the live handle to `tokio::task::spawn_blocking` while keeping the
/// single-writer SQLite invariant intact: the mutex serializes every
/// connection reach into the same `Connection` that the daemon owns.
pub struct RunSupervisor {
    storage: SharedStorage,
    feature: FeatureGate,
    clock: Arc<dyn Clock>,
    fence_state: Mutex<HashMap<String, i64>>,
}

impl RunSupervisor {
    /// Build a supervisor over daemon's single shared SQLite writer.
    /// the resolved [`FeatureGate`], and a [`Clock`] (production:
    /// [`SystemClock`], tests: [`TestClock`]).
    pub fn new(storage: SharedStorage, feature: FeatureGate, clock: Arc<dyn Clock>) -> Self {
        Self {
            storage,
            feature,
            clock,
            fence_state: Mutex::new(HashMap::new()),
        }
    }

    /// Read the highest fence this supervisor has cached for `run_id`.
    /// Returns 0 if the run has never been seen (or has been released and
    /// forgotten). Callers outside this module use it for diagnostic /
    /// recovery reporting only.
    pub fn last_known_fence(&self, run_id: &str) -> i64 {
        match self.fence_state.lock() {
            Ok(state) => state.get(run_id).copied().unwrap_or(0),
            Err(poisoned) => {
                warn!(error = %poisoned, "fence_state mutex poisoned; returning 0");
                0
            }
        }
    }

    /// One supervisor tick: find unclaimed / expired leases and claim each.
    /// The feature gate short-circuits to [`RunOnceOutcome::Disabled`] before
    /// any SQL runs, so a misconfigured daemon cannot accidentally enter the
    /// claim path.
    pub async fn claim_ready(
        &self,
        ttl: SupervisorLeaseTtl,
    ) -> Result<Vec<ClaimOutcome>, SupervisorError> {
        if !self.feature.is_enabled() {
            debug!("run_once: feature gate disabled; short-circuiting");
            return Ok(Vec::new());
        }

        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let active_runs: Vec<String> =
            tokio::task::spawn_blocking(move || -> Result<Vec<String>, SupervisorError> {
                let storage_guard = storage.blocking_lock();
                let conn = storage_guard.conn();
                let now_iso = format_unix_ms_as_iso(clock.now_unix_ms());
                let mut stmt = conn
                    .prepare(
                        "SELECT wr.id
                           FROM workflow_runs wr
                          WHERE wr.lifecycle_phase NOT IN ('succeeded', 'failed', 'cancelled')
                            AND (
                              NOT EXISTS (
                                SELECT 1 FROM workflow_run_lease l WHERE l.run_id = wr.id
                              )
                              OR EXISTS (
                                SELECT 1 FROM workflow_run_lease l
                                 WHERE l.run_id = wr.id AND l.lease_expires_at <= ?1
                              )
                            )
                          ORDER BY wr.started_at ASC
                          LIMIT 100",
                    )
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;
                let ids: Vec<String> = stmt
                    .query_map(params![now_iso], |row| row.get::<_, String>(0))
                    .map_err(StorageError::from)?
                    .collect::<rightkit_db::rusqlite::Result<Vec<_>>>()
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;
                Ok(ids)
            })
            .await
            .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        if active_runs.is_empty() {
            return Ok(Vec::new());
        }

        let mut claimed: Vec<ClaimOutcome> = Vec::with_capacity(active_runs.len());
        let holder = format!("pid_{}", std::process::id());
        for run_id in active_runs {
            match self.claim_run(&run_id, &holder, ttl.initial_secs).await {
                Ok(outcome) => claimed.push(outcome),
                Err(SupervisorError::FenceConflict {
                    run_id: id,
                    expected,
                    found,
                }) => {
                    debug!(
                        run_id = %id,
                        expected,
                        found,
                        "run_once: run was claimed concurrently by another supervisor; skipping"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(run_id = %run_id, error = %e, "run_once: claim_run failed");
                    return Err(e);
                }
            }
        }

        if claimed.is_empty() {
            return Ok(Vec::new());
        }
        info!(count = claimed.len(), "run_once: claimed runs");
        Ok(claimed)
    }

    /// Compatibility observation wrapper for callers that only need ids.
    pub async fn run_once(
        &self,
        ttl: SupervisorLeaseTtl,
    ) -> Result<RunOnceOutcome, SupervisorError> {
        if !self.feature.is_enabled() {
            return Ok(RunOnceOutcome::Disabled);
        }
        let claimed = self.claim_ready(ttl).await?;
        if claimed.is_empty() {
            Ok(RunOnceOutcome::Empty)
        } else {
            Ok(RunOnceOutcome::Claimed {
                run_ids: claimed.into_iter().map(|claim| claim.run_id).collect(),
            })
        }
    }

    /// CAS-fenced lease claim. If the row's existing fence is higher than
    /// this supervisor's cached fence, abort with [`SupervisorError::FenceConflict`]
    /// — that is the protocol-level signal that another supervisor stole the
    /// lease out from under us (typically after our process restarted and a
    /// peer observed the gap).
    pub async fn claim_run(
        &self,
        run_id: &str,
        holder: &str,
        ttl_secs: u64,
    ) -> Result<ClaimOutcome, SupervisorError> {
        if ttl_secs == 0 {
            return Err(SupervisorError::InvalidClaim(format!(
                "ttl_secs must be > 0 (got {ttl_secs})"
            )));
        }

        let known_fence = self.last_known_fence(run_id);
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id_owned = run_id.to_string();
        let holder_owned = holder.to_string();

        let payload =
            tokio::task::spawn_blocking(move || -> Result<ClaimPayload, SupervisorError> {
                let storage_guard = storage.blocking_lock();
                let conn: &Connection = storage_guard.conn();
                let repo = WorkflowRepository::new(conn);

                let run_row: WorkflowRunRow = repo
                    .find_run(&run_id_owned)
                    .map_err(SupervisorError::Storage)?
                    .ok_or_else(|| {
                        SupervisorError::NotFound(format!("workflow_run {run_id_owned}"))
                    })?;

                let now_ms = clock.now_unix_ms();
                let expires_ms = now_ms.saturating_add((ttl_secs as i64).saturating_mul(1000));
                let now_iso = format_unix_ms_as_iso(now_ms);
                let expires_iso = format_unix_ms_as_iso(expires_ms);

                // Compare-and-swap claim: the lease expiry and cached fence
                // are part of the write guard. A read followed by an
                // unconditional upsert lets concurrent dispatchers both win.
                let new_fence = known_fence.saturating_add(1);
                let changed = conn
                    .execute(
                        "INSERT INTO workflow_run_lease(
                        run_id, lease_holder, lease_expires_at, lease_fence, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(run_id) DO UPDATE SET
                         lease_holder = excluded.lease_holder,
                         lease_expires_at = excluded.lease_expires_at,
                         lease_fence = workflow_run_lease.lease_fence + 1,
                         updated_at = excluded.updated_at
                     WHERE workflow_run_lease.lease_expires_at <= ?6
                        OR workflow_run_lease.lease_fence <= ?7",
                        params![
                            run_id_owned,
                            holder_owned,
                            expires_iso,
                            new_fence,
                            now_iso,
                            now_iso,
                            known_fence,
                        ],
                    )
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;

                if changed == 0 {
                    let found = repo
                        .find_run_lease(&run_id_owned)
                        .map_err(SupervisorError::Storage)?
                        .map(|lease| lease.lease_fence)
                        .unwrap_or(known_fence);
                    return Err(SupervisorError::FenceConflict {
                        run_id: run_id_owned,
                        expected: known_fence,
                        found,
                    });
                }

                let written_lease = repo
                    .find_run_lease(&run_id_owned)
                    .map_err(SupervisorError::Storage)?
                    .ok_or_else(|| {
                        SupervisorError::NotFound(format!("workflow_run_lease {run_id_owned}"))
                    })?;
                let actual_fence = written_lease.lease_fence;

                let mut stmt = conn
                    .prepare(
                        "SELECT run_id, node_id, phase, attempt, lease_holder,
                                lease_expires_at, last_reducer_revision, last_event_seq,
                                last_failure_class, last_updated_at
                           FROM workflow_node_states WHERE run_id = ?1",
                    )
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;
                let node_states: Vec<WorkflowNodeStateRow> = stmt
                    .query_map(params![run_id_owned], |row| {
                        Ok(WorkflowNodeStateRow {
                            run_id: row.get(0)?,
                            node_id: row.get(1)?,
                            phase: row.get(2)?,
                            attempt: row.get(3)?,
                            lease_holder: row.get(4)?,
                            lease_expires_at: row.get(5)?,
                            last_reducer_revision: row.get(6)?,
                            last_event_seq: row.get(7)?,
                            last_failure_class: row.get(8)?,
                            last_updated_at: row.get(9)?,
                        })
                    })
                    .map_err(StorageError::from)?
                    .collect::<rightkit_db::rusqlite::Result<Vec<_>>>()
                    .map_err(StorageError::from)
                    .map_err(SupervisorError::Storage)?;

                let version_row: WorkflowVersionRow = repo
                    .find_version(&run_row.workflow_version_id)
                    .map_err(SupervisorError::Storage)?
                    .ok_or_else(|| {
                        SupervisorError::NotFound(format!(
                            "workflow_version {}",
                            run_row.workflow_version_id
                        ))
                    })?;

                let plan: CompiledPlan = serde_json::from_str(&version_row.compiled_plan_json)
                    .map_err(|e| {
                        SupervisorError::Internal(format!(
                            "compiled_plan decode failed for {}: {e}",
                            version_row.id
                        ))
                    })?;

                let lifecycle_snapshot = LifecycleState {
                    workflow_id: WorkflowId::new(
                        Prefix::WorkflowDefinition,
                        run_row.workflow_id.clone(),
                    ),
                    version_id: WorkflowVersionId::new(
                        Prefix::WorkflowVersion,
                        run_row.workflow_version_id.clone(),
                    ),
                    revision: Revision(run_row.lifecycle_revision.max(0) as u64),
                    phase: lifecycle_phase_from_str(&run_row.lifecycle_phase),
                };

                Ok(ClaimPayload {
                    run_id: run_id_owned,
                    lifecycle_snapshot,
                    plan,
                    node_states,
                    lease_fence: actual_fence,
                })
            })
            .await
            .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        // Update the cached fence on success. Cache the *actual* fence we
        // observed rather than the one we computed, so a concurrent bump
        // that the CAS swallowed is reflected immediately.
        match self.fence_state.lock() {
            Ok(mut state) => {
                state.insert(payload.run_id.clone(), payload.lease_fence);
            }
            Err(poisoned) => {
                return Err(SupervisorError::Internal(format!(
                    "fence_state poisoned: {poisoned}"
                )));
            }
        }

        info!(
            run_id = %payload.run_id,
            lease_fence = payload.lease_fence,
            "run claimed"
        );
        Ok(ClaimOutcome {
            run_id: payload.run_id,
            lifecycle_snapshot: payload.lifecycle_snapshot,
            plan: payload.plan,
            node_states: payload.node_states,
            lease_fence: payload.lease_fence,
        })
    }

    /// Extend an owned lease without changing its fencing generation.
    pub async fn renew_run_lease(
        &self,
        run_id: &str,
        holder: &str,
        fence: i64,
        ttl_secs: u64,
    ) -> Result<bool, SupervisorError> {
        if ttl_secs == 0 {
            return Err(SupervisorError::InvalidClaim(
                "renewal ttl must be positive".into(),
            ));
        }
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id = run_id.to_owned();
        let holder = holder.to_owned();
        tokio::task::spawn_blocking(move || {
            let storage = storage.blocking_lock();
            let now_ms = clock.now_unix_ms();
            let now = format_unix_ms_as_iso(now_ms);
            let expires = format_unix_ms_as_iso(
                now_ms.saturating_add((ttl_secs as i64).saturating_mul(1_000)),
            );
            storage
                .conn()
                .execute(
                    "UPDATE workflow_run_lease
                     SET lease_expires_at=MAX(lease_expires_at,?1),updated_at=?2
                     WHERE run_id=?3 AND lease_holder=?4 AND lease_fence=?5
                       AND lease_expires_at>?2",
                    params![expires, now, run_id, holder, fence],
                )
                .map(|changed| changed == 1)
                .map_err(StorageError::from)
                .map_err(SupervisorError::Storage)
        })
        .await
        .map_err(|error| SupervisorError::Internal(format!("lease renewal join: {error}")))?
    }

    /// Drop the lease and append a terminal journal event. `terminal == None`
    /// is treated as success (`run_succeeded`); `terminal == Some(fc)`
    /// emits `run_failed` with `fc` recorded into the run's terminal failure
    /// class. The cached fence is forgotten so a future `claim_run` starts
    /// from a clean slate.
    pub async fn release_run(
        &self,
        run_id: &str,
        holder: &str,
        terminal: Option<FailureClass>,
    ) -> Result<(), SupervisorError> {
        let expected_fence = self.last_known_fence(run_id);
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id_owned = run_id.to_string();
        let holder_owned = holder.to_string();
        let (event_kind, next_phase, failure_class_str): (String, String, Option<String>) =
            match terminal {
                None => ("run_succeeded".to_string(), "succeeded".to_string(), None),
                Some(fc) => (
                    "run_failed".to_string(),
                    "failed".to_string(),
                    Some(format!("{fc:?}")),
                ),
            };
        // Clone the strings the closure needs so the post-closure `info!`
        // can still borrow them (the closure's `move` would otherwise
        // consume them).
        let event_kind_for_closure = event_kind.clone();
        let next_phase_for_closure = next_phase.clone();
        let failure_class_str_for_closure = failure_class_str.clone();

        tokio::task::spawn_blocking(move || -> Result<(), SupervisorError> {
            let storage_guard = storage.blocking_lock();
            let conn = storage_guard.conn();
            let repo = WorkflowRepository::new(conn);
            let run = repo
                .find_run(&run_id_owned)?
                .ok_or_else(|| SupervisorError::NotFound(format!("workflow_run {run_id_owned}")))?;

            let now_ms = clock.now_unix_ms();
            let now_iso = format_unix_ms_as_iso(now_ms);

            let event = JournalAppend {
                run_id: &run_id_owned,
                event_kind: &event_kind_for_closure,
                node_id: None,
                payload_json: "{}",
                created_at: &now_iso,
            };
            let projection = ProjectionUpdate {
                run_id: &run_id_owned,
                run_phase: Some(&next_phase_for_closure),
                run_lifecycle_revision: None,
                run_terminal_at: Some(&now_iso),
                run_terminal_failure_class: failure_class_str_for_closure.as_deref(),
                node: None,
                node_attempt: None,
                node_lease: None,
                node_last_failure_class: None,
                run_lease: None,
                release_run_lease: Some(LeaseRelease {
                    run_id: &run_id_owned,
                    holder: &holder_owned,
                    fence: expected_fence,
                }),
            };
            let command_id = format!("wfc_{}", rightkit_agent_engine_protocol::uuid_v7());
            let command_kind = if failure_class_str_for_closure.is_some() {
                "fail"
            } else {
                "complete"
            };
            let command = rightkit_agent_engine_storage::WorkflowCommandRow {
                run_id: run_id_owned.clone(),
                command_id: command_id.clone(),
                expected_revision: run.lifecycle_revision,
                command_kind: command_kind.to_string(),
                payload_json: "{}".to_string(),
                created_at: now_iso.clone(),
            };
            let reducer_command = Command {
                command_id,
                kind: command_kind.to_string(),
                run_id: rightkit_workflow::control_plane::prelude::RunId::new(
                    Prefix::WorkflowRun,
                    run_id_owned.clone(),
                ),
                expected_revision: Revision(run.lifecycle_revision.max(0) as u64),
                payload_json: "{}".to_string(),
            };
            let snapshot = LifecycleState {
                workflow_id: WorkflowId::new(Prefix::WorkflowDefinition, run.workflow_id),
                version_id: WorkflowVersionId::new(
                    Prefix::WorkflowVersion,
                    run.workflow_version_id,
                ),
                revision: Revision(run.lifecycle_revision.max(0) as u64),
                phase: lifecycle_phase_from_str(&run.lifecycle_phase),
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
                .map_err(|err| match err {
                    StorageError::NotFound(_) => SupervisorError::FenceConflict {
                        run_id: run_id_owned.clone(),
                        expected: expected_fence,
                        found: expected_fence.saturating_add(1),
                    },
                    other => SupervisorError::Storage(other),
                })?;
            Ok(())
        })
        .await
        .map_err(|e| SupervisorError::Internal(format!("spawn_blocking join: {e}")))??;

        if let Ok(mut state) = self.fence_state.lock() {
            state.remove(run_id);
        }

        info!(run_id, event_kind = %event_kind, "run released");
        Ok(())
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

struct ClaimPayload {
    run_id: String,
    lifecycle_snapshot: LifecycleState,
    plan: CompiledPlan,
    node_states: Vec<WorkflowNodeStateRow>,
    lease_fence: i64,
}

/// Map the TEXT `lifecycle_phase` column into the typed enum. Unknown values
/// fall back to [`LifecyclePhase::Pending`] — the supervisor never invents a
/// phase that isn't in the workflow vocabulary.
fn lifecycle_phase_from_str(s: &str) -> LifecyclePhase {
    match s {
        "pending" => LifecyclePhase::Pending,
        "running" => LifecyclePhase::Running,
        "suspended" => LifecyclePhase::Suspended,
        "cancelled" => LifecyclePhase::Cancelled,
        "succeeded" => LifecyclePhase::Succeeded,
        "failed" => LifecyclePhase::Failed,
        "degraded" => LifecyclePhase::Degraded,
        _ => LifecyclePhase::Pending,
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_storage::Storage;
    use rightkit_workflow::control_plane::clock::TestClock;
    use rightkit_workflow::control_plane::prelude::CapabilitySet;

    fn open_storage() -> SharedStorage {
        SharedStorage::new(Storage::open_in_memory().expect("open_in_memory"))
    }

    fn seed_run(conn: &Connection, run_id: &str, workflow_version_id: &str) {
        conn.execute(
            "INSERT INTO workflow_runs(
                id, workflow_id, workflow_version_id,
                lifecycle_phase, lifecycle_revision,
                initiated_by_kind, input_json, started_at, runs_root_durable
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                "wfd-seed",
                workflow_version_id,
                "running",
                0_i64,
                "manual",
                "{}",
                "2026-08-07T00:00:00Z",
                1_i64,
            ],
        )
        .expect("seed workflow_run");
    }

    fn seed_version(conn: &Connection, version_id: &str) {
        let plan = CompiledPlan {
            entry_node: "n1".to_string(),
            nodes: Vec::new(),
            effective_capabilities: CapabilitySet::empty(),
        };
        let plan_json = serde_json::to_string(&plan).expect("encode plan");
        conn.execute(
            "INSERT INTO workflow_versions(
                id, workflow_id, version_number, content_hash,
                definition_json, compiled_plan_json, publication_state,
                created_at, published_at, created_by, notes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                version_id,
                "wfd-seed",
                1_i64,
                "sha256:deadbeef",
                "{}",
                plan_json,
                "published",
                "2026-08-07T00:00:00Z",
                Some("2026-08-07T00:00:00Z"),
                "system",
                "",
            ],
        )
        .expect("seed workflow_version");
    }

    fn build_supervisor(storage: SharedStorage, feature_enabled: bool) -> RunSupervisor {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_700_000_000_000));
        let feature = FeatureGate::from_bool(feature_enabled);
        RunSupervisor::new(storage, feature, clock)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn claim_then_release_round_trip() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        seed_run(conn, "wfr_test", "wfv_seed");
        seed_version(conn, "wfv_seed");
        drop(storage_guard);

        let sup = build_supervisor(storage.clone(), true);

        let outcome = sup
            .claim_run("wfr_test", "supervisor", 60)
            .await
            .expect("claim");
        assert_eq!(outcome.run_id, "wfr_test");
        assert_eq!(outcome.lease_fence, 1);
        assert_eq!(outcome.lifecycle_snapshot.revision, Revision(0));
        assert_eq!(outcome.lifecycle_snapshot.phase, LifecyclePhase::Running);
        assert_eq!(outcome.node_states.len(), 0);
        assert_eq!(outcome.plan.entry_node, "n1");
        assert_eq!(sup.last_known_fence("wfr_test"), 1);

        sup.release_run("wfr_test", "supervisor", None)
            .await
            .expect("release");

        let repo_guard = storage.try_lock().expect("storage lock");
        let repo = WorkflowRepository::new(repo_guard.conn());
        let lease = repo.find_run_lease("wfr_test").expect("find lease");
        assert!(lease.is_none(), "lease row must be deleted after release");

        let journal = repo.replay_journal("wfr_test").expect("replay journal");
        assert_eq!(journal.len(), 1, "exactly one terminal journal event");
        assert_eq!(journal[0].event_kind, "run_succeeded");
        assert_eq!(journal[0].node_id, None);

        let run = repo
            .find_run("wfr_test")
            .expect("find run")
            .expect("present");
        assert_eq!(run.lifecycle_phase, "succeeded");
        assert_eq!(run.lifecycle_revision, 1);
        assert!(run.terminal_at.is_some());
        drop(repo_guard);

        assert_eq!(
            sup.last_known_fence("wfr_test"),
            0,
            "cached fence forgotten after release"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fence_conflict_on_stale_state() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        seed_run(conn, "wfr_test", "wfv_seed");
        seed_version(conn, "wfv_seed");
        // Release the synchronous fixture lock before awaiting supervisor
        // work; claim_run acquires same mutex inside spawn_blocking.
        drop(storage_guard);

        let sup = build_supervisor(storage.clone(), true);

        let outcome = sup
            .claim_run("wfr_test", "supervisor", 60)
            .await
            .expect("first claim");
        assert_eq!(outcome.lease_fence, 1);

        // Externally bump fence to 99 (simulates a peer supervisor that
        // reclaimed the lease after we died).
        let storage_guard = storage.try_lock().expect("storage lock");
        let rows = storage_guard
            .conn()
            .execute(
                "UPDATE workflow_run_lease SET lease_fence = 99 WHERE run_id = ?1",
                params!["wfr_test"],
            )
            .expect("bump fence");
        assert_eq!(rows, 1);
        drop(storage_guard);
        let err = sup
            .claim_run("wfr_test", "supervisor", 60)
            .await
            .expect_err("fence conflict");
        match err {
            SupervisorError::FenceConflict {
                run_id,
                expected,
                found,
            } => {
                assert_eq!(run_id, "wfr_test");
                assert_eq!(expected, 1, "expected cached fence");
                assert_eq!(found, 99, "found bumped fence");
            }
            other => panic!("expected FenceConflict, got {other:?}"),
        }

        // Cache must NOT be updated on conflict — the supervisor must keep
        // observing its own known fence so the next attempt is still a
        // conflict, not silently upgraded to 99.
        assert_eq!(sup.last_known_fence("wfr_test"), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_supervisors_only_one_claims_active_lease() {
        let storage = open_storage();
        {
            let guard = storage.try_lock().expect("storage lock");
            seed_run(guard.conn(), "wfr_race", "wfv_seed");
            seed_version(guard.conn(), "wfv_seed");
        }
        let left = build_supervisor(storage.clone(), true);
        let right = build_supervisor(storage, true);
        let (a, b) = tokio::join!(
            left.claim_run("wfr_race", "pid:1001", 60),
            right.claim_run("wfr_race", "pid:1002", 60),
        );
        assert!(
            a.is_ok() ^ b.is_ok(),
            "CAS claim must have one winner: {a:?} / {b:?}"
        );
        assert!(matches!(a.or(b), Ok(ClaimOutcome { lease_fence: 1, .. })));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn renewal_preserves_fence_and_rejects_wrong_holder() {
        let storage = open_storage();
        {
            let guard = storage.try_lock().expect("storage lock");
            seed_run(guard.conn(), "wfr_renew", "wfv_seed");
            seed_version(guard.conn(), "wfv_seed");
        }
        let sup = build_supervisor(storage.clone(), true);
        let claim = sup
            .claim_run("wfr_renew", "pid:1001", 60)
            .await
            .expect("claim");
        assert!(sup
            .renew_run_lease("wfr_renew", "pid:1001", claim.lease_fence, 30)
            .await
            .expect("renew"));
        assert!(!sup
            .renew_run_lease("wfr_renew", "pid:other", claim.lease_fence, 30)
            .await
            .expect("reject"));
        let guard = storage.try_lock().expect("storage lock");
        assert_eq!(
            WorkflowRepository::new(guard.conn())
                .find_run_lease("wfr_renew")
                .expect("lease")
                .expect("present")
                .lease_fence,
            claim.lease_fence
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_once_skips_when_feature_disabled() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        seed_run(conn, "wfr_test", "wfv_seed");
        seed_version(conn, "wfv_seed");
        drop(storage_guard);

        let sup = build_supervisor(storage.clone(), false);
        let outcome = sup
            .run_once(SupervisorLeaseTtl {
                initial_secs: 60,
                renewal_secs: 30,
            })
            .await
            .expect("run_once");
        assert_eq!(outcome, RunOnceOutcome::Disabled);

        // And no lease row should exist — the gate short-circuited before
        // any SQL ran.
        let repo_guard = storage.try_lock().expect("storage lock");
        let repo = WorkflowRepository::new(repo_guard.conn());
        let lease = repo.find_run_lease("wfr_test").expect("find lease");
        assert!(lease.is_none());
    }
}
