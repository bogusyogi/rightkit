#[test]
fn license_features_are_normalized() {
    let state = DaemonState::new("").with_license_features([" Brews ", "Schedules", ""]);

    assert!(state.has_license_feature("brews"));
    assert!(state.has_license_feature("schedules"));
    assert!(!state.has_license_feature("plugins"));
}

#[test]
fn an_attached_licence_gate_replaces_the_fixed_feature_set() {
    use rightkit_credentials::{Config, MemoryBackend, Namespace, Store};
    // A test build seeds the fixed Pro set; the gate, once attached, is the only source.
    let seeded = DaemonState::new("");
    assert!(seeded.has_license_feature("schedules"));

    let records = Store::open(
        Config::new(Namespace::new("cc.coderight.licence.test").unwrap())
            .backend(std::sync::Arc::new(MemoryBackend::new())),
    );
    let dir = tempfile::tempdir().unwrap();
    let gate = crate::license::LicenseGate::with_parts(
        [7_u8; 32],
        "coderight",
        dir.path(),
        records,
        "license-record",
        std::sync::Arc::new(|| Ok("dev-1".to_string())),
        rightkit_license::SystemClock,
        (std::time::Duration::ZERO, std::time::Duration::ZERO),
    );
    let gated = DaemonState::new("").with_license_gate(std::sync::Arc::new(gate));
    assert!(
        !gated.has_license_feature("schedules"),
        "no stored licence is Free"
    );
    assert!(gated.current_license_features().is_empty());
}

#[test]
fn license_feature_check_recovers_from_poisoned_mutex() {
    let state = DaemonState::new("");
    let features = state.licensed_features.clone();
    let _ = std::thread::spawn(move || {
        let mut guard = lock_recover(&features, "licensed_features");
        guard.insert("brews".to_string());
        panic!("poison licensed features");
    })
    .join();

    assert!(state.has_license_feature("brews"));
}

#[test]
fn free_workspace_registry_inherits_the_conductor_model_for_every_role() {
    let state = DaemonState::new("");
    let mut config = rightkit_app_settings::agent_config::RightKitConfig::default();
    config.orchestration.boss_model = "minimax/minimax-m3".to_string();
    config.orchestration.role_model_mode = "split".to_string();
    config.orchestration.agents.insert(
        "coder".to_string(),
        rightkit_app_settings::agent_config::AgentOverride {
            model_id: "openai/gpt-5".to_string(),
            provider_id: "openai".to_string(),
        },
    );

    let registry = state.agent_registry_with_workspace_config(&config);

    assert!(registry
        .all()
        .iter()
        .all(|agent| agent.model_id == "minimax/minimax-m3"));
    assert!(registry
        .all()
        .iter()
        .all(|agent| agent.provider_id == "minimax"));
}

#[tokio::test]
async fn orchestration_is_available_without_a_pro_license() {
    // The product rule: every tier can orchestrate. Pro buys multi-model
    // role routing, not the conductor itself — an unlicensed install must
    // run the same flow with every role on the conductor's own model.
    // This endpoint used to require `brews`, so /audit and /blueprint died
    // with a bare 402 on free installs.
    let state = state_with_session("free-orchestrate").with_license_features(Vec::<String>::new());
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/free-orchestrate/orchestrate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "goal": "audit this repo", "dryRun": true }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        response.status(),
        StatusCode::PAYMENT_REQUIRED,
        "orchestration must not be paywalled; only role-model splitting is"
    );
}

#[test]
fn subagents_never_inherit_interactive_ask_mode() {
    use rightkit_agent_policy::PermissionMode;
    // A child's approval events publish to its own conduit, which no UI
    // subscribes to. Inheriting `Ask` meant the first shell call waited for
    // an approval nobody could give: the conductor timed out after five
    // minutes and respawned, without bound. Children run AutoSafe instead —
    // read-only commands proceed, destructive ones still require approval.
    assert_eq!(
        effective_child_permission_mode_for_test(Some(PermissionMode::Ask), None),
        Some(PermissionMode::AutoSafe)
    );
    // Stricter explicit modes are preserved, never widened.
    assert_eq!(
        effective_child_permission_mode_for_test(
            Some(PermissionMode::Plan),
            Some(PermissionMode::Plan)
        ),
        Some(PermissionMode::Plan)
    );
}

