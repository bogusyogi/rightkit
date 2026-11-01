//! ACP permission broker.
//!
//! PR 7 of the architecture plan. The broker sits between the
//! streaming layer and any potential executor. When the server emits
//! a `ToolCall` update, the broker asks:
//!
//! > Is this call routed through the attested the host app MCP server,
//! > and is the resolved tool name in the session's tool ceiling?
//!
//! The answer drives the [`AcpPermissionDecision`] the executor
//! surfaces to the supervisor. The default is to deny native
//! execution; only `host app` MCP server calls that match the
//! session's tool ceiling earn an `AllowCodeRightMcp` verdict.
//!
//! The broker does not classify tools by display title. It only
//! looks at:
//!
//! - the *server identity* the server attached to the call (the
//!   `server_name` field);
//! - the resolved *tool name*;
//! - the session-scoped tool ceiling configured by the harness;
//! - the [`CliDelegationPolicy`] the parent session installed.
//!
//! §9.3 of the architecture plan is implemented here.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::driver_host::cli_runtime::ids::CliProviderId;
use crate::driver_host::cli_runtime::policy::CliDelegationPolicy;

/// Outcome of an ACP permission request. The default is to deny
/// native tools and allow only the host app MCP calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpPermissionDecision {
    AllowHostMcp,
    DenyNativeTool { reason: String },
}

impl AcpPermissionDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::DenyNativeTool {
            reason: reason.into(),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, AcpPermissionDecision::AllowHostMcp)
    }
}

impl std::fmt::Display for AcpPermissionDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpPermissionDecision::AllowHostMcp => write!(f, "allow rightkit-agent-mcp"),
            AcpPermissionDecision::DenyNativeTool { reason } => {
                write!(f, "deny native tool ({reason})")
            }
        }
    }
}

/// The information the broker needs about a single tool call. The
/// transport assembles one of these for every `ToolCall` body it
/// decodes; the broker never inspects raw JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub server_name: Option<String>,
    pub arguments: Value,
}

/// What the broker knows about the session it is gating. The
/// transport hands the broker one of these per turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpPermissionContext {
    pub provider_id: CliProviderId,
    pub vendor_session_id: String,
    /// The MCP server name the launch spec pinned as the *only*
    /// allowed server. The transport refuses to launch unless this
    /// is exactly `host app` (or whatever the harness chose), and
    /// the broker refuses any call whose `server_name` does not
    /// match.
    pub attested_mcp_server_name: String,
    /// Tools the model is allowed to invoke on the the host app MCP
    /// server. Anything outside this set is denied, because the
    /// harness owns the tool inventory and the broker must not
    /// invent extra capabilities.
    pub tool_ceiling: BTreeSet<String>,
    /// Session delegation policy. Used to confirm that the provider
    /// itself is allowed in this session; the broker always
    /// defaults to deny when the provider is not in the allowed
    /// set.
    pub delegation_policy: CliDelegationPolicy,
}

impl AcpPermissionContext {
    pub fn new(
        provider_id: CliProviderId,
        vendor_session_id: impl Into<String>,
        attested_mcp_server_name: impl Into<String>,
        tool_ceiling: BTreeSet<String>,
        delegation_policy: CliDelegationPolicy,
    ) -> Self {
        Self {
            provider_id,
            vendor_session_id: vendor_session_id.into(),
            attested_mcp_server_name: attested_mcp_server_name.into(),
            tool_ceiling,
            delegation_policy,
        }
    }
}

/// The broker. Stateless on purpose: the transport owns a single
/// instance per session and feeds it the [`AcpPermissionContext`] on
/// every call.
#[derive(Debug, Default, Clone)]
pub struct AcpPermissionBroker {
    _private: (),
}

