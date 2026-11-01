use crate::legion::error::AdapterError;
use crate::{
    CondOp, Condition, PermissionDecision, PolicyDecisionPoint, PolicyRequest,
    PolicyRule as CoderightRule,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

pub const POLICY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EffectClass {
    FileWrite,
    FileDelete,
    FileMove,
    CommandExec,
    NetworkEgress,
    ProcessSpawn,
    CredentialAccess,
    DependencyInstall,
    VcsCommit,
    VcsPush,
    Publish,
    ExternalSideEffect,
}

impl EffectClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FileWrite => "FILE_WRITE",
            Self::FileDelete => "FILE_DELETE",
            Self::FileMove => "FILE_MOVE",
            Self::CommandExec => "COMMAND_EXEC",
            Self::NetworkEgress => "NETWORK_EGRESS",
            Self::ProcessSpawn => "PROCESS_SPAWN",
            Self::CredentialAccess => "CREDENTIAL_ACCESS",
            Self::DependencyInstall => "DEPENDENCY_INSTALL",
            Self::VcsCommit => "VCS_COMMIT",
            Self::VcsPush => "VCS_PUSH",
            Self::Publish => "PUBLISH",
            Self::ExternalSideEffect => "EXTERNAL_SIDE_EFFECT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Read,
    Write,
    Delete,
    Move,
    Execute,
    Connect,
    Spawn,
    Commit,
    Push,
    Publish,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Execute => "execute",
            Self::Connect => "connect",
            Self::Spawn => "spawn",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::Publish => "publish",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SymlinkState {
    NotFollowed,
    Resolved { target: String },
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalPathInput {
    pub root_identity: String,
    pub repository: String,
    pub worktree: String,
    pub normalized_absolute_path: String,
    pub normalized_relative_path: String,
    pub symlink: SymlinkState,
}

impl CanonicalPathInput {
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.root_identity.trim().is_empty()
            || self.repository.trim().is_empty()
            || self.worktree.trim().is_empty()
        {
            return Err(AdapterError::InvalidRequest(
                "canonical path scope is incomplete".into(),
            ));
        }
        if !self.root_identity.starts_with('/') {
            return Err(AdapterError::InvalidRequest(
                "root identity must be canonical absolute path".into(),
            ));
        }
        if !is_normalized_absolute(&self.normalized_absolute_path)
            || !is_normalized_relative(&self.normalized_relative_path)
        {
            return Err(AdapterError::InvalidRequest(
                "canonical path is not normalized".into(),
            ));
        }
        let suffix = format!("/{}", self.normalized_relative_path);
        if self.normalized_absolute_path != suffix
            && !contains_path_suffix(&self.normalized_absolute_path, &suffix)
        {
            return Err(AdapterError::InvalidRequest(
                "canonical path root/relative projection disagrees".into(),
            ));
        }
        let root = normalize_root(&self.root_identity)?;
        let expected = if self.normalized_relative_path.is_empty() {
            root.clone()
        } else {
            format!("{root}/{}", self.normalized_relative_path)
        };
        if self.normalized_absolute_path != expected {
            return Err(AdapterError::InvalidRequest(
                "canonical path is outside declared root identity".into(),
            ));
        }
        if matches!(self.symlink, SymlinkState::Unknown) {
            return Err(AdapterError::InvalidRequest(
                "symlink resolution is unknown".into(),
            ));
        }
        if let SymlinkState::Resolved { target } = &self.symlink {
            if !is_normalized_absolute(target) {
                return Err(AdapterError::InvalidRequest(
                    "resolved symlink target is not canonical".into(),
                ));
            }
            if target != &root && !target.starts_with(&(root.clone() + "/")) {
                return Err(AdapterError::InvalidRequest(
                    "resolved symlink target escapes declared root identity".into(),
                ));
            }
        }
        Ok(())
    }

    /// Scope-aware containment used by policy translators. String suffixes
    /// alone are insufficient: root identity, repository/worktree identity,
    /// & symlink resolution must all remain identical.
    pub fn contains(&self, child: &Self) -> bool {
        self.validate().is_ok()
            && child.validate().is_ok()
            && self.root_identity == child.root_identity
            && self.repository == child.repository
            && self.worktree == child.worktree
            && self.symlink == child.symlink
            && (self.normalized_absolute_path == child.normalized_absolute_path
                || child
                    .normalized_absolute_path
                    .starts_with(&(self.normalized_absolute_path.clone() + "/")))
    }
}

fn is_normalized_absolute(value: &str) -> bool {
    value.strip_prefix('/').is_some_and(|relative| {
        !relative.is_empty()
            && !relative.contains('\\')
            && !relative.ends_with('/')
            && !relative.split('/').any(|part| {
                part.is_empty() || part == "." || part == ".." || part.chars().any(char::is_control)
            })
    })
}

fn is_normalized_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.ends_with('/')
        && !value.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.chars().any(char::is_control)
        })
}

