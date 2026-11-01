// CR3 #57 item 5: nonce/digest idempotency for retryable session ingress.
// Each route below: identical retry replays and does not re-apply, a
// changed payload under the same nonce conflicts, and no-nonce behavior is
// unchanged.

fn cr3_state_with_storage() -> (
    DaemonState,
    std::sync::Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>,
) {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("s1", None, "/tmp", "Test", None);
    (state, storage)
}

async fn cr3_post(app: &axum::Router, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn fork_duplicate_retry_replays_one_session_and_conflicts_on_changed_body() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let body = r#"{"title":"Try","nonce":"fork-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/s1/fork", body).await;
    assert_eq!(first_status, StatusCode::OK);
    let (retry_status, retry) = cr3_post(&app, "/sessions/s1/fork", body).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(
        retry["id"], first["id"],
        "retry must replay, not fork again"
    );
    assert_eq!(
        state.list_sessions().len(),
        2,
        "parent plus exactly one fork"
    );

    let (conflict, _) = cr3_post(
        &app,
        "/sessions/s1/fork",
        r#"{"title":"Different","nonce":"fork-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
    assert_eq!(state.list_sessions().len(), 2);

    // Without a nonce every request still forks.
    let (a_status, a) = cr3_post(&app, "/sessions/s1/fork", r#"{"title":"Plain"}"#).await;
    let (b_status, b) = cr3_post(&app, "/sessions/s1/fork", r#"{"title":"Plain"}"#).await;
    assert_eq!(a_status, StatusCode::OK);
    assert_eq!(b_status, StatusCode::OK);
    assert_ne!(a["id"], b["id"]);
}

#[tokio::test]
async fn fork_refusal_releases_the_nonce_for_a_corrected_retry() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    // Unknown parent: refused (4xx) before any effect.
    let (status, _) = cr3_post(
        &app,
        "/sessions/ghost/fork",
        r#"{"id":"child-x","nonce":"fork-2"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The same key is not stuck pending: a retry is evaluated again.
    let (status, _) = cr3_post(
        &app,
        "/sessions/ghost/fork",
        r#"{"id":"child-x","nonce":"fork-2"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clone_duplicate_retry_replays_one_clone() {
    let (state, storage) = cr3_state_with_storage();
    wait_for_session(&storage, "s1").await;
    let app = build_router(state);
    let body = r#"{"title":"Copy","nonce":"clone-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/s1/clone", body).await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    let (retry_status, retry) = cr3_post(&app, "/sessions/s1/clone", body).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry, first, "retry must replay the original clone");

    let (conflict, _) = cr3_post(
        &app,
        "/sessions/s1/clone",
        r#"{"title":"Other","nonce":"clone-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
}

#[tokio::test]
async fn steer_duplicate_retry_enqueues_once() {
    let (state, _storage) = cr3_state_with_storage();
    // The steering queue registry is process-global per session id, so use
    // an id no other test touches.
    state.create_session("cr3-steer", None, "/tmp", "Steer", None);
    let app = build_router(state);
    let body = r#"{"text":"keep going","nonce":"steer-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/cr3-steer/steer", body).await;
    assert_eq!(first_status, StatusCode::OK);
    let (retry_status, retry) = cr3_post(&app, "/sessions/cr3-steer/steer", body).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry, first, "retry must replay the original response");

    let (_, listed) = cr3_post(&app, "/sessions/cr3-steer/steer", r#"{"action":"list"}"#).await;
    assert_eq!(
        listed["queued"].as_array().map(Vec::len),
        Some(1),
        "the duplicate retry must not enqueue a second item"
    );

    let (conflict, _) = cr3_post(
        &app,
        "/sessions/cr3-steer/steer",
        r#"{"text":"different","nonce":"steer-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
}

#[tokio::test]
async fn set_goal_duplicate_retry_keeps_the_first_goal_id() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state);
    let body = r#"{"text":"ship it","nonce":"goal-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/s1/goal", body).await;
    assert_eq!(first_status, StatusCode::OK);
    // Goal ids are minted from the clock; a re-executed request would
    // produce a different id.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let (retry_status, retry) = cr3_post(&app, "/sessions/s1/goal", body).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry["goalId"], first["goalId"]);

    let (conflict, _) = cr3_post(
        &app,
        "/sessions/s1/goal",
        r#"{"text":"another goal","nonce":"goal-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
}

#[tokio::test]
async fn rewind_duplicate_retry_replays_without_rewinding_again() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let body = r#"{"point":{"text":"hi","nthFromEnd":0,"createdAt":"2026-01-01T00:00:00Z"},"mode":"files","nonce":"rw-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/s1/rewind", body).await;
    assert_eq!(first_status, StatusCode::OK, "{first}");

    // Remove the live session: re-executing the rewind would now fail with
    // CONFLICT (session not found). A replay never reaches the handler.
    lock_recover(&state.sessions, "sessions").remove("s1");
    let (retry_status, retry) = cr3_post(&app, "/sessions/s1/rewind", body).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry, first);

    let (conflict, _) = cr3_post(
            &app,
            "/sessions/s1/rewind",
            r#"{"point":{"text":"other","nthFromEnd":0,"createdAt":"2026-01-01T00:00:00Z"},"mode":"files","nonce":"rw-1"}"#,
        )
        .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
}

#[tokio::test]
async fn orchestrate_duplicate_retry_records_one_request() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let body = r#"{"goal":"ship the phase","nonce":"orch-1"}"#;

    let (first_status, _) = cr3_post(&app, "/sessions/s1/orchestrate", body).await;
    assert_eq!(first_status, StatusCode::OK);
    let (retry_status, _) = cr3_post(&app, "/sessions/s1/orchestrate", body).await;
    assert_eq!(retry_status, StatusCode::OK);

    let live = state.get_session("s1").unwrap();
    let user_turns = lock_recover(&live.transcript, "session.transcript")
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::User { .. }))
        .count();
    assert_eq!(user_turns, 1, "retry must not start a second run");

    let (conflict, _) = cr3_post(
        &app,
        "/sessions/s1/orchestrate",
        r#"{"goal":"a different goal","nonce":"orch-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
}

#[tokio::test]
async fn nonce_without_durable_storage_is_refused_not_silently_unprotected() {
    let state = state_with_session("s1");
    let app = build_router(state);
    let (status, _) = cr3_post(
        &app,
        "/sessions/s1/goal",
        r#"{"text":"x","nonce":"no-store"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // No nonce: unchanged behavior.
    let (status, _) = cr3_post(&app, "/sessions/s1/goal", r#"{"text":"x"}"#).await;
    assert_eq!(status, StatusCode::OK);
}

async fn cr3_post_with_key(
    app: &axum::Router,
    uri: &str,
    key: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("idempotency-key", key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn compact_duplicate_retry_with_idempotency_key_compacts_once() {
    let (state, _storage) = cr3_state_with_storage();
    let session_id = "cr3-compact";
    let mut live = state.create_session(session_id, None, "/tmp", "Compact", None);
    let (command_tx, mut command_rx) =
        tokio::sync::mpsc::channel::<rightkit_agent_engine_core::CompactionCommandV1>(4);
    let dispatch_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = dispatch_count.clone();
    tokio::spawn(async move {
        while let Some(command) = command_rx.recv().await {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(response) = command.response {
                let _ = response.send(Ok(rightkit_agent_engine_core::CompactionOutcome {
                    compacted: true,
                    summary: "cr3 summary".into(),
                    dropped_count: 3,
                    archive: None,
                }));
            }
        }
    });
    state
        .sessions
        .lock()
        .expect("session map lock")
        .remove(session_id);
    Arc::get_mut(&mut live)
        .expect("fixture session must be uniquely owned")
        .compact_tx = Some(command_tx);
    state
        .sessions
        .lock()
        .expect("session map lock")
        .insert(session_id.into(), live);
    let app = build_router(state);
    let uri = "/sessions/cr3-compact/compact";

    let (first_status, first) = cr3_post_with_key(&app, uri, "compact-1").await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    let (retry_status, retry) = cr3_post_with_key(&app, uri, "compact-1").await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry, first, "retry must replay the original compaction");
    assert_eq!(
        dispatch_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "duplicate retry must not compact twice"
    );

    // A new key is a new request; no key keeps the old unprotected behavior.
    let (new_key_status, _) = cr3_post_with_key(&app, uri, "compact-2").await;
    assert_eq!(new_key_status, StatusCode::OK);
    let (no_key_status, _) = cr3_post(&app, uri, "").await;
    assert_eq!(no_key_status, StatusCode::OK);
    assert_eq!(dispatch_count.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn compact_idempotency_key_without_durable_storage_is_refused() {
    let state = state_with_session("s1");
    let app = build_router(state);
    let (status, _) = cr3_post_with_key(&app, "/sessions/s1/compact", "no-store").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// #47: turn-submission ingress qualification. `POST /sessions/{id}/messages`
// is the one HTTP route that submits a user turn; the orchestrate, steer and
// goal routes above carry the same nonce contract. A replay returns the
// original result without a second turn, a different key is a new turn, and
// a missing key keeps the documented unprotected behavior.

async fn cr3_post_json_with_key(
    app: &axum::Router,
    uri: &str,
    key: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn cr3_user_turns(state: &DaemonState, id: &str) -> usize {
    let live = state.get_session(id).unwrap();
    let count = lock_recover(&live.transcript, "session.transcript")
        .iter()
        .filter(|message| matches!(message, TranscriptMessage::User { .. }))
        .count();
    count
}

#[tokio::test]
async fn post_message_body_nonce_replay_starts_one_turn() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let body = r#"{"text":"hello","nonce":"msg-1"}"#;

    let (first_status, first) = cr3_post(&app, "/sessions/s1/messages", body).await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    assert_eq!(cr3_user_turns(&state, "s1"), 1);

    let (retry_status, retry) = cr3_post(&app, "/sessions/s1/messages", body).await;
    assert_eq!(retry_status, StatusCode::OK, "{retry}");
    assert_eq!(retry, first, "replay must return the original result");
    assert_eq!(
        cr3_user_turns(&state, "s1"),
        1,
        "replay must not start a second turn"
    );

    // Same key, changed body: conflict, still one turn.
    let (conflict, _) = cr3_post(
        &app,
        "/sessions/s1/messages",
        r#"{"text":"different","nonce":"msg-1"}"#,
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);
    assert_eq!(cr3_user_turns(&state, "s1"), 1);

    // A different key with the same body is a new turn.
    let (other_status, other) = cr3_post(
        &app,
        "/sessions/s1/messages",
        r#"{"text":"hello","nonce":"msg-2"}"#,
    )
    .await;
    assert_eq!(other_status, StatusCode::OK, "{other}");
    assert_ne!(other["seq"], first["seq"]);
    assert_eq!(cr3_user_turns(&state, "s1"), 2);
}

#[tokio::test]
async fn post_message_idempotency_key_header_replay_starts_one_turn() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let uri = "/sessions/s1/messages";
    let body = r#"{"text":"via header"}"#;

    let (first_status, first) = cr3_post_json_with_key(&app, uri, "hdr-1", body).await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    let (retry_status, retry) = cr3_post_json_with_key(&app, uri, "hdr-1", body).await;
    assert_eq!(retry_status, StatusCode::OK, "{retry}");
    assert_eq!(retry, first);
    assert_eq!(cr3_user_turns(&state, "s1"), 1);

    let (conflict, _) = cr3_post_json_with_key(&app, uri, "hdr-1", r#"{"text":"changed"}"#).await;
    assert_eq!(conflict, StatusCode::CONFLICT);

    let (new_key_status, _) = cr3_post_json_with_key(&app, uri, "hdr-2", body).await;
    assert_eq!(new_key_status, StatusCode::OK);
    assert_eq!(cr3_user_turns(&state, "s1"), 2);

    // The body nonce wins over the header: this replays the request keyed
    // "hdr-1", not a new request keyed "ignored".
    let (nonce_status, _) = cr3_post_json_with_key(
        &app,
        uri,
        "ignored",
        r#"{"text":"via header","nonce":"hdr-1"}"#,
    )
    .await;
    assert_eq!(nonce_status, StatusCode::OK);
    assert_eq!(
        cr3_user_turns(&state, "s1"),
        2,
        "body nonce hdr-1 replays the first turn"
    );
}

#[tokio::test]
async fn post_message_without_key_starts_a_turn_each_time() {
    let (state, _storage) = cr3_state_with_storage();
    let app = build_router(state.clone());
    let body = r#"{"text":"plain"}"#;
    let (a_status, _) = cr3_post(&app, "/sessions/s1/messages", body).await;
    let (b_status, _) = cr3_post(&app, "/sessions/s1/messages", body).await;
    assert_eq!(a_status, StatusCode::OK);
    assert_eq!(b_status, StatusCode::OK);
    assert_eq!(
        cr3_user_turns(&state, "s1"),
        2,
        "no key means no deduplication"
    );
}

#[tokio::test]
async fn post_message_key_without_durable_storage_is_refused() {
    let state = state_with_session("s1");
    let app = build_router(state.clone());
    let (status, _) =
        cr3_post_json_with_key(&app, "/sessions/s1/messages", "no-store", r#"{"text":"x"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(cr3_user_turns(&state, "s1"), 0);
}
