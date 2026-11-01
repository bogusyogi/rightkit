//! Router proof harness — held-out, contamination-resistant scorer for any
//! `fn(task) -> ModelTier` router (router proof harness, formerly ROADMAP P2 "Router proof harness — held-out,
//! not SWE-bench").
//!
//! Lane R1 (the full capability/cost-model router) is not built yet. This
//! harness targets the **router seam as an interface** ([`RouterUnderTest`])
//! so it can score whatever router lands there later, plus the routers that
//! already exist today:
//!
//! - [`DispatchRouterAdapter`] wraps the SHIPPED heuristic complexity-dispatch
//!   router ([`crate::dispatch::DispatchRouter`], `03a9c7ac`) by mapping its
//!   `Direct`/`Conduct` strategy onto a cheap/capable model tier.
//! - [`AlwaysCapableRouter`] is the baseline every router is compared against:
//!   it always picks the capable tier, so its cost delta is zero by
//!   definition and its accuracy is bounded by how often the label truly
//!   calls for the capable tier.
//! - [`AlwaysCheapRouter`] and [`InvertedRouter`] are deliberately-wrong
//!   routers used as a sanity check: a router that ignores task signals (or
//!   actively inverts the correct call) must score worse than the routers
//!   above on this corpus, or the scorer itself is broken.
//!
//! The corpus ([`held_out_corpus`]) is hand-authored from the host app's own
//! task shapes (mirroring the representative, non-exhaustive approach already
//! used by [`crate::terminal_bench::TbenchCatalog`]), NOT sampled from
//! SWE-bench or any public benchmark — avoiding the contamination risk where
//! a router (or the model behind it) was trained on the eval set itself. See
//! `docs/canon/daemon-master-canon-atoms.md` Lane H2.

use rightkit_llm::agent_providers::pricing::{
    estimate_cost, TASK_COMPLETION_TOKENS, TASK_PROMPT_TOKENS,
};
use serde::{Deserialize, Serialize};

use crate::dispatch::DispatchRouter;

/// Coarse capability/cost tier a router assigns to a task. Deliberately
/// binary (not a full model roster) because the harness scores the ROUTING
/// DECISION, not a specific model's pricing — `tier_model_slug` maps each
/// tier to one representative priced model from `host_providers` so the
/// scorer can report real dollar deltas instead of an invented cost unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTier {
    /// Small/cheap model — appropriate for narrow, single-file, low-ambiguity
    /// tasks.
    Cheap,
    /// Large/capable model (or multi-agent decomposition) — appropriate for
    /// broad-scope, multi-step, or high-ambiguity tasks.
    Capable,
}

impl ModelTier {
    /// Representative priced model slug for this tier, from the curated
    /// table in `rightkit_llm::agent_providers::pricing`. Chosen for a wide, realistic
    /// price gap so cost-delta numbers are meaningful, not for being any
    /// particular provider's current flagship.
    pub fn model_slug(self) -> &'static str {
        match self {
            ModelTier::Cheap => "deepseek/deepseek-v4-flash",
            ModelTier::Capable => "qwen/qwen3.7-max",
        }
    }

    /// Projected USD cost of one task on this tier, using the engine's
    /// standard task token footprint (`TASK_PROMPT_TOKENS` /
    /// `TASK_COMPLETION_TOKENS`) so the number lines up with cost preflight
    /// elsewhere in the engine.
    pub fn projected_task_cost(self) -> f64 {
        estimate_cost(
            self.model_slug(),
            TASK_PROMPT_TOKENS,
            TASK_COMPLETION_TOKENS,
        )
    }
}

/// One held-out, labeled task in the proof corpus.
#[derive(Debug, Clone, Serialize)]
pub struct LabeledTask {
    /// Stable identifier, e.g. "rp-001".
    pub id: &'static str,
    /// The task text a router would receive (same shape `DispatchRouter`
    /// consumes: a free-text user request).
    pub prompt: &'static str,
    /// Ground-truth tier a well-calibrated router should pick for this task.
    /// Hand-labeled by task shape (single-file/narrow => Cheap, broad-scope
    /// or multi-step/decomposition-shaped => Capable) — the same criteria
    /// `DispatchRouter`'s heuristic already encodes, but labeled independently
    /// here so the corpus can catch drift or regressions in that heuristic,
    /// not just restate it.
    pub ideal_tier: ModelTier,
    /// One-line justification for the label, for auditability.
    pub rationale: &'static str,
}

