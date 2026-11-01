//! Memory retrieval routing — daemon integration for FM-071.
//!
//! Wires `crate::RoutingPolicy` (the three-tier bandit) into the
//! daemon's recall path. This module is the `memory.rs:342` call site cited
//! in the feature matrix: it chooses the retrieval tier, enforces the
//! lexical fallback when the embedder is unavailable, and feeds bandit
//! reward/penalty from run outcomes.
//!
//! Tiers: `Low` → lexical only, `Mid` → hybrid, `High` → full hybrid with
//! wider candidate set. The bandit learns via EMA; `fastembed` absence forces
//! `Low`.

use crate::{candidate_limit_for, use_embeddings, RoutingPolicy, Tier};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Shared routing state (DaemonState owns one Arc<Mutex<_>>)
// ---------------------------------------------------------------------------

/// Thread-safe handle to the routing policy. Clone-friendly for Axum state.
#[derive(Clone)]
pub struct MemoryRoutingState {
    inner: Arc<Mutex<RoutingPolicy>>,
}

impl Default for MemoryRoutingState {
    fn default() -> Self {
        Self::new(false)
    }
}

impl MemoryRoutingState {
    pub fn new(embedder_available: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RoutingPolicy::with_embedder_available(
                embedder_available,
            ))),
        }
    }

    pub fn with_params(embedder_available: bool, alpha: f64, epsilon: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(
                RoutingPolicy::with_embedder_available(embedder_available)
                    .with_params(alpha, epsilon),
            )),
        }
    }

    pub fn set_embedder_available(&self, available: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.set_embedder_available(available);
        }
    }

    pub fn is_embedder_available(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.embedder_available)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Public helpers — the contract-facing surface
// ---------------------------------------------------------------------------

/// Choose a retrieval tier for `query`. This is the seam at `memory.rs:342`
/// referenced by the feature matrix: callers provide the query text and an
/// optional embedder-availability override; the policy returns the chosen
/// tier with a hard lexical fallback when the embedder is unavailable.
pub fn choose_tier(
    state: &MemoryRoutingState,
    query: &str,
    embedder_available: Option<bool>,
) -> Tier {
    let mut g = state.inner.lock().expect("memory routing lock");
    // Line 342 region — the routing decision with bandit + fallback.
    g.choose(query, embedder_available)
}

/// Convenience: choose with an explicit bool (no `Option`).
pub fn choose_tier_with_flag(
    state: &MemoryRoutingState,
    query: &str,
    embedder_available: bool,
) -> Tier {
    choose_tier(state, query, Some(embedder_available))
}

/// Record a successful recall for `tier` — bandit reward.
pub fn record_success(state: &MemoryRoutingState, tier: Tier) {
    if let Ok(mut g) = state.inner.lock() {
        g.observe_success(tier);
    }
}

/// Record a failed / unused recall for `tier` — bandit penalty.
pub fn record_miss(state: &MemoryRoutingState, tier: Tier) {
    if let Ok(mut g) = state.inner.lock() {
        g.observe_miss(tier);
    }
}

/// Generic outcome recorder: `hit == true` rewards, `false` penalizes.
pub fn record_outcome(state: &MemoryRoutingState, tier: Tier, hit: bool) {
    if let Ok(mut g) = state.inner.lock() {
        g.observe_outcome(tier, hit);
    }
}

/// Candidate limit for a tier (wiring helper for recall sites).
pub fn limit_for(tier: Tier) -> usize {
    candidate_limit_for(tier)
}

/// Whether the chosen tier should consult embeddings given availability.
pub fn should_use_embeddings(tier: Tier, embedder_available: bool) -> bool {
    use_embeddings(tier, embedder_available)
}

