//! `POST /sessions/import`: create one dormant session from chat messages another
//! tool recorded (the CLI's `import claude-code`).
//!
//! Only the text of each turn is imported: a user message becomes a
//! `userMessageSubmitted` event and an assistant message an
//! `assistantMessageCompleted` event, the same replay facts a fork copies. Nothing
//! is dispatched and no model is called, so an import never spends. The session is
//! dormant; its first message resumes it from the imported transcript.
//!
//! The call is the owner's (`enforce_trusted_source_authority`) and the workspace
//! must be trusted: reading another tool's chats into a workspace is the trust
//! gate's business (the CLI checks it before it reads a file, the daemon again
//! here). The route is on neither the guest nor the LAN allowlist: a guest link is
//! bound to one live session and `/sessions/import` names none.

use axum::{extract::State, http::HeaderMap, http::StatusCode, response::Json};
use rightkit_agent_engine_protocol::{now_iso, ProtocolEvent};
use serde::Deserialize;

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

/// Most messages one import carries; a longer chat is refused, not cut.
pub(crate) const MAX_IMPORT_MESSAGES: usize = 5000;
/// Most characters one imported message keeps.
pub(crate) const MAX_IMPORT_MESSAGE_CHARS: usize = 200_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportSessionRequest {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    /// Where the chat came from (`claude-code`), kept in the session's config.
    #[serde(default)]
    pub source: Option<String>,
    pub messages: Vec<ImportMessage>,
}

/// A timestamp is kept only when it looks like RFC 3339 UTC (`2026-11-01T10:00:00Z`).
fn usable_timestamp(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    let bytes = raw.as_bytes();
    let shaped = raw.len() >= 20
        && raw.len() <= 40
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && raw.ends_with('Z')
        && raw
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z' | '.'));
    shaped.then(|| raw.to_string())
}

/// The replay events for the messages, `seq` starting at `first_seq`. A message with an
/// unknown role or no text is dropped; text over the cap is cut with a marker.
pub(crate) fn import_events(
    session_id: &str,
    messages: &[ImportMessage],
    first_seq: i64,
    fallback_ts: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seq = first_seq;
    for message in messages {
        let text: String = message
            .text
            .chars()
            .take(MAX_IMPORT_MESSAGE_CHARS)
            .collect();
        let text = if message.text.chars().count() > MAX_IMPORT_MESSAGE_CHARS {
            format!("{text}\n[imported text cut at {MAX_IMPORT_MESSAGE_CHARS} characters]")
        } else {
            text
        };
        if text.trim().is_empty() {
            continue;
        }
        let created_at = usable_timestamp(message.timestamp.as_deref())
            .unwrap_or_else(|| fallback_ts.to_string());
        let (kind, value) = match message.role.as_str() {
            "user" => (
                "userMessageSubmitted",
                serde_json::json!({
                    "$kind": "userMessageSubmitted",
                    "sessionId": session_id,
                    "seq": seq,
                    "createdAt": created_at,
                    "text": text,
                }),
            ),
            "assistant" => (
                "assistantMessageCompleted",
                serde_json::json!({
                    "$kind": "assistantMessageCompleted",
                    "sessionId": session_id,
                    "seq": seq,
                    "createdAt": created_at,
                    "message": {
                        "$kind": "assistant",
                        "id": format!("imported-{seq}"),
                        "createdAt": created_at,
                        "content": text,
                        "pinned": false,
                    },
                    "phase": "final",
                }),
            ),
            _ => continue,
        };
        let Ok(mut event) = serde_json::from_value::<ProtocolEvent>(value) else {
            continue;
        };
        event.canonicalize_identity(seq);
        let Ok(payload) = serde_json::to_string(&event) else {
            continue;
        };
        out.push((kind.to_string(), payload));
        seq += 1;
    }
    out
}

pub(crate) async fn import_session_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<ImportSessionRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if !super::lifecycle::is_safe_client_session_id(&body.id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.messages.is_empty() || body.messages.len() > MAX_IMPORT_MESSAGES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let root = std::path::Path::new(body.workspace_root.trim());
    if !root.is_absolute() || !root.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    // The trust gate: no chats are read into a workspace the person has not trusted.
    if state.workspace_trust_for(body.workspace_root.trim())
        != rightkit_agent_policy::WorkspaceTrust::Trusted
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if state.get_session(&body.id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    let storage = state
        .storage
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let id = body.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut store = storage.blocking_lock();
        if store
            .get_session(&body.id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .is_some()
        {
            return Err(StatusCode::CONFLICT);
        }
        let now = now_iso();
        let title = {
            let trimmed = body.title.trim();
            if trimmed.is_empty() {
                "Imported chat".to_string()
            } else {
                trimmed.chars().take(120).collect()
            }
        };
        let config = serde_json::json!({
            "importedFrom": body.source.as_deref().unwrap_or("unknown"),
        });
        let row = rightkit_agent_engine_storage::SessionRow {
            id: body.id.clone(),
            title,
            workspace_root: body.workspace_root.trim().to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
            parent_session_id: None,
            role: None,
            status: "dormant".to_string(),
            config_json: config.to_string(),
            tags: String::new(),
            recovery_marker: None,
        };
        store
            .create_session(&row)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let first_seq = store
            .latest_seq(&body.id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            + 1;
        let events = import_events(&body.id, &body.messages, first_seq, &now);
        let count = events.len();
        for (kind, payload) in &events {
            if store.append_event(&body.id, kind, payload).is_err() {
                let _ = store.delete_session_tree(&body.id);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
        Ok(count)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    Ok(Json(serde_json::json!({ "id": id, "imported": result })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str, timestamp: Option<&str>) -> ImportMessage {
        ImportMessage {
            role: role.to_string(),
            text: text.to_string(),
            timestamp: timestamp.map(str::to_string),
        }
    }

    #[test]
    fn user_and_assistant_text_become_replay_events_in_order() {
        let events = import_events(
            "s1",
            &[
                message("user", "hello", Some("2026-11-01T10:00:00Z")),
                message("assistant", "hi there", None),
            ],
            1,
            "2026-11-13T00:00:00Z",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "userMessageSubmitted");
        assert_eq!(events[1].0, "assistantMessageCompleted");
        let first: serde_json::Value = serde_json::from_str(&events[0].1).unwrap();
        assert_eq!(first["text"], "hello");
        assert_eq!(first["createdAt"], "2026-11-01T10:00:00Z");
        assert_eq!(first["seq"], 1);
        let second: serde_json::Value = serde_json::from_str(&events[1].1).unwrap();
        assert_eq!(second["message"]["content"], "hi there");
        assert_eq!(second["seq"], 2);
        assert_eq!(second["createdAt"], "2026-11-13T00:00:00Z");
    }

    #[test]
    fn unknown_roles_and_blank_text_are_dropped() {
        let events = import_events(
            "s1",
            &[
                message("system", "ignored", None),
                message("user", "   ", None),
                message("user", "kept", None),
            ],
            5,
            "2026-11-13T00:00:00Z",
        );
        assert_eq!(events.len(), 1);
        let only: serde_json::Value = serde_json::from_str(&events[0].1).unwrap();
        assert_eq!(only["seq"], 5);
    }

    #[test]
    fn a_malformed_timestamp_falls_back() {
        assert_eq!(usable_timestamp(Some("yesterday")), None);
        assert_eq!(
            usable_timestamp(Some("2026-11-01T10:00:00.123Z")).as_deref(),
            Some("2026-11-01T10:00:00.123Z")
        );
    }
}
