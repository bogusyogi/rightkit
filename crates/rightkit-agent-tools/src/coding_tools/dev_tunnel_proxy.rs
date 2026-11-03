//! Dev-server tunnel proxy tool (ENG-103).
//!
//! Provider credentials and connector deployment remain private inputs. This
//! tool forwards provider errors unchanged instead of inventing exposure
//! success; proxied bytes are provider-owned and never content-rewritten.
//!
//! The provider owns the tunnel implementation. This module only coordinates
//! the provider registry, session credential lookup, and the returned handle;
//! it does not introduce another tunnel abstraction or rewrite the proxied
//! dev-server traffic.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use crate::coding_tools::tunnel::{
    dev_tunnel_connector_running, probe_local_target, record_exposure, start_dev_tunnel_connector,
    stop_dev_tunnel_connector, take_exposure, tunnel_credential, TunnelRegistry,
    TunnelStartRequest,
};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// A tool that exposes a local development server through a registered tunnel
/// provider while retaining the provider handle needed for a later stop.
pub struct DevTunnelProxyTool {
    registry: TunnelRegistry,
    active: Mutex<HashMap<(String, String), crate::coding_tools::tunnel::TunnelHandle>>,
}

impl DevTunnelProxyTool {
    /// Construct the proxy with the built-in provider registry so production
    /// calls use the same provider surface as the general tunnel tool.
    pub fn new() -> Self {
        Self::with_registry(TunnelRegistry::new())
    }

    /// Construct the proxy with an explicit registry so callers and tests can
    /// select a registered provider without replacing the provider trait.
    pub fn with_registry(registry: TunnelRegistry) -> Self {
        Self {
            registry,
            active: Mutex::new(HashMap::new()),
        }
    }

    fn active_key(session_id: &str, provider: &str) -> (String, String) {
        (session_id.to_string(), provider.to_string())
    }

    fn take_active(
        &self,
        session_id: &str,
        provider: &str,
    ) -> Option<crate::coding_tools::tunnel::TunnelHandle> {
        self.active
            .lock()
            .expect("dev tunnel exposure lock")
            .remove(&Self::active_key(session_id, provider))
    }

    fn active_handle(
        &self,
        session_id: &str,
        provider: &str,
    ) -> Option<crate::coding_tools::tunnel::TunnelHandle> {
        self.active
            .lock()
            .expect("dev tunnel exposure lock")
            .get(&Self::active_key(session_id, provider))
            .cloned()
    }

    fn has_active(&self, session_id: &str, provider: &str) -> bool {
        self.active
            .lock()
            .expect("dev tunnel exposure lock")
            .contains_key(&Self::active_key(session_id, provider))
    }

    fn remember_active(
        &self,
        session_id: &str,
        provider: &str,
        handle: crate::coding_tools::tunnel::TunnelHandle,
    ) {
        self.active
            .lock()
            .expect("dev tunnel exposure lock")
            .insert(Self::active_key(session_id, provider), handle);
    }
}

impl Default for DevTunnelProxyTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for DevTunnelProxyTool {
    fn name(&self) -> &str {
        "dev_tunnel_proxy"
    }

    fn description(&self) -> &str {
        "Expose a local development server through a registered tunnel provider and return the provider's public handle."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action", "provider"],
            "properties": {
                "action": {"type": "string", "enum": ["start", "stop", "status"]},
                "provider": {"type": "string", "description": "TunnelRegistry provider name"},
                "local_host": {"type": "string", "default": "127.0.0.1"},
                "local_port": {"type": "integer", "description": "Local development server port"},
                "label": {"type": "string"},
                "public_hostname": {"type": "string", "description": "Already configured public DNS hostname for this tunnel"}
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Network)
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(value) => value,
            None => return ToolResult::error("Missing required argument: action"),
        };
        let provider_name = match args.get("provider").and_then(Value::as_str) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => return ToolResult::error("Missing required argument: provider"),
        };
        let provider = match self.registry.get(&provider_name) {
            Some(value) => value,
            None => return ToolResult::error(format!("Unknown tunnel provider: {provider_name}")),
        };

        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::Network,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            false,
        );
        match verdict.decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Ask => {
                return ToolResult::error(format!("Tool requires approval: {}", verdict.reason))
            }
            PermissionDecision::Deny => {
                return ToolResult::error(format!("Permission denied: {}", verdict.reason))
            }
        }

        match action {
            "start" => {
                self.start(&provider_name, provider.as_ref(), &args, ctx)
                    .await
            }
            "stop" => self.stop(&provider_name, provider.as_ref(), ctx).await,
            "status" => self.status(&provider_name, provider.as_ref(), ctx).await,
            other => ToolResult::error(format!("Unsupported dev tunnel action: {other}")),
        }
    }
}

