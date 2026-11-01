#[tokio::test]
async fn permission_denials_route_records_denied_tool_results() {
    let root = unique_temp_dir("permission-denials");
    let state = DaemonState::new("").with_permission_denial_store_path(root.join("denials.json"));
    state.create_session("s1", None, "/tmp", "Session", None);
    let live = state.get_session("s1").unwrap();
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: "s1".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-06T00:00:00Z".to_string(),
        result: TranscriptMessage::Tool {
            id: "tr1".to_string(),
            created_at: "2026-06-06T00:00:00Z".to_string(),
            tool_call_id: "tc1".to_string(),
            summary:
                "permission denied: Deny by session permission rule 'shell' (rule:session:shell)"
                    .to_string(),
            output: None,
            trust: None,
            is_error: true,
            process_id: None,
            pinned: false,
        },
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/permission-denials?sessionId=s1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["denials"].as_array().unwrap().len(), 1);
    assert_eq!(v["denials"][0]["sessionId"], "s1");
    assert_eq!(v["denials"][0]["toolCallId"], "tc1");
    assert!(v["denials"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("rule:session:shell"));

    let reloaded =
        DaemonState::new("").with_permission_denial_store_path(root.join("denials.json"));
    let app = build_router(reloaded);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/permission-denials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["denials"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn transcript_records_user_message() {
    let app = build_router(state_with_session("s1"));
    // Send message
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/messages")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hello"}"#))
        .unwrap();
    app.clone().oneshot(req).await.unwrap();
    // Fetch transcript
    let msgs = fetch_transcript_json(&app, "s1").await;
    assert_eq!(msgs[0]["$kind"], "user");
    assert_eq!(msgs[0]["content"], "hello");
}

#[tokio::test]
async fn live_transcript_route_mirrors_conduit_completion_and_tool_results() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    let app = build_router(state);
    live.conduit
        .publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: "s1".to_string(),
            seq: live.conduit.next_seq(),
            created_at: "t".to_string(),
            message: TranscriptMessage::Assistant {
                id: "a-mirror".to_string(),
                created_at: "t".to_string(),
                content: "mirrored assistant".to_string(),
                reasoning_content: Some("safe literal reasoning".to_string()),
                tool_calls: vec![],
                pinned: false,
            },
        });
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: "s1".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "t".to_string(),
        result: TranscriptMessage::Tool {
            id: "tool-mirror".to_string(),
            created_at: "t".to_string(),
            tool_call_id: "tc-mirror".to_string(),
            summary: "mirrored tool".to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        },
    });

    let msgs = wait_for_transcript(&app, "s1", |messages| {
        messages
            .iter()
            .any(|msg| msg["content"] == "mirrored assistant")
            && messages.iter().any(|msg| msg["toolCallId"] == "tc-mirror")
    })
    .await;

    let assistant = msgs
        .iter()
        .find(|msg| msg["content"] == "mirrored assistant")
        .expect("assistant message mirrored");
    assert_eq!(assistant["id"], "assistant:1");
    assert_eq!(assistant["$kind"], "assistant");
    assert_eq!(assistant["content"], "mirrored assistant");
    assert_eq!(assistant["reasoningContent"], "safe literal reasoning");

    let tool = msgs
        .iter()
        .find(|msg| msg["toolCallId"] == "tc-mirror")
        .expect("tool result mirrored");
    assert_eq!(tool["id"], "tool:tc-mirror:2");
    assert_eq!(tool["$kind"], "tool");
    assert_eq!(tool["summary"], "mirrored tool");
    assert_eq!(tool["toolCallId"], "tc-mirror");
}

#[tokio::test]
async fn post_message_threads_nonce_to_user_event() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage);
    state.create_session("s1", None, "/tmp", "Test", None);
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/messages")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hello","nonce":"nonce-123"}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let live = state.get_session("s1").unwrap();
    let event = live
        .conduit
        .events_after(0)
        .into_iter()
        .find(|event| matches!(event, ProtocolEvent::UserMessageSubmitted { .. }))
        .expect("user event");
    assert!(matches!(
        event,
        ProtocolEvent::UserMessageSubmitted { text, nonce, .. }
            if text == "hello" && nonce.as_deref() == Some("nonce-123")
    ));
}

