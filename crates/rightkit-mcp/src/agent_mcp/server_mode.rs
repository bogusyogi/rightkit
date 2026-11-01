//! Trust and audit helpers for the host app's agent-as-MCP-server mode.
//!
//! The binary owns the stdio loop and tool registry; this module keeps the
//! policy decision and per-call audit shape in the MCP crate so it is reusable
//! by every server surface without depending on `host_tools`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::{
    AsyncRequestHandler, McpError, RequestContext, ServerHost, ServerOptions, ToolDescriptor,
    ToolProvider, ToolResult,
};

const MCP_SERVER_MODE_BLOCKED_TOOLS: &[&str] = &[
    "spawn_subagent",
    "send_message",
    "team_create",
    "team_delete",
    "followup_subagent",
    "wait_for_subagents",
    "interrupt_subagent",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum McpServerTrustTier {
    Locked,
    Untrusted,
    Trusted,
    Sandboxed,
}

impl McpServerTrustTier {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "trusted" => Self::Trusted,
            "sandboxed" => Self::Sandboxed,
            "locked" => Self::Locked,
            _ => Self::Untrusted,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Untrusted => "untrusted",
            Self::Trusted => "trusted",
            Self::Sandboxed => "sandboxed",
        }
    }
}

/// Minimal metadata needed to decide whether a the host app tool can be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerToolSummary {
    pub name: String,
    pub read_only: bool,
}

impl McpServerToolSummary {
    pub fn new(name: impl Into<String>, read_only: bool) -> Self {
        Self {
            name: name.into(),
            read_only,
        }
    }
}

/// Policy for which the host app tools are exposed by server mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpServerModePolicy {
    trust_tier: McpServerTrustTier,
    allow_mutate: bool,
}

impl McpServerModePolicy {
    pub fn new(trust_tier: &str, allow_mutate: bool) -> Self {
        Self {
            trust_tier: McpServerTrustTier::parse(trust_tier),
            allow_mutate,
        }
    }

    pub fn trust_tier(&self) -> &'static str {
        self.trust_tier.as_str()
    }

    pub fn allow_mutate(&self) -> bool {
        self.allow_mutate
    }

    pub fn exposes_tool(&self, tool: &McpServerToolSummary) -> bool {
        if self.trust_tier == McpServerTrustTier::Locked {
            return false;
        }
        if MCP_SERVER_MODE_BLOCKED_TOOLS.contains(&tool.name.as_str()) {
            return false;
        }
        if tool.read_only {
            return true;
        }
        self.allow_mutate && self.trust_tier >= McpServerTrustTier::Trusted
    }

    pub fn denial_reason(&self, tool: &McpServerToolSummary) -> Option<String> {
        if self.exposes_tool(tool) {
            return None;
        }
        if self.trust_tier == McpServerTrustTier::Locked {
            return Some("workspace trust is locked; no MCP server tools are exposed".to_string());
        }
        if MCP_SERVER_MODE_BLOCKED_TOOLS.contains(&tool.name.as_str()) {
            return Some(format!(
                "tool '{}' is orchestration-only and unavailable through MCP server mode",
                tool.name
            ));
        }
        if !tool.read_only && !self.allow_mutate {
            return Some(format!(
                "tool '{}' is mutating; restart with --allow-mutate to expose it",
                tool.name
            ));
        }
        Some(format!(
            "tool '{}' requires trusted or sandboxed MCP server trust, current trust is {}",
            tool.name,
            self.trust_tier()
        ))
    }
}

/// One audit row for a tool actually served through MCP server mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServedInvocationAudit {
    pub tool: String,
    pub session: String,
    pub at: String,
    pub outcome: String,
    pub trust_tier: String,
    pub allow_mutate: bool,
}

/// In-memory audit ledger adapter for served MCP invocations.
#[derive(Debug, Clone, Default)]
pub struct McpServerModeGuard {
    policy: McpServerModePolicy,
    audit_rows: Vec<McpServedInvocationAudit>,
}

