//! `triage()` — pure-heuristic model selection (ADR §D1: no LLM classifier
//! in R1a; that returns only in R1b).
//!
//! Rules, in evaluation order:
//! 1. Sensitive floor: `touches_sensitive` hard-floors to capable +
//!    DualJury, regardless of every other signal or the policy threshold.
//! 2. Context-window hard constraint: a candidate is never eligible if its
//!    `context_window_tokens` is less than `current_context_tokens` scaled
//!    by `(1.0 + policy.context_window_headroom_ratio)`.
//! 3. Failure-streak hysteresis: `failure_streak >= 2` floors to capable for
//!    the remainder of the task (oscillation guard — no bang-bang between
//!    cheap and capable turn-to-turn).
//! 4. Budget consult: the caller supplies a ceiling (governance is wired by
//!    the seam lane, never imported here); any candidate whose per-call
//!    projected cost busts the ceiling is excluded.
//! 5. Otherwise: compute a route-down confidence score from
//!    `policy.weights`; route down (cheap) only when the score meets or
//!    exceeds `policy.route_down_confidence_threshold`. Any uncertainty
//!    routes UP (capable) — this includes both a below-threshold score and
//!    an empty cheap-tier candidate set.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::agent_providers::health::HealthState;
use crate::agent_providers::model_metadata::{ModelCapability, ModelTier};

use crate::agent_router::decision::{
    CandidateReceipt, EnforcementMode, RouteDecision, VerifierReceipt,
};

/// Runtime classification for bounded no-progress handling. The middle state
/// is the hysteresis floor used before the hard stall pivot; callers can
/// record the distinction without inferring it from a numeric counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StallState {
    Progressing,
    Hysteresis,
    Stalled,
}

use crate::agent_router::policy::RoutingPolicy;
use crate::agent_router::signal::TaskSignal;

/// Maximum consecutive failed/no-progress turns before a route is marked
/// stalled. The engine's goal controller owns terminal closure; the router
/// uses this bound only to make the escalation reason explicit.
pub const MAX_NO_PROGRESS_STREAK: u32 = 3;

/// Classify the observed failure streak without mutating caller-owned state.
/// `Hysteresis` floors routing to capable before the hard pivot at `Stalled`.
pub fn stall_state(failure_streak: u32) -> StallState {
    if failure_streak >= MAX_NO_PROGRESS_STREAK {
        StallState::Stalled
    } else if failure_streak >= 2 {
        StallState::Hysteresis
    } else {
        StallState::Progressing
    }
}

/// Whether the observed failure streak has reached the bounded stall state.
/// This remains a pure projection of caller-owned runtime state.
pub fn is_stalled(failure_streak: u32) -> bool {
    matches!(stall_state(failure_streak), StallState::Stalled)
}

/// Budget ceiling context for one triage call. The seam lane wires this from
/// `OrchestrationGovernance`; this crate never imports governance directly
/// (ADR §D3: "The router consults `OrchestrationGovernance::gate_dispatch()`
/// before finalizing" — that consultation happens at the call site, not
/// inside this pure function, since governance lives in a different crate
/// this lane does not depend on).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetCeiling {
    /// Maximum acceptable projected cost in USD for a single routed call.
    /// `f64::MAX` means "no ceiling" (caller opted out of a budget check).
    pub max_projected_cost_usd: f64,
}

impl Default for BudgetCeiling {
    fn default() -> Self {
        Self {
            max_projected_cost_usd: f64::MAX,
        }
    }
}

/// Rough projected cost of a single call: prompt tokens priced at
/// `prompt_per_m`, assuming the current context is the prompt. This is a
/// coarse pre-call estimate (the ADR's `est_cost_usd`), not a reconciled
/// actual.
fn projected_cost_usd(candidate: &ModelCapability, context_tokens: u64) -> f64 {
    (context_tokens as f64 / 1_000_000.0) * candidate.prompt_per_m
        + (2_000.0 / 1_000_000.0) * candidate.completion_per_m
}

fn provider_family(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(provider, _)| provider)
}

