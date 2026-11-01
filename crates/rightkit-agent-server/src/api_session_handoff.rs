use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use rightkit_agent_engine_protocol::TranscriptMessage;
use coderight_skills::{CommandAction, CommandResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::{api_workspace_root, lock_recover, now_iso, DaemonState, LiveSession};

pub fn session_handoff_routes() -> Router<DaemonState> {
    Router::new().route("/sessions/{id}/handoff", get(get_session_handoff))
}

pub(crate) async fn command_export(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let as_json = args.iter().any(|arg| arg == "--json");
    let session_id = args
        .iter()
        .find(|arg| arg.as_str() != "--json")
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .or_else(|| {
            active_session_id
                .map(str::trim)
                .filter(|arg| !arg.is_empty())
        });
    let Some(session_id) = session_id else {
        return command_err(
            "Usage: /export [--json] in an active session, or /export <session-id> [--json]",
        );
    };

    let messages = match export_session_messages(state, session_id).await {
        Ok(messages) => messages,
        Err(err) => return command_err(err),
    };
    if as_json {
        let value = serde_json::json!({
            "sessionId": session_id,
            "messageCount": messages.len(),
            "messages": messages,
        });
        let raw = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
        return command_ok(format!(
            "**Session export:** {session_id}\nFormat: json\n\n```json\n{raw}\n```"
        ));
    }

    command_ok(render_session_export_markdown(session_id, &messages))
}

pub(crate) async fn command_summary(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let session_id = args
        .iter()
        .find(|arg| !arg.trim().is_empty())
        .map(|arg| arg.trim())
        .or_else(|| {
            active_session_id
                .map(str::trim)
                .filter(|arg| !arg.is_empty())
        });
    let Some(session_id) = session_id else {
        return command_err("Usage: /summary in an active session, or /summary <session-id>");
    };

    let messages = match export_session_messages(state, session_id).await {
        Ok(messages) => messages,
        Err(err) => return command_err(err),
    };

    command_ok(render_session_summary_markdown(session_id, &messages))
}

pub(crate) async fn command_handoff(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let session_id = args
        .iter()
        .find(|arg| !arg.trim().is_empty())
        .map(|arg| arg.trim())
        .or_else(|| {
            active_session_id
                .map(str::trim)
                .filter(|arg| !arg.is_empty())
        });
    let Some(session_id) = session_id else {
        return command_err("Usage: /handoff in an active session, or /handoff <session-id>");
    };

    // PL-008 live daemon seam: `/handoff` runs the orchestrator-side saga
    // (`daemon::handoff_saga`) — proven-quiescence gate, PL-008's own intake
    // fence (distinct from FM-167's `HandoffController`), preflight,
    // drain/stop of the source, then storage::handoff's durable
    // claim/quiesce/resume/finish checkpoints under CAS controller-epoch —
    // before producing the continuity payload. Any refusal leaves the
    // source, and any already-durable row, untouched or rolled back to
    // `cancelled`; metadata remains available even when persistence is
    // disabled for an auxiliary daemon.
    let live_session = state.get_session(session_id);
    let quiescence = match &live_session {
        Some(live) => evaluate_quiescence(live),
        None => rightkit_agent_engine_storage::QuiescenceOutcome::ReadyToHandoff,
    };
    if quiescence != rightkit_agent_engine_storage::QuiescenceOutcome::ReadyToHandoff {
        return command_err(format!(
            "handoff refused: quiescence outcome {quiescence:?}"
        ));
    }
    if let Some(storage) = &state.storage {
        let storage = storage.clone();
        let id = session_id.to_string();
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1);
        let live_for_drain = live_session.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let store = storage.blocking_lock();
            let now = now_iso();
            let callbacks = DaemonHandoffCallbacks {
                live: live_for_drain.as_deref(),
                store: &store,
                epoch,
            };
            crate::daemon::handoff_saga::start(
                &*store,
                crate::daemon::handoff_saga::global_intake_fence(),
                &callbacks,
                &id,
                "native",
                "claude",
                epoch,
                rightkit_agent_engine_storage::QuiescenceOutcome::ReadyToHandoff,
                &now,
            )
        })
        .await;
    }

    let session = match session_handoff_metadata(state, session_id).await {
        Ok(session) => session,
        Err(StatusCode::NOT_FOUND) => {
            return command_err(format!("Session not found: {session_id}"));
        }
        Err(_) => return command_err(format!("Failed to create handoff for: {session_id}")),
    };
    let payload =
        redact_handoff_payload_for_display(session_handoff_payload(state, session_id, session));
    let raw = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    command_ok(format!(
        "**Session handoff:** {session_id}\n\n```json\n{raw}\n```"
    ))
}

