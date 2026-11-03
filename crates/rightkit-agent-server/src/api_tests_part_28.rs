#[tokio::test]
async fn slash_model_updates_live_session_route() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("s-slash-model-update", None, "/tmp", "Slash model", None);
    let app = build_router(state.clone());

    let model =
        execute_slash_command(app, "/model s-slash-model-update openrouter/next-model").await;

    assert_eq!(model["success"], true);
    let output = model["output"].as_str().unwrap();
    assert!(
        output.contains("**Session model updated:** s-slash-model-update"),
        "{output}"
    );
    assert!(output.contains("Applies to: live session"), "{output}");
    let live = state.get_session("s-slash-model-update").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "openrouter/next-model"
    );
    let route = live.model_route.as_ref().expect("dynamic provider route");
    assert_eq!(
        lock_recover(route, "session.model_route").model_id,
        "openrouter/next-model"
    );
}

#[tokio::test]
async fn slash_model_uses_active_session_context_for_read() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("s-active-model", None, "/tmp", "Active model", None);
    let app = build_router(state);

    let model = execute_slash_command_for_session(app, "/model", "s-active-model").await;

    assert_eq!(model["success"], true);
    let output = model["output"].as_str().unwrap();
    assert!(
        output.contains("**Session model:** s-active-model"),
        "{output}"
    );
    assert!(output.contains("`openrouter/default-model`"), "{output}");
}

#[tokio::test]
async fn slash_model_updates_active_session_context_route() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("s-active-model-update", None, "/tmp", "Active model", None);
    let app = build_router(state.clone());

    let model = execute_slash_command_for_session(
        app,
        "/model openrouter/next-model",
        "s-active-model-update",
    )
    .await;

    assert_eq!(model["success"], true);
    let output = model["output"].as_str().unwrap();
    assert!(
        output.contains("**Session model updated:** s-active-model-update"),
        "{output}"
    );
    assert!(output.contains("Applies to: live session"), "{output}");
    let live = state.get_session("s-active-model-update").unwrap();
    assert_eq!(
        lock_recover(&live.model, "session.model").as_str(),
        "openrouter/next-model"
    );
    let route = live.model_route.as_ref().expect("dynamic provider route");
    assert_eq!(
        lock_recover(route, "session.model_route").model_id,
        "openrouter/next-model"
    );
}

#[tokio::test]
async fn slash_model_rejects_subagent_sessions() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    state.create_session("parent-slash-model", None, "/tmp", "Parent", None);
    state.create_session(
        "child-slash-model",
        Some("coder"),
        "/tmp",
        "Child",
        Some("parent-slash-model"),
    );
    let app = build_router(state);

    let model = execute_slash_command(app, "/model child-slash-model openrouter/next-model").await;

    assert_eq!(model["success"], false);
    let output = model["output"].as_str().unwrap();
    assert!(
        output.contains("sub-agent route is settings-only"),
        "{output}"
    );
}

#[tokio::test]
async fn memory_routes_add_and_search_entries() {
    let memory = crate::test_support::FakeMemoryBackend::new();
    let provenance =
        rightkit_memory::memory_provenance::MemoryProvenanceStore::from_backend(memory.clone());
    let app = build_router(
        DaemonState::new("")
            .with_memory_backend(memory)
            .with_memory_provenance(provenance),
    );
    let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/memory")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{
                            "content": "When touching the scheduler, run cargo test -p coderight scheduler.",
                            "keywords": ["scheduler", "cargo"]
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(create.status(), StatusCode::OK);

    let list = app
        .oneshot(
            Request::builder()
                .uri("/memory?query=scheduler")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let body = list.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["query"], "scheduler");
    assert_eq!(value["entries"][0]["keywords"][0], "scheduler");
    assert!(value["entries"][0]["content"]
        .as_str()
        .unwrap()
        .contains("scheduler"));
    assert!(value["entries"][0].get("embedding").is_none());
}

