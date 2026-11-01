//! CG-A08 — ACP session lifecycle: `session/new`, `session/load`,
//! `session/resume`, `session/close`, `session/list`, `session/delete`, plus
//! the `DaemonClient` HTTP glue and protocol-event → ACP-update mapping they
//! depend on.
//!
//! NOTICE: this module consolidates the pre-existing `acp_bridge.rs`
//! (`DaemonClient`, `protocol_event_to_session_update`,
//! `terminal_stop_reason`) and the session-lifecycle half of `acp_server.rs`
//! (RETAIN action per canon CG-A08) into the ACP module's canonical home.
//! Logic and test lineage are carried forward with only the module path and
//! internal `use` paths changed to fit the new tree.
//!
//! `DaemonClient` is a separate-*process* HTTP client (this binary runs as
//! `coderight acp-server`, a thin protocol adapter), not an in-process
//! daemon route — it reuses `POST /sessions`, `POST /sessions/{id}/messages`,
//! `GET /sessions/{id}/events/history`, `POST /sessions/{id}/interrupt`,
//! `POST /sessions/{id}/tool-approval`, `GET /sessions`,
//! `DELETE /sessions/{id}` rather than adding new daemon HTTP surface.
//!
//! CG-A06 note: `session/new` canonicalizes its durable project root, while
//! `session/load` / `session/resume` admit navigation only inside that bound
//! root. Unresolvable or outside cwd values fail closed.

use rightkit_agents::agent_acp as acp;
use serde_json::{json, Value};

use crate::project_binding::ProjectBinding;

pub struct DaemonClient {
    port: u16,
    token: Option<String>,
    client: rightkit_http::AsyncClient,
}

pub(crate) fn require_session_id(session_id: &str) -> Result<(), acp::AcpError> {
    if session_id.trim().is_empty() {
        Err(acp::AcpError::invalid_params("sessionId must not be empty"))
    } else {
        Ok(())
    }
}

pub(crate) async fn require_existing_session(
    session_id: &str,
    client: &DaemonClient,
) -> Result<(), acp::AcpError> {
    let sessions = client.list_sessions().await.map_err(|error| {
        acp::AcpError::internal_error(format!("failed to list sessions: {error}"))
    })?;
    if sessions
        .iter()
        .any(|row| row.get("id").and_then(Value::as_str) == Some(session_id))
    {
        Ok(())
    } else {
        Err(acp::AcpError::resource_not_found(None))
    }
}

