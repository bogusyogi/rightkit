//! The orchestration loop — drives a `TaskGraph` to completion by routing each
//! ready task to an agent and executing dependency-respecting batches through
//! the [`Conductor`].
//!
//! This is the spine that connects the four primitives that already existed in
//! isolation: the [`TaskGraph`] (dependency DAG), the [`AgentRouter`]
//! (task → agent), the [`Conductor`] (parallel/sequential executor), and a
//! [`Worker`] (the thing that actually runs an agent). It is intentionally pure
//! — it takes a `Worker` and an update callback so it can be unit-tested with
//! `EchoWorker`/`FailWorker` and wired in the daemon to a worker that spawns
//! real `SessionEngine` agents.

mod contract;

use std::sync::Arc;

use rightkit_agent_engine_protocol::TaskState;
use rightkit_agent_engine_tasks::TaskGraph;

use crate::coding::conductor::{
    Conductor, ConductorPlan, Worker, WorkerOutcome, WorkerResult, WorkerTask,
};
use crate::coding::conductor_cache_key::conductor_stable_cache_key;
use crate::coding::handoff_brief::HandoffArchiveConfig;
use crate::coding::predicate::{PredicateContext, PredicateEvaluator};
use crate::coding::recovery::{RecoveringWorker, StepLocator};
use crate::coding::registry::AgentRegistry;
use crate::coding::review_panel::{execute_panel, PanelRunResult, ReviewPanel};
use crate::coding::router::AgentRouter;
use crate::coding::watchdog::RunWatchdog;
use contract::{
    contract_failure_result, handoff_brief_payload, task_payload, validate_contract,
    validate_output_contract, worker_task_contract_value,
};

/// Turn a worker's self-reported success into a typed failure when no
/// environment-observable completion predicate was supplied.
///
/// `WorkerOutcome::Complete` still accurately describes the worker process
/// reaching its return path; it does not certify that the task's requested
/// work happened. Keep that distinction explicit by retaining the output for
/// handoff while clearing `success` and attaching the existing catch-all
/// efficacy failure kind. This is deliberately fail-closed: callers must add
/// an observable predicate before a task can enter `Done`.
fn unverified_completion_result(mut result: WorkerResult) -> WorkerResult {
    result.success = false;
    result.failure_kind = Some(crate::coding::failure::FailureKind::Unknown);
    result
}

fn has_environment_completion_observation(
    predicates: &[rightkit_agent_engine_protocol::DoneWhenPredicate],
) -> bool {
    !predicates.is_empty()
}

fn inject_forced_pivot(payload: String, nudge: Option<&str>) -> String {
    let Some(nudge) = nudge else {
        return payload;
    };
    format!(
        "{payload}\n\n[WATCHDOG FORCED PIVOT]\nThe previous approach stalled. Change approach now, preserve verified work, and continue toward the task's completion predicate.\nPivot instruction: {nudge}"
    )
}

fn requeue_forced_pivot(
    graph: &mut TaskGraph,
    iteration_results: &[WorkerResult],
    failed: &mut Vec<String>,
) -> bool {
    let mut reopened = false;
    for result in iteration_results {
        if let Some(node) = graph.get_mut(&result.task_id) {
            if node.state == TaskState::Failed {
                node.state = TaskState::Open;
                failed.retain(|id| id != &result.task_id);
                reopened = true;
            }
        }
    }
    graph.recompute_states();
    reopened || !graph.ready_tasks().is_empty()
}

/// The result of driving a [`TaskGraph`] to a terminal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    /// True only when every task ended in `Done`.
    pub completed: bool,
    /// Worker results in completion order.
    pub results: Vec<WorkerResult>,
    /// Ids of tasks that failed or could not be routed to any agent.
    pub failed: Vec<String>,
    /// Set when the watchdog detected a stall; contains a human-readable nudge.
    pub stall_nudge: Option<String>,
}

