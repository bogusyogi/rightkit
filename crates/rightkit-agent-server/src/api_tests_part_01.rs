fn state_with_session(id: &str) -> DaemonState {
    let s = DaemonState::new("");
    s.create_session(id, None, "/tmp", "Test", None);
    s
}

fn poison_live_transcript(live: &Arc<LiveSession>) {
    let transcript = live.transcript.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&transcript, "session.transcript");
        panic!("poison transcript");
    })
    .join();
}

fn poison_live_model(live: &Arc<LiveSession>) {
    let model = live.model.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&model, "session.model");
        panic!("poison model");
    })
    .join();
}

fn poison_live_model_route(live: &Arc<LiveSession>) {
    let route = live
        .model_route
        .as_ref()
        .expect("live session should have a provider route")
        .clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&route, "session.model_route");
        panic!("poison model route");
    })
    .join();
}

fn poison_live_spec_id(live: &Arc<LiveSession>) {
    let spec_id = live.spec_id.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&spec_id, "session.spec_id");
        panic!("poison spec_id");
    })
    .join();
}

fn poison_live_updated_at(live: &Arc<LiveSession>) {
    let updated_at = live.updated_at.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&updated_at, "session.updated_at");
        panic!("poison updated_at");
    })
    .join();
}

fn poison_live_goal_state(live: &Arc<LiveSession>) {
    let goal_state = live.goal_state.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&goal_state, "session.goal_state");
        panic!("poison goal_state");
    })
    .join();
}

fn poison_live_background_shells(live: &Arc<LiveSession>) {
    let background_shells = live.background_shells.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&background_shells, "session.background_shells");
        panic!("poison background_shells");
    })
    .join();
}

fn poison_live_artifacts(live: &Arc<LiveSession>) {
    let artifacts = live.artifacts.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&artifacts, "session.artifacts");
        panic!("poison artifacts");
    })
    .join();
}

fn poison_live_agent_roster(live: &Arc<LiveSession>) {
    let live = live.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&live.agent_roster, "session.agent_roster");
        panic!("poison agent_roster");
    })
    .join();
}

#[test]
fn lock_recover_returns_poisoned_mutex_inner_value() {
    let mutex = Arc::new(Mutex::new(41));
    let thread_mutex = mutex.clone();
    let _ = std::thread::spawn(move || {
        let mut guard = lock_recover(&thread_mutex, "test_mutex");
        *guard = 42;
        panic!("poison test mutex");
    })
    .join();

    let guard = lock_recover(&mutex, "test_mutex");
    assert_eq!(*guard, 42);
}

#[tokio::test]
async fn get_session_recovers_from_poisoned_sessions_mutex() {
    let state = state_with_session("poisoned-session");
    let sessions = state.sessions.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&sessions, "sessions");
        panic!("poison sessions");
    })
    .join();

    assert!(state.get_session("poisoned-session").is_some());
}

#[tokio::test]
async fn list_sessions_recovers_from_poisoned_sessions_mutex() {
    let state = state_with_session("poisoned-list-session");
    let sessions = state.sessions.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&sessions, "sessions");
        panic!("poison sessions");
    })
    .join();

    let listed = state.list_sessions();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "poisoned-list-session");
}

#[tokio::test]
async fn transcript_for_session_recovers_from_poisoned_live_transcript_mutex() {
    let state = state_with_session("poisoned-transcript");
    let live = state
        .get_session("poisoned-transcript")
        .expect("seeded session should exist");
    poison_live_transcript(&live);

    let transcript = transcript_for_session(&state, "poisoned-transcript")
        .await
        .expect("live transcript should recover poisoned mutex");

    assert_eq!(transcript.len(), 0);
}

#[tokio::test]
async fn submit_relayed_message_recovers_from_poisoned_live_transcript_mutex() {
    let state = state_with_session("relay-poison");
    let live = state
        .get_session("relay-poison")
        .expect("seeded session should exist");
    poison_live_transcript(&live);

    let seq = state
        .submit_relayed_message("relay-poison", "hello from relay")
        .await
        .expect("relayed message submission should recover poisoned transcript");

    assert!(seq > 0);
    let transcript = transcript_for_session(&state, "relay-poison")
        .await
        .expect("live transcript should remain readable after relayed submit");
    assert!(transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::User { content, pinned, .. }
            if *pinned && content.contains("hello from relay")
    )));
}

#[tokio::test]
async fn read_session_model_recovers_from_poisoned_live_model_mutex() {
    let state = state_with_session("model-read-poison");
    let live = state
        .get_session("model-read-poison")
        .expect("seeded session should exist");
    *lock_recover(&live.model, "session.model") = "openrouter/before-poison".to_string();
    poison_live_model(&live);

    let row = read_session_model(&state, "model-read-poison")
        .await
        .expect("live model read should recover poisoned model mutex");

    assert_eq!(row.model, "openrouter/before-poison");
    assert_eq!(row.status, "running");
}

