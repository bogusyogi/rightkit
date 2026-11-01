#[test]
fn scheduled_workspace_roots_includes_persisted_session_roots() {
    let dir = test_dir("scheduled_workspace_roots_includes_persisted_session_roots");
    let storage = std::sync::Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    {
        let store = storage.blocking_lock();
        store
            .create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "dormant".into(),
                title: "Dormant".into(),
                workspace_root: dir.to_string_lossy().to_string(),
                created_at: "2026-06-06T00:00:00Z".into(),
                updated_at: "2026-06-06T00:00:00Z".into(),
                parent_session_id: None,
                role: None,
                status: "dormant".into(),
                config_json: "{}".into(),
                tags: "[]".into(),
                recovery_marker: None,
            })
            .unwrap();
    }
    let state = DaemonState::new("").with_storage(storage);

    let roots = scheduled_workspace_roots(&state);

    assert!(roots.iter().any(|root| root == &dir));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn fire_schedule_records_runtime_middleware_spans() {
    let dir = test_dir("fire_schedule_records_runtime_middleware_spans");
    let command = if cfg!(windows) {
        "cmd /C echo scheduled-middleware-ok"
    } else {
        "printf scheduled-middleware-ok"
    };
    write_schedule(&dir, "daily", "* * * * *", command);
    let path = schedules_path(&dir);
    let mut schedules: HashMap<String, ScheduleEntry> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = schedules.get_mut("daily").unwrap();
    entry.mode = "scriptOnly".to_string();
    entry.script_allowlist = vec![command.to_string()];
    entry.toolset = Some("cron-safe".to_string());
    entry.execution_backend = Some("local".to_string());
    std::fs::write(&path, serde_json::to_string_pretty(&schedules).unwrap()).unwrap();

    let state = DaemonState::new("");
    fire_schedule_by_id(&state, &dir, "daily").await.unwrap();

    let spans = state.runtime_middleware.lock().unwrap().spans(None);
    let start = spans
        .iter()
        .find(|span| {
            span.session_id.starts_with("schedule-daily-")
                && span.phase == MiddlewarePhase::ScheduledJobStart
        })
        .expect("scheduled job start span");
    let end = spans
        .iter()
        .find(|span| {
            span.session_id == start.session_id && span.phase == MiddlewarePhase::ScheduledJobEnd
        })
        .expect("scheduled job end span");

    assert_eq!(start.status, "started");
    assert_eq!(end.status, "scriptCompleted");
    assert_eq!(end.parent_id.as_deref(), Some(start.id.as_str()));
    assert_eq!(end.attributes["scheduleId"], "daily");
    assert_eq!(end.attributes["mode"], "scriptOnly");
    assert_eq!(end.attributes["toolset"], "cron-safe");
    assert_eq!(end.attributes["executionBackend"], "local");
    assert!(end.attributes["middlewareIds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "builtin.trace-scheduled-job-end"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn telegram_chat_delivery_requires_numeric_chat_id() {
    assert_eq!(telegram_chat_id(Some("7001")).unwrap(), 7001);
    assert!(telegram_chat_id(Some("chat-1"))
        .unwrap_err()
        .contains("numeric Telegram chat id"));
    assert!(telegram_chat_id(None).unwrap_err().contains("required"));
}

#[test]
fn telegram_schedule_messages_are_bounded_for_bot_api() {
    let text = truncate_telegram_text(&"x".repeat(5_000));

    assert!(text.chars().count() <= 4096);
    assert!(text.ends_with("[truncated by RightKit]"));
}
