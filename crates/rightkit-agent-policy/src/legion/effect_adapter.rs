use crate::legion::error::AdapterError;
use crate::legion::policy_adapter::{
    ApprovalRequirement, AuthorizationRequest, CanonicalPathInput, ContractVersion, EffectClass,
    EnforcementLevel, Operation, PolicyAuthorizer, PolicyDecision, PolicyOutcome, TrustLevel,
};
use crate::legion::receipt::EffectReceipt;
use rightkit_sandbox::execution::{
    EffectExecutionError, EffectExecutionOutcome, EffectExecutionRequest, EffectExecutionStatus,
    EffectPerformer,
};

/// Legion policy metadata paired with the host app's lossless execution request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectRequest {
    pub execution: EffectExecutionRequest,
    pub effect_class: EffectClass,
    pub operation: Operation,
    pub target: String,
    pub repository: String,
    pub worktree: String,
    pub approval: ApprovalRequirement,
    pub contract: ContractVersion,
    pub trust: TrustLevel,
    pub enforcement: EnforcementLevel,
    pub tags: BTreeSet<String>,
    pub canonical_path: CanonicalPathInput,
    pub capability_ttl_seconds: u64,
    pub capability_max_uses: u32,
    pub capability_delegable: bool,
    pub lease_active: bool,
    pub receipt_present: bool,
}

impl EffectRequest {
    pub fn validate(&self) -> Result<(), AdapterError> {
        self.execution
            .validate()
            .map_err(|error| AdapterError::InvalidRequest(error.to_string()))?;
        if self.execution.policy_id.trim().is_empty()
            || self.execution.capability_id.trim().is_empty()
        {
            return Err(AdapterError::InvalidRequest(
                "execution policy/capability identity is required".into(),
            ));
        }
        if self.target.trim().is_empty()
            || self.repository.trim().is_empty()
            || self.worktree.trim().is_empty()
        {
            return Err(AdapterError::InvalidRequest(
                "effect target and scope are required".into(),
            ));
        }
        self.canonical_path.validate()?;
        if self.canonical_path.repository != self.repository
            || self.canonical_path.worktree != self.worktree
        {
            return Err(AdapterError::InvalidRequest(
                "canonical path repository/worktree identity disagrees with effect scope".into(),
            ));
        }
        if self.target != self.canonical_path.normalized_relative_path
            && self.target != self.canonical_path.normalized_absolute_path
        {
            return Err(AdapterError::InvalidRequest(
                "effect target is not canonical path".into(),
            ));
        }
        if !self.execution.cwd.starts_with('/') {
            return Err(AdapterError::InvalidRequest(
                "execution cwd must be absolute".into(),
            ));
        }
        Ok(())
    }

    fn authorization(&self) -> AuthorizationRequest {
        AuthorizationRequest {
            request_id: self.execution.invocation_id.clone(),
            effect_class: self.effect_class,
            capability: self.execution.capability_id.clone(),
            operation: self.operation,
            target: self.target.clone(),
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            approval: self.approval,
            contract: self.contract.clone(),
            trust: self.trust,
            enforcement: self.enforcement,
            tags: self.tags.clone(),
            canonical_path: self.canonical_path.clone(),
            capability_ttl_seconds: self.capability_ttl_seconds,
            capability_max_uses: self.capability_max_uses,
            capability_delegable: self.capability_delegable,
            lease_active: self.lease_active,
            receipt_present: self.receipt_present,
        }
    }
}

/// the host app's sole effect adapter. Policy is consulted exactly once before
/// the injected performer; denied/approval requests never reach the performer.
pub struct EffectExecutor<E> {
    authorizer: PolicyAuthorizer,
    performer: E,
}

impl<E> EffectExecutor<E>
where
    E: EffectPerformer,
{
    pub fn new(authorizer: PolicyAuthorizer, performer: E) -> Self {
        Self {
            authorizer,
            performer,
        }
    }
    pub fn authorizer(&self) -> &PolicyAuthorizer {
        &self.authorizer
    }

    pub async fn execute(&self, request: &EffectRequest) -> EffectReceipt {
        if let Err(error) = request.validate() {
            return invalid_receipt(request, &error.to_string());
        }
        if request.execution.policy_id != self.authorizer.pack().policy_id {
            return invalid_receipt(
                request,
                "execution policy identity does not match policy pack",
            );
        }
        let decision = self.authorizer.authorize(&request.authorization());
        match decision.outcome {
            PolicyOutcome::Allow => match self.performer.perform(request.execution.clone()).await {
                Ok(outcome) => receipt_from_outcome(request, decision, outcome),
                Err(error) => receipt_from_error(request, decision, error),
            },
            PolicyOutcome::RequireApproval => {
                let reason = decision.reason.clone();
                receipt_from_decision(request, decision, "require_approval", &reason)
            }
            PolicyOutcome::Deny => {
                let reason = decision.reason.clone();
                receipt_from_decision(request, decision, "deny", &reason)
            }
        }
    }
}

