//! The conductor's execution engine — runs worker tasks sequentially or in parallel.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;

use super::types::{ConductorPlan, WorkerCollection, WorkerOutcome, WorkerResult, WorkerTask};
use crate::coding::conductor_cache_key::conductor_stable_cache_key;
use crate::coding::failure::FailureKind;
use crate::coding::registry::AgentRegistry;
use crate::coding::spawn::{spawn_with_capabilities, SpawnPlan, SpawnRequest};
use crate::coding::teams::section_context::SectionContext;

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
