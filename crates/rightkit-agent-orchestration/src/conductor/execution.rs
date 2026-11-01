//! The conductor's execution engine — runs worker tasks sequentially or in parallel.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;

use super::types::{ConductorPlan, WorkerCollection, WorkerOutcome, WorkerResult, WorkerTask};
use crate::conductor_cache_key::conductor_stable_cache_key;
use crate::failure::FailureKind;
use crate::registry::AgentRegistry;
use crate::spawn::{spawn_with_capabilities, SpawnPlan, SpawnRequest};
use crate::teams::section_context::SectionContext;

/// A worker capable of executing a single [`WorkerTask`].
#[async_trait]
pub trait Worker: Send + Sync {
    async fn run(&self, task: &WorkerTask) -> WorkerResult;
}

/// Drives a [`Worker`] over a [`ConductorPlan`].
pub struct Conductor {
    worker: Arc<dyn Worker>,
    max_in_flight: usize,
    section_context: Option<Arc<SectionContext>>,
}

impl Conductor {
    pub fn new(worker: Arc<dyn Worker>) -> Self {
        Self {
            worker,
            max_in_flight: 8,
            section_context: None,
        }
    }

    /// Attach a shared team context that is read-only from each dispatched
    /// worker's perspective and absent from the legacy path by default.
    pub fn with_section_context(mut self, section_context: SectionContext) -> Self {
        self.section_context = Some(Arc::new(section_context));
        self
    }

    /// Apply the shared team context without changing task identity or any
    /// other dispatch metadata.
    fn task_with_section_context(&self, task: &WorkerTask) -> WorkerTask {
        let Some(context) = &self.section_context else {
            return task.clone();
        };
        let Some(fragment) = context.prompt_fragment() else {
            return task.clone();
        };
        WorkerTask {
            payload: format!("{fragment}\n\n{}", task.payload),
            ..task.clone()
        }
    }

    /// Apply a local hard cap to expanded plan fan-out. A zero supplied cap
    /// is normalized to one so every non-empty plan can make progress.
    pub fn with_max_in_flight(mut self, max_in_flight: usize) -> Self {
        self.max_in_flight = max_in_flight.max(1);
        self
    }

    /// Select a subagent by capability tags at spawn time and return the
    /// runnable task plus transcript artifact describing the model choice.
    ///
    /// When `request.conductor_stable_cache_key` is `Some`, the chosen
    /// `SpawnPlan` carries that key verbatim — every sibling spawned
    /// under the same conductor run shares the same stable-prefix
    /// slot, so they cache-read against each other instead of each
    /// independently recomputing (cache-break).
    pub fn spawn_with_capabilities(
        &self,
        registry: &AgentRegistry,
        request: SpawnRequest,
    ) -> Option<SpawnPlan> {
        spawn_with_capabilities(registry, request)
    }

    /// Run all tasks one at a time, in plan order.
    ///
    /// The plan is expanded via [`expand_plan_for_cache_sharing`] before
    /// dispatch so every sibling task carries the conductor's shared
    /// stable cache key (if the plan was built with conductor identity).
    pub async fn run_sequential(&self, plan: &ConductorPlan) -> Vec<WorkerResult> {
        let expanded = expand_plan_for_cache_sharing(plan);
        let mut results = Vec::with_capacity(expanded.tasks.len());
        for task in &expanded.tasks {
            let task = self.task_with_section_context(task);
            results.push(self.worker.run(&task).await);
        }
        results
    }

