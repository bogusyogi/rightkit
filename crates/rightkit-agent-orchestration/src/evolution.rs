//! P3 evolution suite — runtime wiring for the failure-driven improvement loop.
//!
//! Connects the scaffolds already present in this crate:
//! - `HarnessTracer`: writes per-task JSON-line traces to the filesystem for the
//!   proposer pipeline to analyze.
//! - `DurationAwareRouter`: wraps `AgentRouter`, factoring in the agent's historical
//!   duration-bucket reliability when multiple candidates are available.
//! - `EvolutionHarness`: glues tracing, prompt-version tagging, and the ratchet
//!   trigger together for use in `run_graph`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::conductor::WorkerResult;
use crate::prompt_version::PromptRegistry;
use crate::registry::{AgentDescriptor, AgentRegistry};
use crate::router::AgentRouter;
pub use crate::taste::DurationBucket;
use crate::taste::{RunRecord, TasteScore};
use rightkit_agent_engine_protocol::TaskNode;

// ─── HarnessTracer ──────────────────────────────────────────────────────────

/// Appends JSON-line trace entries for every task result during `run_graph`.
///
/// Files are written to `{traces_dir}/{session_id}.jsonl`. Each line records the
/// task, agent, success flag, output length, and prompt version in use.
pub struct HarnessTracer {
    traces_dir: PathBuf,
    session_id: String,
    buf: Vec<serde_json::Value>,
    flush_every: usize,
}

impl HarnessTracer {
    /// Create a new tracer that flushes every `flush_every` entries.
    pub fn new(traces_dir: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        Self {
            traces_dir: traces_dir.into(),
            session_id: session_id.into(),
            buf: Vec::new(),
            flush_every: 10,
        }
    }

    /// With a flush-every override (useful in tests).
    pub fn with_flush_every(mut self, n: usize) -> Self {
        self.flush_every = n;
        self
    }

    /// Record one task result.  Call after each `WorkerResult` in `run_graph`.
    pub fn record(
        &mut self,
        result: &WorkerResult,
        prompt_version: Option<&str>,
        turn_millis: u64,
    ) {
        let entry = serde_json::json!({
            "task_id":       result.task_id,
            "agent":         result.agent,
            "success":       result.success,
            "output_len":    result.output.len(),
            "mop_fired":     result.mop_fired,
            "input_tokens":  result.input_tokens,
            "turn_ms":       turn_millis,
            "agent_version": result.agent_version,
            "prompt_v":      prompt_version,
            "ts":            chrono_now_iso(),
        });
        self.buf.push(entry);
        if self.buf.len() >= self.flush_every {
            self.flush_to_disk();
        }
    }

    /// Flush all buffered entries to disk. Called at `run_graph` completion.
    pub fn flush_to_disk(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&self.traces_dir);
        let path = self.traces_dir.join(format!("{}.jsonl", self.session_id));
        let mut lines = String::new();
        for entry in self.buf.drain(..) {
            if let Ok(s) = serde_json::to_string(&entry) {
                lines.push_str(&s);
                lines.push('\n');
            }
        }
        if !lines.is_empty() {
            let _ = rightkit_fs::append_record(
                &path,
                lines.as_bytes(),
                rightkit_fs::AppendOptions::default().durable(false),
            );
        }
    }
}

impl Drop for HarnessTracer {
    fn drop(&mut self) {
        self.flush_to_disk();
    }
}