#[tokio::test]
async fn post_message_persists_typed_references_in_transcript_and_event() {
    let state = state_with_session("s1");
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                    "text":"inspect this",
                    "references":[{
                        "id":"ref-1",
                        "kind":"repository",
                        "destination":"https://github.com/stablyai/orca",
                        "provenance":{
                            "source":"composer.link",
                            "createdAt":"2026-07-21T00:00:00Z",
                            "trust":"user"
                        },
                        "actions":["open"]
                    }]
                }"#,
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let live = state.get_session("s1").unwrap();
    let transcript = lock_recover(&live.transcript, "session.transcript").clone();
    let user = transcript
            .iter()
            .find(|message| matches!(message, TranscriptMessage::User { content, .. } if content == "inspect this"))
            .expect("submitted user row");
    assert_eq!(user.user_attachments().len(), 1);
    assert_eq!(user.user_attachments()[0].kind, "repository");

    let event = live
        .conduit
        .events_after(0)
        .into_iter()
        .find(|event| matches!(event, ProtocolEvent::UserMessageSubmitted { .. }))
        .expect("user event");
    assert!(matches!(
        event,
        ProtocolEvent::UserMessageSubmitted { attachments, .. }
            if attachments.len() == 1 && attachments[0].id == "ref-1"
    ));
}

#[tokio::test]
async fn post_message_without_engine_emits_completion() {
    let state = state_with_session("s1");
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/sessions/s1/messages")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hello"}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let live = state.get_session("s1").unwrap();
    let events = live.conduit.events_after(0);
    assert!(events.iter().any(|event| {
            matches!(event, ProtocolEvent::AssistantMessageCompleted { message, .. }
                if matches!(message, TranscriptMessage::Assistant { content, .. }
                if content.contains("no model provider is active")
                        && content.contains("Configure a provider API key or a local provider route")
                        && !content.contains("fast path")))
        }));
}

#[tokio::test]
async fn interrupt_unknown_session() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/ghost/interrupt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// A parent the daemon no longer holds live must still cancel its live
/// descendants, including grandchildren.
#[tokio::test]
async fn interrupt_dormant_parent_cancels_live_descendants() {
    let state = DaemonState::new("");
    state.create_session("child", None, "/tmp", "Child", Some("dormant-parent"));
    state.create_session("grandchild", None, "/tmp", "Grandchild", Some("child"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .get_session("grandchild")
        .unwrap()
        .pending_questions
        .lock()
        .unwrap()
        .insert("q-1".to_string(), tx);
    assert_eq!(state.interrupt_live_children_of("dormant-parent"), 2);
    assert!(rx.await.is_err(), "grandchild wait must be unwound");
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/dormant-parent/interrupt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn interrupt_live_parent_cascades_to_child() {
    let state = state_with_session("p1");
    state.create_session("c1", None, "/tmp", "Child", Some("p1"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .get_session("c1")
        .unwrap()
        .pending_questions
        .lock()
        .unwrap()
        .insert("q-1".to_string(), tx);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/p1/interrupt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(rx.await.is_err());
}

#[tokio::test]
async fn interrupt_known_session() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    let (question_tx, question_rx) = tokio::sync::oneshot::channel();
    live.pending_questions
        .lock()
        .unwrap()
        .insert("q-1".to_string(), question_tx);
    let (plan_tx, plan_rx) = tokio::sync::oneshot::channel();
    live.pending_plans
        .lock()
        .unwrap()
        .insert("p-1".to_string(), plan_tx);
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/interrupt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(question_rx.await.is_err());
    assert!(plan_rx.await.is_err());
}

#[tokio::test]
async fn settings_returns_model_catalog() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/settings/orchestration")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["models"].as_array().unwrap().len() >= 15);
    assert!(v["primaryOrchestratorModelId"].as_str().is_some());
    let grok_build = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "x-ai/grok-build-0.1")
        .unwrap();
    assert_eq!(grok_build["displayName"], "Grok Build 0.1");
    assert_eq!(grok_build["provider"], "openrouter");
    assert_eq!(grok_build["modelSlug"], "x-ai/grok-build-0.1");
    assert_eq!(grok_build["contextWindow"], 256000);
    assert_eq!(grok_build["supportsVision"], true);
    assert_eq!(grok_build["supportsTools"], true);
}

#[tokio::test]
async fn events_after_zero_returns_all() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    let seq = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: "s1".into(),
        seq,
        created_at: "t".into(),
        title: None,
    });
    drop(live);
    // SSE test — just verify the route exists and returns 200
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn events_history_returns_json_for_remote_clients() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    let seq1 = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: "s1".into(),
        seq: seq1,
        created_at: "t".into(),
        title: None,
    });
    let seq2 = live.conduit.next_seq();
    live.conduit
        .publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: "s1".into(),
            seq: seq2,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: "a-remote".into(),
                created_at: "t".into(),
                content: "remote status".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        });
    drop(live);

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/sessions/s1/events/history?after={seq1}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["$kind"], "assistantMessageCompleted");
    assert_eq!(events[0]["message"]["content"], "remote status");
}

