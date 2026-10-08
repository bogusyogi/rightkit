#![cfg(feature = "http")]

use agent_room_core::protocol::{EventKind, Phase, Room, RoomConfig, SeatConfig};
use agent_room_core::server::{router, ServerState};
use agent_room_core::store::RoomStore;
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use std::sync::{Arc, RwLock};
use tempfile::tempdir;
use tokio_stream::StreamExt;
use tower::ServiceExt;

fn room() -> Room {
    let root = tempdir().unwrap().keep();
    let mut room = Room::new(RoomConfig {
        room_id: "service-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: root,
        seats: vec![
            SeatConfig::new("seat-a", "A", "human", "human"),
            SeatConfig::new("seat-b", "B", "model-b", "reviewer"),
        ],
    })
    .unwrap();
    room.advance("seat-a", Phase::Positions).unwrap();
    room.post(
        "seat-a",
        "position-a",
        EventKind::Position,
        "private A",
        None,
    )
    .unwrap();
    room.post(
        "seat-b",
        "position-b",
        EventKind::Position,
        "private B",
        None,
    )
    .unwrap();
    room
}

fn state() -> ServerState {
    ServerState::new(
        Arc::new(RwLock::new(room())),
        [
            ("token-a".into(), "seat-a".into()),
            ("token-b".into(), "seat-b".into()),
        ],
        "watch-secret",
        "http://127.0.0.1:43119",
    )
}

