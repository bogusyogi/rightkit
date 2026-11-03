#[tokio::test]
async fn events_sse_without_kind_filter_returns_200() {
    let state = state_with_session("s2");
    let live = state.get_session("s2").unwrap();
    let seq = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: "s2".into(),
        seq,
        created_at: "t".into(),
        title: None,
    });
    let seq2 = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::AssistantDelta {
        session_id: "s2".into(),
        seq: seq2,
        created_at: "t".into(),
        text: "hello".into(),
        is_reasoning: false,
    });
    drop(live);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s2/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn events_sse_with_kind_filter_returns_200() {
    let state = state_with_session("s3");
    let live = state.get_session("s3").unwrap();
    let seq = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: "s3".into(),
        seq,
        created_at: "t".into(),
        title: None,
    });
    let seq2 = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::AssistantDelta {
        session_id: "s3".into(),
        seq: seq2,
        created_at: "t".into(),
        text: "hello".into(),
        is_reasoning: false,
    });
    drop(live);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s3/events?kind=assistantDelta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn events_sse_with_nonexistent_kind_returns_200() {
    let state = state_with_session("s4");
    let live = state.get_session("s4").unwrap();
    let seq = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: "s4".into(),
        seq,
        created_at: "t".into(),
        title: None,
    });
    drop(live);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s4/events?kind=nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn steer_unknown_session_returns_not_found() {
    let app = build_router(DaemonState::new(""));
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/ghost/steer")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"nudge"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn steer_known_session_returns_ok() {
    let app = build_router(state_with_session("s1"));
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/steer")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"nudge"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["steered"], true);
}

#[tokio::test]
async fn set_goal_and_clear_goal() {
    let app = build_router(state_with_session("s1"));
    // Set goal
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/goal")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"finish the feature"}"#))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["set"], true);
    // Clear goal
    let req = Request::builder()
        .method("DELETE")
        .uri("/sessions/s1/goal")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cleared"], true);
}

#[tokio::test]
async fn replacing_goal_dismisses_prior_goal_without_waiting_on_storage() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-replace", None, "/tmp", "Goal session", None);
    wait_for_session(&storage, "s-replace").await;
    let app = build_router(state);
    for text in ["first", "second"] {
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            app.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/s-replace/goal")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"text":"{text}"}}"#)))
                    .unwrap(),
            ),
        )
        .await
        .expect("replacement must not deadlock")
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let store = storage.lock().await;
    let first = store.active_goal_for_session("s-replace").unwrap().unwrap();
    assert_eq!(first.text, "second");
    assert!(store
        .newest_displayable_goal_for_session("s-replace")
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn clear_goal_preserves_terminal_status() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-terminal", None, "/tmp", "Goal session", None);
    wait_for_session(&storage, "s-terminal").await;
    let app = build_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s-terminal/goal")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"ship it"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let live = state.get_session("s-terminal").unwrap();
    let goal_id = lock_recover(&live.goal_state, "session.goal_state")
        .as_ref()
        .unwrap()
        .id
        .clone();
    {
        let mut goal = lock_recover(&live.goal_state, "session.goal_state");
        let goal = goal.as_mut().unwrap();
        goal.status = "achieved".into();
        goal.closed_at = Some("terminal".into());
        goal.verification_state = "verified".into();
    }
    storage
        .lock()
        .await
        .update_goal_status(
            &goal_id,
            rightkit_agent_engine_storage::GoalStatus::Achieved,
            Some("terminal"),
        )
        .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/sessions/s-terminal/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let persisted = storage.lock().await.get_goal(&goal_id).unwrap().unwrap();
    assert_eq!(
        persisted.status,
        rightkit_agent_engine_storage::GoalStatus::Achieved
    );
    assert!(persisted.dismissed_at.is_some());
}

#[tokio::test]
async fn get_goal_snapshot_returns_null_then_live_goal() {
    let app = build_router(state_with_session("s1"));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["goal"].is_null());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/goal")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"finish"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["goal"]["objective"], "finish");
    assert!(value["goal"]["completionPercent"].is_null());
}

