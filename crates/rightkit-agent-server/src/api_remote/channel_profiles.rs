fn apply_recent_deliveries(
    channel: &mut rightkit_bot_messaging::RemoteChannelDescriptor,
    deliveries: &std::collections::BTreeMap<
        String,
        Vec<rightkit_bot_messaging::RemoteChannelDeliverySummary>,
    >,
) {
    channel.recent_deliveries = deliveries.get(&channel.id).cloned().unwrap_or_default();
}

fn base_remote_channel_descriptor(
    state: &DaemonState,
    id: &str,
    sql_telegram_binding_count: Option<usize>,
) -> Option<rightkit_bot_messaging::RemoteChannelDescriptor> {
    match id {
        "telegram" => {
            let status = &state.telegram_status;
            let runtime = status.runtime.lock().ok().map(|guard| guard.clone());
            let json_binding_count = match rightkit_bot_messaging::load_telegram_session_bindings(
                &status.binding_store_path,
            ) {
                Ok(bindings) => bindings.chats.len(),
                Err(_) => 0,
            };
            let binding_count = sql_telegram_binding_count
                .filter(|count| *count > 0)
                .unwrap_or(json_binding_count);
            Some(rightkit_bot_messaging::telegram_channel_descriptor(
                rightkit_bot_messaging::TelegramChannelDescriptorInput {
                    configured: status.token_configured,
                    allowed_user_count: status.allowed_user_count,
                    long_poll_requested: status.long_poll_requested,
                    binding_count,
                    active_target_count: runtime
                        .as_ref()
                        .map(|runtime| runtime.active_chat_count)
                        .unwrap_or(0),
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
            ))
        }
        "webhook" | "ntfy" => Some(rightkit_bot_messaging::webhook_channel_descriptor(id)),
        _ => None,
    }
}

fn apply_profile_request(
    channel: &mut rightkit_bot_messaging::RemoteChannelDescriptor,
    body: &RemoteChannelProfileRequest,
) {
    if let Some(profile_id) = clean_profile_value(&body.profile_id) {
        channel.profile.profile_id = profile_id;
    } else {
        channel.profile.profile_id = channel.id.clone();
    }
    if let Some(toolset) = clean_profile_value(&body.toolset) {
        channel.profile.toolset = toolset;
    }
    if let Some(delivery_policy) = clean_profile_value(&body.delivery_policy) {
        channel.profile.delivery_policy = delivery_policy;
    }
    if let Some(trust) = clean_profile_value(&body.trust) {
        channel.profile.trust = trust;
    }
    if let Some(enabled) = body.enabled {
        channel.enabled = enabled;
        if !enabled {
            channel.status.state = "disabled".to_string();
        }
    }
}

fn clean_profile_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn remote_channel_row_from_descriptor(
    channel: &rightkit_bot_messaging::RemoteChannelDescriptor,
    updated_at: String,
) -> rightkit_agent_engine_storage::RemoteChannelRow {
    rightkit_agent_engine_storage::RemoteChannelRow {
        id: channel.id.clone(),
        kind: channel.kind.clone(),
        profile_id: channel.profile.profile_id.clone(),
        enabled: channel.enabled,
        configured: channel.configured,
        toolset: channel.profile.toolset.clone(),
        delivery_policy: channel.profile.delivery_policy.clone(),
        trust: channel.profile.trust.clone(),
        state: channel.status.state.clone(),
        last_success_at: channel.status.last_success_at.clone(),
        last_error_at: channel.status.last_error_at.clone(),
        last_error: channel.status.last_error.clone(),
        updated_at,
    }
}

fn apply_stored_channel_profile(
    channel: &mut rightkit_bot_messaging::RemoteChannelDescriptor,
    row: &rightkit_agent_engine_storage::RemoteChannelRow,
) {
    channel.kind = row.kind.clone();
    channel.enabled = row.enabled;
    channel.configured = row.configured;
    channel.profile.profile_id = row.profile_id.clone();
    channel.profile.toolset = row.toolset.clone();
    channel.profile.delivery_policy = row.delivery_policy.clone();
    channel.profile.trust = row.trust.clone();
    channel.status.state = row.state.clone();
    channel.status.last_success_at = row.last_success_at.clone();
    channel.status.last_error_at = row.last_error_at.clone();
    channel.status.last_error = row.last_error.clone();
    channel
        .notes
        .push("Profile policy is backed by a remote channel SQL override.".to_string());
}

fn profiles_from_channels(
    channels: &[rightkit_bot_messaging::RemoteChannelDescriptor],
) -> Vec<rightkit_bot_messaging::RemoteGatewayProfile> {
    let mut profiles =
        std::collections::BTreeMap::<String, rightkit_bot_messaging::RemoteGatewayProfile>::new();
    for channel in channels {
        let entry = profiles
            .entry(channel.profile.profile_id.clone())
            .or_insert_with(|| rightkit_bot_messaging::RemoteGatewayProfile {
                id: channel.profile.profile_id.clone(),
                channels: Vec::new(),
                toolset: channel.profile.toolset.clone(),
                delivery_policy: channel.profile.delivery_policy.clone(),
            });
        if !entry.channels.iter().any(|id| id == &channel.id) {
            entry.channels.push(channel.id.clone());
        }
        entry.toolset = channel.profile.toolset.clone();
        entry.delivery_policy = channel.profile.delivery_policy.clone();
    }
    profiles.into_values().collect()
}

fn apply_configured_channel_profile(
    channel: &mut rightkit_bot_messaging::RemoteChannelDescriptor,
    config: &rightkit_app_settings::agent_config::HostConfig,
) {
    let Some(profile) = config.channel_profiles.get(&channel.id) else {
        return;
    };
    channel.profile.profile_id = channel.id.clone();
    if !profile.toolset.trim().is_empty() {
        channel.profile.toolset = profile.toolset.trim().to_string();
    }
    if !profile.delivery_policy.trim().is_empty() {
        channel.profile.delivery_policy = profile.delivery_policy.trim().to_string();
    }
    if !profile.trust.trim().is_empty() {
        channel.profile.trust = profile.trust.trim().to_string();
    }
}