#[test]
fn kind_matches_no_filter_passes_everything() {
    let data = r#"{"$kind":"assistantDelta","text":"hi"}"#;
    assert!(kind_matches(data, &None));
}

#[test]
fn kind_matches_correct_kind_passes() {
    let data = r#"{"$kind":"assistantDelta","text":"hi"}"#;
    assert!(kind_matches(data, &Some("assistantDelta".into())));
}

#[test]
fn kind_matches_wrong_kind_blocks() {
    let data = r#"{"$kind":"assistantDelta","text":"hi"}"#;
    assert!(!kind_matches(data, &Some("sessionStarted".into())));
}

#[test]
fn kind_matches_nonexistent_kind_blocks() {
    let data = r#"{"$kind":"assistantDelta","text":"hi"}"#;
    assert!(!kind_matches(data, &Some("nonexistent".into())));
}

/// CQ-08: when the transcript/tasks/usage projection subscriber falls more
/// than a broadcast window behind, it must catch up from the durable event
/// log rather than silently serving a partial projection.
#[tokio::test]
async fn lagged_projection_rebuilds_transcript_from_durable_log() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage);
    let session_id = "projection-lag-fixture";
    let projection_gate = crate::api::api_session_utils::evt003_pause_projection(session_id);
    state.create_session(session_id, None, "/tmp", "Test", None);
    let live = state.get_session(session_id).unwrap();

    // Hold projection until durable facts have committed, then evict their
    // broadcasts with live deltas. Recovery must use SQLite replay.
    for i in 0..3 {
        live.conduit
            .publish(ProtocolEvent::AssistantMessageCompleted {
                session_id: session_id.to_string(),
                seq: live.conduit.next_seq(),
                created_at: "t".to_string(),
                message: TranscriptMessage::Assistant {
                    id: format!("a{i}"),
                    created_at: "t".to_string(),
                    content: format!("fact-{i}"),
                    reasoning_content: None,
                    tool_calls: vec![],
                    pinned: false,
                },
            });
    }
    crate::api::api_session_utils::await_event_persistence(live.persistence_state.clone(), 3)
        .await
        .expect("durable facts should persist while projection is paused");
    assert!(lock_recover(&live.transcript, "session.transcript").is_empty());
    for i in 0..600 {
        live.conduit.publish(ProtocolEvent::AssistantDelta {
            session_id: session_id.to_string(),
            // Live projections are anchored by Conduit and do not consume
            // durable sequence numbers.
            seq: 0,
            created_at: "t".to_string(),
            text: format!("d{i}"),
            is_reasoning: false,
        });
    }

    projection_gate.notify_one();

    // Poll: the projection must rebuild from the durable log and apply all
    // three facts despite missing their live broadcasts.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let len = lock_recover(&live.transcript, "session.transcript").len();
        if len == 3 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "projection never caught up; transcript len {len}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    {
        let transcript = lock_recover(&live.transcript, "session.transcript");
        for i in 0..3 {
            assert!(
                transcript.iter().any(|msg| matches!(
                    msg,
                    TranscriptMessage::Assistant { content, .. } if content == &format!("fact-{i}")
                )),
                "missing rebuilt fact-{i}"
            );
        }
    }

    // A post-rebuild fact must apply exactly once (no duplicate from the
    // durable tail overlapping the live broadcast).
    let tail_seq = live.conduit.next_seq();
    assert_eq!(
        tail_seq, 4,
        "live deltas must not reserve durable sequences"
    );
    live.conduit
        .publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: session_id.to_string(),
            seq: tail_seq,
            created_at: "t".to_string(),
            message: TranscriptMessage::Assistant {
                id: "a-tail".to_string(),
                created_at: "t".to_string(),
                content: "fact-tail".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        });
    // This delta broadcasts before the queued durable tail commits. Its
    // anchor must not advance the durable replay cursor past that fact.
    live.conduit.publish(ProtocolEvent::AssistantDelta {
        session_id: session_id.to_string(),
        seq: 0,
        created_at: "t".to_string(),
        text: "tail-inflight".to_string(),
        is_reasoning: false,
    });
    crate::api::api_session_utils::await_event_persistence(
        live.persistence_state.clone(),
        tail_seq,
    )
    .await
    .expect("tail fact should persist and publish before projection assertion");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let transcript = lock_recover(&live.transcript, "session.transcript");
        if transcript.iter().any(|msg| {
            matches!(
                msg,
                TranscriptMessage::Assistant { content, .. } if content == "fact-tail"
            )
        }) {
            assert_eq!(transcript.len(), 4, "durable tail must not re-apply");
            break;
        }
        drop(transcript);
        assert!(
            std::time::Instant::now() < deadline,
            "tail fact never applied"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
