#[tokio::test]
async fn provider_key_routes_save_and_report_status() {
    let root = unique_temp_dir("provider-key-routes");
    let _guard = env_lock();
    let old_home = std::env::var("HOME").ok();
    let old_userprofile = std::env::var("USERPROFILE").ok();
    let previous_coderight_home = std::env::var("RIGHTKIT_SUITE_ROOT").ok();
    let old_openrouter = std::env::var("OPENROUTER_API_KEY").ok();
    // Sibling home and suite root: on macOS the real data folder derives from HOME.
    let (home, suite) = (root.join("home"), root.join("suite"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&suite).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);
    std::env::remove_var("OPENROUTER_API_KEY");

    let app = build_router(DaemonState::new(""));
    let save = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/provider-keys/openrouter")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"apiKey":"sk-test-route"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Keychain-only: a host without a usable keychain fails closed (500), never a file.
    if save.status() == StatusCode::OK {
        let body = save.into_body().collect().await.unwrap().to_bytes();
        let saved: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved["provider"], "openrouter");
        assert_eq!(saved["configured"], true);
        assert_eq!(saved["source"], "keychain");

        let status = app
            .oneshot(
                Request::builder()
                    .uri("/provider-keys/openrouter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let body = status.into_body().collect().await.unwrap().to_bytes();
        let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status["configured"], true);
        assert_eq!(status["source"], "keychain");
        let _ = rightkit_llm::agent_providers::KeyStore::delete_api_key("openrouter");
    } else {
        assert_eq!(save.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match old_userprofile {
        Some(value) => std::env::set_var("USERPROFILE", value),
        None => std::env::remove_var("USERPROFILE"),
    }
    match previous_coderight_home {
        Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
        None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
    }
    match old_openrouter {
        Some(value) => std::env::set_var("OPENROUTER_API_KEY", value),
        None => std::env::remove_var("OPENROUTER_API_KEY"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_uses_requested_model_override() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: vec!["openrouter/fallback-model".into()],
    });
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/sessions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                    "id": "s-model-override",
                    "workspaceRoot": "/tmp",
                    "title": "model override",
                    "model": "openrouter/requested-model"
                }"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(v["model"], "openrouter/requested-model");
    let live = state.get_session("s-model-override").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "openrouter/requested-model"
    );
}

#[tokio::test]
async fn create_session_preserves_requested_model_without_configured_engine() {
    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/sessions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                    "id": "s-keyless-model-override",
                    "workspaceRoot": "/tmp",
                    "title": "keyless model override",
                    "model": "minimax/minimax-m3"
                }"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["model"], "minimax/minimax-m3");
    let live = state.get_session("s-keyless-model-override").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "minimax/minimax-m3"
    );
}

#[tokio::test]
async fn patch_session_model_updates_live_provider_route() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: vec!["openrouter/fallback-model".into()],
    });
    state.create_session("s-live-model", None, "/tmp", "Live model", None);
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-live-model")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openrouter/next-model"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["modelAppliesTo"], "live");

    let live = state.get_session("s-live-model").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "openrouter/next-model"
    );
    let route = live.model_route.as_ref().expect("dynamic provider route");
    let route = lock_recover(route, "session.model_route");
    assert_eq!(route.model_id, "openrouter/next-model");
    assert_eq!(
        route.fallback_models,
        vec!["openrouter/fallback-model".to_string()]
    );
}