impl McpServerModeGuard {
    pub fn new(policy: McpServerModePolicy) -> Self {
        Self {
            policy,
            audit_rows: Vec::new(),
        }
    }

    pub fn policy(&self) -> McpServerModePolicy {
        self.policy
    }

    pub fn record_served_invocation(
        &mut self,
        tool: &McpServerToolSummary,
        session: &str,
        at: &str,
    ) -> bool {
        if !self.policy.exposes_tool(tool) {
            return false;
        }
        self.audit_rows.push(McpServedInvocationAudit {
            tool: tool.name.clone(),
            session: session.to_string(),
            at: at.to_string(),
            outcome: "served".to_string(),
            trust_tier: self.policy.trust_tier().to_string(),
            allow_mutate: self.policy.allow_mutate(),
        });
        true
    }

    pub fn audit_rows(&self) -> &[McpServedInvocationAudit] {
        &self.audit_rows
    }
}

impl Default for McpServerModePolicy {
    fn default() -> Self {
        Self::new("untrusted", false)
    }
}

/// Backend supplied by the host app's execution projection to RightKit MCP.
///
/// Tool descriptors remain synchronous, while calls retain their existing
/// asynchronous execution path through RightKit's parsed dispatcher.
pub trait RightKitMcpBackend: Send + Sync {
    fn should_audit(&self, _tool: &str) -> bool {
        true
    }

    fn tools(&self) -> Vec<ToolDescriptor>;

    fn list_tools<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = crate::Result<Value>> + Send + 'a>> {
        Box::pin(async { Ok(json!({"tools": self.tools()})) })
    }

    fn call<'a>(
        &'a self,
        request_id: &'a Value,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = crate::Result<Value>> + Send + 'a>>;
}

struct RightKitToolProvider {
    backend: Arc<dyn RightKitMcpBackend>,
}

impl ToolProvider for RightKitToolProvider {
    fn list(&self) -> Vec<ToolDescriptor> {
        self.backend.tools()
    }

    fn call(&self, name: &str, _args: Value) -> crate::Result<ToolResult> {
        Err(McpError::ToolNotFound(name.to_string()))
    }
}

struct RightKitCallHandler {
    backend: Arc<dyn RightKitMcpBackend>,
    guard: Arc<Mutex<McpServerModeGuard>>,
    policy: McpServerModePolicy,
}

impl AsyncRequestHandler for RightKitCallHandler {
    fn handle<'a>(
        &'a self,
        request: &'a Value,
        context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<crate::Result<Value>>> + Send + 'a>> {
        Box::pin(async move {
            match request.get("method").and_then(Value::as_str) {
                Some("tools/list") => {
                    return Some(self.backend.list_tools().await.map(|mut payload| {
                        if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut)
                        {
                            tools.retain(|tool| {
                                self.policy.exposes_tool(&McpServerToolSummary::new(
                                    tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                                    tool.pointer("/annotations/readOnlyHint")
                                        .and_then(Value::as_bool)
                                        == Some(true),
                                ))
                            });
                        }
                        payload
                    }));
                }
                Some("tools/call") => {}
                Some("initialize" | "ping") => return None,
                Some(method) => {
                    return Some(Err(McpError::Rpc {
                        code: -32601,
                        message: format!("method not found: {method}"),
                    }))
                }
                None => return None,
            }
            let params = match request.get("params") {
                Some(params) => params,
                None => {
                    return Some(Err(McpError::Rpc {
                        code: -32602,
                        message: "missing params".into(),
                    }))
                }
            };
            let name = match params.get("name").and_then(Value::as_str) {
                Some(name) => name,
                None => {
                    return Some(Err(McpError::Rpc {
                        code: -32602,
                        message: "missing params.name".into(),
                    }))
                }
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let request_id = request.get("id").cloned().unwrap_or(Value::Null);
            // Gateway inventories are live; only execution policy is local.
            let listed = match self.backend.list_tools().await {
                Ok(listed) => listed,
                Err(error) => return Some(Err(error)),
            };
            let tool = listed
                .get("tools")
                .and_then(Value::as_array)
                .and_then(|tools| {
                    tools
                        .iter()
                        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                });
            let tool = match tool {
                Some(tool) => tool,
                None => {
                    return Some(Err(McpError::Rpc {
                        code: -32602,
                        message: format!("unknown tool: {name}"),
                    }))
                }
            };
            let summary = McpServerToolSummary::new(
                name,
                tool.pointer("/annotations/readOnlyHint")
                    .and_then(Value::as_bool)
                    == Some(true),
            );
            if let Some(message) = self.policy.denial_reason(&summary) {
                return Some(Err(McpError::Rpc {
                    code: -32603,
                    message,
                }));
            }
            let result = self.backend.call(&request_id, name, arguments).await;
            if result.is_ok() && self.backend.should_audit(name) {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs().to_string())
                    .unwrap_or_else(|_| "0".into());
                let _ = self
                    .guard
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .record_served_invocation(
                        &summary,
                        context.session.as_deref().unwrap_or("stdio"),
                        &at,
                    );
            }
            Some(result)
        })
    }
}

