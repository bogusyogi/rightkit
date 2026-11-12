async fn cli_remote(command: RemoteCommand) -> i32 {
    match command {
        RemoteCommand::Status => daemon_remote_status().await,
        RemoteCommand::Test => daemon_remote_test().await,
        RemoteCommand::Setup {
            telegram_token,
            allowed_users,
            clear_token,
            enable_web,
            disable_web,
        } => cli_remote_setup(
            telegram_token,
            allowed_users,
            clear_token,
            enable_web,
            disable_web,
        ),
        RemoteCommand::Validate {
            telegram_token,
            api_base,
        } => cli_remote_validate(telegram_token, &api_base).await,
        RemoteCommand::Users {
            telegram_token,
            api_base,
            limit,
        } => cli_remote_users(telegram_token, &api_base, limit).await,
        RemoteCommand::Smoke {
            timeout_seconds,
            poll_interval_seconds,
            no_require_chat,
        } => cli_remote_smoke(timeout_seconds, poll_interval_seconds, !no_require_chat).await,
    }
}

fn cli_remote_setup(
    telegram_token: Option<String>,
    allowed_users: Vec<u64>,
    clear_token: bool,
    enable_web: bool,
    disable_web: bool,
) -> i32 {
    let loader = rightkit_agent_config::ConfigLoader::new();
    // Read-modify-write under the shared config lock: a concurrent save is merged in.
    let next = match loader.update_user(|config| {
        let current = std::mem::take(config);
        *config = apply_remote_setup_to_config(
            current,
            telegram_token.as_deref(),
            &allowed_users,
            clear_token,
            enable_web,
            disable_web,
        );
    }) {
        Ok(next) => next,
        Err(e) => {
            eprintln!("Failed to save RightKit user config: {e}");
            return 1;
        }
    };
    let summary = remote_setup_summary_payload(&next);
    print_json_value(summary);
    0
}

fn remote_setup_summary_payload(cfg: &rightkit_agent_config::HostConfig) -> serde_json::Value {
    serde_json::json!({
        "saved": true,
        "configPath": user_config_path().map(|p| p.display().to_string()),
        "remote": {
            "telegramTokenConfigured": cfg.remote.telegram_token.is_some(),
            "telegramAllowedUserCount": cfg.remote.telegram_allowed_users.len(),
            "webEnabled": cfg.remote.web_enabled(),
            "webEnabledConfigured": cfg.remote.web_enabled,
        },
        "nextSteps": remote_setup_next_steps(cfg),
    })
}

fn remote_setup_next_steps(cfg: &rightkit_agent_config::HostConfig) -> Vec<&'static str> {
    if cfg.remote.telegram_token.is_none() {
        return vec![
            crate::host_identity::interned(format!("Create a bot with Telegram BotFather, then run `{app} remote setup --telegram-token <token>`.", app = crate::host_identity::app())),
            crate::host_identity::interned(format!("After saving a token, run `{app} remote validate` to verify Telegram accepts it.", app = crate::host_identity::app())),
        ];
    }

    if cfg.remote.telegram_allowed_users.is_empty() {
        return vec![
            crate::host_identity::interned(format!(
                "Run `{app} remote validate` to verify Telegram accepts the saved BotFather token.",
                app = crate::host_identity::app()
            )),
            "Message the bot once from the Telegram account that should control RightKit.",
            crate::host_identity::interned(format!(
                "Run `{app} remote users` to discover the numeric Telegram user id.",
                app = crate::host_identity::app()
            )),
            crate::host_identity::interned(format!(
                "Run `{app} remote setup --allowed-user <id>` with the discovered user id.",
                app = crate::host_identity::app()
            )),
        ];
    }

    vec![
        crate::host_identity::interned(format!("Run `{app} remote validate` to verify Telegram accepts the saved BotFather token.", app = crate::host_identity::app())),
        crate::host_identity::interned(format!("Restart `{app} serve` or the desktop app so Telegram long polling starts with the saved token and allowlist.", app = crate::host_identity::app())),
        crate::host_identity::interned(format!("Run `{app} remote test` after restart to inspect redacted daemon readiness.", app = crate::host_identity::app())),
        crate::host_identity::interned(format!("Run `{app} remote smoke --timeout-seconds 120`, then send any message to the bot from an allowed Telegram account.", app = crate::host_identity::app())),
    ]
}

fn apply_remote_setup_to_config(
    mut cfg: rightkit_agent_config::HostConfig,
    telegram_token: Option<&str>,
    allowed_users: &[u64],
    clear_token: bool,
    enable_web: bool,
    disable_web: bool,
) -> rightkit_agent_config::HostConfig {
    if clear_token {
        cfg.remote.telegram_token = None;
    } else if let Some(token) = telegram_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        cfg.remote.telegram_token = Some(token.to_string());
    }
    if !allowed_users.is_empty() {
        cfg.remote.telegram_allowed_users = allowed_users.to_vec();
    }
    if enable_web {
        cfg.remote.web_enabled = Some(true);
    } else if disable_web {
        cfg.remote.web_enabled = Some(false);
    }
    cfg
}

fn user_config_path() -> Option<std::path::PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join("config.toml"))
}

