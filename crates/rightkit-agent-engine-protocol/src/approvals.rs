//! Structured approval request contracts ported from Codex's protocol.
//!
//! DIRECT_PORT_RUST (atom `S:I4`): ported from codex `codex-rs/protocol/src/
//! protocol.rs:963` and `approvals.rs` (`ExecApprovalRequestEvent`,
//! `ReviewDecision`, `ExecPolicyAmendment`, network/guardian approval
//! contexts, elicitation requests). San's three-way `PermissionDecision`
//! (`Permit`/`Reject`/`Prompt`) and the `PermissionGateRequest`/
//! `PermissionGateResponse` pair are the same-family the host app seam these
//! Codex types are wired through — see `docs/canon/daemon-master-canon-
//! atoms.md` for the exact donor revision and NOTICE entry. No behavioral
//! changes were made to the ported shapes beyond the module path
//! (`codex_protocol` -> `rightkit_agent_engine_protocol::approvals`).

use rightkit_approvals::Binding;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// San's three-way permission result: permit, reject, or human prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Permit,
    Reject,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDecisionResult {
    pub decision: PermissionDecision,
    pub reason: String,
    pub tool_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub reviewable: bool,
}

/// Structured pending request sent to an operator surface. `tool_call_id`
/// binds response to one concrete invocation; `input` remains untrusted data.
/// `binding` is what the operator approves: the digest of the exact final
/// payload, its destination, expiry and revision (the approvals core's rule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionGateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub description: String,
    pub input: Value,
    pub binding: Binding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionGateResponse {
    pub allow: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecPolicyAmendment {
    pub command: Vec<String>,
}
impl ExecPolicyAmendment {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }
    pub fn command(&self) -> &[String] {
        &self.command
    }
}
impl From<Vec<String>> for ExecPolicyAmendment {
    fn from(command: Vec<String>) -> Self {
        Self { command }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkApprovalProtocol {
    Http,
    Https,
    Socks5Tcp,
    Socks5Udp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyRuleAction {
    Allow,
    Deny,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyRuleAction,
}

/// Exact effect context bound to a pending approval callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApprovalContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub tool_name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub network: Option<NetworkApprovalContext>,
    /// EFF-002, on the approvals core's rule: the digest of the final
    /// post-transform payload shown to the approver (mandatory), the
    /// destination the call reaches, the expiry and the revision. Session
    /// grants bind to [`PendingApprovalContext::payload_digest`].
    pub binding: Binding,
}

/// Prefix of an effect payload digest (`sha256:<hex>`); the binding holds the hex.
pub const EFFECT_DIGEST_PREFIX: &str = "sha256:";

/// The approvals core binding for one tool call: `effect_digest` is the
/// engine's effect payload digest (`sha256:<hex>` or bare hex) of the final
/// payload, `destination` what the call reaches (the network host, else the
/// tool). No expiry of its own (the pending request lives until answered or
/// cancelled) and revision 0.
pub fn effect_binding(effect_digest: &str, destination: &str) -> Binding {
    Binding::new(
        effect_digest
            .strip_prefix(EFFECT_DIGEST_PREFIX)
            .unwrap_or(effect_digest),
        destination,
    )
}

impl PendingApprovalContext {
    /// The effect payload digest in the engine's form, `sha256:<hex>`, the
    /// key session payload grants are stored under.
    pub fn payload_digest(&self) -> String {
        format!("{EFFECT_DIGEST_PREFIX}{}", self.binding.args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardianRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardianUserAuthorization {
    Unknown,
    Low,
    Medium,
    High,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardianAssessmentOutcome {
    Allow,
    Deny,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianAssessmentStatus {
    InProgress,
    Approved,
    Denied,
    TimedOut,
    Aborted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianAssessmentDecisionSource {
    Agent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianCommandSource {
    Shell,
    UnifiedExec,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuardianAssessmentAction {
    Command {
        source: GuardianCommandSource,
        command: String,
        cwd: String,
    },
    Execve {
        source: GuardianCommandSource,
        program: String,
        argv: Vec<String>,
        cwd: String,
    },
    WriteStdin {
        approval_id: String,
        process_id: String,
        stdin: String,
        cwd: String,
    },
    ApplyPatch {
        cwd: String,
        files: Vec<String>,
    },
    NetworkAccess {
        target: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
    },
    McpToolCall {
        server: String,
        tool_name: String,
        connector_id: Option<String>,
        connector_name: Option<String>,
        tool_title: Option<String>,
    },
    RequestPermissions {
        reason: Option<String>,
        permissions: Value,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardianAssessmentEvent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_item_id: Option<String>,
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub status: GuardianAssessmentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<GuardianRiskLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_authorization: Option<GuardianUserAuthorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_source: Option<GuardianAssessmentDecisionSource>,
    pub action: GuardianAssessmentAction,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecApprovalKind {
    #[default]
    Command,
    WriteStdin,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ApprovedForSession,
    ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },
    NetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },
    Abort,
}
impl ReviewDecision {
    pub const fn is_approved(&self) -> bool {
        matches!(
            self,
            Self::Approved
                | Self::ApprovedForSession
                | Self::ApprovedExecpolicyAmendment { .. }
                | Self::NetworkPolicyAmendment {
                    network_policy_amendment: NetworkPolicyAmendment {
                        action: NetworkPolicyRuleAction::Allow,
                        ..
                    }
                }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecApprovalRequestEvent {
    #[serde(default)]
    pub kind: ExecApprovalKind,
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub turn_id: String,
    pub started_at_ms: i64,
    pub command: Vec<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_decisions: Option<Vec<ReviewDecision>>,
}
impl ExecApprovalRequestEvent {
    pub fn effective_approval_id(&self) -> String {
        self.approval_id
            .clone()
            .unwrap_or_else(|| self.call_id.clone())
    }
    pub fn effective_available_decisions(&self) -> Vec<ReviewDecision> {
        self.available_decisions.clone().unwrap_or_else(|| {
            let mut out = vec![ReviewDecision::Approved];
            if let Some(prefix) = &self.proposed_execpolicy_amendment {
                out.push(ReviewDecision::ApprovedExecpolicyAmendment {
                    proposed_execpolicy_amendment: prefix.clone(),
                });
            }
            out.push(ReviewDecision::Abort);
            out
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ElicitationRequest {
    Form {
        message: String,
        requested_schema: Value,
    },
    Url {
        message: String,
        url: String,
        elicitation_id: String,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationRequestEvent {
    pub turn_id: Option<String>,
    pub server_name: String,
    pub id: String,
    pub request: ElicitationRequest,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}
