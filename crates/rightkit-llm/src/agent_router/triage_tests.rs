//! Tests for [`crate::agent_router::triage::triage`]. Split into its own file (included by
//! `triage.rs` via `#[path]`) to keep `triage.rs` itself under the 400-line
//! module limit.

use super::*;
use crate::agent_router::signal::{PromptShape, TaskSignalBuilder, TurnType};

fn cheap_model(slug: &str, context_window: u64) -> ModelCapability {
    ModelCapability {
        slug: slug.to_string(),
        prompt_per_m: 0.10,
        completion_per_m: 0.20,
        context_window_tokens: context_window,
        compaction_trigger_percent: 70,
        compaction_reserve_tokens: 20_000,
        compaction_max_output_tokens: 8_192,
        tier: ModelTier::Cheap,
        reasoning: 1,
        supported_efforts: None,
        tool_schema_class: "openai-tools".to_string(),
        vision: false,
    }
}

fn capable_model(slug: &str, context_window: u64) -> ModelCapability {
    ModelCapability {
        slug: slug.to_string(),
        prompt_per_m: 2.00,
        completion_per_m: 10.00,
        context_window_tokens: context_window,
        compaction_trigger_percent: 70,
        compaction_reserve_tokens: 20_000,
        compaction_max_output_tokens: 8_192,
        tier: ModelTier::Capable,
        reasoning: 3,
        supported_efforts: None,
        tool_schema_class: "openai-tools".to_string(),
        vision: true,
    }
}

fn roster() -> Vec<ModelCapability> {
    vec![
        cheap_model("cheap/small", 100_000),
        capable_model("capable/big", 200_000),
    ]
}

fn narrow_signal() -> TaskSignal {
    TaskSignalBuilder::new()
        .turn_type(TurnType::PatchApply)
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

#[test]
fn mechanical_signal_routes_cheap() {
    let sig = narrow_signal();
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Cheap);
    assert_eq!(decision.enforcement, EnforcementMode::None);
}

#[test]
fn ambiguous_signal_routes_capable() {
    let sig = TaskSignalBuilder::new()
        .turn_type(TurnType::FreshReasoning)
        .tool_call_density(6)
        .failure_streak(1)
        .remaining_budget_usd(10.0)
        .scoped_file_count(5)
        .prompt_shape(PromptShape {
            word_count: 40,
            sentence_count: 3,
            is_question: false,
            broad_scope: true,
        })
        .current_context_tokens(1_000)
        .build();
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Capable);
}

#[test]
fn sensitive_floors_to_capable_and_dual_jury_regardless_of_threshold() {
    let mut sig = narrow_signal();
    sig.touches_sensitive = true;
    // Even with the most permissive policy, sensitive always wins.
    let policy = RoutingPolicy::aggressive();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Capable);
    assert_eq!(decision.enforcement, EnforcementMode::DualJury);
    assert!((decision.confidence - 1.0).abs() < 1e-9);
}

#[test]
fn context_window_constraint_excludes_undersized_candidate() {
    let mut sig = narrow_signal();
    sig.current_context_tokens = 90_000; // 90k * 1.2 = 108k > cheap's 100k window
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    // Cheap candidate excluded by window headroom; falls through to capable.
    assert_eq!(decision.tier, ModelTier::Capable);
}

#[test]
fn context_window_constraint_allows_when_headroom_sufficient() {
    let mut sig = narrow_signal();
    sig.current_context_tokens = 50_000; // 50k * 1.2 = 60k <= cheap's 100k window
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Cheap);
}

#[test]
fn failure_streak_hysteresis_floors_to_capable() {
    let mut sig = narrow_signal();
    sig.failure_streak = 2;
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Capable);
    assert!(decision.reason.contains("hysteresis"));
}

#[test]
fn failure_streak_one_does_not_trigger_hysteresis() {
    let mut sig = narrow_signal();
    sig.failure_streak = 1;
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    // failure_streak=1 loses the no_failure_streak weight but the rest of
    // the narrow signal can still clear a lenient-enough threshold; assert
    // it at least does not floor to capable via the hysteresis rule
    // specifically (reason must not mention hysteresis).
    assert!(!decision.reason.contains("hysteresis"));
}

#[test]
fn failure_streak_maps_to_typed_progress_hysteresis_and_stall_states() {
    assert_eq!(stall_state(0), StallState::Progressing);
    assert_eq!(stall_state(1), StallState::Progressing);
    assert_eq!(stall_state(2), StallState::Hysteresis);
    assert_eq!(stall_state(MAX_NO_PROGRESS_STREAK), StallState::Stalled);
}

