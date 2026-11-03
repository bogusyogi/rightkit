fn clean_remote_source_label(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(48)
        .collect::<String>()
}

fn opaque_remote_target_key(channel_id: &str, target_key: &str) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(channel_id.as_bytes());
    hasher.update(b":");
    hasher.update(target_key.as_bytes());
    format!("sha256:{}", hasher.finalize())
}

fn redacted_remote_target(channel_id: &str, opaque_target_key: &str) -> String {
    let suffix = opaque_target_key
        .strip_prefix("sha256:")
        .unwrap_or(opaque_target_key)
        .chars()
        .take(12)
        .collect::<String>();
    format!("{channel_id}:target-{suffix}")
}

fn status_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

fn resolve_configured_telegram_token(
    env_vars: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let loader = rightkit_app_settings::agent_config::ConfigLoader::new();
    let config = loader.apply_env(loader.load_files_default(), env_vars);
    config
        .remote
        .telegram_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

async fn telegram_sql_binding_count(state: &DaemonState) -> Option<usize> {
    let storage = state.storage.as_ref()?;
    let store = storage.lock().await;
    store
        .list_remote_bindings_for_channel("telegram")
        .map(|rows| rows.len())
        .ok()
}

fn telegram_status_payload(
    state: &DaemonState,
    sql_binding_count: Option<usize>,
) -> serde_json::Value {
    let status = &state.telegram_status;
    let runtime = status.runtime.lock().ok().map(|guard| guard.clone());
    let bindings =
        rightkit_bot_messaging::load_telegram_session_bindings(&status.binding_store_path);
    let (json_binding_count, binding_store_readable, binding_store_error) = match bindings {
        Ok(bindings) => (bindings.chats.len(), true, serde_json::Value::Null),
        Err(error) => (0, false, serde_json::Value::String(error)),
    };
    let binding_count = sql_binding_count
        .filter(|count| *count > 0)
        .unwrap_or(json_binding_count);
    serde_json::json!({
        "state": telegram_state_label(state),
        "tokenConfigured": status.token_configured,
        "allowedUserCount": status.allowed_user_count,
        "longPollRequested": status.long_poll_requested,
        "bindingStorePath": status.binding_store_path,
        "bindingStoreReadable": binding_store_readable,
        "bindingCount": binding_count,
        "bindingStoreError": binding_store_error,
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
            "chatCursors": runtime.chat_cursors.into_iter().map(|row| serde_json::json!({
                "chat": row.chat_label,
                "sessionId": row.session_id,
                "lastPushedSeq": row.last_pushed_seq,
            })).collect::<Vec<_>>(),
        })),
        "notes": [
            "Bot token value and chat ids are intentionally redacted.",
            "Chat ids are represented as redacted chat-1/chat-2 labels."
        ],
    })
}

async fn remote_binding_count(state: &DaemonState, channel_id: &str) -> Option<usize> {
    let storage = state.storage.as_ref()?;
    let store = storage.lock().await;
    store
        .list_remote_bindings_for_channel(channel_id)
        .map(|rows| rows.len())
        .ok()
}

async fn stored_remote_channels(
    state: &DaemonState,
) -> Vec<rightkit_agent_engine_storage::RemoteChannelRow> {
    let Some(storage) = &state.storage else {
        return Vec::new();
    };
    let store = storage.lock().await;
    store.list_remote_channels().unwrap_or_default()
}

async fn recent_remote_channel_deliveries(
    state: &DaemonState,
    channel_ids: &[&str],
) -> std::collections::BTreeMap<String, Vec<rightkit_bot_messaging::RemoteChannelDeliverySummary>> {
    let Some(storage) = &state.storage else {
        return std::collections::BTreeMap::new();
    };
    let store = storage.lock().await;
    let mut deliveries = std::collections::BTreeMap::new();
    for channel_id in channel_ids {
        let rows = store
            .list_remote_deliveries_for_channel(channel_id, 5)
            .unwrap_or_default()
            .into_iter()
            .map(remote_delivery_summary)
            .collect::<Vec<_>>();
        deliveries.insert((*channel_id).to_string(), rows);
    }
    deliveries
}

