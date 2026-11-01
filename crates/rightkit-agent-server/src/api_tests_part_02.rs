#[tokio::test]
async fn record_subagent_transcript_artifact_recovers_from_poisoned_live_mutexes() {
    let state = state_with_session("artifact-parent-poison");
    let parent = state
        .get_session("artifact-parent-poison")
        .expect("parent session should exist");
    let child = state.create_session(
        "artifact-child-poison",
        Some("worker"),
        "/tmp",
        "worker subtask",
        Some("artifact-parent-poison"),
    );
    let fork = build_subagent_fork(
        "artifact-child-poison".to_string(),
        "artifact-parent-poison".to_string(),
        "worker".to_string(),
        "summarize the child transcript".to_string(),
        "/tmp".to_string(),
        Some(&parent),
        &rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        true,
        None,
    );
    poison_live_transcript(&child);
    poison_live_spec_id(&parent);
    poison_live_artifacts(&parent);

    record_subagent_transcript_artifact(&state, &fork, true);

    let artifacts = lock_recover(&parent.artifacts, "session.artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].kind, "subagent_transcript");
    assert_eq!(artifacts[0].provenance, "artifact-child-poison");
}

#[tokio::test]
async fn subagent_fork_carries_single_mode_parent_model() {
    let state = state_with_session("model-parent");
    let parent = state.get_session("model-parent").unwrap();
    *lock_recover(&parent.model, "session.model") = "minimax/minimax-m3".into();
    let inherited_model = lock_recover(&parent.model, "session.model").clone();
    let fork = build_subagent_fork(
        "model-child".into(),
        "model-parent".into(),
        "researcher".into(),
        "inspect architecture".into(),
        "/tmp".into(),
        Some(&parent),
        &rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        true,
        Some(inherited_model),
    );

    assert_eq!(
        fork.runtime_options().model.as_deref(),
        Some("minimax/minimax-m3")
    );
}

#[tokio::test]
async fn subagent_fork_cannot_expand_parent_authority_tools_or_budget() {
    let state = DaemonState::new("");
    let parent = state.create_session_with_options(
        "bounded-parent",
        None,
        "/tmp",
        "bounded parent",
        None,
        SessionRuntimeOptions {
            permission_mode: Some(rightkit_agent_policy::PermissionMode::Plan),
            allowed_tools: vec!["read_file".into(), "git_diff".into()],
            disallowed_tools: vec!["shell".into()],
            budget_ceiling_usd: Some(0.02),
            ..SessionRuntimeOptions::default()
        },
    );
    let fork = build_subagent_fork(
        "bounded-child".into(),
        "bounded-parent".into(),
        "researcher".into(),
        "inspect only".into(),
        "/tmp".into(),
        Some(&parent),
        &rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
            permission_mode: Some(rightkit_agent_policy::PermissionMode::Bypass),
            allowed_tools: vec!["read_file".into(), "shell".into()],
            disallowed_tools: vec!["git_diff".into()],
            budget_microusd: Some(50_000),
            ..rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default()
        },
        true,
        None,
    );

    let options = fork.runtime_options();
    assert_eq!(
        options.permission_mode,
        Some(rightkit_agent_policy::PermissionMode::Plan)
    );
    assert_eq!(options.allowed_tools, vec!["read_file"]);
    assert_eq!(options.disallowed_tools, vec!["git_diff", "shell"]);
    assert_eq!(options.budget_ceiling_usd, Some(0.02));

    let child = state.create_session_with_options(
        "bounded-child",
        Some("researcher"),
        "/tmp",
        "bounded child",
        Some("bounded-parent"),
        options,
    );
    assert_eq!(
        child.permission_mode_tx.as_ref().map(|tx| *tx.borrow()),
        Some(rightkit_agent_policy::PermissionMode::Plan),
        "the live channel must not reset an explicitly bounded child to Bypass"
    );
    assert!(Arc::ptr_eq(&parent.lineage_budget, &child.lineage_budget));
}

