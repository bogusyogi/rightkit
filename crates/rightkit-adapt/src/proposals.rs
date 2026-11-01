//! WP-11 / ADR-0021: proposal value types and deterministic builder.
//! FM-091 governance — proposals carry immutable base_version + candidate
//! payload, are gated by `EvalGate` (held-out + safety + cost/latency), and
//! require a human `PromotionRecord` before `OptimizationAdmission`.

use crate::eval_gate::{EvalEvidence, EvalGate, GateOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    AddNode {
        node_kind: String,
    },
    RemoveNode {
        node_id: String,
    },
    ChangeRetry {
        node_id: String,
        new_attempts: u32,
    },
    ChangeCapability {
        added: Vec<String>,
        removed: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub workflow_id: String,
    pub proposed_by: String,
    pub change_kind: ProposalKind,
    pub diff_summary: String,
    pub evidence: EvalEvidence,
    pub created_at: String,
    pub gate_outcome: GateOutcome,
    /// Absent until a human reviewer records a promotion decision.
    pub promotion: Option<PromotionRecord>,
    /// Immutable source version and candidate payload captured offline.
    #[serde(default)]
    pub base_version_id: String,
    #[serde(default)]
    pub base_content_hash: String,
    #[serde(default)]
    pub candidate_definition_json: String,
    #[serde(default)]
    pub candidate_compiled_plan_json: String,
}

/// Durable review receipt. Activation is deliberate, reversible, & monitored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub approver: String,
    pub benchmark_id: String,
    pub activation_id: String,
    pub rollback_id: String,
    pub monitoring_id: String,
}

impl Proposal {
    /// Return reviewed copy without mutating immutable proposal draft evidence.
    pub fn reviewed_promotion(&self, receipt: PromotionRecord) -> Result<Self, String> {
        if !matches!(self.gate_outcome, GateOutcome::Proceed { .. }) {
            return Err(
                "proposal has not cleared training, heldout, safety, cost, & latency gates".into(),
            );
        }
        if receipt.approver.trim().is_empty()
            || receipt.benchmark_id.trim().is_empty()
            || receipt.activation_id.trim().is_empty()
            || receipt.rollback_id.trim().is_empty()
            || receipt.monitoring_id.trim().is_empty()
        {
            return Err("promotion receipt requires approver, benchmark, activation, rollback, & monitoring ids".into());
        }
        let mut promoted = self.clone();
        promoted.promotion = Some(receipt);
        Ok(promoted)
    }
}

/// Admission switch owned by monitoring, not by proposal authors. Once a
/// monitored promotion fails, future activation is denied until a fresh,
/// independently reviewed proposal is supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationAdmission {
    pub proposal_id: String,
    pub activation_id: String,
    pub admitting: bool,
    pub rollback_reason: Option<String>,
}

impl OptimizationAdmission {
    pub fn from_promotion(proposal: &Proposal) -> Result<Self, String> {
        let receipt = proposal
            .promotion
            .as_ref()
            .ok_or_else(|| "proposal is not reviewed for activation".to_string())?;
        Ok(Self {
            proposal_id: proposal.id.clone(),
            activation_id: receipt.activation_id.clone(),
            admitting: true,
            rollback_reason: None,
        })
    }

    /// Monitoring failure performs a forward-admission rollback; completed
    /// work remains auditable but no later run receives this optimization.
    pub fn fail_monitoring(&mut self, reason: impl Into<String>) {
        self.admitting = false;
        self.rollback_reason = Some(reason.into());
    }
}

#[derive(Debug, Clone)]
pub struct ProposalBuilder {
    workflow_id: String,
    proposed_by: String,
    change_kind: Option<ProposalKind>,
    diff_summary: String,
    evidence: Option<EvalEvidence>,
    created_at: String,
    base_version_id: String,
    base_content_hash: String,
    candidate_definition_json: String,
    candidate_compiled_plan_json: String,
}

