async fn storage_records_for_session(
    state: &DaemonState,
    session_id: &str,
) -> Option<Vec<ToolInvocationRecord>> {
    let storage = state.storage.as_ref()?.clone();
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store
            .list_tool_invocations_for_session(&session_id)
            .ok()
            .map(|rows| rows.into_iter().map(record_from_storage_row).collect())
    })
    .await
    .ok()
    .flatten()
}

async fn storage_record_by_id(
    state: &DaemonState,
    invocation_id: &str,
) -> Option<ToolInvocationRecord> {
    let storage = state.storage.as_ref()?.clone();
    let invocation_id = invocation_id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store
            .get_tool_invocation(&invocation_id)
            .ok()
            .flatten()
            .map(record_from_storage_row)
    })
    .await
    .ok()
    .flatten()
}

async fn storage_events_by_invocation(
    state: &DaemonState,
    invocation_id: &str,
) -> Option<Vec<ToolInvocationEventRow>> {
    let storage = state.storage.as_ref()?.clone();
    let invocation_id = invocation_id.to_string();
    tokio::task::spawn_blocking(move || {
        let store = storage.blocking_lock();
        store
            .list_tool_invocation_events(&invocation_id)
            .ok()
            .map(|rows| rows.into_iter().map(event_from_storage_row).collect())
    })
    .await
    .ok()
    .flatten()
}

fn storage_row_from_builder(
    builder: InvocationBuilder,
    lifecycle_state: &str,
    updated_at: &str,
) -> rightkit_agent_engine_storage::ToolInvocationStoreRow {
    rightkit_agent_engine_storage::ToolInvocationStoreRow {
        id: builder.id.clone(),
        session_id: builder.session_id,
        turn_id: infer_turn_id(&builder.id),
        tool_name: builder.tool_name,
        schema_version: builder.schema_version,
        args_hash: builder.args_hash,
        args_redacted_json: serde_json::to_string(&builder.args_redacted_json)
            .unwrap_or_else(|_| "null".to_string()),
        lifecycle_state: lifecycle_state.to_string(),
        policy_verdict_id: builder.policy_verdict_id,
        approval_id: builder.approval_id,
        process_id: builder.process_id,
        started_at: builder.started_at.or_else(|| Some(updated_at.to_string())),
        completed_at: builder.completed_at,
        output_hash: builder.output_hash,
        artifact_ids_json: "[]".to_string(),
        error_class: builder.error_class,
        updated_at: updated_at.to_string(),
    }
}

fn storage_event_row(
    session_id: &str,
    invocation_id: &str,
    seq: i64,
    kind: &str,
    created_at: &str,
    payload_redacted_json: Value,
) -> rightkit_agent_engine_storage::ToolInvocationEventStoreRow {
    rightkit_agent_engine_storage::ToolInvocationEventStoreRow {
        invocation_id: invocation_id.to_string(),
        session_id: session_id.to_string(),
        seq,
        kind: kind.to_string(),
        created_at: created_at.to_string(),
        payload_redacted_json: serde_json::to_string(&payload_redacted_json)
            .unwrap_or_else(|_| "null".to_string()),
    }
}

fn record_from_storage_row(row: rightkit_agent_engine_storage::ToolInvocationStoreRow) -> ToolInvocationRecord {
    ToolInvocationRecord {
        id: row.id,
        session_id: row.session_id,
        turn_id: row.turn_id,
        tool_name: row.tool_name,
        schema_version: row.schema_version,
        args_hash: row.args_hash,
        args_redacted_json: serde_json::from_str(&row.args_redacted_json).unwrap_or(Value::Null),
        lifecycle_state: row.lifecycle_state,
        policy_verdict_id: row.policy_verdict_id,
        approval_id: row.approval_id,
        process_id: row.process_id,
        started_at: row.started_at,
        completed_at: row.completed_at,
        output_hash: row.output_hash,
        artifact_ids: serde_json::from_str(&row.artifact_ids_json).unwrap_or_default(),
        error_class: row.error_class,
    }
}

fn event_from_storage_row(
    row: rightkit_agent_engine_storage::ToolInvocationEventStoreRow,
) -> ToolInvocationEventRow {
    ToolInvocationEventRow {
        invocation_id: row.invocation_id,
        session_id: row.session_id,
        seq: row.seq,
        kind: row.kind,
        created_at: row.created_at,
        payload_redacted_json: serde_json::from_str(&row.payload_redacted_json)
            .unwrap_or(Value::Null),
    }
}

fn tool_schema_versions(state: &DaemonState) -> HashMap<String, String> {
    let registry = ToolRegistry::build_default_with_plugins(state.plugin_dir.clone());
    registry
        .all()
        .iter()
        .map(|tool| {
            let schema = tool.input_schema();
            (
                tool.name().to_string(),
                schema_version(tool.name(), &schema),
            )
        })
        .collect()
}

fn invocation_event_row(
    session_id: &str,
    invocation_id: &str,
    seq: i64,
    kind: &str,
    created_at: &str,
    payload_redacted_json: Value,
) -> ToolInvocationEventRow {
    ToolInvocationEventRow {
        invocation_id: invocation_id.to_string(),
        session_id: session_id.to_string(),
        seq,
        kind: kind.to_string(),
        created_at: created_at.to_string(),
        payload_redacted_json,
    }
}

fn redacted_tool_call(call: &ToolCall) -> Value {
    let args = serde_json::from_str::<Value>(&call.arguments_json).unwrap_or(Value::Null);
    serde_json::json!({
        "id": call.id,
        "name": call.name,
        "argumentsJson": serde_json::to_string(&redact_json(args)).unwrap_or_else(|_| "{}".to_string()),
    })
}

fn redact_json(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    if lower.contains("token")
                        || lower.contains("secret")
                        || lower.contains("password")
                        || lower.contains("api_key")
                        || lower.contains("apikey")
                        || lower == "key"
                    {
                        (key, Value::String("[redacted]".to_string()))
                    } else {
                        (key, redact_json(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_json).collect()),
        other => other,
    }
}

fn output_hash(summary: &str, output: Option<&rightkit_agent_engine_protocol::OutputRef>) -> String {
    if let Some(hash) = output.and_then(|out| out.content_hash.clone()) {
        return hash;
    }
    format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(summary.as_bytes())
    )
}

fn error_class(summary: &str) -> String {
    let lower = summary.to_ascii_lowercase();
    if lower.contains("permission denied") {
        "permission_denied"
    } else if lower.contains("guardrail denied") {
        "guardrail_denied"
    } else if lower.contains("denied by user") {
        "user_denied"
    } else if lower.contains("unknown tool") {
        "unknown_tool"
    } else {
        "tool_error"
    }
    .to_string()
}

fn infer_turn_id(invocation_id: &str) -> String {
    invocation_id
        .split_once(':')
        .map(|(turn, _)| turn.to_string())
        .unwrap_or_default()
}