/// PL-008 saga callbacks bound to the live daemon: preflight validates the
/// target pair, drain interrupts any live turn, and stop delivers any
/// gap-arriving messages retained while intake was fenced (durable retention
/// of gap-arriving messages) before the destination is allowed to resume.
struct DaemonHandoffCallbacks<'a> {
    live: Option<&'a LiveSession>,
    store: &'a rightkit_agent_engine_storage::Storage,
    epoch: u64,
}

impl<'a> crate::daemon::handoff_saga::HandoffCallbacks for DaemonHandoffCallbacks<'a> {
    fn preflight_target(
        &self,
        _session_id: &str,
        source: &str,
        destination: &str,
    ) -> Result<(), String> {
        if source.trim().is_empty() || destination.trim().is_empty() || source == destination {
            return Err(
                "handoff requires distinct non-empty source/destination targets".to_string(),
            );
        }
        Ok(())
    }

    fn drain_source(&self, _session_id: &str) -> Result<(), String> {
        if let Some(live) = self.live {
            if let Some(tx) = &live.interrupt_tx {
                let _ = tx.send(true);
            }
        }
        Ok(())
    }

    fn stop_source(&self, session_id: &str) -> Result<(), String> {
        if let Ok(messages) = self
            .store
            .pending_handoff_gap_messages(session_id, self.epoch)
        {
            let delivered_at = now_iso();
            for message in messages {
                let _ = self.store.mark_handoff_gap_delivered(
                    session_id,
                    self.epoch,
                    &message.message_key,
                    &delivered_at,
                );
            }
        }
        Ok(())
    }
}

/// PL-008 proven-quiescence signals sourced from the live session. A
/// dormant session (no live entry) is handled at the call site — it is
/// trivially `ReadyToHandoff` since nothing is running against it.
struct SessionQuiescenceSignals<'a> {
    live: &'a LiveSession,
}

impl<'a> crate::daemon::handoff_saga::QuiescenceSignals for SessionQuiescenceSignals<'a> {
    fn observed(&self) -> bool {
        true
    }

    fn has_pending_decision(&self) -> bool {
        !lock_recover(&self.live.pending_approvals, "handoff.pending_approvals").is_empty()
            || !lock_recover(&self.live.pending_questions, "handoff.pending_questions").is_empty()
    }

    fn has_draft(&self) -> bool {
        !lock_recover(&self.live.pending_plans, "handoff.pending_plans").is_empty()
    }

    fn has_active_work(&self) -> bool {
        for _ in 0..2 {
            if self.live.live_turn_handle.is_active()
                || self
                    .live
                    .inference_active_requests
                    .load(std::sync::atomic::Ordering::Acquire)
                    > 0
            {
                return true;
            }
            std::thread::yield_now();
        }
        false
    }
}

fn evaluate_quiescence(live: &LiveSession) -> rightkit_agent_engine_storage::QuiescenceOutcome {
    crate::daemon::handoff_saga::evaluate_quiescence(&SessionQuiescenceSignals { live })
}

async fn get_session_handoff(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<SessionHandoffQuery>,
) -> Result<Json<SessionHandoffPayload>, StatusCode> {
    let session = session_handoff_metadata(&state, &id).await?;
    Ok(Json(session_handoff_payload_after(
        &state,
        &id,
        session,
        query.after.unwrap_or(0).max(0),
    )))
}