#[tokio::test]
async fn interrupt_clears_pending_approvals() {
    // A run parked on a tool approval used to ignore Stop entirely: the
    // handler dropped pending questions and plans but not approvals, so the
    // tool future never unwound. Dropping the sender resolves the receiver
    // to Err, which the dispatcher reads as denied.
    let state = state_with_session("interrupt-approvals");
    let live = state.get_session("interrupt-approvals").unwrap();
    let (tx, _rx) = tokio::sync::oneshot::channel::<bool>();
    lock_recover(&live.pending_approvals, "session.pending_approvals")
        .insert("call-1".to_string(), tx);

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/interrupt-approvals/interrupt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        lock_recover(&live.pending_approvals, "session.pending_approvals").is_empty(),
        "interrupt must drop pending approvals or a parked run cannot be stopped"
    );
}

#[tokio::test]
async fn orchestrate_records_the_request_in_the_transcript() {
    // Slash commands reach the daemon via /commands/execute, so an
    // orchestrated run left NO user message: the request vanished on
    // reload and the auto-titler (which needs a user+assistant pair) could
    // never fire, so every /audit session stayed "New session".
    let state = state_with_session("orch-records");
    let live = state.get_session("orch-records").unwrap();
    let app = build_router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/orch-records/orchestrate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "goal": "Audit this repository", "dryRun": false })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let transcript = lock_recover(&live.transcript, "session.transcript").clone();
    let user_rows: Vec<_> = transcript
        .iter()
        .filter(|m| matches!(m, TranscriptMessage::User { .. }))
        .collect();
    assert_eq!(user_rows.len(), 1, "the goal must appear as a user message");
}

#[test]
fn missing_license_feature_returns_payment_required() {
    let state = DaemonState::new("").with_license_features(Vec::<String>::new());

    assert_eq!(
        require_license_feature(&state, "brews"),
        Err(StatusCode::PAYMENT_REQUIRED)
    );
}

