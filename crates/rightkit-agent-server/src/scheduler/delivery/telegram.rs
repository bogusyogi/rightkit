async fn deliver_telegram_chat_result(
    entry: &ScheduleEntry,
    session_id: &str,
    result_status: &str,
    target: &DeliveryTarget,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> Result<(), String> {
    let chat_id = telegram_chat_id(target.target.as_deref())?;
    let token = resolve_configured_telegram_token()
        .ok_or_else(|| "Telegram bot token is not configured".to_string())?;
    let api_base = telegram_api_base_url()?;
    let text = telegram_schedule_message(
        entry,
        session_id,
        result_status,
        source_artifact,
        fallback_content,
    );
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "disable_web_page_preview": true,
    });
    let url = format!(
        "{}/bot{}/sendMessage",
        api_base.as_str().trim_end_matches('/'),
        token
    );
    let client = rightkit_agent_daemon::http_client::endpoint_client(&url, Duration::from_secs(10))
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    // The bot token is part of the URL path: register it so no error text echoes it.
    let request = rightkit_http::Request::post(url)
        .redact_value(token)
        .json(&body)
        .map_err(|e| format!("Telegram sendMessage failed: {e}"))?;
    let response = client
        .send_any(&request)
        .await
        .map_err(|e| format!("Telegram sendMessage failed: {e}"))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "Telegram sendMessage returned HTTP {}",
            response.status
        ));
    }
    Ok(())
}

pub(super) fn telegram_chat_id(target: Option<&str>) -> Result<i64, String> {
    let raw = target
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "target chat id is required".to_string())?;
    raw.parse::<i64>()
        .map_err(|_| "target chat id must be a numeric Telegram chat id".to_string())
}

fn resolve_configured_telegram_token() -> Option<String> {
    let env_vars = std::env::vars().collect::<HashMap<_, _>>();
    let loader = rightkit_agent_config::ConfigLoader::new();
    let config = loader.apply_env(loader.load_files_default(), &env_vars);
    config
        .remote
        .telegram_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn telegram_api_base_url() -> Result<url::Url, String> {
    let raw = std::env::var("RIGHTKIT_TELEGRAM_API_BASE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://api.telegram.org".to_string());
    let url =
        url::Url::parse(raw.trim()).map_err(|e| format!("invalid Telegram API base URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "unsupported Telegram API base URL scheme '{other}'"
        )),
    }
}

fn telegram_schedule_message(
    entry: &ScheduleEntry,
    session_id: &str,
    result_status: &str,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> String {
    let artifact = source_artifact
        .map(|path| format!("\nArtifact: {}", path.display()))
        .unwrap_or_default();
    truncate_telegram_text(&format!(
        "RightKit schedule `{}` finished with `{}`.\nSession: `{}`{}\n\n{}",
        entry.id,
        result_status,
        session_id,
        artifact,
        delivery_message_content(source_artifact, fallback_content)
    ))
}

pub(super) fn truncate_telegram_text(text: &str) -> String {
    const MAX_TELEGRAM_TEXT_CHARS: usize = 4096;
    if text.chars().count() <= MAX_TELEGRAM_TEXT_CHARS {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(MAX_TELEGRAM_TEXT_CHARS.saturating_sub(32))
        .collect::<String>();
    truncated.push_str("\n...[truncated by RightKit]");
    truncated
}
