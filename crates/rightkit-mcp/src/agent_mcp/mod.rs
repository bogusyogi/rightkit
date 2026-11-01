//! host_mcp — Model Context Protocol client layer.
//!
//! Shared transports: one connection per MCP server, multiplexed across all
//! the host app sessions. Stateful HTTP servers receive a `Mcp-Session-Id` header
//! per the host app session for correct state isolation within the shared process.
//!
//! Three transports:
//! - [`HttpTransport`] — Streamable HTTP (supergateway `--outputTransport streamableHttp`)
//! - [`SseTransport`]  — SSE (supergateway default / single-client servers)
//! - [`StdioTransport`] — stdio (spawn once, share across sessions)
//!
//! Config is loaded from `~/.config/host app/mcp.json` (Claude desktop format).

pub mod config;
mod error;
pub mod lsp_bridge;
mod manager;
pub mod oauth;
mod pool;
mod server_mode;
pub mod shared_pool;
mod transport;
pub mod trust_gate;
mod types;

pub use config::{
    attach_stored_oauth_tokens, expand_config_env, expand_server_env, load_for_session,
    load_with_config_path_pool_and_process_launcher, load_with_pool, load_with_trust_gate,
    load_with_trust_gate_and_project_trust, load_with_trust_gate_pooled, ProjectTrust,
};
pub use error::McpError;
pub use lsp_bridge::{
    lsp_mcp_tool_definitions, Diagnostic, GotoDefinitionResult, HoverResult, LspBridgeError,
    LspMcpBridge, LspPosition, LspServerConfig,
};
pub use manager::{
    tool_glob_matches, CanonicalMembraneBinding, CanonicalMembraneCaller, McpManager,
    McpToolNameFilter,
};
pub use oauth::{
    detect_headless_from_env, McpOAuthLoginOptions, McpOAuthLoginResult, McpOAuthTokens,
};
#[cfg(feature = "agent_mcp-os-keyring")]
pub use oauth::{login_oauth, refresh_oauth_tokens, revoke_oauth_tokens, McpOAuthCredentialStore};
pub use pool::{pool_key, McpTransportPool, DEFAULT_MAX_POOLED_INSTANCES};
pub use crate::{parse_qualified_name, sanitize_tool_name};
pub use crate::{McpError as RightKitMcpError, RequestContext, ToolDescriptor};
pub use server_mode::{
    McpServedInvocationAudit, McpServerModeGuard, McpServerModePolicy, McpServerToolSummary,
    RightKitMcpBackend, RightKitMcpServer,
};
pub use shared_pool::{get_or_init_shared_pool, get_shared_pool, init_shared_pool, SharedMcpPool};
pub use transport::{
    initialize_params, map_rightkit_error, negotiate_version, rightkit_process_launcher,
    HttpTransport, McpTransport, SseTransport, StdioTransport, StubTransport, MCP_PROTOCOL_VERSION,
    MCP_SUPPORTED_VERSIONS,
};
pub use trust_gate::{
    decide_workspace_stdio, default_trust_store_path, gate_workspace_stdio_servers,
    load_trust_store, stdio_server_spec, PendingApproval, TrustDecision, TrustStore,
};
pub use types::{
    HttpConfig, McpChannelPermissions, McpConfigFile, McpOAuthConfig, McpResourceContent,
    McpResourceDef, McpServerConfig, McpToolAnnotations, McpToolDef, McpToolResult, McpTrustPolicy,
    SseConfig, StdioConfig,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn tool(name: &str, desc: &str) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: desc.to_string(),
            annotations: McpToolAnnotations::default(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn ok_result(content: &str) -> McpToolResult {
        McpToolResult {
            content: content.to_string(),
            is_error: false,
        }
    }

    #[tokio::test]
    async fn scoped_server_unload_advances_generation_and_reload_is_clean() {
        let mut manager = McpManager::new();
        let first: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "v1")]));
        manager.register_scoped("plugin:alpha", "svc", first);
        assert_eq!(manager.discover_all().await.len(), 1);
        let registered_generation = manager.generation();

        assert_eq!(manager.unload_scope("plugin:alpha"), 1);
        assert!(manager.generation() > registered_generation);
        assert!(manager.discover_all().await.is_empty());
        assert_eq!(manager.unload_scope("plugin:alpha"), 0);

        let second: Arc<dyn McpTransport> =
            Arc::new(StubTransport::new(vec![tool("search", "v2")]));
        manager.register_scoped("plugin:alpha", "svc", second);
        let reloaded = manager.discover_all().await;
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].description, "v2");
    }

    #[tokio::test]
    async fn unscoped_replacement_detaches_prior_scope() {
        let mut manager = McpManager::new();
        let scoped: Arc<dyn McpTransport> =
            Arc::new(StubTransport::new(vec![tool("search", "scoped")]));
        manager.register_scoped("plugin:alpha", "svc", scoped);
        let replacement: Arc<dyn McpTransport> =
            Arc::new(StubTransport::new(vec![tool("search", "replacement")]));
        manager.register("svc", replacement);

        assert_eq!(manager.unload_scope("plugin:alpha"), 0);
        let tools = manager.discover_all().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].description, "replacement");
    }

    // ── sanitize ──────────────────────────────────────────────────────────

    #[test]
    fn sanitize_basic() {
        assert_eq!(
            sanitize_tool_name("github", "get_pr"),
            "mcp__github__get_pr"
        );
    }

    #[test]
    fn sanitize_lowercases() {
        assert_eq!(sanitize_tool_name("GitHub", "GetPR"), "mcp__github__getpr");
    }

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(
            sanitize_tool_name("git-hub", "get.pr"),
            "mcp__git_hub__get_pr"
        );
    }

    #[test]
    fn sanitize_collapses_underscores() {
        assert_eq!(
            sanitize_tool_name("git--hub", "get..pr"),
            "mcp__git_hub__get_pr"
        );
    }

    #[test]
    fn parse_qualified_basic() {
        assert_eq!(
            parse_qualified_name("mcp__github__get_pr"),
            Some(("github".to_string(), "get_pr".to_string()))
        );
    }

    #[test]
    fn parse_qualified_not_mcp_returns_none() {
        assert_eq!(parse_qualified_name("github__get_pr"), None);
    }

    #[test]
    fn parse_qualified_missing_tool_returns_none() {
        assert_eq!(parse_qualified_name("mcp__github"), None);
    }

    #[test]
    fn sanitize_parse_roundtrip() {
        let qualified = sanitize_tool_name("github", "get_pr");
        assert_eq!(
            parse_qualified_name(&qualified),
            Some(("github".to_string(), "get_pr".to_string()))
        );
    }

    // ── StubTransport ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn stub_transport_list_tools() {
        let stub = StubTransport::new(vec![tool("search", "search the web")]);
        let tools = stub.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
    }

    #[tokio::test]
    async fn stub_transport_call_configured() {
        let stub = StubTransport::new(vec![tool("search", "d")])
            .with_response("search", ok_result("hits"));
        let res = stub
            .call_tool("search", serde_json::json!({}), None)
            .await
            .unwrap();
        assert_eq!(res.content, "hits");
        assert!(!res.is_error);
    }

    #[tokio::test]
    async fn stub_transport_call_unconfigured_errors() {
        let stub = StubTransport::new(vec![tool("search", "d")]);
        let err = stub.call_tool("search", serde_json::json!({}), None).await;
        assert!(matches!(err, Err(McpError::ToolNotFound(_))));
    }

    // ── McpManager ────────────────────────────────────────────────────────

    #[test]
    fn manager_register_and_server_names() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![]));
        mgr.register("github", stub);
        let mut names = mgr.server_names();
        names.sort();
        assert_eq!(names, vec!["github".to_string()]);
    }

    #[tokio::test]
    async fn manager_discover_all_qualifies_names() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "d")]));
        mgr.register("brave", stub);
        let tools = mgr.discover_all().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__brave__search");
    }

    #[tokio::test]
    async fn manager_discover_all_multiple_servers() {
        let mut mgr = McpManager::new();
        let a: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "d")]));
        let b: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("get_pr", "d"),
            tool("list_pr", "d"),
        ]));
        mgr.register("brave", a);
        mgr.register("github", b);
        let tools = mgr.discover_all().await;
        assert_eq!(tools.len(), 3);
        let mut names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "mcp__brave__search".to_string(),
                "mcp__github__get_pr".to_string(),
                "mcp__github__list_pr".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn manager_discover_all_hides_mutating_tools_below_trust_tier() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            read_only_tool("search", "d"),
            tool("write", "d"),
        ]));
        mgr.register("docs", stub);
        mgr.register_trust_policy(
            "docs",
            McpTrustPolicy {
                preset: "untrusted".to_string(),
                approved_by: None,
                approved_at: None,
            },
        );

        let tools = mgr.discover_all().await;
        let names: Vec<String> = tools.into_iter().map(|tool| tool.name).collect();

        assert_eq!(names, vec!["mcp__docs__search".to_string()]);
    }

    #[tokio::test]
    async fn manager_invoke_routes_to_correct_server() {
        let mut mgr = McpManager::new();
        let gh: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("get_pr", "d")])
                .with_response("get_pr", ok_result("pr#1")),
        );
        mgr.register("github", gh);
        let res = mgr
            .invoke("mcp__github__get_pr", serde_json::json!({}), None)
            .await
            .unwrap();
        assert_eq!(res.content, "pr#1");
    }

    #[tokio::test]
    async fn manager_invoke_with_session_id() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("query", "d")]).with_response("query", ok_result("rows")),
        );
        mgr.register("db", stub);
        // session_id is passed through; stub ignores it but the call must succeed
        let res = mgr
            .invoke("mcp__db__query", serde_json::json!({}), Some("sess-abc"))
            .await
            .unwrap();
        assert_eq!(res.content, "rows");
    }

    #[tokio::test]
    async fn manager_invoke_records_audit_for_served_invocation() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![read_only_tool("query", "d")])
                .with_response("query", ok_result("rows")),
        );
        mgr.register("db", stub);

        let res = mgr
            .invoke("mcp__db__query", serde_json::json!({}), Some("sess-abc"))
            .await
            .unwrap();

        assert_eq!(res.content, "rows");
        assert_eq!(mgr.invocation_audit_rows().len(), 1);
        assert_eq!(mgr.invocation_audit_rows()[0].tool, "mcp__db__query");
        assert_eq!(mgr.invocation_audit_rows()[0].session, "sess-abc");
    }

    #[tokio::test]
    async fn manager_invoke_unknown_server_errors() {
        let mgr = McpManager::new();
        let err = mgr
            .invoke("mcp__nope__get_pr", serde_json::json!({}), None)
            .await;
        assert!(matches!(err, Err(McpError::ToolNotFound(_))));
    }

    #[tokio::test]
    async fn manager_invoke_bad_name_errors() {
        let mgr = McpManager::new();
        let err = mgr
            .invoke("not_qualified", serde_json::json!({}), None)
            .await;
        assert!(matches!(err, Err(McpError::ToolNotFound(_))));
    }

    #[test]
    fn manager_empty() {
        let mgr = McpManager::new();
        assert!(mgr.is_empty());
        assert!(mgr.server_names().is_empty());
    }

    #[test]
    fn manager_channel_permissions_allow_unscoped_servers() {
        let mgr = McpManager::new();
        assert!(mgr.is_channel_allowed("github", "desktop"));
    }

    #[test]
    fn manager_channel_permissions_match_allowed_channels() {
        let mut mgr = McpManager::new();
        mgr.register_channel_permissions(
            "github",
            McpChannelPermissions {
                allowed_channels: vec!["telegram".to_string(), "cli".to_string()],
                allow_mutating: false,
            },
        );

        assert!(mgr.is_channel_allowed("github", "telegram"));
        assert!(mgr.is_channel_allowed("github", "cli"));
        assert!(!mgr.is_channel_allowed("github", "desktop"));
    }

    // ── config file parsing ────────────────────────────────────────────────

    #[test]
    fn config_file_parses_http() {
        let json = r#"{"mcpServers":{"db":{"type":"http","url":"http://127.0.0.1:9105/mcp"}}}"#;
        let cfg: McpConfigFile = serde_json::from_str(json).unwrap();
        assert!(matches!(cfg.mcp_servers["db"], McpServerConfig::Http(_)));
    }

    #[test]
    fn config_file_parses_sse() {
        let json = r#"{"mcpServers":{"c7":{"type":"sse","url":"http://127.0.0.1:9100/sse"}}}"#;
        let cfg: McpConfigFile = serde_json::from_str(json).unwrap();
        assert!(matches!(cfg.mcp_servers["c7"], McpServerConfig::Sse(_)));
    }

    #[test]
    fn config_file_parses_stdio() {
        let json =
            r#"{"mcpServers":{"gh":{"type":"stdio","command":"npx","args":["-y","@mcp/github"]}}}"#;
        let cfg: McpConfigFile = serde_json::from_str(json).unwrap();
        match &cfg.mcp_servers["gh"] {
            McpServerConfig::Stdio(c) => {
                assert_eq!(c.command, "npx");
                assert_eq!(c.args, vec!["-y", "@mcp/github"]);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn config_file_parses_channel_permissions() {
        let json = r#"{
          "mcpServers":{"gh":{"type":"stdio","command":"npx"}},
          "mcpChannelPermissions":{
            "gh":{"allowedChannels":["desktop","telegram"],"allowMutating":false}
          }
        }"#;
        let cfg: McpConfigFile = serde_json::from_str(json).unwrap();
        let permissions = cfg.mcp_channel_permissions.get("gh").unwrap();
        assert_eq!(permissions.allowed_channels, vec!["desktop", "telegram"]);
        assert!(!permissions.allow_mutating);
    }

    fn read_only_tool(name: &str, desc: &str) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: desc.to_string(),
            annotations: McpToolAnnotations {
                read_only_hint: Some(true),
                ..McpToolAnnotations::default()
            },
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn tool_def_parses_annotations() {
        let json = r#"{
          "name": "search",
          "description": "search docs",
          "input_schema": {"type": "object"},
          "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
          }
        }"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();

        assert_eq!(tool.annotations.read_only_hint, Some(true));
        assert_eq!(tool.annotations.destructive_hint, Some(false));
        assert_eq!(tool.annotations.idempotent_hint, Some(true));
        assert_eq!(tool.annotations.open_world_hint, Some(false));
    }

    #[test]
    fn mcp_error_display() {
        assert_eq!(
            McpError::ToolNotFound("x".to_string()).to_string(),
            "tool not found: x"
        );
        assert_eq!(McpError::NotConnected.to_string(), "not connected");
        assert_eq!(
            McpError::Transport("boom".to_string()).to_string(),
            "transport error: boom"
        );
        assert_eq!(
            McpError::Serde("bad".to_string()).to_string(),
            "serde error: bad"
        );
    }

    #[test]
    fn server_mode_hides_mutating_tools_below_trust_tier() {
        let policy = McpServerModePolicy::new("untrusted", true);
        let read = McpServerToolSummary::new("read_file", true);
        let write = McpServerToolSummary::new("write_file", false);

        assert!(policy.exposes_tool(&read));
        assert!(!policy.exposes_tool(&write));
    }

    #[test]
    fn server_mode_audits_served_invocation() {
        let mut guard = McpServerModeGuard::new(McpServerModePolicy::new("trusted", true));
        let write = McpServerToolSummary::new("write_file", false);

        assert!(guard.record_served_invocation(&write, "session-1", "2026-06-29T00:00:00Z"));

        assert_eq!(guard.audit_rows().len(), 1);
        assert_eq!(guard.audit_rows()[0].tool, "write_file");
        assert_eq!(guard.audit_rows()[0].session, "session-1");
        assert_eq!(guard.audit_rows()[0].outcome, "served");
    }

    // ── FM-184: collision rejection + sanitized-vs-original fix ───────────────

    #[tokio::test]
    async fn fm184_case_collision_rejected_atomically() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("Search", "d"),
            tool("search", "d"),
        ]));
        mgr.register("svc", stub);
        let tools = mgr.discover_all().await;
        // Both collide to mcp__svc__search -> neither registered.
        assert!(
            tools.is_empty(),
            "case collision must reject atomically, got {tools:?}"
        );
        let checked = mgr.discover_all_checked().await;
        assert!(checked.is_err());
        if let Err(McpError::Collision {
            normalized,
            servers,
            originals,
        }) = checked
        {
            assert_eq!(normalized, "mcp__svc__search");
            assert!(servers.contains(&"svc".to_string()));
            assert!(originals.contains(&"Search".to_string()));
            assert!(originals.contains(&"search".to_string()));
        } else {
            panic!("expected Collision");
        }
        // Invoke for colliding name must fail.
        let err = mgr
            .invoke("mcp__svc__search", serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::ToolNotFound(_)));
    }

    #[tokio::test]
    async fn fm184_punctuation_collision_rejected() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("get-pr", "d"),
            tool("get_pr", "d"),
            tool("get.pr", "d"),
        ]));
        mgr.register("github", stub);
        let tools = mgr.discover_all().await;
        assert!(tools.is_empty(), "punctuation variants must collide");
        // Non-colliding tool still present alongside collision set.
        let mut mgr2 = McpManager::new();
        let stub2: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("get-pr", "d"),
            tool("get_pr", "d"),
            tool("list_pr", "d"),
        ]));
        mgr2.register("github", stub2);
        let tools2 = mgr2.discover_all().await;
        // Only list_pr should survive; get-pr variants rejected atomically.
        assert_eq!(tools2.len(), 1);
        assert_eq!(tools2[0].name, "mcp__github__list_pr");
    }

    #[tokio::test]
    async fn fm184_unicode_collision_rejected() {
        // "café" -> clean "caf_" (é -> _), "caf-" -> "caf_", "caf_" -> "caf_"
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("café", "d"),
            tool("caf_", "d"),
        ]));
        mgr.register("svc", stub);
        let tools = mgr.discover_all().await;
        assert!(tools.is_empty(), "unicode vs ascii underscore must collide");
        let mut mgr2 = McpManager::new();
        let stub2: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("café", "d")]));
        mgr2.register("svc", stub2);
        let tools2 = mgr2.discover_all().await;
        assert_eq!(tools2.len(), 1);
        assert_eq!(tools2[0].name, "mcp__svc__caf_");
    }

    #[tokio::test]
    async fn fm184_sanitized_vs_original_invoke_fixed() {
        let mut mgr = McpManager::new();
        let stub: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("Get-PR", "d")])
                .with_response("Get-PR", ok_result("pr#1")),
        );
        mgr.register("GitHub", stub);
        // discover should expose sanitized name mcp__github__get_pr (server lowercased, tool sanitized)
        let tools = mgr.discover_all().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__github__get_pr");
        // invoke via sanitized qualified name must route to original "Get-PR"
        let res = mgr
            .invoke("mcp__github__get_pr", serde_json::json!({}), None)
            .await
            .unwrap();
        assert_eq!(res.content, "pr#1");
        // incorrect casing/punct still resolves via sanitized name, not original string equality
        // invoking with original mixed-case tool fragment should fail (qualified must be sanitized)
        let err = mgr
            .invoke("mcp__github__Get-PR", serde_json::json!({}), None)
            .await
            .unwrap_err();
        // "Get-PR" sanitizes to get_pr, but qualified with Get-PR contains hyphen+cap, which is not the sanitized form
        // Our invoke expects sanitized qualified, so it should be ToolNotFound.
        assert!(matches!(err, McpError::ToolNotFound(_)));
    }

    #[tokio::test]
    async fn fm184_reconnect_rebuilds_registry() {
        use std::sync::Mutex as StdMutex;
        struct DynamicStub {
            tools: Arc<StdMutex<Vec<McpToolDef>>>,
            responses: HashMap<String, McpToolResult>,
        }
        #[async_trait::async_trait]
        impl McpTransport for DynamicStub {
            async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
                Ok(self.tools.lock().unwrap().clone())
            }
            async fn call_tool(
                &self,
                name: &str,
                _args: serde_json::Value,
                _session: Option<&str>,
            ) -> Result<McpToolResult, McpError> {
                self.responses
                    .get(name)
                    .cloned()
                    .ok_or_else(|| McpError::ToolNotFound(name.to_string()))
            }
            async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
                Ok(vec![])
            }
            async fn read_resource(&self, _uri: &str) -> Result<McpResourceContent, McpError> {
                Err(McpError::ResourceNotFound(_uri.to_string()))
            }
        }
        let shared = Arc::new(StdMutex::new(vec![tool("search", "d")]));
        let dyn_stub = Arc::new(DynamicStub {
            tools: shared.clone(),
            responses: HashMap::from([("search".to_string(), ok_result("ok"))]),
        });
        let mut mgr = McpManager::new();
        mgr.register("svc", dyn_stub);
        let tools1 = mgr.discover_all().await;
        assert_eq!(tools1.len(), 1);
        assert_eq!(tools1[0].name, "mcp__svc__search");
        // Simulate reconnect where server now exposes colliding tools.
        *shared.lock().unwrap() = vec![tool("search", "d"), tool("Search", "d")];
        let tools2 = mgr.discover_all().await;
        // Rebuilt registry must now detect collision and reject.
        assert!(
            tools2.is_empty(),
            "reconnect with collision must be rejected"
        );
        assert!(mgr.discover_all_checked().await.is_err());
        // Reconnect again with single tool should recover.
        *shared.lock().unwrap() = vec![tool("search", "d")];
        let tools3 = mgr.discover_all().await;
        assert_eq!(tools3.len(), 1);
    }

    #[tokio::test]
    async fn fm184_hot_reload_new_manager_rejects_collisions() {
        // Hot-reload is a new manager built from new config; collisions must still be rejected.
        let mut mgr1 = McpManager::new();
        let stub1: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "d")]));
        mgr1.register("svc", stub1);
        assert_eq!(mgr1.discover_all().await.len(), 1);

        let mut mgr2 = McpManager::new();
        let stub2: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![
            tool("search", "d"),
            tool("SEARCH", "d"),
        ]));
        mgr2.register("svc", stub2);
        let tools2 = mgr2.discover_all().await;
        assert!(
            tools2.is_empty(),
            "hot-reloaded manager must reject case collision"
        );
        assert!(mgr2.discover_all_checked().await.is_err());
    }

    #[tokio::test]
    async fn fm184_cross_server_sanitized_collision() {
        // Servers "git-hub" and "git_hub" sanitize to same "git_hub", so their tools collide.
        let mut mgr = McpManager::new();
        let a: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "d")]));
        let b: Arc<dyn McpTransport> = Arc::new(StubTransport::new(vec![tool("search", "d")]));
        mgr.register("git-hub", a);
        mgr.register("git_hub", b);
        let tools = mgr.discover_all().await;
        // Both mcp__git_hub__search collide -> none registered.
        assert!(
            tools.is_empty(),
            "cross-server sanitized collision must reject, got {tools:?}"
        );
        let err = mgr.discover_all_checked().await.unwrap_err();
        if let McpError::Collision {
            normalized,
            servers,
            ..
        } = err
        {
            assert_eq!(normalized, "mcp__git_hub__search");
            assert!(servers.contains(&"git-hub".to_string()));
            assert!(servers.contains(&"git_hub".to_string()));
        } else {
            panic!("expected collision");
        }
    }

    fn canonical_binding() -> CanonicalMembraneBinding {
        CanonicalMembraneBinding {
            service_id: "service".into(),
            installation_id: "installation".into(),
            cortex_store_id: "store".into(),
            release_generation: "release".into(),
            startup_generation: 1,
            runtime_origin: "installed".into(),
            stable_install_root: Some("C:/installed/context".into()),
            protocol_version: 1,
            schema_version: 1,
        }
    }

    fn hidden_read_manager() -> McpManager {
        let mut manager = McpManager::new();
        let endpoint = "stdio://installed membrane";
        let transport: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("pull", "Pull"), tool("push", "Push")])
                .with_endpoint_identity(endpoint)
                .with_response("membrane_blueprint", ok_result("blueprint"))
                .with_response("membrane_memory_read", ok_result("memory"))
                .with_response("membrane_source_read", ok_result("source")),
        );
        manager.register("membrane", transport);
        assert!(manager.bind_canonical_membrane_server("membrane", canonical_binding(), endpoint));
        assert!(
            manager.bind_canonical_membrane_caller(CanonicalMembraneCaller {
                root: "C:/repo".into(),
                repository_id: "repo".into(),
                scope_id: "scope".into(),
                scope_descriptor: None,
            })
        );
        manager
    }

    #[tokio::test]
    async fn canonical_read_bridge_calls_hidden_reads_without_changing_public_inventory() {
        let manager = hidden_read_manager();
        let inventory = manager.discover_all().await;
        assert_eq!(
            inventory
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__membrane__pull", "mcp__membrane__push"]
        );
        for alias in ["blueprint_recall", "blueprint_search", "blueprint_expand"] {
            assert!(manager.has_canonical_membrane_read_bridge(alias));
        }
        for (tool_name, args, expected) in [
            (
                "blueprint_recall",
                serde_json::json!({"operation":"recall","repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                "blueprint",
            ),
            (
                "membrane_memory_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                "memory",
            ),
            (
                "membrane_source_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                "source",
            ),
        ] {
            let result = manager
                .invoke_canonical_membrane_read_with_cancel(
                    tool_name,
                    args,
                    Some("session"),
                    "cli",
                    None,
                )
                .await
                .unwrap();
            assert_eq!(result.content, expected);
        }
    }

    #[tokio::test]
    async fn canonical_read_bridge_rejects_policy_denial_replacement_and_mutating_ops() {
        let mut manager = hidden_read_manager();
        manager.register_channel_permissions(
            "membrane",
            McpChannelPermissions {
                allowed_channels: vec!["desktop".into()],
                allow_mutating: false,
            },
        );
        let denied = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_memory_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "cli",
                None,
            )
            .await;
        assert!(denied.is_err());
        let unsupported = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_blueprint",
                serde_json::json!({"operation":"refresh","repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "desktop",
                None,
            )
            .await;
        assert!(unsupported.is_err());
        let unknown = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_checkpoint_save",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "desktop",
                None,
            )
            .await;
        assert!(unknown.is_err());

        manager.register_channel_permissions(
            "membrane",
            McpChannelPermissions {
                allowed_channels: vec!["desktop".into()],
                allow_mutating: false,
            },
        );
        manager.register_trust_policy(
            "membrane",
            McpTrustPolicy {
                preset: "locked".into(),
                approved_by: None,
                approved_at: None,
            },
        );
        let trust_denied = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_memory_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "desktop",
                None,
            )
            .await;
        assert!(trust_denied.is_err());
        manager.register_trust_policy(
            "membrane",
            McpTrustPolicy {
                preset: "trusted".into(),
                approved_by: None,
                approved_at: None,
            },
        );

        let replacement: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("pull", "Pull"), tool("push", "Push")])
                .with_endpoint_identity("stdio://installed membrane")
                .with_response("membrane_memory_read", ok_result("replacement")),
        );
        manager.register("membrane", replacement);
        let replaced = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_memory_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "desktop",
                None,
            )
            .await;
        assert!(replaced.is_err());
    }

    #[tokio::test]
    async fn canonical_read_bridge_retains_cancellation() {
        let manager = hidden_read_manager();
        let (tx, rx) = tokio::sync::watch::channel(true);
        let cancelled = manager
            .invoke_canonical_membrane_read_with_cancel(
                "membrane_memory_read",
                serde_json::json!({"repository":"repo","caller":{"root":"C:/repo","repositoryId":"repo","scopeId":"scope"}}),
                None,
                "cli",
                Some(rx),
            )
            .await;
        assert!(matches!(cancelled, Err(McpError::Cancelled)));
        drop(tx);
    }

    #[test]
    fn canonical_attestation_does_not_follow_old_transport_to_an_alias() {
        let endpoint = "stdio://installed membrane";
        let original: Arc<dyn McpTransport> = Arc::new(
            StubTransport::new(vec![tool("pull", "Pull")]).with_endpoint_identity(endpoint),
        );
        let mut manager = McpManager::new();
        manager.register("membrane", original.clone());
        manager.register("old_alias", original);
        assert!(manager.bind_canonical_membrane_server("membrane", canonical_binding(), endpoint));
        assert!(manager.is_canonical_membrane_tool("mcp__membrane__pull"));
        assert!(!manager.is_canonical_membrane_tool("mcp__old_alias__pull"));
        manager.register(
            "membrane",
            Arc::new(
                StubTransport::new(vec![tool("pull", "Replacement")])
                    .with_endpoint_identity(endpoint),
            ),
        );
        assert!(!manager.is_canonical_membrane_tool("mcp__membrane__pull"));
    }
}
