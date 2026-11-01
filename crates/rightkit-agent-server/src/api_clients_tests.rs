use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

fn state() -> DaemonState {
    DaemonState::new("")
}

async fn call(
    state: &DaemonState,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let router = client_routes().with_state(state.clone());
    let body_bytes = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body_bytes)
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

#[tokio::test]
async fn attach_heartbeat_detach_round_trip() {
    let state = state();
    let (status, body) = call(
        &state,
        "POST",
        "/clients",
        serde_json::json!({"kind": "cli", "pid": 123}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let client_id = body["clientId"].as_str().unwrap().to_string();
    assert_eq!(
        body["heartbeatSeconds"],
        state.presence.config().heartbeat_seconds
    );
    assert_eq!(
        body["expirySeconds"],
        state.presence.config().expiry_seconds
    );
    assert!(state.presence.ever_attached());

    let (status, _) = call(
        &state,
        "POST",
        &format!("/clients/{client_id}/heartbeat"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = call(&state, "GET", "/clients", serde_json::Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["clients"].as_array().unwrap().len(), 1);

    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/clients/{client_id}"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = call(&state, "GET", "/clients", serde_json::Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["clients"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn heartbeat_unknown_client_is_404() {
    let state = state();
    let (status, _) = call(
        &state,
        "POST",
        "/clients/c-does-not-exist/heartbeat",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn detach_unknown_client_is_404() {
    let state = state();
    let (status, _) = call(
        &state,
        "DELETE",
        "/clients/c-does-not-exist",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn second_app_attach_is_409_but_second_cli_is_not() {
    let state = state();
    let (status, _) = call(
        &state,
        "POST",
        "/clients",
        serde_json::json!({"kind": "app", "pid": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        &state,
        "POST",
        "/clients",
        serde_json::json!({"kind": "app", "pid": 2}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = call(
        &state,
        "POST",
        "/clients",
        serde_json::json!({"kind": "cli", "pid": 3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn invalid_kind_is_bad_request() {
    let state = state();
    let (status, _) = call(
        &state,
        "POST",
        "/clients",
        serde_json::json!({"kind": "browser", "pid": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ── startup_guard_decision ───────────────────────────────────────────

#[test]
fn no_existing_daemon_proceeds() {
    assert!(startup_guard_decision(None, "/db/a.db", "http://x").is_ok());
}

#[test]
fn unhealthy_existing_daemon_proceeds() {
    let health = serde_json::json!({"ok": false, "databasePath": "/db/a.db"});
    assert!(startup_guard_decision(Some(&health), "/db/a.db", "http://x").is_ok());
}

#[test]
fn missing_database_path_proceeds() {
    let health = serde_json::json!({"ok": true});
    assert!(startup_guard_decision(Some(&health), "/db/a.db", "http://x").is_ok());
}

#[test]
fn different_db_path_proceeds() {
    let health = serde_json::json!({"ok": true, "databasePath": "/db/other.db"});
    assert!(startup_guard_decision(Some(&health), "/db/a.db", "http://x").is_ok());
}

#[test]
fn same_db_path_refuses() {
    let health = serde_json::json!({"ok": true, "databasePath": "/db/a.db"});
    let err =
        startup_guard_decision(Some(&health), "/db/a.db", "http://127.0.0.1:1234").unwrap_err();
    assert!(err.contains("already running"));
    assert!(err.contains("http://127.0.0.1:1234"));
    assert!(err.contains("RIGHTKIT_SERVE_FORCE=1"));
}
