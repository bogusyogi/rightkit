use crate::api::{lock_recover, DaemonState, LiveSession};
use crate::api_permissions::enforce_trusted_source_authority;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCredentialRequest {
    pub tool_call_id: String,
    pub action: String,
    #[serde(default)]
    pub value: Option<String>,
}

pub(crate) async fn resolve_credential_request(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ResolveCredentialRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let outcome = match body.action.as_str() {
        "supply" => "supplied",
        "decline" | "cancel" => "declined",
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let resume = match body.action.as_str() {
        "supply" => {
            let value = body
                .value
                .filter(|value| !value.trim().is_empty() && value.len() <= 64 * 1024)
                .ok_or(StatusCode::BAD_REQUEST)?;
            rightkit_agent_engine_core::credential_request::CredentialResume::Supplied(
                rightkit_agent_engine_core::credential_request::SecretValue::new(value),
            )
        }
        "decline" | "cancel" => rightkit_agent_engine_core::credential_request::CredentialResume::Declined,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let resolved = lock_recover(
        &live.pending_credential_requests,
        "session.pending_credential_requests",
    )
    .take_and_resolve(&body.tool_call_id, resume);
    if !resolved {
        return Err(StatusCode::NOT_FOUND);
    }
    live.conduit.emit(
        |seq| rightkit_agent_engine_protocol::ProtocolEvent::CredentialResolved {
            session_id: id,
            seq,
            created_at: rightkit_agent_engine_protocol::time::now_iso(),
            tool_call_id: body.tool_call_id,
            outcome: outcome.to_string(),
        },
    );
    Ok(Json(serde_json::json!({ "resolved": true })))
}

#[allow(dead_code)]
pub(crate) fn pending_credentials(
    live: &LiveSession,
) -> Vec<rightkit_agent_engine_core::credential_request::CredentialRequest> {
    lock_recover(
        &live.pending_credential_requests,
        "session.pending_credential_requests",
    )
    .pending()
}