impl DaemonClient {
    pub fn new(port: u16, token: Option<String>) -> Self {
        let client = rightkit_http::http_client::daemon_client(&format!("http://127.0.0.1:{port}"))
            .expect("rightkit-http client builds with the platform verifier");
        Self {
            port,
            token,
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// One daemon call: `method` on `path`, bearer token when configured, JSON `body`
    /// when present. Any status is a response; transport failures are errors.
    async fn call(
        &self,
        method: rightkit_http::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<rightkit_http::Response, String> {
        let mut request = rightkit_http::Request::new(method, self.url(path));
        if let Some(token) = &self.token {
            request = request.bearer(token);
        }
        if let Some(body) = body {
            request = request.json(body).map_err(|e| e.to_string())?;
        }
        self.client
            .send_any(&request)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn create_session(
        &self,
        session_id: &str,
        workspace_root: &str,
        title: &str,
    ) -> Result<Value, String> {
        let body = json!({
            "id": session_id,
            "workspaceRoot": workspace_root,
            "title": title,
        });
        let resp = self
            .call(rightkit_http::Method::Post, "/sessions", Some(&body))
            .await?;
        let status = resp.status;
        if status == 409 {
            return Ok(json!({ "id": session_id, "alreadyExists": true }));
        }
        if !rightkit_http::http_client::is_success(status) {
            return Err(format!("HTTP {status}"));
        }
        resp.json::<Value>().map_err(|e| e.to_string())
    }

    pub async fn send_message(&self, session_id: &str, text: &str) -> Result<i64, String> {
        let body = json!({ "text": text });
        let resp = self
            .call(
                rightkit_http::Method::Post,
                &format!("/sessions/{session_id}/messages"),
                Some(&body),
            )
            .await?;
        let status = resp.status;
        if !rightkit_http::http_client::is_success(status) {
            return Err(format!("HTTP {status}"));
        }
        let value: Value = resp.json().map_err(|e| e.to_string())?;
        Ok(value.get("seq").and_then(Value::as_i64).unwrap_or(0))
    }

    pub async fn events_after(&self, session_id: &str, after: i64) -> Result<Vec<Value>, String> {
        let resp = self
            .call(
                rightkit_http::Method::Get,
                &format!("/sessions/{session_id}/events/history?after={after}"),
                None,
            )
            .await?;
        let status = resp.status;
        if status == 404 {
            return Ok(Vec::new());
        }
        if !rightkit_http::http_client::is_success(status) {
            return Err(format!("HTTP {status}"));
        }
        resp.json::<Vec<Value>>().map_err(|e| e.to_string())
    }

    pub async fn interrupt_session(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .call(
                rightkit_http::Method::Post,
                &format!("/sessions/{session_id}/interrupt"),
                None,
            )
            .await?;
        if !rightkit_http::http_client::is_success(resp.status) {
            return Err(format!("HTTP {}", resp.status));
        }
        Ok(())
    }

    pub async fn resolve_tool_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), String> {
        let body = json!({ "toolCallId": tool_call_id, "approved": approved });
        let resp = self
            .call(
                rightkit_http::Method::Post,
                &format!("/sessions/{session_id}/tool-approval"),
                Some(&body),
            )
            .await?;
        let status = resp.status;
        // Already-resolved / unknown tool call ids are not fatal to the ACP
        // caller — the underlying approval race is CR's, not ACP's, to solve.
        if !rightkit_http::http_client::is_success(status) && status != 404 {
            return Err(format!("HTTP {status}"));
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<Value>, String> {
        let resp = self
            .call(rightkit_http::Method::Get, "/sessions", None)
            .await?;
        if !rightkit_http::http_client::is_success(resp.status) {
            return Err(format!("HTTP {}", resp.status));
        }
        resp.json::<Vec<Value>>().map_err(|e| e.to_string())
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .call(
                rightkit_http::Method::Delete,
                &format!("/sessions/{session_id}"),
                None,
            )
            .await?;
        let status = resp.status;
        if !rightkit_http::http_client::is_success(status) && status != 404 {
            return Err(format!("HTTP {status}"));
        }
        Ok(())
    }

    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .call(
                rightkit_http::Method::Post,
                &format!("/sessions/{session_id}/kill"),
                None,
            )
            .await?;
        let status = resp.status;
        if !rightkit_http::http_client::is_success(status) && status != 404 {
            return Err(format!("HTTP {status}"));
        }
        Ok(())
    }
}

/// Event discriminator of a raw history row. The daemon serializes
/// `ProtocolEvent` with a `$kind` tag.
pub fn event_kind(event: &Value) -> Option<&str> {
    event.get("$kind").and_then(Value::as_str)
}

/// Translates one raw `ProtocolEvent` JSON row from the daemon's event
/// history into zero-or-one ACP `session/update` payloads. Returns `None`
/// for event kinds that don't have an ACP-visible projection yet (task
/// graph, budget, sub-agent roster, etc. — out of scope for baseline
/// message/tool-call streaming).
pub fn protocol_event_to_session_update(event: &Value) -> Option<acp::SessionUpdate> {
    let kind = event_kind(event)?;
    match kind {
        "assistantDelta" => {
            let text = event.get("text").and_then(Value::as_str)?.to_string();
            Some(acp::SessionUpdate::AgentMessageChunk(
                acp::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        text,
                        annotations: None,
                    }),
                },
            ))
        }
        // Live token deltas are never durable, so the polled history carries
        // the model's text only as the completed assistant message. Project
        // it as the turn's message chunk so ACP clients always receive the
        // answer as `agent_message_chunk` before the stop reason.
        "assistantMessageCompleted" => {
            let message = event
                .get("message")
                .or_else(|| event.get("data").and_then(|data| data.get("message")))?;
            let text = message.get("content").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            Some(acp::SessionUpdate::AgentMessageChunk(
                acp::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        text: text.to_string(),
                        annotations: None,
                    }),
                },
            ))
        }
        "toolCallStarted" => {
            let call = event.get("call")?;
            let tool_call_id = call.get("id").and_then(Value::as_str)?.to_string();
            let title = call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool_call")
                .to_string();
            Some(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate {
                tool_call_id,
                title,
                status: acp::ToolCallStatus::InProgress,
                raw_input: call.get("input").cloned(),
            }))
        }
        "toolResultAdded" => {
            let tool_call_id = event.get("toolCallId").and_then(Value::as_str)?.to_string();
            let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(true);
            Some(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate {
                tool_call_id,
                title: "tool_call".to_string(),
                status: if ok {
                    acp::ToolCallStatus::Completed
                } else {
                    acp::ToolCallStatus::Failed
                },
                raw_input: None,
            }))
        }
        _ => None,
    }
}