/// Snapshot diagnostics for `/diagnostics/memory-routing` or metrics.
pub fn snapshot(state: &MemoryRoutingState) -> RoutingSnapshot {
    let g = state.inner.lock().expect("memory routing lock");
    let arms = g.snapshot();
    RoutingSnapshot {
        low: arms[0],
        mid: arms[1],
        high: arms[2],
        embedder_available: g.embedder_available,
        total_choices: g.total_choices,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingSnapshot {
    pub low: crate::BanditStats,
    pub mid: crate::BanditStats,
    pub high: crate::BanditStats,
    pub embedder_available: bool,
    pub total_choices: u64,
}

// ---------------------------------------------------------------------------
// Run-outcome feedback — called from the conductor / session completion path
// ---------------------------------------------------------------------------

/// Translate a run outcome into per-tier feedback.
/// `used_ratio` is the fraction of retrieved memories actually consumed by
/// the agent (from `MemoryRetrievalEvalGate`). Below 0.4 is a miss signal.
pub fn feedback_from_run(
    state: &MemoryRoutingState,
    tier: Tier,
    used_ratio: f64,
    recall_hit: bool,
) {
    let success = recall_hit && used_ratio >= 0.40;
    record_outcome(state, tier, success);
}

// ---------------------------------------------------------------------------
// Config wiring — apply `MemoryConfig` to a routing state
// ---------------------------------------------------------------------------

/// Apply a `rightkit_app_settings::agent_config::MemoryConfig` to an existing routing state.
/// Called once at daemon start and again on config reload.
pub fn apply_config(
    state: &MemoryRoutingState,
    cfg: &rightkit_app_settings::agent_config::MemoryConfig,
) {
    if let Ok(mut g) = state.inner.lock() {
        g.alpha = cfg.bandit_alpha().clamp(0.01, 0.9);
        g.epsilon = cfg.bandit_epsilon().clamp(0.0, 0.5);
        // When bandit is disabled and an explicit tier is set, bias that
        // tier's reward to 1.0 so the bandit deterministically prefers it.
        if !cfg.bandit_enabled() {
            if let Some(explicit) = cfg.explicit_tier() {
                for t in Tier::ALL {
                    let idx = t.index();
                    let is_target = t.to_string() == explicit;
                    g.arms[idx].reward_ema = if is_target { 1.0 } else { 0.0 };
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback helper — embedder probe
// ---------------------------------------------------------------------------

/// Probe whether the embedder is available. In production this checks the
/// `fastembed` feature + model file; in tests it is injected via
/// `MemoryRoutingState::set_embedder_available`.
pub fn probe_embedder_available() -> bool {
    // The `fastembed` cargo feature gates the ONNX embedder. When the feature
    // is off, the daemon is always in lexical fallback. When on, availability
    // is determined by whether the model file was successfully loaded — that
    // runtime probe is owned by the memory store behind Membrane; here we expose the compile-time
    // half so `memory.rs:342` has a single fallback predicate to call.
    cfg!(feature = "fastembed")
}

// ---------------------------------------------------------------------------
// Recall integration stub — demonstrates tier-aware candidate sizing
// ---------------------------------------------------------------------------

/// Build a recall plan for a query, returning the chosen tier and candidate
/// limit. Recall sites call this, then fan out to FTS / vector search with
/// the returned limit and `use_embeddings` flag.
pub fn plan_recall(
    state: &MemoryRoutingState,
    query: &str,
    embedder_available: Option<bool>,
) -> RecallPlan {
    let available = embedder_available.unwrap_or_else(probe_embedder_available);
    let tier = choose_tier(state, query, Some(available));
    let candidate_limit = limit_for(tier);
    let embeddings = should_use_embeddings(tier, available);
    RecallPlan {
        tier,
        candidate_limit,
        use_embeddings: embeddings,
        embedder_available: available,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallPlan {
    pub tier: Tier,
    pub candidate_limit: usize,
    pub use_embeddings: bool,
    pub embedder_available: bool,
}

// ---------------------------------------------------------------------------
// Tests — kept in this file so `cargo test -p coderight --lib` covers the
// wiring at `memory.rs:342` without needing a separate integration suite.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_fallback_without_embedder() {
        let state = MemoryRoutingState::new(false);
        let tier = choose_tier(&state, "find auth decisions", Some(false));
        assert_eq!(tier, Tier::Low);
        assert!(!should_use_embeddings(tier, false));
        assert_eq!(limit_for(tier), 20);
    }

    #[test]
    fn probe_matches_feature_flag() {
        // With default features (no fastembed), probe is false.
        let probed = probe_embedder_available();
        assert_eq!(probed, cfg!(feature = "fastembed"));
    }

    #[test]
    fn plan_recall_low_when_no_embedder() {
        let state = MemoryRoutingState::new(false);
        let plan = plan_recall(&state, "hello", Some(false));
        assert_eq!(plan.tier, Tier::Low);
        assert!(!plan.use_embeddings);
        assert_eq!(plan.candidate_limit, candidate_limit_for(Tier::Low));
    }

    #[test]
    fn bandit_reward_moves_tier_preference() {
        let state = MemoryRoutingState::with_params(true, 0.5, 0.0);
        // Reward Mid heavily.
        for _ in 0..10 {
            record_success(&state, Tier::Mid);
        }
        for _ in 0..10 {
            record_miss(&state, Tier::High);
            record_miss(&state, Tier::Low);
        }
        let tier = choose_tier(&state, "query", Some(true));
        assert_eq!(tier, Tier::Mid);
    }

    #[test]
    fn feedback_from_run_threshold() {
        let state = MemoryRoutingState::with_params(true, 0.5, 0.0);
        let before = snapshot(&state).mid.reward_ema;
        feedback_from_run(&state, Tier::Mid, 0.2, true); // low used_ratio -> miss
        let after = snapshot(&state).mid.reward_ema;
        assert!(after < before);
        feedback_from_run(&state, Tier::Mid, 0.9, true); // high used_ratio -> hit
        let after2 = snapshot(&state).mid.reward_ema;
        assert!(after2 > after);
    }

    #[test]
    fn apply_config_overrides_alpha_epsilon() {
        let state = MemoryRoutingState::new(true);
        let cfg = rightkit_app_settings::agent_config::MemoryConfig {
            bandit_alpha: Some(0.7),
            bandit_epsilon: Some(0.25),
            ..Default::default()
        };
        apply_config(&state, &cfg);
        let g = state.inner.lock().unwrap();
        assert!((g.alpha - 0.7).abs() < 1e-9);
        assert!((g.epsilon - 0.25).abs() < 1e-9);
    }

    #[test]
    fn apply_config_explicit_tier_biases_bandit() {
        let state = MemoryRoutingState::new(true);
        let cfg = rightkit_app_settings::agent_config::MemoryConfig {
            bandit_enabled: Some(false),
            retrieval_tier: "high".to_string(),
            ..Default::default()
        };
        apply_config(&state, &cfg);
        let tier = {
            let g = state.inner.lock().unwrap();
            g.choose_bandit()
        };
        assert_eq!(tier, Tier::High);
    }
}