/// Drive `graph` to a terminal state.
///
/// Each iteration: recompute blocked/ready state, take every task whose
/// dependencies are satisfied, route it to an agent (unroutable tasks fail),
/// then run the routable batch in parallel through the [`Conductor`]. Results
/// mark tasks `Done`/`Failed`; the loop ends when no task can make progress.
/// `on_update` is invoked after every state transition so a caller can stream
/// `TaskGraphUpdated` events.
///
/// `max_concurrent_tasks`: cap on how many tasks run in one batch (0 = unlimited).
///
/// When `conductor_identity` and `conductor_day` are both `Some`, every
/// sibling task spawned in this run carries the same
/// `Some(shared_key)` value on `WorkerTask::conductor_stable_cache_key`
/// (cross-worker prefix-cache sharing, ROADMAP P1). Pass `None, None` to
/// keep the single-worker (direct) path unchanged.
pub async fn run_graph(
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    mut on_update: impl FnMut(&TaskGraph),
) -> RunOutcome {
    run_graph_with_conductor(
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        &mut on_update,
        None,
        None,
    )
    .await
}

/// FM-062 — construct a [`RecoveringWorker`] at the `run_graph` seam so
/// `StepLocator` + `rollback_step` is live. Daemon already wraps `SpawnWorker`
/// in `core_helpers.rs:149`; this helper is the orchestration-crate's
/// non-test caller required to close the gap.
pub fn recovering_worker(
    inner: Arc<dyn Worker>,
    max_attempts: u32,
    per_task_timeout: std::time::Duration,
) -> Arc<dyn Worker> {
    // StepLocator is exercised inside RecoveringWorker::run via
    // StepLocator::locate / last_good_step, which populates
    // WorkerResult::rollback_step.
    let _locator_hint = StepLocator::locate("");
    Arc::new(RecoveringWorker::new(inner, max_attempts, per_task_timeout))
}

/// Same as [`run_graph`] but wraps `worker` in a [`RecoveringWorker`] at the
/// seam so failing outputs get a `rollback_step` hint via `StepLocator`.
pub async fn run_graph_with_recovery(
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    mut on_update: impl FnMut(&TaskGraph),
) -> RunOutcome {
    let worker = recovering_worker(worker, 3, std::time::Duration::from_secs(120));
    run_graph(
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        &mut on_update,
    )
    .await
}