fn contains_path_suffix(absolute: &str, suffix: &str) -> bool {
    absolute == suffix
        || absolute
            .strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty())
}

fn normalize_root(value: &str) -> Result<String, AdapterError> {
    if !is_normalized_absolute(value) {
        return Err(AdapterError::InvalidRequest(
            "root identity is not a canonical absolute root".into(),
        ));
    }
    Ok(value.to_owned())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalRequirement {
    None,
    User,
    Authority,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLevel {
    Unauthenticated,
    HostConnectionTrust,
    CapabilitySignature,
}

impl TrustLevel {
    pub fn satisfies(self, minimum: Self) -> bool {
        self >= minimum
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementLevel {
    Unsupported,
    ReadOnly,
    Observed,
    Strong,
}

impl EnforcementLevel {
    pub fn satisfies(self, minimum: Self) -> bool {
        self >= minimum
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleDecision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVersion {
    pub name: String,
    pub major: u32,
    pub minor: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCeiling {
    pub effects: BTreeSet<EffectClass>,
    pub operations: BTreeSet<String>,
    pub targets: BTreeSet<String>,
    pub max_ttl_seconds: u64,
    pub max_uses: u32,
    pub delegable: bool,
    pub trust: TrustLevel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeasePolicy {
    pub max_ttl_seconds: u64,
    pub max_uses: u32,
    pub delegable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustMinima {
    pub mutation: TrustLevel,
    pub read_only: TrustLevel,
    pub claim_release: TrustLevel,
    pub legacy_import: TrustLevel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEnforcement {
    pub required_for_mutation: EnforcementLevel,
    pub required_for_read_only: EnforcementLevel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRequirements {
    pub effect_receipt: bool,
    pub bind_policy_digest: bool,
    pub bind_capability_id: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePredicate {
    #[serde(default)]
    pub effect_class: Option<EffectClass>,
    #[serde(default)]
    pub operations: BTreeSet<Operation>,
    #[serde(default)]
    pub repositories: BTreeSet<String>,
    #[serde(default)]
    pub worktrees: BTreeSet<String>,
    #[serde(default)]
    pub path_prefixes: BTreeSet<String>,
    #[serde(default)]
    pub required_tags: BTreeSet<String>,
    #[serde(default)]
    pub contract: Option<ContractVersion>,
    #[serde(default)]
    pub trust: Option<TrustLevel>,
    #[serde(default)]
    pub approval: Option<ApprovalRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub schema_version: u32,
    pub id: String,
    pub effect_class: EffectClass,
    pub rule: RuleDecision,
    #[serde(default)]
    pub predicate: RulePredicate,
    #[serde(default)]
    pub approval_required: bool,
    #[serde(default = "default_trust")]
    pub trust_minimum: TrustLevel,
    #[serde(default = "default_enforcement")]
    pub required_enforcement: EnforcementLevel,
    #[serde(default)]
    pub receipt_required: bool,
    #[serde(default)]
    pub exception_capable: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPack {
    pub schema_version: u32,
    pub kind: String,
    pub policy_id: String,
    pub version: u32,
    pub contract_versions: Vec<ContractVersion>,
    pub unclassified_effect: String,
    pub effect_rules: Vec<PolicyRule>,
    pub capability: CapabilityCeiling,
    pub leases: LeasePolicy,
    pub trust_minima: TrustMinima,
    pub host_enforcement: HostEnforcement,
    pub receipt_requirements: ReceiptRequirements,
}

fn default_trust() -> TrustLevel {
    TrustLevel::Unauthenticated
}
fn default_enforcement() -> EnforcementLevel {
    EnforcementLevel::Unsupported
}

impl PolicyPack {
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(AdapterError::UnsupportedSchema(self.schema_version));
        }
        if self.kind != "arcane-policy-pack"
            || self.policy_id.trim().is_empty()
            || self.version == 0
        {
            return Err(AdapterError::InvalidPolicy(
                "kind, policy_id, and positive version are required".into(),
            ));
        }
        if self.unclassified_effect != "deny" {
            return Err(AdapterError::InvalidPolicy(
                "unclassified effects must deny".into(),
            ));
        }
        let mut ids = HashSet::new();
        for contract in &self.contract_versions {
            if contract.name.trim().is_empty() || contract.major == 0 {
                return Err(AdapterError::InvalidPolicy(
                    "invalid contract version".into(),
                ));
            }
        }
        for rule in &self.effect_rules {
            if rule.schema_version != POLICY_SCHEMA_VERSION {
                return Err(AdapterError::UnsupportedSchema(rule.schema_version));
            }
            if rule.id.trim().is_empty() || !ids.insert(&rule.id) {
                return Err(AdapterError::InvalidPolicy(
                    "rule IDs must be non-empty and unique".into(),
                ));
            }
        }
        if self.capability.max_ttl_seconds == 0
            || self.capability.max_uses == 0
            || self.leases.max_ttl_seconds == 0
            || self.leases.max_uses == 0
        {
            return Err(AdapterError::InvalidPolicy(
                "lease ceilings must be positive".into(),
            ));
        }
        if self.capability.max_ttl_seconds > self.leases.max_ttl_seconds
            || self.capability.max_uses > self.leases.max_uses
            || self.capability.delegable && !self.leases.delegable
        {
            return Err(AdapterError::InvalidPolicy(
                "capability ceiling exceeds lease policy".into(),
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AdapterError> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.contract_versions.sort();
        canonical.effect_rules.sort_by(|a, b| a.id.cmp(&b.id));
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|e| AdapterError::InvalidPolicy(e.to_string()))?;
        let digest = rightkit_crypto::hex_encode(&rightkit_crypto::sha256(&bytes));
        Ok(format!("sha256:{digest}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRequest {
    pub request_id: String,
    pub effect_class: EffectClass,
    pub capability: String,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyOutcome {
    Allow,
    Deny,
    RequireApproval,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyDecision {
    pub outcome: PolicyOutcome,
    pub rule_id: Option<String>,
    pub matched_rule_ids: Vec<String>,
    pub rejected_rule_ids: Vec<String>,
    pub reason: String,
    pub policy_id: String,
    pub policy_version: u32,
    pub policy_digest: String,
}

/// Translates Legion policy data into the existing the host app PDP. The PDP is
/// called exactly once per `authorize` call; no second evaluator is consulted.
#[derive(Clone, Debug)]
pub struct PolicyAuthorizer {
    pack: PolicyPack,
    pdp: PolicyDecisionPoint,
    digest: String,
}

pub type PolicyAuthorizerAdapter = PolicyAuthorizer;
pub type LegionPolicyPack = PolicyPack;

impl PolicyAuthorizer {
    pub fn new(pack: PolicyPack) -> Result<Self, AdapterError> {
        let digest = pack.digest()?;
        let mut pdp = PolicyDecisionPoint::new();
        let mut rules = pack.effect_rules.clone();
        // One PDP pass owns terminal precedence. Non-exception denials win;
        // allows then outrank exception-capable denials, matching Legion's
        // explicit exception rule. Stable IDs break ties within each class.
        rules.sort_by(|left, right| {
            precedence_class(left)
                .cmp(&precedence_class(right))
                .then_with(|| left.id.cmp(&right.id))
        });
        for rule in rules {
            pdp.add_rule(to_host_rule(&rule)?);
        }
        Ok(Self { pack, pdp, digest })
    }

    pub fn pack(&self) -> &PolicyPack {
        &self.pack
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn authorize(&self, request: &AuthorizationRequest) -> PolicyDecision {
        if !self
            .pack
            .contract_versions
            .iter()
            .any(|contract| contract == &request.contract)
        {
            return self.denied("unsupported contract version".into());
        }
        let input = match to_policy_request(request) {
            Ok(value) => value,
            Err(error) => return self.denied(error.to_string()),
        };
        let ceiling_failure = self.ceiling_failure(request);
        // This is the sole terminal PDP invocation for this effect. It returns
        // stable all-match evidence & exception-aware precedence itself.
        let pdp = self.pdp.evaluate_with_evidence(&input);
        let base = |outcome, rule_id, reason| PolicyDecision {
            outcome,
            rule_id,
            matched_rule_ids: pdp.matched_rule_ids.clone(),
            rejected_rule_ids: pdp.rejected_rule_ids.clone(),
            reason,
            policy_id: self.pack.policy_id.clone(),
            policy_version: self.pack.version,
            policy_digest: self.digest.clone(),
        };
        if let Some(reason) = ceiling_failure {
            return base(PolicyOutcome::Deny, pdp.rule_id.clone(), reason.into());
        }
        if pdp.matched_rule_ids.is_empty() {
            return base(PolicyOutcome::Deny, None, "no matching rule".into());
        }
        let selected = pdp
            .rule_id
            .as_ref()
            .and_then(|id| self.pack.effect_rules.iter().find(|rule| &rule.id == id));
        let Some(rule) = selected else {
            return base(
                PolicyOutcome::Deny,
                None,
                "PDP returned unknown stable rule ID".into(),
            );
        };
        if pdp.decision == PermissionDecision::Deny {
            return base(
                PolicyOutcome::Deny,
                Some(rule.id.clone()),
                "Agent PDP decision: Deny".into(),
            );
        }
        if rule.approval_required
            && !request_approval_satisfies(request.approval, ApprovalRequirement::User)
        {
            return base(
                PolicyOutcome::RequireApproval,
                Some(rule.id.clone()),
                "approval required".into(),
            );
        }
        if !request.lease_active {
            return base(
                PolicyOutcome::Deny,
                Some(rule.id.clone()),
                "capability lease is not active".into(),
            );
        }
        if (rule.receipt_required || self.pack.receipt_requirements.effect_receipt)
            && !request.receipt_present
        {
            return base(
                PolicyOutcome::Deny,
                Some(rule.id.clone()),
                "effect receipt is required".into(),
            );
        }
        if !request.trust.satisfies(
            rule.trust_minimum
                .max(required_trust(&self.pack, request.operation)),
        ) {
            return base(
                PolicyOutcome::Deny,
                Some(rule.id.clone()),
                "trust is insufficient".into(),
            );
        }
        if !request.enforcement.satisfies(
            rule.required_enforcement
                .max(required_enforcement(&self.pack, request.operation)),
        ) {
            return base(
                PolicyOutcome::Deny,
                Some(rule.id.clone()),
                "enforcement is insufficient".into(),
            );
        }
        let outcome = match pdp.decision {
            PermissionDecision::Allow => PolicyOutcome::Allow,
            PermissionDecision::Deny => PolicyOutcome::Deny,
            PermissionDecision::Ask => PolicyOutcome::RequireApproval,
        };
        base(
            outcome,
            Some(rule.id.clone()),
            format!("Agent PDP decision: {:?}", pdp.decision),
        )
    }

    fn denied(&self, reason: String) -> PolicyDecision {
        PolicyDecision {
            outcome: PolicyOutcome::Deny,
            rule_id: None,
            matched_rule_ids: Vec::new(),
            rejected_rule_ids: Vec::new(),
            reason,
            policy_id: self.pack.policy_id.clone(),
            policy_version: self.pack.version,
            policy_digest: self.digest.clone(),
        }
    }

    fn ceiling_failure(&self, request: &AuthorizationRequest) -> Option<&'static str> {
        if let Err(error) = request.canonical_path.validate() {
            return Some(match error {
                AdapterError::InvalidRequest(_) => "invalid canonical path",
                _ => "invalid canonical path",
            });
        }
        let ceiling = &self.pack.capability;
        if !ceiling.effects.contains(&request.effect_class) {
            return Some("capability ceiling excludes effect class");
        }
        if !ceiling.operations.contains(request.operation.as_str()) {
            return Some("capability ceiling excludes operation");
        }
        if !ceiling.targets.is_empty()
            && !ceiling.targets.iter().any(|target| {
                request.target == *target || request.target.starts_with(&(target.clone() + "/"))
            })
        {
            return Some("capability ceiling excludes target");
        }
        if !request.trust.satisfies(ceiling.trust) {
            return Some("capability ceiling requires stronger trust");
        }
        if request.capability_ttl_seconds == 0
            || request.capability_ttl_seconds > ceiling.max_ttl_seconds
        {
            return Some("capability TTL exceeds ceiling");
        }
        if request.capability_max_uses == 0 || request.capability_max_uses > ceiling.max_uses {
            return Some("capability uses exceed ceiling");
        }
        if request.capability_delegable && !ceiling.delegable {
            return Some("capability delegation exceeds ceiling");
        }
        if !request.lease_active {
            return Some("capability lease is not active");
        }
        if self.pack.receipt_requirements.effect_receipt && !request.receipt_present {
            return Some("effect receipt is required");
        }
        None
    }
}

fn to_host_rule(rule: &PolicyRule) -> Result<CoderightRule, AdapterError> {
    let mut output = CoderightRule::new(
        rule.id.clone(),
        match rule.rule {
            RuleDecision::Allow => PermissionDecision::Allow,
            RuleDecision::Deny => PermissionDecision::Deny,
        },
        tool_name(rule.effect_class),
    );
    let p = &rule.predicate;
    let mut conditions = Vec::new();
    if let Some(effect) = p.effect_class {
        conditions.push(Condition::new("effect_class", CondOp::Eq, effect.as_str()));
    }
    if !p.operations.is_empty() {
        conditions.push(Condition::new(
            "operation",
            CondOp::In,
            join(p.operations.iter().map(|v| v.as_str())),
        ));
    }
    if !p.repositories.is_empty() {
        conditions.push(Condition::new(
            "repository",
            CondOp::In,
            join(p.repositories.iter().map(String::as_str)),
        ));
    }
    if !p.worktrees.is_empty() {
        conditions.push(Condition::new(
            "worktree",
            CondOp::In,
            join(p.worktrees.iter().map(String::as_str)),
        ));
    }
    // PDP's StartsWithAny condition preserves Legion's any-prefix semantics
    // inside its single authoritative evaluation.
    if !p.path_prefixes.is_empty() {
        conditions.push(Condition::new(
            "target",
            CondOp::StartsWithAny,
            join(p.path_prefixes.iter().map(String::as_str)),
        ));
    }
    if let Some(contract) = &p.contract {
        conditions.push(Condition::new(
            "contract",
            CondOp::Eq,
            contract_key(contract),
        ));
    }
    if let Some(trust) = p.trust {
        conditions.push(Condition::new("trust", CondOp::In, trust_values(trust)));
    }
    if let Some(approval) = p.approval {
        conditions.push(Condition::new(
            "approval",
            CondOp::In,
            approval_values(approval),
        ));
    }
    // Rule-level approval, trust, receipt, & enforcement are obligations on a
    // selected rule, not match predicates. Encoding them as conditions lets a
    // weaker rule win precisely when stronger evidence is missing.
    if rule.rule == RuleDecision::Deny && rule.exception_capable {
        conditions.push(Condition::new("exception", CondOp::Eq, "false"));
    }
    for tag in &p.required_tags {
        conditions.push(Condition::new("tags", CondOp::Eq, tag.clone()));
    }
    for condition in conditions {
        output = output.with_condition(condition);
    }
    Ok(output.with_exception_capable(rule.exception_capable))
}

fn to_policy_request(request: &AuthorizationRequest) -> Result<PolicyRequest, AdapterError> {
    if request.request_id.trim().is_empty()
        || request.capability.trim().is_empty()
        || request.target.trim().is_empty()
        || request.repository.trim().is_empty()
        || request.worktree.trim().is_empty()
    {
        return Err(AdapterError::InvalidRequest(
            "request identity, capability, and target are required".into(),
        ));
    }
    let mut output = PolicyRequest::new(tool_name(request.effect_class))
        .with_attribute("effect_class", request.effect_class.as_str())
        .with_attribute("capability", request.capability.clone())
        .with_attribute("operation", request.operation.as_str())
        .with_attribute(
            "target",
            request.canonical_path.normalized_relative_path.clone(),
        )
        .with_attribute("repository", request.repository.clone())
        .with_attribute("worktree", request.worktree.clone())
        .with_attribute(
            "root_identity",
            request.canonical_path.root_identity.clone(),
        )
        .with_attribute(
            "normalized_relative_path",
            request.canonical_path.normalized_relative_path.clone(),
        )
        .with_attribute("symlink", format!("{:?}", request.canonical_path.symlink))
        .with_attribute("approval", format!("{:?}", request.approval))
        .with_attribute("contract", contract_key(&request.contract))
        .with_attribute("trust", format!("{:?}", request.trust))
        .with_attribute("enforcement", format!("{:?}", request.enforcement))
        .with_attribute(
            "capability_ttl_seconds",
            request.capability_ttl_seconds.to_string(),
        )
        .with_attribute(
            "capability_max_uses",
            request.capability_max_uses.to_string(),
        )
        .with_attribute(
            "capability_delegable",
            request.capability_delegable.to_string(),
        )
        .with_attribute("lease_active", request.lease_active.to_string())
        .with_attribute("receipt_present", request.receipt_present.to_string());
    for tag in &request.tags {
        output = output.with_attribute("tags", tag);
    }
    output = output.with_attribute(
        "exception",
        if request.tags.contains("exception") {
            "true"
        } else {
            "false"
        },
    );
    Ok(output)
}

fn precedence_class(rule: &PolicyRule) -> u8 {
    match (rule.rule, rule.exception_capable) {
        (RuleDecision::Deny, false) => 0,
        (RuleDecision::Allow, _) => 1,
        (RuleDecision::Deny, true) => 2,
    }
}

fn request_approval_satisfies(actual: ApprovalRequirement, required: ApprovalRequirement) -> bool {
    match required {
        ApprovalRequirement::None => true,
        ApprovalRequirement::User => matches!(
            actual,
            ApprovalRequirement::User | ApprovalRequirement::Authority
        ),
        ApprovalRequirement::Authority => actual == ApprovalRequirement::Authority,
    }
}

fn required_trust(pack: &PolicyPack, operation: Operation) -> TrustLevel {
    if operation == Operation::Read {
        pack.trust_minima.read_only
    } else {
        pack.trust_minima.mutation
    }
}

fn required_enforcement(pack: &PolicyPack, operation: Operation) -> EnforcementLevel {
    if operation == Operation::Read {
        pack.host_enforcement.required_for_read_only
    } else {
        pack.host_enforcement.required_for_mutation
    }
}

fn tool_name(effect: EffectClass) -> String {
    format!("legion:{}", effect.as_str())
}
fn contract_key(contract: &ContractVersion) -> String {
    format!("{}@{}.{}", contract.name, contract.major, contract.minor)
}
fn join<'a, I>(values: I) -> String
where
    I: Iterator<Item = &'a str>,
{
    values.collect::<Vec<_>>().join(",")
}
fn trust_values(minimum: TrustLevel) -> String {
    match minimum {
        TrustLevel::Unauthenticated => "Unauthenticated,HostConnectionTrust,CapabilitySignature",
        TrustLevel::HostConnectionTrust => "HostConnectionTrust,CapabilitySignature",
        TrustLevel::CapabilitySignature => "CapabilitySignature",
    }
    .into()
}
fn approval_values(minimum: ApprovalRequirement) -> String {
    match minimum {
        ApprovalRequirement::None => "None,User,Authority",
        ApprovalRequirement::User => "User,Authority",
        ApprovalRequirement::Authority => "Authority",
    }
    .into()
}