impl AcpPermissionBroker {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Decide whether to allow a tool call. The function is total:
    /// it always returns either `AllowCodeRightMcp` or
    /// `DenyNativeTool { reason }`. Denials include a human-readable
    /// reason so the executor can surface it to the model.
    pub fn decide(
        &self,
        context: &AcpPermissionContext,
        call: &AcpToolCall,
    ) -> AcpPermissionDecision {
        // Gate 1 — provider must be allowed by the session's
        // delegation policy. The transport should never reach the
        // broker with a non-allowed provider, but the broker
        // re-checks anyway because provider membership is the only
        // signal that the harness actually invited this runtime.
        if !context.delegation_policy.allowed_child_providers.is_empty()
            && !context
                .delegation_policy
                .allowed_child_providers
                .contains(&context.provider_id)
        {
            return AcpPermissionDecision::deny(format!(
                "provider {:?} is not in the session's allowed_child_providers",
                context.provider_id
            ));
        }

        // Gate 2 — server identity must match the attested
        // the host app MCP server. Anything else is a native tool the
        // server is trying to slip past the harness.
        match &call.server_name {
            Some(server_name) if server_name == &context.attested_mcp_server_name => {}
            Some(other) => {
                return AcpPermissionDecision::deny(format!(
                    "tool call routed through server {other:?} \
                     instead of the attested Host MCP server {:?}",
                    context.attested_mcp_server_name
                ));
            }
            None => {
                return AcpPermissionDecision::deny(
                    "tool call carries no server_name; \
                     the harness cannot verify the call is routed through Host MCP",
                );
            }
        }

        // Gate 3 — resolved tool name must be in the session's
        // tool ceiling. The harness owns the tool inventory; the
        // broker never invents a capability.
        if !context.tool_ceiling.contains(&call.tool_name) {
            return AcpPermissionDecision::deny(format!(
                "tool {:?} is not in the session's tool ceiling",
                call.tool_name
            ));
        }

        // Gate 4 — deny by category when the call claims to be a
        // native shell, file, browser, or delegation tool even
        // though the server_name says otherwise. The broker errs on
        // the side of denial because the architecture plan forbids
        // trusting vendor-native tools.
        if is_native_tool_alias(&call.tool_name) {
            return AcpPermissionDecision::deny(format!(
                "tool {:?} is a vendor-native alias; the harness denies native tools",
                call.tool_name
            ));
        }

        AcpPermissionDecision::AllowHostMcp
    }
}