/// Same as [`run_graph`] but lets the caller opt in to cross-worker
/// prefix-cache sharing by passing `(conductor_identity, conductor_day)`.
///
/// When both are `Some`, every sibling worker spawned in this run
/// carries the same conductor-stable cache key, so the workers
/// cache-read against the same slot instead of each independently
/// recomputing. The handoff brief (per-worker CONTEXT tier) is
/// excluded by construction.
#[allow(clippy::too_many_arguments)]
pub async fn run_graph_with_conductor(
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    mut on_update: impl FnMut(&TaskGraph),
    conductor_identity: Option<String>,
    conductor_day: Option<String>,
) -> RunOutcome {
    run_graph_with_conductor_and_handoff_archive(
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        &mut on_update,
        conductor_identity,
        conductor_day,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_graph_with_conductor_and_handoff_archive(
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    mut on_update: impl FnMut(&TaskGraph),
    conductor_identity: Option<String>,
    conductor_day: Option<String>,
    handoff_archive: Option<HandoffArchiveConfig>,
) -> RunOutcome {
    run_graph_with_checkpoint_hook(
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        &mut on_update,
        conductor_identity,
        conductor_day,
        handoff_archive.as_ref(),
        Vec::new(),
        |_, _| Ok::<(), std::convert::Infallible>(()),
    )
    .await
    .expect("infallible checkpoint hook cannot fail")
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_graph_with_checkpoint_hook<E>(
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    mut on_update: impl FnMut(&TaskGraph),
    conductor_identity: Option<String>,
    conductor_day: Option<String>,
    handoff_archive: Option<&HandoffArchiveConfig>,
    initial_results: Vec<WorkerResult>,
    mut checkpoint: impl FnMut(&TaskGraph, &[WorkerResult]) -> Result<(), E>,
) -> Result<RunOutcome, E> {
    let mut results = initial_results;
    let mut failed: Vec<String> = Vec::new();
    let mut watchdog = RunWatchdog::new(3);
    let mut stall_nudge: Option<String> = None;
    let mut pending_pivot: Option<String> = None;
    let mut forced_pivot_used = false;

    // Compute the shared stable cache key ONCE per run when the caller
    // opted in to cross-worker prefix-cache sharing. Every sibling task
    // dispatched below carries `Some(shared_key)` on its
    // `conductor_stable_cache_key` field so each sibling cache-reads
    // against the same slot instead of each independently recomputing
    // (cache-break). `None` for the single-worker (direct) path.
    let shared_conductor_cache_key: Option<String> = match (&conductor_identity, &conductor_day) {
        (Some(id), Some(day)) => Some(conductor_stable_cache_key(id, day)),
        _ => None,
    };

    loop {
        graph.recompute_states();

        // Snapshot ready task ids (immutable borrow ends here).
        let ready_ids: Vec<String> = graph.ready_tasks().iter().map(|t| t.id.clone()).collect();
        if ready_ids.is_empty() {
            break;
        }

        // Route each ready task; unroutable tasks fail immediately.
        let mut batch: Vec<WorkerTask> = Vec::new();
        let mut panel_batch: Vec<(String, Option<ReviewPanel>, ReviewPanel, String)> = Vec::new();
        let mut to_fail: Vec<String> = Vec::new();
        let mut contract_failures: Vec<WorkerResult> = Vec::new();
        for id in &ready_ids {
            let node = match graph.get(id) {
                Some(n) => n,
                None => continue,
            };
            match router.route_with_trace(registry, node) {
                Some(decision) if decision.jury_panel.is_some() => {
                    panel_batch.push((
                        id.clone(),
                        decision.council_panel,
                        decision.jury_panel.unwrap(),
                        inject_forced_pivot(task_payload(goal, node), pending_pivot.as_deref()),
                    ));
                }
                Some(decision) => {
                    let Some(agent) = registry.get(&decision.agent_name) else {
                        to_fail.push(id.clone());
                        continue;
                    };
                    let agent_version = agent.version.clone();
                    let payload = inject_forced_pivot(
                        handoff_brief_payload(goal, node, Some(agent), &results, handoff_archive),
                        pending_pivot.as_deref(),
                    );
                    let task = WorkerTask {
                        task_id: id.clone(),
                        agent: agent.name.clone(),
                        payload,
                        agent_version,
                        conductor_stable_cache_key: shared_conductor_cache_key.clone(),
                    };
                    if let Some(schema) = &agent.input_schema {
                        let value = worker_task_contract_value(&task);
                        if let Err(err) = validate_contract(schema, &value) {
                            to_fail.push(id.clone());
                            contract_failures.push(contract_failure_result(
                                &task,
                                format!("input schema validation failed: {err}"),
                            ));
                            continue;
                        }
                    }
                    batch.push(task)
                }
                None => to_fail.push(id.clone()),
            }
        }

        for id in &to_fail {
            if let Some(n) = graph.get_mut(id) {
                n.state = TaskState::Failed;
            }
            failed.push(id.clone());
        }
        results.append(&mut contract_failures);

        if batch.is_empty() && panel_batch.is_empty() {
            // Only unroutable tasks were ready. Their dependents will block and
            // the next iteration will find nothing ready, ending the loop.
            checkpoint(graph, &[])?;
            on_update(graph);
            continue;
        }

        // The pivot belongs to this next dispatched batch only, including
        // every sibling selected by the concurrency cap.
        pending_pivot = None;

        // Apply concurrency cap: excess tasks remain Open and are picked up next iteration.
        if max_concurrent_tasks > 0 {
            let available = max_concurrent_tasks;
            if batch.len() > available {
                batch.truncate(available);
                panel_batch.clear();
            } else {
                let remaining = available.saturating_sub(batch.len());
                if panel_batch.len() > remaining {
                    panel_batch.truncate(remaining);
                }
            }
        }

        // Mark the batch in-progress and announce.
        for wt in &batch {
            if let Some(n) = graph.get_mut(&wt.task_id) {
                n.state = TaskState::InProgress;
            }
        }
        for (task_id, _, _, _) in &panel_batch {
            if let Some(n) = graph.get_mut(task_id) {
                n.state = TaskState::InProgress;
            }
        }
        checkpoint(graph, &[])?;
        on_update(graph);

        // Panel-routed tasks produce a verdict surface, not citation-bearing
        // findings prose — exclude them from the citation auto-inject below.
        let panel_task_ids: std::collections::HashSet<String> =
            panel_batch.iter().map(|(id, _, _, _)| id.clone()).collect();

        // Execute the routable worker and panel batches in parallel.
        let plan = ConductorPlan::new(batch, true);
        let conductor = Conductor::new(worker.clone());
        let worker_results = conductor.execute(&plan);
        let panel_worker = worker.clone();
        let panel_results = async move {
            let mut handles = Vec::new();
            for (task_id, council, panel, payload) in panel_batch {
                let worker = panel_worker.clone();
                handles.push(tokio::spawn(async move {
                    let council_run = match council {
                        Some(council) => {
                            Some(execute_panel(&council, &task_id, &payload, worker.clone()).await)
                        }
                        None => None,
                    };
                    let jury_payload = with_council_preflight(&payload, council_run.as_ref());
                    let run = execute_panel(&panel, &task_id, &jury_payload, worker).await;
                    WorkerResult {
                        task_id,
                        agent: run.panel.id.clone(),
                        success: run.success,
                        output: summarize_panel_sequence(council_run.as_ref(), &run),
                        input_tokens: 0,
                        mop_fired: false,
                        failure_kind: if run.success {
                            None
                        } else {
                            Some(crate::coding::failure::FailureKind::Unknown)
                        },
                        agent_version: None,
                        rollback_step: None,
                        outcome: WorkerOutcome::Complete,
                    }
                }));
            }
            let mut out = Vec::new();
            for handle in handles {
                if let Ok(result) = handle.await {
                    out.push(result);
                }
            }
            out
        };
        let (mut batch_results, mut panel_results) = tokio::join!(worker_results, panel_results);
        batch_results.append(&mut panel_results);
        let batch_results: Vec<_> = batch_results
            .into_iter()
            .map(|result| validate_output_contract(registry, result))
            .collect();

        let any_success = batch_results.iter().any(|r| r.success);
        if any_success {
            watchdog.record_progress();
            stall_nudge = None;
        } else {
            watchdog.tick_without_progress();
        }

        let mut iteration_results = Vec::new();
        for r in batch_results {
            // A task is Done only if the worker succeeded AND an observable
            // DoneWhen predicate holds — never on the worker's self-reported
            // success alone. Empty predicates are an unverified completion.
            //
            // A `PartialWithLimit`/`ApiError` outcome NEVER counts as
            // satisfied, even if `success` is (incorrectly, or by a stale
            // caller) `true` — the conductor's honesty guarantee must not
            // depend on every producer getting the boolean right (parity
            // P-2). The partial output still flows into synthesis/handoff
            // via `r.output`, marked as partial by its `outcome`.
            let mut r = r;
            let predicates_met = if r.success && r.outcome.counts_as_complete() {
                let predicates = {
                    let mut preds = graph
                        .get(&r.task_id)
                        .map(|n| n.done_when_predicates.clone())
                        .unwrap_or_default();
                    if preds.is_empty() {
                        if panel_task_ids.contains(&r.task_id) {
                            // A panel's parsed verdict is the environment
                            // observation for this path; do not trust the
                            // worker's success bit by itself.
                            preds.push(rightkit_agent_engine_protocol::DoneWhenPredicate {
                                predicate: "contains:Decision:".into(),
                                weight: 1.0,
                            });
                        } else if let Some(n) = graph.get(&r.task_id) {
                            if matches!(
                                n.category.as_str(),
                                "audit" | "research" | "security" | "review"
                            ) {
                                preds.push(rightkit_agent_engine_protocol::DoneWhenPredicate {
                                    predicate: "has_citations".into(),
                                    weight: 1.0,
                                });
                            }
                        }
                    }
                    preds
                };
                if !has_environment_completion_observation(&predicates) {
                    r = unverified_completion_result(r);
                    false
                } else {
                    let ctx = PredicateContext {
                        output: &r.output,
                        success: r.success,
                        exit_code: None,
                    };
                    let pred_results = PredicateEvaluator::evaluate_all(&predicates, &ctx);
                    PredicateEvaluator::is_done(&pred_results, r.success, 1.0)
                }
            } else {
                false
            };

            if predicates_met {
                let _ = graph.mark_done(&r.task_id);
            } else {
                // Either the worker failed, or it claimed success but the
                // DoneWhen predicates are unsatisfied (unverified completion).
                // Escalate to the next role if one remains, else mark Failed.
                let escalated = if let Some(n) = graph.get_mut(&r.task_id) {
                    let next_role = n
                        .escalation_path
                        .split(',')
                        .map(str::trim)
                        .find(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    if let Some(role) = next_role {
                        let remaining: Vec<&str> = n
                            .escalation_path
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .skip(1)
                            .collect();
                        n.assigned_role = role;
                        n.escalation_path = remaining.join(",");
                        // Mark as Escalated (transient), then next iteration will
                        // recompute to Open when dependencies are satisfied.
                        n.state = TaskState::Escalated;
                        true
                    } else {
                        n.state = TaskState::Failed;
                        false
                    }
                } else {
                    false
                };
                if !escalated {
                    failed.push(r.task_id.clone());
                }
            }
            iteration_results.push(r.clone());
            results.push(r);
        }
        checkpoint(graph, &iteration_results)?;
        on_update(graph);

        if watchdog.is_stalled() {
            let nudge = watchdog.nudge_message();
            if !forced_pivot_used && requeue_forced_pivot(graph, &iteration_results, &mut failed) {
                forced_pivot_used = true;
                pending_pivot = Some(nudge);
                watchdog.record_progress();
                checkpoint(graph, &[])?;
                on_update(graph);
                continue;
            }
            stall_nudge = Some(nudge);
            break;
        }
    }

    let completed = stall_nudge.is_none()
        && graph.tasks().all(|t| t.state == TaskState::Done)
        && !graph.is_empty();
    Ok(RunOutcome {
        completed,
        results,
        failed,
        stall_nudge,
    })
}

fn with_council_preflight(payload: &str, council_run: Option<&PanelRunResult>) -> String {
    match council_run {
        Some(run) => format!(
            "{payload}\n\nAdvisory council preflight (not a verdict):\n{}",
            run.output
        ),
        None => payload.to_string(),
    }
}

fn summarize_panel_sequence(
    council_run: Option<&PanelRunResult>,
    jury_run: &PanelRunResult,
) -> String {
    match council_run {
        Some(council) => format!(
            "Advisory Council Preflight:\n{}\n\nVerdict Jury:\n{}",
            council.output, jury_run.output
        ),
        None => jury_run.output.clone(),
    }
}

#[cfg(test)]
mod environment_completion_tests {
    use super::*;

    fn worker_result() -> WorkerResult {
        WorkerResult {
            task_id: "task-1".into(),
            agent: "agent".into(),
            success: true,
            output: "worker says done".into(),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: None,
            agent_version: None,
            rollback_step: None,
            outcome: WorkerOutcome::Complete,
        }
    }

    #[test]
    fn empty_predicates_are_not_an_environment_observation() {
        assert!(!has_environment_completion_observation(&[]));

        let result = unverified_completion_result(worker_result());
        assert!(!result.success);
        assert_eq!(
            result.failure_kind,
            Some(crate::coding::failure::FailureKind::Unknown)
        );
        assert_eq!(result.outcome, WorkerOutcome::Complete);
        assert_eq!(result.output, "worker says done");
    }

    #[test]
    fn explicit_predicate_remains_an_environment_observation() {
        let predicates = [rightkit_agent_engine_protocol::DoneWhenPredicate {
            predicate: "contains:verified".into(),
            weight: 1.0,
        }];
        assert!(has_environment_completion_observation(&predicates));
    }
}

#[cfg(test)]
#[path = "orchestrator_tests.rs"]
mod orchestrator_tests;