async fn session_handoff_metadata(
    state: &DaemonState,
    id: &str,
) -> Result<serde_json::Value, StatusCode> {
    if let Some(live) = state.get_session(id) {
        let mut row = live_session_metadata(&live);
        row["status"] = serde_json::json!("running");
        return Ok(row);
    }

    let Some(storage) = &state.storage else {
        return Err(StatusCode::NOT_FOUND);
    };
    let storage2 = storage.clone();
    let id2 = id.to_string();
    let row = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        let high_water_mark = store
            .all_events(&id2)
            .ok()
            .and_then(|events| events.iter().map(|event| event.seq).max())
            .unwrap_or(0);
        store
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.id == id2)
            .map(|row| (row, high_water_mark))
    })
    .await
    .map_err(|_| StatusCode::NOT_FOUND)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let (row, high_water_mark) = row;
    let cfg: serde_json::Value =
        serde_json::from_str(&row.config_json).unwrap_or(serde_json::Value::Null);
    let updated_at = if row.updated_at.is_empty() {
        row.created_at.clone()
    } else {
        row.updated_at.clone()
    };
    Ok(serde_json::json!({
        "id": row.id,
        "title": row.title,
        "model": cfg.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "workspaceRoot": api_workspace_root(&row.workspace_root),
        "createdAt": row.created_at,
        "updatedAt": updated_at,
        "role": row.role,
        "toolset": cfg.get("toolset").cloned().unwrap_or(serde_json::Value::Null),
        "specId": cfg.get("specId").cloned().unwrap_or(serde_json::Value::Null),
        "parentSessionId": row.parent_session_id,
        "forkedFromSessionId": cfg.get("forkedFromSessionId").cloned().unwrap_or(serde_json::Value::Null),
        "executionBackend": cfg.get("executionBackend").cloned().unwrap_or(serde_json::Value::Null),
        "executionWorkspaceId": cfg.get("executionWorkspaceId").cloned().unwrap_or(serde_json::Value::Null),
        "mcpChannel": cfg.get("mcpChannel").cloned().unwrap_or_else(|| serde_json::json!("desktop")),
        "safeMode": cfg.get("safeMode").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "allowedTools": cfg.get("allowedTools").cloned().unwrap_or_else(|| serde_json::json!([])),
        "disallowedTools": cfg.get("disallowedTools").cloned().unwrap_or_else(|| serde_json::json!([])),
        "effort": cfg.get("effort").cloned().unwrap_or(serde_json::Value::Null),
        "eventHighWaterMark": high_water_mark,
        "status": row.status,
        "tags": serde_json::from_str::<Vec<String>>(&row.tags).unwrap_or_default(),
    }))
}

fn live_session_metadata(s: &LiveSession) -> serde_json::Value {
    let title = lock_recover(&s.title, "s.title").clone();
    let model = lock_recover(&s.model, "s.model").clone();
    let updated_at = lock_recover(&s.updated_at, "s.updated_at").clone();
    let spec_id = lock_recover(&s.spec_id, "s.spec_id").clone();
    serde_json::json!({
        "id": s.id,
        "title": title,
        "model": model,
        "workspaceRoot": api_workspace_root(&s.workspace_root),
        "createdAt": s.created_at,
        "updatedAt": updated_at,
        "role": s.role,
        "toolset": s.toolset,
        "specId": spec_id,
        "parentSessionId": s.parent_session_id,
        "forkedFromSessionId": s.forked_from_session_id,
        "executionBackend": s.execution_backend,
        "executionWorkspaceId": s.execution_workspace_id,
        "mcpChannel": s.mcp_channel,
        "safeMode": s.safe_mode,
        "allowedTools": s.allowed_tools,
        "disallowedTools": s.disallowed_tools,
        "effort": lock_recover(&s.effort, "session.effort").clone(),
        "eventHighWaterMark": s.conduit.current_high_water(),
        "tags": [],
    })
}

fn session_handoff_payload(
    state: &DaemonState,
    id: &str,
    session: serde_json::Value,
) -> SessionHandoffPayload {
    session_handoff_payload_after(state, id, session, 0)
}

