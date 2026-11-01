#[tokio::test]
async fn create_session_uses_workspace_config_defaults_when_request_is_silent() {
    let root = unique_temp_dir("session-config");
    write_session_config_fixture(&root);
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "daemon-default".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    let app = build_router(state.clone());
    let body = serde_json::json!({
        "id": "workspace-config-session",
        "workspaceRoot": root.to_string_lossy(),
        "title": "Workspace config session"
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["model"], "workspace-model");
    assert_eq!(
        value["allowedTools"],
        serde_json::json!(["read_file", "search"])
    );
    assert_eq!(value["disallowedTools"], serde_json::json!(["shell"]));
    assert_eq!(value["effort"], "high");
    assert_eq!(value["thinkingBudgetTokens"], 8192);
    assert_eq!(value["disableThinking"], false);
    let live = state.get_session("workspace-config-session").unwrap();
    assert_eq!(live_session_permission_mode(&live), "AcceptEdits");
    let route = live.model_route.as_ref().expect("dynamic provider route");
    let route = lock_recover(route, "session.model_route");
    assert_eq!(
        route.fallback_models,
        vec![
            "workspace-fallback-a".to_string(),
            "workspace-fallback-b".to_string()
        ]
    );
    assert_eq!(route.thinking_budget_tokens, Some(8192));
    assert!(!route.disable_thinking);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_preserves_requested_active_skills() {
    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "active-skill-session",
                        "workspaceRoot": ".",
                        "activeSkills": [" architecture ", ""]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let live = state.get_session("active-skill-session").unwrap();
    assert_eq!(live.active_skills, vec!["architecture"]);
}

#[tokio::test]
async fn create_session_applies_workspace_local_provider_route() {
    let root = unique_temp_dir("session-local-provider-route");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("config.toml"),
        r#"
[model]
default-model = "qwen2.5-coder:7b"

[provider-routes.ollama-local]
display-name = "Local: qwen2.5-coder:7b"
base-url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder:7b"
api-key-required = false
"#,
    )
    .unwrap();
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "cloud-fallback-key".into(),
        model_id: "daemon-default".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "workspace-local-model-session",
                        "workspaceRoot": root.to_string_lossy(),
                        "title": "Workspace local model session"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let live = state.get_session("workspace-local-model-session").unwrap();
    let route = live.model_route.as_ref().expect("dynamic provider route");
    let route = lock_recover(route, "session.model_route");
    assert_eq!(route.model_id, "qwen2.5-coder:7b");
    assert_eq!(route.base_url, "http://127.0.0.1:11434/v1");
    assert_eq!(route.api_key, "");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_rejects_invalid_effort() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"id": "s-invalid-effort", "effort": "maximum"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_session_rejects_zero_thinking_budget() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"id": "s-invalid-thinking-budget", "thinkingBudgetTokens": 0}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

fn write_test_plugin(dir: &std::path::Path, id: &str, enabled: bool) {
    let plugin_dir = dir.join(id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let manifest = serde_json::json!({
        "id": id,
        "name": "Test Plugin",
        "version": "1.0.0",
        "description": "A route-level plugin fixture",
        "entry": "plugin.wasm",
        "provides_tools": ["fixture_tool"],
        "provides_commands": ["fixture"],
        "enabled": enabled
    });
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn safe_mode_session_tool_registry_excludes_plugin_tools() {
    let root = unique_temp_dir("safe-mode-tool-registry");
    let plugin_dir = root.join("plugins");
    write_test_plugin(&plugin_dir, "fixture", true);

    let normal = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    assert!(normal
        .all()
        .iter()
        .any(|tool| tool.name() == "fixture_tool"));

    let safe = build_session_tool_registry(
        plugin_dir.clone(),
        true,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    assert!(!safe.all().iter().any(|tool| tool.name() == "fixture_tool"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn safe_mode_session_tool_registry_is_read_only_with_empty_legacy_filters() {
    let safe = build_session_tool_registry(
        std::path::PathBuf::new(),
        true,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );

    assert!(safe.all().iter().any(|tool| tool.name() == "read_file"));
    assert!(!safe.all().iter().any(|tool| tool.name() == "write_file"));
    assert!(!safe.all().iter().any(|tool| tool.name() == "shell"));
}

#[test]
fn fm073_live_session_registry_projects_lazy_config_without_removing_tools() {
    let eager = build_session_tool_registry(
        std::path::PathBuf::new(),
        true,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let lazy = build_session_tool_registry(
        std::path::PathBuf::new(),
        true,
        None,
        &[],
        &[],
        true,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );

    assert!(!eager.lazy_loading_enabled());
    assert!(lazy.lazy_loading_enabled());
    assert!(eager.is_prelude_tool("web_search"));
    assert!(!lazy.is_prelude_tool("web_search"));
    assert!(lazy.all().iter().any(|tool| tool.name() == "web_search"));
}

#[test]
fn safe_mode_session_tool_registry_preserves_narrow_filters_and_denials() {
    let role_allowed = vec![
        "read_file".to_string(),
        "search".to_string(),
        "write_file".to_string(),
    ];
    let safe = build_session_tool_registry(
        std::path::PathBuf::new(),
        true,
        Some(&role_allowed),
        &[],
        &["search".to_string()],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );

    assert!(safe.all().iter().any(|tool| tool.name() == "read_file"));
    assert!(!safe.all().iter().any(|tool| tool.name() == "search"));
    assert!(!safe.all().iter().any(|tool| tool.name() == "write_file"));
}

#[test]
fn denied_tools_stay_denied_after_alias_expansion_and_always_on_union() {
    // CQ-01: the catalog builders expand group aliases and union ALWAYS_ON
    // tools onto the filtered selection, so a deny checked only against the
    // pre-expansion list is reintroduced into the executable registry.
    let denied_alias_member = build_session_tool_registry(
        std::path::PathBuf::new(),
        false,
        None,
        &["shell".to_string()],
        &["powershell".to_string()],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    assert!(denied_alias_member
        .all()
        .iter()
        .any(|tool| tool.name() == "shell"));
    assert!(!denied_alias_member
        .all()
        .iter()
        .any(|tool| tool.name() == "powershell"));

    let denied_always_on = build_session_tool_registry(
        std::path::PathBuf::new(),
        false,
        None,
        &[],
        &["todo_write".to_string()],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    assert!(!denied_always_on
        .all()
        .iter()
        .any(|tool| tool.name() == "todo_write"));
}

#[test]
fn safe_mode_registry_excludes_non_read_only_always_on_tools() {
    // The ALWAYS_ON union inside build_for_allowed re-added mcp_call —
    // an arbitrary external invocation that is not read-only — even after
    // the safe-mode filter selected only read-only tools.
    let safe = build_session_tool_registry(
        std::path::PathBuf::new(),
        true,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    assert!(!safe.all().iter().any(|tool| tool.name() == "mcp_call"));
}

#[test]
fn safe_mode_disables_user_surfaces() {
    let normal = SessionRuntimeOptions::default();
    assert!(session_user_surfaces_enabled(&normal));

    let safe = SessionRuntimeOptions {
        safe_mode: true,
        ..SessionRuntimeOptions::default()
    };
    assert!(!session_user_surfaces_enabled(&safe));
}

#[tokio::test(flavor = "multi_thread")]
async fn safe_mode_session_skips_persistent_permission_rules_and_persists_flag() {
    let root = unique_temp_dir("safe-mode-session");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store_path = root.join("permission-rules.json");
    let workspace_key = workspace.to_string_lossy().to_string();
    let mut projects = serde_json::Map::new();
    projects.insert(
        workspace_key,
        serde_json::json!([{"pattern": "write_file", "effect": "ask"}]),
    );
    std::fs::write(
        &store_path,
        serde_json::json!({
            "user": [{"pattern": "shell", "effect": "deny"}],
            "projects": projects
        })
        .to_string(),
    )
    .unwrap();
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("")
        .with_permission_rule_store_path(store_path)
        .with_storage(storage.clone());

    let safe = state.create_session_with_options(
        "s-safe",
        None,
        &workspace.to_string_lossy(),
        "Safe",
        None,
        SessionRuntimeOptions {
            safe_mode: true,
            ..SessionRuntimeOptions::default()
        },
    );
    assert!(
        lock_recover(&safe.permission_rules, "session.permission_rules")
            .all_rules()
            .is_empty()
    );
    assert_eq!(session_row(&safe)["safeMode"], true);

    let normal = state.create_session(
        "s-normal",
        None,
        &workspace.to_string_lossy(),
        "Normal",
        None,
    );
    assert_eq!(
        lock_recover(&normal.permission_rules, "session.permission_rules")
            .all_rules()
            .len(),
        2
    );

    let persisted = wait_for_session(&storage, "s-safe").await;
    let config: serde_json::Value = serde_json::from_str(&persisted.config_json).unwrap();
    assert_eq!(config["safeMode"], true);

    let _ = std::fs::remove_dir_all(root);
}

fn write_schedule_fixture(root: &std::path::Path) {
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("schedules.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "daily-review": {
                "id": "daily-review",
                "cron_expr": "0 9 * * *",
                "prompt": "review pending PRs"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        coderight_dir.join("schedule-runs.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "lastClaimedMinute": {
                "daily-review": 12345
            },
            "history": [
                {
                    "scheduleId": "daily-review",
                    "epochMinute": 12344,
                    "status": "claimed",
                    "sessionId": "schedule-daily-review",
                    "message": "claimed for execution"
                },
                {
                    "scheduleId": "daily-review",
                    "epochMinute": 12345,
                    "status": "submitted",
                    "sessionId": "schedule-daily-review",
                    "message": "submitted scheduled prompt"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

// S:M2 — per-agent tool isolation. Each session/agent gets one
// independently-built `ToolRegistry` after role/request allow intersection
// + deny subtraction. The same `Arc<ToolRegistry>` is what the provider
// prelude projection and the engine dispatch path read; siblings must not
// leak capabilities. This fixture pins the contract that the live
// `api_state_impl_04` session-creation path already enforces.
#[test]
fn sibling_sessions_have_independent_tool_registries() {
    use std::collections::HashSet;
    use std::sync::Arc;

    // A only sees read-only tools; B only sees write-side tools. The two
    // allowlists are disjoint by design so any leakage is observable.
    let a_allowed = vec!["read_file".to_string(), "search".to_string()];
    let b_allowed = vec!["write_file".to_string(), "edit_file".to_string()];

    let root = unique_temp_dir("m2-sibling-isolation");
    let plugin_dir = root.join("plugins");
    let reg_a = Arc::new(build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &a_allowed,
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    ));
    let reg_b = Arc::new(build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &b_allowed,
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    ));

    // The two registries are distinct instances (no shared mutable
    // surface); a mutation on one must not appear on the other.
    assert!(!Arc::ptr_eq(&reg_a, &reg_b));

    let names_a: HashSet<String> = reg_a
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    let names_b: HashSet<String> = reg_b
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();

    // A's projection contains A's allowlist tools and excludes B's.
    assert!(names_a.contains("read_file"));
    assert!(names_a.contains("search"));
    assert!(!names_a.contains("write_file"));
    assert!(!names_a.contains("edit_file"));

    // B's projection contains B's allowlist tools and excludes A's.
    assert!(names_b.contains("write_file"));
    assert!(names_b.contains("edit_file"));
    assert!(!names_b.contains("read_file"));
    assert!(!names_b.contains("search"));

    // Provider prelude projection (the same prelude
    // `ProviderLlmClient::complete_with_metadata` ships) must reflect
    // the per-session filter — never the global default set.
    let messages: Vec<rightkit_agent_engine_protocol::TranscriptMessage> = Vec::new();
    let prelude_a: HashSet<String> =
        rightkit_agent_tools::coding_tools::registry::tool_defs_for_full_history(&reg_a, &messages)
            .iter()
            .filter_map(|def| {
                def.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
            .collect();
    let prelude_b: HashSet<String> =
        rightkit_agent_tools::coding_tools::registry::tool_defs_for_full_history(&reg_b, &messages)
            .iter()
            .filter_map(|def| {
                def.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
            .collect();
    assert!(prelude_a.contains("read_file") && !prelude_a.contains("write_file"));
    assert!(prelude_b.contains("write_file") && !prelude_b.contains("read_file"));

    // Engine dispatch lookup uses the same registry the prelude does, so
    // a sibling-only tool is unreachable from the wrong session.
    assert!(reg_a.get("write_file").is_none());
    assert!(reg_b.get("read_file").is_none());

    // Deny wins: subtract a tool that the request explicitly allows —
    // A's allowlist still loses `search` to the deny list.
    let reg_a_denied = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &a_allowed,
        &["search".to_string()],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let names_a_denied: HashSet<String> = reg_a_denied
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(!names_a_denied.contains("search"));
    assert!(names_a_denied.contains("read_file"));

    // Default unrestricted behavior: with no allow list and no deny
    // list the projection is the canonical default set, so a session
    // that does not opt into isolation still sees the full surface.
    let reg_default = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let default_names: HashSet<String> = reg_default
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(default_names.contains("read_file"));
    assert!(default_names.contains("write_file"));
    assert!(default_names.contains("search"));

    let _ = std::fs::remove_dir_all(root);
}

// S:M2 — live chain evidence. The session-creation path
// (`api_state_impl_04::create_session_seeded_with_options`) builds exactly
// one `Arc<ToolRegistry>` and passes the same `Arc::clone` to both
// `ProviderLlmClient::*` and `SessionEngine::new`. The same `Arc` is
// reachable through `LiveSession::model_route` / engine-config consumers
// — a second independent build would break this contract. This fixture
// pins the projection inputs that the live path feeds into
// `build_session_tool_registry`, so any future change to
// `effective_tool_filter` is forced to keep A/B projection disjoint.
#[test]
fn m2_per_session_registry_uses_role_then_request_allow_intersected_with_deny() {
    use std::collections::HashSet;

    // Role allows a superset; the request narrows. Effective allow is
    // role ∩ request.
    let role_allowed: Vec<String> = vec![
        "read_file".to_string(),
        "search".to_string(),
        "write_file".to_string(),
    ];
    let requested: Vec<String> = vec!["read_file".to_string(), "search".to_string()];

    let root = unique_temp_dir("m2-role-request-intersect");
    let plugin_dir = root.join("plugins");
    let reg = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        Some(&role_allowed),
        &requested,
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let names: HashSet<String> = reg
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(names.contains("read_file"));
    assert!(names.contains("search"));
    assert!(!names.contains("write_file"));

    // With request empty, role alone is the projection.
    let reg_role_only = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        Some(&role_allowed),
        &[],
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let names_role_only: HashSet<String> = reg_role_only
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(names_role_only.contains("write_file"));

    // With role empty, request alone is the projection.
    let reg_req_only = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &requested,
        &[],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let names_req_only: HashSet<String> = reg_req_only
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(names_req_only.contains("read_file"));
    assert!(!names_req_only.contains("write_file"));

    // Deny subtracts from the default surface when no allow filter is
    // supplied (this is the path siblings fall back to when only a deny
    // list is set).
    let reg_deny_only = build_session_tool_registry(
        plugin_dir.clone(),
        false,
        None,
        &[],
        &["write_file".to_string()],
        false,
        rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
        None,
    );
    let names_deny_only: HashSet<String> = reg_deny_only
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(!names_deny_only.contains("write_file"));
    assert!(names_deny_only.contains("read_file"));

    let _ = std::fs::remove_dir_all(root);
}