#[tokio::test]
async fn get_goal_snapshot_reads_persisted_only_session() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s-persisted", None, "/tmp", "Goal session", None);
    wait_for_session(&storage, "s-persisted").await;
    let app = build_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s-persisted/goal")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"persisted goal"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    lock_recover(&state.sessions, "sessions").remove("s-persisted");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/s-persisted/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let goal = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["goal"].clone();
    assert_eq!(goal["objective"], "persisted goal");
    assert_eq!(goal["status"], "active");
    assert!(goal["completionPercent"].is_null());

    state.create_session("s-empty", None, "/tmp", "Empty session", None);
    wait_for_session(&storage, "s-empty").await;
    lock_recover(&state.sessions, "sessions").remove("s-empty");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/s-empty/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["goal"].is_null());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/sessions/missing/goal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rewind_unknown_session_returns_not_found() {
    let app = build_router(DaemonState::new(""));
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/ghost/rewind")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn goal_set_pause_resume_drives_shared_state() {
    let state = state_with_session("s1");
    let app = build_router(state.clone());
    // Set a goal with a token budget.
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/goal")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"ship it","budgetTokens":5000}"#))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.get_session("s1").unwrap();
        let guard = lock_recover(&live.goal_state, "session.goal_state");
        let gs = guard.as_ref().expect("goal state created");
        assert_eq!(gs.status, "active");
        assert_eq!(gs.budget_tokens, 5000);
    }
    // Pause → shared state flips to paused (engine stops continuing).
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/goal/pause")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        {
            let live = state.get_session("s1").unwrap();
            let status = lock_recover(&live.goal_state, "session.goal_state")
                .as_ref()
                .unwrap()
                .status
                .clone();
            status
        },
        "paused"
    );
    // Resume → back to active.
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/goal/resume")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        {
            let live = state.get_session("s1").unwrap();
            let status = lock_recover(&live.goal_state, "session.goal_state")
                .as_ref()
                .unwrap()
                .status
                .clone();
            status
        },
        "active"
    );
}