fn remote_delivery_summary(
    row: rightkit_agent_engine_storage::RemoteDeliveryRow,
) -> rightkit_bot_messaging::RemoteChannelDeliverySummary {
    rightkit_bot_messaging::RemoteChannelDeliverySummary {
        id: row.id,
        binding_id: row.binding_id,
        target_redacted: row.target_redacted,
        status: row.status,
        delivery_kind: row.delivery_kind,
        error: row.error,
        created_at: row.created_at,
    }
}

fn remote_channels_payload(
    state: &DaemonState,
    sql_telegram_binding_count: Option<usize>,
    stored_channels: &[rightkit_agent_engine_storage::RemoteChannelRow],
    recent_deliveries: &std::collections::BTreeMap<
        String,
        Vec<rightkit_bot_messaging::RemoteChannelDeliverySummary>,
    >,
) -> serde_json::Value {
    let config = rightkit_app_settings::agent_config::ConfigLoader::new().load_files_default();
    let mut stored_by_id = stored_channels
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect::<std::collections::BTreeMap<_, _>>();
    let status = &state.telegram_status;
    let runtime = status.runtime.lock().ok().map(|guard| guard.clone());
    let json_binding_count =
        match rightkit_bot_messaging::load_telegram_session_bindings(&status.binding_store_path) {
            Ok(bindings) => bindings.chats.len(),
            Err(_) => 0,
        };
    let binding_count = sql_telegram_binding_count
        .filter(|count| *count > 0)
        .unwrap_or(json_binding_count);
    let active_target_count = runtime
        .as_ref()
        .map(|runtime| runtime.active_chat_count)
        .unwrap_or(0);
    let mut channel = rightkit_bot_messaging::telegram_channel_descriptor(
        rightkit_bot_messaging::TelegramChannelDescriptorInput {
            configured: status.token_configured,
            allowed_user_count: status.allowed_user_count,
            long_poll_requested: status.long_poll_requested,
            binding_count,
            active_target_count,
            last_success_at: runtime
                .as_ref()
                .and_then(|runtime| runtime.last_success_at.clone()),
            last_error_at: runtime
                .as_ref()
                .and_then(|runtime| runtime.last_error_at.clone()),
            last_error: runtime
                .as_ref()
                .and_then(|runtime| runtime.last_error.clone()),
        },
    );
    apply_configured_channel_profile(&mut channel, &config);
    if let Some(row) = stored_by_id.remove(channel.id.as_str()) {
        apply_stored_channel_profile(&mut channel, row);
    }
    apply_recent_deliveries(&mut channel, recent_deliveries);
    let mut webhook = rightkit_bot_messaging::webhook_channel_descriptor("webhook");
    apply_configured_channel_profile(&mut webhook, &config);
    if let Some(row) = stored_by_id.remove(webhook.id.as_str()) {
        apply_stored_channel_profile(&mut webhook, row);
    }
    apply_recent_deliveries(&mut webhook, recent_deliveries);
    let mut ntfy = rightkit_bot_messaging::webhook_channel_descriptor("ntfy");
    apply_configured_channel_profile(&mut ntfy, &config);
    if let Some(row) = stored_by_id.remove(ntfy.id.as_str()) {
        apply_stored_channel_profile(&mut ntfy, row);
    }
    apply_recent_deliveries(&mut ntfy, recent_deliveries);
    let channels = vec![channel, webhook, ntfy];
    let snapshot = rightkit_bot_messaging::RemoteChannelsSnapshot {
        profiles: profiles_from_channels(&channels),
        channels,
    };
    serde_json::to_value(snapshot).unwrap()
}
