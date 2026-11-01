async fn post_remote_channel_outbound_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RemoteChannelOutboundRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(status) = enforce_trusted_source_authority(&headers) {
        return (
            status,
            Json(
                json!({"ok": false, "error": "lower-trust source cannot send remote outbound deliveries"}),
            ),
        );
    }
    let text = body.text.trim();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "text is required"})),
        );
    }
    if body.targets.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "at least one target is required"})),
        );
    }
    let Some(storage) = &state.storage else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "storage not available"})),
        );
    };
    let stored_channels = stored_remote_channels(&state).await;
    let stored_channel = stored_channels.iter().find(|row| row.id == id);
    let Some(mut descriptor) = base_remote_channel_descriptor(&state, &id, None) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "remote channel not found"})),
        );
    };
    if let Some(row) = stored_channel {
        apply_stored_channel_profile(&mut descriptor, row);
    }
    if !descriptor.enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": "remote channel is disabled"})),
        );
    }
    if descriptor.kind != "webhook" {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"ok": false, "error": "remote channel does not support outbound delivery"}),
            ),
        );
    }

    let source = body
        .source
        .as_deref()
        .map(clean_remote_source_label)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "remote-channel-outbound".to_string());
    let delivery_kind = body
        .delivery_kind
        .as_deref()
        .map(clean_remote_source_label)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("outbound-{id}"));
    let session_id = body
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from);
    let adapter = rightkit_bot_messaging::WebhookChannelAdapter::new();
    let mut results = Vec::new();
    let mut delivered = 0usize;
    let mut failed = 0usize;
    for target in body.targets {
        let payload = rightkit_bot_messaging::RemoteDeliveryPayload {
            kind: "remoteChannelOutbound".to_string(),
            source: source.clone(),
            delivery_kind: delivery_kind.clone(),
            schedule_id: None,
            session_id: session_id.clone(),
            status: None,
            content: text.to_string(),
            artifact_path: None,
        };
        let delivery_target = rightkit_bot_messaging::RemoteDeliveryTarget {
            channel: id.clone(),
            target: target.target,
            allow_private_network: target.allow_private_network,
        };
        let record = match adapter
            .deliver(delivery_target.clone(), payload.clone())
            .await
        {
            Ok(record) => {
                delivered += 1;
                record
            }
            Err(error) => {
                failed += 1;
                adapter
                    .record_delivery(delivery_target, payload, "failed", Some(error.to_string()))
                    .unwrap_or_else(|_| rightkit_bot_messaging::RemoteDeliveryRecord {
                        id: format!("remote-delivery-{}", status_timestamp()),
                        channel: id.clone(),
                        target_redacted: "<invalid-url>".to_string(),
                        status: "failed".to_string(),
                        error: Some(error.to_string()),
                        payload: rightkit_bot_messaging::RemoteDeliveryPayload {
                            kind: "remoteChannelOutbound".to_string(),
                            source: source.clone(),
                            delivery_kind: delivery_kind.clone(),
                            schedule_id: None,
                            session_id: session_id.clone(),
                            status: None,
                            content: text.to_string(),
                            artifact_path: None,
                        },
                        created_at: status_timestamp(),
                    })
            }
        };
        record_remote_outbound_delivery(storage, &record).await;
        results.push(json!({
            "id": record.id,
            "targetRedacted": record.target_redacted,
            "status": record.status,
            "deliveryKind": record.payload.delivery_kind,
            "error": record.error,
            "createdAt": record.created_at,
        }));
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": failed == 0,
            "channelId": id,
            "attempted": delivered + failed,
            "delivered": delivered,
            "failed": failed,
            "results": results,
        })),
    )
}

