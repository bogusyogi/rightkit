#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::extract::State;
    use axum::http::{Method, Request, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::any;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::task::JoinHandle;

    #[test]
    fn approval_resolution_payload_uses_daemon_camel_case_contract() {
        let body = super::approval_resolution_body("tc-1", true);

        assert_eq!(body["toolCallId"], "tc-1");
        assert_eq!(body["approved"], true);
        assert!(body.get("tool_call_id").is_none());
    }

    #[test]
    fn telegram_poll_backoff_doubles_until_cap_and_resets() {
        let mut backoff = TelegramPollBackoff::new(Duration::from_millis(250));
        assert_eq!(backoff.record_failure(), Duration::from_millis(250));
        assert_eq!(backoff.record_failure(), Duration::from_millis(500));
        assert_eq!(backoff.record_failure(), Duration::from_millis(1000));
        backoff.reset();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.record_failure(), Duration::from_millis(250));

        let mut capped = TelegramPollBackoff::new(Duration::from_secs(30));
        assert_eq!(capped.record_failure(), Duration::from_secs(30));
        assert_eq!(capped.record_failure(), Duration::from_secs(60));
        assert_eq!(capped.record_failure(), Duration::from_secs(60));
    }

    #[derive(Clone, Default)]
    struct FakeTelegramState {
        polls: Arc<AtomicUsize>,
        non_ok_polls_remaining: Arc<AtomicUsize>,
        update_batches_remaining: Arc<AtomicUsize>,
        sent_messages: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone, Default)]
    struct FakeDaemonState {
        created_sessions: Arc<Mutex<Vec<Value>>>,
        recorded_bindings: Arc<Mutex<Vec<Value>>>,
        fanout_messages: Arc<Mutex<Vec<Value>>>,
    }

    async fn spawn_router(router: Router) -> (u16, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (port, handle)
    }

    async fn fake_telegram_handler(
        State(state): State<FakeTelegramState>,
        req: Request<Body>,
    ) -> Response {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        if method == Method::GET && path.ends_with("/getUpdates") {
            state.polls.fetch_add(1, Ordering::SeqCst);
            if state
                .non_ok_polls_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    if remaining > 0 {
                        Some(remaining - 1)
                    } else {
                        None
                    }
                })
                .is_ok()
            {
                return Json(serde_json::json!({
                    "ok": false,
                    "description": "temporary Telegram outage"
                }))
                .into_response();
            }
            let result = if state
                .update_batches_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    if remaining > 0 {
                        Some(remaining - 1)
                    } else {
                        None
                    }
                })
                .is_ok()
            {
                serde_json::json!([
                    {
                        "update_id": 10,
                        "message": {
                            "from": { "id": 42 },
                            "chat": { "id": 7001 },
                            "text": "hello from telegram"
                        }
                    },
                    {
                        "update_id": 11,
                        "message": {
                            "from": { "id": 42 },
                            "chat": { "id": 7001 },
                            "text": "/stop"
                        }
                    }
                ])
            } else {
                serde_json::json!([])
            };
            return Json(serde_json::json!({ "ok": true, "result": result })).into_response();
        }

        if method == Method::POST && path.ends_with("/sendMessage") {
            let body = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            state.sent_messages.lock().unwrap().push(value);
            return Json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1 }
            }))
            .into_response();
        }

        (StatusCode::NOT_FOUND, "unexpected Telegram API request").into_response()
    }

    async fn fake_daemon_handler(
        State(state): State<FakeDaemonState>,
        req: Request<Body>,
    ) -> Response {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let authorized = req
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer daemon-token");
        if !authorized {
            return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        }

        if method == Method::POST && path == "/sessions" {
            let body = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            state.created_sessions.lock().unwrap().push(value);
            return Json(serde_json::json!({
                "id": "telegram-default",
                "toolset": "telegram-remote",
                "mcpChannel": "telegram"
            }))
            .into_response();
        }

        if method == Method::POST && path == "/remote/bindings" {
            let body = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            state.recorded_bindings.lock().unwrap().push(value);
            return Json(serde_json::json!({ "ok": true })).into_response();
        }

        if method == Method::POST && path == "/remote/channels/telegram/fanout" {
            let body = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            state.fanout_messages.lock().unwrap().push(value);
            return Json(serde_json::json!({
                "ok": true,
                "channelId": "telegram",
                "attempted": 1,
                "delivered": 1,
                "failed": 0,
                "results": [{
                    "bindingId": "telegram-target",
                    "sessionId": "telegram-default",
                    "targetRedacted": "telegram:target-1",
                    "status": "delivered",
                    "seq": 1,
                    "error": null
                }]
            }))
            .into_response();
        }

        if method == Method::GET && path == "/sessions/telegram-default/events/history" {
            return Json(Vec::<rightkit_agent_engine_protocol::ProtocolEvent>::new()).into_response();
        }

        (StatusCode::NOT_FOUND, "unexpected daemon request").into_response()
    }

    #[tokio::test]
    async fn long_poll_loop_submits_message_and_sends_reply_against_local_telegram_api() {
        let telegram = FakeTelegramState::default();
        telegram.update_batches_remaining.store(1, Ordering::SeqCst);
        let daemon = FakeDaemonState::default();

        let (telegram_port, telegram_handle) = spawn_router(
            Router::new()
                .fallback(any(fake_telegram_handler))
                .with_state(telegram.clone()),
        )
        .await;
        let (daemon_port, daemon_handle) = spawn_router(
            Router::new()
                .fallback(any(fake_daemon_handler))
                .with_state(daemon.clone()),
        )
        .await;

        let runtime_status = new_telegram_runtime_status_store();
        let bot_handle = tokio::spawn(run_telegram_bot_with_api_base(
            "test-token".to_string(),
            vec![42],
            daemon_port,
            "daemon-token".to_string(),
            runtime_status.clone(),
            format!("http://127.0.0.1:{telegram_port}"),
            0,
            Duration::from_millis(1),
        ));

        for _ in 0..100 {
            let submitted = daemon.fanout_messages.lock().unwrap().len();
            let replies = telegram.sent_messages.lock().unwrap().len();
            if submitted >= 2 && replies >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        bot_handle.abort();
        telegram_handle.abort();
        daemon_handle.abort();

        let bindings = daemon.recorded_bindings.lock().unwrap().clone();
        assert_eq!(bindings.len(), 2, "bindings: {bindings:?}");
        assert_eq!(bindings[0]["channelId"], "telegram");
        assert_eq!(bindings[0]["targetKey"], "7001");
        assert_eq!(bindings[0]["sessionId"], "telegram-default");
        assert_eq!(bindings[1]["channelId"], "telegram");
        assert_eq!(bindings[1]["targetKey"], "7001");
        assert_eq!(bindings[1]["sessionId"], "telegram-default");

        let fanout = daemon.fanout_messages.lock().unwrap().clone();
        assert_eq!(fanout.len(), 2, "fanout: {fanout:?}");
        assert_eq!(fanout[0]["text"], "hello from telegram");
        assert_eq!(fanout[0]["source"], "telegram-long-poll");
        assert_eq!(fanout[0]["targetKey"], "7001");
        assert_eq!(fanout[0]["sessionId"], "telegram-default");
        assert_eq!(fanout[1]["text"], "/stop");
        assert_eq!(fanout[1]["source"], "telegram-long-poll");
        assert_eq!(fanout[1]["targetKey"], "7001");
        assert_eq!(fanout[1]["sessionId"], "telegram-default");

        let created = daemon.created_sessions.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "created: {created:?}");
        assert_eq!(created[0]["id"], "telegram-default");
        assert_eq!(created[0]["title"], "Telegram remote");
        assert_eq!(created[0]["toolset"], "telegram-remote");
        assert_eq!(created[0]["mcpChannel"], "telegram");

        let replies = telegram.sent_messages.lock().unwrap().clone();
        assert_eq!(replies.len(), 2, "replies: {replies:?}");
        assert_eq!(replies[0]["chat_id"], 7001);
        assert_eq!(replies[0]["text"], "📨 Sent.");
        assert_eq!(replies[1]["chat_id"], 7001);
        assert_eq!(replies[1]["text"], "🛑 Stop requested.");

        let runtime = runtime_status.lock().unwrap().clone();
        assert!(runtime.last_success_at.is_some(), "runtime: {runtime:?}");
        assert_eq!(runtime.active_chat_count, 1);
        assert_eq!(runtime.chat_cursors[0].session_id, "telegram-default");
    }

    #[tokio::test]
    async fn long_poll_loop_recovers_after_transient_telegram_failure() {
        let telegram = FakeTelegramState::default();
        telegram.non_ok_polls_remaining.store(1, Ordering::SeqCst);
        telegram.update_batches_remaining.store(1, Ordering::SeqCst);
        let daemon = FakeDaemonState::default();

        let (telegram_port, telegram_handle) = spawn_router(
            Router::new()
                .fallback(any(fake_telegram_handler))
                .with_state(telegram.clone()),
        )
        .await;
        let (daemon_port, daemon_handle) = spawn_router(
            Router::new()
                .fallback(any(fake_daemon_handler))
                .with_state(daemon.clone()),
        )
        .await;

        let runtime_status = new_telegram_runtime_status_store();
        let bot_handle = tokio::spawn(run_telegram_bot_with_api_base(
            "test-token".to_string(),
            vec![42],
            daemon_port,
            "daemon-token".to_string(),
            runtime_status.clone(),
            format!("http://127.0.0.1:{telegram_port}"),
            0,
            Duration::from_millis(1),
        ));

        for _ in 0..100 {
            let submitted = daemon.fanout_messages.lock().unwrap().len();
            if submitted >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        bot_handle.abort();
        telegram_handle.abort();
        daemon_handle.abort();

        assert!(
            telegram.polls.load(Ordering::SeqCst) >= 2,
            "expected retry after transient failure"
        );
        let fanout = daemon.fanout_messages.lock().unwrap().clone();
        assert_eq!(fanout.len(), 2, "fanout: {fanout:?}");
        let runtime = runtime_status.lock().unwrap().clone();
        assert!(runtime.last_success_at.is_some(), "runtime: {runtime:?}");
        assert_eq!(runtime.last_error, None);
        assert_eq!(runtime.consecutive_failures, 0);
        assert_eq!(runtime.next_retry_ms, None);
    }
}
