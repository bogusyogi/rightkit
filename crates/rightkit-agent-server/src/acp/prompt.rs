//! CG-A08 — `session/prompt` handler for the ACP stdio bridge: submits the
//! prompt, polls the daemon's event history, streams `session/update`
//! notifications, and round-trips `session/request_permission` for tool
//! approvals.
//!
//! NOTICE: relocated from the pre-existing `acp_server_prompt.rs`
//! (RightKit-original, RETAIN action per canon CG-A08); logic and test
//! lineage carried forward unchanged aside from module paths.

use super::session::{
    event_kind, protocol_event_to_session_update, require_existing_session, require_session_id,
    terminal_stop_reason, DaemonClient,
};
use super::transport::AcpTransport;
use rightkit_agents::agent_acp as acp;
use serde_json::{json, Value};

pub async fn handle_session_prompt(
    params: Value,
    client: &DaemonClient,
    transport: &AcpTransport,
) -> Result<Value, acp::AcpError> {
    let request: acp::PromptRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    require_session_id(&request.session_id)?;
    if request.prompt.is_empty() {
        return Err(acp::AcpError::invalid_params("prompt must not be empty"));
    }
    require_existing_session(&request.session_id, client).await?;
    let text = acp::prompt_to_text(&request.prompt);
    let start_seq = client
        .send_message(&request.session_id, &text)
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to submit prompt: {e}")))?;

    // Poll event history for a terminal event, streaming updates and
    // resolving tool-approval requests as they arrive. Bounded so a stuck
    // engine can't hang the ACP client forever; the caller can still
    // `session/cancel` mid-poll.
    let mut after = start_seq.max(0) - 1;
    let mut resolved_approvals = std::collections::HashSet::new();
    for _ in 0..1200 {
        let events = client
            .events_after(&request.session_id, after)
            .await
            .unwrap_or_default();
        for event in &events {
            if let Some(seq) = event.get("seq").and_then(Value::as_i64) {
                after = after.max(seq);
            }
            if event_kind(event) == Some("toolApprovalRequested") {
                if let Some(tool_call_id) = event
                    .get("call")
                    .and_then(|c| c.get("id"))
                    .and_then(Value::as_str)
                {
                    if resolved_approvals.insert(tool_call_id.to_string()) {
                        resolve_tool_approval_over_acp(
                            &request.session_id,
                            tool_call_id,
                            event,
                            client,
                            transport,
                        )
                        .await;
                    }
                }
                continue;
            }
            if let Some(update) = protocol_event_to_session_update(event) {
                emit_session_update(&request.session_id, update, transport);
            }
            if let Some(reason) = terminal_stop_reason(event) {
                return serde_json::to_value(acp::PromptResponse {
                    stop_reason: reason,
                })
                .map_err(|e| acp::AcpError::internal_error(e.to_string()));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(prompt_timeout_error(&request.session_id, after))
}

pub fn prompt_timeout_error(session_id: &str, after_seq: i64) -> acp::AcpError {
    acp::AcpError::with_message(
        acp::AcpErrorCode::RequestCancelled,
        "session/prompt timed out before a terminal event",
    )
    .with_data(json!({
        "code": "acp_prompt_timeout",
        "sessionId": session_id,
        "afterSeq": after_seq,
        "retryable": true,
        "effectState": "unknown"
    }))
}

/// Sends a real `session/request_permission` request to the ACP client and
/// blocks until it answers, then forwards the decision to the daemon's
/// existing binary tool-approval gate. If the client never answers (closed
/// pipe, or doesn't implement the method), the tool call is denied —
/// fail-closed, matching CR's own default-deny posture for unresolved
/// approvals.
async fn resolve_tool_approval_over_acp(
    session_id: &str,
    tool_call_id: &str,
    event: &Value,
    client: &DaemonClient,
    transport: &AcpTransport,
) {
    let title = event
        .get("call")
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool_call")
        .to_string();
    let raw_input = event.get("call").and_then(|c| c.get("input")).cloned();
    let request = acp::RequestPermissionRequest {
        session_id: session_id.to_string(),
        tool_call: acp::ToolCallUpdate {
            tool_call_id: tool_call_id.to_string(),
            title,
            status: acp::ToolCallStatus::Pending,
            raw_input,
        },
        options: acp::standard_permission_options(),
    };
    let params = serde_json::to_value(&request).unwrap_or_default();
    let approved = match transport
        .send_request(acp::METHOD_SESSION_REQUEST_PERMISSION, params)
        .await
    {
        Some(response) => match response.get("result") {
            Some(result) => {
                match serde_json::from_value::<acp::RequestPermissionResponse>(result.clone()) {
                    Ok(parsed) => acp::outcome_to_approval(&parsed.outcome),
                    Err(_) => false,
                }
            }
            None => false,
        },
        None => false,
    };
    let _ = client
        .resolve_tool_approval(session_id, tool_call_id, approved)
        .await;
}

/// Emits an ACP `session/update` notification, out-of-band from the eventual
/// `session/prompt` response (matches the protocol's streaming contract:
/// updates arrive as the turn runs, the response only signals completion +
/// stop reason).
pub(crate) fn emit_session_update(
    session_id: &str,
    update: acp::SessionUpdate,
    transport: &AcpTransport,
) {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": acp::METHOD_SESSION_UPDATE,
        "params": acp::UpdateSessionNotification {
            session_id: session_id.to_string(),
            update,
        },
    });
    let _ = transport.send_line(&notification);
}
