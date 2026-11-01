#[tokio::test]
async fn trace_route_links_requirement_task_and_evidence() {
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
    let spec_id = created["id"].as_str().unwrap();

    let tasks_uri = format!("/specs/{spec_id}/tasks");
    let add_tasks_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(tasks_uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Implement login","evidenceIds":["test:login"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-evidence",
        None,
        "D:/work",
        "Spec evidence session",
        None,
    );
    live.artifacts
        .lock()
        .unwrap()
        .push(rightkit_agent_engine_protocol::ArtifactRow {
            id: "artifact-login-1".to_string(),
            path: "reports/login-verification.json".to_string(),
            kind: "goal_verification".to_string(),
            content_hash: "sha256:login".to_string(),
            shippable: true,
            producer: "auto-jury".to_string(),
            provenance: "spec-trace-test".to_string(),
            verdict: Some(rightkit_agent_engine_protocol::ArtifactVerdict {
                passed: true,
                juror: "auto-jury".to_string(),
                bound_hash: "sha256:login".to_string(),
                summary: "Login verification passed".to_string(),
            }),
            approvals: Vec::new(),
            meta: serde_json::json!({
                "specId": spec_id,
                "taskId": "T001",
                "evidenceId": "test:login",
                "requirementIds": ["FR-001"],
                "successCriterionIds": ["SC-001"]
            })
            .to_string(),
            trust_label: "unreviewed".to_string(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
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
        link["fromKind"] == "requirement" && link["fromId"] == "FR-001" && link["toKind"] == "task"
    }));
    assert!(trace["links"].as_array().unwrap().iter().any(|link| {
        link["fromKind"] == "task" && link["toKind"] == "evidence" && link["toId"] == "test:login"
    }));
    assert!(trace["links"].as_array().unwrap().iter().any(|link| {
        link["fromKind"] == "task"
            && link["fromId"] == "T001"
            && link["toKind"] == "evidence"
            && link["toId"] == "artifact-login-1"
    }));
    assert!(trace["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|evidence| {
            evidence["id"] == "artifact-login-1"
                && evidence["sessionId"] == "sess-spec-evidence"
                && evidence["taskId"] == "T001"
                && evidence["evidenceId"] == "test:login"
                && evidence["path"] == "reports/login-verification.json"
                && evidence["verdictPassed"] == true
        }));
}

#[tokio::test]
async fn bound_session_artifacts_are_auto_stamped_as_spec_evidence() {
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

    let live = state.create_session(
        "sess-auto-spec-evidence",
        None,
        "D:/work",
        "Spec evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    crate::api::push_session_artifact(
        &live,
        rightkit_agent_engine_protocol::ArtifactRow {
            id: "artifact-auto-spec".to_string(),
            path: "reports/auto-verification.json".to_string(),
            kind: "goal_verification".to_string(),
            content_hash: "sha256:auto".to_string(),
            shippable: true,
            producer: "auto-jury".to_string(),
            provenance: "bound-session-test".to_string(),
            verdict: None,
            approvals: Vec::new(),
            trust_label: "unreviewed".to_string(),
            meta: String::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
        },
    );

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
    let evidence = trace["evidence"].as_array().unwrap();
    assert!(evidence.iter().any(|row| {
        row["id"] == "artifact-auto-spec"
            && row["sessionId"] == "sess-auto-spec-evidence"
            && row["taskId"].is_null()
            && row["path"] == "reports/auto-verification.json"
    }));
    let artifact = live.artifacts.lock().unwrap()[0].clone();
    let meta: serde_json::Value = serde_json::from_str(&artifact.meta).unwrap();
    assert_eq!(meta["specId"], spec_id);
    assert_eq!(meta["evidenceSource"], "bound-session-artifact");
}

#[tokio::test]
async fn bound_session_tool_results_are_spec_trace_evidence() {
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
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Verify login","evidenceIds":["tool:login"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-tool-evidence",
        None,
        "D:/work",
        "Spec tool evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        call: rightkit_agent_engine_protocol::ToolCall {
            id: "turn-1:tc-login-test".to_string(),
            name: "shell".to_string(),
            arguments_json: serde_json::json!({
                "command": "cargo test login",
                "specId": spec_id,
                "taskId": "T001",
                "evidenceId": "tool:login",
                "requirementIds": ["FR-001"],
                "successCriterionIds": ["SC-001"]
            })
            .to_string(),
        },
    });
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        result: TranscriptMessage::Tool {
            id: "tool-result-login".to_string(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            tool_call_id: "turn-1:tc-login-test".to_string(),
            summary: "login verification passed".to_string(),
            output: Some(rightkit_agent_engine_protocol::OutputRef {
                id: "out-login".to_string(),
                kind: "tool_output".to_string(),
                size_bytes: 24,
                content_hash: Some("sha256:tool-login".to_string()),
                preview: Some("login verification passed".to_string()),
                media_type: "text/plain; charset=utf-8".to_string(),
                truncated: false,
                retrievable: true,
            }),
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        },
    });
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:02Z".to_string(),
        call: rightkit_agent_engine_protocol::ToolCall {
            id: "turn-2:tc-login-denied".to_string(),
            name: "shell".to_string(),
            arguments_json: serde_json::json!({
                "command": "cargo test login --write-fixture",
                "specId": spec_id,
                "taskId": "T001",
                "evidenceId": "tool:login-denied"
            })
            .to_string(),
        },
    });
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:03Z".to_string(),
        result: TranscriptMessage::Tool {
            id: "tool-result-login-denied".to_string(),
            created_at: "2026-06-08T00:00:03Z".to_string(),
            tool_call_id: "turn-2:tc-login-denied".to_string(),
            summary: "permission denied: shell write fixture requires approval".to_string(),
            output: None,
            trust: None,
            is_error: true,
            process_id: None,
            pinned: false,
        },
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
            && link["toId"] == "tool:turn-1:tc-login-test"
    }));
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "tool:turn-1:tc-login-test"
            && row["sessionId"] == "sess-spec-tool-evidence"
            && row["taskId"] == "T001"
            && row["evidenceId"] == "tool:login"
            && row["path"] == "tool:shell/turn-1:tc-login-test/out-login"
            && row["kind"] == "tool_invocation"
            && row["contentHash"] == "sha256:tool-login"
            && row["verdictPassed"] == true
    }));
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "tool:turn-2:tc-login-denied"
            && row["sessionId"] == "sess-spec-tool-evidence"
            && row["taskId"] == "T001"
            && row["evidenceId"] == "tool:login-denied"
            && row["path"] == "tool:shell/turn-2:tc-login-denied/summary"
            && row["kind"] == "tool_error"
            && row["verdictPassed"] == false
            && row["shippable"] == false
            && row["verdictSummary"]
                .as_str()
                .unwrap()
                .contains("permission denied")
    }));
}