#[tokio::test]
async fn patch_session_model_accepts_agent_alias_for_live_route() {
    let root = unique_temp_dir("patch-agent-alias");
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "fallback-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    let workspace_config = rightkit_app_settings::agent_config::ConfigLoader::new().load_files_for_workspace(&root);
    let expected = state
        .provider_route_for_model(
            &workspace_config,
            "coder",
            state.engine_cfg.as_ref().unwrap(),
            None,
            None,
            false,
        )
        .model_id;
    state.create_session(
        "s-agent-model",
        None,
        &root.to_string_lossy(),
        "Agent model",
        None,
    );
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-agent-model")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"coder"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let live = state.get_session("s-agent-model").unwrap();
    assert_eq!(lock_recover(&live.model, "session.model").as_str(), "coder");
    let route = live.model_route.as_ref().expect("dynamic provider route");
    assert_eq!(
        lock_recover(route, "session.model_route").model_id,
        expected
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn patch_session_model_rejects_subagent_sessions() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("parent-model", None, "/tmp", "Parent", None);
    state.create_session(
        "child-model",
        Some("coder"),
        "/tmp",
        "Child",
        Some("parent-model"),
    );
    let original_model = {
        let live = state.get_session("child-model").unwrap();
        let model = lock_recover(&live.model, "session.model").clone();
        model
    };
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/child-model")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"openrouter/next-model"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let live = state.get_session("child-model").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        original_model
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_session_structured_selection_persists_binds_and_restarts_legacy_client() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    storage
        .lock()
        .await
        .upsert_direct_inference_connection(&rightkit_agent_engine_storage::DirectInferenceConnectionRow {
            id: "direct-patch".into(),
            name: "Patch direct".into(),
            protocol: "openai_compatible".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            auth_kind: "none".into(),
            auth_header_name: None,
            public_headers_json: "{}".into(),
            manual_models_json: r#"["manual-model"]"#.into(),
            trust_private_network: true,
            credential_configured: false,
            credential_revision: 0,
            configuration_revision: 1,
            created_at: "2026-08-11T00:00:00Z".into(),
            updated_at: "2026-08-11T00:00:00Z".into(),
        })
        .unwrap();
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-selection-patch", None, "/tmp", "Selection patch", None);
    wait_for_session(&storage, "s-selection-patch").await;

    let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/sessions/s-selection-patch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"inferenceSelection":{"schemaVersion":1,"authority":"direct","connectionId":"direct-patch","protocol":"openai_compatible","modelId":"manual-model","routePolicyId":null}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["inferenceSelectionUpdated"], true);
    assert_eq!(value["inferenceSelectionAppliesTo"], "restarted");

    let row = storage
        .lock()
        .await
        .get_session("s-selection-patch")
        .unwrap()
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["inferenceSelection"]["modelId"], "manual-model");
    assert!(config["inferenceFingerprint"].as_str().is_some());
    assert!(config.get("model").is_none());
    assert!(storage
        .lock()
        .await
        .inference_runtime_binding("s-selection-patch", "s-selection-patch")
        .unwrap()
        .is_some());
    let live = state.get_session("s-selection-patch").unwrap();
    assert_eq!(
        lock_recover(&live.inference_selection, "session.inference_selection")
            .as_ref()
            .map(|selection| selection.model_id.as_str()),
        Some("manual-model")
    );
}

#[tokio::test]
async fn patch_session_structured_selection_refuses_active_inference_without_mutation() {
    let state = DaemonState::new("");
    state.create_session("s-selection-active", None, "/tmp", "Original title", None);
    let live = state.get_session("s-selection-active").unwrap();
    live.inference_active_requests
        .store(1, std::sync::atomic::Ordering::Release);

    let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/sessions/s-selection-active")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"title":"Must not apply","inferenceSelection":{"schemaVersion":1,"authority":"direct","connectionId":"missing","protocol":"openai_compatible","modelId":"model","routePolicyId":null}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        lock_recover(&live.title, "session.title").as_str(),
        "Original title"
    );
    assert!(lock_recover(&live.inference_selection, "session.inference_selection").is_none());
}