impl ProposalBuilder {
    pub fn new(workflow_id: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            proposed_by: "system".to_string(),
            change_kind: None,
            diff_summary: String::new(),
            evidence: None,
            created_at: String::new(),
            base_version_id: String::new(),
            base_content_hash: String::new(),
            candidate_definition_json: String::new(),
            candidate_compiled_plan_json: String::new(),
        }
    }

    pub fn proposed_by(mut self, value: impl Into<String>) -> Self {
        self.proposed_by = value.into();
        self
    }

    pub fn change_kind(mut self, value: ProposalKind) -> Self {
        self.change_kind = Some(value);
        self
    }

    pub fn diff_summary(mut self, value: impl Into<String>) -> Self {
        self.diff_summary = value.into();
        self
    }

    pub fn evidence(mut self, value: EvalEvidence) -> Self {
        self.evidence = Some(value);
        self
    }

    pub fn created_at(mut self, value: impl Into<String>) -> Self {
        self.created_at = value.into();
        self
    }

    pub fn base_version(mut self, id: impl Into<String>, hash: impl Into<String>) -> Self {
        self.base_version_id = id.into();
        self.base_content_hash = hash.into();
        self
    }
    pub fn candidate_payload(
        mut self,
        definition: impl Into<String>,
        plan: impl Into<String>,
    ) -> Self {
        self.candidate_definition_json = definition.into();
        self.candidate_compiled_plan_json = plan.into();
        self
    }

    pub fn build(self) -> Result<Proposal, String> {
        if self.workflow_id.trim().is_empty() {
            return Err("workflow id is required".to_string());
        }
        if self.base_version_id.trim().is_empty() || self.base_content_hash.trim().is_empty() {
            return Err("base version id and content hash are required".to_string());
        }
        if self.candidate_definition_json.trim().is_empty()
            || self.candidate_compiled_plan_json.trim().is_empty()
        {
            return Err("candidate definition and compiled plan are required".to_string());
        }
        let change_kind = self
            .change_kind
            .ok_or_else(|| "change kind is required".to_string())?;
        let evidence = self
            .evidence
            .ok_or_else(|| "evaluation evidence is required".to_string())?;
        let canonical = serde_json::to_vec(&(
            &self.workflow_id,
            &self.proposed_by,
            &change_kind,
            &self.diff_summary,
            &evidence,
            &self.created_at,
            &self.base_version_id,
            &self.base_content_hash,
            &self.candidate_definition_json,
            &self.candidate_compiled_plan_json,
        ))
        .map_err(|error| error.to_string())?;
        let digest = rightkit_crypto::sha256(AsRef::<[u8]>::as_ref(&canonical));
        let id = format!("prop_{}", rightkit_crypto::hex_encode(&digest));
        let gate_outcome = EvalGate::new().evaluate(&evidence);
        Ok(Proposal {
            id,
            workflow_id: self.workflow_id,
            proposed_by: self.proposed_by,
            change_kind,
            diff_summary: self.diff_summary,
            evidence,
            created_at: self.created_at,
            gate_outcome,
            promotion: None,
            base_version_id: self.base_version_id,
            base_content_hash: self.base_content_hash,
            candidate_definition_json: self.candidate_definition_json,
            candidate_compiled_plan_json: self.candidate_compiled_plan_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> EvalEvidence {
        EvalEvidence {
            train_pass_rate_pct: 100.0,
            heldout_pass_rate_pct: 100.0,
            safety_pass_rate_pct: 100.0,
            cost_ratio_to_baseline: 1.0,
            latency_ratio_to_baseline: 1.0,
            pass_rate_pct: 100.0,
            regression_count: 0,
            new_untested_capability_requested: false,
            total_runs: 1,
            expected_run_count_min: 1,
        }
    }

    fn builder() -> ProposalBuilder {
        ProposalBuilder::new("wf")
            .base_version("v1", "hash")
            .candidate_payload("{\"nodes\":[]}", "{\"plan\":[]}")
            .change_kind(ProposalKind::AddNode {
                node_kind: "task".into(),
            })
            .evidence(evidence())
            .created_at("now")
    }

    #[test]
    fn identity_includes_immutable_payload() {
        let first = builder().build().unwrap();
        let second = builder()
            .candidate_payload("{\"nodes\":[1]}", "{\"plan\":[]}")
            .build()
            .unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn rejects_missing_immutable_payload() {
        assert!(ProposalBuilder::new("wf").build().is_err());
        assert!(builder().base_version("", "hash").build().is_err());
        assert!(builder().candidate_payload("", "plan").build().is_err());
    }
}