#[tokio::test]
async fn telegram_status_redacts_token_and_reports_bindings() {
    let binding_path = std::env::temp_dir().join(format!(
        "coderight-telegram-bindings-{}.json",
        std::process::id()
    ));
    std::fs::write(
        &binding_path,
        r#"{"chats":{"12345":"default","67890":"review"}}"#,
    )
    .unwrap();
    let mut cfg = rightkit_app_settings::agent_config::RemoteConfig::default();
    cfg.telegram_token = Some("secret-token".to_string());
    cfg.telegram_allowed_users = vec![12345, 67890];
    let mut state = DaemonState::new("").with_telegram_remote_config(cfg);
    state.telegram_status.binding_store_path = binding_path.clone();
    {
        let mut runtime = lock_recover(&state.telegram_status.runtime, "telegram_status.runtime");
        runtime.started_at = Some("unix:1".to_string());
        runtime.last_poll_at = Some("unix:2".to_string());
        runtime.last_success_at = Some("unix:3".to_string());
        runtime.update_offset = 42;
        runtime.active_chat_count = 2;
        runtime.chat_cursors = vec![
            crate::telegram_task::TelegramChatCursorStatus {
                chat_label: "chat-1".to_string(),
                session_id: "default".to_string(),
                last_pushed_seq: 10,
            },
            crate::telegram_task::TelegramChatCursorStatus {
                chat_label: "chat-2".to_string(),
                session_id: "review".to_string(),
                last_pushed_seq: 20,
            },
        ];
    }
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/remote/telegram/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("secret-token"));
    assert!(!raw.contains("12345"));
    assert!(!raw.contains("67890"));
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["state"], "long_poll_requested");
    assert_eq!(body["tokenConfigured"], true);
    assert_eq!(body["allowedUserCount"], 2);
    assert_eq!(body["bindingCount"], 2);
    assert_eq!(body["bindingStoreReadable"], true);
    assert_eq!(body["runtime"]["lastPollAt"], "unix:2");
    assert_eq!(body["runtime"]["lastSuccessAt"], "unix:3");
    assert_eq!(body["runtime"]["updateOffset"], 42);
    assert_eq!(body["runtime"]["activeChatCount"], 2);
    assert_eq!(body["runtime"]["chatCursors"][0]["chat"], "chat-1");
    assert_eq!(body["runtime"]["chatCursors"][0]["lastPushedSeq"], 10);

    let _ = std::fs::remove_file(binding_path);
}

#[tokio::test]
async fn telegram_test_blocks_when_remote_is_not_configured() {
    let state = DaemonState::new("");
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/remote/telegram/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["verdict"], "block");
    assert_eq!(body["state"], "not_configured");
    assert_eq!(body["checks"][0]["id"], "telegram_token");
    assert_eq!(body["checks"][0]["status"], "block");
    assert_eq!(body["checks"][1]["id"], "allowed_users");
    assert_eq!(body["checks"][1]["status"], "block");
}

#[tokio::test]
async fn telegram_test_passes_when_configured_and_poll_succeeded_without_secret_echo() {
    let binding_path = std::env::temp_dir().join(format!(
        "coderight-telegram-test-bindings-{}.json",
        std::process::id()
    ));
    std::fs::write(
        &binding_path,
        r#"{"chats":{"12345":"default","67890":"session-2"}}"#,
    )
    .unwrap();
    let mut cfg = rightkit_app_settings::agent_config::RemoteConfig::default();
    cfg.telegram_token = Some("secret-token".to_string());
    cfg.telegram_allowed_users = vec![12345];
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let store = storage.lock().await;
        store
            .upsert_remote_binding(&rightkit_agent_engine_storage::RemoteBindingRow {
                id: "telegram-binding-1".to_string(),
                channel_id: "telegram".to_string(),
                target_key: "opaque-chat-key-1".to_string(),
                session_id: Some("default".to_string()),
                target_label: Some("chat-1".to_string()),
                target_redacted: Some("telegram-chat:<redacted-1>".to_string()),
                status: "active".to_string(),
                created_at: "unix:1".to_string(),
                updated_at: "unix:2".to_string(),
            })
            .unwrap();
        store
            .upsert_remote_binding(&rightkit_agent_engine_storage::RemoteBindingRow {
                id: "telegram-binding-2".to_string(),
                channel_id: "telegram".to_string(),
                target_key: "opaque-chat-key-2".to_string(),
                session_id: Some("session-2".to_string()),
                target_label: Some("chat-2".to_string()),
                target_redacted: Some("telegram-chat:<redacted-2>".to_string()),
                status: "active".to_string(),
                created_at: "unix:1".to_string(),
                updated_at: "unix:2".to_string(),
            })
            .unwrap();
    }
    let mut state = DaemonState::new("")
        .with_telegram_remote_config(cfg)
        .with_storage(storage);
    state.telegram_status.binding_store_path = binding_path.clone();
    {
        let mut runtime = lock_recover(&state.telegram_status.runtime, "telegram_status.runtime");
        runtime.started_at = Some("unix:1".to_string());
        runtime.last_poll_at = Some("unix:2".to_string());
        runtime.last_success_at = Some("unix:3".to_string());
    }
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/remote/telegram/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("secret-token"));
    assert!(!raw.contains("12345"));
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["verdict"], "pass");
    assert_eq!(body["state"], "long_poll_requested");
    assert_eq!(body["bindingCount"], 2);
    assert_eq!(body["checks"][0]["status"], "pass");
    assert_eq!(body["checks"][1]["status"], "pass");
    assert_eq!(body["checks"][2]["status"], "pass");
    assert_eq!(body["checks"][3]["status"], "pass");

    let _ = std::fs::remove_file(binding_path);
}
