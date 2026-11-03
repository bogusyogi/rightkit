#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod api_health_surface_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // ── machine/status, machine/info ────────────────────────────────────────
    // FM-118: these routes were added but had no test coverage. They share the
    // `bound_port == 0 && database_path.is_empty()` "not yet initialized" gate
    // with a typed 503 unavailable envelope, then otherwise report a typed,
    // non-secret snapshot of the daemon's live health state.

    #[tokio::test]
    async fn machine_status_reports_unavailable_before_runtime_info_is_set() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/machine/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "unavailable");
        assert_eq!(json["service"], "coderight-daemon");
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("daemon not yet initialized"));
    }

    #[tokio::test]
    async fn machine_info_reports_unavailable_before_runtime_info_is_set() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/machine/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "unavailable");
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("daemon not yet initialized"));
    }

    #[tokio::test]
    async fn machine_status_reports_ready_once_runtime_info_is_set() {
        let state = DaemonState::new("").with_runtime_info(4123, "D:\\db\\coderight.sqlite");
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/machine/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let payload: MachineStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.port, 4123);
        assert_eq!(payload.database_path, "D:\\db\\coderight.sqlite");
        assert_eq!(payload.service, "coderight-daemon");
        // No storage attached in this state -> not available -> degraded.
        assert!(!payload.storage_available);
        assert!(payload.degraded);
        assert!(!payload.ok);
        assert_eq!(payload.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn machine_status_reports_corrupt_permission_rules_and_clears_on_repair() {
        let dir = std::env::temp_dir().join(format!("cr-perm-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permission-rules.json");
        std::fs::write(&path, "{ not json").unwrap();
        let state = DaemonState::new("")
            .with_runtime_info(4125, "in-memory")
            .with_permission_rule_store_path(path.clone());
        let fetch = |state: DaemonState| async move {
            let resp = crate::api::build_router(state)
                .oneshot(
                    Request::builder()
                        .uri("/machine/status")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<MachineStatusResponse>(&body).unwrap()
        };
        let payload = fetch(state.clone()).await;
        assert!(payload.degraded);
        assert!(payload
            .degraded_reasons
            .iter()
            .any(|r| r.contains("permission rules unreadable") && r.contains("all tools denied")));
        std::fs::write(&path, r#"{"user":[],"projects":{}}"#).unwrap();
        let payload = fetch(state).await;
        assert!(payload.degraded_reasons.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn machine_status_reflects_storage_availability() {
        let store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
        let storage = std::sync::Arc::new(tokio::sync::Mutex::new(store));
        let state = DaemonState::new("")
            .with_runtime_info(4124, "in-memory")
            .with_storage(storage);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/machine/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let payload: MachineStatusResponse = serde_json::from_slice(&body).unwrap();
        assert!(payload.storage_available);
        // The read pool is a file-backed-storage facility; an in-memory store
        // reports no pool at all. Either way the count must never be negative
        // or fabricated, so only a present pool is asserted to be sane.
        if let Some(idle) = payload.storage_read_pool_idle {
            assert!(
                idle <= 64,
                "idle count must be a plausible pool size: {idle}"
            );
        }
    }

    #[tokio::test]
    async fn machine_info_reports_process_and_runtime_fields_once_initialized() {
        let state = DaemonState::new("").with_runtime_info(4125, "D:\\db\\coderight.sqlite");
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/machine/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let payload: MachineInfoResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.port, 4125);
        assert_eq!(payload.database_path, "D:\\db\\coderight.sqlite");
        assert_eq!(payload.service, "coderight-daemon");
        assert_eq!(payload.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(payload.os, std::env::consts::OS);
        assert_eq!(payload.arch, std::env::consts::ARCH);
        assert_eq!(payload.pid, std::process::id());
        assert!(!payload.storage_available);
    }

    #[tokio::test]
    async fn health_machine_status_alias_route_matches_machine_status() {
        let state = DaemonState::new("").with_runtime_info(4126, "aliased.sqlite");
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health/machine/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let payload: MachineStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.database_path, "aliased.sqlite");
    }

    // ── /diagnostics/denials ─────────────────────────────────────────────────
    // Pins the filtering, pagination, and default/clamped limit semantics that
    // `list_denials_handler` layers on top of `DenialTracker::query`.

    fn state_with_denials(records: &[(&str, &str, &str, &str)]) -> DaemonState {
        let state = DaemonState::new("");
        {
            let mut tracker = lock_recover(&state.denial_tracker, "denial_tracker");
            for &(tool, reason, session, at) in records {
                tracker.record_denial(tool, reason, session, at);
            }
        }
        state
    }

    #[tokio::test]
    async fn list_denials_handler_defaults_to_50_limit_and_zero_offset() {
        let state = state_with_denials(&[
            ("bash", "blocked", "s1", "2026-01-01T00:00:00Z"),
            ("fs", "blocked", "s2", "2026-01-01T00:00:01Z"),
        ]);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/diagnostics/denials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["limit"], 50);
        assert_eq!(json["offset"], 0);
        assert_eq!(json["total"], 2);
        assert_eq!(json["denials"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_denials_handler_filters_by_session_and_tool() {
        let state = state_with_denials(&[
            ("bash", "blocked", "s1", "2026-01-01T00:00:00Z"),
            ("fs", "blocked", "s1", "2026-01-01T00:00:01Z"),
            ("bash", "blocked", "s2", "2026-01-01T00:00:02Z"),
        ]);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/diagnostics/denials?session=s1&tool=bash")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 1);
        let denials = json["denials"].as_array().unwrap();
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0]["tool"], "bash");
        assert_eq!(denials[0]["session"], "s1");
    }

    #[tokio::test]
    async fn list_denials_handler_accepts_session_id_alias() {
        let state = state_with_denials(&[("bash", "blocked", "s1", "2026-01-01T00:00:00Z")]);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/diagnostics/denials?sessionId=s1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 1);
        assert_eq!(json["denials"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_denials_handler_clamps_limit_to_500_and_honors_offset() {
        let records: Vec<(&str, &str, &str, &str)> = (0..3)
            .map(|_| ("bash", "blocked", "s1", "2026-01-01T00:00:00Z"))
            .collect();
        let state = state_with_denials(&records);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/diagnostics/denials?limit=9000&offset=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["limit"], 500, "limit must clamp to the 500 ceiling");
        assert_eq!(json["offset"], 2);
        assert_eq!(json["total"], 3);
        assert_eq!(
            json["denials"].as_array().unwrap().len(),
            1,
            "only the one record past offset 2 should be returned"
        );
    }

    #[tokio::test]
    async fn list_denials_handler_returns_empty_for_unmatched_filters() {
        let state = state_with_denials(&[("bash", "blocked", "s1", "2026-01-01T00:00:00Z")]);
        let app = crate::api::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/diagnostics/denials?session=nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 0);
        assert!(json["denials"].as_array().unwrap().is_empty());
    }
}
