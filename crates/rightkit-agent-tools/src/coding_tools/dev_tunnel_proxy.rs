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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::tunnel::{
        configure_tunnel_credential, TunnelHandle, TunnelProvider, TunnelProviderConfig,
        TunnelStatus,
    };
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    struct StubTunnelProvider {
        starts: AtomicU32,
        stops: AtomicU32,
        start_error: Option<&'static str>,
    }

    impl StubTunnelProvider {
        fn working() -> Self {
            Self {
                starts: AtomicU32::new(0),
                stops: AtomicU32::new(0),
                start_error: None,
            }
        }

        fn failing() -> Self {
            Self {
                starts: AtomicU32::new(0),
                stops: AtomicU32::new(0),
                start_error: Some("stub provider: no live account"),
            }
        }
    }

    #[async_trait::async_trait]
    impl TunnelProvider for StubTunnelProvider {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn description(&self) -> &'static str {
            "In-memory provider for dev tunnel proxy tests."
        }

        async fn start(
            &self,
            config: &TunnelProviderConfig,
            request: &TunnelStartRequest,
        ) -> Result<TunnelHandle, String> {
            if let Some(error) = self.start_error {
                return Err(error.to_string());
            }
            assert_eq!(config.token, "test-token");
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(TunnelHandle {
                provider: self.name().to_string(),
                external_id: format!("dev-{}", request.local_port),
                public_hostname: Some("dev.example.test".to_string()),
                connector_token: None,
            })
        }

        async fn stop(
            &self,
            config: &TunnelProviderConfig,
            handle: &TunnelHandle,
        ) -> Result<(), String> {
            let _ = handle;
            assert_eq!(config.token, "test-token");
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn status(
            &self,
            config: &TunnelProviderConfig,
            handle: &TunnelHandle,
        ) -> Result<TunnelStatus, String> {
            let _ = (config, handle);
            Ok(TunnelStatus::Active)
        }
    }

    fn context(session_id: &str) -> ToolContext {
        ToolContext::new(
            session_id,
            ".",
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    fn tool(provider: Arc<StubTunnelProvider>) -> DevTunnelProxyTool {
        let mut registry = TunnelRegistry::new();
        registry.register(provider);
        DevTunnelProxyTool::with_registry(registry)
    }

    fn configure(session_id: &str) {
        configure_tunnel_credential(
            session_id,
            "stub",
            TunnelProviderConfig {
                token: "test-token".to_string(),
                account_id: None,
            },
        );
    }

    fn loopback_listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind loopback test listener")
    }

    #[tokio::test]
    async fn start_and_stop_round_trip_through_the_provider() {
        let provider = Arc::new(StubTunnelProvider::working());
        let proxy = tool(provider.clone());
        let session_id = "dev-tunnel-proxy-round-trip";
        configure(session_id);
        let ctx = context(session_id);
        let listener = loopback_listener();
        let port = listener.local_addr().expect("listener address").port();

        let started = proxy
            .execute(
                json!({"action": "start", "provider": "stub", "local_port": port}),
                &ctx,
            )
            .await;
        assert!(!started.is_error, "{}", started.content);
        assert_eq!(provider.starts.load(Ordering::SeqCst), 1);

        let stopped = proxy
            .execute(json!({"action": "stop", "provider": "stub"}), &ctx)
            .await;
        assert!(!stopped.is_error, "{}", stopped.content);
        assert_eq!(provider.stops.load(Ordering::SeqCst), 1);

        crate::coding_tools::tunnel::retire_tunnel_session(session_id);
    }

    #[tokio::test]
    async fn provider_error_is_returned_unchanged() {
        let provider = Arc::new(StubTunnelProvider::failing());
        let proxy = tool(provider);
        let session_id = "dev-tunnel-proxy-provider-error";
        configure(session_id);
        let listener = loopback_listener();
        let port = listener.local_addr().expect("listener address").port();
        let result = proxy
            .execute(
                json!({"action": "start", "provider": "stub", "local_port": port}),
                &context(session_id),
            )
            .await;

        assert!(result.is_error);
        assert_eq!(result.content, "stub provider: no live account");
        crate::coding_tools::tunnel::retire_tunnel_session(session_id);
    }

    #[tokio::test]
    async fn stop_is_idempotent_after_the_provider_stops_the_tunnel() {
        let provider = Arc::new(StubTunnelProvider::working());
        let proxy = tool(provider.clone());
        let session_id = "dev-tunnel-proxy-idempotent-stop";
        configure(session_id);
        let ctx = context(session_id);
        let listener = loopback_listener();
        let port = listener.local_addr().expect("listener address").port();

        proxy
            .execute(
                json!({"action": "start", "provider": "stub", "local_port": port}),
                &ctx,
            )
            .await;
        let first_stop = proxy
            .execute(json!({"action": "stop", "provider": "stub"}), &ctx)
            .await;
        let second_stop = proxy
            .execute(json!({"action": "stop", "provider": "stub"}), &ctx)
            .await;

        assert!(!first_stop.is_error, "{}", first_stop.content);
        assert!(!second_stop.is_error, "{}", second_stop.content);
        assert_eq!(provider.stops.load(Ordering::SeqCst), 1);
        crate::coding_tools::tunnel::retire_tunnel_session(session_id);
    }

    #[tokio::test]
    async fn start_refuses_non_loopback_target_before_calling_provider() {
        let provider = Arc::new(StubTunnelProvider::working());
        let proxy = tool(provider.clone());
        let session_id = "dev-tunnel-proxy-non-loopback";
        configure(session_id);
        let result = proxy
            .execute(
                json!({
                    "action": "start",
                    "provider": "stub",
                    "local_host": "10.0.0.2",
                    "local_port": 4173
                }),
                &context(session_id),
            )
            .await;

        assert!(result.is_error);
        assert_eq!(provider.starts.load(Ordering::SeqCst), 0);
        crate::coding_tools::tunnel::retire_tunnel_session(session_id);
    }
}
