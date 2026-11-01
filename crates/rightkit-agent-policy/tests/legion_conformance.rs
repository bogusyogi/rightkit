use async_trait::async_trait;
use rightkit_agent_policy::legion::*;
use rightkit_sandbox::execution::{
    EffectExecutionError, EffectExecutionEvidence, EffectExecutionOutcome, EffectExecutionRequest,
    EffectExecutionStatus, EffectPerformer,
};
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn pack(rule: RuleDecision) -> PolicyPack {
    let effects = [EffectClass::FileWrite].into_iter().collect();
    let capability_operations: BTreeSet<_> = ["write".to_string()].into_iter().collect();
    let rule_operations: BTreeSet<_> = [Operation::Write].into_iter().collect();
    PolicyPack {
        schema_version: 1,
        kind: "arcane-policy-pack".into(),
        policy_id: "conformance".into(),
        version: 1,
        contract_versions: vec![ContractVersion {
            name: "effect".into(),
            major: 1,
            minor: 0,
        }],
        unclassified_effect: "deny".into(),
        effect_rules: vec![PolicyRule {
            schema_version: 1,
            id: "file-write".into(),
            effect_class: EffectClass::FileWrite,
            rule,
            predicate: RulePredicate {
                operations: rule_operations,
                ..RulePredicate::default()
            },
            approval_required: false,
            trust_minimum: TrustLevel::Unauthenticated,
            required_enforcement: EnforcementLevel::Unsupported,
            receipt_required: false,
            exception_capable: false,
            note: None,
        }],
        capability: CapabilityCeiling {
            effects,
            operations: capability_operations,
            targets: BTreeSet::new(),
            max_ttl_seconds: 60,
            max_uses: 1,
            delegable: false,
            trust: TrustLevel::Unauthenticated,
        },
        leases: LeasePolicy {
            max_ttl_seconds: 60,
            max_uses: 1,
            delegable: false,
        },
        trust_minima: TrustMinima {
            mutation: TrustLevel::Unauthenticated,
            read_only: TrustLevel::Unauthenticated,
            claim_release: TrustLevel::Unauthenticated,
            legacy_import: TrustLevel::Unauthenticated,
        },
        host_enforcement: HostEnforcement {
            required_for_mutation: EnforcementLevel::Unsupported,
            required_for_read_only: EnforcementLevel::Unsupported,
        },
        receipt_requirements: ReceiptRequirements {
            effect_receipt: true,
            bind_policy_digest: true,
            bind_capability_id: true,
        },
    }
}

fn request() -> EffectRequest {
    let execution =
        EffectExecutionRequest::new("inv-1", "/bin/tool", vec!["--safe".into()], "/workspace")
            .with_context("task-1", "plan-1", "conformance", "cap-1");
    EffectRequest {
        execution,
        effect_class: EffectClass::FileWrite,
        operation: Operation::Write,
        target: "src/lib.rs".into(),
        repository: "repo".into(),
        worktree: "tree".into(),
        approval: ApprovalRequirement::None,
        contract: ContractVersion {
            name: "effect".into(),
            major: 1,
            minor: 0,
        },
        trust: TrustLevel::Unauthenticated,
        enforcement: EnforcementLevel::Unsupported,
        tags: BTreeSet::new(),
        canonical_path: CanonicalPathInput {
            root_identity: "/workspace".into(),
            repository: "repo".into(),
            worktree: "tree".into(),
            normalized_absolute_path: "/workspace/src/lib.rs".into(),
            normalized_relative_path: "src/lib.rs".into(),
            symlink: SymlinkState::NotFollowed,
        },
        capability_ttl_seconds: 10,
        capability_max_uses: 1,
        capability_delegable: false,
        lease_active: true,
        receipt_present: true,
    }
}

#[derive(Clone)]
struct Counting(Arc<AtomicUsize>);