#[tokio::test]
async fn sibling_sessions_share_one_atomic_lineage_budget() {
    let state = DaemonState::new("");
    let parent = state.create_session_with_options(
        "lineage-parent",
        None,
        "/tmp",
        "lineage parent",
        None,
        SessionRuntimeOptions {
            budget_ceiling_usd: Some(0.02),
            ..SessionRuntimeOptions::default()
        },
    );
    let first = state.create_session(
        "lineage-child-a",
        Some("worker"),
        "/tmp",
        "first",
        Some("lineage-parent"),
    );
    let second = state.create_session(
        "lineage-child-b",
        Some("worker"),
        "/tmp",
        "second",
        Some("lineage-parent"),
    );
    assert!(Arc::ptr_eq(&parent.lineage_budget, &first.lineage_budget));
    assert!(Arc::ptr_eq(&parent.lineage_budget, &second.lineage_budget));

    let start = Arc::new(std::sync::Barrier::new(3));
    let reserved = Arc::new(std::sync::Barrier::new(3));
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for child in [&first, &second] {
            let budget = child.lineage_budget.clone();
            let start = start.clone();
            let reserved = reserved.clone();
            let outcomes = outcomes.clone();
            scope.spawn(move || {
                start.wait();
                let reservation = budget.reserve_remaining();
                lock_recover(&outcomes, "lineage_test.outcomes").push(reservation.is_ok());
                reserved.wait();
                drop(reservation);
            });
        }
        start.wait();
        reserved.wait();
    });
    assert_eq!(
        lock_recover(&outcomes, "lineage_test.outcomes")
            .iter()
            .filter(|success| **success)
            .count(),
        1,
        "only one sibling may hold the remaining aggregate budget"
    );

    parent
        .lineage_budget
        .reserve_remaining()
        .unwrap()
        .reconcile_microusd(10_000);
    assert_eq!(parent.lineage_budget.remaining_microusd(), Some(10_000));
}

#[tokio::test]
async fn create_session_returns_existing_after_poisoned_sessions_mutex() {
    let state = state_with_session("poisoned-duplicate");
    let existing = state
        .get_session("poisoned-duplicate")
        .expect("seeded session should exist");
    let sessions = state.sessions.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&sessions, "sessions");
        panic!("poison sessions");
    })
    .join();

    let duplicate = state.create_session("poisoned-duplicate", None, "/tmp", "New title", None);

    assert!(Arc::ptr_eq(&existing, &duplicate));
}

#[tokio::test]
async fn create_scheduled_session_recovers_from_poisoned_execution_backends_mutex() {
    let state = DaemonState::new("");
    let execution_backends = state.execution_backends.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&execution_backends, "execution_backends");
        panic!("poison execution backends");
    })
    .join();

    let session = state
        .create_scheduled_session_with_toolset_policy(
            "scheduled-backend-poison",
            "/tmp",
            "Scheduled",
            None,
            Vec::new(),
            None,
        )
        .expect("scheduled session should recover poisoned execution backend registry");

    assert_eq!(session.id, "scheduled-backend-poison");
    assert_eq!(session.mcp_channel, "schedule");
}

#[tokio::test]
async fn create_recipe_session_recovers_from_poisoned_execution_backends_mutex() {
    let state = DaemonState::new("");
    let execution_backends = state.execution_backends.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&execution_backends, "execution_backends");
        panic!("poison execution backends");
    })
    .join();

    let session = state
        .create_recipe_session_with_toolset_policy(
            "recipe-backend-poison",
            "/tmp",
            "Recipe",
            None,
            Vec::new(),
            None,
        )
        .expect("recipe session should recover poisoned execution backend registry");

    assert_eq!(session.id, "recipe-backend-poison");
    assert_eq!(session.mcp_channel, "recipe");
}