/// RightKit-backed MCP server host used by the host app's stdio ingress.
pub struct RightKitMcpServer {
    host: ServerHost,
    guard: Arc<Mutex<McpServerModeGuard>>,
}

impl RightKitMcpServer {
    pub fn new(
        name: &str,
        version: &str,
        backend: Arc<dyn RightKitMcpBackend>,
        policy: McpServerModePolicy,
    ) -> Self {
        Self::with_guard(
            name,
            version,
            backend,
            Arc::new(Mutex::new(McpServerModeGuard::new(policy))),
        )
    }

    pub fn with_guard(
        name: &str,
        version: &str,
        backend: Arc<dyn RightKitMcpBackend>,
        guard: Arc<Mutex<McpServerModeGuard>>,
    ) -> Self {
        let policy = guard
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .policy();
        let host = ServerHost::new(
            name,
            version,
            Arc::new(RightKitToolProvider {
                backend: backend.clone(),
            }),
        )
        .with_async_handler(Arc::new(RightKitCallHandler {
            backend,
            guard: guard.clone(),
            policy,
        }))
        .with_protocol_versions(
            crate::agent_mcp::transport::MCP_SUPPORTED_VERSIONS
                .iter()
                .map(|v| (*v).into())
                .collect(),
        )
        .with_response_limits(crate::ResponseLimits {
            max_response_bytes: 8 * 1024 * 1024,
            max_frame_bytes: 1024 * 1024,
        })
        .with_options(ServerOptions {
            id_policy: crate::IdPolicy::NullResponse,
            initialized_policy: crate::InitializedPolicy::Ignore,
            ..ServerOptions::default()
        });
        Self { host, guard }
    }

    pub fn host(&self) -> &ServerHost {
        &self.host
    }

    pub fn guard(&self) -> Arc<Mutex<McpServerModeGuard>> {
        self.guard.clone()
    }

    pub fn audit_rows(&self) -> Vec<McpServedInvocationAudit> {
        self.guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .audit_rows()
            .to_vec()
    }

    pub async fn dispatch(&self, request: &Value) -> crate::Result<Option<Value>> {
        self.dispatch_with_context(request, &RequestContext::default())
            .await
    }

    pub async fn dispatch_with_context(
        &self,
        request: &Value,
        context: &RequestContext,
    ) -> crate::Result<Option<Value>> {
        self.host.dispatch_checked_async(request, context).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_mode_keeps_orchestration_outside_mcp() {
        let policy = McpServerModePolicy::new("trusted", true);
        assert!(policy.exposes_tool(&McpServerToolSummary::new("read_file", true)));
        assert!(!policy.exposes_tool(&McpServerToolSummary::new("spawn_subagent", false)));
        assert!(policy
            .denial_reason(&McpServerToolSummary::new("spawn_subagent", false))
            .unwrap()
            .contains("orchestration-only"));
    }
}