pub type HostEffectExecutor<E> = EffectExecutor<E>;
pub type EffectExecutorAdapter<E> = EffectExecutor<E>;

fn receipt_from_outcome(
    request: &EffectRequest,
    decision: PolicyDecision,
    outcome: EffectExecutionOutcome,
) -> EffectReceipt {
    let complete = outcome.status == EffectExecutionStatus::Succeeded
        && outcome.evidence.process_tree.cleanup_complete;
    let result = format!("{:?}", outcome.status).to_lowercase();
    EffectReceipt {
        schema_version: 1,
        receipt_id: format!("receipt:{}", request.execution.invocation_id),
        request_id: request.execution.invocation_id.clone(),
        invocation_id: request.execution.invocation_id.clone(),
        task_id: request.execution.task_id.clone(),
        plan_id: request.execution.plan_id.clone(),
        policy_id: decision.policy_id,
        policy_version: decision.policy_version,
        policy_digest: decision.policy_digest,
        capability_id: request.execution.capability_id.clone(),
        effect_class: request.effect_class.as_str().into(),
        target: request.target.clone(),
        operation: request.operation.as_str().into(),
        decision: "allow".into(),
        approval: format!("{:?}", request.approval).to_lowercase(),
        result,
        complete,
        matched_rule_ids: decision.matched_rule_ids,
        rejected_rule_ids: decision.rejected_rule_ids,
        capability_ttl_seconds: request.capability_ttl_seconds,
        capability_max_uses: request.capability_max_uses,
        capability_delegable: request.capability_delegable,
        lease_active: request.lease_active,
        execution_request: Some(request.execution.clone()),
        evidence: outcome.evidence.clone(),
        outcome: Some(outcome),
        gaps: if complete {
            Vec::new()
        } else {
            vec!["execution_incomplete".into()]
        },
    }
}

fn receipt_from_error(
    request: &EffectRequest,
    decision: PolicyDecision,
    error: EffectExecutionError,
) -> EffectReceipt {
    let mut receipt = receipt_from_decision(request, decision, "allow", &error.to_string());
    receipt.execution_request = Some(request.execution.clone());
    receipt.result = error.to_string();
    receipt
}

fn receipt_from_decision(
    request: &EffectRequest,
    decision: PolicyDecision,
    verdict: &str,
    reason: &str,
) -> EffectReceipt {
    let mut receipt = EffectReceipt::denied(
        &request.execution.invocation_id,
        request.effect_class.as_str(),
        &request.target,
        request.operation.as_str(),
        reason,
        (
            &decision.policy_id,
            decision.policy_version,
            &decision.policy_digest,
        ),
    );
    receipt.invocation_id = request.execution.invocation_id.clone();
    receipt.task_id = request.execution.task_id.clone();
    receipt.plan_id = request.execution.plan_id.clone();
    receipt.capability_id = request.execution.capability_id.clone();
    receipt.approval = format!("{:?}", request.approval).to_lowercase();
    receipt.decision = verdict.into();
    receipt.matched_rule_ids = decision.matched_rule_ids;
    receipt.rejected_rule_ids = decision.rejected_rule_ids;
    receipt.capability_ttl_seconds = request.capability_ttl_seconds;
    receipt.capability_max_uses = request.capability_max_uses;
    receipt.capability_delegable = request.capability_delegable;
    receipt.lease_active = request.lease_active;
    receipt
}

fn invalid_receipt(request: &EffectRequest, reason: &str) -> EffectReceipt {
    receipt_from_decision(
        request,
        PolicyDecision {
            outcome: PolicyOutcome::Deny,
            rule_id: None,
            matched_rule_ids: Vec::new(),
            rejected_rule_ids: Vec::new(),
            reason: reason.into(),
            policy_id: request.execution.policy_id.clone(),
            policy_version: 0,
            policy_digest: "sha256:invalid".into(),
        },
        "deny",
        reason,
    )
}
