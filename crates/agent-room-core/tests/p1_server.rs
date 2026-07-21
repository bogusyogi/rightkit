use agent_room_core::protocol::{Phase, Room, RoomConfig, SeatConfig};
use agent_room_core::server::{router, ServerState};
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use std::sync::{Arc, RwLock};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:43119";

fn state() -> ServerState {
    let workspace = tempfile::tempdir().unwrap().keep();
    let mut room = Room::new(RoomConfig {
        room_id: "p1-browser-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace,
        seats: vec![
            SeatConfig::new("human", "Adrian", "human", "human"),
            SeatConfig::new("reviewer", "Codex", "codex", "reviewer"),
        ],
    })
    .unwrap();
    room.advance("human", Phase::Positions).unwrap();
    ServerState::new(
        Arc::new(RwLock::new(room)),
        [("human-bearer".into(), "human".into())],
        "pair-code",
        ORIGIN,
    )
}

fn sealed_state() -> ServerState {
    let workspace = tempfile::tempdir().unwrap().keep();
    let mut room = Room::new(RoomConfig {
        room_id: "sealed-browser-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace,
        seats: vec![
            SeatConfig::new("human", "Adrian", "human", "human"),
            SeatConfig::new("reviewer", "Codex", "codex", "reviewer"),
        ],
    })
    .unwrap();
    for phase in [
        Phase::Positions,
        Phase::PeerDebate,
        Phase::Dispositions,
        Phase::AuthorReview,
        Phase::Voting,
        Phase::Verdict,
    ] {
        room.advance("human", phase).unwrap();
    }
    room.seal("human").unwrap();
    ServerState::new(
        Arc::new(RwLock::new(room)),
        [("human-bearer".into(), "human".into())],
        "pair-code",
        ORIGIN,
    )
}

async fn cookie(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/pair")
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("pairing_code=pair-code"))
                .unwrap(),
        )
        .await
        .unwrap();
    response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn authenticated_dashboard_exposes_browser_say_control() {
    let app = router(state());
    let cookie = cookie(&app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("<textarea"));
    assert!(html.contains("Interject"));
    assert!(html.contains("/api/browser-say"));
    assert!(html.contains("Discussion"));
    assert!(html.contains("Activity"));
    assert!(html.contains("Artifacts"));
    assert!(html.contains("id=\"say-target\""));
    assert!(html.contains("renderActivity"));
    assert!(html.contains("renderArtifacts"));
    assert!(html.contains("renderPhaseProgress"));
}

#[tokio::test]
async fn browser_say_requires_cookie_and_exact_origin_then_posts_human_event() {
    let app = router(state());
    let cookie = cookie(&app).await;
    let body = r#"{"request_id":"browser-say-1","body":"human browser interjection"}"#;

    for origin in [None, Some("https://evil.example")] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/browser-say")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/browser-say")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = app
        .oneshot(
            Request::builder()
                .uri("/api/watch-status")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(snapshot.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(value["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["body"] == "human browser interjection"));
}

#[tokio::test]
async fn browser_say_accepts_an_addressed_seat_and_watch_status_exposes_live_progress() {
    let app = router(state());
    let cookie = cookie(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/browser-say")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"request_id":"browser-target-1","body":"Inspect rollback","target_seat_id":"reviewer"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let event: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(event["target_seat_id"], "reviewer");

    let snapshot = app
        .oneshot(
            Request::builder()
                .uri("/api/watch-status")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(snapshot.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["seats"][1]["activity"]["state"], "attention");
    assert_eq!(
        value["seats"][1]["activity"]["detail"],
        "Addressed in message #1"
    );
    assert_eq!(value["phase_progress"]["required"], 1);
    assert_eq!(value["phase_progress"]["responded"], 0);
    assert_eq!(
        value["phase_progress"]["outstanding_seat_ids"][0],
        "reviewer"
    );
}

#[tokio::test]
async fn watch_status_projects_actionable_fallback_diagnostics_without_absolute_log_paths() {
    let run_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        run_dir.path().join("seat-sessions.json"),
        r#"{"schema_version":1,"status":"aborted","seats":[{"seat":"codex","seat_id":"reviewer","pid":42,"status":"failed","ready_event_seq":null,"stdout_log":"C:\\private\\seat-codex.stdout.log","stderr_log":"C:\\private\\seat-codex.stderr.log"}]}"#,
    )
    .unwrap();
    std::fs::write(
        run_dir.path().join("launch.failure.json"),
        r#"{"schema_version":1,"status":"aborted","stage":"preflight","seat_failures":[{"seat":"codex","code":"authentication_failed","detail":"Not logged in","stdout_log":"C:\\private\\seat-codex.stdout.log","stderr_log":"C:\\private\\seat-codex.stderr.log"}]}"#,
    )
    .unwrap();
    let app = router(state().with_run_dir(run_dir.path()));
    let cookie = cookie(&app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/watch-status")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();

    assert_eq!(value["health"]["status"], "fallback_required");
    assert_eq!(
        value["health"]["failures"][0]["action"],
        "Sign in to the seat CLI, then relaunch the Council."
    );
    assert_eq!(
        value["health"]["failures"][0]["stderr_log"],
        "seat-codex.stderr.log"
    );
    assert!(!value.to_string().contains("C:\\\\private"));
    assert_eq!(value["seats"][1]["activity"]["state"], "failed");
}

#[tokio::test]
async fn sealed_dashboard_disables_and_rejects_human_interjection() {
    let app = router(sealed_state());
    let cookie = cookie(&app).await;
    let snapshot = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/watch-status")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(snapshot.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["sealed"], true);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/browser-say")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"request_id":"late","body":"too late"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let dashboard = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = String::from_utf8(
        to_bytes(dashboard.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Review complete · room sealed"));
    assert!(html.contains("say.disabled=value.sealed"));
}
