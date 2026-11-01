async fn get_remote_channels_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let sql_telegram_binding_count = remote_binding_count(&state, "telegram").await;
    let stored_channels = stored_remote_channels(&state).await;
    let recent_deliveries =
        recent_remote_channel_deliveries(&state, &["telegram", "webhook", "ntfy"]).await;
    Json(remote_channels_payload(
        &state,
        sql_telegram_binding_count,
        &stored_channels,
        &recent_deliveries,
    ))
}

async fn get_telegram_status_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let sql_count = telegram_sql_binding_count(&state).await;
    Json(telegram_status_payload(&state, sql_count))
}

async fn get_telegram_test_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let sql_count = telegram_sql_binding_count(&state).await;
    Json(telegram_test_payload(&state, sql_count))
}

async fn get_telegram_users_handler(
    Query(query): Query<TelegramUsersQuery>,
) -> Json<serde_json::Value> {
    let env_vars = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    let Some(token) = resolve_configured_telegram_token(&env_vars) else {
        return Json(serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": false,
            "detail": "No Telegram bot token is configured. Save the BotFather token first or set RIGHTKIT_TELEGRAM_TOKEN.",
            "users": [],
            "userCount": 0,
        }));
    };

    match fetch_telegram_updates(
        "https://api.telegram.org",
        &token,
        query.limit.unwrap_or(20).clamp(1, 100),
    )
    .await
    {
        Ok(value) => Json(telegram_users_payload(&value)),
        Err(error) => Json(serde_json::json!({
            "verdict": "block",
            "telegramTokenConfigured": true,
            "detail": "Telegram getUpdates request failed.",
            "error": error,
            "users": [],
            "userCount": 0,
        })),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RemoteBindingRequest {
    channel_id: String,
    target_key: String,
    session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RemoteChannelProfileRequest {
    profile_id: Option<String>,
    toolset: Option<String>,
    delivery_policy: Option<String>,
    trust: Option<String>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RemoteChannelFanoutRequest {
    text: String,
    source: Option<String>,
    target_key: Option<String>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RemoteChannelOutboundTarget {
    target: String,
    #[serde(default)]
    allow_private_network: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RemoteChannelOutboundRequest {
    text: String,
    source: Option<String>,
    delivery_kind: Option<String>,
    session_id: Option<String>,
    targets: Vec<RemoteChannelOutboundTarget>,
}

async fn post_remote_binding_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<RemoteBindingRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(status) = enforce_trusted_source_authority(&headers) {
        return (
            status,
            Json(json!({"ok": false, "error": "lower-trust source cannot mutate remote bindings"})),
        );
    }
    let Some(storage) = &state.storage else {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "error": "storage not available"})),
        );
    };
    let now = status_timestamp();
    let opaque_target_key = opaque_remote_target_key(&body.channel_id, &body.target_key);
    let binding_id = format!("{}-{}", body.channel_id, opaque_target_key);
    let target_redacted = Some(redacted_remote_target(&body.channel_id, &opaque_target_key));
    let row = rightkit_agent_engine_storage::RemoteBindingRow {
        id: binding_id,
        channel_id: body.channel_id,
        target_key: opaque_target_key,
        session_id: Some(body.session_id),
        target_label: target_redacted.clone(),
        target_redacted,
        status: "active".to_string(),
        created_at: now.clone(),
        updated_at: now,
    };
    let store = storage.lock().await;
    match store.upsert_remote_binding(&row) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (
            StatusCode::OK,
            Json(json!({"ok": false, "error": e.to_string()})),
        ),
    }
}

async fn post_remote_channel_profile_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RemoteChannelProfileRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(status) = enforce_trusted_source_authority(&headers) {
        return (
            status,
            Json(
                json!({"ok": false, "error": "lower-trust source cannot mutate remote channel profiles"}),
            ),
        );
    }
    let Some(storage) = &state.storage else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "storage not available"})),
        );
    };
    let Some(mut descriptor) = base_remote_channel_descriptor(&state, &id, None) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "remote channel not found"})),
        );
    };
    apply_profile_request(&mut descriptor, &body);
    let row = remote_channel_row_from_descriptor(&descriptor, status_timestamp());
    let store = storage.lock().await;
    match store.upsert_remote_channel(&row) {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "channel": descriptor})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        ),
    }
}
