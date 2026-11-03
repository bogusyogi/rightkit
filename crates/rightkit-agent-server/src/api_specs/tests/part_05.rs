#[tokio::test]
async fn bound_session_orchestration_observations_are_spec_trace_evidence() {
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
                        r#"{"tasks":[{"requirementIds":["FR-001"],"successCriterionIds":["SC-001"],"description":"Review implementation","evidenceIds":["obs:review"]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(add_tasks_resp.status(), StatusCode::OK);

    let live = state.create_session(
        "sess-spec-orchestration-evidence",
        None,
        "D:/work",
        "Spec orchestration evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit
        .publish(ProtocolEvent::OrchestrationObservationRecorded {
            session_id: live.id.clone(),
            seq: live.conduit.next_seq(),
            created_at: "2026-06-08T00:00:04Z".to_string(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: "obs-review-1".to_string(),
                kind: "review_jury".to_string(),
                subject: "T001".to_string(),
                summary: "review jury passed".to_string(),
                fields: HashMap::from([
                    ("taskId".to_string(), "T001".to_string()),
                    ("evidenceId".to_string(), "obs:review".to_string()),
                    ("verdictPassed".to_string(), "true".to_string()),
                ]),
                created_at: "2026-06-08T00:00:04Z".to_string(),
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
            && link["toId"] == "orchestration:1:obs-review-1"
    }));
    assert!(trace["evidence"].as_array().unwrap().iter().any(|row| {
        row["id"] == "orchestration:1:obs-review-1"
            && row["sessionId"] == "sess-spec-orchestration-evidence"
            && row["taskId"] == "T001"
            && row["evidenceId"] == "obs:review"
            && row["path"] == "orchestration:review_jury/obs-review-1"
            && row["kind"] == "orchestration_observation"
            && row["producer"] == "review_jury"
            && row["provenance"] == "bound-session-orchestration-observation"
            && row["verdictPassed"] == true
            && row["shippable"] == true
    }));
}

#[tokio::test]
async fn bound_session_protocol_provenance_events_are_spec_trace_evidence() {
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
                        r#"{"workspaceRoot":"D:/work","title":"Runtime provenance","description":"Track runtime provenance","functionalRequirements":[{"id":"FR-001","text":"Runtime events are traceable"}],"successCriteria":[{"id":"SC-001","text":"Trace shows runtime provenance"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap().to_string();

    let live = state.create_session(
        "sess-spec-protocol-evidence",
        None,
        "D:/work",
        "Spec protocol evidence session",
        None,
    );
    *live.spec_id.lock().unwrap() = Some(spec_id.clone());
    live.conduit.publish(ProtocolEvent::SessionStarted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:04Z".to_string(),
        title: Some("Spec protocol evidence session".to_string()),
    });
    live.conduit.publish(ProtocolEvent::RunInterrupted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:04Z".to_string(),
        reason: Some("operator stopped run".to_string()),
    });
    live.conduit.publish(ProtocolEvent::RunFailed {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:05Z".to_string(),
        error: "provider returned 500".to_string(),
        kind: "provider_error".to_string(),
        hint: "retry later".to_string(),
    });
    live.conduit.publish(ProtocolEvent::BudgetExceeded {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:06Z".to_string(),
        scope: "session".to_string(),
        current_usd: 2.5,
        ceiling_usd: 2.0,
        hint: "increase budget".to_string(),
    });
    live.conduit.publish(ProtocolEvent::ProviderRouteUpdated {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:07Z".to_string(),
        state: "fallback".to_string(),
        provider: "openrouter".to_string(),
        model: "test-model".to_string(),
        failure_kind: "rate_limited".to_string(),
        detail: "primary model throttled".to_string(),
    });
    live.conduit.publish(ProtocolEvent::SessionCompacted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:07Z".to_string(),
        summary: "Trust-preserving compact summary".to_string(),
        dropped_count: 12,
        tier: String::new(),
        tokens_before: 0,
        tokens_after: 0,
        epoch: 0,
        command_id: String::new(),
        retained_turns: 8,
    });
    live.conduit.publish(ProtocolEvent::UsageUpdated {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:07Z".to_string(),
        prompt_tokens: 100,
        completion_tokens: 25,
        cost_usd: 0.0125,
        context_tokens: 125,
        context_percent: 1.5,
        cache_read_tokens: 10,
        cache_creation_tokens: 5,
        cost_known: true,
    });
    live.conduit.publish(ProtocolEvent::AgentQueryIssued {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:08Z".to_string(),
        query_id: "query-1".to_string(),
        from_role: "planner".to_string(),
        to_role: "reviewer".to_string(),
        query_type: "risk-review".to_string(),
        expected_format: "verdict".to_string(),
    });
    live.conduit.publish(ProtocolEvent::AgentQueryResponded {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:09Z".to_string(),
        query_id: "query-1".to_string(),
        from_role: "reviewer".to_string(),
        to_role: "planner".to_string(),
        confidence: 0.82,
    });
    live.conduit.publish(ProtocolEvent::SubAgentCompleted {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:10Z".to_string(),
        sub_session_id: "sub-1".to_string(),
        role: "implementer".to_string(),
        success: true,
        summary: "patch ready".to_string(),
        outcome: "complete".to_string(),
        outcome_detail: String::new(),
    });
    live.conduit.publish(ProtocolEvent::SubAgentSpawned {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:10Z".to_string(),
        sub_session_id: "sub-2".to_string(),
        role: "reviewer".to_string(),
        task_title: "Review patch".to_string(),
    });
    live.conduit.publish(ProtocolEvent::SubAgentCancelled {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:10Z".to_string(),
        sub_session_id: "sub-2".to_string(),
        role: "reviewer".to_string(),
        cancelled_by: "budget".to_string(),
    });
    live.conduit.publish(ProtocolEvent::HumanSteeringInjected {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:10Z".to_string(),
        injected_by: "operator".to_string(),
        content_preview: "Prefer the safer implementation path".to_string(),
        text: Some("Prefer the safer implementation path".to_string()),
    });
    live.conduit.publish(ProtocolEvent::UserQuestionAsked {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:11Z".to_string(),
        question_id: "question-1".to_string(),
        questions: vec![rightkit_agent_engine_protocol::UserQuestion {
            question: "Ship it?".to_string(),
            header: "Ship".to_string(),
            options: vec![rightkit_agent_engine_protocol::UserQuestionOption {
                label: "Yes".to_string(),
                description: "Proceed".to_string(),
            }],
            multi_select: false,
        }],
    });
    live.conduit.publish(ProtocolEvent::UserQuestionAnswered {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:11Z".to_string(),
        question_id: "question-1".to_string(),
    });
    live.conduit.publish(ProtocolEvent::PlanPresented {
        session_id: live.id.clone(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:12Z".to_string(),
        plan_id: "plan-1".to_string(),
        plan: "Implement runtime provenance trace evidence.".to_string(),
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
    let evidence = trace["evidence"].as_array().unwrap();
    for kind in [
        "session_started",
        "run_interrupted",
        "run_failed",
        "session_compacted",
        "usage_updated",
        "budget_exceeded",
        "provider_route_event",
        "agent_query_issued",
        "agent_query_responded",
        "subagent_spawned",
        "subagent_completed",
        "subagent_cancelled",
        "human_steering_injected",
        "user_question_asked",
        "user_question_answered",
        "plan_presented",
    ] {
        assert!(
            evidence.iter().any(|row| {
                row["sessionId"] == "sess-spec-protocol-evidence"
                    && row["kind"] == kind
                    && row["provenance"] == "bound-session-protocol-event"
                    && row["contentHash"].as_str().unwrap().len() == 64
            }),
            "missing protocol provenance evidence for {kind}"
        );
    }
    assert!(evidence.iter().any(|row| {
        row["kind"] == "run_failed"
            && row["verdictPassed"] == false
            && row["shippable"] == false
            && row["verdictSummary"]
                .as_str()
                .unwrap()
                .contains("retry later")
    }));
    assert!(evidence.iter().any(|row| {
        row["kind"] == "subagent_completed"
            && row["verdictPassed"] == true
            && row["shippable"] == true
    }));
}