#[test]
fn is_stalled_below_threshold_is_false() {
    assert!(!is_stalled(MAX_NO_PROGRESS_STREAK - 1));
}

#[test]
fn is_stalled_at_threshold_is_true() {
    assert!(is_stalled(MAX_NO_PROGRESS_STREAK));
}

#[test]
fn is_stalled_above_threshold_is_true() {
    assert!(is_stalled(MAX_NO_PROGRESS_STREAK + 1));
}

#[test]
fn is_stalled_zero_streak_is_false() {
    assert!(!is_stalled(0));
}

#[test]
fn stall_bound_floors_to_capable_with_distinct_reason_from_hysteresis() {
    let mut sig = narrow_signal();
    sig.failure_streak = MAX_NO_PROGRESS_STREAK;
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Capable);
    assert!(decision.reason.contains("stall"));
    assert!(decision
        .reason
        .contains(&MAX_NO_PROGRESS_STREAK.to_string()));
}

#[test]
fn one_below_stall_bound_still_hits_hysteresis_not_stall() {
    let mut sig = narrow_signal();
    sig.failure_streak = MAX_NO_PROGRESS_STREAK - 1;
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(decision.tier, ModelTier::Capable);
    assert!(decision.reason.contains("hysteresis"));
    assert!(!decision.reason.contains("stall"));
}

#[test]
fn budget_ceiling_excludes_candidate_that_busts_it() {
    let sig = narrow_signal();
    let policy = RoutingPolicy::conservative();
    let tiny_budget = BudgetCeiling {
        max_projected_cost_usd: 0.0000001,
    };
    // Both candidates cost more than the ceiling at 1000 tokens context;
    // still must return a decision from any_eligible_candidate's
    // exhaustive fallback path returning None if truly nothing fits.
    let decision = triage(&sig, &roster(), &policy, &tiny_budget);
    assert!(decision.is_none());
}

#[test]
fn deterministic_same_input_same_decision() {
    let sig = narrow_signal();
    let policy = RoutingPolicy::conservative();
    let budget = BudgetCeiling::default();
    let d1 = triage(&sig, &roster(), &policy, &budget).unwrap();
    let d2 = triage(&sig, &roster(), &policy, &budget).unwrap();
    assert_eq!(d1, d2);
}

#[test]
fn schema_version_present() {
    let sig = narrow_signal();
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &roster(), &policy, &BudgetCeiling::default()).unwrap();
    assert_eq!(
        decision.schema_version,
        crate::agent_router::decision::ROUTE_DECISION_SCHEMA_VERSION
    );
}

#[test]
fn empty_roster_returns_none() {
    let sig = narrow_signal();
    let policy = RoutingPolicy::conservative();
    let decision = triage(&sig, &[], &policy, &BudgetCeiling::default());
    assert!(decision.is_none());
}

#[test]
fn decision_receipt_records_expected_value_cost_and_independent_verifier() {
    let decision = triage(
        &narrow_signal(),
        &roster(),
        &RoutingPolicy::conservative(),
        &BudgetCeiling::default(),
    )
    .unwrap();
    assert_eq!(decision.candidates.len(), 2);
    assert!(decision.expected_cost_usd > 0.0);
    assert!(!decision.route_fingerprint.is_empty());
    let verifier = decision.verifier.unwrap();
    assert!(verifier.independent);
    assert_eq!(verifier.model_id.as_deref(), Some("capable/big"));
    assert!(verifier.degradation_reason.is_none());
}

#[test]
fn same_provider_verifier_degradation_is_explicit() {
    let same_family = vec![
        cheap_model("provider/small", 100_000),
        capable_model("provider/big", 200_000),
    ];
    let decision = triage(
        &narrow_signal(),
        &same_family,
        &RoutingPolicy::conservative(),
        &BudgetCeiling::default(),
    )
    .unwrap();
    let verifier = decision.verifier.unwrap();
    assert!(!verifier.independent);
    assert!(verifier.degradation_reason.is_some());
}

#[test]
fn live_failed_route_is_excluded_from_expected_value_set() {
    let health = HashMap::from([("cheap/small".to_string(), HealthState::Failed)]);
    let decision = triage_with_health(
        &narrow_signal(),
        &roster(),
        &RoutingPolicy::conservative(),
        &BudgetCeiling::default(),
        &health,
    )
    .unwrap();
    assert_eq!(decision.model_id, "capable/big");
    let failed = decision
        .candidates
        .iter()
        .find(|candidate| candidate.model_id == "cheap/small")
        .unwrap();
    assert!(!failed.qualified);
    assert_eq!(failed.exclusion_reason.as_deref(), Some("route_failed"));
}