impl DevTunnelProxyTool {
    async fn start(
        &self,
        provider_name: &str,
        provider: &dyn crate::coding_tools::tunnel::TunnelProvider,
        args: &Value,
        ctx: &ToolContext,
    ) -> ToolResult {
        if self.has_active(&ctx.session_id, provider_name) {
            return ToolResult::error(format!(
                "A {provider_name} development tunnel is already active in this session."
            ));
        }
        let config = match tunnel_credential(&ctx.session_id, provider_name) {
            Some(value) => value,
            None => {
                return ToolResult::error(format!(
                    "No managed token configured for {provider_name} in this session; run action=configure first."
                ))
            }
        };
        let local_port = match args.get("local_port").and_then(Value::as_u64) {
            Some(value) if value > 0 && value <= u16::MAX as u64 => value as u16,
            _ => return ToolResult::error("Missing or invalid required argument: local_port"),
        };
        let local_host = args
            .get("local_host")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1");
        if local_host.trim().is_empty() {
            return ToolResult::error("local_host must not be empty");
        }
        if let Err(error) = probe_local_target(local_host, local_port) {
            return ToolResult::error(error);
        }
        let request = TunnelStartRequest {
            local_host: local_host.to_string(),
            local_port,
            label: args
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
            public_hostname: args
                .get("public_hostname")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
        };

        match provider.start(&config, &request).await {
            Ok(handle) => {
                if provider_name == "cloudflare_tunnel" {
                    if let Err(error) = start_dev_tunnel_connector(
                        &ctx.session_id,
                        provider_name,
                        &handle,
                        &ctx.workspace_root,
                    )
                    .await
                    {
                        let (message, still_exposed) =
                            crate::coding_tools::tunnel::rollback_failed_start(
                                provider,
                                &config,
                                &ctx.session_id,
                                provider_name,
                                &handle,
                                error,
                            )
                            .await;
                        if still_exposed {
                            self.remember_active(&ctx.session_id, provider_name, handle.clone());
                            record_exposure(&ctx.session_id, provider_name, handle, false);
                        }
                        return ToolResult::error(message);
                    }
                }
                self.remember_active(&ctx.session_id, provider_name, handle.clone());
                // Session teardown owns the same resource even if this tool
                // instance is gone before its explicit stop action.
                record_exposure(&ctx.session_id, provider_name, handle.clone(), false);
                ToolResult::ok(
                    json!({
                        "event": "dev_tunnel_started",
                        "provider": handle.provider,
                        "external_id": handle.external_id,
                        "public_hostname": handle.public_hostname,
                        "target": format!("{}:{}", request.local_host, request.local_port),
                    })
                    .to_string(),
                )
            }
            Err(error) => ToolResult::error(error),
        }
    }

    async fn stop(
        &self,
        provider_name: &str,
        provider: &dyn crate::coding_tools::tunnel::TunnelProvider,
        ctx: &ToolContext,
    ) -> ToolResult {
        if !self.has_active(&ctx.session_id, provider_name) {
            return ToolResult::ok(
                json!({"event": "dev_tunnel_stopped", "provider": provider_name, "status": "stopped"}).to_string(),
            );
        }
        let config = match tunnel_credential(&ctx.session_id, provider_name) {
            Some(value) => value,
            None => {
                return ToolResult::error(format!(
                    "No managed token configured for {provider_name} in this session."
                ))
            }
        };
        let handle = match self.take_active(&ctx.session_id, provider_name) {
            Some(value) => value,
            None => {
                return ToolResult::ok(
                    json!({"event": "dev_tunnel_stopped", "provider": provider_name, "status": "stopped"}).to_string(),
                )
            }
        };
        match provider.stop(&config, &handle).await {
            Ok(()) => match stop_dev_tunnel_connector(&ctx.session_id, provider_name).await {
                Ok(()) => {
                    let _ = take_exposure(&ctx.session_id, provider_name);
                    ToolResult::ok(
                        json!({
                            "event": "dev_tunnel_stopped",
                            "provider": provider_name,
                            "status": "stopped",
                        })
                        .to_string(),
                    )
                }
                Err(error) => {
                    self.remember_active(&ctx.session_id, provider_name, handle);
                    ToolResult::error(error)
                }
            },
            Err(error) => {
                self.remember_active(&ctx.session_id, provider_name, handle);
                ToolResult::error(error)
            }
        }
    }

    async fn status(
        &self,
        provider_name: &str,
        provider: &dyn crate::coding_tools::tunnel::TunnelProvider,
        ctx: &ToolContext,
    ) -> ToolResult {
        let handle = match self.active_handle(&ctx.session_id, provider_name) {
            Some(value) => value,
            None => {
                return ToolResult::ok(
                    json!({"provider": provider_name, "status": "stopped"}).to_string(),
                )
            }
        };
        let config = match tunnel_credential(&ctx.session_id, provider_name) {
            Some(value) => value,
            None => {
                return ToolResult::error(format!(
                    "No managed token configured for {provider_name} in this session."
                ))
            }
        };
        match provider.status(&config, &handle).await {
            Ok(status) => match dev_tunnel_connector_running(&ctx.session_id, provider_name) {
                Ok(connector_running) => ToolResult::ok(
                    json!({
                        "provider": provider_name,
                        "event": "dev_tunnel_status",
                        "status": status,
                        "connector_running": connector_running,
                    })
                    .to_string(),
                ),
                Err(error) => ToolResult::error(error),
            },
            Err(error) => ToolResult::error(error),
        }
    }
}
