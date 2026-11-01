//! `RouteDecision` — the output shape of [`crate::agent_router::triage::triage`].

use serde::{Deserialize, Serialize};

use crate::agent_providers::model_metadata::ModelTier;

/// Whether the routed decision requires the DualJury enforcement path
/// (ADR §D1: sensitive-surface tasks are hard-floored to capable + DualJury
/// regardless of any other signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementMode {
    None,
    DualJury,
}

/// The schema version stamped on every stored routing decision (ADR §D1/§D5:
/// "`schema_version` on every stored routing decision").
pub const ROUTE_DECISION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateReceipt {
    pub model_id: String,
    pub provider_family: String,
    pub expected_success: f64,
    pub expected_cost_usd: f64,
    pub expected_value: f64,
    pub health: String,
    pub qualified: bool,
    pub exclusion_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifierReceipt {
    pub model_id: Option<String>,
    pub independent: bool,
    pub degradation_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTrigger {
    TypedFailure,
    EmptyResult,
    InvalidResult,
    UnmetPredicate,
    VerifierRejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationReceipt {
    pub attempt: u8,
    pub trigger: EscalationTrigger,
    pub from_model_id: String,
    pub to_model_id: String,
    pub route_fingerprint: String,
}

/// One triage outcome: which model to run, at what tier, why, and how
/// confident the heuristic was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub model_id: String,
    pub tier: ModelTier,
    pub reason: String,
    pub confidence: f64,
    pub enforcement: EnforcementMode,
    pub schema_version: u32,
    #[serde(default)]
    pub candidates: Vec<CandidateReceipt>,
    #[serde(default)]
    pub expected_cost_usd: f64,
    #[serde(default)]
    pub verifier: Option<VerifierReceipt>,
    #[serde(default)]
    pub route_fingerprint: String,
    #[serde(default)]
    pub escalation: Option<EscalationReceipt>,
}

impl RouteDecision {
    pub fn new(
        model_id: impl Into<String>,
        tier: ModelTier,
        reason: impl Into<String>,
        confidence: f64,
        enforcement: EnforcementMode,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            tier,
            reason: reason.into(),
            confidence,
            enforcement,
            schema_version: ROUTE_DECISION_SCHEMA_VERSION,
            candidates: Vec::new(),
            expected_cost_usd: 0.0,
            verifier: None,
            route_fingerprint: String::new(),
            escalation: None,
        }
    }

    pub fn with_receipt(
        mut self,
        candidates: Vec<CandidateReceipt>,
        expected_cost_usd: f64,
        verifier: VerifierReceipt,
        route_fingerprint: String,
    ) -> Self {
        self.candidates = candidates;
        self.expected_cost_usd = expected_cost_usd;
        self.verifier = Some(verifier);
        self.route_fingerprint = route_fingerprint;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stamps_current_schema_version() {
        let decision = RouteDecision::new(
            "deepseek/deepseek-v4-flash",
            ModelTier::Cheap,
            "test",
            0.9,
            EnforcementMode::None,
        );
        assert_eq!(decision.schema_version, ROUTE_DECISION_SCHEMA_VERSION);
    }
}