fn expected_success(sig: &TaskSignal, candidate: &ModelCapability, policy: &RoutingPolicy) -> f64 {
    let route_down = route_down_confidence(sig, policy);
    let routine = route_down >= policy.route_down_confidence_threshold;
    let base: f64 = match candidate.tier {
        // Preserve the held-out-calibrated shape of the shipped router as a
        // feature rather than discarding it: cheap success rises only when
        // multiple independent narrow/routine signals agree.
        ModelTier::Cheap if routine => 0.92,
        ModelTier::Cheap => 0.45 + 0.25 * route_down,
        ModelTier::Capable if routine => 0.86,
        ModelTier::Capable => 0.94,
    };
    (base + (candidate.reasoning as f64 - 1.0) * 0.015).clamp(0.0, 0.99)
}

fn candidate_receipts(
    sig: &TaskSignal,
    roster: &[ModelCapability],
    policy: &RoutingPolicy,
    budget: &BudgetCeiling,
    health: &HashMap<String, HealthState>,
) -> Vec<CandidateReceipt> {
    let max_cost = roster
        .iter()
        .map(|candidate| projected_cost_usd(candidate, sig.current_context_tokens))
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    let mut receipts = roster
        .iter()
        .map(|candidate| {
            let route_health = health
                .get(&candidate.slug)
                .copied()
                .unwrap_or(HealthState::Healthy);
            let cost = projected_cost_usd(candidate, sig.current_context_tokens);
            let mut qualified = true;
            let mut exclusion_reason = None;
            if !has_context_headroom(
                candidate,
                sig.current_context_tokens,
                policy.context_window_headroom_ratio,
            ) {
                qualified = false;
                exclusion_reason = Some("context_window".into());
            } else if cost > budget.max_projected_cost_usd {
                qualified = false;
                exclusion_reason = Some("budget".into());
            } else if !route_health.is_usable() {
                qualified = false;
                exclusion_reason = Some("route_failed".into());
            }
            let health_multiplier = route_health.weight();
            let success = expected_success(sig, candidate, policy) * health_multiplier;
            let cost_penalty = (cost / max_cost).min(1.0) * 0.20;
            CandidateReceipt {
                model_id: candidate.slug.clone(),
                provider_family: provider_family(&candidate.slug).to_string(),
                expected_success: success,
                expected_cost_usd: cost,
                expected_value: success - cost_penalty,
                health: format!("{route_health:?}").to_ascii_lowercase(),
                qualified,
                exclusion_reason,
            }
        })
        .collect::<Vec<_>>();
    receipts.sort_by(|left, right| {
        right
            .expected_value
            .total_cmp(&left.expected_value)
            .then_with(|| left.expected_cost_usd.total_cmp(&right.expected_cost_usd))
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    receipts
}

fn verifier_for(producer: &ModelCapability, receipts: &[CandidateReceipt]) -> VerifierReceipt {
    let producer_family = provider_family(&producer.slug);
    if let Some(candidate) = receipts.iter().find(|candidate| {
        candidate.qualified
            && candidate.model_id != producer.slug
            && candidate.provider_family != producer_family
    }) {
        return VerifierReceipt {
            model_id: Some(candidate.model_id.clone()),
            independent: true,
            degradation_reason: None,
        };
    }
    let fallback = receipts
        .iter()
        .find(|candidate| candidate.qualified && candidate.model_id != producer.slug)
        .map(|candidate| candidate.model_id.clone());
    VerifierReceipt {
        model_id: fallback,
        independent: false,
        degradation_reason: Some(
            "no qualified verifier from a different provider family".to_string(),
        ),
    }
}

fn attach_receipt(
    decision: RouteDecision,
    producer: &ModelCapability,
    receipts: Vec<CandidateReceipt>,
    sig: &TaskSignal,
) -> RouteDecision {
    let expected_cost = receipts
        .iter()
        .find(|candidate| candidate.model_id == producer.slug)
        .map(|candidate| candidate.expected_cost_usd)
        .unwrap_or_default();
    let verifier = verifier_for(producer, &receipts);
    let mut fingerprint = DefaultHasher::new();
    serde_json::to_string(sig)
        .unwrap_or_default()
        .hash(&mut fingerprint);
    for candidate in &receipts {
        candidate.model_id.hash(&mut fingerprint);
        candidate.expected_cost_usd.to_bits().hash(&mut fingerprint);
        candidate.qualified.hash(&mut fingerprint);
    }
    decision.with_receipt(
        receipts,
        expected_cost,
        verifier,
        format!("{:016x}", fingerprint.finish()),
    )
}

/// True if `candidate`'s context window has enough headroom above
/// `current_context_tokens` per the policy's required ratio.
fn has_context_headroom(
    candidate: &ModelCapability,
    current_context_tokens: u64,
    headroom_ratio: f64,
) -> bool {
    let required = (current_context_tokens as f64) * (1.0 + headroom_ratio);
    (candidate.context_window_tokens as f64) >= required
}

/// Compute the route-down confidence score in `[0.0, 1.0]` from a
/// `TaskSignal` and the policy's named weights. Higher = more confident that
/// the cheap tier is sufficient.
fn route_down_confidence(sig: &TaskSignal, policy: &RoutingPolicy) -> f64 {
    let mut score = 0.0;

    if sig.tool_call_density <= 2 {
        score += policy.weights.low_tool_call_density;
    }
    if sig.failure_streak == 0 {
        score += policy.weights.no_failure_streak;
    }
    if !sig.prompt_shape.broad_scope && !sig.prompt_shape.is_question {
        // A short, non-broad-scope, non-question prompt is the narrowest
        // shape; longer questions or broad-scope asks reduce, not add, to
        // this term.
        if sig.prompt_shape.word_count <= 20 && sig.prompt_shape.sentence_count <= 1 {
            score += policy.weights.narrow_prompt_shape;
        }
    } else if sig.prompt_shape.is_question && sig.prompt_shape.word_count <= 20 {
        // Short explanatory questions are also a strong cheap signal,
        // mirroring DispatchRouter's own "short question -> direct" rule.
        score += policy.weights.narrow_prompt_shape;
    }
    if sig.scoped_file_count <= 1 {
        score += policy.weights.single_file_scope;
    }
    if sig.remaining_budget_usd > 0.0 {
        score += policy.weights.budget_headroom;
    }

    score.clamp(0.0, 1.0)
}

/// Find the cheapest-tier candidate (by ascending `prompt_per_m`) in
/// `roster` that has context-window headroom over `current_context_tokens`
/// and stays within `budget.max_projected_cost_usd`.
fn best_candidate<'a>(
    roster: &'a [ModelCapability],
    tier: ModelTier,
    current_context_tokens: u64,
    headroom_ratio: f64,
    budget: &BudgetCeiling,
) -> Option<&'a ModelCapability> {
    roster
        .iter()
        .filter(|m| m.tier == tier)
        .filter(|m| has_context_headroom(m, current_context_tokens, headroom_ratio))
        .filter(|m| projected_cost_usd(m, current_context_tokens) <= budget.max_projected_cost_usd)
        .min_by(|a, b| {
            a.prompt_per_m
                .partial_cmp(&b.prompt_per_m)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Fall back across tiers when the preferred tier has no eligible candidate
/// (context-window or budget excluded every row): capable first (never
/// silently degrade a floor to nothing), then cheap, then `None` if the
/// roster truly has nothing eligible.
fn any_eligible_candidate<'a>(
    roster: &'a [ModelCapability],
    current_context_tokens: u64,
    headroom_ratio: f64,
    budget: &BudgetCeiling,
) -> Option<&'a ModelCapability> {
    best_candidate(
        roster,
        ModelTier::Capable,
        current_context_tokens,
        headroom_ratio,
        budget,
    )
    .or_else(|| {
        best_candidate(
            roster,
            ModelTier::Cheap,
            current_context_tokens,
            headroom_ratio,
            budget,
        )
    })
}

/// Decide which model should run this task. Pure function: same inputs
/// always yield the same [`RouteDecision`]. Returns `None` only when the
/// roster has no candidate that fits the context-window and budget
/// constraints at all (caller should fall back to
/// `PinContext.configured_default`).
pub fn triage(
    sig: &TaskSignal,
    roster: &[ModelCapability],
    policy: &RoutingPolicy,
    budget: &BudgetCeiling,
) -> Option<RouteDecision> {
    let health = crate::agent_providers::health::live_route_health();
    triage_with_health(sig, roster, policy, budget, &health)
}

pub fn triage_with_health(
    sig: &TaskSignal,
    roster: &[ModelCapability],
    policy: &RoutingPolicy,
    budget: &BudgetCeiling,
    health: &HashMap<String, HealthState>,
) -> Option<RouteDecision> {
    let headroom_ratio = policy.context_window_headroom_ratio;
    let receipts = candidate_receipts(sig, roster, policy, budget, health);

    // Rule 1: sensitive floor — capable + DualJury, regardless of threshold.
    if sig.touches_sensitive {
        let candidate = best_candidate(
            roster,
            ModelTier::Capable,
            sig.current_context_tokens,
            headroom_ratio,
            budget,
        )
        .or_else(|| {
            any_eligible_candidate(roster, sig.current_context_tokens, headroom_ratio, budget)
        })?;
        return Some(attach_receipt(
            RouteDecision::new(
                candidate.slug.clone(),
                candidate.tier,
                "Sensitive-surface floor: task touches auth/money/migrate/delete/secret \
             surface area; hard-floored to a capable model with DualJury enforcement \
             regardless of every other signal.",
                1.0,
                EnforcementMode::DualJury,
            ),
            candidate,
            receipts,
            sig,
        ));
    }

    // A bounded stall is a stronger form of the existing hysteresis floor.
    // Keep it separate so callers and receipts can distinguish a pivot caused
    // by ordinary failures from the terminal no-progress bound.
    if is_stalled(sig.failure_streak) {
        let candidate = best_candidate(
            roster,
            ModelTier::Capable,
            sig.current_context_tokens,
            headroom_ratio,
            budget,
        )
        .or_else(|| {
            any_eligible_candidate(roster, sig.current_context_tokens, headroom_ratio, budget)
        })?;
        return Some(attach_receipt(
            RouteDecision::new(
                candidate.slug.clone(),
                candidate.tier,
                format!(
                    "No-progress stall bound: {} consecutive failed-outcome turns reached the \
                     bounded stall threshold {}; pivoting to capable for the remainder of this task.",
                    sig.failure_streak,
                    MAX_NO_PROGRESS_STREAK,
                ),
                1.0,
                EnforcementMode::None,
            ),
            candidate,
            receipts,
            sig,
        ));
    }

    // Rule 3: failure-streak hysteresis — floor to capable for the
    // remainder of the task once streak >= 2, no oscillation back down.
    if matches!(
        stall_state(sig.failure_streak),
        StallState::Hysteresis | StallState::Stalled
    ) {
        let candidate = best_candidate(
            roster,
            ModelTier::Capable,
            sig.current_context_tokens,
            headroom_ratio,
            budget,
        )
        .or_else(|| {
            any_eligible_candidate(roster, sig.current_context_tokens, headroom_ratio, budget)
        })?;
        return Some(attach_receipt(
            RouteDecision::new(
                candidate.slug.clone(),
                candidate.tier,
                format!(
                    "Failure-streak hysteresis: {} consecutive failed-outcome turns; \
                 escalated to capable for the remainder of this task to avoid oscillation.",
                    sig.failure_streak
                ),
                1.0,
                EnforcementMode::None,
            ),
            candidate,
            receipts,
            sig,
        ));
    }

    // Rule 5: maximize expected value across the qualified set. The legacy
    // route-down confidence remains one calibrated feature and is recorded in
    // the reason, but it no longer hardcodes cheap-vs-capable selection.
    let confidence = route_down_confidence(sig, policy);
    let winner = receipts.iter().find(|candidate| candidate.qualified)?;
    let candidate = roster.iter().find(|row| row.slug == winner.model_id)?;
    Some(attach_receipt(
        RouteDecision::new(
            candidate.slug.clone(),
            candidate.tier,
            format!(
                "Expected-value selection chose {} (success {:.2}, estimated cost ${:.6}, \
             value {:.3}); route-down confidence {confidence:.2} under {} policy.",
                candidate.slug,
                winner.expected_success,
                winner.expected_cost_usd,
                winner.expected_value,
                policy.name,
            ),
            winner.expected_success,
            EnforcementMode::None,
        ),
        candidate,
        receipts,
        sig,
    ))
}

#[cfg(test)]
#[path = "triage_tests.rs"]
mod tests;

#[cfg(test)]
mod stall_bound_inline_tests {
    use super::*;

    #[test]
    fn is_stalled_boundary_is_three() {
        assert!(!is_stalled(MAX_NO_PROGRESS_STREAK - 1));
        assert!(is_stalled(MAX_NO_PROGRESS_STREAK));
        assert!(is_stalled(MAX_NO_PROGRESS_STREAK + 5));
    }

    #[test]
    fn stall_threshold_distinct_from_hysteresis_floor() {
        // Hysteresis floors at 2, stall at 3 — distinct receipts.
        const { assert!(2 < MAX_NO_PROGRESS_STREAK) };
        assert!(!is_stalled(2));
        assert!(is_stalled(3));
    }
}