    /// Run tasks with bounded fan-out and surface each result as it becomes
    /// ready. Durable identity stays on each result's `task_id`; callers must
    /// not infer plan identity from vector position.
    ///
    /// If a worker's `run()` panics, `JoinHandle::await` returns `Err`
    /// (tokio's `JoinError`) for exactly that task instead of taking any
    /// sibling down with it. That slot resolves to a failed [`WorkerResult`]
    /// typed [`FailureKind::WorkerPanic`], carrying the panic message as
    /// `output` — every other sibling's real result is untouched. Order is
    /// Results are returned in completion order so an early ready result is
    /// never held behind a slow earlier plan item.
    ///
    /// The plan is expanded via [`expand_plan_for_cache_sharing`] before
    /// dispatch so every sibling task carries the conductor's shared
    /// stable cache key.
    pub async fn run_parallel(&self, plan: &ConductorPlan) -> Vec<WorkerResult> {
        let expanded = expand_plan_for_cache_sharing(plan);
        let n = expanded.tasks.len();
        if n == 0 {
            return Vec::new();
        }

        let mut pending = VecDeque::from(expanded.tasks);
        let mut running = tokio::task::JoinSet::new();
        let mut running_tasks = HashMap::new();
        let mut results = Vec::with_capacity(n);
        while running.len() < self.max_in_flight {
            let Some(task) = pending.pop_front() else {
                break;
            };
            let task = self.task_with_section_context(&task);
            spawn_bounded_worker(&mut running, &mut running_tasks, &self.worker, task);
        }
        while let Some(joined) = running.join_next_with_id().await {
            match joined {
                Ok((id, result)) => {
                    running_tasks.remove(&id);
                    results.push(result);
                }
                Err(join_err) => {
                    let task = running_tasks
                        .remove(&join_err.id())
                        .expect("every bounded worker id is registered before polling");
                    results.push(worker_panic_result(&task, &join_err));
                }
            }
            if let Some(task) = pending.pop_front() {
                let task = self.task_with_section_context(&task);
                spawn_bounded_worker(&mut running, &mut running_tasks, &self.worker, task);
            }
        }
        results
    }

    /// Dispatch to parallel or sequential execution based on `plan.parallel`.
    pub async fn execute(&self, plan: &ConductorPlan) -> Vec<WorkerResult> {
        self.collect(plan).await.into_results()
    }

    /// Collect every worker result before exposing the downstream boundary.
    ///
    /// The returned type deliberately represents a completed collection. Use
    /// [`Conductor::execute_with_synthesis`] when a final task must receive a
    /// synthesized view of this batch.
    pub async fn collect(&self, plan: &ConductorPlan) -> WorkerCollection {
        let results = if plan.parallel {
            self.run_parallel(plan).await
        } else {
            self.run_sequential(plan).await
        };
        WorkerCollection::new(results)
    }

    /// Collect worker outputs, synthesize them, then dispatch the final task.
    ///
    /// The final task is not run until collection has completed and its
    /// payload has been assembled by [`synthesize_worker_outputs`]. The first
    /// tuple item remains available for durable accounting and downstream
    /// inspection; the second is the final dispatched result.
    pub async fn execute_with_synthesis(
        &self,
        plan: &ConductorPlan,
        final_task: WorkerTask,
        max_payload_bytes: usize,
    ) -> Result<(WorkerCollection, WorkerResult), String> {
        let collected = self.collect(plan).await;
        let final_result = self
            .execute_synthesis(plan, &collected, final_task, max_payload_bytes)
            .await?;
        Ok((collected, final_result))
    }

    /// Synthesize an already collected batch, then dispatch one final task.
    /// Callers can inspect failures, budgets, or policy between collection &
    /// final dispatch without rebuilding synthesis rules.
    pub async fn execute_synthesis(
        &self,
        source_plan: &ConductorPlan,
        collected: &WorkerCollection,
        mut final_task: WorkerTask,
        max_payload_bytes: usize,
    ) -> Result<WorkerResult, String> {
        final_task.payload = synthesize_worker_outputs(
            &final_task.payload,
            collected.as_slice(),
            max_payload_bytes,
        )?;
        let final_plan = ConductorPlan {
            tasks: vec![final_task],
            parallel: false,
            conductor_identity: source_plan.conductor_identity.clone(),
            conductor_day: source_plan.conductor_day.clone(),
        };
        let final_result = self
            .run_sequential(&final_plan)
            .await
            .into_iter()
            .next()
            .expect("one final synthesis task must dispatch");
        Ok(final_result)
    }
}

