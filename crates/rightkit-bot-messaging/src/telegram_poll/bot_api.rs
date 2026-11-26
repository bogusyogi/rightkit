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

    // Telegram rejects a message longer than 4096 UTF-16 units, so long replies go out in
    // chunks; the buttons ride on the last chunk so they sit under the full text.
    let chunks = split_telegram_text(&reply.text, TELEGRAM_MAX_MESSAGE_UTF16);
    let last = chunks.len().saturating_sub(1);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let mut body = serde_json::json!({
            "chat_id": reply.chat_id,
            "text": chunk,
        });
        if index == last && !reply_markup.is_null() {
            body["reply_markup"] = reply_markup.clone();
        }
        if let Err(e) = telegram_post(client, url.clone(), bot_token, &body).await {
            tracing::warn!(chat_id = %reply.chat_id, error = %e, "telegram sendMessage failed");
            return;
        }
    }
}

/// Telegram's `sendMessage` text limit, counted in UTF-16 code units.
const TELEGRAM_MAX_MESSAGE_UTF16: usize = 4096;

/// Split `text` into pieces of at most `max_utf16` UTF-16 units each, never inside a
/// character, preferring to break after a newline. Empty text yields one empty piece.
fn split_telegram_text(text: &str, max_utf16: usize) -> Vec<String> {
    let max_utf16 = max_utf16.max(2);
    let mut chunks = Vec::new();
    let mut rest = text;
    loop {
        let mut units = 0usize;
        let mut end = rest.len();
        let mut last_newline = None;
        for (offset, ch) in rest.char_indices() {
            if units + ch.len_utf16() > max_utf16 {
                end = offset;
                break;
            }
            units += ch.len_utf16();
            if ch == '\n' {
                last_newline = Some(offset + 1);
            }
        }
        if end == rest.len() {
            chunks.push(rest.to_string());
            return chunks;
        }
        // Break at a newline only when it keeps at least half a chunk.
        let cut = last_newline.filter(|cut| *cut >= end / 2).unwrap_or(end);
        chunks.push(rest[..cut].to_string());
        rest = &rest[cut..];
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
    let response = client.send_any(&request).await.map_err(|e| e.to_string())?;
    // `send_any` returns every status; a Bot API rejection (4xx/5xx) is a failure.
    if !(200..300).contains(&response.status) {
        return Err(format!("Telegram returned HTTP {}", response.status));
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------
