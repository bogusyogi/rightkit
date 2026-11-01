//! WP-03b — NodeExecutor (dispatch worker) for the autonomous workflow
//! control plane (ADR-0021).
//!
//! The executor is the WP-03 component that walks **one** ready node per
//! pass. It sits behind [`RunSupervisor`]: the supervisor hands it a
//! [`ClaimOutcome`] snapshot (run id, plan, current node states, lease
//! fence), and the executor either:
//!
//! 1. Walks the plan topologically to find the smallest-id node whose
//!    dependency set is satisfied (no incoming edges, or every predecessor
//!    in a terminal-success phase).
//! 2. Tries to acquire a [`ResourceLimiters`] slot non-blockingly. If the
//!    cap is reached the executor returns [`PassOutcome::Busy`].
//! 3. Emits the durable journal events `node_claimed`, `node_started`,
//    and then either `node_completed` or `node_failed` (each through
//!    snapshot-bound command application inside
//!    `tokio::task::spawn_blocking`).
//! 4. Calls into the bounded inner executor through the [`ExecuteNode`]
//!    trait and returns [`PassOutcome::Advanced`] or
//!    [`PassOutcome::Failed`].
//!
//! Invariants this module enforces:
//!
//! 1. **One node per pass.** A pass either advances exactly one node,
//!    returns `Empty`, returns `Busy`, or reports a failure. The executor
//!    never batches.
//! 2. **RAII reservation.** A [`ResourceReservation`] is held for the
//!    full pass and released on drop — a panic or early return cannot
//!    leak a slot.
//! 3. **Clock-only time.** Every wall-clock read goes through the
//!    [`Clock`] trait; tests use [`TestClock`].
//! 4. **No free-form unwraps.** Production code returns [`ExecutorError`]
//!    for every failure path. Tests may use `expect` for fixture setup.
//! 5. **Reducer is authoritative.** The executor never edits lifecycle
//!    projections directly; the journal writer and the reducer in
//!    `rightkit_workflow::control_plane::reducer` own every state transition.

#![forbid(unsafe_code)]
#![allow(clippy::needless_return)]

use crate::workflow::triggers::format_unix_ms_as_iso;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_db::rusqlite::OptionalExtension;
use thiserror::Error;
use tracing::{debug, info};

use crate::api::SharedStorage;
use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, Storage, StorageError, WorkflowApprovalRow,
    WorkflowAttentionRow, WorkflowCommandRow, WorkflowEffectRow, WorkflowJournalWriter,
    WorkflowNodeStateRow, WorkflowOutboxRow, WorkflowQueueRow, WorkflowRepository, WorkflowWaitRow,
};
use rightkit_workflow::control_plane::context::{
    ContextBudget, ContextBuilder, ContextSection, IterationKind,
};
use rightkit_workflow::control_plane::domain::{
    BackoffStrategy, CompiledPlan, ContentHash, NodeConfig, NodeId, NodeSpec, RunId,
};
use rightkit_workflow::control_plane::ids::Prefix;
use rightkit_workflow::control_plane::prelude::{Clock, FailureClass};
use rightkit_workflow::control_plane::reducer::{
    reduce_node, CommandKind, NodeCommand, NodeLifecycleState, NodePhase,
};

use super::{
    ClaimOutcome, DispatcherError, ExecuteNode, NodeDispatchContext, NodeDispatcher, NodeOutput,
    RunSupervisor, SupervisorError,
};

// ─── Resource limiters + reservation ────────────────────────────────────────

/// Tunable concurrency / size caps applied to the executor.
///
/// * `max_concurrent_per_run` — soft cap on in-flight nodes per run. The
///   executor refuses to start a second node in the same run while one is
///   still leased (the supervisor owns the canonical lease gate; this is
///   a defence-in-depth cap that prevents `run_pass` from racing the
///   reducer).
/// * `max_total_concurrent` — global cap across all runs. The
///   [`NodeExecutor`] refuses to advance any node when the global count
///   has reached this value; callers use this to apply a single
///   machine-wide ceiling without coordinating with peer daemons.
/// * `max_input_bytes` — input payload size cap. The executor refuses
///   `run_input` blobs larger than this so a runaway binding cannot blow
///   up the executor's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimiters {
    pub max_concurrent_per_run: usize,
    pub max_total_concurrent: usize,
    pub max_input_bytes: usize,
}

/// Opaque RAII token returned by [`NodeExecutor::try_acquire`]. The slot
/// is released when the reservation is dropped; the executor never holds
/// a reservation across an `.await` yield without it.
pub struct ResourceReservation {
    global: Arc<AtomicUsize>,
    per_run: Arc<Mutex<HashMap<String, usize>>>,
    run_id: String,
    slots: usize,
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        // 1. Per-run counter — release the slot back to this run's budget.
        //    The lock is uncontended in practice; on poisoning we still
        //    decrement via `into_inner` so the counter cannot leak.
        match self.per_run.lock() {
            Ok(mut map) => {
                if let Some(count) = map.get_mut(&self.run_id) {
                    *count = count.saturating_sub(self.slots);
                    if *count == 0 {
                        map.remove(&self.run_id);
                    }
                }
            }
            Err(poisoned) => {
                let mut map = poisoned.into_inner();
                if let Some(count) = map.get_mut(&self.run_id) {
                    *count = count.saturating_sub(self.slots);
                    if *count == 0 {
                        map.remove(&self.run_id);
                    }
                }
            }
        }

        // 2. Global counter.
        self.global.fetch_sub(self.slots, Ordering::AcqRel);
    }
}

// ─── Pass outcome + errors ──────────────────────────────────────────────────

/// One executor pass result. The supervisor consumes this to decide what
/// to do next (poll again, idle, surface failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassOutcome {
    /// Nothing to do — the snapshot is empty or no ready nodes exist.
    Empty,
    /// Every node has reached a successful terminal projection.
    Completed { run_id: String },
    /// A durable wait/approval/effect hold owns next progress.
    Waiting { run_id: String },
    /// One node was advanced (claimed, started, dispatched, completed).
    Advanced {
        run_id: String,
        advanced_nodes: Vec<String>,
    },
    /// A node dispatch failed; the run should not be retried without
    /// operator intervention unless the failure class is retryable.
    Failed {
        run_id: String,
        node_id: String,
        failure: FailureClass,
    },
    /// The resource limiter refused to admit a new node. The caller
    /// should back off and retry the pass on its next tick.
    Busy,
}