#[tokio::test]
async fn create_session_recovers_backend_config_after_poisoned_execution_backends_mutex() {
    let state = DaemonState::new("");
    let execution_backends = state.execution_backends.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&execution_backends, "execution_backends");
        panic!("poison execution backends");
    })
    .join();

    let session = state.create_session_with_options(
        "session-backend-poison",
        None,
        "/tmp",
        "Backend",
        None,
        SessionRuntimeOptions {
            execution_backend: Some("local".to_string()),
            ..SessionRuntimeOptions::default()
        },
    );

    assert_eq!(session.execution_backend.as_deref(), Some("local"));
    assert_eq!(
        session.execution_network_profile.as_deref(),
        Some("local-host")
    );
}

#[tokio::test]
async fn create_session_recovers_from_poisoned_settings_mutex() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".to_string(),
        model_id: "openrouter/test-model".to_string(),
        base_url: "http://127.0.0.1:9".to_string(),
        fallback_models: Vec::new(),
    });
    let settings = state.settings.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&settings, "settings");
        panic!("poison settings");
    })
    .join();

    let session = state.create_session("settings-poison", None, "/tmp", "Settings", None);

    assert_eq!(session.id, "settings-poison");
    assert!(session.msg_tx.is_some());
}

#[tokio::test]
async fn native_harness_explicit_openai_starts_without_default_engine() {
    let state = DaemonState::new("");
    assert!(state.engine_cfg.is_none());
    let session = state.create_session_with_options(
        "openai-without-default-engine",
        None,
        "/tmp",
        "Native OpenAI",
        None,
        SessionRuntimeOptions {
            model: Some("gpt-6-astra".to_string()),
            provider_id: Some("openai".to_string()),
            ..SessionRuntimeOptions::default()
        },
    );
    assert!(session.msg_tx.is_some());
    let route = lock_recover(session.model_route.as_ref().unwrap(), "session.model_route");
    assert_eq!(route.model_id, "gpt-6-astra");
    assert_eq!(
        route.base_url,
        rightkit_llm::agent_providers::find_provider("openai")
            .unwrap()
            .base_url
    );
}

#[tokio::test]
async fn native_harness_model_switch_updates_engine_without_default() {
    let state = DaemonState::new("");
    let session = state.create_session_with_options(
        "native-model-switch",
        None,
        "/tmp",
        "Native model switch",
        None,
        SessionRuntimeOptions {
            model: Some("gpt-6-astra".into()),
            provider_id: Some("openai".into()),
            ..SessionRuntimeOptions::default()
        },
    );
    assert!(state.engine_cfg.is_none());
    assert!(update_session_route(
        &state,
        &session.id,
        "gpt-6.1-sol",
        Some("openai"),
        Some("high"),
    )
    .await
    .unwrap());
    {
        let route = lock_recover(session.model_route.as_ref().unwrap(), "session.model_route");
        assert_eq!(route.model_id, "gpt-6.1-sol");
        assert_eq!(route.effort.as_deref(), Some("high"));
    }
    assert!(update_session_effort(&state, &session.id, "low")
        .await
        .unwrap());
    let route = lock_recover(session.model_route.as_ref().unwrap(), "session.model_route");
    assert_eq!(route.model_id, "gpt-6.1-sol");
    assert_eq!(route.effort.as_deref(), Some("low"));
    assert_eq!(
        lock_recover(&session.model, "session.model").as_str(),
        route.model_id
    );
    assert_eq!(
        lock_recover(&session.provider_id, "session.provider_id").as_deref(),
        Some("openai")
    );
}

#[tokio::test]
async fn native_harness_providerless_session_remains_without_engine() {
    let state = DaemonState::new("");
    let session = state.create_session("no-native-selection", None, "/tmp", "No model", None);
    assert!(session.msg_tx.is_none());
}

