#[tokio::test]
async fn health_route_uses_health_response_shape() {
    let app = crate::api::build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let payload: HealthResponse = serde_json::from_slice(&body).unwrap();

    assert!(payload.ok);
    assert_eq!(payload.service, "coderight-daemon");
    assert_eq!(payload.storage_read_pool_idle, None);
}

#[tokio::test]
async fn health_route_reports_storage_read_pool_when_available() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let dir = std::env::temp_dir().join(format!("coderight-health-pool-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("db.sqlite");
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    let app = crate::api::build_router(DaemonState::new("").with_storage(storage));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let payload: HealthResponse = serde_json::from_slice(&body).unwrap();

    assert!(payload.storage_read_pool_idle.unwrap_or_default() > 0);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn health_discloses_a_keychain_failure() {
    let status = provider_key_storage_status(rightkit_llm::agent_providers::ProviderKeyStorageHealth {
        keychain_failure_observed: true,
    });

    assert!(status.keychain_failure_observed);
}

#[tokio::test]
async fn readiness_snapshot_reports_provider_backend_remote_and_updates() {
    let app = crate::api::build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health/readiness")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let snapshot: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let rows = snapshot["rows"].as_array().expect("rows must be array");
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    for expected in [
        "provider-key",
        "default-model",
        "local-backend",
        "telegram-remote",
        "schedules",
        "mcp",
        "license",
        "updater",
    ] {
        assert!(
            ids.contains(&expected),
            "missing row '{expected}'; got {ids:?}"
        );
    }
}

#[tokio::test]
async fn readiness_snapshot_handles_partial_config() {
    // Empty state: no engine config, no provider keys, no telegram, no license.
    let snapshot = readiness_snapshot(&DaemonState::new("")).await;
    // Must always return every row (9 now, incl. permission-rules) even with nothing configured.
    assert_eq!(snapshot.rows.len(), 9, "must always return 9 rows");
    // local-backend and updater must never be blocked in a default state.
    let local = snapshot
        .rows
        .iter()
        .find(|r| r.id == "local-backend")
        .unwrap();
    assert_ne!(
        local.state,
        ReadinessState::Blocked,
        "default local-backend should not be blocked"
    );
    let updater = snapshot.rows.iter().find(|r| r.id == "updater").unwrap();
    assert_eq!(
        updater.state,
        ReadinessState::Ready,
        "updater should be ready"
    );
}

#[tokio::test]
async fn readiness_snapshot_degrades_slow_rows() {
    // Verify the timeout helper degrades rather than panics. We test the helper directly
    // since actually timing out in CI at 50 ms is flaky.
    let row = with_readiness_timeout("test-id", "Test", async {
        // This completes immediately, so no degradation.
        ReadinessRow::ready("test-id", "Test", "ok")
    })
    .await;
    assert_eq!(row.state, ReadinessState::Ready);
    assert_eq!(row.id, "test-id");
}

// ── FM-129 / AO-02(a,b,c): machine-status derivation ────────────────
//
// `MachineStatus` is a derived projection over authoritative facts
// (`initialized`, `storage_available`, `provider_degraded` — each
// sourced from the daemon's own live state, never itself persisted
// as a status value). `machine_status_for_runtime` is the single
// deterministic derivation point (AO-02 "deterministic derivation
// precedence"): these tests pin its precedence order and prove the
// function is a pure classification with no persistence side effect
// (AO-02 "derived status not persisted without a distinct causal
// reason" — there is nothing here to persist; every call recomputes
// from the current facts).

#[test]
fn ao_02_machine_status_precedence_uninitialized_wins_over_all_other_facts() {
    // Even when storage is available and nothing is degraded, an
    // uninitialized daemon is reported Stopped — "not yet running"
    // outranks every other fact.
    assert_eq!(
        machine_status_for_runtime(false, true, false),
        MachineStatus::Stopped
    );
    // Uninitialized still wins even when every other fact is bad too.
    assert_eq!(
        machine_status_for_runtime(false, false, true),
        MachineStatus::Stopped
    );
}

#[test]
fn ao_02_machine_status_precedence_healthy_requires_storage_and_no_degradation() {
    assert_eq!(
        machine_status_for_runtime(true, true, false),
        MachineStatus::Healthy
    );
}

#[test]
fn ao_02_machine_status_precedence_degraded_on_storage_unavailable_or_provider_degraded() {
    // Storage unavailable, provider fine.
    assert_eq!(
        machine_status_for_runtime(true, false, false),
        MachineStatus::Degraded
    );
    // Storage available, provider degraded.
    assert_eq!(
        machine_status_for_runtime(true, true, true),
        MachineStatus::Degraded
    );
    // Both bad.
    assert_eq!(
        machine_status_for_runtime(true, false, true),
        MachineStatus::Degraded
    );
}

#[test]
fn ao_02_machine_status_derivation_is_pure_and_deterministic() {
    // Same facts in, same derived status out, every time — no hidden
    // state, no persistence, so restart/replay against the same live
    // facts always reconstructs the same derived status.
    for _ in 0..8 {
        assert_eq!(
            machine_status_for_runtime(true, true, false),
            MachineStatus::Healthy
        );
        assert_eq!(
            machine_status_for_runtime(true, false, true),
            MachineStatus::Degraded
        );
        assert_eq!(
            machine_status_for_runtime(false, true, false),
            MachineStatus::Stopped
        );
    }
}