/// A turn is considered finished once one of these terminal event kinds
/// appears in the polled history; the matched kind decides the ACP
/// `StopReason`.
pub fn terminal_stop_reason(event: &Value) -> Option<acp::StopReason> {
    match event_kind(event)? {
        "assistantMessageCompleted" => Some(acp::StopReason::EndTurn),
        "sessionTurnFinished" => Some(acp::StopReason::EndTurn),
        "streamResyncRequired" => Some(acp::StopReason::Refusal),
        "overloaded" => Some(acp::StopReason::MaxTurnRequests),
        "runInterrupted" => Some(acp::StopReason::Cancelled),
        "runFailed" => Some(acp::StopReason::Refusal),
        "budgetExceeded" => Some(acp::StopReason::MaxTurnRequests),
        _ => None,
    }
}

// ── session/new, session/load, session/resume, session/close ──────────────

pub async fn handle_session_new(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::NewSessionRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    let workspace_root = trusted_workspace_root(&request.cwd)?;
    let session_id = format!("acp-{}", now_millis());
    client
        .create_session(&session_id, &workspace_root, "ACP session")
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to create session: {e}")))?;
    serde_json::to_value(acp::NewSessionResponse { session_id })
        .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

pub async fn handle_session_load(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::LoadSessionRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    ensure_session_bound(&request.session_id, &request.cwd, client).await?;
    serde_json::to_value(acp::LoadSessionResponse {})
        .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

pub async fn handle_session_resume(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::ResumeSessionRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    ensure_session_bound(&request.session_id, &request.cwd, client).await?;
    serde_json::to_value(acp::ResumeSessionResponse {})
        .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

async fn ensure_session_bound(
    session_id: &str,
    cwd: &str,
    client: &DaemonClient,
) -> Result<(), acp::AcpError> {
    if session_id.trim().is_empty() {
        return Err(acp::AcpError::invalid_params("sessionId must not be empty"));
    }
    let existing = client.list_sessions().await.unwrap_or_default();
    let known = existing
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(session_id));
    if let Some(row) = known {
        let workspace_root = row
            .get("workspaceRoot")
            .and_then(Value::as_str)
            .ok_or_else(|| acp::AcpError::invalid_params("session has no workspace root"))?;
        let binding = ProjectBinding::bind(workspace_root);
        if binding.evaluate(cwd).is_trusted() {
            return Ok(());
        }
        return Err(acp::AcpError::invalid_params(
            "cwd is outside or cannot resolve within the session's trusted project root",
        ));
    }
    let workspace_root = trusted_workspace_root(cwd)?;
    client
        .create_session(session_id, &workspace_root, "ACP session")
        .await
        .map(|_| ())
        .map_err(|e| acp::AcpError::resource_not_found(None).with_data(json!({ "detail": e })))
}

/// Resolve a new ACP cwd before it becomes durable session state. A path that
/// cannot be canonicalized is not admitted: later navigation would otherwise
/// have no trustworthy project boundary to evaluate against.
fn trusted_workspace_root(cwd: &str) -> Result<String, acp::AcpError> {
    if cwd.trim().is_empty() {
        return Err(acp::AcpError::invalid_params("cwd must not be empty"));
    }
    let binding = ProjectBinding::bind(cwd);
    let Some(root) = binding.canonical_root() else {
        return Err(acp::AcpError::invalid_params(
            "cwd must resolve to an accessible project root",
        ));
    };
    if !binding.evaluate(cwd).is_trusted() {
        return Err(acp::AcpError::invalid_params(
            "cwd is outside or cannot resolve within its trusted project root",
        ));
    }
    Ok(root.display().to_string())
}

pub async fn handle_session_close(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::CloseSessionRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    require_session_id(&request.session_id)?;
    client
        .kill_session(&request.session_id)
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to close session: {e}")))?;
    serde_json::to_value(acp::CloseSessionResponse {})
        .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

// ── session/list, session/delete ───────────────────────────────────────────

pub async fn handle_session_list(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::ListSessionsRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    let rows = client
        .list_sessions()
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to list sessions: {e}")))?;
    let sessions: Vec<acp::SessionInfo> = rows
        .iter()
        .filter(|row| {
            request
                .cwd
                .as_deref()
                .is_none_or(|cwd| row.get("workspaceRoot").and_then(Value::as_str) == Some(cwd))
        })
        .map(|row| acp::SessionInfo {
            session_id: row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cwd: row
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            additional_directories: Vec::new(),
            title: row.get("title").and_then(Value::as_str).map(str::to_string),
            updated_at: row
                .get("updatedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect();
    serde_json::to_value(acp::ListSessionsResponse {
        sessions,
        next_cursor: None,
    })
    .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

pub async fn handle_session_delete(
    params: Value,
    client: &DaemonClient,
) -> Result<Value, acp::AcpError> {
    let request: acp::DeleteSessionRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    require_session_id(&request.session_id)?;
    client
        .delete_session(&request.session_id)
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to delete session: {e}")))?;
    serde_json::to_value(acp::DeleteSessionResponse {})
        .map_err(|e| acp::AcpError::internal_error(e.to_string()))
}

pub fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_delta_maps_to_agent_message_chunk() {
        let event = json!({"$kind": "assistantDelta", "text": "hi"});
        let update = protocol_event_to_session_update(&event).unwrap();
        assert_eq!(update.kind(), acp::SessionUpdateKind::AgentMessageChunk);
    }

    #[test]
    fn completed_assistant_message_maps_to_agent_message_chunk() {
        let event = json!({"$kind": "assistantMessageCompleted", "message": {"content": "done"}});
        let update = protocol_event_to_session_update(&event).unwrap();
        assert_eq!(update.kind(), acp::SessionUpdateKind::AgentMessageChunk);
    }

    #[test]
    fn unmapped_event_kind_returns_none() {
        let event = json!({"$kind": "taskGraphUpdated"});
        assert!(protocol_event_to_session_update(&event).is_none());
    }

    #[test]
    fn assistant_message_completed_is_end_turn() {
        let event = json!({"$kind": "assistantMessageCompleted"});
        assert_eq!(terminal_stop_reason(&event), Some(acp::StopReason::EndTurn));
    }

    #[test]
    fn run_interrupted_is_cancelled_stop_reason() {
        let event = json!({"$kind": "runInterrupted"});
        assert_eq!(
            terminal_stop_reason(&event),
            Some(acp::StopReason::Cancelled)
        );
    }

    #[test]
    fn non_terminal_event_has_no_stop_reason() {
        let event = json!({"$kind": "assistantDelta"});
        assert_eq!(terminal_stop_reason(&event), None);
    }

    #[test]
    fn public_backpressure_events_close_the_acp_turn() {
        assert_eq!(
            terminal_stop_reason(&json!({"$kind": "streamResyncRequired"})),
            Some(acp::StopReason::Refusal)
        );
        assert_eq!(
            terminal_stop_reason(&json!({"$kind": "overloaded"})),
            Some(acp::StopReason::MaxTurnRequests)
        );
    }
}
