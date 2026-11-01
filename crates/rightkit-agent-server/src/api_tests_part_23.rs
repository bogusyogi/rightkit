#[tokio::test(flavor = "multi_thread")]
async fn schedule_session_message_delivery_posts_script_result_to_target_session() {
    let root = unique_temp_dir("schedules-session-message-delivery");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo delivery-session-message-ok"
    } else {
        "printf delivery-session-message-ok"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["delivery"] = serde_json::json!([
        { "kind": "session_message", "target": "s-target" }
    ]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let state = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    state.create_session(
        "s-target",
        None,
        &root.to_string_lossy(),
        "Target session",
        None,
    );
    let app = build_router(state.clone());
    let root_query = encode_query_value(&root.to_string_lossy());

    let run = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let latest = &value["latestRun"];
    assert_eq!(latest["status"], "scriptCompleted");
    assert_eq!(latest["deliveryStatus"], "delivered");
    assert!(latest["deliveryErrors"]
        .as_array()
        .map(|errors| errors.is_empty())
        .unwrap_or(true));

    let history = lock_recover(&state.message_history, "message_history");
    assert_eq!(history.len(), 1);
    assert!(history[0]
        .from_session_id
        .starts_with("schedule-daily-review-"));
    assert_eq!(history[0].target_session_id.as_deref(), Some("s-target"));
    assert_eq!(history[0].channel, "schedule");
    assert_eq!(history[0].delivery_status, "delivered");
    assert_eq!(history[0].delivered_session_ids, vec!["s-target"]);
    assert!(history[0].message.contains("delivery-session-message-ok"));
    drop(history);

    let target = state.get_session("s-target").unwrap();
    let transcript = lock_recover(&target.transcript, "session.transcript");
    assert!(transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::User { content, pinned: true, trust: Some(_), .. }
            if content.contains("Message from schedule-daily-review-")
                && content.contains("delivery-session-message-ok")
                && content.contains("context only")
    )));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn schedule_runs_route_returns_direct_history_for_schedule() {
    let root = unique_temp_dir("schedules-runs-route");
    write_schedule_fixture(&root);
    let app = build_router(DaemonState::new(""));
    let root_query = encode_query_value(&root.to_string_lossy());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/schedules/daily-review/runs?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "daily-review");
    assert!(json["runStorePath"]
        .as_str()
        .unwrap()
        .contains("schedule-runs.json"));
    assert_eq!(json["runs"].as_array().unwrap().len(), 2);
    assert_eq!(json["runs"][0]["epochMinute"], 12345);
    assert_eq!(json["runs"][0]["status"], "submitted");
    assert_eq!(json["runs"][1]["epochMinute"], 12344);
    assert_eq!(json["latestRun"]["epochMinute"], 12345);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_and_patch_schedule_routes_persist_rich_metadata() {
    let root = unique_temp_dir("schedules-create-patch");
    let workdir = root.join("jobs");
    std::fs::create_dir_all(&workdir).unwrap();
    let app = build_router(DaemonState::new(""));
    let root_query = encode_query_value(&root.to_string_lossy());

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/schedules?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "morning-audit",
                        "cronExpr": "0 9 * * *",
                        "prompt": "review overnight agent runs",
                        "timeZone": "US/Eastern",
                        "skills": ["review"],
                        "workdir": workdir.to_string_lossy(),
                        "toolset": "review",
                        "executionBackend": "local",
                        "delivery": [{
                            "kind": "webhook",
                            "target": "http://127.0.0.1:1234/schedule",
                            "allowPrivateNetwork": true
                        }],
                        "mode": "agent",
                        "paused": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = create.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["created"], true);
    assert_eq!(created["schedule"]["id"], "morning-audit");
    assert_eq!(created["schedule"]["timeZone"], "America/New_York");
    assert_eq!(created["schedule"]["paused"], true);

    let patch = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/schedules/morning-audit?workspaceRoot={root_query}"
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "cronExpr": "30 10 * * 1",
                        "prompt": "review weekly agent runs",
                        "skills": ["review", "qa"],
                        "toolset": "dev-safe",
                        "paused": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::OK);
    let body = patch.into_body().collect().await.unwrap().to_bytes();
    let patched: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(patched["updated"], true);
    assert_eq!(patched["schedule"]["cronExpr"], "30 10 * * 1");
    assert_eq!(patched["schedule"]["prompt"], "review weekly agent runs");
    assert_eq!(patched["schedule"]["toolset"], "dev-safe");
    assert_eq!(patched["schedule"]["paused"], false);
    assert_eq!(patched["schedule"]["skills"][1], "qa");

    let schedules_path = root.join(".coderight").join("schedules.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schedules_path).unwrap()).unwrap();
    assert_eq!(persisted["morning-audit"]["cron_expr"], "30 10 * * 1");
    assert_eq!(persisted["morning-audit"]["time_zone"], "America/New_York");
    assert_eq!(
        persisted["morning-audit"]["workdir"].as_str().unwrap(),
        workdir.to_string_lossy()
    );
    assert_eq!(persisted["morning-audit"]["execution_backend"], "local");
    assert_eq!(persisted["morning-audit"]["delivery"][0]["kind"], "webhook");
    assert_eq!(
        persisted["morning-audit"]["delivery"][0]["target"],
        "http://127.0.0.1:1234/schedule"
    );
    assert_eq!(
        persisted["morning-audit"]["delivery"][0]["allowPrivateNetwork"],
        true
    );
    assert_eq!(persisted["morning-audit"]["paused"], false);

    let _ = std::fs::remove_dir_all(root);
}
