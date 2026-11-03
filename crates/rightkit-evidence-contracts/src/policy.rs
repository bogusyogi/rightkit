use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    canonical_digest,
    id::{AgentId, RequestId, TaskId},
    require_version,
    trace::AuthorityKind,
    ContractError,
};

/// Actor identity resolved by a host adapter, carried independently from an
/// effect request. Caller-provided request fields never authenticate a role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookActorContext {
    Authenticated(AuthorityKind),
    Unavailable,
    Conflicting,
}

impl HookActorContext {
    /// Sage and Oracle are read-only authorities; Alchemist receives no
    /// additional permissions from its role.
    pub fn requires_read_only_effects(self) -> bool {
        matches!(
            self,
            Self::Authenticated(AuthorityKind::Sage | AuthorityKind::Oracle)
        )
    }

    pub fn permits_effect(self, effect_class: EffectClass) -> bool {
        !self.requires_read_only_effects() || effect_class == EffectClass::MCP_KNOWN_OBSERVATION
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum EffectClass {
    FILE_WRITE,
    FILE_DELETE,
    FILE_MOVE,
    COMMAND_EXEC,
    NETWORK_EGRESS,
    PROCESS_SPAWN,
    CREDENTIAL_ACCESS,
    DEPENDENCY_INSTALL,
    VCS_COMMIT,
    VCS_PUSH,
    PUBLISH,
    /// A write, send, or delete *positively identified* through an MCP
    /// tool's name or operation. This is intentionally distinct from
    /// ordinary filesystem/network classes so the Guard can keep the newly
    /// covered external-tool surface denied by the canonical default policy.
    EXTERNAL_SIDE_EFFECT,
    /// MCP tool lacking a positive classification signal. Kept distinct from
    /// `EXTERNAL_SIDE_EFFECT` so receipts report observed uncertainty truthfully.
    /// Canonical policy denies this class by default: unknown tools fail closed
    /// until an operator supplies a narrower policy rule or classification.
    /// Only hook fallback logic assigns this class; caller-supplied class strings
    /// cannot claim unclassified treatment.
    MCP_UNCLASSIFIED_OBSERVATION,
    /// A tool the Guard positively recognises as read-only (MCP or host).
    ///
    /// Distinct from `MCP_UNCLASSIFIED_OBSERVATION`, which means "we do not
    /// know what this does" and must stay denied. A named tool that only
    /// reads is ordinary work, and denying it stopped that work with no way
    /// to proceed. The allowlist lives in the hook and is deliberately tiny:
    /// membership is a claim the Guard makes about a specific tool, never a
    /// verb pattern a third-party server can match by naming itself well.
    MCP_KNOWN_OBSERVATION,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalRequirement {
    None,
    User,
    Authority,
}

fn default_operations() -> Vec<String> {
    vec!["*".into()]
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub id: String,
    pub effect_class: EffectClass,
    pub allowed: bool,
    pub approval: ApprovalRequirement,
    pub targets: Vec<String>,
    /// Operation-level discrimination within one effect class (e.g. bounded
    /// vs. destructive FILE_DELETE). Matched the same way as `targets`:
    /// exact string or "*". Defaults to `["*"]` so existing rules and
    /// fixtures written before this field existed keep matching every
    /// operation, unchanged.
    #[serde(default = "default_operations")]
    pub operations: Vec<String>,
    pub required_trust: Option<String>,
    pub required_enforcement: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyPack {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub id: String,
    pub version: u32,
    pub rules: Vec<PolicyRule>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl PolicyPack {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_version(self.schema_version, 1)?;
        let mut ids = std::collections::BTreeSet::new();
        if self.id.trim().is_empty() {
            return Err(ContractError::InvalidContract {
                path: "id".into(),
                reason: "must be non-empty".into(),
            });
        }
        if self.version == 0 {
            return Err(ContractError::InvalidContract {
                path: "version".into(),
                reason: "must be positive".into(),
            });
        }
        for rule in &self.rules {
            require_version(rule.schema_version, 1)?;
            if !ids.insert(&rule.id) {
                return Err(ContractError::InvalidContract {
                    path: "rules.id".into(),
                    reason: "duplicate rule id".into(),
                });
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, crate::canonical::CanonicalError> {
        canonical_digest(self)
    }
}

fn rule(id: &str, effect_class: EffectClass, allowed: bool, operations: &[&str]) -> PolicyRule {
    PolicyRule {
        schema_version: 1,
        id: id.into(),
        effect_class,
        allowed,
        approval: ApprovalRequirement::None,
        targets: vec!["*".into()],
        operations: operations.iter().map(|value| value.to_string()).collect(),
        required_trust: None,
        required_enforcement: Vec::new(),
    }
}

/// The Guard's built-in default policy. This pack is always present in the
/// normal installed state — ambient permission is an explicit policy
/// decision here, never the absence of policy.
///
/// The line this pack draws is destruction, not caution. A push, a publish, a
/// dependency install, an outbound request, a spawned process: none of them
/// destroy anything, and all of them are ordinary work in this workspace.
/// Denying them by default did not make the operator safer — it stopped the
/// work and offered no way to proceed, which is how a guard stops being one.
/// What stays denied is what cannot be undone or what leaks: a destructive
/// delete, a history-rewriting push, credential material, and a positively
/// classified external MCP write/send/delete. Those are refusals rather than prompts on
/// purpose: an approval an operator grants without reading is not a control,
/// and the rare legitimate rewrite is better done by hand.
///
/// `FILE_DELETE` gets two rules instead of one class-level decision:
/// ordinary/bounded deletes (`operations: ["*"]`) stay ambient-allowed so a
/// normal source-file removal in a refactor needs no approval, while a
/// second rule reserves specific destructive-delete operation tags for
/// denial once the hook adapter can distinguish them. Today the adapter
/// (the hook binary's `default_operation`) always tags `FILE_DELETE` as
/// `"delete"` unless a caller supplies an explicit operation, and raw-shell
/// recursive/broad deletes are already hard-gated earlier in dispatch — so
/// no request currently reaches this pack carrying one of the reserved
/// operation tags below. Wiring the adapter to emit them for a genuinely
/// destructive delete path is future work; this ships the schema capability
/// (the `operations` field plus the two-rule shape) now.
///
/// `MCP_UNCLASSIFIED_OBSERVATION` stays ambient under the host's permissions.
/// Its separate class preserves uncertainty in receipts; an allow does not
/// certify the tool as read-only. Projects may deny specific targets or operations.
pub fn canonical_default_policy_pack() -> PolicyPack {
    PolicyPack {
        schema_version: 1,
        id: "canonical-default".into(),
        version: 1,
        rules: vec![
            rule(
                "default-file-write-allow",
                EffectClass::FILE_WRITE,
                true,
                &["*"],
            ),
            rule(
                "default-file-move-allow",
                EffectClass::FILE_MOVE,
                true,
                &["*"],
            ),
            rule(
                "default-vcs-commit-allow",
                EffectClass::VCS_COMMIT,
                true,
                &["*"],
            ),
            rule(
                "default-command-exec-allow",
                EffectClass::COMMAND_EXEC,
                true,
                &["*"],
            ),
            rule(
                "default-file-delete-ordinary-allow",
                EffectClass::FILE_DELETE,
                true,
                &["*"],
            ),
            rule(
                "default-file-delete-destructive-deny",
                EffectClass::FILE_DELETE,
                false,
                &["delete-recursive", "delete-force", "delete-broad"],
            ),
            rule(
                "default-mcp-known-observation-allow",
                EffectClass::MCP_KNOWN_OBSERVATION,
                true,
                &["*"],
            ),
            rule(
                "default-credential-access-deny",
                EffectClass::CREDENTIAL_ACCESS,
                false,
                &["*"],
            ),
            rule("default-publish-allow", EffectClass::PUBLISH, true, &["*"]),
            // A push adds commits to a remote. It destroys nothing, and it is
            // ordinary work in every repository here; denying it stopped that
            // work with no way to proceed.
            rule(
                "default-vcs-push-allow",
                EffectClass::VCS_PUSH,
                true,
                &["*"],
            ),
            rule(
                "default-dependency-install-allow",
                EffectClass::DEPENDENCY_INSTALL,
                true,
                &["*"],
            ),
            rule(
                "default-network-egress-allow",
                EffectClass::NETWORK_EGRESS,
                true,
                &["*"],
            ),
            rule(
                "default-process-spawn-allow",
                EffectClass::PROCESS_SPAWN,
                true,
                &["*"],
            ),
            rule(
                "default-external-side-effect-deny",
                EffectClass::EXTERNAL_SIDE_EFFECT,
                false,
                &["*"],
            ),
            // A tool missing from our classification table still follows host
            // permissions. Retain its unclassified identity in the receipt.
            rule(
                "default-mcp-unclassified-observation-allow",
                EffectClass::MCP_UNCLASSIFIED_OBSERVATION,
                true,
                &["*"],
            ),
        ],
        extensions: BTreeMap::new(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectRequest {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    pub schema_version: u32,
    pub request_id: RequestId,
    pub task_id: TaskId,
    pub requested_by: AgentId,
    pub effect_class: EffectClass,
    pub target: String,
    pub operation: String,
    pub preview: Option<String>,
    pub source_revision: String,
    pub approval_required: bool,
}

impl EffectRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_version(self.schema_version, 1)?;
        for (path, value) in [
            ("target", &self.target),
            ("operation", &self.operation),
            ("source_revision", &self.source_revision),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::InvalidContract {
                    path: path.into(),
                    reason: "must be non-empty".into(),
                });
            }
        }
        Ok(())
    }
}