/// Held-out proof corpus. Hand-authored, NOT derived from SWE-bench or any
/// public benchmark — see module docs for why that matters. Deliberately
/// small (representative, not exhaustive) and versioned in source so any
/// future edit is a reviewable diff, matching `TbenchCatalog::all_tasks`'s
/// approach in this crate.
pub fn held_out_corpus() -> Vec<LabeledTask> {
    vec![
        LabeledTask {
            id: "rp-001",
            prompt: "What does the `estimate_cost` function in pricing.rs do?",
            ideal_tier: ModelTier::Cheap,
            rationale: "Short explanatory question about one file; no editing or planning.",
        },
        LabeledTask {
            id: "rp-002",
            prompt: "Fix the typo in README.md",
            ideal_tier: ModelTier::Cheap,
            rationale: "Trivial single-file edit, no ambiguity.",
        },
        LabeledTask {
            id: "rp-003",
            prompt: "Rename the `foo` variable to `bar` in utils.rs",
            ideal_tier: ModelTier::Cheap,
            rationale: "Small local edit signal, single file, low word count.",
        },
        LabeledTask {
            id: "rp-004",
            prompt: "Why is this test flaky?",
            ideal_tier: ModelTier::Cheap,
            rationale: "Short diagnostic question, no repo-wide scope stated.",
        },
        LabeledTask {
            id: "rp-005",
            prompt: "Add a doc comment above the `run` function in main.rs",
            ideal_tier: ModelTier::Cheap,
            rationale: "Single-file scoped request under the word-count threshold.",
        },
        LabeledTask {
            id: "rp-006",
            prompt: "How do I run the test suite for this crate?",
            ideal_tier: ModelTier::Cheap,
            rationale: "Short explanatory question; answer needs no code changes.",
        },
        LabeledTask {
            id: "rp-007",
            prompt: "Change the default port from 8080 to 9090",
            ideal_tier: ModelTier::Cheap,
            rationale: "Small, well-scoped local edit signal.",
        },
        LabeledTask {
            id: "rp-008",
            prompt: "Refactor error handling across the entire codebase to use a shared error type, update all call sites, and add tests for every handler",
            ideal_tier: ModelTier::Capable,
            rationale: "Repo-wide refactor plus multi-step follow-on work (update, add tests) — the shape `DispatchRouter` scores highest for decomposition.",
        },
        LabeledTask {
            id: "rp-009",
            prompt: "Migrate the entire storage layer from SQLite to Postgres: design the schema, write the migration, update every usage, and verify with an end-to-end test",
            ideal_tier: ModelTier::Capable,
            rationale: "Explicit migration + end-to-end + broad scope + multi-step; needs decomposition across many files.",
        },
        LabeledTask {
            id: "rp-010",
            prompt: "Audit and fix all usages of the deprecated config loader across the project, then update the docs and add regression tests",
            ideal_tier: ModelTier::Capable,
            rationale: "Explicit 'audit and fix' plus broad scope ('across the project') plus multi-step chain.",
        },
        LabeledTask {
            id: "rp-011",
            prompt: "Build a new feature end-to-end: a task graph conductor that coordinates three worker agents, wire it into the daemon, add API routes, and update the frontend to show progress",
            ideal_tier: ModelTier::Capable,
            rationale: "Explicit decomposition vocabulary ('end-to-end', 'task graph', 'conductor') plus 3+ file paths implied plus multi-step.",
        },
        LabeledTask {
            id: "rp-012",
            prompt: "Every handler in api.rs, api_sessions.rs, and api_execution.rs needs the new auth middleware; wire it in, update the tests in each file, and confirm the daemon still boots",
            ideal_tier: ModelTier::Capable,
            rationale: "3+ explicit file paths, 'every handler' broad-scope phrase, multi-step chain (wire, update, confirm).",
        },
        LabeledTask {
            id: "rp-013",
            prompt: "There's a bug somewhere in the dispatch heuristic — investigate, find the root cause across the module, and fix it with tests",
            ideal_tier: ModelTier::Capable,
            rationale: "Open-ended investigation with no single named file, multi-step (investigate, find, fix, test) and broad implied scope.",
        },
        LabeledTask {
            id: "rp-014",
            prompt: "Implement a full plugin system: a manifest schema, a loader, a permission gate, and a sample plugin, then add docs",
            ideal_tier: ModelTier::Capable,
            rationale: "'Implement' + 'feature'-shaped multi-step chain across several new files.",
        },
        LabeledTask {
            id: "rp-015",
            prompt: "What's the difference between the `Direct` and `Conduct` execution strategies?",
            ideal_tier: ModelTier::Cheap,
            rationale: "Short explanatory question, no broad scope, no editing.",
        },
    ]
}

