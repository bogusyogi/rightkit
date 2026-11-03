async fn list_session_tool_invocations_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ToolInvocationRecord>>, StatusCode> {
    if let Some(records) = storage_records_for_session(&state, &id).await {
        if !records.is_empty() {
            return Ok(Json(records));
        }
    }
    let events = session_events(&state, &id).await?;
    Ok(Json(records_from_events(&state, events)))
}

async fn get_tool_invocation_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<ToolInvocationRecord>, StatusCode> {
    if let Some(record) = storage_record_by_id(&state, &id).await {
        return Ok(Json(record));
    }
    let Some(record) = find_invocation(&state, &id)
        .await?
        .map(InvocationBuilder::into_record)
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(record))
}

async fn get_tool_invocation_events_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ToolInvocationEventRow>>, StatusCode> {
    if let Some(events) = storage_events_by_invocation(&state, &id).await {
        if !events.is_empty() {
            return Ok(Json(events));
        }
    }
    let Some(record) = find_invocation(&state, &id).await? else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(record.events))
}

async fn cancel_tool_invocation_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CancelToolInvocationBody>,
) -> Result<Json<CancelToolInvocationResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let record = if let Some(record) = storage_record_by_id(&state, &id).await {
        record
    } else {
        let Some(record) = find_invocation(&state, &id).await? else {
            return Err(StatusCode::NOT_FOUND);
        };
        record.into_record()
    };
    let live = state
        .get_session(&record.session_id)
        .ok_or(StatusCode::CONFLICT)?;
    let sender = lock_recover(&live.pending_approvals, "live.pending_approvals").remove(&id);
    let cancel_token = lock_recover(&live.cancel_tokens, "live.cancel_tokens").remove(&id);
    let cancelled_questions = if record.tool_name == "ask_user" {
        let mut pending = lock_recover(&live.pending_questions, "live.pending_questions");
        let count = pending.len();
        pending.clear();
        count
    } else {
        0
    };

    // If neither a pending approval nor an in-flight cancel token exists, the
    // invocation cannot be cancelled. A persisted requested `ask_user` is
    // cancellable after session rehydration even when its original oneshot no
    // longer exists; settling that ledger row prevents a permanent request.
    if sender.is_none()
        && cancel_token.is_none()
        && cancelled_questions == 0
        && !(record.tool_name == "ask_user" && record.lifecycle_state == "requested")
    {
        return Err(StatusCode::CONFLICT);
    }

    state
        .tool_invocation_cancellations
        .lock()
        .unwrap()
        .push(ToolInvocationCancellation {
            invocation_id: id.clone(),
            session_id: record.session_id.clone(),
            reason: body.reason.clone(),
            created_at: crate::api::now_iso(),
        });
    if let Some(storage) = &state.storage {
        let cancellation = rightkit_agent_engine_storage::ToolInvocationCancellationRow {
            invocation_id: id.clone(),
            session_id: record.session_id.clone(),
            reason: body.reason.clone(),
            created_at: crate::api::now_iso(),
        };
        let storage = storage.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let store = storage.blocking_lock();
            if let Err(e) = store.upsert_tool_invocation_cancellation(&cancellation) {
                eprintln!("[tool-invocations] failed to persist cancellation: {e}");
            }
            if let Some(mut row) = store
                .get_tool_invocation(&cancellation.invocation_id)
                .ok()
                .flatten()
            {
                row.lifecycle_state = "cancelled".to_string();
                row.updated_at = cancellation.created_at.clone();
                let _ = store.upsert_tool_invocation(&row);
            }
            let _ = store.append_tool_invocation_event(
                &rightkit_agent_engine_storage::ToolInvocationEventStoreRow {
                    invocation_id: cancellation.invocation_id.clone(),
                    session_id: cancellation.session_id.clone(),
                    seq: i64::MAX,
                    kind: "cancelRequested".to_string(),
                    created_at: cancellation.created_at.clone(),
                    payload_redacted_json: serde_json::json!({
                        "toolCallId": cancellation.invocation_id,
                        "reason": cancellation.reason,
                    })
                    .to_string(),
                },
            );
        })
        .await;
    }
    if let Some(tx) = sender {
        let _ = tx.send(false);
    }
    if let Some(token) = cancel_token {
        let _ = token.send(true);
    }
    Ok(Json(CancelToolInvocationResponse {
        cancelled: true,
        tool_invocation_id: id,
        session_id: record.session_id,
        reason: body.reason,
    }))
}

async fn find_invocation(
    state: &DaemonState,
    invocation_id: &str,
) -> Result<Option<InvocationBuilder>, StatusCode> {
    for session in state.list_sessions() {
        let events = session.conduit.events_after(0);
        let mut builders = builders_from_events(state, events);
        if let Some(builder) = builders.remove(invocation_id) {
            return Ok(Some(builder));
        }
    }

    let Some(storage) = state.storage.clone() else {
        return Ok(None);
    };
    let rows = tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store.list_sessions().unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    for row in rows {
        let events = session_events(state, &row.id).await.unwrap_or_default();
        let mut builders = builders_from_events(state, events);
        if let Some(builder) = builders.remove(invocation_id) {
            return Ok(Some(builder));
        }
    }
    Ok(None)
}

async fn session_events(
    state: &DaemonState,
    session_id: &str,
) -> Result<Vec<ProtocolEvent>, StatusCode> {
    if let Some(live) = state.get_session(session_id) {
        return Ok(live.conduit.events_after(0));
    }
    let Some(storage) = state.storage.clone() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let sid = session_id.to_string();
    let rows = tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store.all_events(&sid).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .collect())
}
