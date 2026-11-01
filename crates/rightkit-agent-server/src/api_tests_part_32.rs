#[tokio::test]
async fn operator_resolution_routes_reject_lower_trust_sources() {
    let state = DaemonState::new("");
    let live = state.create_session("s1", None, "D:/work", "Trusted resolution", None);
    let (approval_tx, _approval_rx) = tokio::sync::oneshot::channel();
    lock_recover(&live.pending_approvals, "session.pending_approvals")
        .insert("tc-1".to_string(), approval_tx);
    let (question_tx, _question_rx) = tokio::sync::oneshot::channel();
    lock_recover(&live.pending_questions, "session.pending_questions")
        .insert("q-1".to_string(), question_tx);
    let (plan_tx, _plan_rx) = tokio::sync::oneshot::channel();
    lock_recover(&live.pending_plans, "session.pending_plans")
        .insert("plan-1".to_string(), plan_tx);
    let app = build_router(state);

    let approval = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/tool-approval")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(
                    serde_json::json!({
                        "toolCallId": "tc-1",
                        "approved": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(approval.status(), StatusCode::FORBIDDEN);
    assert!(
        lock_recover(&live.pending_approvals, "session.pending_approvals").contains_key("tc-1")
    );

    let question = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/answer-question")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "toolUntrusted")
                .body(Body::from(
                    serde_json::json!({
                        "questionId": "q-1",
                        "answers": {"Choice": "yes"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(question.status(), StatusCode::FORBIDDEN);
    assert!(lock_recover(&live.pending_questions, "session.pending_questions").contains_key("q-1"));

    let plan = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/resolve-plan")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "untrusted")
                .body(Body::from(
                    serde_json::json!({
                        "planId": "plan-1",
                        "approved": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plan.status(), StatusCode::FORBIDDEN);
    assert!(lock_recover(&live.pending_plans, "session.pending_plans").contains_key("plan-1"));
}

fn pending_approval(
    live: &LiveSession,
    call_id: &str,
    request_id: Option<&str>,
) -> tokio::sync::oneshot::Receiver<bool> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    lock_recover(&live.pending_approvals, "approval regression sender").insert(call_id.into(), tx);
    lock_recover(
        &live.pending_approval_contexts,
        "approval regression context",
    )
    .insert(
        call_id.into(),
        rightkit_agent_engine_protocol::PendingApprovalContext {
            request_id: request_id.map(str::to_owned),
            tool_name: "shell".into(),
            command: Some("echo approval".into()),
            network: None,
            payload_digest: None,
        },
    );
    rx
}

#[tokio::test]
async fn stale_or_duplicate_amendment_leaves_rules_unchanged() {
    let state = state_with_session("approval-stale-amendment");
    let live = state.get_session("approval-stale-amendment").unwrap();
    let before = lock_recover(&live.permission_rules, "rules")
        .all_rules()
        .len();
    let app = build_router(state);
    let body = serde_json::json!({"toolCallId":"missing-amendment", "approved":true, "decision":{"approved_execpolicy_amendment":{"proposed_execpolicy_amendment":["rm","-rf"]}}});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-stale-amendment/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        lock_recover(&live.permission_rules, "rules")
            .all_rules()
            .len(),
        before
    );
}

#[tokio::test]
async fn valid_session_approval_adds_scoped_rule_and_duplicate_is_inert() {
    let state = state_with_session("approval-session-scope");
    let live = state.get_session("approval-session-scope").unwrap();
    let rx = pending_approval(&live, "session-call", None);
    let before = lock_recover(&live.permission_rules, "rules")
        .all_rules()
        .len();
    let app = build_router(state);
    let body = serde_json::json!({"toolCallId":"session-call", "approved":true, "decision":"approved_for_session"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-session-scope/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(rx.await.unwrap());
    assert_eq!(
        lock_recover(&live.permission_rules, "rules")
            .all_rules()
            .len(),
        before + 1
    );
    let duplicate = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-session-scope/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        lock_recover(&live.permission_rules, "rules")
            .all_rules()
            .len(),
        before + 1
    );
}

#[tokio::test]
async fn structured_approval_without_context_is_rejected_without_rule_mutation() {
    let state = state_with_session("approval-missing-context");
    let live = state.get_session("approval-missing-context").unwrap();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    lock_recover(&live.pending_approvals, "sender").insert("no-context".into(), tx);
    let before = lock_recover(&live.permission_rules, "rules")
        .all_rules()
        .len();
    let app = build_router(state);
    let body = serde_json::json!({"toolCallId":"no-context", "approved":true, "decision":"approved_for_session"});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-missing-context/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        lock_recover(&live.permission_rules, "rules")
            .all_rules()
            .len(),
        before
    );
}

#[tokio::test]
async fn mismatched_request_id_preserves_pending_approval() {
    let state = state_with_session("approval-request-id");
    let live = state.get_session("approval-request-id").unwrap();
    let _rx = pending_approval(&live, "request-bound", Some("request-live"));
    let app = build_router(state);
    let body = serde_json::json!({"toolCallId":"request-bound", "requestId":"request-stale", "approved":true});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-request-id/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(lock_recover(&live.pending_approvals, "sender").contains_key("request-bound"));
}

#[tokio::test]
async fn closed_amendment_receiver_leaves_rules_unchanged() {
    let state = state_with_session("approval-closed-amendment");
    let live = state.get_session("approval-closed-amendment").unwrap();
    let rx = pending_approval(&live, "closed-amendment", None);
    drop(rx);
    let before = lock_recover(&live.permission_rules, "rules")
        .all_rules()
        .len();
    let app = build_router(state);
    let body = serde_json::json!({"toolCallId":"closed-amendment", "approved":true, "decision":{"approved_execpolicy_amendment":{"proposed_execpolicy_amendment":["rm","-rf"]}}});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/approval-closed-amendment/tool-approval")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        lock_recover(&live.permission_rules, "rules")
            .all_rules()
            .len(),
        before
    );
}

#[tokio::test]
async fn session_control_routes_reject_lower_trust_sources() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    *lock_recover(&live.goal, "session.goal") = Some("trusted-goal".to_string());
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::Assistant {
        id: "assistant-1".to_string(),
        created_at: "t".to_string(),
        content: "keep this context".to_string(),
        tool_calls: Vec::new(),
        reasoning_content: None,
        pinned: false,
    });
    let app = build_router(state.clone());

    let cases = [
        ("DELETE", "/sessions/s1", None),
        ("POST", "/sessions/s1/kill", None),
        ("POST", "/sessions/s1/resume", None),
        ("POST", "/sessions/s1/interrupt", None),
        ("POST", "/sessions/s1/compact", None),
        ("POST", "/sessions/s1/rewind", None),
        (
            "PATCH",
            "/sessions/s1",
            Some(serde_json::json!({"title": "lower-trust title"})),
        ),
        (
            "POST",
            "/sessions/s1/spec",
            Some(serde_json::json!({"specId": "spec-from-untrusted"})),
        ),
        ("DELETE", "/sessions/s1/spec", None),
        (
            "POST",
            "/sessions/s1/steer",
            Some(serde_json::json!({"text": "do untrusted steering"})),
        ),
        (
            "POST",
            "/sessions/s1/goal",
            Some(serde_json::json!({"text": "lower-trust goal"})),
        ),
        ("DELETE", "/sessions/s1/goal", None),
        (
            "POST",
            "/sessions/s1/orchestrate",
            Some(serde_json::json!({"goal": "spawn from lower trust", "dryRun": true})),
        ),
    ];

    for (method, uri, body) in cases {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-coderight-source-trust", "remoteAgentUntrusted");
        let body = if let Some(body) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let resp = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }

    let live = state
        .get_session("s1")
        .expect("lower-trust delete/kill must not remove the session");
    assert_eq!(lock_recover(&live.title, "session.title").as_str(), "Test");
    assert_eq!(
        lock_recover(&live.goal, "session.goal").as_deref(),
        Some("trusted-goal")
    );
    assert!(lock_recover(&live.transcript, "session.transcript")
        .iter()
        .all(|message| !matches!(message, TranscriptMessage::Compact { .. })));
}
