/// Run the Telegram long-poll loop. Intended to be spawned as a tokio task.
///
/// - `bot_token`: the Telegram Bot API token
/// - `allowed_users`: Telegram user IDs that are permitted to interact
/// - `port`: local daemon HTTP port (for self-calls)
/// - `daemon_token`: bearer token accepted by the daemon API
pub async fn run_telegram_bot(
    bot_token: String,
    allowed_users: Vec<u64>,
    port: u16,
    daemon_token: String,
    runtime_status: TelegramRuntimeStatusStore,
) {
    run_telegram_bot_with_api_base(
        bot_token,
        allowed_users,
        port,
        daemon_token,
        runtime_status,
        "https://api.telegram.org".to_string(),
        25,
        Duration::from_secs(5),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn run_telegram_bot_with_api_base(
    bot_token: String,
    allowed_users: Vec<u64>,
    port: u16,
    daemon_token: String,
    runtime_status: TelegramRuntimeStatusStore,
    api_base_url: String,
    poll_timeout_secs: u64,
    retry_delay: Duration,
) {
    // Long polls hold the request open for `poll_timeout_secs`; the ceiling leaves headroom.
    let http = match rightkit_http::http_client::endpoint_client(
        &api_base_url,
        Duration::from_secs(poll_timeout_secs.saturating_add(35)),
    ) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(error = %error, "telegram could not build its HTTP client");
            return;
        }
    };
    let daemon = HttpDaemonClient::new(port, daemon_token);
    let default_session = "telegram-default";
    if let Err(error) = daemon
        .ensure_session(
            default_session,
            "Telegram remote",
            "telegram-remote",
            "telegram",
        )
        .await
    {
        tracing::warn!(error = %error, "telegram failed to ensure remote session");
        update_status(&runtime_status, |status| {
            status.last_error_at = Some(status_timestamp());
            status.last_error = Some(format!("failed to ensure remote session: {error}"));
        });
        return;
    }

    let tg_config = TelegramConfig {
        bot_token: bot_token.clone(),
        allowed_users,
        default_session: default_session.to_string(),
    };
    let binding_store_path = telegram_binding_store_path();
    let bot = TelegramBot::new_with_binding_store(daemon, &tg_config, &binding_store_path);
    let allowed_users: HashSet<u64> = tg_config.allowed_users.iter().copied().collect();

    let mut offset: i64 = 0;
    let mut chat_event_seq: HashMap<i64, i64> = HashMap::new();
    let mut chat_event_sessions: HashMap<i64, String> = HashMap::new();
    let mut poll_backoff = TelegramPollBackoff::new(retry_delay);

    update_status(&runtime_status, |status| {
        status.started_at = Some(status_timestamp());
        status.last_error = None;
        status.consecutive_failures = 0;
        status.next_retry_ms = None;
    });
    info!("telegram long-poll loop started");

    loop {
        let url = format!(
            "{}?offset={}&timeout={}",
            telegram_api_url(&api_base_url, &bot_token, "getUpdates"),
            offset,
            poll_timeout_secs
        );

        update_status(&runtime_status, |status| {
            status.last_poll_at = Some(status_timestamp());
            status.update_offset = offset;
        });

        let resp = match http
            .send_any(&rightkit_http::Request::get(url).redact_value(&bot_token))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let delay = poll_backoff.record_failure();
                tracing::warn!(error = %e, "telegram getUpdates network error");
                update_status(&runtime_status, |status| {
                    status.last_error_at = Some(status_timestamp());
                    status.last_error = Some(format!("getUpdates network error: {e}"));
                    status.consecutive_failures = poll_backoff.failures();
                    status.next_retry_ms = Some(delay.as_millis() as u64);
                });
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        let body: Value = match resp.json() {
            Ok(v) => v,
            Err(e) => {
                let delay = poll_backoff.record_failure();
                tracing::warn!(error = %e, "telegram getUpdates parse error");
                update_status(&runtime_status, |status| {
                    status.last_error_at = Some(status_timestamp());
                    status.last_error = Some(format!("getUpdates parse error: {e}"));
                    status.consecutive_failures = poll_backoff.failures();
                    status.next_retry_ms = Some(delay.as_millis() as u64);
                });
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        if !body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let delay = poll_backoff.record_failure();
            tracing::warn!(body = %body, "telegram getUpdates non-ok response");
            update_status(&runtime_status, |status| {
                status.last_error_at = Some(status_timestamp());
                status.last_error = Some(format!("getUpdates non-ok response: {body}"));
                status.consecutive_failures = poll_backoff.failures();
                status.next_retry_ms = Some(delay.as_millis() as u64);
            });
            tokio::time::sleep(delay).await;
            continue;
        }
        poll_backoff.reset();
        update_status(&runtime_status, |status| {
            status.last_success_at = Some(status_timestamp());
            status.last_error = None;
            status.consecutive_failures = 0;
            status.next_retry_ms = None;
            status.update_offset = offset;
        });

        let results = match body.get("result").and_then(|r| r.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                // Empty poll — loop immediately (offset unchanged).
                continue;
            }
        };

        for update in &results {
            let (update_id, tg_update, cq_id) = match parse_update(update) {
                Some(t) => t,
                None => {
                    // Unknown update shape — still advance the offset.
                    if let Some(uid) = update.get("update_id").and_then(|u| u.as_u64()) {
                        offset = offset.max(uid as i64 + 1);
                    }
                    continue;
                }
            };

            // Advance offset before the async call so we never re-process.
            offset = offset.max(update_id as i64 + 1);

            // Answer callbacks immediately to dismiss the Telegram spinner.
            if let Some(ref cqid) = cq_id {
                answer_callback(&http, &api_base_url, &bot_token, cqid).await;
            }

            let authorized_update = allowed_users.contains(&tg_update.user_id());
            let chat_id = tg_update.chat_id();

            match bot.handle(tg_update).await {
                Ok(reply) => send_reply(&http, &api_base_url, &bot_token, reply).await,
                Err(e) => {
                    tracing::warn!(chat_id = %chat_id, error = %e, "telegram update handler error");
                }
            }

            if authorized_update {
                let session_id = bot.session_for_chat(chat_id);
                let previous = chat_event_sessions.insert(chat_id, session_id.clone());
                if previous.as_deref() == Some(session_id.as_str()) {
                    chat_event_seq.entry(chat_id).or_insert(0);
                } else {
                    chat_event_seq.insert(chat_id, 0);
                }
                update_chat_cursors(&runtime_status, &chat_event_seq, &chat_event_sessions);
            }
        }

        for (chat_id, after_seq) in chat_event_seq.iter_mut() {
            match bot.event_replies_after(*chat_id, *after_seq).await {
                Ok((next_seq, replies)) => {
                    *after_seq = next_seq;
                    for reply in replies {
                        send_reply(&http, &api_base_url, &bot_token, reply).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(chat_id = %chat_id, error = %e, "telegram event poll error");
                    update_status(&runtime_status, |status| {
                        status.last_error_at = Some(status_timestamp());
                        status.last_error = Some(format!("event poll error: {e}"));
                    });
                }
            }
        }
        update_chat_cursors(&runtime_status, &chat_event_seq, &chat_event_sessions);
    }
}