#[tokio::test]
async fn dispatch_entry_respects_routing_feature_gate() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let direct_calls = Arc::new(AtomicUsize::new(0));
    let conduct_calls = Arc::new(AtomicUsize::new(0));
    let emitted = Arc::new(Mutex::new(Vec::<String>::new()));

    let direct_counter = direct_calls.clone();
    let conduct_counter = conduct_calls.clone();
    let emitted_decisions = emitted.clone();
    let outcome = run_dispatched_session_entry(
        false,
        "refactor the auth module across the codebase and add tests",
        None,
        move |decision| {
            emitted_decisions
                .lock()
                .unwrap()
                .push(decision.strategy_label())
        },
        move || {
            let direct_counter = direct_counter.clone();
            async move {
                direct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        move || {
            let conduct_counter = conduct_counter.clone();
            async move {
                conduct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(outcome, SessionEntryOutcome::Direct);
    assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
    assert_eq!(conduct_calls.load(Ordering::SeqCst), 0);
    assert_eq!(emitted.lock().unwrap().as_slice(), ["direct"]);

    let direct_calls = Arc::new(AtomicUsize::new(0));
    let conduct_calls = Arc::new(AtomicUsize::new(0));
    let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let direct_counter = direct_calls.clone();
    let conduct_counter = conduct_calls.clone();
    let emitted_decisions = emitted.clone();
    let outcome = run_dispatched_session_entry(
        true,
        "refactor the auth module across the codebase and add tests",
        None,
        move |decision| {
            emitted_decisions
                .lock()
                .unwrap()
                .push(decision.strategy_label())
        },
        move || {
            let direct_counter = direct_counter.clone();
            async move {
                direct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        move || {
            let conduct_counter = conduct_counter.clone();
            async move {
                conduct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(outcome, SessionEntryOutcome::Conduct);
    assert_eq!(direct_calls.load(Ordering::SeqCst), 0);
    assert_eq!(conduct_calls.load(Ordering::SeqCst), 1);
    assert_eq!(emitted.lock().unwrap().as_slice(), ["multi-agent"]);

    let direct_calls = Arc::new(AtomicUsize::new(0));
    let conduct_calls = Arc::new(AtomicUsize::new(0));
    let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let direct_counter = direct_calls.clone();
    let conduct_counter = conduct_calls.clone();
    let emitted_decisions = emitted.clone();
    let outcome = run_dispatched_session_entry(
        true,
        "what does this function do?",
        None,
        move |decision| {
            emitted_decisions
                .lock()
                .unwrap()
                .push(decision.strategy_label())
        },
        move || {
            let direct_counter = direct_counter.clone();
            async move {
                direct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        move || {
            let conduct_counter = conduct_counter.clone();
            async move {
                conduct_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(outcome, SessionEntryOutcome::Direct);
    assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
    assert_eq!(conduct_calls.load(Ordering::SeqCst), 0);
    assert_eq!(emitted.lock().unwrap().as_slice(), ["direct"]);
}

#[tokio::test]
async fn free_license_still_reaches_the_conductor_orchestration_route() {
    let state = state_with_session("free-orchestrate").with_license_features(Vec::<String>::new());
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/free-orchestrate/orchestrate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"goal": "do pro work"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Orchestration is NOT a paid feature. Pro buys multi-model role
    // routing; the conductor itself runs on every tier, collapsed onto the
    // conductor's own model when unentitled. This test previously asserted
    // a 402 here, which is what made /audit and /blueprint dead commands on
    // unlicensed installs.
    assert_ne!(response.status(), StatusCode::PAYMENT_REQUIRED);
}

#[tokio::test]
async fn free_license_rejects_schedule_creation_route() {
    let root = unique_temp_dir("free-schedule-gate");
    let root_query = encode_query_value(&root.to_string_lossy());
    let app = build_router(DaemonState::new("").with_license_features(Vec::<String>::new()));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/schedules?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "pro-only",
                        "cronExpr": "* * * * *",
                        "prompt": "run later"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let _ = std::fs::remove_dir_all(root);
}

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join("coderight-test-tmp")
        .join(format!("coderight-{label}-{nanos}"))
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::test_support::process_env_lock()
}

#[test]
fn session_openapi_components_cover_session_routes() {
    let components = session_openapi_components();
    for key in [
        "SessionRow",
        "CreateSessionRequest",
        "ForkSessionRequest",
        "ForkSessionResponse",
        "PatchSessionRequest",
        "BindSessionSpecRequest",
        "PostMessageRequest",
        "PostMessageResponse",
        "CompactSessionRequest",
        "CompactSessionResponse",
        "ResolveToolApprovalRequest",
        "ResolveToolApprovalResponse",
        "AnswerQuestionRequest",
        "AnswerQuestionResponse",
        "ResolvePlanRequest",
        "ResolvePlanResponse",
        "ResumeSessionResponse",
        "InterruptSessionResponse",
        "SteerSessionRequest",
        "SteerSessionResponse",
        "SetGoalRequest",
        "SetGoalResponse",
        "ClearGoalResponse",
        "PauseGoalResponse",
        "ResumeGoalResponse",
        "EditGoalRequest",
        "EditGoalResponse",
        "RewindSessionResponse",
        "OrchestrateSessionRequest",
        "OrchestrateSessionResponse",
        "DeleteSessionResponse",
        "SessionSpecCompletionResponse",
        "TranscriptMessage",
        "ProtocolEvent",
        "UntrustedContentResponse",
        "AwaySummaryResponse",
        "TaskNode",
        "ArtifactRow",
        "InstructionSourceRow",
        "InstructionStatus",
    ] {
        assert!(components.contains_key(key), "missing {key}");
    }
    assert_eq!(
        create_session_request_schema()["$ref"],
        "#/components/schemas/CreateSessionRequest"
    );
    assert_eq!(
        fork_session_request_schema()["$ref"],
        "#/components/schemas/ForkSessionRequest"
    );
    assert_eq!(
        fork_session_response_schema()["$ref"],
        "#/components/schemas/ForkSessionResponse"
    );
    assert_eq!(
        post_message_response_schema()["$ref"],
        "#/components/schemas/PostMessageResponse"
    );
    assert_eq!(
        orchestrate_session_request_schema()["$ref"],
        "#/components/schemas/OrchestrateSessionRequest"
    );
    assert_eq!(
        rewind_session_response_schema()["$ref"],
        "#/components/schemas/RewindSessionResponse"
    );
    assert_eq!(
        instruction_status_response_schema()["$ref"],
        "#/components/schemas/InstructionStatus"
    );
    assert_eq!(
        task_nodes_response_schema()["items"]["$ref"],
        "#/components/schemas/TaskNode"
    );
}
