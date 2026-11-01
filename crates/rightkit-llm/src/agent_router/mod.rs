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

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::agent_providers::model_metadata::ModelTier;

    fn roster() -> Vec<ModelCapability> {
        vec![
            ModelCapability {
                slug: "cheap/small".to_string(),
                prompt_per_m: 0.10,
                completion_per_m: 0.20,
                context_window_tokens: 100_000,
                compaction_trigger_percent: 70,
                compaction_reserve_tokens: 20_000,
                compaction_max_output_tokens: 8_192,
                tier: ModelTier::Cheap,
                reasoning: 1,
                supported_efforts: None,
                tool_schema_class: "openai-tools".to_string(),
                vision: false,
            },
            ModelCapability {
                slug: "capable/big".to_string(),
                prompt_per_m: 2.00,
                completion_per_m: 10.00,
                context_window_tokens: 200_000,
                compaction_trigger_percent: 70,
                compaction_reserve_tokens: 20_000,
                compaction_max_output_tokens: 8_192,
                tier: ModelTier::Capable,
                reasoning: 3,
                supported_efforts: None,
                tool_schema_class: "openai-tools".to_string(),
                vision: true,
            },
        ]
    }

    fn narrow_signal() -> TaskSignal {
        TaskSignalBuilder::new()
            .tool_call_density(1)
            .failure_streak(0)
            .remaining_budget_usd(10.0)
            .scoped_file_count(1)
            .prompt_shape(PromptShape {
                word_count: 8,
                sentence_count: 1,
                is_question: false,
                broad_scope: false,
            })
            .current_context_tokens(1_000)
            .build()
    }

    // Table-driven: every precedence level from ADR §D5, exhaustively.
    #[test]
    fn precedence_per_turn_pin_beats_all() {
        let pins = PinContext {
            per_turn_pin: Some("capable/big".to_string()),
            per_agent_override: Some("cheap/small".to_string()),
            session_pin: Some("cheap/small".to_string()),
            configured_default: Some("cheap/small".to_string()),
        };
        let decision = route(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap();
        assert_eq!(decision.model_id, "capable/big");
    }

    #[test]
    fn precedence_per_agent_override_beats_session_pin_and_router() {
        let pins = PinContext {
            per_turn_pin: None,
            per_agent_override: Some("capable/big".to_string()),
            session_pin: Some("cheap/small".to_string()),
            configured_default: None,
        };
        let decision = route(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap();
        assert_eq!(decision.model_id, "capable/big");
    }

    #[test]
    fn precedence_session_pin_beats_router_decision() {
        // Signal is "narrow" (would route cheap), but the session pin names
        // the capable model — the hard wall means the router's own opinion
        // never gets a vote.
        let pins = PinContext {
            per_turn_pin: None,
            per_agent_override: None,
            session_pin: Some("capable/big".to_string()),
            configured_default: None,
        };
        let decision = route(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap();
        assert_eq!(decision.model_id, "capable/big");
    }

    #[test]
    fn unknown_pin_fails_visibly_against_authoritative_catalog() {
        let pins = PinContext {
            per_turn_pin: Some("unknown/model".into()),
            ..PinContext::default()
        };
        let error = route_checked(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap_err();
        assert_eq!(error, RouteError::UnknownModel("unknown/model".into()));
    }

    #[test]
    fn precedence_no_pins_falls_to_router_decision() {
        let pins = PinContext::default();
        let decision = route(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap();
        assert_eq!(decision.model_id, "cheap/small");
    }

    #[test]
    fn precedence_configured_default_used_only_when_router_yields_nothing() {
        let pins = PinContext {
            per_turn_pin: None,
            per_agent_override: None,
            session_pin: None,
            configured_default: Some("cheap/small".to_string()),
        };
        // An undersized but catalogued row forces triage() to return None,
        // leaving the explicitly configured default as the final level.
        let mut known_roster = roster();
        known_roster.truncate(1);
        known_roster[0].context_window_tokens = 1;
        let decision = route(
            &narrow_signal(),
            &known_roster,
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        )
        .unwrap();
        assert_eq!(decision.model_id, "cheap/small");
    }

    #[test]
    fn no_pin_and_no_router_candidate_and_no_default_returns_none() {
        let pins = PinContext::default();
        let decision = route(
            &narrow_signal(),
            &[],
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &pins,
        );
        assert!(decision.is_none());
    }

    #[test]
    fn typed_failure_allows_exactly_one_escalation_and_preserves_fingerprint() {
        let prior = route(
            &narrow_signal(),
            &roster(),
            &RoutingPolicy::conservative(),
            &BudgetCeiling::default(),
            &PinContext::default(),
        )
        .unwrap();
        let fingerprint = prior.route_fingerprint.clone();
        let escalated =
            bounded_escalation(&prior, EscalationTrigger::VerifierRejected, &roster()).unwrap();
        assert_eq!(escalated.model_id, "capable/big");
        assert_eq!(escalated.route_fingerprint, fingerprint);
        assert_eq!(
            escalated.escalation.as_ref().unwrap().route_fingerprint,
            fingerprint
        );
        assert_eq!(
            bounded_escalation(&escalated, EscalationTrigger::TypedFailure, &roster()),
            Err(RouteError::AlreadyEscalated)
        );
    }

    #[test]
    fn capable_model_escalates_once_to_higher_reasoning_model() {
        let mut models = roster();
        models[0].slug = "capable/fast".into();
        models[0].tier = ModelTier::Capable;
        models[0].reasoning = 1;
        models[1].slug = "capable/reasoning".into();
        models[1].tier = ModelTier::Capable;
        models[1].reasoning = 3;

        let mut prior = RouteDecision::new(
            "capable/fast",
            ModelTier::Capable,
            "initial capable route",
            0.70,
            EnforcementMode::None,
        );
        prior.route_fingerprint = "route-fingerprint".into();
        prior.candidates = vec![
            CandidateReceipt {
                model_id: "capable/fast".into(),
                provider_family: "test".into(),
                expected_success: 0.70,
                expected_cost_usd: 0.01,
                expected_value: 0.69,
                health: "healthy".into(),
                qualified: true,
                exclusion_reason: None,
            },
            CandidateReceipt {
                model_id: "capable/reasoning".into(),
                provider_family: "test".into(),
                expected_success: 0.90,
                expected_cost_usd: 0.03,
                expected_value: 0.87,
                health: "healthy".into(),
                qualified: true,
                exclusion_reason: None,
            },
        ];

        let escalated =
            bounded_escalation(&prior, EscalationTrigger::VerifierRejected, &models).unwrap();
        let selected = models
            .iter()
            .find(|model| model.slug == escalated.model_id)
            .unwrap();
        assert_eq!(escalated.model_id, "capable/reasoning");
        assert!(selected.reasoning > models[0].reasoning);
        assert_eq!(escalated.route_fingerprint, prior.route_fingerprint);
        assert_eq!(
            bounded_escalation(&escalated, EscalationTrigger::TypedFailure, &models),
            Err(RouteError::AlreadyEscalated)
        );
    }
}