#[tokio::test]
async fn create_session_recovers_engine_registry_after_poisoned_execution_backends_mutex() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".to_string(),
        model_id: "openrouter/test-model".to_string(),
        base_url: "http://127.0.0.1:9".to_string(),
        fallback_models: Vec::new(),
    });
    let execution_backends = state.execution_backends.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&execution_backends, "execution_backends");
        panic!("poison execution backends");
    })
    .join();

    let session = state.create_session(
        "engine-registry-poison",
        None,
        "/tmp",
        "Engine Registry",
        None,
    );

    assert_eq!(session.id, "engine-registry-poison");
    assert!(session.msg_tx.is_some());
}

#[test]
fn workspace_trust_for_recovers_from_poisoned_mutex() {
    let state = DaemonState::new("");
    let workspace = "C:/work/trusted";
    let trust_store = state.workspace_trust_store.clone();
    let key = workspace_trust_key(workspace);
    let _ = std::thread::spawn(move || {
        let mut store = lock_recover(&trust_store, "workspace_trust_store");
        store.projects.insert(
            key,
            crate::api_stores::WorkspaceTrustRecord {
                trust: "trusted".to_string(),
                approved_by: Some("test".to_string()),
                updated_at: "2026-06-28T00:00:00Z".to_string(),
            },
        );
        panic!("poison workspace trust");
    })
    .join();

    assert_eq!(
        state.workspace_trust_for(workspace),
        rightkit_agent_policy::WorkspaceTrust::Trusted
    );
}

#[test]
fn seed_persistent_permission_rules_recovers_from_poisoned_mutex() {
    let state = DaemonState::new("");
    let permission_store = state.permission_rule_store.clone();
    let _ = std::thread::spawn(move || {
        let mut store = lock_recover(&permission_store, "permission_rule_store");
        store
            .user
            .push(crate::api_stores::PersistentPermissionRule {
                pattern: "shell".to_string(),
                effect: "allow".to_string(),
            });
        panic!("poison permission rules");
    })
    .join();

    let mut rules = rightkit_agent_policy::RuleStore::new();
    state.seed_persistent_permission_rules(&mut rules, "C:/work/trusted");

    assert_eq!(
        rules.effective("shell"),
        rightkit_agent_policy::PermissionDecision::Allow
    );
}

#[test]
fn goal_state_from_row_round_trips_for_resume() {
    // The resume reload path depends on this mapping; a dropped/renamed field
    // would silently lose goal state across a restart.
    let row = rightkit_agent_engine_storage::GoalRow {
        id: "g1".into(),
        session_id: "s1".into(),
        text: "ship it".into(),
        status: rightkit_agent_engine_storage::GoalStatus::Paused,
        budget_usd: 5.0,
        budget_tokens: 1000,
        tokens_used: 250,
        continuation_count: 3,
        no_progress_count: 1,
        started_at: "t0".into(),
        closed_at: None,
        dismissed_at: None,
        decomposition_state: "missing".into(),
        plan_revision: 0,
        now: None,
        next: None,
        steps: Vec::new(),
        verification_state: "unverified".into(),
    };
    let gs = goal_state_from_row(row);
    assert_eq!(gs.id, "g1");
    assert_eq!(gs.text, "ship it");
    assert_eq!(gs.status, "paused");
    assert_eq!(gs.budget_usd, 5.0);
    assert_eq!(gs.budget_tokens, 1000);
    assert_eq!(gs.tokens_used, 250);
    assert_eq!(gs.continuation_count, 3);
    assert_eq!(gs.no_progress_count, 1);
    assert_eq!(gs.cost_usd, 0.0);
}

#[test]
fn apply_fable_mode_off_is_identity() {
    let (effort, prompt) = apply_fable_mode(false, Some("low".into()), Some("base prompt".into()));
    assert_eq!(effort.as_deref(), Some("low"));
    assert_eq!(prompt.as_deref(), Some("base prompt"));
}

#[test]
fn apply_fable_mode_forces_high_and_appends_directive() {
    let (effort, prompt) = apply_fable_mode(true, Some("low".into()), Some("base prompt".into()));
    assert_eq!(effort.as_deref(), Some("high"), "fable forces high effort");
    let prompt = prompt.expect("prompt present");
    assert!(prompt.starts_with("base prompt"), "base prompt preserved");
    assert!(prompt.contains("FABLE MODE"), "directive appended");
    assert!(prompt.contains("<fable_mode>"));
}

