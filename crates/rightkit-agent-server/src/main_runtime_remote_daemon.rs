async fn cli_remote_smoke(
    timeout_seconds: u64,
    poll_interval_seconds: u64,
    require_chat: bool,
) -> i32 {
    let (base_url, token) = match resolve_daemon() {
        Some(p) => p,
        None => {
            print_json_value(serde_json::json!({
                "verdict": "block",
                "detail": "No running daemon found. Start one with `coderight serve` or the desktop app.",
            }));
            return 1;
        }
    };

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds.max(1));
    let poll_interval = std::time::Duration::from_secs(poll_interval_seconds.max(1));
    let mut last_error: Option<String> = None;
    let mut last_test_payload = serde_json::Value::Null;

    loop {
        match rightkit_http::http_client::daemon_get(
            &format!("{base_url}/remote/telegram/test"),
            &token,
        )
        .await
        {
            Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
                match resp.json::<serde_json::Value>() {
                    Ok(value) => {
                        let payload = telegram_live_smoke_payload(&value, require_chat, false);
                        let pass = payload["verdict"] == "pass";
                        last_test_payload = value;
                        if pass {
                            print_json_value(payload);
                            return 0;
                        }
                    }
                    Err(error) => {
                        last_error = Some(format!("Daemon returned non-JSON remote test: {error}"));
                    }
                }
            }
            Ok(resp) => {
                last_error = Some(format!("Daemon returned {} for remote smoke", resp.status));
            }
            Err(error) => {
                last_error = Some(format!("Failed to contact daemon: {error}"));
            }
        }

        if std::time::Instant::now() >= deadline {
            let mut payload = if last_test_payload.is_null() {
                serde_json::json!({
                    "verdict": "block",
                    "detail": last_error.clone().unwrap_or_else(|| "Timed out before any daemon remote test payload was available.".to_string()),
                })
            } else {
                telegram_live_smoke_payload(&last_test_payload, require_chat, true)
            };
            if let Some(error) = last_error {
                payload["lastError"] = serde_json::Value::String(error);
            }
            print_json_value(payload);
            return 1;
        }

        tokio::time::sleep(poll_interval).await;
    }
}

fn telegram_live_smoke_payload(
    test_payload: &serde_json::Value,
    require_chat: bool,
    timed_out: bool,
) -> serde_json::Value {
    let readiness = test_payload
        .get("verdict")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("block");
    let active_chat_count = test_payload
        .get("runtime")
        .and_then(|runtime| runtime.get("activeChatCount"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let telegram_token_configured = test_payload
        .get("tokenConfigured")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let allowed_user_count = test_payload
        .get("allowedUserCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let readiness_passed = readiness == "pass";
    let chat_passed = !require_chat || active_chat_count > 0;
    let verdict = if readiness_passed && chat_passed {
        "pass"
    } else if timed_out {
        "block"
    } else {
        "warn"
    };

    serde_json::json!({
        "verdict": verdict,
        "requireChat": require_chat,
        "readinessVerdict": readiness,
        "activeChatCount": active_chat_count,
        "telegramTokenConfigured": telegram_token_configured,
        "allowedUserCount": allowed_user_count,
        "bindingCount": test_payload.get("bindingCount").and_then(serde_json::Value::as_u64).unwrap_or(0),
        "runtime": test_payload.get("runtime").cloned().unwrap_or(serde_json::Value::Null),
        "detail": if readiness_passed && chat_passed {
            "Live Telegram daemon readiness is passing and an allowed chat has been observed."
        } else if !readiness_passed {
            "Telegram daemon readiness is not passing yet."
        } else {
            "Telegram long-poll readiness is passing, but no allowed Telegram chat has been observed yet."
        },
        "nextSteps": telegram_live_smoke_next_steps(
            readiness_passed,
            chat_passed,
            telegram_token_configured,
            allowed_user_count,
        ),
    })
}

fn telegram_live_smoke_next_steps(
    readiness_passed: bool,
    chat_passed: bool,
    telegram_token_configured: bool,
    allowed_user_count: u64,
) -> Vec<&'static str> {
    if readiness_passed && chat_passed {
        return vec![
            "Send a normal Telegram command/message to the bot to exercise the target session.",
            "Use `coderight remote status` for redacted cursor and binding details.",
        ];
    }

    if !telegram_token_configured {
        return vec![
            "Create a bot with Telegram BotFather.",
            "Save the bot token in Settings > Remote or run `coderight remote setup --telegram-token <token>`.",
            "Restart `coderight serve` or the desktop app, then rerun `coderight remote smoke --timeout-seconds 120`.",
        ];
    }

    if allowed_user_count == 0 {
        return vec![
            "Run `coderight remote validate` to verify Telegram accepts the saved BotFather token.",
            "Message the bot once from the Telegram account that should control RightKit.",
            "Run `coderight remote users`, then save the discovered user id with `coderight remote setup --allowed-user <id>`.",
            "Restart `coderight serve` or the desktop app, then rerun `coderight remote smoke --timeout-seconds 120`.",
        ];
    }

    if !readiness_passed {
        return vec![
            "Run `coderight remote test` to inspect the failing readiness check.",
            "Confirm the daemon/app was restarted after saving the BotFather token and allowed Telegram user ids.",
            "If Telegram reports a token or network error, rerun `coderight remote validate`.",
        ];
    }

    vec![
        "Send any message to the bot from an allowed Telegram account.",
        "Keep this command running until activeChatCount becomes non-zero.",
    ]
}

async fn daemon_remote_json(label: &str, path: &str) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!("No running daemon found. Start one with `coderight serve`.");
            return 1;
        }
    };

    match rightkit_http::http_client::daemon_get(&format!("{base_url}{path}"), &token).await {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
            let value = resp
                .json::<serde_json::Value>()
                .unwrap_or(serde_json::Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            0
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for remote {label}", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

fn print_json_value(value: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}