#[tokio::test]
async fn goal_resume_resets_gap_audit_and_persists_active_status() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s1", None, "/tmp", "Goal session", None);
    wait_for_session(&storage, "s1").await;
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/goal")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"ship it"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let live = state.get_session("s1").unwrap();
    let goal_id = lock_recover(&live.goal_state, "session.goal_state")
        .as_ref()
        .unwrap()
        .id
        .clone();
    {
        let gap = rightkit_agent_engine_core::UnresolvedGoalGap {
            criterion_ids: vec!["criterion-1".into()],
            evidence_ids: vec!["src/lib.rs:1".into()],
            fallback_lines: Vec::new(),
        };
        let mut tracker = lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker");
        tracker.observe(&gap, false);
        tracker.observe(&gap, false);
    }
    storage
        .lock()
        .await
        .update_goal_status(
            &goal_id,
            rightkit_agent_engine_storage::GoalStatus::Paused,
            None,
        )
        .unwrap();
    set_goal_status_field(&live, "paused");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/goal/resume")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").same_gap_count(),
        0
    );
    assert_eq!(
        storage
            .lock()
            .await
            .get_goal(&goal_id)
            .unwrap()
            .unwrap()
            .status,
        rightkit_agent_engine_storage::GoalStatus::Active
    );

    let gap = rightkit_agent_engine_core::UnresolvedGoalGap {
        criterion_ids: vec!["criterion-2".into()],
        evidence_ids: Vec::new(),
        fallback_lines: Vec::new(),
    };
    lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").observe(&gap, false);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/goal/edit")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"text":"ship the revised goal","budgetTokens":4321,"budgetUsd":2.5}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").same_gap_count(),
        0
    );
    let persisted = storage.lock().await.get_goal(&goal_id).unwrap().unwrap();
    assert_eq!(persisted.text, "ship the revised goal");
    assert_eq!(persisted.budget_tokens, 4321);
    assert!((persisted.budget_usd - 2.5).abs() < f64::EPSILON);

    let restarted = DaemonState::new("").with_storage(storage.clone());
    let resumed = restarted
        .resume_session_with_ws("s1", None)
        .await
        .expect("persisted session should resume");
    let resumed_goal = lock_recover(&resumed.goal_state, "session.goal_state")
        .clone()
        .expect("edited active goal should survive restart");
    assert_eq!(resumed_goal.status, "active");
    assert_eq!(resumed_goal.text, "ship the revised goal");
    assert_eq!(resumed_goal.budget_tokens, 4321);
    assert!((resumed_goal.budget_usd - 2.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn agents_route_lists_roster_rows_with_usage() {
    let state = state_with_session("p");
    {
        let parent = state.get_session("p").unwrap();
        let mut roster = lock_recover(&parent.agent_roster, "session.agent_roster");
        roster.ensure("w1", "reviewer", "test/model");
        roster.ensure("w2", "skeptic", "test/model");
        roster.record_usage("w1", 100, 20, 0);
        roster.record_tool("w1", "read_file");
    }
    let app = build_router(state);
    let req = Request::builder()
        .method("GET")
        .uri("/sessions/p/agents")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["subSessionId"], "w1");
    assert_eq!(arr[0]["tokensIn"], 100);
    assert_eq!(arr[0]["toolUses"], 1);
}

#[tokio::test]
async fn compare_request_validates_required_fields() {
    let app = build_router(DaemonState::new(""));
    // Missing both models and task (invalid request)
    let req = Request::builder()
        .method("POST")
        .uri("/runtime/compare")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"prompt":"test"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compare_request_rejects_zero_max_turns() {
    let app = build_router(DaemonState::new(""));
    let req = Request::builder()
        .method("POST")
        .uri("/runtime/compare")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"task":"test","variantA":"coder","variantB":"engineer","maxTurns":0}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn compare_request_deserializes_model_mode() {
    let json = r#"{"prompt":"test prompt","models":["model-a","model-b"]}"#;
    let req: CompareRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.prompt, Some("test prompt".into()));
    assert_eq!(req.models, Some(vec!["model-a".into(), "model-b".into()]));
    assert_eq!(req.task, None);
    assert_eq!(req.variant_a, None);
}

#[test]
fn compare_request_deserializes_agent_mode() {
    let json = r#"{"task":"implement feature X","variantA":"coder","variantB":"engineer","workspacePath":"/tmp/ws"}"#;
    let req: CompareRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.task, Some("implement feature X".into()));
    assert_eq!(req.variant_a, Some("coder".into()));
    assert_eq!(req.variant_b, Some("engineer".into()));
    assert_eq!(req.workspace_path, Some("/tmp/ws".into()));
    assert_eq!(req.max_turns, None);
    assert_eq!(req.prompt, None);
}

#[test]
fn compare_request_deserializes_max_turns() {
    let json = r#"{"task":"implement feature X","variantA":"coder","variantB":"engineer","workspacePath":"/tmp/ws","maxTurns":5}"#;
    let req: CompareRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.max_turns, Some(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_parent_cancels_and_purges_entire_session_tree() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("parent", None, "/tmp", "Parent", None);
    let child = state.create_session("child", Some("researcher"), "/tmp", "Child", Some("parent"));
    let grandchild = state.create_session(
        "grandchild",
        Some("reviewer"),
        "/tmp",
        "Grandchild",
        Some("child"),
    );
    wait_for_session(&storage, "parent").await;
    wait_for_session(&storage, "child").await;
    wait_for_session(&storage, "grandchild").await;

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/sessions/parent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(child.interrupt_tx.is_some());
    assert!(grandchild.interrupt_tx.is_some());
    assert!(state.list_sessions().is_empty());
    {
        let store = storage.lock().await;
        assert!(store.list_sessions().unwrap().is_empty());
    }

    // A late event from an already-interrupted child must not resurrect any
    // durable history after the operator deleted its parent task.
    child
        .conduit
        .publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: "child".to_string(),
            seq: child.conduit.next_seq(),
            created_at: now_iso(),
            message: TranscriptMessage::Assistant {
                id: "late".to_string(),
                created_at: now_iso(),
                content: "late completion".to_string(),
                tool_calls: Vec::new(),
                reasoning_content: None,
                pinned: false,
            },
        });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    let store = storage.lock().await;
    assert!(store.events_after("child", 0).unwrap().is_empty());
}
