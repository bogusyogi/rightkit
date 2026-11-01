// The env lock is intentionally held for the whole async test body.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::super::*;
    use crate::agent_mcp::error::McpError;
    use crate::agent_mcp::trust_gate::TrustStore;
    use crate::agent_mcp::types::StdioConfig;

    // ── workspace MCP trust gate (audit A-6) ───────────────────────────────
    //
    // These tests isolate two on-disk surfaces per run: `RIGHTKIT_MCP_CONFIG`
    // is pointed at a definitely-missing file so a real `~/.config/host app/mcp.json`
    // on the dev machine never leaks in, and the trust-store path is an
    // explicit temp file passed to `load_with_trust_gate` rather than the
    // default `~/.config/...` location.

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialises tests that mutate process env (`RIGHTKIT_MCP_CONFIG`, the
    /// trust-store override): the unit runner is parallel, so without the lock
    /// one test's user config path leaks into another's.
    fn isolated_env_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var(
            "RIGHTKIT_MCP_CONFIG",
            "definitely-missing-rightkit-agent-mcp-config.json",
        );
        guard
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-agent-mcp-trust-gate-test-{tag}-{}",
            unix_timestamp_seconds()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_workspace_mcp_json(workspace: &Path, body: &str) {
        std::fs::write(workspace.join(".mcp.json"), body).unwrap();
    }

    #[tokio::test]
    async fn fresh_workspace_stdio_server_is_not_spawned_and_pending() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("fresh");
        write_workspace_mcp_json(
            &workspace,
            r#"{"mcpServers":{"evil":{"type":"stdio","command":"does-not-exist-binary","args":["-y"]}}}"#,
        );
        let trust_store_path = workspace.join("trust.json");

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        assert!(manager.server_names().contains(&"evil".to_string()));
        // Registered as a StubTransport with zero tools — never the real
        // command, so `list_tools` resolves immediately with nothing rather
        // than attempting to spawn `does-not-exist-binary`.
        let tools = manager.discover_all().await;
        assert!(tools.iter().all(|t| !t.name.contains("evil")));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn approved_workspace_stdio_server_spawns() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("approved");
        let cfg = StdioConfig {
            command: "does-not-exist-binary".to_string(),
            args: vec!["-y".to_string()],
            env: HashMap::new(),
        };
        write_workspace_mcp_json(
            &workspace,
            r#"{"mcpServers":{"ok":{"type":"stdio","command":"does-not-exist-binary","args":["-y"]}}}"#,
        );
        let trust_store_path = workspace.join("trust.json");
        let workspace_key = workspace.to_string_lossy().to_string();
        let mut store = TrustStore::new();
        store.approve(
            &workspace_key,
            "ok",
            &crate::agent_mcp::trust_gate::stdio_server_spec(&cfg),
        );
        store.save(&trust_store_path).unwrap();

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        // Approved servers go through the REAL build_manager path, i.e. a
        // StdioTransport is registered (not a StubTransport) — connecting it
        // would attempt to spawn the real command, which is exactly the
        // behavior an approval should unlock.
        assert!(manager.server_names().contains(&"ok".to_string()));
        let err = manager
            .invoke("mcp__ok__whatever", serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Transport(_)));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn denied_workspace_stdio_server_never_spawns() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("denied");
        let cfg = StdioConfig {
            command: "does-not-exist-binary".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        write_workspace_mcp_json(
            &workspace,
            r#"{"mcpServers":{"bad":{"type":"stdio","command":"does-not-exist-binary","args":[]}}}"#,
        );
        let trust_store_path = workspace.join("trust.json");
        let workspace_key = workspace.to_string_lossy().to_string();
        let mut store = TrustStore::new();
        store.deny(
            &workspace_key,
            "bad",
            &crate::agent_mcp::trust_gate::stdio_server_spec(&cfg),
        );
        store.save(&trust_store_path).unwrap();

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        let tools = manager.discover_all().await;
        assert!(
            tools.iter().all(|tool| !tool.name.contains("bad")),
            "a denied workspace server must not contribute tools"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn config_hash_change_reprompts_and_stub_replaces_previously_approved_server() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("hash-change");
        let original = StdioConfig {
            command: "does-not-exist-binary".to_string(),
            args: vec!["-y".to_string()],
            env: HashMap::new(),
        };
        let trust_store_path = workspace.join("trust.json");
        let workspace_key = workspace.to_string_lossy().to_string();
        let mut store = TrustStore::new();
        store.approve(
            &workspace_key,
            "ok",
            &crate::agent_mcp::trust_gate::stdio_server_spec(&original),
        );
        store.save(&trust_store_path).unwrap();

        // The workspace file now declares a DIFFERENT command line for the
        // same server name — the stored approval no longer matches.
        write_workspace_mcp_json(
            &workspace,
            r#"{"mcpServers":{"ok":{"type":"stdio","command":"does-not-exist-binary","args":["-y","--extra-flag"]}}}"#,
        );

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        let tools = manager.discover_all().await;
        assert!(
            tools.iter().all(|tool| !tool.name.contains("ok")),
            "changed config must re-prompt, not reuse the stale approval"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn global_config_stdio_server_spawns_without_trust_prompt() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("global-only");
        // No `.mcp.json` in the workspace at all — only the built-in
        // `context7` default server is present, which is exempt from the
        // workspace trust gate entirely.
        let trust_store_path = workspace.join("trust.json");

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        assert!(manager.server_names().contains(&"context7".to_string()));
        // No trust file was ever written — nothing was pending to decide.
        assert!(!trust_store_path.exists());

        let _ = std::fs::remove_dir_all(&workspace);
    }

    // ── project-trust gate (#131 A203): fail closed for ALL transports ─────

    const PROJECT_HTTP_AND_STDIO: &str = r#"{"mcpServers":{
        "evil-http":{"type":"http","url":"http://evil-host.invalid/mcp"},
        "evil-stdio":{"type":"stdio","command":"does-not-exist-binary"}
    }}"#;

    #[tokio::test]
    async fn untrusted_workspace_loads_no_project_servers_of_any_transport() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("untrusted-all");
        write_workspace_mcp_json(&workspace, PROJECT_HTTP_AND_STDIO);
        let trust_store_path = workspace.join("trust.json");

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Untrusted,
        )
        .await;

        let names = manager.server_names();
        assert!(!names.contains(&"evil-http".to_string()), "{names:?}");
        assert!(!names.contains(&"evil-stdio".to_string()), "{names:?}");
        // Built-in defaults are not project config and still load.
        assert!(names.contains(&"context7".to_string()));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn trusted_workspace_loads_project_http_server() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("trusted-http");
        write_workspace_mcp_json(&workspace, PROJECT_HTTP_AND_STDIO);
        let trust_store_path = workspace.join("trust.json");

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Trusted,
        )
        .await;

        assert!(manager.server_names().contains(&"evil-http".to_string()));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn untrusted_project_config_cannot_shadow_a_user_server() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("shadow");
        let user_config = workspace.join("user-mcp.json");
        std::fs::write(
            &user_config,
            r#"{"mcpServers":{"shared":{"type":"http","url":"http://user-host.invalid/mcp"}}}"#,
        )
        .unwrap();
        std::env::set_var("RIGHTKIT_MCP_CONFIG", &user_config);
        write_workspace_mcp_json(
            &workspace,
            r#"{"mcpServers":{"shared":{"type":"http","url":"http://evil-host.invalid/mcp"}}}"#,
        );
        let trust_store_path = workspace.join("trust.json");

        let manager = load_with_trust_gate_and_project_trust(
            Some(&workspace),
            &trust_store_path,
            ProjectTrust::Untrusted,
        )
        .await;

        let identity = manager
            .transport_endpoint_identity("shared")
            .expect("user-level server stays registered");
        assert!(identity.contains("user-host"), "{identity}");
        assert!(!identity.contains("evil-host"), "{identity}");

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn default_loader_resolves_trust_from_persisted_store_and_fails_closed() {
        let _env_lock = isolated_env_guard();
        let workspace = temp_workspace("resolve");
        write_workspace_mcp_json(&workspace, PROJECT_HTTP_AND_STDIO);
        let mcp_trust_store = workspace.join("mcp-trust.json");
        let ws_trust_store = workspace.join("workspace-trust.json");
        std::env::set_var(
            rightkit_app_settings::agent_config::workspace_trust::WORKSPACE_TRUST_STORE_ENV,
            &ws_trust_store,
        );

        // No record at all: untrusted.
        let manager = load_with_trust_gate(Some(&workspace), &mcp_trust_store).await;
        assert!(!manager.server_names().contains(&"evil-http".to_string()));

        // Explicit non-trusted label: still untrusted.
        let key =
            rightkit_app_settings::agent_config::workspace_trust_key(&workspace.to_string_lossy());
        std::fs::write(
            &ws_trust_store,
            serde_json::json!({"projects": {key.clone(): {"trust":"sandboxed","approvedBy":null,"updatedAt":"t"}}}).to_string(),
        )
        .unwrap();
        let manager = load_with_trust_gate(Some(&workspace), &mcp_trust_store).await;
        assert!(!manager.server_names().contains(&"evil-http".to_string()));

        // Explicit trusted record: project HTTP server loads.
        std::fs::write(
            &ws_trust_store,
            serde_json::json!({"projects": {key: {"trust":"trusted","approvedBy":"user","updatedAt":"t"}}}).to_string(),
        )
        .unwrap();
        let manager = load_with_trust_gate(Some(&workspace), &mcp_trust_store).await;
        assert!(manager.server_names().contains(&"evil-http".to_string()));

        std::env::remove_var(
            rightkit_app_settings::agent_config::workspace_trust::WORKSPACE_TRUST_STORE_ENV,
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    fn unix_timestamp_seconds() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default()
    }
}
