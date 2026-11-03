use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    canonical_digest,
    id::{InvocationId, PlanId, ProviderId, ReceiptId, RequestId, TaskId},
    policy::EffectClass,
    require_version, ContractError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvocationStatus {
    Ok,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationReceipt {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub receipt_id: ReceiptId,
    pub invocation_id: InvocationId,
    pub request_id: RequestId,
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub provider: ProviderId,
    pub status: InvocationStatus,
    pub complete: bool,
    pub findings: Vec<String>,
    pub gaps: Vec<String>,
    pub artifacts: BTreeMap<String, String>,
}

impl InvocationReceipt {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_version(self.schema_version, 1)?;
        if self.complete && (!self.gaps.is_empty() || self.status != InvocationStatus::Ok) {
            return Err(ContractError::InvalidContract {
                path: "complete".into(),
                reason: "only successful gap-free invocation may be complete".into(),
            });
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, crate::canonical::CanonicalError> {
        canonical_digest(self)
    }
}

/// Concrete host mechanism that was bound to a work node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorMechanismClass {
    Builtin,
    Process,
    Lsp,
    TinyModel,
    SemanticModel,
    Human,
}

/// Typed result explaining why a host could not complete or retain a
/// binding. `Unsupported` is distinct from authority denial and from a
/// mechanism that was temporarily unreachable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorBindingOutcome {
    Unsupported,
    Ambiguous,
    Unreachable,
    Denied,
    Terminated,
    VerificationFailed,
}

/// The concrete executor selected for a node, without exposing a
/// host-specific executable or provider name as portable plan semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorBinding {
    pub class: ExecutorMechanismClass,
    pub capability: String,
    pub implementation_version: String,
}

/// Whether binding required a semantic escalation beyond the preferred
/// deterministic mechanism.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorEscalation {
    None,
    SemanticModel,
    StrongerModel,
}

/// Status of the completion check associated with the host binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingVerificationStatus {
    Passed,
    Failed,
    NotRun,
}

/// Completion-check status recorded alongside a host binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingVerification {
    pub status: BindingVerificationStatus,
}

/// Host binding receipt for one materialized work node.
///
/// The requirement digest identifies the portable requirement the host
/// matched. `binding` records the selected mechanism when one exists;
/// `outcome` records a typed failure such as `Unsupported` when it does
/// not. A host must not silently replace a forbidden semantic requirement
/// with a model executor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorBindingReceiptV1 {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub node_id: String,
    pub requirement_digest: String,
    pub binding: Option<ExecutorBinding>,
    pub semantic_model_used: bool,
    pub model_receipt_ref: Option<String>,
    pub escalation: ExecutorEscalation,
    pub outcome: Option<ExecutorBindingOutcome>,
    pub verification: BindingVerification,
}

impl ExecutorBindingReceiptV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_version(self.schema_version, 1)?;
        if self.binding.is_none() && self.outcome.is_none() {
            return Err(ContractError::InvalidContract {
                path: "binding".into(),
                reason: "an unbound node must record a binding outcome".into(),
            });
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, crate::canonical::CanonicalError> {
        canonical_digest(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectReceipt {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub receipt_id: ReceiptId,
    pub request_id: RequestId,
    pub effect_class: EffectClass,
    pub target: String,
    pub operation: String,
    pub result: String,
    pub matched: bool,
    pub actual_diff_digest: Option<String>,
    pub evidence: BTreeMap<String, serde_json::Value>,
}