#[tokio::test]
async fn update_session_model_recovers_from_poisoned_live_model_mutex() {
    let state = state_with_session("model-update-poison");
    let live = state
        .get_session("model-update-poison")
        .expect("seeded session should exist");
    *lock_recover(&live.model, "session.model") = "openrouter/before-poison".to_string();
    poison_live_model(&live);

    update_session_model(&state, "model-update-poison", "openrouter/after-poison")
        .await
        .expect("live model update should recover poisoned model mutex");

    let row = session_row(&live);
    assert_eq!(row["model"], "openrouter/after-poison");
}

#[tokio::test]
async fn update_session_model_recovers_from_poisoned_live_model_route_mutex() {
    let state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "openrouter/default-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: vec!["openrouter/fallback-model".into()],
    });
    state.create_session("model-route-poison", None, "/tmp", "Route Poison", None);
    let live = state
        .get_session("model-route-poison")
        .expect("seeded session should exist");
    poison_live_model_route(&live);

    update_session_model(
        &state,
        "model-route-poison",
        "openrouter/route-after-poison",
    )
    .await
    .expect("live model update should recover poisoned provider route mutex");

    let route = live
        .model_route
        .as_ref()
        .expect("live session should keep provider route");
    let route = lock_recover(route, "session.model_route");
    assert_eq!(route.model_id, "openrouter/route-after-poison");
    assert_eq!(
        route.fallback_models,
        vec!["openrouter/fallback-model".to_string()]
    );
}

#[tokio::test]
async fn session_spec_id_recovers_from_poisoned_live_spec_id_mutex() {
    let state = state_with_session("spec-read-poison");
    let live = state
        .get_session("spec-read-poison")
        .expect("seeded session should exist");
    *lock_recover(&live.spec_id, "session.spec_id") = Some("spec-before-poison".to_string());
    poison_live_spec_id(&live);

    let spec_id = session_spec_id(&state, "spec-read-poison")
        .await
        .expect("live spec id read should recover poisoned spec mutex");

    assert_eq!(spec_id.as_deref(), Some("spec-before-poison"));
}

#[tokio::test]
async fn update_session_spec_binding_recovers_from_poisoned_live_metadata_mutexes() {
    let state = state_with_session("spec-bind-poison");
    let live = state
        .get_session("spec-bind-poison")
        .expect("seeded session should exist");
    poison_live_spec_id(&live);
    poison_live_updated_at(&live);

    update_session_spec_binding(&state, "spec-bind-poison", Some("spec-after-poison".into()))
        .await
        .expect("live spec binding should recover poisoned metadata mutexes");

    let row = session_row(&live);
    assert_eq!(row["specId"], "spec-after-poison");
}

#[tokio::test]
async fn set_goal_status_field_recovers_from_poisoned_goal_state_mutex() {
    let state = state_with_session("goal-status-poison");
    let live = state
        .get_session("goal-status-poison")
        .expect("seeded session should exist");
    *lock_recover(&live.goal_state, "session.goal_state") =
        Some(rightkit_agent_engine_core::GoalState {
            id: "goal-poison".into(),
            text: "keep going".into(),
            status: "active".into(),
            budget_usd: 0.0,
            budget_tokens: 0,
            tokens_used: 0,
            cost_usd: 0.0,
            continuation_count: 0,
            no_progress_count: 0,
            gds_score: 0.0,
            verification_present: false,
            verification_approved: false,
            started_at: "t0".into(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        });
    poison_live_goal_state(&live);

    assert!(set_goal_status_field(&live, "paused"));
    let goal = lock_recover(&live.goal_state, "session.goal_state");
    assert_eq!(
        goal.as_ref().expect("goal state should exist").status,
        "paused"
    );
}

#[tokio::test]
async fn live_shell_statuses_recovers_from_poisoned_session_and_shell_mutexes() {
    let state = DaemonState::new("");
    let live = state.create_session("shell-status-poison", None, "/tmp", "Shells", None);
    poison_live_background_shells(&live);
    let sessions = state.sessions.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&sessions, "sessions");
        panic!("poison sessions");
    })
    .join();

    let statuses = live_shell_statuses_for_workspace(&state, FsPath::new("/tmp"));

    assert!(statuses.is_empty());
}

#[tokio::test]
async fn publish_agent_roster_recovers_from_poisoned_roster_and_goal_state_mutexes() {
    let state = state_with_session("roster-poison");
    let live = state
        .get_session("roster-poison")
        .expect("seeded session should exist");
    *lock_recover(&live.goal_state, "session.goal_state") =
        Some(rightkit_agent_engine_core::GoalState {
            id: "goal-roster-poison".into(),
            text: "keep budget meter alive".into(),
            status: "active".into(),
            budget_usd: 0.0,
            budget_tokens: 0,
            tokens_used: 123,
            cost_usd: 0.0,
            continuation_count: 0,
            no_progress_count: 0,
            gds_score: 0.0,
            verification_present: false,
            verification_approved: false,
            started_at: "t0".into(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        });
    poison_live_agent_roster(&live);
    poison_live_goal_state(&live);

    publish_agent_roster(&live);

    let goal = lock_recover(&live.goal_state, "session.goal_state");
    assert_eq!(
        goal.as_ref().expect("goal state should exist").tokens_used,
        0
    );
}
