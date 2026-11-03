#[tokio::test]
async fn schedule_admin_audit_records_redacted_mutations() {
    let root = unique_temp_dir("schedules-admin-audit");
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
                        "id": "audit-target",
                        "cronExpr": "0 9 * * *",
                        "prompt": "sensitive schedule prompt",
                        "toolset": "cron-safe",
                        "delivery": [{
                            "kind": "webhook",
                            "target": "https://example.invalid/private-hook",
                            "allowPrivateNetwork": false
                        }],
                        "mode": "agent"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);

    let patch = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/schedules/audit-target?workspaceRoot={root_query}"
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "prompt": "new sensitive prompt",
                        "paused": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::OK);

    for (method, path, body) in [
        ("POST", "/schedules/audit-target/pause", Body::empty()),
        ("POST", "/schedules/audit-target/resume", Body::empty()),
        (
            "PUT",
            "/schedules/audit-target/time-zone",
            Body::from(r#"{"timeZone":"US/Eastern"}"#),
        ),
    ] {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("{path}?workspaceRoot={root_query}"));
        if method == "PUT" {
            builder = builder.header("content-type", "application/json");
        }
        let resp = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{method} {path}");
    }

    let delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/schedules/audit-target?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);

    let audit = app
        .oneshot(
            Request::builder()
                .uri(format!("/schedules/audit?workspaceRoot={root_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(audit.status(), StatusCode::OK);
    let body = audit.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["auditStorePath"]
        .as_str()
        .unwrap()
        .ends_with("schedule-admin-audit.json"));
    assert_eq!(v["history"].as_array().unwrap().len(), 6);

    let mut actions: Vec<_> = v["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap().to_string())
        .collect();
    actions.sort();
    assert_eq!(
        actions,
        vec![
            "create".to_string(),
            "delete".to_string(),
            "patch".to_string(),
            "pause".to_string(),
            "resume".to_string(),
            "updateTimeZone".to_string(),
        ]
    );
    for event in v["history"].as_array().unwrap() {
        assert_eq!(event["scheduleId"], "audit-target");
        assert_eq!(event["actor"], "local-daemon-api");
        assert_eq!(event["source"], "daemon-http");
        assert!(event["createdAt"].as_str().unwrap().contains('T'));
        assert!(event["details"]["deliveryKinds"].is_array() || event["action"] == "delete");
    }

    let rendered = serde_json::to_string(&v).unwrap();
    assert!(!rendered.contains("sensitive schedule prompt"));
    assert!(!rendered.contains("new sensitive prompt"));
    assert!(!rendered.contains("example.invalid/private-hook"));

    let audit_path = root.join(".coderight").join("schedule-admin-audit.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(audit_path).unwrap()).unwrap();
    assert_eq!(persisted["history"].as_array().unwrap().len(), 6);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_schedule_rejects_absolute_workdir_outside_workspace() {
    let root = unique_temp_dir("schedules-workdir-trust");
    let outside = root
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("outside-schedule-workdir");
    std::fs::create_dir_all(&outside).unwrap();
    let app = build_router(DaemonState::new(""));
    let root_query = encode_query_value(&root.to_string_lossy());

    let create = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/schedules?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "outside-workdir",
                        "cronExpr": "0 9 * * *",
                        "prompt": "review",
                        "workdir": outside.to_string_lossy(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[tokio::test]
async fn delete_schedule_route_removes_workspace_schedule() {
    let root = unique_temp_dir("schedules-delete");
    write_schedule_fixture(&root);
    let app = build_router(DaemonState::new(""));
    let uri = format!(
        "/schedules/daily-review?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["deleted"], true);
    assert_eq!(v["id"], "daily-review");

    let schedules_path = root.join(".coderight").join("schedules.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schedules_path).unwrap()).unwrap();
    assert_eq!(persisted.as_object().unwrap().len(), 0);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn update_schedule_timezone_route_persists_workspace_schedule_timezone() {
    let root = unique_temp_dir("schedules-timezone");
    write_schedule_fixture(&root);
    let app = build_router(DaemonState::new(""));
    let uri = format!(
        "/schedules/daily-review/time-zone?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"timeZone":"US/Eastern"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["schedule"]["id"], "daily-review");
    assert_eq!(v["schedule"]["timeZone"], "America/New_York");

    let schedules_path = root.join(".coderight").join("schedules.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schedules_path).unwrap()).unwrap();
    assert_eq!(persisted["daily-review"]["time_zone"], "America/New_York");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn tasks_route_accepts_camel_case_session_id() {
    let state = state_with_session("s1");
    let live = state.get_session("s1").unwrap();
    lock_recover(&live.tasks, "session.tasks").push(rightkit_agent_engine_protocol::TaskNode {
        id: "live-1".into(),
        title: "Live taskgraph node".into(),
        deps: vec![],
        done_when: "done".into(),
        done_when_predicates: vec![],
        state: rightkit_agent_engine_protocol::TaskState::Open,
        category: "runtime".into(),
        estimated_duration: String::new(),
        assigned_role: "coder".into(),
        escalation_path: String::new(),
        open_questions: vec![],
        created_at: "t1".into(),
        updated_at: "t1".into(),
    });
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/tasks?sessionId=s1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], "live-1");
    assert_eq!(v[0]["source"], "session-taskgraph");
}

#[tokio::test]
async fn tasks_route_lists_workspace_task_records_with_output_metadata() {
    let root = unique_temp_dir("tasks-list");
    write_task_fixture(&root);
    let app = build_router(DaemonState::new(""));
    let uri = format!(
        "/tasks?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], "task-42");
    assert_eq!(v[0]["title"], "Run verification suite");
    assert_eq!(v[0]["state"], "inProgress");
    assert_eq!(v[0]["kind"], "shell");
    assert_eq!(v[0]["command"], "cargo test");
    assert_eq!(v[0]["shellId"], "shell-1");
    assert_eq!(v[0]["source"], "workspace-record");
    assert_eq!(v[0]["processStatus"], "unavailable");
    assert_eq!(v[0]["processAttachment"], "detached");
    assert_eq!(v[0]["canPollOutput"], false);
    assert_eq!(v[0]["canStop"], false);
    assert!(v[0]["processStatusReason"]
        .as_str()
        .unwrap()
        .contains("stored task output"));
    assert!(v[0]["lifecycleStatusReason"]
        .as_str()
        .unwrap()
        .contains("not attached"));
    assert_eq!(v[0]["outputBytes"], 24);
    assert_eq!(v[0]["outputPreview"], "running tests\nall green\n");
    assert!(
        v[0]["taskStorePath"]
            .as_str()
            .unwrap()
            .ends_with(".coderight\\tasks.json")
            || v[0]["taskStorePath"]
                .as_str()
                .unwrap()
                .ends_with(".coderight/tasks.json")
    );

    let _ = std::fs::remove_dir_all(root);
}