#[tokio::test]
async fn free_role_session_ignores_split_workspace_agent_override() {
    let root = unique_temp_dir("workspace-agent-override");
    // Session model resolution merges the USER's global config
    // (`<state root>/config.toml`) under the workspace one, so a
    // developer whose real config sets `[orchestration] boss-model` resolves
    // that alias instead of the engine default and this test fails on their
    // machine only. Point RIGHTKIT_SUITE_ROOT at an empty dir for the duration.
    let _guard = env_lock();
    let home = unique_temp_dir("workspace-agent-override-home");
    std::fs::create_dir_all(&home).unwrap();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    let restore_home = || match &prior_home {
        Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
        None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
    };

    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("config.toml"),
        r#"
[orchestration]
boss-model = "openrouter/default-model"

[orchestration.agents.coder]
model-id = "minimax/minimax-m3"
provider-id = "openrouter"
"#,
    )
    .unwrap();

    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });

    let live = state.create_session(
        "workspace-role-session",
        Some("coder"),
        &root.to_string_lossy(),
        "Workspace role",
        None,
    );
    // Restore before asserting so a failure cannot leak RIGHTKIT_SUITE_ROOT into
    // the rest of the suite.
    restore_home();
    let _ = std::fs::remove_dir_all(&home);

    // Without the split entitlement the workspace's per-role override must
    // not leak into the session; single-role-model mode collapses every
    // role onto the one engine model.
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "openrouter/default-model"
    );
    let route = live.model_route.as_ref().expect("dynamic provider route");
    assert_eq!(
        lock_recover(route, "session.model_route").model_id,
        "openrouter/default-model"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn provider_route_for_model_uses_workspace_agent_override_for_alias() {
    let _guard = env_lock();
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    let mut workspace_config = rightkit_app_settings::agent_config::HostConfig::default();
    workspace_config.orchestration.boss_model = "conductor".into();
    workspace_config.orchestration.agents.insert(
        "conductor".into(),
        rightkit_app_settings::agent_config::AgentOverride {
            model_id: "minimax/minimax-m3".into(),
            provider_id: "openrouter".into(),
        },
    );
    let fallback = state.engine_cfg.as_ref().expect("engine config");

    let route =
        state.provider_route_for_model(&workspace_config, "conductor", fallback, None, None, false);

    assert_eq!(route.model_id, "MiniMax-M3");
}

#[tokio::test]
async fn provider_route_for_minimax_slug_uses_native_subscription_endpoint() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "fallback-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        fallback_models: Vec::new(),
    });
    let workspace_config =
        rightkit_app_settings::agent_config::ConfigLoader::new().load_files_for_workspace(FsPath::new("/tmp"));

    let route = state.provider_route_for_model(
        &workspace_config,
        "minimax/minimax-m3",
        state.engine_cfg.as_ref().unwrap(),
        None,
        None,
        false,
    );

    assert_eq!(route.base_url, "https://api.minimax.io/v1");
    assert_eq!(route.model_id, "MiniMax-M3");
}

#[tokio::test]
async fn role_session_model_override_uses_minimax_subscription_route() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "fallback-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        fallback_models: Vec::new(),
    });

    let live = state.create_session_with_options(
        "minimax-child",
        Some("researcher"),
        "/tmp",
        "MiniMax child",
        Some("parent"),
        SessionRuntimeOptions {
            model: Some("minimax/minimax-m3".into()),
            ..SessionRuntimeOptions::default()
        },
    );

    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "minimax/minimax-m3"
    );
    let route = lock_recover(
        live.model_route.as_ref().expect("dynamic model route"),
        "session.model_route",
    );
    assert_eq!(route.base_url, "https://api.minimax.io/v1");
    assert_eq!(route.model_id, "MiniMax-M3");
}

#[tokio::test]
async fn top_level_session_model_override_uses_minimax_subscription_route() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "openrouter-fallback-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        fallback_models: Vec::new(),
    });

    let live = state.create_session_with_options(
        "minimax-parent",
        None,
        "/tmp",
        "MiniMax parent",
        None,
        SessionRuntimeOptions {
            model: Some("minimax/minimax-m3".into()),
            ..SessionRuntimeOptions::default()
        },
    );

    let route = lock_recover(
        live.model_route.as_ref().expect("dynamic model route"),
        "session.model_route",
    );
    assert_eq!(route.base_url, "https://api.minimax.io/v1");
    assert_eq!(route.model_id, "MiniMax-M3");
}

