use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_engine_protocol::{ProtocolEvent, ToolCall, TranscriptMessage};
use rightkit_agent_coding_tools::registry::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

use crate::api::{lock_recover, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_tool_schemas::schema_version;

pub(crate) fn persist_tool_invocation_event(
    store: &mut rightkit_agent_engine_storage::Storage,
    event: &ProtocolEvent,
) -> Result<(), String> {
    match event {
        ProtocolEvent::ToolCallStarted {
            session_id,
            seq,
            created_at,
            call,
        } => {
            let builder = builder_from_call(session_id, call, &HashMap::new());
            store
                .upsert_tool_invocation(&storage_row_from_builder(builder, "requested", created_at))
                .map_err(|e| e.to_string())?;
            store
                .append_tool_invocation_event(&storage_event_row(
                    session_id,
                    &call.id,
                    *seq,
                    "toolCallStarted",
                    created_at,
                    serde_json::json!({ "call": redacted_tool_call(call) }),
                ))
                .map_err(|e| e.to_string())?;
        }
        ProtocolEvent::ToolApprovalRequested {
            session_id,
            seq,
            created_at,
            call,
            risk_label,
            proposed_change,
            policy_source,
            middleware_decisions,
        } => {
            let mut row = store
                .get_tool_invocation(&call.id)
                .map_err(|e| e.to_string())?
                .unwrap_or_else(|| {
                    storage_row_from_builder(
                        builder_from_call(session_id, call, &HashMap::new()),
                        "approvalRequested",
                        created_at,
                    )
                });
            row.lifecycle_state = "approvalRequested".to_string();
            row.approval_id = Some(call.id.clone());
            row.policy_verdict_id = policy_source.clone();
            row.updated_at = created_at.clone();
            store
                .upsert_tool_invocation(&row)
                .map_err(|e| e.to_string())?;
            store
                .append_tool_invocation_event(&storage_event_row(
                    session_id,
                    &call.id,
                    *seq,
                    "toolApprovalRequested",
                    created_at,
                    serde_json::json!({
                        "call": redacted_tool_call(call),
                        "riskLabel": risk_label,
                        "proposedChange": proposed_change,
                        "policySource": policy_source,
                        "middlewareDecisions": middleware_decisions,
                    }),
                ))
                .map_err(|e| e.to_string())?;
        }
        ProtocolEvent::ToolApprovalResolved {
            session_id,
            seq,
            created_at,
            tool_call_id,
            approved,
            reason,
        } => {
            if let Some(mut row) = store
                .get_tool_invocation(tool_call_id)
                .map_err(|e| e.to_string())?
            {
                row.lifecycle_state = if *approved { "approved" } else { "denied" }.to_string();
                row.approval_id = Some(tool_call_id.clone());
                row.updated_at = created_at.clone();
                store
                    .upsert_tool_invocation(&row)
                    .map_err(|e| e.to_string())?;
            }
            let mut payload = serde_json::json!({
                "toolCallId": tool_call_id,
                "approved": approved,
            });
            // Same shape as the wire: `reason` is present only on a denial that carries one.
            if let Some(reason) = reason {
                payload["reason"] = serde_json::json!(reason);
            }
            store
                .append_tool_invocation_event(&storage_event_row(
                    session_id,
                    tool_call_id,
                    *seq,
                    "toolApprovalResolved",
                    created_at,
                    payload,
                ))
                .map_err(|e| e.to_string())?;
        }
        ProtocolEvent::ToolResultAdded {
            session_id,
            seq,
            created_at,
            result:
                TranscriptMessage::Tool {
                    tool_call_id,
                    summary,
                    output,
                    is_error,
                    process_id,
                    ..
                },
        } => {
            if let Some(mut row) = store
                .get_tool_invocation(tool_call_id)
                .map_err(|e| e.to_string())?
            {
                row.lifecycle_state = if *is_error { "failed" } else { "completed" }.to_string();
                row.completed_at = Some(created_at.clone());
                row.output_hash = Some(output_hash(summary, output.as_ref()));
                row.error_class = if *is_error {
                    Some(error_class(summary))
                } else {
                    None
                };
                row.process_id = process_id.clone();
                row.updated_at = created_at.clone();
                store
                    .upsert_tool_invocation(&row)
                    .map_err(|e| e.to_string())?;
            }
            store
                .append_tool_invocation_event(&storage_event_row(
                    session_id,
                    tool_call_id,
                    *seq,
                    "toolResultAdded",
                    created_at,
                    serde_json::json!({
                        "toolCallId": tool_call_id,
                        "summary": summary,
                        "output": output,
                        "isError": is_error,
                    }),
                ))
                .map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ToolInvocationCancellation {
    pub invocation_id: String,
    pub session_id: String,
    pub reason: Option<String>,
    pub created_at: String,
}

pub(crate) fn tool_invocation_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/sessions/{id}/tool-invocations",
            get(list_session_tool_invocations_handler),
        )
        .route("/tool-invocations/{id}", get(get_tool_invocation_handler))
        .route(
            "/tool-invocations/{id}/cancel",
            post(cancel_tool_invocation_handler),
        )
        .route(
            "/tool-invocations/{id}/events",
            get(get_tool_invocation_events_handler),
        )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolInvocationRecord {
    id: String,
    session_id: String,
    turn_id: String,
    tool_name: String,
    schema_version: String,
    args_hash: String,
    args_redacted_json: Value,
    lifecycle_state: String,
    policy_verdict_id: Option<String>,
    approval_id: Option<String>,
    process_id: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    output_hash: Option<String>,
    artifact_ids: Vec<String>,
    error_class: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolInvocationEventRow {
    invocation_id: String,
    session_id: String,
    seq: i64,
    kind: String,
    created_at: String,
    payload_redacted_json: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelToolInvocationResponse {
    cancelled: bool,
    tool_invocation_id: String,
    session_id: String,
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelToolInvocationBody {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct InvocationBuilder {
    id: String,
    session_id: String,
    tool_name: String,
    schema_version: String,
    args_hash: String,
    args_redacted_json: Value,
    lifecycle_state: String,
    policy_verdict_id: Option<String>,
    approval_id: Option<String>,
    process_id: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    output_hash: Option<String>,
    error_class: Option<String>,
    events: Vec<ToolInvocationEventRow>,
}

impl InvocationBuilder {
    fn into_record(self) -> ToolInvocationRecord {
        ToolInvocationRecord {
            id: self.id.clone(),
            session_id: self.session_id,
            turn_id: infer_turn_id(&self.id),
            tool_name: self.tool_name,
            schema_version: self.schema_version,
            args_hash: self.args_hash,
            args_redacted_json: self.args_redacted_json,
            lifecycle_state: self.lifecycle_state,
            policy_verdict_id: self.policy_verdict_id,
            approval_id: self.approval_id,
            process_id: self.process_id,
            started_at: self.started_at,
            completed_at: self.completed_at,
            output_hash: self.output_hash,
            artifact_ids: Vec::new(),
            error_class: self.error_class,
        }
    }
}