async fn paired_cookie(app: &axum::Router) -> String {
    let response = app
        .clone()
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/pair")
                .header(header::ORIGIN, "http://127.0.0.1:43119")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("pairing_code=watch-secret"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/"));
    assert!(!cookie.contains("watch-secret"));
    cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn private_join_link_authorizes_and_redirects_without_exposing_its_capability() {
    let app = router(state());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/join/watch-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/");
    assert_eq!(
        response.headers().get(header::REFERRER_POLICY).unwrap(),
        "no-referrer"
    );
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(!cookie.contains("watch-secret"));
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("watch-secret"));

    let rejected = app
        .oneshot(
            Request::builder()
                .uri("/join/wrong-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn seat_status_requires_bearer_and_matches_visible_event_projection() {
    let app = router(state());
    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["room_id"], "service-room");
    assert_eq!(value["seat_id"], "seat-a");
    assert_eq!(value["phase"], "Positions");
    assert_eq!(value["latest_visible_cursor"], 1);
    assert_eq!(value["events"].as_array().unwrap().len(), 1);
    assert_eq!(value["events"][0]["body"], "private A");
    assert_eq!(value["budget"]["max_messages"], 12);
    assert_eq!(value["usage"]["sent_messages"], 1);
    assert_eq!(value["escalation_rate"], 0.0);
    let next = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tool/next")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"cursor":0,"wait_ms":45000}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(next.status(), StatusCode::OK);
    let streamed: serde_json::Value =
        serde_json::from_slice(&to_bytes(next.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(streamed, value["events"]);
}

#[tokio::test]
async fn browser_watch_uses_strict_cookie_exact_origin_csp_and_live_ui() {
    let app = router(state());
    let pair_page = app
        .clone()
        .oneshot(Request::builder().uri("/pair").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let pair_csp = pair_page
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(pair_csp.contains("form-action 'self'"));
    let pair_html = String::from_utf8(
        to_bytes(pair_page.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(pair_html.contains("pairing_code"));
    assert!(!pair_html.contains("watch-secret"));
    let cookie = paired_cookie(&app).await;
    let forbidden = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, "http://127.0.0.1:43119")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let csp = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("EventSource"));
    assert!(html.contains("/api/watch-status"));
    assert!(html.contains("Participants"));
    assert!(html.contains("Discussion"));
    assert!(html.contains("Room state"));
    assert!(html.contains("No messages yet"));
    assert!(html.contains("aria-live=\"polite\""));
    assert!(html.contains("Interject"));
    assert!(html.contains("<textarea"));
    assert!(html.contains("/api/browser-say"));
    assert!(html.contains("new EventSource(`/api/stream?cursor="));
    assert!(!html.contains("innerHTML"));
    assert!(!html.contains("JSON.stringify(e)"));
    assert!(!html.contains("watch-secret"));

    let snapshot = app
        .oneshot(
            Request::builder()
                .uri("/api/watch-status")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, "http://127.0.0.1:43119")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(snapshot.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["phase"], "Positions");
    assert_eq!(value["seats"].as_array().unwrap().len(), 2);
    assert_eq!(value["seats"][0]["budget"]["max_messages"], 12);
}

#[tokio::test]
async fn sse_replay_uses_cookie_projection_and_never_exposes_credentials() {
    let app = router(state());
    let cookie = paired_cookie(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stream?cursor=0&once=true")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, "http://127.0.0.1:43119")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("private A"));
    assert!(body.contains("private B"));
    assert!(!body.contains("token-a"));
    assert!(!body.contains("watch-secret"));

    let cli = app
        .oneshot(
            Request::builder()
                .uri("/api/watch-stream?cursor=0&once=true")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cli.status(), StatusCode::OK);
    let cli_body = String::from_utf8(
        to_bytes(cli.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(cli_body.contains("private A"));
    assert!(cli_body.contains("private B"));
}

#[tokio::test]
async fn cli_say_is_bearer_authenticated_and_idempotent() {
    let app = router(state());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/api/say")
            .header(header::AUTHORIZATION, "Bearer token-a")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"request_id":"human-1","body":"Check the rollback proof"}"#,
            ))
            .unwrap()
    };
    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let replay = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let conflict = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/say")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"request_id":"human-1","body":"Changed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn http_join_and_rejoin_rotate_credentials_and_old_bearer_stops_working() {
    let app = router(state());
    let join = app.clone().oneshot(
        Request::builder().method("POST").uri("/api/join")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"room_id":"service-room","name":"B","model":"model-b","role":"reviewer","resume_token":null}"#)).unwrap()
    ).await.unwrap();
    assert_eq!(join.status(), StatusCode::OK);
    let first: serde_json::Value =
        serde_json::from_slice(&to_bytes(join.into_body(), usize::MAX).await.unwrap()).unwrap();
    let old_bearer = first["bearer_token"].as_str().unwrap().to_string();
    let old_resume = first["resume_token"].as_str().unwrap().to_string();

    let rejoin_body = serde_json::json!({
        "room_id":"service-room", "name":"B", "model":"model-b", "role":"reviewer",
        "resume_token":old_resume,
    })
    .to_string();
    let rejoin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/join")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(rejoin_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejoin.status(), StatusCode::OK);
    let second: serde_json::Value =
        serde_json::from_slice(&to_bytes(rejoin.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_ne!(second["bearer_token"], old_bearer);
    let old_status = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header(header::AUTHORIZATION, format!("Bearer {old_bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_status.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sse_stays_live_after_replay_and_delivers_new_human_event() {
    let app = router(state());
    let cookie = paired_cookie(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stream?cursor=2")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, "http://127.0.0.1:43119")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut data = response.into_body().into_data_stream();
    let say = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/say")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"live-1","body":"live interjection"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(say.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(1), data.next())
        .await
        .expect("live event deadline")
        .expect("stream remains open")
        .unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("live interjection"));
}

#[tokio::test]
async fn typed_tool_post_rejects_plumbing_and_mode_forbidden_tasks() {
    let app = router(state());
    let raw = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tool/post")
                .header(header::AUTHORIZATION, "Bearer token-b")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"raw","kind":"info","body":"hi","system_prompt":"leak"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::BAD_REQUEST);

    let post = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tool/post")
                .header(header::AUTHORIZATION, "Bearer token-b")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"typed","kind":"info","body":"typed evidence"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK);
    let tasks = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tool/tasks")
                .header(header::AUTHORIZATION, "Bearer token-b")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"request_id":"tasks","action":"list"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tasks.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn accepted_http_mutation_is_persisted_to_room_sqlite() {
    let run_dir = tempfile::tempdir().unwrap();
    let initial = room();
    RoomStore::create(run_dir.path(), &initial).unwrap();
    let shared = Arc::new(RwLock::new(initial));
    let state = ServerState::new(
        shared,
        [("token-a".into(), "seat-a".into())],
        "watch-secret",
        "http://127.0.0.1:43119",
    )
    .with_run_dir(run_dir.path());
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/say")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"persist-1","body":"durable interjection"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let store = RoomStore::open(run_dir.path()).unwrap();
    let loaded = store.load().unwrap();
    assert!(loaded
        .events
        .iter()
        .any(|event| event.body == "durable interjection"));
}

#[tokio::test]
async fn all_agent_joins_open_positions_and_all_positions_reveal_atomically() {
    let workspace = tempfile::tempdir().unwrap().keep();
    let room = Room::new(RoomConfig {
        room_id: "auto-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace,
        seats: vec![
            SeatConfig::new("human", "Adrian", "human", "human"),
            SeatConfig::new("a", "Claude", "claude", "reviewer"),
            SeatConfig::new("b", "Codex", "codex", "implementer"),
        ],
    })
    .unwrap();
    let shared = Arc::new(RwLock::new(room));
    let server = ServerState::new(
        shared.clone(),
        [("human-token".into(), "human".into())],
        "pair",
        "http://127.0.0.1:43119",
    );
    let app = router(server.clone());
    let join = |name: &str, model: &str, role: &str| {
        serde_json::json!({
            "room_id":"auto-room", "name":name, "model":model, "role":role, "resume_token":null,
        })
        .to_string()
    };
    // The implementer seat has authority, so its join carries the host-minted token.
    let codex_token = server.mint_bootstrap_token("b").unwrap();
    let a = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/join")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(join("Claude", "claude", "reviewer")))
                .unwrap(),
        )
        .await
        .unwrap();
    let a: serde_json::Value =
        serde_json::from_slice(&to_bytes(a.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(shared.read().unwrap().phase, Phase::Open);
    let b = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/join")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "room_id":"auto-room", "name":"Codex", "model":"codex",
                        "role":"implementer", "resume_token":null, "bootstrap_token":codex_token,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let b: serde_json::Value =
        serde_json::from_slice(&to_bytes(b.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(shared.read().unwrap().phase, Phase::Positions);
    let position = |request_id: &str, body: &str| {
        serde_json::json!({
            "request_id":request_id,"kind":"position","body":body,
        })
        .to_string()
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tool/post")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", a["bearer_token"].as_str().unwrap()),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(position("a-pos", "A")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(shared.read().unwrap().phase, Phase::Positions);
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/tool/post")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", b["bearer_token"].as_str().unwrap()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(position("b-pos", "B")))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(shared.read().unwrap().phase, Phase::PeerDebate);
}

#[tokio::test]
async fn admin_phase_lane_requires_human_and_records_transition() {
    let app = router(state());
    let body = r#"{"request_id":"admin-phase","target":"PeerDebate"}"#;
    let forbidden = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/advance")
                .header(header::AUTHORIZATION, "Bearer token-b")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let advanced = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/advance")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(advanced.status(), StatusCode::OK);
    let event: serde_json::Value =
        serde_json::from_slice(&to_bytes(advanced.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(event["body"], "phase:PeerDebate");
}

#[tokio::test]
async fn room_next_long_polls_until_phase_reveal_makes_peer_position_visible() {
    let app = router(state());
    let waiting_app = app.clone();
    let waiting = tokio::spawn(async move {
        waiting_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tool/next")
                    .header(header::AUTHORIZATION, "Bearer token-a")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"cursor":1,"wait_ms":30000}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let advanced = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/advance")
                .header(header::AUTHORIZATION, "Bearer token-a")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"reveal-wait","target":"PeerDebate"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(advanced.status(), StatusCode::OK);
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("long poll released")
        .unwrap();
    let events: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["body"] == "private B"));
}