fn test_driver_binding(session_id: &str, driver_kind: &str) -> rightkit_agent_engine_storage::DriverBindingRow {
    rightkit_agent_engine_storage::DriverBindingRow {
        id: format!("binding-{session_id}-{driver_kind}"),
        driver_kind: driver_kind.to_string(),
        session_id: session_id.to_string(),
        vendor_session_id: None,
        vendor_thread_id: None,
        vendor_turn_id: None,
        binary_version: "1.0.0".to_string(),
        schema_hash: "hash".to_string(),
        status: "leased".to_string(),
        last_event_key: None,
        last_event_cursor: None,
        coderight_agent_id: None,
        parent_agent_id: String::new(),
        provider_id: Some(driver_kind.to_string()),
        transport_kind: None,
        account_profile_id: None,
        auth_mode: None,
        account_fingerprint: None,
        model_id: None,
        executable_path: None,
        executable_sha256: None,
        protocol_version: None,
        profile_hash: None,
        policy_hash: None,
        tool_schema_hash: None,
        harness_attestation_id: None,
        isolation_mode: None,
        resume_strategy: None,
        context_epoch: 0,
        process_generation: 0,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_session_driver_kind_switches_native_to_codex_and_persists() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session(
        "s-driver-native-to-codex",
        None,
        "/tmp",
        "Driver switch",
        None,
    );
    wait_for_session(&storage, "s-driver-native-to-codex").await;

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-driver-native-to-codex")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"driverKind":"codex"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["driverKindUpdated"], true);
    assert_eq!(v["driverKind"], "codex");

    let row = storage
        .lock()
        .await
        .get_session("s-driver-native-to-codex")
        .unwrap()
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["driverKind"], "codex");

    // The live value matters more than the stored one: issue_capability_lease
    // compares against it, so a session that persisted "codex" while still
    // holding "native" in memory would 409 on the very next send.
    let live = state.get_session("s-driver-native-to-codex").unwrap();
    assert_eq!(
        *lock_recover(&live.driver_kind, "session.driver_kind"),
        "codex"
    );
}

#[tokio::test]
async fn patch_session_driver_kind_rejects_invalid_value() {
    let state = DaemonState::new("");
    state.create_session("s-driver-invalid", None, "/tmp", "Driver invalid", None);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-driver-invalid")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"driverKind":"bogus"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_session_driver_kind_conflicts_with_existing_binding_of_a_different_kind() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-driver-conflict", None, "/tmp", "Driver conflict", None);
    wait_for_session(&storage, "s-driver-conflict").await;
    storage
        .lock()
        .await
        .upsert_driver_binding(&test_driver_binding("s-driver-conflict", "codex"))
        .unwrap();

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-driver-conflict")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"driverKind":"claude"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let row = storage
        .lock()
        .await
        .get_session("s-driver-conflict")
        .unwrap()
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    // Creation seeds driverKind:"native" into config_json; the refused
    // patch must leave that untouched rather than partially applying.
    assert_eq!(config["driverKind"], "native");
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_session_driver_kind_same_as_existing_binding_is_not_a_conflict() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-driver-noop", None, "/tmp", "Driver noop", None);
    wait_for_session(&storage, "s-driver-noop").await;
    storage
        .lock()
        .await
        .upsert_driver_binding(&test_driver_binding("s-driver-noop", "codex"))
        .unwrap();

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/sessions/s-driver-noop")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"driverKind":"codex"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["driverKindUpdated"], true);

    let row = storage
        .lock()
        .await
        .get_session("s-driver-noop")
        .unwrap()
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["driverKind"], "codex");
}

#[tokio::test]
async fn slash_model_reports_current_session_model() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("s-slash-model", None, "/tmp", "Slash model", None);
    let app = build_router(state);

    let model = execute_slash_command(app, "/model s-slash-model").await;

    assert_eq!(model["success"], true);
    let output = model["output"].as_str().unwrap();
    assert!(
        output.contains("**Session model:** s-slash-model"),
        "{output}"
    );
    assert!(output.contains("`openrouter/default-model`"), "{output}");
    assert!(output.contains("Status: running"), "{output}");
}