fn session_handoff_payload_after(
    state: &DaemonState,
    id: &str,
    session: serde_json::Value,
    after: i64,
) -> SessionHandoffPayload {
    // A handoff payload is a live controller boundary, not merely transcript
    // metadata. Consumers can therefore refuse stale intake while root
    // ownership is being transferred.
    let controller_in_flight = state.handoff_intake_is_fenced(id);
    let base_url = daemon_base_url(state);
    let encoded_id = percent_encode_url_component(id);
    let session_path = format!("/sessions/{encoded_id}");
    let after = after.max(0);
    let high_water_mark = session
        .get("eventHighWaterMark")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0);
    let events_sse = format!("{session_path}/events?after={after}");
    let events_history = format!("{session_path}/events/history?after={after}");
    SessionHandoffPayload {
        kind: "coderight.session_handoff.v1".to_string(),
        created_at: now_iso(),
        session,
        daemon: SessionHandoffDaemon {
            base_url: base_url.clone(),
            requires_token: !state.token.is_empty(),
            token_header: "Authorization".to_string(),
            token_scheme: "Bearer".to_string(),
            access_token: if state.token.is_empty() {
                None
            } else {
                Some(state.token.clone())
            },
        },
        endpoints: SessionHandoffEndpoints {
            session: session_path.clone(),
            transcript: format!("{session_path}/transcript"),
            events_sse: events_sse.clone(),
            events_history: events_history.clone(),
            messages: format!("{session_path}/messages"),
            steer: format!("{session_path}/steer"),
            interrupt: format!("{session_path}/interrupt"),
            compact: format!("{session_path}/compact"),
            resume: format!("{session_path}/resume"),
            tool_approval: format!("{session_path}/tool-approval"),
            answer_question: format!("{session_path}/answer-question"),
            resolve_plan: format!("{session_path}/resolve-plan"),
        },
        continuity: SessionHandoffContinuity {
            after,
            high_water_mark,
            replay_endpoint: events_history,
            stream_endpoint: events_sse,
            resync_event: "streamResyncRequired".to_string(),
            deduplicate_by: "sessionId+seq".to_string(),
        },
        web_url: format!("{base_url}/?pane=session&session={encoded_id}"),
        notes: {
            let mut notes = vec![
            "Use Authorization: Bearer <accessToken> for authenticated HTTP requests when requiresToken is true.".to_string(),
            "Use eventsHistory for JSON polling clients and eventsSse for browser EventSource clients.".to_string(),
            "Reconnect with the last real event seq as after; on streamResyncRequired, replay eventsHistory from that cursor before reopening eventsSse.".to_string(),
            ];
            if controller_in_flight {
                notes.push(
                    "Session intake is fenced until its root-controller handoff completes."
                        .to_string(),
                );
            }
            notes
        },
    }
}