#[async_trait]
impl EffectPerformer for Counting {
    async fn perform(
        &self,
        _: EffectExecutionRequest,
    ) -> Result<EffectExecutionOutcome, EffectExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let mut evidence = EffectExecutionEvidence::default();
        evidence.process_tree.cleanup_complete = true;
        Ok(EffectExecutionOutcome {
            status: EffectExecutionStatus::Succeeded,
            evidence,
        })
    }
}

#[tokio::test]
async fn digest_and_rule_identity_are_stable() {
    let first = pack(RuleDecision::Allow);
    let mut second = first.clone();
    second.effect_rules.reverse();
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let receipt = EffectExecutor::new(PolicyAuthorizer::new(first).unwrap(), Counting(calls))
        .execute(&request())
        .await;
    assert_eq!(
        receipt.matched_rule_ids,
        vec!["file-write"],
        "{}",
        receipt.result
    );
    assert!(receipt.policy_digest.starts_with("sha256:"));
}

#[tokio::test]
async fn deny_never_reaches_actual_performer() {
    let calls = Arc::new(AtomicUsize::new(0));
    let receipt = EffectExecutor::new(
        PolicyAuthorizer::new(pack(RuleDecision::Deny)).unwrap(),
        Counting(calls.clone()),
    )
    .execute(&request())
    .await;
    assert_eq!(receipt.decision, "deny");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!receipt.is_complete());
}

#[tokio::test]
async fn capability_and_execution_evidence_are_retained() {
    let calls = Arc::new(AtomicUsize::new(0));
    let receipt = EffectExecutor::new(
        PolicyAuthorizer::new(pack(RuleDecision::Allow)).unwrap(),
        Counting(calls),
    )
    .execute(&request())
    .await;
    assert_eq!(receipt.decision, "allow");
    assert_eq!(receipt.capability_id, "cap-1");
    assert_eq!(receipt.invocation_id, "inv-1");
    assert_eq!(
        receipt.execution_request.as_ref().unwrap().argv,
        vec!["--safe"]
    );
    assert!(receipt.outcome.is_some());
    assert!(receipt.is_complete());
}

#[tokio::test]
async fn capability_operation_outside_ceiling_fails_closed() {
    let mut blocked = request();
    blocked.operation = Operation::Connect;
    let receipt = EffectExecutor::new(
        PolicyAuthorizer::new(pack(RuleDecision::Allow)).unwrap(),
        Counting(Arc::new(AtomicUsize::new(0))),
    )
    .execute(&blocked)
    .await;
    assert_eq!(receipt.decision, "deny");
    assert!(receipt.result.contains("capability ceiling"));
}

#[tokio::test]
async fn selected_rule_obligations_cannot_fall_through_to_weaker_rule() {
    let mut policy = pack(RuleDecision::Allow);
    let mut weaker = policy.effect_rules[0].clone();
    weaker.id = "z-weaker".into();
    policy.effect_rules[0].id = "a-approval".into();
    policy.effect_rules[0].approval_required = true;
    policy.effect_rules.push(weaker);
    let calls = Arc::new(AtomicUsize::new(0));
    let receipt = EffectExecutor::new(
        PolicyAuthorizer::new(policy).unwrap(),
        Counting(calls.clone()),
    )
    .execute(&request())
    .await;
    assert_eq!(receipt.decision, "require_approval");
    assert_eq!(receipt.matched_rule_ids, vec!["a-approval", "z-weaker"]);
    assert!(receipt.result.contains("approval required"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn required_tags_match_exact_values() {
    let mut policy = pack(RuleDecision::Allow);
    policy.effect_rules[0]
        .predicate
        .required_tags
        .insert("approved".into());
    let mut near_match = request();
    near_match.tags.insert("not-approved".into());
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = EffectExecutor::new(
        PolicyAuthorizer::new(policy).unwrap(),
        Counting(calls.clone()),
    );
    assert_eq!(executor.execute(&near_match).await.decision, "deny");
    near_match.tags.insert("approved".into());
    assert_eq!(executor.execute(&near_match).await.decision, "allow");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