/// The interface every router — present or future — is scored against. Lane
/// R1's eventual capability/cost-model router only needs to implement this
/// trait to be pluggable into this harness; the trait signature intentionally
/// mirrors `fn(task) -> model tier` from the Lane H2 spec, made object-safe
/// and testable via an inherent method rather than a bare `fn` so adapters
/// can carry state (e.g. wrapping `DispatchRouter`, which is itself async).
pub trait RouterUnderTest {
    /// Human-readable name for reporting.
    fn name(&self) -> &'static str;
    /// Decide a tier for one task prompt.
    fn route(&self, task: &str) -> ModelTier;
}

/// Adapter around the SHIPPED heuristic complexity-dispatch router
/// (`DispatchRouter`, `03a9c7ac`). Maps its `Direct` strategy to `Cheap` and
/// `Conduct { .. }` to `Capable` — `Direct` runs a single agent turn (the
/// cheaper path), `Conduct` hands the task to the multi-agent conductor (the
/// more expensive, more capable path). Only the heuristic tier is exercised
/// here (no classifier fallback) so the harness stays deterministic and
/// offline.
pub struct DispatchRouterAdapter {
    inner: DispatchRouter,
}

impl DispatchRouterAdapter {
    pub fn new() -> Self {
        Self {
            inner: DispatchRouter::new(),
        }
    }
}

impl Default for DispatchRouterAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RouterUnderTest for DispatchRouterAdapter {
    fn name(&self) -> &'static str {
        "dispatch-router-heuristic (03a9c7ac)"
    }

    fn route(&self, task: &str) -> ModelTier {
        // `DispatchRouter::decide` is async only because it can fall back to
        // an LLM classifier; with no classifier supplied it resolves
        // synchronously off the heuristic branch, so a tiny throwaway
        // current-thread runtime is safe and keeps this trait sync.
        let decision = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(self.inner.decide(task, None));
        match decision.strategy {
            crate::dispatch::ExecutionStrategy::Direct => ModelTier::Cheap,
            crate::dispatch::ExecutionStrategy::Conduct { .. } => ModelTier::Capable,
        }
    }
}

/// Baseline: always route to the capable tier. This is the "always-capable"
/// comparison point the Lane H2 spec asks for — its cost delta against
/// itself is zero, and its accuracy equals the fraction of the corpus whose
/// ideal tier is `Capable`.
pub struct AlwaysCapableRouter;

impl RouterUnderTest for AlwaysCapableRouter {
    fn name(&self) -> &'static str {
        "always-capable (baseline)"
    }

    fn route(&self, _task: &str) -> ModelTier {
        ModelTier::Capable
    }
}

/// Deliberately-wrong router #1: always routes to the cheap tier regardless
/// of task shape. Used as a sanity check — a router with zero signal should
/// score strictly worse than `DispatchRouterAdapter` on this corpus.
pub struct AlwaysCheapRouter;

impl RouterUnderTest for AlwaysCheapRouter {
    fn name(&self) -> &'static str {
        "always-cheap (deliberately wrong)"
    }

    fn route(&self, _task: &str) -> ModelTier {
        ModelTier::Cheap
    }
}