/// Build the deterministic synthesis payload consumed by a final/downstream
/// worker. Worker completion order is irrelevant: fragments are content-
/// addressed, duplicate normalized outputs are absorbed once, and fragments
/// render in canonical task/hash order.
pub fn synthesize_worker_outputs(
    original: &str,
    results: &[WorkerResult],
    max_payload_bytes: usize,
) -> Result<String, String> {
    const SYNTHESIS_INSTRUCTIONS: &str = "\n\nYou are the final routed-orchestration synthesizer. Review the independent drafts below, resolve conflicts, and return only the final answer required by the original task. Do not mention this evaluation or the drafts.\n";

    let mut fragments = BTreeMap::<(String, String), CanonicalFragment>::new();
    for result in results {
        let normalized = normalize_synthesis_text(&result.output);
        let hash = rightkit_fs::digest::sha256_bytes(normalized.as_bytes());
        let key = (hash.clone(), normalized);
        let entry = fragments.entry(key).or_insert_with(|| CanonicalFragment {
            task_id: result.task_id.clone(),
            agent: result.agent.clone(),
            output: result.output.clone(),
            hash,
            reporters: BTreeSet::new(),
        });
        entry.reporters.insert(result.agent.clone());
        let representative = (&result.task_id, &result.agent, &result.output);
        let current = (&entry.task_id, &entry.agent, &entry.output);
        if representative < current {
            entry.task_id = result.task_id.clone();
            entry.agent = result.agent.clone();
            entry.output = result.output.clone();
        }
    }

    let mut ordered: Vec<_> = fragments.into_values().collect();
    ordered.sort_by(|left, right| {
        left.task_id
            .cmp(&right.task_id)
            .then_with(|| left.hash.cmp(&right.hash))
    });

    let mut out = String::with_capacity(original.len().saturating_add(1024));
    out.push_str(original);
    out.push_str(SYNTHESIS_INSTRUCTIONS);
    for (index, fragment) in ordered.iter().enumerate() {
        out.push_str(&format!("\n--- draft {} ({})", index + 1, fragment.agent));
        if fragment.reporters.len() > 1 {
            out.push_str(&format!(
                "; reported by workers {}",
                fragment
                    .reporters
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str(" ---\n");
        out.push_str(&fragment.output);
        if out.len() > max_payload_bytes {
            return Err("candidate draft bundle exceeded the bounded synthesis limit".into());
        }
    }
    Ok(out)
}

struct CanonicalFragment {
    task_id: String,
    agent: String,
    output: String,
    hash: String,
    reporters: BTreeSet<String>,
}

fn normalize_synthesis_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn spawn_bounded_worker(
    running: &mut tokio::task::JoinSet<WorkerResult>,
    running_tasks: &mut HashMap<tokio::task::Id, WorkerTask>,
    worker: &Arc<dyn Worker>,
    task: WorkerTask,
) {
    let task_for_run = task.clone();
    let worker = Arc::clone(worker);
    let abort_handle = running.spawn(async move { worker.run(&task_for_run).await });
    running_tasks.insert(abort_handle.id(), task);
}

/// Synthesize a failed [`WorkerResult`] for a task whose spawned worker
/// panicked (or was cancelled/aborted — tokio surfaces both as `JoinError`).
/// Keeps `run_parallel` total: every plan index resolves to a real
/// `WorkerResult`, never a missing slot.
fn worker_panic_result(task: &WorkerTask, join_err: &tokio::task::JoinError) -> WorkerResult {
    let output = if join_err.is_panic() {
        format!("worker panicked: {join_err}")
    } else {
        format!("worker task did not complete: {join_err}")
    };
    WorkerResult {
        task_id: task.task_id.clone(),
        agent: task.agent.clone(),
        success: false,
        output,
        input_tokens: 0,
        mop_fired: false,
        failure_kind: Some(FailureKind::WorkerPanic),
        agent_version: task.agent_version.clone(),
        rollback_step: None,
        outcome: WorkerOutcome::Complete,
    }
}

/// Stamp the conductor's shared stable cache key onto every sibling task.
///
/// When the plan was built with [`ConductorPlan::with_conductor_identity`]
/// (i.e. both `conductor_identity` and `conductor_day` are `Some`), this
/// helper produces a copy of the plan where every task carries the same
/// `Some(shared_key)` value on `conductor_stable_cache_key`. The key is
/// computed ONCE per plan from `(conductor_identity, conductor_day)` so
/// sibling workers share the cache slot instead of each independently
/// recomputing (cache-break).
///
/// For plans without conductor identity (single-worker / direct path),
/// tasks pass through unchanged with `conductor_stable_cache_key = None`
/// — the worker derives its own stable key via `SessionEngine`.
fn expand_plan_for_cache_sharing(plan: &ConductorPlan) -> ConductorPlan {
    let (identity, day) = match (&plan.conductor_identity, &plan.conductor_day) {
        (Some(id), Some(d)) => (id.clone(), d.clone()),
        _ => {
            return ConductorPlan {
                tasks: plan.tasks.clone(),
                parallel: plan.parallel,
                conductor_identity: None,
                conductor_day: None,
            };
        }
    };

    let shared_key = conductor_stable_cache_key(&identity, &day);
    let tasks = plan
        .tasks
        .iter()
        .map(|task| WorkerTask {
            conductor_stable_cache_key: Some(shared_key.clone()),
            ..task.clone()
        })
        .collect();

    ConductorPlan {
        tasks,
        parallel: plan.parallel,
        conductor_identity: Some(identity),
        conductor_day: Some(day),
    }
}

/// A test worker that succeeds and echoes the payload.
pub struct EchoWorker;

#[async_trait]
impl Worker for EchoWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        WorkerResult {
            task_id: task.task_id.clone(),
            agent: task.agent.clone(),
            success: true,
            output: task.payload.clone(),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: None,
            agent_version: task.agent_version.clone(),
            rollback_step: None,
            outcome: WorkerOutcome::Complete,
        }
    }
}

/// A test worker that always fails.
pub struct FailWorker;

#[async_trait]
impl Worker for FailWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        WorkerResult {
            task_id: task.task_id.clone(),
            agent: task.agent.clone(),
            success: false,
            output: format!("failed: {}", task.payload),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: Some(FailureKind::Unknown),
            agent_version: task.agent_version.clone(),
            rollback_step: None,
            outcome: WorkerOutcome::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AgentDescriptor, AgentRegistry};
    use crate::teams::section_context::SectionContext;

    fn task(id: &str) -> WorkerTask {
        WorkerTask {
            task_id: id.into(),
            agent: "agent".into(),
            payload: format!("payload-{id}"),
            agent_version: None,
            conductor_stable_cache_key: None,
        }
    }

    fn plan(ids: &[&str], parallel: bool) -> ConductorPlan {
        ConductorPlan {
            tasks: ids.iter().map(|id| task(id)).collect(),
            parallel,
            conductor_identity: None,
            conductor_day: None,
        }
    }

    #[tokio::test]
    async fn sequential_runs_all_in_order() {
        let c = Conductor::new(Arc::new(EchoWorker));
        let results = c.run_sequential(&plan(&["a", "b", "c"], false)).await;
        let ids: Vec<_> = results.iter().map(|r| r.task_id.clone()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert!(results.iter().all(|r| r.success));
    }

    #[tokio::test]
    async fn absent_section_context_preserves_worker_payload_byte_for_byte() {
        let conductor = Conductor::new(Arc::new(EchoWorker));
        let results = conductor.run_sequential(&plan(&["a"], false)).await;
        assert_eq!(results[0].output, "payload-a");
    }

    #[tokio::test]
    async fn shared_section_context_is_visible_to_each_peer() {
        let mut context = SectionContext::new();
        context
            .write(
                "team-brief",
                "peer-owner",
                "Coordinate through the shared brief.",
            )
            .unwrap();
        let conductor = Conductor::new(Arc::new(EchoWorker)).with_section_context(context);

        let results = conductor.execute(&plan(&["peer-a", "peer-b"], true)).await;
        assert_eq!(results.len(), 2);
        for result in results {
            assert!(result.output.contains("## Shared Team Context"));
            assert!(result
                .output
                .contains("Coordinate through the shared brief."));
            assert!(result.output.contains("payload-peer-"));
        }
    }

    #[tokio::test]
    async fn parallel_runs_all() {
        let c = Conductor::new(Arc::new(EchoWorker));
        let results = c.run_parallel(&plan(&["a", "b", "c", "d"], true)).await;
        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|r| r.success));
    }

    #[tokio::test]
    async fn parallel_surfaces_results_in_ready_order() {
        struct DelayedWorker;
        #[async_trait]
        impl Worker for DelayedWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                let delay = if task.task_id == "slow" { 30 } else { 1 };
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                EchoWorker.run(task).await
            }
        }
        let c = Conductor::new(Arc::new(DelayedWorker));
        let results = c
            .run_parallel(&plan(&["slow", "fast-a", "fast-b"], true))
            .await;
        let ids: Vec<_> = results.iter().map(|r| r.task_id.clone()).collect();
        assert_ne!(ids[0], "slow");
        assert_eq!(ids.last().map(String::as_str), Some("slow"));
    }

    #[tokio::test]
    async fn parallel_never_exceeds_configured_in_flight_cap() {
        struct TrackingWorker {
            active: Arc<std::sync::atomic::AtomicUsize>,
            peak: Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait]
        impl Worker for TrackingWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                use std::sync::atomic::Ordering;
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                EchoWorker.run(task).await
            }
        }
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker = TrackingWorker {
            active,
            peak: Arc::clone(&peak),
        };
        let c = Conductor::new(Arc::new(worker)).with_max_in_flight(2);
        let results = c
            .run_parallel(&plan(&["a", "b", "c", "d", "e"], true))
            .await;
        assert_eq!(results.len(), 5);
        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn execute_dispatches_parallel() {
        let c = Conductor::new(Arc::new(EchoWorker));
        let results = c.execute(&plan(&["a", "b"], true)).await;
        let ids: Vec<_> = results.iter().map(|r| r.task_id.clone()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn execute_dispatches_sequential() {
        let c = Conductor::new(Arc::new(EchoWorker));
        let results = c.execute(&plan(&["a", "b"], false)).await;
        let ids: Vec<_> = results.iter().map(|r| r.task_id.clone()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn synthesis_is_after_collection_and_before_final_dispatch() {
        struct PhaseWorker {
            calls: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        }

        #[async_trait]
        impl Worker for PhaseWorker {
            async fn run(&self, task: &WorkerTask) -> WorkerResult {
                self.calls
                    .lock()
                    .unwrap()
                    .push((task.task_id.clone(), task.payload.clone()));
                EchoWorker.run(task).await
            }
        }

        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let conductor = Conductor::new(Arc::new(PhaseWorker {
            calls: Arc::clone(&calls),
        }));
        let final_task = WorkerTask {
            task_id: "final".into(),
            agent: "synthesizer".into(),
            payload: "original request".into(),
            agent_version: None,
            conductor_stable_cache_key: None,
        };

        let (collected, final_result) = conductor
            .execute_with_synthesis(
                &plan(&["worker-a", "worker-b"], true),
                final_task,
                256 * 1024,
            )
            .await
            .expect("bounded synthesis");

        assert_eq!(collected.len(), 2);
        assert!(final_result.output.contains("payload-worker-a"));
        assert!(final_result.output.contains("payload-worker-b"));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls[..2]
            .iter()
            .all(|(task_id, _)| task_id.starts_with("worker-")));
        assert_eq!(calls[2].0, "final");
        assert!(calls[2].1.contains("payload-worker-a"));
        assert!(calls[2].1.contains("payload-worker-b"));
    }

    #[test]
    fn synthesis_merge_is_order_independent_and_deduplicates_outputs() {
        let a = EchoWorkerResult {
            task_id: "worker-a".into(),
            agent: "A".into(),
            output: "same   finding".into(),
        };
        let b = EchoWorkerResult {
            task_id: "worker-b".into(),
            agent: "B".into(),
            output: "same finding".into(),
        };
        let c = EchoWorkerResult {
            task_id: "worker-c".into(),
            agent: "C".into(),
            output: "different finding".into(),
        };
        let first = synthesize_worker_outputs(
            "request",
            &[a.clone().result(), b.clone().result(), c.clone().result()],
            256 * 1024,
        )
        .expect("bounded synthesis");
        let second =
            synthesize_worker_outputs("request", &[c.result(), a.result(), b.result()], 256 * 1024)
                .expect("bounded synthesis");

        assert_eq!(first, second);
        assert_eq!(first.matches("same   finding").count(), 1);
        assert!(first.contains("reported by workers A, B"));
        assert_eq!(first.matches("different finding").count(), 1);
    }

    #[test]
    fn single_worker_synthesis_keeps_existing_payload_shape() {
        let result = EchoWorkerResult {
            task_id: "worker-a".into(),
            agent: "A".into(),
            output: "draft output".into(),
        }
        .result();

        assert_eq!(
            synthesize_worker_outputs("request", &[result], 256 * 1024)
                .expect("bounded synthesis"),
            "request\n\nYou are the final routed-orchestration synthesizer. Review the independent drafts below, resolve conflicts, and return only the final answer required by the original task. Do not mention this evaluation or the drafts.\n\n--- draft 1 (A) ---\ndraft output"
        );
    }

    #[test]
    fn synthesis_rejects_payload_over_bound() {
        let result = EchoWorkerResult {
            task_id: "worker-a".into(),
            agent: "A".into(),
            output: "draft output".into(),
        }
        .result();

        assert!(synthesize_worker_outputs("request", &[result], 8).is_err());
    }

    #[derive(Clone)]
    struct EchoWorkerResult {
        task_id: String,
        agent: String,
        output: String,
    }

    impl EchoWorkerResult {
        fn result(self) -> WorkerResult {
            WorkerResult {
                task_id: self.task_id,
                agent: self.agent,
                success: true,
                output: self.output,
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: None,
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            }
        }
    }

    #[tokio::test]
    async fn echo_worker_success() {
        let c = Conductor::new(Arc::new(EchoWorker));
        let results = c.execute(&plan(&["a"], false)).await;
        assert!(results[0].success);
        assert_eq!(results[0].output, "payload-a");
    }

    #[tokio::test]
    async fn fail_worker_failure() {
        let c = Conductor::new(Arc::new(FailWorker));
        let results = c.execute(&plan(&["a"], true)).await;
        assert!(!results[0].success);
    }

    #[tokio::test]
    async fn empty_plan_empty_results() {
        let c = Conductor::new(Arc::new(EchoWorker));
        assert!(c.run_parallel(&plan(&[], true)).await.is_empty());
        assert!(c.run_sequential(&plan(&[], false)).await.is_empty());
        assert!(c.execute(&plan(&[], true)).await.is_empty());
    }

    #[test]
    fn conductor_spawn_selects_agent_at_spawn_time_and_returns_audit_artifact() {
        let mut registry = AgentRegistry::new();
        let mut coder = AgentDescriptor::new("coder", "coder", "fast/code");
        coder.capabilities = vec!["coding".into()];
        registry.register(coder);
        let mut security =
            AgentDescriptor::new("security-engineer", "security-engineer", "deep/sec");
        security.capabilities = vec!["security".into(), "threat-modeling".into()];
        registry.register(security);

        let conductor = Conductor::new(Arc::new(EchoWorker));
        let spawn = conductor
            .spawn_with_capabilities(
                &registry,
                SpawnRequest {
                    task_id: "spawn-security".into(),
                    payload: "review auth".into(),
                    required_capabilities: vec!["security".into()],
                    conductor_stable_cache_key: None,
                    requested_provider_id: None,
                    requested_model_id: None,
                    requested_tier: None,
                    read_only: false,
                    include_project_instructions_override: None,
                },
            )
            .expect("security capability should spawn the matching descriptor");

        assert_eq!(spawn.agent, "security-engineer");
        assert_eq!(spawn.model_id, "deep/sec");
        assert_eq!(
            spawn.transcript_artifact.agent_descriptor_name,
            "security-engineer"
        );
        assert_eq!(spawn.transcript_artifact.model_id, "deep/sec");
    }

    // ----- Cross-worker prefix-cache sharing (ROADMAP P1) ---------------

    /// Records every task the worker observes so tests can inspect what
    /// the conductor stamped on each sibling.
    struct RecordingWorker {
        observed: std::sync::Arc<std::sync::Mutex<Vec<WorkerTask>>>,
    }

    #[async_trait]
    impl Worker for RecordingWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            self.observed.lock().unwrap().push(task.clone());
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: String::new(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            }
        }
    }

    fn recording_worker() -> (Arc<RecordingWorker>, Arc<std::sync::Mutex<Vec<WorkerTask>>>) {
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker = Arc::new(RecordingWorker {
            observed: observed.clone(),
        });
        (worker, observed)
    }

    fn plan_with_conductor(
        ids: &[&str],
        parallel: bool,
        identity: &str,
        day: &str,
    ) -> ConductorPlan {
        ConductorPlan::with_conductor_identity(
            ids.iter().map(|id| task(id)).collect(),
            parallel,
            identity,
            day,
        )
    }

    #[tokio::test]
    async fn sibling_workers_under_one_conductor_share_stable_cache_key() {
        // Two sibling workers spawned under one conductor run with the
        // same identity + day MUST observe the SAME stable cache key on
        // their incoming `WorkerTask`. This is the structural guarantee
        // behind cross-worker prefix-cache sharing (ROADMAP P1): sibling
        // workers cache-read against each other, not cache-break.
        let (worker, observed) = recording_worker();
        let conductor = Conductor::new(worker);

        let plan = plan_with_conductor(
            &["alpha", "beta"],
            true,
            "rightkit-agent-conductor-v1",
            "2026-06-30",
        );
        let _ = conductor.execute(&plan).await;

        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        let key_alpha = observed[0]
            .conductor_stable_cache_key
            .as_ref()
            .expect("sibling worker must carry an inherited stable cache key");
        let key_beta = observed[1]
            .conductor_stable_cache_key
            .as_ref()
            .expect("sibling worker must carry an inherited stable cache key");
        assert_eq!(
            key_alpha, key_beta,
            "siblings under one conductor run must share the stable cache key (cache-read, not cache-break)"
        );
        assert!(
            key_alpha.starts_with(crate::conductor_cache_key::CONDUCTOR_CACHE_KEY_PREFIX),
            "shared key must use the conductor namespace prefix"
        );
    }

    #[tokio::test]
    async fn different_conductor_identity_yields_different_shared_key() {
        // Same day, two distinct conductor identities → the two plans
        // produce different shared keys. Worker A and worker B under
        // conductor `v1` share key K1; worker C under conductor `v2`
        // gets a different key K2.
        let (worker_a, observed_a) = recording_worker();
        let (worker_b, observed_b) = recording_worker();

        let conductor_a = Conductor::new(worker_a);
        let conductor_b = Conductor::new(worker_b);

        let _ = conductor_a
            .execute(&plan_with_conductor(
                &["x"],
                false,
                "rightkit-agent-conductor-v1",
                "2026-06-30",
            ))
            .await;
        let _ = conductor_b
            .execute(&plan_with_conductor(
                &["y"],
                false,
                "rightkit-agent-conductor-v2",
                "2026-06-30",
            ))
            .await;

        let key_v1 = observed_a.lock().unwrap()[0]
            .conductor_stable_cache_key
            .clone()
            .unwrap();
        let key_v2 = observed_b.lock().unwrap()[0]
            .conductor_stable_cache_key
            .clone()
            .unwrap();

        assert_ne!(
            key_v1, key_v2,
            "different conductor identity must produce a different shared key (cache break)"
        );
    }

    #[tokio::test]
    async fn different_day_yields_different_shared_key() {
        // Same identity, two distinct days → different shared keys
        // (mirrors the SessionEngine day-boundary rule).
        let (worker_a, observed_a) = recording_worker();
        let (worker_b, observed_b) = recording_worker();

        let _ = Conductor::new(worker_a)
            .execute(&plan_with_conductor(
                &["m"],
                false,
                "rightkit-agent-conductor-v1",
                "2026-06-30",
            ))
            .await;
        let _ = Conductor::new(worker_b)
            .execute(&plan_with_conductor(
                &["n"],
                false,
                "rightkit-agent-conductor-v1",
                "2026-07-01",
            ))
            .await;

        let key_d1 = observed_a.lock().unwrap()[0]
            .conductor_stable_cache_key
            .clone()
            .unwrap();
        let key_d2 = observed_b.lock().unwrap()[0]
            .conductor_stable_cache_key
            .clone()
            .unwrap();

        assert_ne!(
            key_d1, key_d2,
            "different UTC day must produce a different shared key"
        );
    }

    #[tokio::test]
    async fn handoff_brief_does_not_alter_shared_key_across_siblings() {
        // The handoff brief is the per-worker CONTEXT tier. Two siblings
        // under one conductor with different briefs (e.g. one tasked
        // with `auth`, the other with `payment`) MUST still receive the
        // SAME shared stable cache key — the brief cannot enter the
        // hash by construction.
        let (worker, observed) = recording_worker();
        let conductor = Conductor::new(worker);

        // Mutate the payload to mimic differing briefs after the plan
        // was built. The conductor must NOT consult the payload when
        // computing the shared key.
        let mut plan = plan_with_conductor(
            &["auth-task", "payment-task"],
            true,
            "rightkit-agent-conductor-v1",
            "2026-06-30",
        );
        plan.tasks[0].payload = "## Handoff Brief\n\n### Task\nImplement OAuth flow".into();
        plan.tasks[1].payload = "## Handoff Brief\n\n### Task\nImplement PCI capture".into();

        let _ = conductor.execute(&plan).await;
        let observed = observed.lock().unwrap();

        let key_a = observed[0].conductor_stable_cache_key.as_ref().unwrap();
        let key_b = observed[1].conductor_stable_cache_key.as_ref().unwrap();
        assert_eq!(
            key_a, key_b,
            "different briefs MUST NOT break the shared stable cache key"
        );
    }

    #[tokio::test]
    async fn single_worker_plan_has_no_inherited_conductor_key() {
        // A plan built with `ConductorPlan::new` (no conductor identity)
        // is the single-worker / direct path. Tasks must pass through
        // with `conductor_stable_cache_key = None` so the worker
        // derives its own stable key via `SessionEngine`.
        let (worker, observed) = recording_worker();
        let conductor = Conductor::new(worker);

        let _ = conductor.execute(&plan(&["only"], false)).await;

        let observed = observed.lock().unwrap();
        assert_eq!(
            observed[0].conductor_stable_cache_key, None,
            "single-worker (direct) path must NOT carry an inherited conductor key"
        );
    }

    #[test]
    fn conductor_plan_shares_conductor_cache_key_predicate() {
        let single = ConductorPlan::new(vec![task("a")], false);
        assert!(
            !single.shares_conductor_cache_key(),
            "plan built with `new` does not share a conductor cache key"
        );

        let missing_day = ConductorPlan {
            tasks: vec![task("a")],
            parallel: false,
            conductor_identity: Some("rightkit-agent-conductor-v1".into()),
            conductor_day: None,
        };
        assert!(
            !missing_day.shares_conductor_cache_key(),
            "missing day → no shared key"
        );

        let shared = ConductorPlan::with_conductor_identity(
            vec![task("a"), task("b")],
            true,
            "rightkit-agent-conductor-v1",
            "2026-06-30",
        );
        assert!(
            shared.shares_conductor_cache_key(),
            "plan built with `with_conductor_identity` shares a conductor cache key"
        );
    }
}

#[cfg(test)]
#[path = "execution_worker_panic_tests.rs"]
mod execution_worker_panic_tests;
