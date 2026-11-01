/// The cross-harness session import route was rejected in
/// `docs/research/competitive-landscape.md` A1 ("Rejected; remove").
/// Drive the real router and assert the route is absent, not just missing
/// from the public manifest — a router-only `merge()` left in `api_router.rs`
/// would still answer `POST /sessions/import` even if the manifest entry
/// were removed, so this is the test that fails the build if a future
/// refactor re-introduces the surface through a different seam.
#[tokio::test]
async fn sessions_import_route_is_absent_from_the_running_router() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/import")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"path":"/tmp/x.jsonl","dryRun":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            resp.status(),
            axum::http::StatusCode::NOT_FOUND | axum::http::StatusCode::METHOD_NOT_ALLOWED
        ),
        "POST /sessions/import must not be a registered route on the daemon; got {}",
        resp.status()
    );
}

/// The loopback Membrane context route is gone after the CLI cutover.
#[tokio::test]
async fn removed_membrane_context_route_stays_absent() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/membrane/context")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "body": {},
                        "identity": {
                            "sessionId": "session-h8",
                            "taskId": "task-h8",
                            "requestId": "request-h8",
                            "generation": "generation-h8"
                        },
                        "capacity": {
                            "contextWindowTokens": 1024,
                            "reservedTokens": 128,
                            "observedContextTokens": 256,
                            "estimatorBasis": {"id": "test", "version": "v1"}
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    // The Membrane CLI cutover (6cb6a726d) removed the loopback
    // `/runtime/membrane/context` route; context now comes from Pulse's CLI.
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

/// Sanity-check that the router still answers an unrelated session route,
/// so the negative assertion above is not a false positive from a
/// misconfigured test router.
#[tokio::test]
async fn sessions_route_still_responds_to_a_real_endpoint() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // 200 (empty list) or 5xx (storage not initialised) is fine; the point
    // is that `/sessions` is registered, while `/sessions/import` is not.
    let status = resp.status();
    assert!(
        status == axum::http::StatusCode::OK || status.is_server_error(),
        "GET /sessions should be routed (got {status})"
    );
}

/// The Files panel depends on `/workspace/tree`. Keeping the implementation
/// module compiled is insufficient: the production router must mount it.
#[tokio::test]
async fn workspace_tree_route_is_mounted_on_the_running_router() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                // Deliberately omit the required workspaceRoot query. A
                // mounted handler returns 400; a missing router merge is 404.
                .uri("/workspace/tree")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "/workspace/tree must be mounted on the production router"
    );
}

/// Root listing is the Files panel's first real request. Merely proving
/// the route exists misses the empty-path contract used by the frontend.
#[tokio::test]
async fn workspace_tree_lists_the_selected_workspace_root() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("coderight-router-root-{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("visible.txt"), "visible").unwrap();

    let app = build_router(DaemonState::new(""));
    let uri = format!("/workspace/tree?workspaceRoot={}&path=", root.display());
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "the Files panel must be able to list an empty relative root path"
    );
}
