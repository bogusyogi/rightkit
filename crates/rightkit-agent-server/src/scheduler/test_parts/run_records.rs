#[test]
fn schedule_event_factory_never_claims_backend_resolution_without_evidence() {
    let mut entry = base_entry("local-job");
    entry.execution_backend = Some("local".to_string());
    let event = schedule_run_event(&entry, 42, "submitted", "sess-1", "ok");
    assert_eq!(event.execution_backend.as_deref(), Some("local"));
    assert_eq!(event.resolved_execution_backend, None);

    let mut local_entry = base_entry("implicit-local-job");
    local_entry.execution_backend = None;
    let local_event = schedule_run_event(&local_entry, 42, "submitted", "sess-2", "ok");
    assert_eq!(local_event.resolved_execution_backend, None);

    // Explicit "local" is still only a request until an executor is entered.
    let mut explicit_local = base_entry("explicit-local");
    explicit_local.execution_backend = Some("local".to_string());
    let explicit_event = schedule_run_event(&explicit_local, 42, "submitted", "sess-3", "ok");
    assert_eq!(explicit_event.resolved_execution_backend, None);

    let mut evidenced = explicit_event;
    mark_local_execution_resolved(&mut evidenced);
    assert_eq!(
        evidenced.resolved_execution_backend.as_deref(),
        Some("local")
    );
}

#[test]
fn rejected_non_local_schedule_persists_request_without_resolved_backend() {
    let dir = test_dir("rejected_non_local_schedule_backend");
    let mut entry = base_entry("remote-agent-job");
    entry.execution_backend = Some("ssh-production".to_string());

    let outcome = record_schedule_failure(
        &dir,
        &entry,
        "sess-remote",
        "execution backend 'ssh-production' is not routed for agent execution yet",
    );
    assert_eq!(outcome.status, "failed");

    let persisted = load_run_state(&dir).unwrap();
    let event = persisted.history.last().expect("durable failure row");
    assert_eq!(
        event.execution_backend.as_deref(),
        Some("ssh-production"),
        "the requested backend remains auditable"
    );
    assert_eq!(
        event.resolved_execution_backend, None,
        "a rejected request must never be reported as resolved/applied"
    );
    assert_eq!(event.status, "failed");

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn accepted_prompt_without_live_engine_does_not_claim_execution_resolution() {
    let dir = test_dir("failed_local_prompt_submission_resolution");
    let mut entry = base_entry("local-without-provider");
    entry.execution_backend = Some("local".to_string());
    let state = DaemonState::new("");

    fire_schedule(&state, &dir, &entry).await;

    let persisted = load_run_state(&dir).unwrap();
    let event = persisted.history.last().expect("durable failure row");
    assert_eq!(event.status, "submitted");
    assert_eq!(event.execution_backend.as_deref(), Some("local"));
    assert_eq!(
        event.resolved_execution_backend, None,
        "creating an inert session is not evidence that an executor accepted the prompt"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn schedule_run_v2_rows_parse_with_v1_defaults() {
    // A v1-era JSON row (no resolved_execution_backend field) deserializes without error;
    // the missing optional fields default to None.
    let v1_json = r#"{
            "scheduleId": "old-job",
            "epochMinute": 99,
            "status": "submitted",
            "sessionId": "sess-old",
            "message": "ok",
            "mode": "agent"
        }"#;
    let event: ScheduleRunEvent = serde_json::from_str(v1_json).unwrap();
    assert_eq!(event.schedule_id, "old-job");
    assert_eq!(event.execution_backend, None);
    assert_eq!(event.resolved_execution_backend, None);
}
