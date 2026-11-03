#[allow(clippy::too_many_arguments)]
async fn deliver_webhook_result(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    result_status: &str,
    target: &DeliveryTarget,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> Result<(), String> {
    let Some(url) = target
        .target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("target URL is required".to_string());
    };
    let payload = rightkit_bot_messaging::RemoteDeliveryPayload {
        kind: "scheduleResult".to_string(),
        source: "schedule".to_string(),
        delivery_kind: target.kind.clone(),
        schedule_id: Some(entry.id.clone()),
        session_id: Some(session_id.to_string()),
        status: Some(result_status.to_string()),
        content: delivery_message_content(source_artifact, fallback_content),
        artifact_path: source_artifact.map(|path| path.to_string_lossy().to_string()),
    };
    let remote_target = rightkit_bot_messaging::RemoteDeliveryTarget {
        channel: target.kind.clone(),
        target: url.to_string(),
        allow_private_network: target.allow_private_network,
    };
    let adapter = rightkit_bot_messaging::WebhookChannelAdapter::new();
    let record = match adapter
        .deliver(remote_target.clone(), payload.clone())
        .await
    {
        Ok(record) => record,
        Err(error) => {
            let record = adapter
                .record_delivery(remote_target, payload, "failed", Some(error.to_string()))
                .map_err(|e| e.to_string())?;
            record_sql_remote_delivery(state, &record).await;
            let _ = rightkit_bot_messaging::append_remote_delivery(&remote_deliveries_path(root), record);
            return Err(error.to_string());
        }
    };
    record_sql_remote_delivery(state, &record).await;
    rightkit_bot_messaging::append_remote_delivery(&remote_deliveries_path(root), record)
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn record_sql_remote_delivery(
    state: &DaemonState,
    record: &rightkit_bot_messaging::RemoteDeliveryRecord,
) {
    let Some(storage) = &state.storage else {
        return;
    };
    let payload_json = match serde_json::to_string(&record.payload) {
        Ok(value) => value,
        Err(e) => {
            warn!(record_id = %record.id, error = %e, "failed to encode remote delivery for SQL store");
            return;
        }
    };
    let row = rightkit_agent_engine_storage::RemoteDeliveryRow {
        id: record.id.clone(),
        channel_id: record.channel.clone(),
        binding_id: None,
        target_redacted: record.target_redacted.clone(),
        status: record.status.clone(),
        delivery_kind: record.payload.delivery_kind.clone(),
        payload_json,
        error: record.error.clone(),
        created_at: record.created_at.clone(),
    };
    let store = storage.lock().await;
    if let Err(e) = store.record_remote_delivery(&row) {
        warn!(record_id = %record.id, error = %e, "failed to record remote delivery in SQL store");
    }
}