/// Deliberately-wrong router #2: inverts the shipped heuristic's decision.
/// Stronger sanity check than `AlwaysCheapRouter` because it has the SAME
/// information as `DispatchRouterAdapter` and actively uses it backwards —
/// if this ever scored as well as or better than the real router, the
/// scorer itself would be broken.
pub struct InvertedRouter {
    inner: DispatchRouterAdapter,
}

impl InvertedRouter {
    pub fn new() -> Self {
        Self {
            inner: DispatchRouterAdapter::new(),
        }
    }
}

impl Default for InvertedRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl RouterUnderTest for InvertedRouter {
    fn name(&self) -> &'static str {
        "inverted-dispatch (deliberately wrong)"
    }

    fn route(&self, task: &str) -> ModelTier {
        match self.inner.route(task) {
            ModelTier::Cheap => ModelTier::Capable,
            ModelTier::Capable => ModelTier::Cheap,
        }
    }
}

/// Per-task scoring detail, kept for audit/debugging of a scorer run.
#[derive(Debug, Clone, Serialize)]
pub struct TaskScore {
    pub task_id: &'static str,
    pub ideal_tier: ModelTier,
    pub routed_tier: ModelTier,
    pub correct: bool,
}

/// Aggregate report for one router scored against the held-out corpus.
#[derive(Debug, Clone, Serialize)]
pub struct ProofReport {
    pub router_name: String,
    pub corpus_size: usize,
    /// Fraction of tasks where `routed_tier == ideal_tier`, in [0.0, 1.0].
    pub accuracy: f64,
    /// Total projected USD cost across the corpus using the router's actual
    /// picks.
    pub total_cost_usd: f64,
    /// Total projected USD cost across the corpus if every task had run on
    /// the capable tier (the "always capable" cost floor for comparison).
    pub always_capable_cost_usd: f64,
    /// `always_capable_cost_usd - total_cost_usd`. Positive means the router
    /// saved money vs always using the capable model; negative means it cost
    /// MORE (e.g. a router that over-routes to Capable, or picks it for
    /// tasks that didn't need it while adding overhead — not possible with
    /// only two tiers here, but the sign is kept general for when Lane R1
    /// adds more tiers).
    pub cost_delta_usd: f64,
    /// Average projected prompt+completion tokens per task under the
    /// router's picks (using the engine's standard per-task token
    /// footprint), vs the same figure for always-capable. With the current
    /// two-tier model this is a fixed number per tier, but reporting it
    /// explicitly is what the Lane H2 spec asks for and it becomes
    /// meaningful once tiers carry different token footprints, not just
    /// different prices.
    pub avg_tokens_per_task: f64,
    pub always_capable_avg_tokens_per_task: f64,
    pub per_task: Vec<TaskScore>,
}

fn tier_tokens(_tier: ModelTier) -> f64 {
    (TASK_PROMPT_TOKENS + TASK_COMPLETION_TOKENS) as f64
}

