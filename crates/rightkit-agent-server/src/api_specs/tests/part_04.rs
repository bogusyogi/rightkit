#[tokio::test]
async fn bound_session_tool_approvals_are_spec_trace_evidence() {
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();

    let add_tasks_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/specs/{spec_id}/tasks"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Approve login migration","evidenceIds":["approval:login"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-approval-evidence",
        None,
        "D:/work",
        "Spec approval evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit.publish(ProtocolEvent::ToolApprovalRequested {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        call: rightkit_agent_engine_protocol::ToolCall {
            id: "turn-1:tc-login-approval".to_string(),
            name: "write_file".to_string(),
            arguments_json: serde_json::json!({
                "path": "src/login.rs",
                "content": "new login",
                "specId": spec_id,
                "taskId": "T001",
                "evidenceId": "approval:login",
                "requirementIds": ["FR-001"],
                "successCriterionIds": ["SC-001"]
            })
            .to_string(),
        },
        risk_label: "needs-approval".to_string(),
        proposed_change: Some("+ new login".to_string()),
        policy_source: Some("permission-mode.ask".to_string()),
        middleware_decisions: Vec::new(),
    });
    live.conduit.publish(ProtocolEvent::ToolApprovalResolved {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        tool_call_id: "turn-1:tc-login-approval".to_string(),
        approved: false,
    });

    let trace_resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/trace"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trace_resp.status(), StatusCode::OK);
    let body = trace_resp.into_body().collect().await.unwrap().to_bytes();
    let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(trace["links"].as_array().unwrap().iter().any(|link| {
        link["fromKind"] == "task"
            && link["fromId"] == "T001"
            && link["toKind"] == "evidence"
            && link["toId"] == "approval:turn-1:tc-login-approval"
    }));
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "approval:turn-1:tc-login-approval"
            && row["sessionId"] == "sess-spec-approval-evidence"
            && row["taskId"] == "T001"
            && row["evidenceId"] == "approval:login"
            && row["path"] == "approval:write_file/turn-1:tc-login-approval"
            && row["kind"] == "tool_approval"
            && row["shippable"] == false
            && row["verdictPassed"] == false
            && row["verdictSummary"].as_str().unwrap().contains("denied")
    }));
}

#[tokio::test]
async fn bound_session_verification_events_are_spec_trace_evidence() {
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();

    let plan_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/plan"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"includeVerification":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plan_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-verification-evidence",
        None,
        "D:/work",
        "Spec verification evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit.publish(ProtocolEvent::VerificationFinished {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:02Z".to_string(),
        summary: "cargo test passed".to_string(),
        passed: true,
        tool: "cargo test".to_string(),
        issue_count: 0,
        issues: Vec::new(),
    });

    let trace_resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/trace"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trace_resp.status(), StatusCode::OK);
    let body = trace_resp.into_body().collect().await.unwrap().to_bytes();
    let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "verification:1"
            && row["sessionId"] == "sess-spec-verification-evidence"
            && row["taskId"] == "T002"
            && row["path"] == "verification:cargo test/1"
            && row["kind"] == "verification_event"
            && row["producer"] == "cargo test"
            && row["provenance"] == "bound-session-verification-event"
            && row["verdictPassed"] == true
            && row["shippable"] == true
    }));
}

#[tokio::test]
async fn bound_session_task_graph_events_are_spec_trace_evidence() {
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"id":"FR-001","text":"Users can log in"}],"successCriteria":[{"id":"SC-001","text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();

    let add_tasks_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/specs/{spec_id}/tasks"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Verify login","evidenceIds":["task:login"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-task-graph-evidence",
        None,
        "D:/work",
        "Spec task graph evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit.publish(ProtocolEvent::TaskGraphUpdated {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:03Z".to_string(),
        summary: "1 todo completed".to_string(),
        tasks: vec![rightkit_agent_engine_protocol::TaskNode {
            id: "T001".to_string(),
            title: "Verify login".to_string(),
            deps: Vec::new(),
            done_when: "Login test passes".to_string(),
            done_when_predicates: Vec::new(),
            state: TaskState::Done,
            category: "verification".to_string(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:03Z".to_string(),
        }],
    });

    let trace_resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/specs/{spec_id}/trace"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trace_resp.status(), StatusCode::OK);
    let body = trace_resp.into_body().collect().await.unwrap().to_bytes();
    let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(trace["links"].as_array().unwrap().iter().any(|link| {
        link["fromKind"] == "task"
            && link["fromId"] == "T001"
            && link["toKind"] == "evidence"
            && link["toId"] == "task_graph:1:T001"
    }));
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "task_graph:1:T001"
            && row["sessionId"] == "sess-spec-task-graph-evidence"
            && row["taskId"] == "T001"
            && row["path"] == "task_graph:1/T001"
            && row["kind"] == "task_graph_event"
            && row["producer"] == "task_graph"
            && row["provenance"] == "bound-session-task-graph-event"
            && row["verdictPassed"] == true
            && row["shippable"] == true
    }));
}
