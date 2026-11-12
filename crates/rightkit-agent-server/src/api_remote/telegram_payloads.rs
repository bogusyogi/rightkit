async fn fetch_telegram_updates(
    api_base: &str,
    token: &str,
    limit: u32,
) -> Result<serde_json::Value, String> {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/bot{token}/getUpdates?limit={limit}");
    let client = rightkit_agent_daemon::http_client::endpoint_client(
        &url,
        std::time::Duration::from_secs(60),
    )?;
    // The bot token is part of the URL path: register it so no error text echoes it.
    let response = rightkit_agent_daemon::http_client::send_any(
        &client,
        &rightkit_http::Request::get(url).redact_value(token),
    )
    .await?;
    let status = response.status;
    response
        .json::<serde_json::Value>()
        .map_err(|e| format!("Telegram returned {status}, but the response was not JSON: {e}"))
}

fn telegram_users_payload(value: &serde_json::Value) -> serde_json::Value {
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !ok {
        return serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": true,
            "detail": value.get("description").and_then(serde_json::Value::as_str).unwrap_or("Telegram rejected the getUpdates request."),
            "users": [],
            "userCount": 0,
        });
    }

    let mut seen = std::collections::BTreeMap::<i64, serde_json::Value>::new();
    let updates = value
        .get("result")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for update in updates {
        let Some(update_id) = update.get("update_id").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let Some(message) = update
            .get("message")
            .or_else(|| update.get("edited_message"))
            .or_else(|| update.get("callback_query").and_then(|c| c.get("message")))
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        let Some(from) = update
            .get("message")
            .or_else(|| update.get("edited_message"))
            .and_then(|m| m.get("from"))
            .or_else(|| update.get("callback_query").and_then(|c| c.get("from")))
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        let Some(user_id) = from.get("id").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let chat = message.get("chat").and_then(serde_json::Value::as_object);
        seen.insert(
            user_id,
            serde_json::json!({
                "userId": user_id,
                "username": from.get("username").and_then(serde_json::Value::as_str),
                "firstName": from.get("first_name").and_then(serde_json::Value::as_str),
                "lastName": from.get("last_name").and_then(serde_json::Value::as_str),
                "chatId": chat.and_then(|chat| chat.get("id")).and_then(serde_json::Value::as_i64),
                "chatType": chat.and_then(|chat| chat.get("type")).and_then(serde_json::Value::as_str),
                "lastUpdateId": update_id,
            }),
        );
    }

    let users = seen.into_values().collect::<Vec<_>>();
    serde_json::json!({
        "verdict": if users.is_empty() { "warn" } else { "pass" },
        "telegramTokenConfigured": true,
        "userCount": users.len(),
        "users": users,
        "notes": [
            "Message bodies and bot tokens are intentionally omitted.",
            "Ask the intended operator to message the bot once if no users are listed."
        ],
    })
}

fn telegram_test_payload(
    state: &DaemonState,
    sql_binding_count: Option<usize>,
) -> serde_json::Value {
    let status = &state.telegram_status;
    let runtime = status.runtime.lock().ok().map(|guard| guard.clone());
    let binding_store =
        rightkit_bot_messaging::load_telegram_session_bindings(&status.binding_store_path);
    let (json_binding_count, binding_store_readable, binding_store_error) = match binding_store {
        Ok(bindings) => (bindings.chats.len(), true, None),
        Err(error) => (0, false, Some(error)),
    };
    let binding_count = sql_binding_count
        .filter(|count| *count > 0)
        .unwrap_or(json_binding_count);
    let binding_detail = if binding_store_readable {
        format!("{binding_count} binding(s), readable")
    } else {
        binding_store_error
            .as_deref()
            .unwrap_or("not readable")
            .to_string()
    };

    let mut checks = Vec::new();
    checks.push(check(
        "telegram_token",
        "Telegram bot token",
        if status.token_configured {
            "pass"
        } else {
            "block"
        },
        if status.token_configured {
            "configured"
        } else {
            "not configured"
        },
        if status.token_configured {
            ""
        } else {
            "Set remote.telegram-token in config or RIGHTKIT_TELEGRAM_TOKEN."
        },
    ));
    checks.push(check(
        "allowed_users",
        "Telegram allowed users",
        if status.allowed_user_count > 0 {
            "pass"
        } else {
            "block"
        },
        &format!("{} allowed user(s)", status.allowed_user_count),
        if status.allowed_user_count > 0 {
            ""
        } else {
            "Add at least one Telegram user id to remote.telegram-allowed-users; an empty allow-list denies everyone."
        },
    ));
    checks.push(check(
        "binding_store",
        "Telegram binding store",
        if binding_store_readable {
            "pass"
        } else {
            "warn"
        },
        &binding_detail,
        if binding_store_readable {
            ""
        } else {
            "Fix the binding store path permissions so chat-to-session bindings can persist."
        },
    ));

    let runtime_check = match runtime.as_ref() {
        Some(runtime) if runtime.last_success_at.is_some() => check(
            "long_poll",
            "Telegram long poll",
            "pass",
            "polling has succeeded",
            "",
        ),
        Some(runtime) if runtime.last_error.is_some() => check(
            "long_poll",
            "Telegram long poll",
            "warn",
            runtime.last_error.as_deref().unwrap_or("last poll errored"),
            "Inspect the token, network, and Telegram API reachability.",
        ),
        _ if status.token_configured && status.long_poll_requested => check(
            "long_poll",
            "Telegram long poll",
            "warn",
            "configured but no successful poll has been observed in this daemon run",
            "Send a Telegram message to the bot or wait for the next poll heartbeat.",
        ),
        _ => check(
            "long_poll",
            "Telegram long poll",
            "block",
            "not running because Telegram is not configured",
            "Configure a bot token and allowed user before expecting bot-message E2E.",
        ),
    };
    checks.push(runtime_check);

    let verdict = if checks.iter().any(|check| check["status"] == "block") {
        "block"
    } else if checks.iter().any(|check| check["status"] == "warn") {
        "warn"
    } else {
        "pass"
    };

    serde_json::json!({
        "verdict": verdict,
        "state": telegram_state_label(state),
        "tokenConfigured": status.token_configured,
        "allowedUserCount": status.allowed_user_count,
        "bindingStoreReadable": binding_store_readable,
        "bindingCount": binding_count,
        "runtime": runtime.map(|runtime| serde_json::json!({
            "startedAt": runtime.started_at,
            "lastPollAt": runtime.last_poll_at,
            "lastSuccessAt": runtime.last_success_at,
            "lastErrorAt": runtime.last_error_at,
            "lastError": runtime.last_error,
            "consecutiveFailures": runtime.consecutive_failures,
            "nextRetryMs": runtime.next_retry_ms,
            "updateOffset": runtime.update_offset,
            "activeChatCount": runtime.active_chat_count,
        })),
        "checks": checks,
        "notes": [
            "This test is secret-redacted and does not echo bot tokens or raw chat ids.",
            "A pass verdict proves daemon-side Telegram readiness; a full bot-message E2E still requires sending a message from an allowed Telegram account."
        ],
    })
}

fn telegram_state_label(state: &DaemonState) -> &'static str {
    let status = &state.telegram_status;
    if !status.token_configured {
        "not_configured"
    } else if status.allowed_user_count == 0 {
        "configured_without_allowlist"
    } else if status.long_poll_requested {
        "long_poll_requested"
    } else {
        "configured"
    }
}

fn check(id: &str, label: &str, status: &str, detail: &str, fix: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "label": label,
        "status": status,
        "detail": detail,
        "fix": fix,
    })
}
