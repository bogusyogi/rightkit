/// Send a reply to Telegram via `sendMessage`.
fn telegram_api_url(api_base_url: &str, bot_token: &str, method: &str) -> String {
    format!(
        "{}/bot{}/{}",
        api_base_url.trim_end_matches('/'),
        bot_token,
        method
    )
}

async fn send_reply(client: &Client, api_base_url: &str, bot_token: &str, reply: TelegramReply) {
    let url = telegram_api_url(api_base_url, bot_token, "sendMessage");

    // Build inline keyboard if buttons are present.
    let reply_markup = if reply.buttons.is_empty() {
        Value::Null
    } else {
        build_inline_keyboard(&reply.buttons)
    };

    let mut body = serde_json::json!({
        "chat_id": reply.chat_id,
        "text": reply.text,
    });

    if !reply_markup.is_null() {
        body["reply_markup"] = reply_markup;
    }

    if let Err(e) = telegram_post(client, url, bot_token, &body).await {
        tracing::warn!(chat_id = %reply.chat_id, error = %e, "telegram sendMessage failed");
    }
}

/// Answer a callback query (dismisses the Telegram spinner on the button).
async fn answer_callback(
    client: &Client,
    api_base_url: &str,
    bot_token: &str,
    callback_query_id: &str,
) {
    let url = telegram_api_url(api_base_url, bot_token, "answerCallbackQuery");
    let body = serde_json::json!({ "callback_query_id": callback_query_id });
    if let Err(e) = telegram_post(client, url, bot_token, &body).await {
        tracing::warn!(callback_id = %callback_query_id, error = %e, "telegram answerCallbackQuery failed");
    }
}

/// POST a Bot API method. The bot token is part of the URL path, so it is registered
/// with the request and never appears in the returned error text.
async fn telegram_post(
    client: &Client,
    url: String,
    bot_token: &str,
    body: &Value,
) -> Result<(), String> {
    let request = rightkit_http::Request::post(url)
        .redact_value(bot_token)
        .json(body)
        .map_err(|e| e.to_string())?;
    client
        .send_any(&request)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Build Telegram inline keyboard markup JSON from a list of `InlineButton`s.
/// Puts all buttons in a single row.
fn build_inline_keyboard(buttons: &[InlineButton]) -> Value {
    let row: Vec<Value> = buttons
        .iter()
        .map(|b| serde_json::json!({ "text": b.label, "callback_data": b.data }))
        .collect();
    serde_json::json!({ "inline_keyboard": [row] })
}

/// Parse the raw Telegram update JSON into a `TelegramUpdate`, also returning
/// the `update_id` and, for callback queries, the `callback_query_id`.
fn parse_update(update: &Value) -> Option<(u64, TelegramUpdate, Option<String>)> {
    let update_id = update.get("update_id")?.as_u64()?;

    // Plain message
    if let Some(msg) = update.get("message") {
        let user_id = msg.pointer("/from/id")?.as_u64()?;
        let chat_id = msg.pointer("/chat/id")?.as_i64()?;
        let text = msg.get("text")?.as_str()?.to_string();
        return Some((
            update_id,
            TelegramUpdate::Message {
                user_id,
                chat_id,
                text,
            },
            None,
        ));
    }

    // Callback query (inline button tap)
    if let Some(cq) = update.get("callback_query") {
        let cq_id = cq.get("id")?.as_str()?.to_string();
        let user_id = cq.pointer("/from/id")?.as_u64()?;
        let chat_id = cq.pointer("/message/chat/id")?.as_i64()?;
        let data = cq.get("data")?.as_str()?.to_string();
        return Some((
            update_id,
            TelegramUpdate::Callback {
                user_id,
                chat_id,
                data,
            },
            Some(cq_id),
        ));
    }

    None
}

pub(crate) fn telegram_binding_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("RIGHTKIT_TELEGRAM_BINDINGS") {
        return PathBuf::from(path);
    }
    if let Some(root) = rightkit_app_settings::agent_config::state_root() {
        return root.join("telegram-bindings.json");
    }
    PathBuf::from(".coderight").join("telegram-bindings.json")
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------