#[test]
fn apply_fable_mode_supplies_directive_when_no_base_prompt() {
    let (effort, prompt) = apply_fable_mode(true, None, None);
    assert_eq!(effort.as_deref(), Some("high"));
    assert!(prompt.unwrap().contains("FABLE MODE"));
}

#[test]
fn apply_brief_default_off_is_identity() {
    let result = apply_brief_default(false, Some("base prompt".into()));
    assert_eq!(result.as_deref(), Some("base prompt"));
}

#[test]
fn apply_brief_default_on_appends_directive() {
    let result = apply_brief_default(true, Some("base prompt".into()));
    let prompt = result.expect("prompt present");
    assert!(prompt.starts_with("base prompt"), "base prompt preserved");
    assert!(prompt.contains("<brief>"), "brief directive appended");
    assert!(prompt.contains("TERSE OUTPUT"));
}

#[test]
fn apply_brief_default_supplies_directive_when_no_base_prompt() {
    let result = apply_brief_default(true, None);
    assert!(result.unwrap().contains("<brief>"));
}

#[test]
fn compose_session_prompt_includes_brief_when_on() {
    let prompt = compose_session_system_prompt(None, None, None, None, None, None, None, None);
    // compose_session_system_prompt always starts with DEFAULT_CODING_DIRECTIVE
    let base = prompt.unwrap();
    // apply_brief_default is called externally; simulate the full pipeline:
    let with_brief = apply_brief_default(true, Some(base.clone()));
    assert!(with_brief.unwrap().contains("<brief>"));
    // Without brief:
    assert!(!base.contains("<brief>"));
}

async fn auth_status(app: &Router, method: &str, uri: &str, bearer: Option<&str>) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn mutating_routes_reject_missing_or_wrong_token() {
    const TOKEN: &str = "test-launch-token-deadbeef";
    let app = build_router(DaemonState::new(TOKEN));

    // No token → 401. The auth layer runs before the handler, so the route
    // need not resolve to a live session for the rejection to fire.
    assert_eq!(
        auth_status(&app, "GET", "/sessions", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        auth_status(&app, "POST", "/sessions/x/messages", None).await,
        StatusCode::UNAUTHORIZED
    );
    // Wrong token → 401.
    assert_eq!(
        auth_status(&app, "GET", "/sessions", Some("not-the-token")).await,
        StatusCode::UNAUTHORIZED
    );
    // Correct token → gate opens (handler runs; any non-401 status proves auth passed).
    assert_ne!(
        auth_status(&app, "GET", "/sessions", Some(TOKEN)).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn exempt_routes_bypass_token_gate() {
    let app = build_router(DaemonState::new("test-launch-token-deadbeef"));
    // Unauthenticated connectivity probe.
    assert_ne!(
        auth_status(&app, "GET", "/health", None).await,
        StatusCode::UNAUTHORIZED
    );
    // CORS preflight is never gated.
    assert_ne!(
        auth_status(&app, "OPTIONS", "/sessions", None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_create_session_same_id_yields_one_live_session() {
    let state = DaemonState::new("");
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let state = state.clone();
        tasks.push(tokio::spawn(async move {
            state.create_session("single-flight-id", None, "/tmp", "t", None)
        }));
    }
    let mut sessions = Vec::new();
    for task in tasks {
        sessions.push(task.await.expect("create task"));
    }
    for other in &sessions[1..] {
        assert!(
            Arc::ptr_eq(&sessions[0], other),
            "every concurrent create must resolve to the same live session"
        );
    }
    let live = lock_recover(&state.sessions, "sessions");
    assert_eq!(live.len(), 1);
    assert!(Arc::ptr_eq(
        live.get("single-flight-id").unwrap(),
        &sessions[0]
    ));
}