fn chrono_now_iso() -> String {
    // Simple ISO timestamp without external deps.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

// ─── DurationAwareRouter ────────────────────────────────────────────────────

/// Wraps `AgentRouter`, augmenting role-based routing with duration-bucket
/// reliability: when two candidates are otherwise equal, prefer the agent whose
/// `TasteScore` shows higher reliability in the task's expected duration bucket.
///
/// Taste scores are supplied externally (from the daemon's taste-engine layer)
/// so this struct stays free of storage dependencies.
pub struct DurationAwareRouter {
    inner: AgentRouter,
    /// role → per-bucket reliability (Short/Medium/Long/VeryLong).
    taste: HashMap<String, TasteScore>,
}

impl Default for DurationAwareRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl DurationAwareRouter {
    pub fn new() -> Self {
        Self {
            inner: AgentRouter::new(),
            taste: HashMap::new(),
        }
    }

    /// Register a taste score for a role.  Called from the daemon on startup and
    /// after each promotion cycle.
    pub fn register_taste(&mut self, role: impl Into<String>, score: TasteScore) {
        self.taste.insert(role.into(), score);
    }

    /// Route `task`, preferring agents that have higher reliability in `bucket`.
    pub fn route<'a>(
        &self,
        registry: &'a AgentRegistry,
        task: &TaskNode,
        bucket: DurationBucket,
    ) -> Option<&'a AgentDescriptor> {
        // First try the standard role/capability match from the inner router.
        let primary = self.inner.route(registry, task)?;

        // If there are multiple agents with the same role, pick the one with the
        // best reliability in the target duration bucket.
        let candidates: Vec<&AgentDescriptor> = registry
            .all()
            .iter()
            .filter(|a| a.role == primary.role)
            .collect();

        if candidates.len() <= 1 {
            return Some(primary);
        }

        candidates.into_iter().max_by(|a, b| {
            let ra = self
                .taste
                .get(&a.role)
                .map(|s| bucket_reliability(s, bucket))
                .unwrap_or(0.5);
            let rb = self
                .taste
                .get(&b.role)
                .map(|s| bucket_reliability(s, bucket))
                .unwrap_or(0.5);
            ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

fn bucket_reliability(score: &TasteScore, bucket: DurationBucket) -> f64 {
    match bucket {
        DurationBucket::Short => score.reliability,
        DurationBucket::Medium => score.reliability,
        DurationBucket::Long => score.long_horizon_reliability,
        DurationBucket::VeryLong => score.long_horizon_reliability,
    }
}

// ─── EvolutionHarness ───────────────────────────────────────────────────────

/// Ties HarnessTracer + PromptRegistry prompt-version tagging together for use
/// during `run_graph`.
///
/// Usage:
/// ```ignore
/// let mut harness = EvolutionHarness::new("~/<state-dir>/traces", session_id);
/// harness.set_active_prompt_version("coder-system", 2);
/// // after each batch result:
/// harness.record_result(&worker_result, turn_ms);
/// // at end of run:
/// harness.finalize();
/// ```
pub struct EvolutionHarness {
    tracer: HarnessTracer,
    prompt_registry: PromptRegistry,
    /// role → currently active prompt version id (format: "<prompt_id>@v<N>")
    active_versions: HashMap<String, String>,
    /// Minimum GDS threshold to count a run as a "ratchet success" for the
    /// failure-driven ratchet.  Default: 0.90.
    ratchet_threshold: f64,
    /// How many failures before the harness recommends a ratchet step.
    ratchet_failures: usize,
    failure_count: usize,
}

impl EvolutionHarness {
    pub fn new(traces_dir: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        Self {
            tracer: HarnessTracer::new(traces_dir, &session_id),
            prompt_registry: PromptRegistry::new(),
            active_versions: HashMap::new(),
            ratchet_threshold: 0.90,
            ratchet_failures: 3,
            failure_count: 0,
        }
    }

    /// Override the ratchet threshold (GDS floor for a "good run").
    pub fn with_ratchet_threshold(mut self, t: f64) -> Self {
        self.ratchet_threshold = t;
        self
    }

    /// Set the active prompt version for a role.
    pub fn set_active_prompt_version(&mut self, prompt_id: &str, version: u32) {
        let key = format!("{prompt_id}@v{version}");
        self.active_versions.insert(prompt_id.to_string(), key);
    }

    /// Record a worker result, tagging it in the prompt registry and tracing it.
    pub fn record_result(&mut self, result: &WorkerResult, turn_ms: u64) {
        let pv = self.active_versions.get(&result.agent).cloned();
        let pv_str = pv.as_deref();
        // Tag the turn in the prompt registry.
        if let Some(pv) = pv_str {
            self.prompt_registry.tag_turn(&result.task_id, pv);
        }
        // Trace to disk.
        self.tracer.record(result, pv_str, turn_ms);
        // Track failures for the ratchet.
        if !result.success {
            self.failure_count += 1;
        }
    }

    /// Returns true when enough failures have accumulated to warrant a ratchet step.
    /// The caller (daemon-level orchestrator) should then invoke the proposer pipeline.
    pub fn should_ratchet(&self) -> bool {
        self.failure_count >= self.ratchet_failures
    }

    /// Reset the failure counter after a ratchet step has been dispatched.
    pub fn reset_ratchet(&mut self) {
        self.failure_count = 0;
    }

    /// Finalize: flush traces to disk.
    pub fn finalize(&mut self) {
        self.tracer.flush_to_disk();
    }

    /// Expose the prompt registry for callers that want to query tagged turns.
    pub fn prompt_registry(&self) -> &PromptRegistry {
        &self.prompt_registry
    }

    /// Access the active prompt versions map.
    pub fn active_versions(&self) -> &HashMap<String, String> {
        &self.active_versions
    }
}

// ─── Helpers for run_graph integration ──────────────────────────────────────

/// Estimate the `DurationBucket` from a task's `estimated_duration` string.
/// Accepted forms: "short", "medium", "long", "very-long" (case-insensitive),
/// or plain integers (minutes): 0–15 = Short, 16–60 = Medium, 61–240 = Long,
/// >240 = VeryLong.
pub fn bucket_from_estimate(estimate: &str) -> DurationBucket {
    let lower = estimate.trim().to_lowercase();
    match lower.as_str() {
        "short" | "s" => return DurationBucket::Short,
        "medium" | "m" | "med" => return DurationBucket::Medium,
        "long" | "l" => return DurationBucket::Long,
        "very-long" | "verylong" | "xl" | "very_long" => return DurationBucket::VeryLong,
        _ => {}
    }
    if let Ok(mins) = lower.parse::<u64>() {
        return match mins {
            0..=15 => DurationBucket::Short,
            16..=60 => DurationBucket::Medium,
            61..=240 => DurationBucket::Long,
            _ => DurationBucket::VeryLong,
        };
    }
    DurationBucket::Medium
}

/// Build a `RunRecord` from a `WorkerResult` for feeding into the taste engine.
/// `turn_ms` is the wall-clock duration of the run.
pub fn run_record_from_result(
    result: &WorkerResult,
    bucket: DurationBucket,
    _turn_ms: u64,
) -> RunRecord {
    let context_efficiency_proxy = if result.input_tokens > 0 {
        1.0 - (result.input_tokens as f64 / 200_000.0).min(1.0)
    } else {
        0.5
    };
    let _ = context_efficiency_proxy; // will be wired into TasteScore when available

    RunRecord {
        bucket,
        gds: if result.success { 1.0 } else { 0.0 },
        input_tokens: result.input_tokens,
        mop_fired: result.mop_fired,
        completed_without_escalation: result.success,
        quality: if result.success { 0.8 } else { 0.0 },
    }
}