/// Returns true for tool names that look like vendor-native shell,
/// file, browser, or delegation surfaces even when the server_name
/// has been spoofed. Used as the last gate before granting.
fn is_native_tool_alias(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("mcp.") {
        return matches!(
            lower.as_str(),
            "mcp.shell"
                | "mcp.bash"
                | "mcp.exec"
                | "mcp.run_command"
                | "mcp.browser"
                | "mcp.fetch"
                | "mcp.web_search"
                | "mcp.delegate"
        );
    }
    let aliases = [
        "shell",
        "bash",
        "exec",
        "run_command",
        "fs.read_text_file",
        "fs.write_text_file",
        "read_file",
        "write_file",
        "browser",
        "fetch",
        "web_search",
        "team.create",
        "subagent.spawn",
        "delegate",
    ];
    aliases
        .iter()
        .any(|alias| lower == *alias || lower.ends_with(&format!(".{alias}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(allowed: &[&str], ceiling: &[&str]) -> AcpPermissionContext {
        let allowed_set: BTreeSet<CliProviderId> =
            allowed.iter().map(|s| CliProviderId::new(*s)).collect();
        let mut ceiling_set = BTreeSet::new();
        for tool in ceiling {
            ceiling_set.insert((*tool).to_string());
        }
        AcpPermissionContext::new(
            CliProviderId::new("grok"),
            "v-1",
            "rightkit-agent",
            ceiling_set,
            CliDelegationPolicy {
                allowed_child_providers: allowed_set,
                ..CliDelegationPolicy::default()
            },
        )
    }

    fn call(server: Option<&str>, tool: &str) -> AcpToolCall {
        AcpToolCall {
            call_id: "c-1".into(),
            tool_name: tool.into(),
            server_name: server.map(str::to_string),
            arguments: json!({}),
        }
    }

    /// A well-formed call routed through `host app` MCP must be
    /// allowed.
    #[test]
    fn grant_allows_host_mcp_tool_in_ceiling() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["grok"], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(Some("rightkit-agent"), "mcp.read_file"));
        assert!(decision.is_allowed(), "decision was {decision:?}");
    }

    /// A call routed through a server other than `host app` is
    /// denied — the harness cannot guarantee the call is going
    /// through MCP.
    #[test]
    fn deny_when_server_name_mismatches_attested_mcp() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["grok"], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(Some("filesystem"), "mcp.read_file"));
        match decision {
            AcpPermissionDecision::DenyNativeTool { reason } => {
                assert!(reason.contains("filesystem"));
                assert!(reason.contains("rightkit-agent"));
            }
            other => panic!("expected DenyNativeTool, got {other:?}"),
        }
    }

    /// A call that omits the server identity entirely is denied
    /// because the harness cannot verify its routing.
    #[test]
    fn deny_when_server_name_is_missing() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["grok"], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(None, "mcp.read_file"));
        assert!(matches!(
            decision,
            AcpPermissionDecision::DenyNativeTool { .. }
        ));
    }

    /// A well-routed call whose tool is outside the ceiling is
    /// denied; the harness owns the tool inventory.
    #[test]
    fn deny_when_tool_not_in_ceiling() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["grok"], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(Some("rightkit-agent"), "mcp.delete_file"));
        match decision {
            AcpPermissionDecision::DenyNativeTool { reason } => {
                assert!(reason.contains("not in the session"));
            }
            other => panic!("expected DenyNativeTool, got {other:?}"),
        }
    }

    /// A call from a provider that is not in the allowed set is
    /// denied before any other gate.
    #[test]
    fn deny_when_provider_not_in_allowed_set() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["cursor"], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(Some("rightkit-agent"), "mcp.read_file"));
        match decision {
            AcpPermissionDecision::DenyNativeTool { reason } => {
                assert!(reason.contains("not in the session's allowed_child_providers"));
            }
            other => panic!("expected DenyNativeTool, got {other:?}"),
        }
    }

    /// Native aliases are denied even when the server_name looks
    /// correct. The broker errs on the side of denial.
    #[test]
    fn deny_when_tool_name_looks_native() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&["grok"], &["mcp.shell", "mcp.fs.read_text_file"]);
        let decision = broker.decide(&ctx, &call(Some("rightkit-agent"), "mcp.shell"));
        match decision {
            AcpPermissionDecision::DenyNativeTool { reason } => {
                assert!(reason.contains("native alias"));
            }
            other => panic!("expected DenyNativeTool, got {other:?}"),
        }
    }

    /// When the delegation policy is empty, every provider is allowed
    /// (the harness did not pin any constraint). The broker still
    /// gates on server name and tool ceiling.
    #[test]
    fn empty_allowed_set_does_not_deny_any_provider() {
        let broker = AcpPermissionBroker::new();
        let ctx = context(&[], &["mcp.read_file"]);
        let decision = broker.decide(&ctx, &call(Some("rightkit-agent"), "mcp.read_file"));
        assert!(decision.is_allowed(), "decision was {decision:?}");
    }

    /// A `DenyNativeTool` decision must surface a reason string for
    /// the executor to log.
    #[test]
    fn deny_carries_a_reason_string() {
        let d = AcpPermissionDecision::deny("native file read not allowed");
        match d {
            AcpPermissionDecision::DenyNativeTool { reason } => {
                assert!(reason.contains("not allowed"));
            }
            other => panic!("expected DenyNativeTool, got {:?}", other),
        }
    }

    /// The `is_allowed` accessor must agree with the variant.
    #[test]
    fn is_allowed_accessor_matches_variant() {
        assert!(AcpPermissionDecision::AllowHostMcp.is_allowed());
        assert!(!AcpPermissionDecision::DenyNativeTool { reason: "x".into() }.is_allowed());
    }

    /// The native-tool alias check must cover shell / file /
    /// browser / delegation names.
    #[test]
    fn native_alias_check_recognizes_known_aliases() {
        for alias in [
            "shell",
            "BASH",
            "exec",
            "fs.read_text_file",
            "team.create",
            "browser",
            "fetch",
            "delegate",
            "mcp.shell",
        ] {
            assert!(
                is_native_tool_alias(alias),
                "expected {alias} to be flagged"
            );
        }
        assert!(!is_native_tool_alias("mcp.read_file"));
        assert!(!is_native_tool_alias("mcp.write_file"));
    }
}
