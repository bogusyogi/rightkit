use rightkit_sandbox::execution::{
    EffectExecutionEvidence, EffectExecutionOutcome, EffectExecutionRequest,
};
use serde::{Deserialize, Serialize};

pub type ExecutionEvidence = EffectExecutionEvidence;

/// Immutable the host app projection of one Legion effect decision and its full
/// execution evidence. The original execution request is retained losslessly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EffectReceipt {
    pub schema_version: u32,
    pub receipt_id: String,
    pub request_id: String,
    pub invocation_id: String,
    pub task_id: String,
    pub plan_id: String,
    pub policy_id: String,
    pub policy_version: u32,
    pub policy_digest: String,
    pub capability_id: String,
    pub effect_class: String,
    pub target: String,
    pub operation: String,
    pub decision: String,
    pub approval: String,
    pub result: String,
    pub complete: bool,
    pub matched_rule_ids: Vec<String>,
    pub rejected_rule_ids: Vec<String>,
    pub capability_ttl_seconds: u64,
    pub capability_max_uses: u32,
    pub capability_delegable: bool,
    pub lease_active: bool,
    pub execution_request: Option<EffectExecutionRequest>,
    pub outcome: Option<EffectExecutionOutcome>,
    pub evidence: ExecutionEvidence,
    #[serde(default)]
    pub gaps: Vec<String>,
}

impl EffectReceipt {
    pub fn denied(
        request_id: &str,
        class: &str,
        target: &str,
        operation: &str,
        reason: &str,
        policy: (&str, u32, &str),
    ) -> Self {
        Self {
            schema_version: 1,
            receipt_id: format!("receipt:{request_id}"),
            request_id: request_id.into(),
            invocation_id: request_id.into(),
            task_id: String::new(),
            plan_id: String::new(),
            policy_id: policy.0.into(),
            policy_version: policy.1,
            policy_digest: policy.2.into(),
            capability_id: String::new(),
            effect_class: class.into(),
            target: target.into(),
            operation: operation.into(),
            decision: "deny".into(),
            approval: "none".into(),
            result: reason.into(),
            complete: false,
            matched_rule_ids: Vec::new(),
            rejected_rule_ids: Vec::new(),
            capability_ttl_seconds: 0,
            capability_max_uses: 0,
            capability_delegable: false,
            lease_active: false,
            execution_request: None,
            outcome: None,
            evidence: ExecutionEvidence::default(),
            gaps: vec![reason.into()],
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete && self.gaps.is_empty()
    }
}
