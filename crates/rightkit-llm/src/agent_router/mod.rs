//! `host_router` — pure-heuristic model selection (ADR
//! `docs/canon/daemon-master-canon-atoms.md`, Lane R1a-L2).
//!
//! This crate answers a question distinct from
//! `host_orchestration::dispatch::DispatchRouter` (Direct vs Conduct):
//! **which model** should run a given piece of work. It composes with, and
//! never replaces, the complexity-dispatch router (ADR §0).
//!
//! No LLM classifier lives here — that is explicitly deferred to R1b (ADR
//! §D1). Everything in this crate is a pure function over a
//! [`signal::TaskSignal`], a model roster
//! (`crate::agent_providers::model_metadata::ModelCapability`), and a
//! [`policy::RoutingPolicy`].

pub mod decision;
pub mod pins;
pub mod policy;
pub mod prompt_shape;
pub mod signal;
pub mod triage;

pub use decision::{
    CandidateReceipt, EnforcementMode, EscalationReceipt, EscalationTrigger, RouteDecision,
    VerifierReceipt, ROUTE_DECISION_SCHEMA_VERSION,
};
pub use pins::{resolve as resolve_pins, PinContext, PinResolution, PinSource};
pub use policy::{RoutingPolicy, SignalWeights};
pub use signal::{PromptShape, TaskSignal, TaskSignalBuilder, TurnType};
pub use triage::{
    is_stalled, stall_state, triage, triage_with_health, BudgetCeiling, StallState,
    MAX_NO_PROGRESS_STREAK,
};

use crate::agent_providers::model_metadata::ModelCapability;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouteError {
    #[error("model `{0}` is not present in the authoritative live catalog")]
    UnknownModel(String),
    #[error("route already used its one bounded escalation")]
    AlreadyEscalated,
    #[error("no qualified escalation candidate remains")]
    NoQualifiedEscalation,
}

/// Full entry point composing pin precedence (ADR §D5) with the heuristic
/// `triage()` (ADR §D1/§D3): resolves pins first — a pin at any level above
/// `Router` short-circuits and the heuristic never runs — and only calls
/// `triage()` when nothing pins this request. Falls back to
/// `pins.configured_default` if `triage()` itself yields no eligible
/// candidate (roster empty, or every candidate excluded by context-window/
/// budget constraints).
pub fn route(
    sig: &TaskSignal,
    roster: &[ModelCapability],
    policy: &RoutingPolicy,
    budget: &BudgetCeiling,
    pins: &PinContext,
) -> Option<RouteDecision> {
    route_checked(sig, roster, policy, budget, pins)
        .ok()
        .flatten()
}

/// Strict routing entry point. Unlike the compatibility [`route`] wrapper,
/// this preserves the visible error required at execution boundaries when a
/// pin or configured default names a model absent from the authoritative
/// catalog.
pub fn route_checked(
    sig: &TaskSignal,
    roster: &[ModelCapability],
    policy: &RoutingPolicy,
    budget: &BudgetCeiling,
    pins: &PinContext,
) -> Result<Option<RouteDecision>, RouteError> {
    let resolution = resolve_pins(pins);
    if resolution.is_pinned() {
        let model_id = resolution
            .model_id
            .expect("pinned resolution carries a model_id");
        let tier = roster
            .iter()
            .find(|m| m.slug == model_id)
            .map(|m| m.tier)
            .ok_or_else(|| RouteError::UnknownModel(model_id.clone()))?;
        let reason = match resolution.source {
            PinSource::PerTurn => "Explicit per-turn user pin; router does not run.",
            PinSource::PerAgentOverride => "Per-agent override pin; router does not run.",
            PinSource::SessionPin => {
                "Session pin is a hard wall; router contributes no vote for this session."
            }
            PinSource::Router | PinSource::ConfiguredDefault => {
                unreachable!("is_pinned() only true for PerTurn/PerAgentOverride/SessionPin")
            }
        };
        return Ok(Some(RouteDecision::new(
            model_id,
            tier,
            reason,
            1.0,
            EnforcementMode::None,
        )));
    }

    if let Some(decision) = triage::triage(sig, roster, policy, budget) {
        return Ok(Some(decision));
    }

    pins.configured_default
        .as_ref()
        .map(|model_id| {
            let tier = roster
                .iter()
                .find(|m| &m.slug == model_id)
                .map(|m| m.tier)
                .ok_or_else(|| RouteError::UnknownModel(model_id.clone()))?;
            Ok(RouteDecision::new(
                model_id.clone(),
                tier,
                "Router produced no eligible candidate; falling back to the configured default.",
                0.0,
                EnforcementMode::None,
            ))
        })
        .transpose()
}

/// Apply at most one evidence-backed escalation while preserving the route
/// fingerprint that identifies the original task/candidate decision.
pub fn bounded_escalation(
    prior: &RouteDecision,
    trigger: EscalationTrigger,
    roster: &[ModelCapability],
) -> Result<RouteDecision, RouteError> {
    if prior.escalation.is_some() {
        return Err(RouteError::AlreadyEscalated);
    }
    let current_success = prior
        .candidates
        .iter()
        .find(|candidate| candidate.model_id == prior.model_id)
        .map(|candidate| candidate.expected_success)
        .unwrap_or_default();
    let candidate = prior
        .candidates
        .iter()
        .filter(|candidate| {
            let tier_escalates = roster
                .iter()
                .find(|model| model.slug == candidate.model_id)
                .is_some_and(|model| {
                    prior.tier == crate::agent_providers::model_metadata::ModelTier::Cheap
                        && model.tier == crate::agent_providers::model_metadata::ModelTier::Capable
                });
            candidate.qualified
                && candidate.model_id != prior.model_id
                && (candidate.expected_success > current_success || tier_escalates)
        })
        .max_by(|left, right| {
            left.expected_success
                .total_cmp(&right.expected_success)
                .then_with(|| right.expected_cost_usd.total_cmp(&left.expected_cost_usd))
        })
        .ok_or(RouteError::NoQualifiedEscalation)?;
    let capability = roster
        .iter()
        .find(|model| model.slug == candidate.model_id)
        .ok_or_else(|| RouteError::UnknownModel(candidate.model_id.clone()))?;
    let mut decision = prior.clone();
    let from = decision.model_id.clone();
    decision.model_id = capability.slug.clone();
    decision.tier = capability.tier;
    decision.confidence = candidate.expected_success;
    decision.expected_cost_usd = candidate.expected_cost_usd;
    decision.reason = format!(
        "Bounded escalation after {trigger:?}: {from} -> {}; route fingerprint preserved.",
        capability.slug
    );
    decision.escalation = Some(EscalationReceipt {
        attempt: 1,
        trigger,
        from_model_id: from,
        to_model_id: capability.slug.clone(),
        route_fingerprint: prior.route_fingerprint.clone(),
    });
    Ok(decision)
}