async fn post_remote_channel_fanout_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RemoteChannelFanoutRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(status) = enforce_trusted_source_authority(&headers) {
        return (
            status,
            Json(
                json!({"ok": false, "error": "lower-trust source cannot send remote outbound deliveries"}),
            ),
        );
    }
    let text = body.text.trim();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "text is required"})),
        );
    }
    let Some(storage) = &state.storage else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "storage not available"})),
        );
    };
    let stored_channels = stored_remote_channels(&state).await;
    let stored_channel = stored_channels.iter().find(|row| row.id == id);
    let Some(mut descriptor) = base_remote_channel_descriptor(&state, &id, None) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "remote channel not found"})),
        );
    };
    if let Some(row) = stored_channel {
        apply_stored_channel_profile(&mut descriptor, row);
    }
    if !descriptor.enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": "remote channel is disabled"})),
        );
    }

    let bindings = {
        let store = storage.lock().await;
        match store.list_remote_bindings_for_channel(&id) {
            Ok(rows) => rows,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": e.to_string()})),
                )
            }
        }
    };
    let requested_target_key = body
        .target_key
        .as_deref()
        .map(|target| opaque_remote_target_key(&id, target));
    let requested_session_id = body
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let active_bindings = bindings
        .into_iter()
        .filter(|row| {
            row.status == "active"
                && row.session_id.is_some()
                && requested_target_key
                    .as_deref()
                    .map(|target| row.target_key == target)
                    .unwrap_or(true)
                && requested_session_id
                    .map(|session_id| row.session_id.as_deref() == Some(session_id))
                    .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    let mut results = Vec::new();
    let mut delivered = 0usize;
    let mut failed = 0usize;
    for binding in &active_bindings {
        let session_id = binding.session_id.as_deref().unwrap_or_default();
        let target_redacted = binding
            .target_redacted
            .clone()
            .or_else(|| binding.target_label.clone())
            .unwrap_or_else(|| redacted_remote_target(&id, &binding.target_key));
        let source = body.source.as_deref().map(clean_remote_source_label);
        let submit = state
            .submit_remote_channel_text(
                session_id,
                &id,
                text,
                source.as_deref(),
                Some(&target_redacted),
            )
            .await;
        let (status, seq, error) = match submit {
            Ok(seq) => {
                delivered += 1;
                ("delivered".to_string(), Some(seq), None)
            }
            Err(e) => {
                failed += 1;
                ("failed".to_string(), None, Some(e))
            }
        };
        let result = json!({
            "bindingId": binding.id,
            "sessionId": session_id,
            "targetRedacted": target_redacted,
            "status": status,
            "seq": seq,
            "error": error,
        });
        record_remote_fanout_delivery(
            storage,
            &id,
            binding,
            &target_redacted,
            text,
            &status,
            error.as_deref(),
        )
        .await;
        results.push(result);
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": failed == 0,
            "channelId": id,
            "attempted": active_bindings.len(),
            "delivered": delivered,
            "failed": failed,
            "results": results,
        })),
    )
}

async fn record_remote_outbound_delivery(
    storage: &crate::api::SharedStorage,
    record: &rightkit_bot_messaging::RemoteDeliveryRecord,
) {
    let row = rightkit_agent_engine_storage::RemoteDeliveryRow {
        id: record.id.clone(),
        channel_id: record.channel.clone(),
        binding_id: None,
        target_redacted: record.target_redacted.clone(),
        status: record.status.clone(),
        delivery_kind: record.payload.delivery_kind.clone(),
        payload_json: serde_json::to_string(&record.payload).unwrap_or_else(|_| "{}".to_string()),
        error: record.error.clone(),
        created_at: record.created_at.clone(),
    };
    let store = storage.lock().await;
    if let Err(e) = store.record_remote_delivery(&row) {
        eprintln!(
            "[remote] failed to record outbound delivery {}: {e}",
            row.id
        );
    }
}

async fn record_remote_fanout_delivery(
    storage: &crate::api::SharedStorage,
    channel_id: &str,
    binding: &rightkit_agent_engine_storage::RemoteBindingRow,
    target_redacted: &str,
    text: &str,
    status: &str,
    error: Option<&str>,
) {
    let payload = rightkit_bot_messaging::RemoteDeliveryPayload {
        kind: "remoteChannelFanout".to_string(),
        source: "remote-channel".to_string(),
        delivery_kind: "inbound_session_message".to_string(),
        schedule_id: None,
        session_id: binding.session_id.clone(),
        status: Some(status.to_string()),
        content: text.to_string(),
        artifact_path: None,
    };
    let row = rightkit_agent_engine_storage::RemoteDeliveryRow {
        id: format!(
            "remote-fanout-{}-{}-{}",
            channel_id,
            binding.id,
            status_timestamp()
        ),
        channel_id: channel_id.to_string(),
        binding_id: Some(binding.id.clone()),
        target_redacted: target_redacted.to_string(),
        status: status.to_string(),
        delivery_kind: "inbound_session_message".to_string(),
        payload_json: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        error: error.map(str::to_string),
        created_at: status_timestamp(),
    };
    let store = storage.lock().await;
    if let Err(e) = store.record_remote_delivery(&row) {
        eprintln!("[remote] failed to record fanout delivery {}: {e}", row.id);
    }
}