/// Typed errors the executor can return. The supervisor surfaces these to
/// the caller; the daemon maps them to HTTP/UI attention items.
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// [`ResourceLimiters`] rejected the slot acquisition. The executor
    /// translates this internally into [`PassOutcome::Busy`]; this
    /// variant is reserved for the `Busy` discriminant on the error
    /// enum itself.
    #[error("busy: resource limit reached")]
    Busy,
    /// The run row or its compiled plan is missing from storage.
    #[error("missing compiled plan")]
    MissingPlan,
    /// The dispatcher rejected the dispatch attempt.
    #[error("dispatch: {0}")]
    Dispatch(#[from] DispatcherError),
    /// The reducer refused a command (terminal, revision conflict,
    /// duplicate, …).
    #[error("reducer: {0}")]
    Reducer(String),
    /// The supervisor refused a journal append or projection update.
    #[error("supervisor: {0}")]
    Supervisor(#[from] SupervisorError),
    /// Underlying storage layer error.
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

#[derive(Clone)]
struct DurableHold {
    wait: Option<WorkflowWaitRow>,
    approval: Option<WorkflowApprovalRow>,
    effect: Option<WorkflowEffectRow>,
    attention: Option<WorkflowAttentionRow>,
    outbox: Option<WorkflowOutboxRow>,
}

// ─── Node executor ──────────────────────────────────────────────────────────

/// One executor instance. Cheap to construct; the heavy state lives in
/// the shared `Arc<AtomicUsize>` (global) and `Arc<Mutex<HashMap>>`
/// (per-run) caps that are cloned from the same source when the
/// executor is wrapped in an `Arc` for daemon-wide use.
pub struct NodeExecutor {
    /// Shared storage handle. The executor needs raw `Arc<Mutex<Storage>>`
    /// access (not just the supervisor façade) because `WorkflowJournalWriter`
    /// takes a `&Connection` directly; the supervisor does not expose
    /// its storage. The mutex lets the executor hand the live handle to
    /// `tokio::task::spawn_blocking` while keeping the single-writer
    /// SQLite invariant intact. Marked `#[allow(dead_code)]` because the
    /// supervisor is currently the only call site that constructs an
    /// executor — but the field is required for journal writes.
    #[allow(dead_code)]
    storage: SharedStorage,
    /// Run-supervisor handle. The executor owns a reference so it can
    /// route through the supervisor in future WP-04 / WP-05 work, but
    /// the WP-03b code path does not call into it directly.
    #[allow(dead_code)]
    supervisor: Arc<RunSupervisor>,
    /// Bounded inner executor. Stored as `Arc<dyn ExecuteNode + Send +
    /// Sync>` so tests can inject a deterministic dispatcher without
    /// touching `dispatcher.rs`.
    dispatcher: Arc<dyn ExecuteNode + Send + Sync>,
    /// Wall-clock source. Every `created_at` the executor writes goes
    /// through this trait; tests use `TestClock`.
    clock: Arc<dyn Clock>,
    /// Tunable caps.
    limiters: ResourceLimiters,
    /// Global in-flight slot counter (shared across all reservations).
    total_active: Arc<AtomicUsize>,
    /// Per-run in-flight slot counter (shared across all reservations).
    per_run_active: Arc<Mutex<HashMap<String, usize>>>,
}

impl NodeExecutor {
    /// Build a new executor.
    ///
    /// * `storage` — durable `Arc<Mutex<Storage>>` the executor writes
    ///   through. The supervisor does not expose its own storage handle
    ///   (it is private), so the executor takes its own reference.
    /// * `supervisor` — `RunSupervisor` reference for future expansion.
    ///   The WP-03b path does not call into it directly.
    /// * `dispatcher` — bounded inner executor. The default
    ///   [`NodeDispatcher`] rejects every kind in WP-03 (see
    ///   `dispatcher.rs`); tests inject a custom dispatcher through
    ///   [`NodeExecutor::with_test_dispatcher`].
    /// * `clock` — wall-clock source; tests use [`TestClock`].
    /// * `limiters` — concurrency / size caps applied to the executor.
    pub fn new(
        storage: SharedStorage,
        supervisor: Arc<RunSupervisor>,
        dispatcher: Arc<dyn ExecuteNode + Send + Sync>,
        clock: Arc<dyn Clock>,
        limiters: ResourceLimiters,
    ) -> Self {
        Self {
            storage,
            supervisor,
            dispatcher,
            clock,
            limiters,
            total_active: Arc::new(AtomicUsize::new(0)),
            per_run_active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Test-only constructor that takes an arbitrary dispatcher trait
    /// object so unit tests can verify the executor's wiring against a
    /// deterministic fake instead of the fallback dispatcher that rejects every
    /// kind.
    #[cfg(test)]
    pub(crate) fn with_test_dispatcher(
        storage: SharedStorage,
        supervisor: Arc<RunSupervisor>,
        dispatcher: Arc<dyn ExecuteNode + Send + Sync>,
        clock: Arc<dyn Clock>,
        limiters: ResourceLimiters,
    ) -> Self {
        Self {
            storage,
            supervisor,
            dispatcher,
            clock,
            limiters,
            total_active: Arc::new(AtomicUsize::new(0)),
            per_run_active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Try to reserve one slot for `run_id`. Returns `None` when either
    /// the global cap or the per-run cap has been reached. The
    /// acquisition is non-blocking: it spins at most once on the global
    /// atomic and never blocks on a remote lease. Any successful bump is
    /// rolled back if the per-run cap rejects.
    fn try_acquire(&self, run_id: &str) -> Option<ResourceReservation> {
        // 1. Global cap.
        let mut current = self.total_active.load(Ordering::Acquire);
        loop {
            if current >= self.limiters.max_total_concurrent {
                return None;
            }
            match self.total_active.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }

        // 2. Per-run cap. Roll back the global bump on rejection.
        let per_run_acquired = match self.per_run_active.lock() {
            Ok(mut map) => {
                let entry = map.entry(run_id.to_string()).or_insert(0);
                if *entry >= self.limiters.max_concurrent_per_run {
                    false
                } else {
                    *entry = entry.saturating_add(1);
                    true
                }
            }
            Err(poisoned) => {
                let mut map = poisoned.into_inner();
                let entry = map.entry(run_id.to_string()).or_insert(0);
                if *entry >= self.limiters.max_concurrent_per_run {
                    false
                } else {
                    *entry = entry.saturating_add(1);
                    true
                }
            }
        };

        if !per_run_acquired {
            self.total_active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        Some(ResourceReservation {
            global: Arc::clone(&self.total_active),
            per_run: Arc::clone(&self.per_run_active),
            run_id: run_id.to_string(),
            slots: 1,
        })
    }

    /// Run a single pass over `snapshot`. The supervisor hands the
    /// executor one (or zero) claimed runs at a time; the executor
    /// either advances exactly one node, returns `Empty` / `Busy` /
    /// `Failed`, or surfaces a typed [`ExecutorError`].
    pub async fn run_pass(&self, snapshot: ClaimOutcome) -> Result<PassOutcome, ExecutorError> {
        let ClaimOutcome {
            run_id,
            lifecycle_snapshot: _lifecycle_snapshot,
            plan,
            node_states,
            lease_fence,
        } = snapshot;

        if plan.nodes.is_empty() {
            debug!(
                target: "coderight.workflow.executor",
                run_id = %run_id,
                "executor: plan has no nodes; returning Empty"
            );
            return Ok(PassOutcome::Completed { run_id });
        }

        // 1. Pick a ready node (smallest id whose dependency set is
        //    satisfied in `node_states`). If none, the run is empty.
        let selections = self.load_branch_selections(&run_id).await?;
        // Expand acknowledged Map/Loop selections into immutable child specs.
        // Child ids come only from the prior durable control output, so replay
        // produces byte-identical plans and cannot invent extra work.
        let plan = materialize_control_children(&plan, &selections);
        let reachable = reachable_nodes(&plan, &selections);
        if let Some(skipped) = plan.nodes.iter().find(|node| {
            !reachable.contains(&node.id)
                && !node_states.iter().any(|state| state.node_id == node.id)
        }) {
            self.apply_node_command(
                &run_id,
                &skipped.id,
                CommandKind::NodeSkip,
                0,
                "node_skipped",
                r#"{"reason":"unselected_branch"}"#,
                "skipped",
                None,
                None,
                None,
                None,
                node_snapshot(&node_states, &skipped.id),
            )
            .await?;
            return Ok(PassOutcome::Advanced {
                run_id,
                advanced_nodes: vec![skipped.id.clone()],
            });
        }
        let ready = match pick_ready_node(&plan, &node_states, &selections) {
            Some(n) => n,
            None => {
                debug!(
                    target: "coderight.workflow.executor",
                    run_id = %run_id,
                    "executor: no ready nodes; returning Empty"
                );
                let reachable = reachable_nodes(&plan, &selections);
                if node_states
                    .iter()
                    .any(|state| reachable.contains(&state.node_id) && state.phase == "waiting")
                {
                    return Ok(PassOutcome::Waiting { run_id });
                }
                if reachable.iter().all(|node| {
                    node_states.iter().any(|state| {
                        state.node_id == *node
                            && matches!(state.phase.as_str(), "completed" | "succeeded" | "skipped")
                    })
                }) {
                    return Ok(PassOutcome::Completed { run_id });
                }
                return Ok(PassOutcome::Empty);
            }
        };

        // 2. Acquire a reservation slot. Non-blocking: returns `None`
        //    when the cap has been reached, which we map to `Busy`.
        let _reservation = match self.try_acquire(&run_id) {
            Some(r) => r,
            None => {
                info!(
                    target: "coderight.workflow.executor",
                    run_id = %run_id,
                    max_total_concurrent = self.limiters.max_total_concurrent,
                    "executor: resource limit reached; returning Busy"
                );
                return Ok(PassOutcome::Busy);
            }
        };

        // 3. Determine attempt count from any prior `node_states` row
        //    for this node. First attempt is 1; retries increment.
        let attempt = previous_attempt(&node_states, &ready.id).saturating_add(1);
        let node_id_owned = ready.id.clone();
        let holder = format!("pid_{}", std::process::id());
        let attempt_id = self
            .claim_attempt(&run_id, &ready.id, lease_fence, &holder, attempt as i64)
            .await?;

        // 4. Emit `node_claimed` (phase = leased, attempt = N).
        let claimed_payload = format!(r#"{{"attempt":{attempt}}}"#);
        let mut node_snapshot = node_snapshot(&node_states, &node_id_owned);
        node_snapshot = self
            .apply_node_command(
                &run_id,
                &node_id_owned,
                CommandKind::NodeClaim,
                lease_fence as u64,
                "node_claimed",
                &claimed_payload,
                "claimed",
                Some(attempt as i64),
                None,
                Some(WorkflowQueueRow {
                    run_id: run_id.clone(),
                    node_id: node_id_owned.clone(),
                    available_at: format_unix_ms_as_iso(self.clock.now_unix_ms()),
                    priority: 0,
                    claimed_by: None,
                    claim_fence: None,
                }),
                None,
                node_snapshot,
            )
            .await?;

        // 5. Emit `node_started` (phase = running, attempt = N).
        let started_payload = format!(r#"{{"attempt":{attempt}}}"#);
        node_snapshot = self
            .apply_node_command(
                &run_id,
                &node_id_owned,
                CommandKind::NodeStart,
                lease_fence as u64,
                "node_started",
                &started_payload,
                "running",
                None,
                None,
                None,
                None,
                node_snapshot,
            )
            .await?;

        // 6. Build the immutable attempt context from durable run/version
        // authority. Trigger input never gets to choose a workspace or trust
        // tier, & only declared input plus ancestor outputs are projected.
        let ctx = self
            .build_dispatch_context(
                &run_id,
                &ready,
                &plan,
                &node_states,
                node_snapshot.revision.0,
                self.clock.now_unix_ms(),
            )
            .await?;
        let dispatch_result = self
            .execute_with_heartbeat(&run_id, &attempt_id, lease_fence, &holder, &ready, &ctx)
            .await;

        // 7. Emit the terminal event and return the outcome. The
        //    `ResourceReservation` is held until the end of `run_pass`,
        //    at which point it drops and releases the slot.
        match dispatch_result {
            Ok(output) => {
                let result_json = output.output_json.to_string();
                self.complete_attempt(
                    &attempt_id,
                    lease_fence,
                    &holder,
                    "succeeded",
                    Some(&result_json),
                    None,
                )
                .await?;
                self.apply_node_command(
                    &run_id,
                    &node_id_owned,
                    CommandKind::NodeComplete,
                    lease_fence as u64,
                    "node_completed",
                    "{}",
                    "succeeded",
                    None,
                    None,
                    None,
                    None,
                    node_snapshot,
                )
                .await?;
                Ok(PassOutcome::Advanced {
                    run_id,
                    advanced_nodes: vec![node_id_owned],
                })
            }
            Err(
                DispatcherError::Waiting(reason)
                | DispatcherError::ApprovalRequired(reason)
                | DispatcherError::EffectIntent(reason),
            ) => {
                let wait_json = serde_json::json!({"reason": reason}).to_string();
                self.complete_attempt(
                    &attempt_id,
                    lease_fence,
                    &holder,
                    "cancelled",
                    None,
                    Some(&wait_json),
                )
                .await?;
                let payload = serde_json::json!({"attempt": attempt, "reason": reason}).to_string();
                let hold = durable_hold(
                    &run_id,
                    &node_id_owned,
                    &ready,
                    &ctx,
                    self.clock.now_unix_ms(),
                )?;
                self.apply_node_command(
                    &run_id,
                    &node_id_owned,
                    CommandKind::NodeWait,
                    lease_fence as u64,
                    "node_waiting",
                    &payload,
                    "waiting",
                    None,
                    None,
                    None,
                    Some(hold),
                    node_snapshot,
                )
                .await?;
                Ok(PassOutcome::Waiting { run_id })
            }
            Err(error) => {
                let (failure, transport_retryable, message) = match error {
                    DispatcherError::ExecutionFailure {
                        class,
                        retryable,
                        message,
                    } => (class, retryable, message),
                    other => (FailureClass::ExecutorUnavailable, true, other.to_string()),
                };
                let attempt_error = serde_json::json!({
                    "class": format!("{failure:?}"),
                    "message": &message,
                })
                .to_string();
                self.complete_attempt(
                    &attempt_id,
                    lease_fence,
                    &holder,
                    "failed",
                    None,
                    Some(&attempt_error),
                )
                .await?;
                let failure_label = format!("{failure:?}");
                let retryable = transport_retryable
                    && attempt < ready.retry.max_attempts as usize
                    && ready.retry.retryable.contains(&failure);
                let delay_ms = retry_delay_ms(&ready.retry.backoff, attempt);
                let failure_payload = serde_json::json!({
                    "attempt": attempt,
                    "retryScheduled": retryable,
                    "delayMs": delay_ms,
                    "reason": &message,
                })
                .to_string();
                self.apply_node_command(
                    &run_id,
                    &node_id_owned,
                    if retryable {
                        CommandKind::NodeRetry
                    } else {
                        CommandKind::NodeFail
                    },
                    lease_fence as u64,
                    if retryable {
                        "node_retry_scheduled"
                    } else {
                        "node_failed"
                    },
                    &failure_payload,
                    if retryable { "ready" } else { "failed" },
                    None,
                    Some(failure_label),
                    retryable.then(|| WorkflowQueueRow {
                        run_id: run_id.clone(),
                        node_id: node_id_owned.clone(),
                        available_at: format_unix_ms_as_iso(
                            self.clock.now_unix_ms().saturating_add(delay_ms as i64),
                        ),
                        priority: 0,
                        claimed_by: None,
                        claim_fence: None,
                    }),
                    None,
                    node_snapshot,
                )
                .await?;
                if retryable {
                    Ok(PassOutcome::Waiting { run_id })
                } else {
                    Ok(PassOutcome::Failed {
                        run_id,
                        node_id: node_id_owned,
                        failure,
                    })
                }
            }
        }
    }

    /// Returns the current global in-flight slot count. Used for
    /// observability / tests.
    pub fn global_in_use(&self) -> usize {
        self.total_active.load(Ordering::Acquire)
    }

    /// Returns the current per-run in-flight slot count, or `0` if no
    /// nodes for `run_id` are leased.
    pub fn per_run_in_use(&self, run_id: &str) -> usize {
        match self.per_run_active.lock() {
            Ok(map) => map.get(run_id).copied().unwrap_or(0),
            Err(poisoned) => poisoned.into_inner().get(run_id).copied().unwrap_or(0),
        }
    }

    async fn execute_with_heartbeat(
        &self,
        run_id: &str,
        attempt_id: &str,
        fence: i64,
        holder: &str,
        spec: &NodeSpec,
        ctx: &NodeDispatchContext,
    ) -> Result<NodeOutput, DispatcherError> {
        let dispatch = self.dispatcher.execute_node(spec, ctx);
        tokio::pin!(dispatch);
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(10),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                result = &mut dispatch => return result,
                _ = heartbeat.tick() => {
                    self.refresh_attempt_lease(attempt_id, fence, holder).await
                        .map_err(|error| DispatcherError::Runtime(error.to_string()))?;
                    if !self.supervisor.renew_run_lease(run_id, holder, fence, 30).await
                        .map_err(|error| DispatcherError::Runtime(error.to_string()))?
                    {
                        return Err(DispatcherError::Runtime("run lease lost during dispatch".into()));
                    }
                }
            }
        }
    }

    async fn load_branch_selections(
        &self,
        run_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, ExecutorError> {
        let storage = self.storage.clone();
        let run_id = run_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let storage = storage.blocking_lock();
            let mut statement = storage.conn().prepare(
                "SELECT node_id,result_json FROM workflow_attempts
                 WHERE run_id=?1 AND state='succeeded' AND result_json IS NOT NULL
                 ORDER BY attempt DESC",
            )?;
            let mut selected = HashMap::new();
            for row in statement.query_map(rightkit_db::rusqlite::params![run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (node, raw) = row?;
                if selected.contains_key(&node) {
                    continue;
                }
                let targets = serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|value| value.get("selected").cloned())
                    .and_then(|value| serde_json::from_value(value).ok());
                if let Some(targets) = targets {
                    selected.insert(node, targets);
                }
            }
            Ok::<_, StorageError>(selected)
        })
        .await
        .map_err(|error| {
            ExecutorError::Storage(StorageError::Serde(format!("selection join: {error}")))
        })?
        .map_err(ExecutorError::Storage)
    }

    async fn build_dispatch_context(
        &self,
        run_id: &str,
        ready: &NodeSpec,
        plan: &CompiledPlan,
        states: &[WorkflowNodeStateRow],
        revision: u64,
        now_unix_ms: i64,
    ) -> Result<NodeDispatchContext, ExecutorError> {
        let storage = self.storage.clone();
        let run_id_owned = run_id.to_owned();
        let node_id = ready.id.clone();
        let plan = plan.clone();
        let states = states.to_vec();
        let ready = ready.clone();
        tokio::task::spawn_blocking(move || -> Result<NodeDispatchContext, ExecutorError> {
            let storage = storage.blocking_lock();
            let conn = storage.conn();
            let repo = rightkit_agent_engine_storage::WorkflowRepository::new(conn);
            let run = repo
                .find_run(&run_id_owned)?
                .ok_or(ExecutorError::MissingPlan)?;
            let version = repo
                .find_version(&run.workflow_version_id)?
                .ok_or(ExecutorError::MissingPlan)?;
            let definition: serde_json::Value = serde_json::from_str(&version.definition_json)
                .map_err(|e| DispatcherError::Runtime(format!("invalid durable definition: {e}")))?;
            let input: serde_json::Value = serde_json::from_str(&run.input_json)
                .map_err(|e| DispatcherError::Runtime(format!("invalid durable run input: {e}")))?;
            let declared = definition.pointer("/spec/inputs").and_then(serde_json::Value::as_object)
                .ok_or_else(|| DispatcherError::Runtime("definition has no declared inputs".into()))?;
            let input_object = input.as_object().ok_or_else(|| {
                DispatcherError::Runtime("run input must be an object".into())
            })?;
            if input_object.keys().any(|key| !declared.contains_key(key)) {
                return Err(DispatcherError::Runtime("run input contains undeclared fields".into()).into());
            }

            // Workspace location & trust must be immutable version authority.
            // Existing schema has no authority field, so legacy versions fail
            // closed instead of adopting attacker-controlled `workspaceRoot`.
            let workspace_root = definition.pointer("/spec/workspace/root")
                .and_then(serde_json::Value::as_str)
                .filter(|root| !root.trim().is_empty())
                .ok_or_else(|| DispatcherError::Runtime(
                    "published version lacks authoritative workspace root".into(),
                ))?
                .to_owned();
            let workspace_trusted = definition.pointer("/spec/workspace/trust")
                .and_then(serde_json::Value::as_str) == Some("trusted");

            let ancestors = ancestor_ids(&plan, &node_id);
            let mut outputs = HashMap::new();
            for upstream in ancestors {
                if !states.iter().any(|state| state.node_id == upstream && matches!(state.phase.as_str(), "completed" | "succeeded")) {
                    continue;
                }
                let output: Option<String> = conn.query_row(
                    "SELECT result_json FROM workflow_attempts WHERE run_id=?1 AND node_id=?2 AND state='succeeded' ORDER BY attempt DESC LIMIT 1",
                    rightkit_db::rusqlite::params![run_id_owned, upstream],
                    |row| row.get(0),
                ).optional().map_err(rightkit_agent_engine_storage::StorageError::from)?;
                if let Some(raw) = output {
                    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| DispatcherError::Runtime(format!("invalid upstream output: {e}")))?;
                    outputs.insert(upstream, value);
                }
            }

            let budget = context_budget(&ready, &definition, now_unix_ms)?;
            let mut builder = ContextBuilder::new("v1")
                .with_provenance(
                    RunId::new(Prefix::WorkflowRun, &run_id_owned),
                    NodeId::new(Prefix::NodeRun, &node_id),
                    revision,
                )
                .with_capabilities(ready.capabilities.clone())
                .with_budget(budget);
            for (key, value) in input_object {
                builder = builder.push(ContextSection::RunInput { key: key.clone(), value: value.clone() })
                    .map_err(|e| DispatcherError::Runtime(format!("invalid input context: {e}")))?;
            }
            for (upstream, value) in &outputs {
                builder = builder.push(ContextSection::NodeOutput {
                    node_id: NodeId::new(Prefix::NodeRun, upstream), value: value.clone(),
                }).map_err(|e| DispatcherError::Runtime(format!("invalid output context: {e}")))?;
            }
            let iteration = iteration_scope(&ready.id, ready.kind, &input);
            if let Some((index, kind, item)) = iteration.clone() {
                builder = builder.push(ContextSection::Iteration { index, kind, item })
                    .map_err(|e| DispatcherError::Runtime(format!("invalid iteration context: {e}")))?;
            }
            let manifest = builder.build().map_err(|e| DispatcherError::Runtime(format!("manifest: {e}")))?;
            Ok(NodeDispatchContext {
                run_input: input,
                node_outputs: outputs,
                workspace_root,
                prompt_trust: if workspace_trusted { rightkit_agent_engine_protocol::ContentTrust::WorkspaceTrusted } else { rightkit_agent_engine_protocol::ContentTrust::User },
                manifest,
                compiled_plan: plan,
                version_id: version.id,
                iteration_index: iteration.map(|(index, _, _)| index),
            })
        }).await.map_err(|e| ExecutorError::Storage(StorageError::Serde(format!("spawn_blocking join: {e}"))))?
    }

    /// Atomically reduce one node lifecycle command & apply its journal,
    /// projection, & optional queue mutation.
    async fn apply_node_command(
        &self,
        run_id: &str,
        node_id: &str,
        kind: CommandKind,
        generation: u64,
        event_kind: &str,
        payload_json: &str,
        node_phase: &str,
        node_attempt: Option<i64>,
        node_last_failure_class: Option<String>,
        queue: Option<WorkflowQueueRow>,
        hold: Option<DurableHold>,
        snapshot: NodeLifecycleState,
    ) -> Result<NodeLifecycleState, ExecutorError> {
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id_owned = run_id.to_string();
        let node_id_owned = node_id.to_string();
        let event_kind_owned = event_kind.to_string();
        let payload_owned = payload_json.to_string();
        let node_phase_owned = node_phase.to_string();

        let next =
            tokio::task::spawn_blocking(move || -> Result<NodeLifecycleState, ExecutorError> {
                let storage_guard = storage.blocking_lock();
                let conn = storage_guard.conn();
                let now_ms = clock.now_unix_ms();
                let now_iso = format_unix_ms_as_iso(now_ms);
                let event = JournalAppend {
                    run_id: &run_id_owned,
                    event_kind: &event_kind_owned,
                    node_id: Some(&node_id_owned),
                    payload_json: &payload_owned,
                    created_at: &now_iso,
                };
                let projection = ProjectionUpdate {
                    run_id: &run_id_owned,
                    node: Some((&node_id_owned, &node_phase_owned)),
                    node_attempt,
                    node_last_failure_class: node_last_failure_class.as_deref(),
                    ..Default::default()
                };
                let command_id = format!("wfc_{}", rightkit_agent_engine_protocol::uuid_v7());
                let command = WorkflowCommandRow {
                    run_id: run_id_owned.clone(),
                    command_id: command_id.clone(),
                    expected_revision: snapshot.revision.0 as i64,
                    command_kind: kind.as_str().to_string(),
                    payload_json: payload_owned.clone(),
                    created_at: now_iso.clone(),
                };
                let node_command = NodeCommand {
                    command_id,
                    kind,
                    node_id: node_id_owned.clone(),
                    expected_revision: snapshot.revision,
                    generation,
                    approval_decision: None,
                    effect_observation: None,
                };
                let next = reduce_node(&snapshot, &node_command).map_err(|error| {
                    StorageError::Serde(format!("node reducer rejected command: {error}"))
                })?;
                WorkflowJournalWriter::new(conn).apply_node_command_from_snapshot(
                    &snapshot,
                    &node_command,
                    &command,
                    &payload_owned,
                    &event,
                    &projection,
                    queue.as_ref(),
                    hold.as_ref().and_then(|value| value.wait.as_ref()),
                    hold.as_ref().and_then(|value| value.approval.as_ref()),
                    hold.as_ref().and_then(|value| value.effect.as_ref()),
                    hold.as_ref().and_then(|value| value.attention.as_ref()),
                    hold.as_ref().and_then(|value| value.outbox.as_ref()),
                )?;
                Ok(next)
            })
            .await
            .map_err(|e| {
                ExecutorError::Storage(StorageError::Serde(format!("spawn_blocking join: {e}")))
            })??;

        Ok(next)
    }

    async fn claim_attempt(
        &self,
        run_id: &str,
        node_id: &str,
        fence: i64,
        holder: &str,
        attempt: i64,
    ) -> Result<String, ExecutorError> {
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let run_id = run_id.to_string();
        let node_id = node_id.to_string();
        let holder = holder.to_string();
        tokio::task::spawn_blocking(move || -> Result<String, ExecutorError> {
            let storage = storage.blocking_lock();
            let now = format_unix_ms_as_iso(clock.now_unix_ms());
            let repo = rightkit_agent_engine_storage::WorkflowRepository::new(storage.conn());
            if repo.find_queue_node(&run_id, &node_id)?.is_none() {
                repo.enqueue_node(&WorkflowQueueRow {
                    run_id: run_id.clone(),
                    node_id: node_id.clone(),
                    available_at: now.clone(),
                    priority: 0,
                    claimed_by: None,
                    claim_fence: None,
                })?;
            }
            if repo
                .claim_node(&run_id, &node_id, &now, &holder, fence)?
                .is_none()
            {
                return Err(ExecutorError::Busy);
            }
            let id = format!("wfa-{run_id}-{node_id}-{attempt}-{fence}");
            repo.insert_attempt(&rightkit_agent_engine_storage::WorkflowAttemptRow {
                id: id.clone(),
                run_id: run_id.clone(),
                node_id: node_id.clone(),
                attempt,
                lease_fence: fence,
                state: "running".into(),
                started_at: now.clone(),
                finished_at: None,
            })?;
            let heartbeat = rightkit_agent_engine_storage::WorkflowAttemptUpdate {
                id: &id,
                generation: fence,
                holder: &holder,
                at: &now,
            };
            if !repo.heartbeat_attempt(&heartbeat)? {
                return Err(ExecutorError::Reducer("lease lost before dispatch".into()));
            }
            Ok(id)
        })
        .await
        .map_err(|e| {
            ExecutorError::Storage(StorageError::Serde(format!("attempt claim join: {e}")))
        })?
    }

    async fn complete_attempt(
        &self,
        id: &str,
        fence: i64,
        holder: &str,
        state: &str,
        result: Option<&str>,
        failure: Option<&str>,
    ) -> Result<(), ExecutorError> {
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let id = id.to_string();
        let holder = holder.to_string();
        let state = state.to_string();
        let result = result.map(str::to_string);
        let failure = failure.map(str::to_string);
        tokio::task::spawn_blocking(move || -> Result<(), ExecutorError> {
            let storage = storage.blocking_lock();
            let now = format_unix_ms_as_iso(clock.now_unix_ms());
            let repo = rightkit_agent_engine_storage::WorkflowRepository::new(storage.conn());
            let update = rightkit_agent_engine_storage::WorkflowAttemptUpdate {
                id: &id,
                generation: fence,
                holder: &holder,
                at: &now,
            };
            if !repo.complete_attempt(&update, &state, result.as_deref(), failure.as_deref())? {
                return Err(ExecutorError::Reducer(
                    "lease lost before attempt completion".into(),
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| {
            ExecutorError::Storage(StorageError::Serde(format!("attempt completion join: {e}")))
        })?
    }

    async fn refresh_attempt_lease(
        &self,
        id: &str,
        fence: i64,
        holder: &str,
    ) -> Result<(), ExecutorError> {
        let storage = self.storage.clone();
        let clock = self.clock.clone();
        let id = id.to_owned();
        let holder = holder.to_owned();
        tokio::task::spawn_blocking(move || {
            let storage = storage.blocking_lock();
            let now = format_unix_ms_as_iso(clock.now_unix_ms());
            let update = rightkit_agent_engine_storage::WorkflowAttemptUpdate {
                id: &id,
                generation: fence,
                holder: &holder,
                at: &now,
            };
            if !WorkflowRepository::new(storage.conn()).heartbeat_attempt(&update)? {
                return Err(ExecutorError::Reducer(
                    "attempt lease lost during dispatch".into(),
                ));
            }
            Ok(())
        })
        .await
        .map_err(|error| {
            ExecutorError::Storage(StorageError::Serde(format!("heartbeat join: {error}")))
        })?
    }
}

/// Decode only canonical materialized child ids. Input item lookup is bounded
/// by the declared `items` array; malformed ids never gain iteration context.
fn iteration_scope(
    node_id: &str,
    kind: rightkit_workflow::control_plane::prelude::NodeKind,
    input: &serde_json::Value,
) -> Option<(u64, IterationKind, serde_json::Value)> {
    let index = node_id.rsplit_once("__iter_")?.1.parse::<u64>().ok()?;
    let iteration_kind = match kind {
        rightkit_workflow::control_plane::prelude::NodeKind::Map => IterationKind::Map,
        rightkit_workflow::control_plane::prelude::NodeKind::Loop => IterationKind::Loop,
        _ => return None,
    };
    let item = input
        .get("items")
        .and_then(|v| v.as_array())
        .and_then(|items| items.get(index as usize))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Some((index, iteration_kind, item))
}

fn materialize_control_children(
    source: &CompiledPlan,
    selections: &HashMap<String, Vec<String>>,
) -> CompiledPlan {
    let mut plan = source.clone();
    let mut additions = Vec::new();
    for control in source.nodes.iter().filter(|node| {
        matches!(
            node.kind,
            rightkit_workflow::control_plane::prelude::NodeKind::Map
                | rightkit_workflow::control_plane::prelude::NodeKind::Loop
        )
    }) {
        let Some(ids) = selections.get(&control.id) else {
            continue;
        };
        for (index, child_id) in ids.iter().enumerate() {
            let Some(template_id) = control.edges.first().map(|edge| edge.target.as_str()) else {
                continue;
            };
            let Some(template) = source.nodes.iter().find(|node| node.id == template_id) else {
                continue;
            };
            if !child_id.starts_with(&format!("{}__iter_", control.id))
                || plan.nodes.iter().any(|node| node.id == *child_id)
            {
                continue;
            }
            let mut child = template.clone();
            child.id = child_id.clone();
            additions.push((control.id.clone(), template_id.to_owned(), child, index));
        }
    }
    for (control_id, template_id, child, _) in &additions {
        for node in &mut plan.nodes {
            if node.id == *control_id {
                node.edges.retain(|edge| edge.target != *template_id);
                node.edges
                    .push(rightkit_workflow::control_plane::prelude::EdgeSpec {
                        target: child.id.clone(),
                        when: None,
                    });
            }
        }
        plan.nodes.retain(|node| node.id != *template_id);
    }
    plan.nodes
        .extend(additions.into_iter().map(|(_, _, child, _)| child));
    plan
}

// ─── Free helpers ───────────────────────────────────────────────────────────

/// Pick the first (smallest-id) ready node whose dependency set is
/// satisfied. "Satisfied" means every predecessor is either absent from
/// `node_states` (no journal event has touched it yet — the reducer
/// hasn't moved it to a terminal phase) or present in a terminal-success
/// phase (`succeeded` / `completed`).
///
/// If the plan has no nodes the function returns `None`. If at least one
/// node is ready, the function returns the smallest-id ready node. If no
/// node is ready (every node has an unfinished predecessor), the function
/// returns `None` and `run_pass` maps that to [`PassOutcome::Empty`].
fn pick_ready_node(
    plan: &CompiledPlan,
    states: &[WorkflowNodeStateRow],
    selections: &HashMap<String, Vec<String>>,
) -> Option<NodeSpec> {
    // Build the predecessor map: `target → [sources]`. A node is
    // "ready" once every source has terminated successfully.
    let mut predecessors: HashMap<String, Vec<String>> = HashMap::new();
    for node in &plan.nodes {
        for edge in &node.edges {
            predecessors
                .entry(edge.target.clone())
                .or_default()
                .push(node.id.clone());
        }
    }

    let states_by_id: HashMap<&str, &WorkflowNodeStateRow> =
        states.iter().map(|s| (s.node_id.as_str(), s)).collect();

    let terminal_success = |phase: &str| matches!(phase, "succeeded" | "completed");
    let reachable = reachable_nodes(plan, selections);

    let mut candidates: Vec<&NodeSpec> = plan
        .nodes
        .iter()
        .filter(|node| reachable.contains(&node.id))
        .filter(|n| {
            if states_by_id
                .get(n.id.as_str())
                .is_some_and(|state| !matches!(state.phase.as_str(), "ready" | "pending"))
            {
                return false;
            }
            let preds = match predecessors.get(&n.id) {
                Some(v) => v.as_slice(),
                None => &[],
            };
            if preds.is_empty() {
                return true;
            }
            preds.iter().filter(|p| reachable.contains(*p)).all(|p| {
                states_by_id
                    .get(p.as_str())
                    .map(|s| terminal_success(s.phase.as_str()))
                    .unwrap_or(false)
            })
        })
        .collect();

    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    candidates.into_iter().next().cloned()
}

fn reachable_nodes(
    plan: &CompiledPlan,
    selections: &HashMap<String, Vec<String>>,
) -> std::collections::HashSet<String> {
    let mut reachable = std::collections::HashSet::from([plan.entry_node.clone()]);
    loop {
        let before = reachable.len();
        let newly_reachable: Vec<String> = plan
            .nodes
            .iter()
            .filter(|node| reachable.contains(&node.id))
            .flat_map(|node| {
                let selected = (node.kind
                    == rightkit_workflow::control_plane::prelude::NodeKind::Condition)
                    .then(|| selections.get(&node.id))
                    .flatten();
                node.edges
                    .iter()
                    .filter(move |edge| {
                        selected.is_none_or(|targets| targets.contains(&edge.target))
                    })
                    .map(|edge| edge.target.clone())
            })
            .collect();
        reachable.extend(newly_reachable);
        if reachable.len() == before {
            break;
        }
    }
    reachable
}

/// Look up the previous attempt count for `node_id` in `states`. Returns
/// `0` when the node has no recorded state (first attempt).
/// Return only nodes with a directed path into `node_id`; unrelated completed
/// work never crosses a binding boundary.
fn ancestor_ids(plan: &CompiledPlan, node_id: &str) -> Vec<String> {
    let mut pending = vec![node_id.to_owned()];
    let mut ancestors = std::collections::BTreeSet::new();
    while let Some(target) = pending.pop() {
        for candidate in &plan.nodes {
            if candidate.edges.iter().any(|edge| edge.target == target)
                && ancestors.insert(candidate.id.clone())
            {
                pending.push(candidate.id.clone());
            }
        }
    }
    ancestors.into_iter().collect()
}

/// Extract only bounded, version-declared limits. Missing bounds fail closed;
/// an executor must not invent an unlimited deadline from a run payload.
fn context_budget(
    spec: &NodeSpec,
    definition: &serde_json::Value,
    now_unix_ms: i64,
) -> Result<ContextBudget, ExecutorError> {
    let workflow_budget = definition.pointer("/spec/budgets");
    let max_runtime_ms = match &spec.config {
        NodeConfig::Execution { max_runtime_ms, .. } => *max_runtime_ms,
        NodeConfig::Wait { timeout_ms, .. } => *timeout_ms,
        NodeConfig::Approval {
            expires_after_ms, ..
        } => *expires_after_ms,
        NodeConfig::Effect { .. }
        | NodeConfig::Map { .. }
        | NodeConfig::Loop { .. }
        | NodeConfig::Condition { .. }
        | NodeConfig::Join { .. } => 1_000,
        NodeConfig::None => {
            return Err(DispatcherError::Runtime("node lacks typed bounds".into()).into());
        }
    };
    let max_tokens = workflow_budget
        .and_then(|value| value.get("maxTokens"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| DispatcherError::Runtime("version lacks bounded tokens".into()))?;
    let max_cost_usd_micros = workflow_budget
        .and_then(|value| value.get("maxCostUsd"))
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * 1_000_000.0).floor() as u64);
    Ok(ContextBudget {
        deadline_unix_ms: Some(now_unix_ms.saturating_add(max_runtime_ms as i64)),
        max_tokens: Some(max_tokens),
        max_cost_usd_micros,
        max_inner_retries: Some(spec.retry.max_attempts.saturating_sub(1)),
    })
}

/// Shape durable hold rows before the node coordinator commits its reducer
/// command. No caller writes wait, approval, effect, attention, or outbox
/// state separately from `node_waiting`.
fn durable_hold(
    run_id: &str,
    node_id: &str,
    spec: &NodeSpec,
    ctx: &NodeDispatchContext,
    now_unix_ms: i64,
) -> Result<DurableHold, ExecutorError> {
    let now = format_unix_ms_as_iso(now_unix_ms);
    let manifest_hash = ctx.manifest.provenance().content_hash.to_string();
    let id = |kind: &str| format!("wf_{kind}_{}", rightkit_agent_engine_protocol::uuid_v7());
    let attention = |kind: &str, title: &str, summary: &str, key: String| WorkflowAttentionRow {
        id: id("attn"),
        run_id: Some(run_id.into()),
        node_id: Some(node_id.into()),
        effect_id: None,
        approval_id: None,
        kind: kind.into(),
        state: "open".into(),
        severity: "warning".into(),
        title: title.into(),
        summary: summary.into(),
        deep_link_json: "{}".into(),
        dedupe_key: key,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    match (&spec.kind, &spec.config) {
        (
            rightkit_workflow::control_plane::prelude::NodeKind::WaitTimer,
            NodeConfig::Wait { timeout_ms, .. },
        ) => Ok(DurableHold {
            wait: Some(WorkflowWaitRow {
                id: id("wait"),
                run_id: run_id.into(),
                node_id: node_id.into(),
                kind: "timer".into(),
                state: "open".into(),
                correlation_key: format!("{run_id}:{node_id}"),
                topic: None,
                filter_expression: None,
                filter_hash: None,
                wake_at_unix_ms: Some(now_unix_ms.saturating_add(*timeout_ms as i64)),
                deadline_unix_ms: Some(now_unix_ms.saturating_add(*timeout_ms as i64)),
                matched_count: 0,
                max_matches: 1,
                created_at: now.clone(),
                updated_at: now.clone(),
            }),
            approval: None,
            effect: None,
            attention: None,
            outbox: None,
        }),
        (
            rightkit_workflow::control_plane::prelude::NodeKind::WaitEvent,
            NodeConfig::Wait {
                max_matches,
                timeout_ms,
                topic: Some(topic),
                correlation_key: Some(correlation_key),
                filter_expression: Some(filter_expression),
                filter_hash: Some(filter_hash),
                ..
            },
        ) => Ok(DurableHold {
            wait: Some(WorkflowWaitRow {
                id: id("wait"),
                run_id: run_id.into(),
                node_id: node_id.into(),
                kind: "event".into(),
                state: "open".into(),
                correlation_key: correlation_key.clone(),
                topic: Some(topic.clone()),
                filter_expression: Some(
                    rightkit_workflow::control_plane::expression::canonical_text(filter_expression),
                ),
                filter_hash: Some(filter_hash.clone()),
                wake_at_unix_ms: None,
                deadline_unix_ms: Some(now_unix_ms.saturating_add(*timeout_ms as i64)),
                matched_count: 0,
                max_matches: *max_matches as i64,
                created_at: now.clone(),
                updated_at: now.clone(),
            }),
            approval: None,
            effect: None,
            attention: None,
            outbox: None,
        }),
        (
            rightkit_workflow::control_plane::prelude::NodeKind::Approval,
            NodeConfig::Approval {
                role,
                expires_after_ms,
            },
        ) => {
            let approval_id = id("approval");
            let key = format!("approval:{run_id}:{node_id}:{manifest_hash}");
            Ok(DurableHold {
                wait: None,
                approval: Some(WorkflowApprovalRow {
                    id: approval_id.clone(),
                    run_id: run_id.into(),
                    node_id: node_id.into(),
                    version_id: ctx.version_id.clone(),
                    inputs_hash: manifest_hash.clone(),
                    outputs_hash: manifest_hash.clone(),
                    artifact_refs_json: "[]".into(),
                    capabilities_json: serde_json::to_string(ctx.manifest.capabilities())
                        .unwrap_or_else(|_| "[]".into()),
                    effect_request_hash: manifest_hash.clone(),
                    expires_at_unix_ms: now_unix_ms.saturating_add(*expires_after_ms as i64),
                    role: role.clone(),
                    one_shot: true,
                    state: "requested".into(),
                    decision_id: None,
                    decision_actor: None,
                    decision_reason: None,
                    decision_signature: None,
                    decided_at_unix_ms: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                }),
                effect: None,
                attention: Some(attention(
                    "approval",
                    "Workflow approval required",
                    "A hash-bound workflow approval is awaiting a decision.",
                    key.clone(),
                )),
                outbox: Some(WorkflowOutboxRow {
                    id: id("outbox"),
                    run_id: run_id.into(),
                    kind: "approval".into(),
                    topic: "workflow.approval.requested".into(),
                    dedupe_key: key,
                    payload_json: serde_json::json!({"approvalId": approval_id, "nodeId": node_id})
                        .to_string(),
                    state: "pending".into(),
                    available_at: now.clone(),
                    attempt_count: 0,
                    max_attempts: 5,
                    last_error_json: None,
                    next_attempt_at: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                }),
            })
        }
        (
            rightkit_workflow::control_plane::prelude::NodeKind::Effect,
            NodeConfig::Effect {
                adapter,
                operation,
                draft_only,
                max_attempts,
            },
        ) => {
            if !draft_only {
                return Err(DispatcherError::Runtime("effect must be draft-only".into()).into());
            }
            let payload_hash = ContentHash::sha256(manifest_hash.as_bytes()).to_string();
            let key = format!("effect:{run_id}:{node_id}:{payload_hash}");
            let effect_id = id("effect");
            Ok(DurableHold {
                wait: None,
                approval: None,
                effect: Some(WorkflowEffectRow {
                    id: effect_id.clone(),
                    run_id: run_id.into(),
                    node_id: node_id.into(),
                    adapter: adapter.clone(),
                    operation: operation.clone(),
                    destination_hash: manifest_hash.clone(),
                    payload_hash,
                    idempotency_key: key.clone(),
                    state: "planned".into(),
                    attempt_count: 0,
                    max_attempts: *max_attempts as i64,
                    receipt_json: None,
                    outcome_json: None,
                    reconciliation_json: None,
                    failure_json: None,
                    requested_at_unix_ms: now_unix_ms,
                    ttl_unix_ms: now_unix_ms.saturating_add(300_000),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                }),
                attention: Some(attention(
                    "effect",
                    "Workflow effect planned",
                    "A draft-only effect is awaiting governed delivery.",
                    key.clone(),
                )),
                outbox: Some(WorkflowOutboxRow {
                    id: id("outbox"),
                    run_id: run_id.into(),
                    kind: "effect".into(),
                    topic: "workflow.effect.planned".into(),
                    dedupe_key: key,
                    payload_json: serde_json::json!({"effectId": effect_id, "nodeId": node_id})
                        .to_string(),
                    state: "pending".into(),
                    available_at: now.clone(),
                    attempt_count: 0,
                    max_attempts: 5,
                    last_error_json: None,
                    next_attempt_at: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                }),
            })
        }
        _ => Err(
            DispatcherError::Runtime("durable hold lacks typed node configuration".into()).into(),
        ),
    }
}

fn previous_attempt(states: &[WorkflowNodeStateRow], node_id: &str) -> usize {
    states
        .iter()
        .find(|s| s.node_id == node_id)
        .map(|s| s.attempt.max(0) as usize)
        .unwrap_or(0)
}

fn retry_delay_ms(backoff: &BackoffStrategy, attempt: usize) -> u64 {
    match backoff {
        BackoffStrategy::Fixed { delay_ms } => *delay_ms,
        BackoffStrategy::Exponential {
            base_ms, max_ms, ..
        } => base_ms
            .saturating_mul(
                1_u64
                    .checked_shl(attempt.saturating_sub(1) as u32)
                    .unwrap_or(u64::MAX),
            )
            .min(*max_ms),
    }
}

fn node_snapshot(states: &[WorkflowNodeStateRow], node_id: &str) -> NodeLifecycleState {
    let row = states.iter().find(|state| state.node_id == node_id);
    let phase = row.map_or(NodePhase::Ready, |state| match state.phase.as_str() {
        "leased" | "claimed" => NodePhase::Claimed,
        "running" => NodePhase::Running,
        "waiting" => NodePhase::Waiting,
        "completed" | "succeeded" => NodePhase::Succeeded,
        "skipped" => NodePhase::Skipped,
        "failed" => NodePhase::Failed,
        "cancelled" => NodePhase::Cancelled,
        _ => NodePhase::Ready,
    });
    NodeLifecycleState {
        node_id: node_id.to_string(),
        revision: rightkit_workflow::control_plane::prelude::Revision(
            row.map(|state| state.last_reducer_revision.max(0) as u64)
                .unwrap_or(0),
        ),
        generation: 0,
        phase,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use rightkit_agent_engine_storage::{Storage, WorkflowRepository};
    use rightkit_db::rusqlite::{params, Connection};
    use rightkit_workflow::control_plane::clock::TestClock;
    use rightkit_workflow::control_plane::domain::{
        Capability, CapabilitySet, EdgeSpec, NodeKind, RetryPolicy,
    };
    use rightkit_workflow::control_plane::prelude::FeatureGate;

    fn open_storage() -> SharedStorage {
        SharedStorage::new(Storage::open_in_memory().expect("in-memory storage"))
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

    fn seed_version(conn: &Connection, version_id: &str, plan: &CompiledPlan) {
        let plan_json = serde_json::to_string(plan).expect("encode plan");
        // Published versions must carry immutable input/workspace authority;
        // executor deliberately fails closed when either is absent.
        let definition_json = serde_json::json!({
            "spec": {
                "inputs": {},
                "budgets": {"maxTokens": 1024},
                "workspace": {"root": "/tmp/coderight-test", "trust": "trusted"}
            }
        })
        .to_string();
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
                definition_json,
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

    fn node(id: &str, edges: &[&str]) -> NodeSpec {
        NodeSpec {
            id: id.to_string(),
            kind: NodeKind::Transform,
            label: None,
            capabilities: CapabilitySet::from_iter([Capability::new("fs.read")]),
            edges: edges
                .iter()
                .map(|t| EdgeSpec {
                    target: (*t).to_string(),
                    when: None,
                })
                .collect(),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffStrategy::Fixed { delay_ms: 100 },
                retryable: Vec::new(),
            },
            config: NodeConfig::Execution {
                runtime: "native".to_string(),
                instruction: String::new(),
                runtime_target: None,
                recipe_ref: None,
                inputs_json: "{}".to_string(),
                transform_operation: Some("identity".to_string()),
                template: None,
                max_runtime_ms: 1_000,
                max_output_bytes: 1_024,
                output_schema: "{}".to_string(),
            },
        }
    }

    fn two_node_plan() -> CompiledPlan {
        CompiledPlan {
            entry_node: "n1".to_string(),
            nodes: vec![node("n1", &["n2"]), node("n2", &[])],
            effective_capabilities: CapabilitySet::empty(),
        }
    }

    fn state(id: &str, phase: &str) -> WorkflowNodeStateRow {
        WorkflowNodeStateRow {
            run_id: "wfr-test".into(),
            node_id: id.into(),
            phase: phase.into(),
            attempt: 1,
            lease_holder: None,
            lease_expires_at: None,
            last_reducer_revision: 1,
            last_event_seq: 1,
            last_failure_class: None,
            last_updated_at: "2026-08-07T00:00:00Z".into(),
        }
    }

    #[test]
    fn selection_excludes_completed_and_unselected_nodes() {
        let mut plan = two_node_plan();
        assert_eq!(
            pick_ready_node(&plan, &[state("n1", "succeeded")], &HashMap::new())
                .expect("next")
                .id,
            "n2"
        );
        plan.nodes[0].kind = NodeKind::Condition;
        plan.nodes[0].edges.push(EdgeSpec {
            target: "n3".into(),
            when: Some("false".into()),
        });
        plan.nodes.push(node("n3", &[]));
        let selections = HashMap::from([("n1".into(), vec!["n2".into()])]);
        assert!(!reachable_nodes(&plan, &selections).contains("n3"));
        assert_eq!(
            pick_ready_node(&plan, &[state("n1", "succeeded")], &selections)
                .expect("selected branch")
                .id,
            "n2"
        );
    }

    #[test]
    fn iteration_scope_is_bounded_and_replay_stable() {
        let input = serde_json::json!({"items":["a","b"]});
        let first =
            iteration_scope("map__iter_1", NodeKind::Map, &input).expect("canonical map child");
        assert_eq!(first.0, 1);
        assert_eq!(first.1, IterationKind::Map);
        assert_eq!(first.2, serde_json::json!("b"));
        assert_eq!(
            iteration_scope("map__iter_2", NodeKind::Map, &input)
                .unwrap()
                .2,
            serde_json::Value::Null
        );
        assert!(iteration_scope("map__iter_x", NodeKind::Map, &input).is_none());
    }

    #[test]
    fn control_children_materialize_once_from_durable_selection() {
        let mut control = node("map", &["body"]);
        control.kind = NodeKind::Map;
        control.config = NodeConfig::Map { max_items: 2 };
        let plan = CompiledPlan {
            entry_node: "map".into(),
            nodes: vec![control, node("body", &[])],
            effective_capabilities: CapabilitySet::empty(),
        };
        let selected = HashMap::from([(
            "map".into(),
            vec!["map__iter_0".into(), "map__iter_1".into()],
        )]);
        let replay = materialize_control_children(&plan, &selected);
        assert!(replay.nodes.iter().any(|node| node.id == "map__iter_0"));
        assert!(replay.nodes.iter().any(|node| node.id == "map__iter_1"));
        assert!(!replay.nodes.iter().any(|node| node.id == "body"));
        assert_eq!(
            materialize_control_children(&replay, &selected).nodes,
            replay.nodes
        );
    }

    /// Test dispatcher that always succeeds and returns a synthetic
    /// `NodeOutput`. The default `NodeDispatcher` rejects every kind in
    /// WP-03a, so we can't use it to exercise the `Advanced` path.
    struct TestOkDispatcher;

    #[async_trait]
    impl ExecuteNode for TestOkDispatcher {
        async fn execute_node(
            &self,
            _spec: &NodeSpec,
            _ctx: &NodeDispatchContext,
        ) -> Result<NodeOutput, DispatcherError> {
            Ok(NodeOutput {
                output_json: serde_json::json!({"ok": true}),
                artifacts: Vec::new(),
                cost_usd_micros: 0,
                tokens_used: 0,
            })
        }
    }

    fn build_executor(
        storage: SharedStorage,
        dispatcher: Arc<dyn ExecuteNode + Send + Sync>,
        limiters: ResourceLimiters,
    ) -> (Arc<RunSupervisor>, Arc<dyn Clock>, NodeExecutor) {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_700_000_000_000));
        let feature = FeatureGate::from_bool(true);
        let supervisor = Arc::new(RunSupervisor::new(storage.clone(), feature, clock.clone()));
        let executor = NodeExecutor::with_test_dispatcher(
            storage,
            supervisor.clone(),
            dispatcher,
            clock.clone(),
            limiters,
        );
        (supervisor, clock, executor)
    }

    /// `executor_picks_first_ready_node`: a 2-node CompiledPlan where
    /// only the entry has zero in-edges produces `advanced_nodes == ["n1"]`
    /// in a single pass.
    #[tokio::test(flavor = "current_thread")]
    async fn executor_picks_first_ready_node() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        let plan = two_node_plan();
        seed_version(conn, "wfv-seed", &plan);
        seed_run(conn, "wfr-test", "wfv-seed");
        drop(storage_guard);

        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_700_000_000_000));
        let feature = FeatureGate::from_bool(true);
        let supervisor = Arc::new(RunSupervisor::new(storage.clone(), feature, clock.clone()));
        let claim = supervisor
            .claim_run("wfr-test", "supervisor", 60)
            .await
            .expect("claim run");

        let dispatcher: Arc<dyn ExecuteNode + Send + Sync> = Arc::new(TestOkDispatcher);
        let limiters = ResourceLimiters {
            max_concurrent_per_run: 1,
            max_total_concurrent: 1,
            max_input_bytes: 1024,
        };
        let executor = NodeExecutor::with_test_dispatcher(
            storage.clone(),
            supervisor,
            dispatcher,
            clock,
            limiters,
        );

        let pass = executor.run_pass(claim).await.expect("run_pass succeeds");
        match pass {
            PassOutcome::Advanced {
                run_id,
                advanced_nodes,
            } => {
                assert_eq!(run_id, "wfr-test");
                assert_eq!(
                    advanced_nodes,
                    vec!["n1".to_string()],
                    "executor must advance the entry node first"
                );
            }
            other => panic!("expected PassOutcome::Advanced, got {other:?}"),
        }

        // And the journal must contain the three node-state events.
        let repo_guard = storage.try_lock().expect("storage lock");
        let repo = WorkflowRepository::new(repo_guard.conn());
        let journal = repo.replay_journal("wfr-test").expect("replay journal");
        let node_kinds: Vec<&str> = journal
            .iter()
            .filter_map(|r| {
                if r.node_id.is_some() {
                    Some(r.event_kind.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            node_kinds,
            vec!["node_claimed", "node_started", "node_completed"],
            "executor must emit claimed/started/completed for the advanced node"
        );
    }

    /// `executor_returns_busy_when_limit_reached`: `max_total_concurrent = 0`
    /// rejects every acquire — the executor must return `Busy` even when
    /// there is a ready node to advance.
    #[tokio::test(flavor = "current_thread")]
    async fn executor_returns_busy_when_limit_reached() {
        let storage = open_storage();
        let storage_guard = storage.try_lock().expect("storage lock");
        let conn = storage_guard.conn();
        let plan = two_node_plan();
        seed_version(conn, "wfv-seed", &plan);
        seed_run(conn, "wfr-test", "wfv-seed");
        drop(storage_guard);

        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_700_000_000_000));
        let feature = FeatureGate::from_bool(true);
        let supervisor = Arc::new(RunSupervisor::new(storage.clone(), feature, clock.clone()));
        let claim = supervisor
            .claim_run("wfr-test", "supervisor", 60)
            .await
            .expect("claim run");

        let dispatcher: Arc<dyn ExecuteNode + Send + Sync> = Arc::new(TestOkDispatcher);
        let limiters = ResourceLimiters {
            max_concurrent_per_run: 1,
            max_total_concurrent: 0,
            max_input_bytes: 1024,
        };
        let executor =
            NodeExecutor::with_test_dispatcher(storage, supervisor, dispatcher, clock, limiters);

        let pass = executor.run_pass(claim).await.expect("run_pass");
        assert!(
            matches!(pass, PassOutcome::Busy),
            "executor must refuse acquire when max_total_concurrent = 0; got {pass:?}",
        );
    }
}