async fn cli_remote_validate(telegram_token: Option<String>, api_base: &str) -> i32 {
    let Some(token) = resolve_telegram_token_arg(telegram_token.as_deref()) else {
        print_json_value(serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": false,
            "detail": format!("No Telegram bot token configured. Run `{app} remote setup --telegram-token ...` or pass --telegram-token.", app = crate::host_identity::app()),
        }));
        return 1;
    };

    match fetch_telegram_get_me(api_base, &token).await {
        Ok(value) => {
            let payload = telegram_validate_payload(&value);
            let ok = payload["verdict"] == "pass";
            print_json_value(payload);
            if ok {
                0
            } else {
                1
            }
        }
        Err(error) => {
            print_json_value(serde_json::json!({
                "verdict": "block",
                "telegramTokenConfigured": true,
                "detail": "Telegram Bot API validation request failed.",
                "error": error,
            }));
            1
        }
    }
}

async fn cli_remote_users(telegram_token: Option<String>, api_base: &str, limit: u32) -> i32 {
    let Some(token) = resolve_telegram_token_arg(telegram_token.as_deref()) else {
        print_json_value(serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": false,
            "detail": format!("No Telegram bot token configured. Run `{app} remote setup --telegram-token ...` or pass --telegram-token.", app = crate::host_identity::app()),
        }));
        return 1;
    };

    match fetch_telegram_updates(api_base, &token, limit.clamp(1, 100)).await {
        Ok(value) => {
            let payload = telegram_users_payload(&value);
            let ok = payload["verdict"] != "block";
            print_json_value(payload);
            if ok {
                0
            } else {
                1
            }
        }
        Err(error) => {
            print_json_value(serde_json::json!({
                "verdict": "block",
                "telegramTokenConfigured": true,
                "detail": "Telegram getUpdates request failed.",
                "error": error,
            }));
            1
        }
    }
}

fn resolve_telegram_token_arg(telegram_token: Option<&str>) -> Option<String> {
    let configured = load_current_merged_config().remote.telegram_token;
    telegram_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .or_else(|| configured.filter(|token| !token.trim().is_empty()))
}

async fn fetch_telegram_get_me(api_base: &str, token: &str) -> Result<serde_json::Value, String> {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/bot{token}/getMe");
    let client =
        rightkit_http::http_client::endpoint_client(&url, std::time::Duration::from_secs(60))?;
    // The bot token is part of the URL path: register it so no error text echoes it.
    let response = rightkit_http::http_client::send_any(
        &client,
        &rightkit_http::Request::get(url).redact_value(token),
    )
    .await?;
    let status = response.status;
    let value = response
        .json::<serde_json::Value>()
        .map_err(|e| format!("Telegram returned {status}, but the response was not JSON: {e}"))?;
    Ok(value)
}

async fn fetch_telegram_updates(
    api_base: &str,
    token: &str,
    limit: u32,
) -> Result<serde_json::Value, String> {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/bot{token}/getUpdates?limit={limit}");
    let client =
        rightkit_http::http_client::endpoint_client(&url, std::time::Duration::from_secs(60))?;
    let response = rightkit_http::http_client::send_any(
        &client,
        &rightkit_http::Request::get(url).redact_value(token),
    )
    .await?;
    let status = response.status;
    let value = response
        .json::<serde_json::Value>()
        .map_err(|e| format!("Telegram returned {status}, but the response was not JSON: {e}"))?;
    Ok(value)
}

fn telegram_validate_payload(value: &serde_json::Value) -> serde_json::Value {
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !ok {
        return serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": true,
            "telegramAcceptedToken": false,
            "detail": value.get("description").and_then(serde_json::Value::as_str).unwrap_or("Telegram rejected the bot token."),
        });
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    serde_json::json!({
        "verdict": "pass",
        "telegramTokenConfigured": true,
        "telegramAcceptedToken": true,
        "bot": {
            "id": result.get("id").and_then(serde_json::Value::as_i64),
            "username": result.get("username").and_then(serde_json::Value::as_str),
            "firstName": result.get("first_name").and_then(serde_json::Value::as_str),
            "isBot": result.get("is_bot").and_then(serde_json::Value::as_bool),
        },
        "nextSteps": [
            format!("Restart `{app} serve` or the desktop app if the token was just saved.", app = crate::host_identity::app()),
            format!("Run `{app} remote test` to inspect daemon-side readiness.", app = crate::host_identity::app()),
            "Send a message to the bot from an allowed Telegram account for the final live smoke."
        ]
    })
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
        "nextSteps": [
            "Ask the intended user to send any message to the bot, then run this command again if no users are listed.",
            format!("Copy the desired userId into `{app} remote setup --allowed-user <id>`.", app = crate::host_identity::app()),
            format!("Restart the daemon/app and run `{app} remote test` before the final live message smoke.", app = crate::host_identity::app())
        ]
    })
}

async fn daemon_remote_status() -> i32 {
    daemon_remote_json("status", "/remote/telegram/status").await
}

async fn daemon_remote_test() -> i32 {
    daemon_remote_json("test", "/remote/telegram/test").await
}
