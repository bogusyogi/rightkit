#[tokio::test(flavor = "multi_thread")]
async fn schedule_webhook_and_ntfy_delivery_posts_script_result_to_targets() {
    async fn webhook_handler(
        State(received): State<Arc<Mutex<Vec<serde_json::Value>>>>,
        Json(body): Json<serde_json::Value>,
    ) -> StatusCode {
        lock_recover(&received, "test.received").push(body);
        StatusCode::NO_CONTENT
    }

    let received = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::Router::new()
        .route("/schedule", post(webhook_handler))
        .with_state(received.clone());
    let server_task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });

    let root = unique_temp_dir("schedules-webhook-delivery");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo delivery-webhook-ok"
    } else {
        "printf delivery-webhook-ok"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["delivery"] = serde_json::json!([
        {
            "kind": "webhook",
            "target": format!("http://{addr}/schedule"),
            "allowPrivateNetwork": true
        },
        {
            "kind": "ntfy",
            "target": format!("http://{addr}/schedule"),
            "allowPrivateNetwork": true
        }
    ]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let app = build_router(DaemonState::new("").with_storage(storage.clone()));
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
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let latest = &json["latestRun"];
    assert_eq!(latest["status"], "scriptCompleted");
    assert_eq!(latest["deliveryStatus"], "delivered");
    assert!(latest["deliveryErrors"]
        .as_array()
        .map(|errors| errors.is_empty())
        .unwrap_or(true));

    let payloads = lock_recover(&received, "test.received");
    assert_eq!(payloads.len(), 2);
    let delivery_kinds: std::collections::HashSet<_> = payloads
        .iter()
        .map(|payload| payload["deliveryKind"].as_str().unwrap_or_default())
        .collect();
    assert!(delivery_kinds.contains("webhook"));
    assert!(delivery_kinds.contains("ntfy"));
    for payload in payloads.iter() {
        assert_eq!(payload["kind"], "scheduleResult");
        assert_eq!(payload["scheduleId"], "daily-review");
        assert!(payload["sessionId"]
            .as_str()
            .is_some_and(|id| id.starts_with("schedule-daily-review-")));
        assert_eq!(payload["status"], "scriptCompleted");
        assert!(payload["content"]
            .as_str()
            .unwrap_or_default()
            .contains("delivery-webhook-ok"));
        assert!(payload["artifactPath"]
            .as_str()
            .unwrap_or_default()
            .ends_with(".txt"));
    }
    drop(payloads);

    let delivery_store = root.join(".coderight").join("remote-deliveries.json");
    let deliveries: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(delivery_store).unwrap()).unwrap();
    assert_eq!(deliveries.as_array().unwrap().len(), 2);
    assert!(deliveries.as_array().unwrap().iter().any(|record| {
        record["channel"] == "webhook"
            && record["status"] == "delivered"
            && record["payload"]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("delivery-webhook-ok")
    }));
    assert!(deliveries.as_array().unwrap().iter().any(|record| {
        record["channel"] == "ntfy"
            && record["targetRedacted"]
                .as_str()
                .unwrap()
                .contains("/schedule")
    }));
    let sql_deliveries = {
        let store = storage.lock().await;
        store
            .list_remote_deliveries_for_channel("webhook", 10)
            .unwrap()
    };
    assert_eq!(sql_deliveries.len(), 1);
    assert_eq!(sql_deliveries[0].status, "delivered");
    assert_eq!(sql_deliveries[0].delivery_kind, "webhook");
    assert!(sql_deliveries[0]
        .payload_json
        .contains("delivery-webhook-ok"));
    assert!(sql_deliveries[0].target_redacted.contains("/schedule"));

    let sql_ntfy_deliveries = {
        let store = storage.lock().await;
        store
            .list_remote_deliveries_for_channel("ntfy", 10)
            .unwrap()
    };
    assert_eq!(sql_ntfy_deliveries.len(), 1);
    assert_eq!(sql_ntfy_deliveries[0].delivery_kind, "ntfy");

    server_task.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_telegram_chat_delivery_posts_script_result_to_bot_api() {
    async fn telegram_handler(
        State(received): State<Arc<Mutex<Vec<serde_json::Value>>>>,
        req: Request<Body>,
    ) -> StatusCode {
        assert!(req.uri().path().ends_with("/bottest-token/sendMessage"));
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        lock_recover(&received, "test.received").push(payload);
        StatusCode::OK
    }

    let received = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::Router::new()
        .route("/bottest-token/sendMessage", post(telegram_handler))
        .with_state(received.clone());
    let server_task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _guard = env_lock();
    let previous_token = std::env::var_os("RIGHTKIT_TELEGRAM_TOKEN");
    let previous_base = std::env::var_os("RIGHTKIT_TELEGRAM_API_BASE");
    std::env::set_var("RIGHTKIT_TELEGRAM_TOKEN", "test-token");
    std::env::set_var("RIGHTKIT_TELEGRAM_API_BASE", format!("http://{addr}"));

    let root = unique_temp_dir("schedules-telegram-delivery");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo delivery-telegram-ok"
    } else {
        "printf delivery-telegram-ok"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["delivery"] = serde_json::json!([
        { "kind": "telegram_chat", "target": "7001" }
    ]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
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

    match previous_token {
        Some(value) => std::env::set_var("RIGHTKIT_TELEGRAM_TOKEN", value),
        None => std::env::remove_var("RIGHTKIT_TELEGRAM_TOKEN"),
    }
    match previous_base {
        Some(value) => std::env::set_var("RIGHTKIT_TELEGRAM_API_BASE", value),
        None => std::env::remove_var("RIGHTKIT_TELEGRAM_API_BASE"),
    }
    server_task.abort();

    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let latest = &json["latestRun"];
    assert_eq!(latest["status"], "scriptCompleted");
    assert_eq!(latest["deliveryStatus"], "delivered");
    assert!(latest["deliveryErrors"]
        .as_array()
        .map(|errors| errors.is_empty())
        .unwrap_or(true));

    let payloads = lock_recover(&received, "test.received");
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["chat_id"], 7001);
    assert!(payloads[0]["text"]
        .as_str()
        .unwrap_or_default()
        .contains("delivery-telegram-ok"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn schedule_desktop_notification_delivery_writes_notification_outbox() {
    let root = unique_temp_dir("schedules-desktop-notification-delivery");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo delivery-desktop-notification-ok"
    } else {
        "printf delivery-desktop-notification-ok"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["delivery"] = serde_json::json!([
        { "kind": "desktop_notification" }
    ]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
    let root_query = encode_query_value(&root.to_string_lossy());
    let run = app
        .clone()
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
    assert_eq!(value["latestRun"]["deliveryStatus"], "delivered");

    let notifications = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/notifications?workspaceRoot={root_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(notifications.status(), StatusCode::OK);
    let body = notifications
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["unreadCount"], 1);
    let row = &listed["notifications"][0];
    assert_eq!(row["kind"], "desktopNotification");
    assert_eq!(row["source"], "schedule");
    assert_eq!(row["scheduleId"], "daily-review");
    assert!(row["sessionId"]
        .as_str()
        .is_some_and(|id| id.starts_with("schedule-daily-review-")));
    assert_eq!(row["read"], false);
    assert!(row["body"]
        .as_str()
        .unwrap_or_default()
        .contains("delivery-desktop-notification-ok"));
    let id = row["id"].as_str().unwrap();

    let dismiss = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/notifications/{id}/dismiss?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dismiss.status(), StatusCode::OK);
    let body = dismiss.into_body().collect().await.unwrap().to_bytes();
    let dismissed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(dismissed["notification"]["read"], true);

    let _ = std::fs::remove_dir_all(root);
}