/// Run `router` over `corpus` and produce a [`ProofReport`]. This is the
/// harness's single entry point — both the CLI-style end-to-end test and any
/// future automation call this same function so the reported numbers are
/// always computed one way.
pub fn score_router(router: &dyn RouterUnderTest, corpus: &[LabeledTask]) -> ProofReport {
    let mut per_task = Vec::with_capacity(corpus.len());
    let mut correct = 0usize;
    let mut total_cost = 0.0f64;
    let mut always_capable_cost = 0.0f64;
    let mut total_tokens = 0.0f64;
    let mut always_capable_tokens = 0.0f64;

    for task in corpus {
        let routed_tier = router.route(task.prompt);
        let is_correct = routed_tier == task.ideal_tier;
        if is_correct {
            correct += 1;
        }
        total_cost += routed_tier.projected_task_cost();
        always_capable_cost += ModelTier::Capable.projected_task_cost();
        total_tokens += tier_tokens(routed_tier);
        always_capable_tokens += tier_tokens(ModelTier::Capable);

        per_task.push(TaskScore {
            task_id: task.id,
            ideal_tier: task.ideal_tier,
            routed_tier,
            correct: is_correct,
        });
    }

    let corpus_size = corpus.len();
    let accuracy = if corpus_size == 0 {
        0.0
    } else {
        correct as f64 / corpus_size as f64
    };

    ProofReport {
        router_name: router.name().to_string(),
        corpus_size,
        accuracy,
        total_cost_usd: total_cost,
        always_capable_cost_usd: always_capable_cost,
        cost_delta_usd: always_capable_cost - total_cost,
        avg_tokens_per_task: if corpus_size == 0 {
            0.0
        } else {
            total_tokens / corpus_size as f64
        },
        always_capable_avg_tokens_per_task: if corpus_size == 0 {
            0.0
        } else {
            always_capable_tokens / corpus_size as f64
        },
        per_task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_is_nonempty_and_labeled() {
        let corpus = held_out_corpus();
        assert!(corpus.len() >= 10, "corpus should have a real sample size");
        // Corpus must contain both labels — a corpus that's all one tier
        // can't distinguish a real router from `AlwaysCapableRouter`.
        assert!(corpus.iter().any(|t| t.ideal_tier == ModelTier::Cheap));
        assert!(corpus.iter().any(|t| t.ideal_tier == ModelTier::Capable));
    }

    #[test]
    fn dispatch_router_scores_reasonably_on_held_out_corpus() {
        let corpus = held_out_corpus();
        let report = score_router(&DispatchRouterAdapter::new(), &corpus);
        assert_eq!(report.corpus_size, corpus.len());
        // The corpus was labeled using the same shape signals the heuristic
        // uses, so it should do meaningfully better than chance (0.5).
        assert!(
            report.accuracy >= 0.7,
            "expected the shipped heuristic to score >=0.7 accuracy, got {}",
            report.accuracy
        );
    }

    #[test]
    fn always_capable_baseline_has_zero_cost_delta() {
        let corpus = held_out_corpus();
        let report = score_router(&AlwaysCapableRouter, &corpus);
        assert!(
            report.cost_delta_usd.abs() < 1e-9,
            "always-capable must cost exactly the always-capable floor, got delta {}",
            report.cost_delta_usd
        );
    }

    #[test]
    fn a_real_router_saves_cost_vs_always_capable() {
        let corpus = held_out_corpus();
        let report = score_router(&DispatchRouterAdapter::new(), &corpus);
        assert!(
            report.cost_delta_usd > 0.0,
            "expected the heuristic router to save money vs always-capable by routing \
             some tasks to the cheap tier, got delta {}",
            report.cost_delta_usd
        );
        assert!(report.avg_tokens_per_task <= report.always_capable_avg_tokens_per_task);
    }

    #[test]
    fn deliberately_wrong_routers_score_worse_than_the_real_router() {
        let corpus = held_out_corpus();
        let real = score_router(&DispatchRouterAdapter::new(), &corpus);
        let always_cheap = score_router(&AlwaysCheapRouter, &corpus);
        let inverted = score_router(&InvertedRouter::new(), &corpus);

        assert!(
            always_cheap.accuracy < real.accuracy,
            "always-cheap ({}) should score worse than the real router ({})",
            always_cheap.accuracy,
            real.accuracy
        );
        assert!(
            inverted.accuracy < real.accuracy,
            "inverted router ({}) should score worse than the real router ({})",
            inverted.accuracy,
            real.accuracy
        );
        // The inverted router has the exact same information as the real
        // router and actively misuses it — it should be at or below chance,
        // which is the strongest form of this sanity check.
        assert!(
            inverted.accuracy <= 0.5,
            "inverted router should be at or below chance, got {}",
            inverted.accuracy
        );
    }

    #[test]
    fn score_router_handles_empty_corpus_without_panicking() {
        let report = score_router(&AlwaysCapableRouter, &[]);
        assert_eq!(report.corpus_size, 0);
        assert_eq!(report.accuracy, 0.0);
        assert_eq!(report.cost_delta_usd, 0.0);
    }
}