fn percent_encode_url_component(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionHandoffPayload {
    pub kind: String,
    pub created_at: String,
    pub session: Value,
    pub daemon: SessionHandoffDaemon,
    pub endpoints: SessionHandoffEndpoints,
    #[serde(default)]
    pub continuity: SessionHandoffContinuity,
    pub web_url: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionHandoffContinuity {
    /// Last event sequence the client has accounted for when this payload was
    /// requested. Fresh clients start at zero; reconnecting clients may pass
    /// `?after=N` to get endpoints bound to their persisted cursor.
    pub after: i64,
    /// Highest event sequence known by daemon for this durable session when
    /// handoff was created.
    pub high_water_mark: i64,
    pub replay_endpoint: String,
    pub stream_endpoint: String,
    /// Terminal SSE control event emitted when contiguous in-memory replay is
    /// unavailable; clients must use replay_endpoint to recover.
    pub resync_event: String,
    /// Stable event identity used to discard overlap across replay/reconnect.
    pub deduplicate_by: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SessionHandoffQuery {
    after: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionHandoffDaemon {
    pub base_url: String,
    pub requires_token: bool,
    pub token_header: String,
    pub token_scheme: String,
    pub access_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionHandoffEndpoints {
    pub session: String,
    pub transcript: String,
    pub events_sse: String,
    pub events_history: String,
    pub messages: String,
    pub steer: String,
    pub interrupt: String,
    pub compact: String,
    pub resume: String,
    pub tool_approval: String,
    pub answer_question: String,
    pub resolve_plan: String,
}

fn redact_handoff_payload_for_display(mut payload: SessionHandoffPayload) -> SessionHandoffPayload {
    if payload.daemon.access_token.is_some() {
        payload.daemon.access_token = Some("[redacted]".to_string());
    }
    payload
}

fn daemon_base_url(state: &DaemonState) -> String {
    format!("http://127.0.0.1:{}", state.bound_port)
}

pub(crate) fn session_handoff_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "SessionHandoffPayload".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "createdAt", "session", "daemon", "endpoints", "continuity", "webUrl", "notes"],
            "properties": {
                "kind": {"type": "string", "const": "coderight.session_handoff.v1"},
                "createdAt": {"type": "string"},
                "session": {"type": "object", "additionalProperties": true},
                "daemon": {"$ref": "#/components/schemas/SessionHandoffDaemon"},
                "endpoints": {"$ref": "#/components/schemas/SessionHandoffEndpoints"},
                "continuity": {"$ref": "#/components/schemas/SessionHandoffContinuity"},
                "webUrl": {"type": "string"},
                "notes": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SessionHandoffContinuity".to_string(),
        json!({
            "type": "object",
            "required": [
                "after",
                "highWaterMark",
                "replayEndpoint",
                "streamEndpoint",
                "resyncEvent",
                "deduplicateBy"
            ],
            "properties": {
                "after": {"type": "integer", "minimum": 0},
                "highWaterMark": {"type": "integer", "minimum": 0},
                "replayEndpoint": {"type": "string"},
                "streamEndpoint": {"type": "string"},
                "resyncEvent": {"type": "string", "const": "streamResyncRequired"},
                "deduplicateBy": {"type": "string", "const": "sessionId+seq"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SessionHandoffDaemon".to_string(),
        json!({
            "type": "object",
            "required": ["baseUrl", "requiresToken", "tokenHeader", "tokenScheme", "accessToken"],
            "properties": {
                "baseUrl": {"type": "string"},
                "requiresToken": {"type": "boolean"},
                "tokenHeader": {"type": "string"},
                "tokenScheme": {"type": "string"},
                "accessToken": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SessionHandoffEndpoints".to_string(),
        json!({
            "type": "object",
            "required": [
                "session",
                "transcript",
                "eventsSse",
                "eventsHistory",
                "messages",
                "steer",
                "interrupt",
                "compact",
                "resume",
                "toolApproval",
                "answerQuestion",
                "resolvePlan"
            ],
            "properties": {
                "session": {"type": "string"},
                "transcript": {"type": "string"},
                "eventsSse": {"type": "string"},
                "eventsHistory": {"type": "string"},
                "messages": {"type": "string"},
                "steer": {"type": "string"},
                "interrupt": {"type": "string"},
                "compact": {"type": "string"},
                "resume": {"type": "string"},
                "toolApproval": {"type": "string"},
                "answerQuestion": {"type": "string"},
                "resolvePlan": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn session_handoff_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SessionHandoffPayload"})
}

async fn export_session_messages(
    state: &DaemonState,
    session_id: &str,
) -> Result<Vec<TranscriptMessage>, String> {
    if let Some(live) = state.get_session(session_id) {
        return Ok(lock_recover(&live.transcript, "live.transcript").clone());
    }

    let Some(storage) = &state.storage else {
        return Err(format!("Session not found: {session_id}"));
    };
    let storage2 = storage.clone();
    let id = session_id.to_string();
    let events = tokio::task::spawn_blocking(move || {
        let store = storage2.blocking_lock();
        store.all_events(&id)
    })
    .await
    .map_err(|_| format!("Failed to read session events: {session_id}"))?
    .map_err(|_| format!("Session not found: {session_id}"))?;
    if events.is_empty() {
        return Err(format!("Session not found: {session_id}"));
    }
    Ok(crate::rehydrate::rehydrate_transcript(&events))
}

fn render_session_export_markdown(session_id: &str, messages: &[TranscriptMessage]) -> String {
    let visible_count = messages
        .iter()
        .filter(|message| export_visible_message(message).is_some())
        .count();
    let mut out = format!(
        "**Session export:** {session_id}\nFormat: markdown\nMessages: {visible_count} visible of {}\n",
        messages.len()
    );
    for message in messages {
        if let Some(rendered) = export_visible_message(message) {
            out.push('\n');
            out.push_str(&rendered);
        }
    }
    scrub_export_internal_markers(&out)
}

fn export_visible_message(message: &TranscriptMessage) -> Option<String> {
    match message {
        TranscriptMessage::User {
            created_at,
            content,
            ..
        } => {
            let content = clean_export_content(content);
            if content.is_empty() || is_hidden_export_user_message(&content) {
                None
            } else {
                Some(format!("## User\n_Time: {created_at}_\n\n{content}\n"))
            }
        }
        TranscriptMessage::Assistant {
            created_at,
            content,
            ..
        } => {
            let content = clean_export_content(content);
            if content.is_empty() {
                None
            } else {
                Some(format!("## Assistant\n_Time: {created_at}_\n\n{content}\n"))
            }
        }
        TranscriptMessage::System { .. }
        | TranscriptMessage::Tool { .. }
        | TranscriptMessage::Compact { .. }
        | TranscriptMessage::Fold { .. } => None,
    }
}

fn is_hidden_export_user_message(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("<system-reminder>") || trimmed.starts_with("[view_image:")
}

fn clean_export_content(content: &str) -> String {
    scrub_system_reminder_blocks(content)
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("[view_image:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn scrub_export_internal_markers(markdown: &str) -> String {
    clean_export_content(markdown)
}

fn scrub_system_reminder_blocks(content: &str) -> String {
    let mut remaining = content;
    let mut out = String::new();
    loop {
        let Some(start) = remaining.find("<system-reminder>") else {
            out.push_str(remaining);
            break;
        };
        out.push_str(&remaining[..start]);
        let after_start = &remaining[start + "<system-reminder>".len()..];
        let Some(end) = after_start.find("</system-reminder>") else {
            break;
        };
        remaining = &after_start[end + "</system-reminder>".len()..];
    }
    out
}

fn render_session_summary_markdown(session_id: &str, messages: &[TranscriptMessage]) -> String {
    let mut system_count = 0usize;
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut tool_count = 0usize;
    let mut tool_error_count = 0usize;
    let mut compact_count = 0usize;
    let mut last_user: Option<&str> = None;
    let mut last_assistant: Option<&str> = None;
    let mut recent_tools: Vec<String> = Vec::new();

    for message in messages {
        match message {
            TranscriptMessage::System { .. } => system_count += 1,
            TranscriptMessage::User { content, .. } => {
                user_count += 1;
                last_user = Some(content);
            }
            TranscriptMessage::Assistant { content, .. } => {
                assistant_count += 1;
                last_assistant = Some(content);
            }
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                is_error,
                ..
            } => {
                tool_count += 1;
                if *is_error {
                    tool_error_count += 1;
                }
                recent_tools.push(format!(
                    "- `{tool_call_id}` {}: {}",
                    if *is_error { "error" } else { "ok" },
                    excerpt(summary, 120)
                ));
            }
            TranscriptMessage::Compact { .. } => compact_count += 1,
            TranscriptMessage::Fold { .. } => {
                // Fold messages are counted like compactions for summary purposes.
                compact_count += 1;
            }
        }
    }

    let mut out = format!(
        "**Session summary:** {session_id}\nMessages: {}\n\n",
        messages.len()
    );
    out.push_str("Counts:\n");
    out.push_str(&format!("- System: {system_count}\n"));
    out.push_str(&format!("- User: {user_count}\n"));
    out.push_str(&format!("- Assistant: {assistant_count}\n"));
    out.push_str(&format!(
        "- Tool results: {tool_count} ({tool_error_count} error{})\n",
        if tool_error_count == 1 { "" } else { "s" }
    ));
    out.push_str(&format!("- Compactions: {compact_count}\n"));

    if let Some(content) = last_user.map(|content| excerpt(content, 280)) {
        out.push_str(&format!("\nLast user message:\n> {content}\n"));
    }
    if let Some(content) = last_assistant.map(|content| excerpt(content, 280)) {
        out.push_str(&format!("\nLast assistant message:\n> {content}\n"));
    }
    if !recent_tools.is_empty() {
        out.push_str("\nRecent tool results:\n");
        let start = recent_tools.len().saturating_sub(5);
        for row in &recent_tools[start..] {
            out.push_str(row);
            out.push('\n');
        }
    }
    out
}

fn excerpt(input: &str, max_chars: usize) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn command_err(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SessionRuntimeOptions;
    use std::sync::Arc;

    #[tokio::test]
    async fn session_handoff_payload_round_trips_through_typed_contract() {
        let state = DaemonState::new("secret-token").with_runtime_info(61234, "/tmp/cr.db");
        state.create_session_with_options(
            "s1",
            None,
            "/tmp/workspace/.coderight/worktrees/s1",
            "Bridge me",
            Some("parent"),
            SessionRuntimeOptions {
                safe_mode: true,
                forked_from_session_id: Some("parent".to_string()),
                toolset: Some("review".to_string()),
                spec_id: Some("spec-1".to_string()),
                execution_backend: Some("custom-backend".to_string()),
                execution_workspace_id: Some("run-1".to_string()),
                mcp_channel: "cli".to_string(),
                ..SessionRuntimeOptions::default()
            },
        );

        let session = state.get_session("s1").expect("live session");
        let fixed_session_at = "2099-01-02T03:04:05Z";
        *lock_recover(&session.updated_at, "session.updated_at") = fixed_session_at.to_string();
        let mut session_metadata = live_session_metadata(&session);
        session_metadata["createdAt"] = json!(fixed_session_at);
        session_metadata["updatedAt"] = json!(fixed_session_at);
        let mut payload = session_handoff_payload(&state, "s1", session_metadata);
        payload.created_at = "2099-01-02T03:04:06Z".to_string();
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/cli-json/session-handoff.v1.json"
        ))
        .expect("parse handoff fixture");
        let actual = serde_json::to_value(&payload).expect("serialize handoff payload");
        // Keep old fixture stable as a compatibility projection while allowing
        // version-one payloads to carry reconnect metadata.
        let mut legacy = actual.clone();
        legacy["session"]
            .as_object_mut()
            .expect("session object")
            .remove("eventHighWaterMark");
        legacy
            .as_object_mut()
            .expect("handoff object")
            .remove("continuity");
        legacy["notes"] = fixture["notes"].clone();
        assert_eq!(legacy, fixture);
        let raw = serde_json::to_string(&payload).expect("serialize handoff payload");
        let decoded: SessionHandoffPayload =
            serde_json::from_str(&raw).expect("deserialize handoff payload");

        assert_eq!(decoded, payload);
        assert_eq!(decoded.kind, "coderight.session_handoff.v1");
        assert_eq!(decoded.daemon.access_token.as_deref(), Some("secret-token"));
        assert_eq!(decoded.endpoints.messages, "/sessions/s1/messages");
        assert_eq!(
            decoded.endpoints.events_history,
            "/sessions/s1/events/history?after=0"
        );
        assert_eq!(
            decoded.web_url,
            "http://127.0.0.1:61234/?pane=session&session=s1"
        );
        assert_eq!(decoded.session["safeMode"], true);
        assert_eq!(decoded.session["forkedFromSessionId"], "parent");
        assert_eq!(decoded.session["updatedAt"], "2099-01-02T03:04:05Z");
        assert_eq!(decoded.session["toolset"], "review");
        assert_eq!(decoded.session["specId"], "spec-1");
        assert_eq!(decoded.session["executionBackend"], "custom-backend");
        assert_eq!(decoded.session["executionWorkspaceId"], "run-1");
        assert_eq!(decoded.session["mcpChannel"], "cli");
        assert_eq!(decoded.continuity.after, 0);
        assert_eq!(decoded.continuity.high_water_mark, 0);
        assert_eq!(
            decoded.continuity.replay_endpoint,
            "/sessions/s1/events/history?after=0"
        );
        assert_eq!(
            decoded.continuity.stream_endpoint,
            "/sessions/s1/events?after=0"
        );
        assert_eq!(decoded.continuity.resync_event, "streamResyncRequired");
        assert_eq!(decoded.continuity.deduplicate_by, "sessionId+seq");
    }

    #[tokio::test]
    async fn displayed_session_handoff_redaction_preserves_round_trip_shape() {
        let state = DaemonState::new("secret-token").with_runtime_info(61234, "/tmp/cr.db");
        state.create_session("s1", None, "/tmp/workspace", "Bridge me", None);

        let session = state.get_session("s1").expect("live session");
        let payload = session_handoff_payload(&state, "s1", live_session_metadata(&session));
        let displayed = redact_handoff_payload_for_display(payload);
        let raw = serde_json::to_string(&displayed).expect("serialize display payload");
        let decoded: SessionHandoffPayload =
            serde_json::from_str(&raw).expect("deserialize display payload");

        assert_eq!(decoded.daemon.access_token.as_deref(), Some("[redacted]"));
        assert_eq!(
            decoded.endpoints.tool_approval,
            "/sessions/s1/tool-approval"
        );
    }

    #[tokio::test]
    async fn session_handoff_payload_encodes_session_id_urls() {
        let state = DaemonState::new("").with_runtime_info(61234, "/tmp/cr.db");
        let payload = session_handoff_payload(
            &state,
            "review child/1",
            serde_json::json!({ "id": "review child/1" }),
        );

        assert_eq!(payload.endpoints.session, "/sessions/review%20child%2F1");
        assert_eq!(
            payload.endpoints.messages,
            "/sessions/review%20child%2F1/messages"
        );
        assert_eq!(
            payload.web_url,
            "http://127.0.0.1:61234/?pane=session&session=review%20child%2F1"
        );
    }

    #[tokio::test]
    async fn session_handoff_payload_binds_reconnect_cursor_to_both_event_endpoints() {
        let state = DaemonState::new("").with_runtime_info(61234, "/tmp/cr.db");
        let payload = session_handoff_payload_after(
            &state,
            "s1",
            serde_json::json!({
                "id": "s1",
                "eventHighWaterMark": 42,
            }),
            17,
        );

        assert_eq!(payload.continuity.after, 17);
        assert_eq!(payload.continuity.high_water_mark, 42);
        assert_eq!(payload.endpoints.events_sse, "/sessions/s1/events?after=17");
        assert_eq!(
            payload.endpoints.events_history,
            "/sessions/s1/events/history?after=17"
        );
        assert_eq!(
            payload.continuity.replay_endpoint,
            payload.endpoints.events_history
        );
        assert_eq!(
            payload.continuity.stream_endpoint,
            payload.endpoints.events_sse
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dormant_session_handoff_preserves_safe_mode_and_fork_context() {
        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage.clone());
        state.create_session_with_options(
            "s1",
            None,
            "/tmp/workspace",
            "Bridge me",
            Some("parent"),
            SessionRuntimeOptions {
                safe_mode: true,
                forked_from_session_id: Some("parent".to_string()),
                toolset: Some("dev-safe".to_string()),
                spec_id: Some("spec-2".to_string()),
                execution_backend: Some("custom-backend".to_string()),
                execution_workspace_id: Some("run-2".to_string()),
                mcp_channel: "desktop".to_string(),
                allowed_tools: vec!["read_file".to_string()],
                disallowed_tools: vec!["shell".to_string()],
                effort: Some("high".to_string()),
                ..SessionRuntimeOptions::default()
            },
        );
        // Session rows persist fire-and-forget (`create_session_seeded_with_options`
        // spawns the durable write), so dropping the live entry immediately races
        // that task and the dormant lookup 404s. Wait for the row first.
        let mut persisted = false;
        for _ in 0..200 {
            if storage.lock().await.get_session("s1").unwrap().is_some() {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(persisted, "session `s1` was not persisted within 1s");
        lock_recover(&state.sessions, "state.sessions").remove("s1");

        let session = session_handoff_metadata(&state, "s1").await.unwrap();

        assert_eq!(session["safeMode"], true);
        assert_eq!(session["forkedFromSessionId"], "parent");
        assert_eq!(session["toolset"], "dev-safe");
        assert_eq!(session["specId"], "spec-2");
        assert_eq!(session["executionBackend"], "custom-backend");
        assert_eq!(session["executionWorkspaceId"], "run-2");
        assert_eq!(session["mcpChannel"], "desktop");
        assert_eq!(session["allowedTools"], serde_json::json!(["read_file"]));
        assert_eq!(session["disallowedTools"], serde_json::json!(["shell"]));
        assert_eq!(session["effort"], "high");
        assert_eq!(session["eventHighWaterMark"], 0);
    }
}
