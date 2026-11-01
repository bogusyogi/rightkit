//! HTTP surface for VG-007 durable peer-team coordination
//! (`rightkit_agent_orchestration::teams`): a shared task board, mailbox, and
//! mission log across independent sessions/processes — distinct from the
//! existing `/teams` roster endpoints in `api_coordination.rs`, which manage
//! parent-child subagent team *definitions*, not peer runtime coordination.
//!
//! `api_router::build_router` mounts [`peer_team_routes`] on the daemon's
//! authenticated management router. The runtime module exports this file;
//! therefore board, mailbox, and mission-log operations reach durable shared
//! state rather than remaining an unregistered library surface.
//!
//! Each handler opens a [`TeamCoordinationStore`] scoped to the `team_id` in
//! the request path. The store is cheap to (re)open — SQLite connection plus
//! an idempotent `CREATE TABLE IF NOT EXISTS` migration — so no new
//! `DaemonState` field is required; this keeps the lane's allowlist
//! self-contained and avoids touching `api_state_types.rs` (a hub file).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_orchestration::teams::{TaskBoardStatus, TeamCoordinationStore};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::api::DaemonState;

/// Root directory holding one SQLite file per team:
/// `<root>/<team_id>.sqlite`. Overridable for tests/CI via
/// `RIGHTKIT_PEER_TEAM_STORE_ROOT`.
fn peer_team_store_root() -> PathBuf {
    std::env::var_os("RIGHTKIT_PEER_TEAM_STORE_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            rightkit_app_settings::agent_config::state_root().map(|root| root.join("peer-teams"))
        })
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".coderight")
                .join("peer-teams")
        })
}

fn sanitize_team_id(team_id: &str) -> Result<String, StatusCode> {
    if team_id.is_empty()
        || team_id.len() > 128
        || !team_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(team_id.to_string())
}

fn open_store(team_id: &str) -> Result<TeamCoordinationStore, StatusCode> {
    let safe_id = sanitize_team_id(team_id)?;
    let path = peer_team_store_root().join(format!("{safe_id}.sqlite"));
    TeamCoordinationStore::open(path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn peer_team_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/peer-teams/{team_id}/board",
            get(board_list_handler).post(board_post_handler),
        )
        .route(
            "/peer-teams/{team_id}/board/claim",
            post(board_claim_handler),
        )
        .route(
            "/peer-teams/{team_id}/board/{item_id}/status",
            post(board_status_handler),
        )
        .route(
            "/peer-teams/{team_id}/mailbox",
            get(mailbox_peek_handler).post(mailbox_send_handler),
        )
        .route(
            "/peer-teams/{team_id}/mailbox/{message_id}/ack",
            post(mailbox_ack_handler),
        )
        .route(
            "/peer-teams/{team_id}/mission-log",
            get(missionlog_since_handler).post(missionlog_append_handler),
        )
}

#[derive(Debug, Deserialize)]
struct BoardPostBody {
    title: String,
    #[serde(default)]
    payload: Value,
}

async fn board_post_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Json(body): Json<BoardPostBody>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let item = store
        .board_post(&team_id, &body.title, body.payload)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "item": item })))
}

async fn board_list_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let items = store
        .board_list(&team_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct BoardClaimBody {
    #[serde(rename = "peerId")]
    peer_id: String,
}

async fn board_claim_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Json(body): Json<BoardClaimBody>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let claimed = store
        .board_claim_next(&team_id, &body.peer_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "item": claimed })))
}

#[derive(Debug, Deserialize)]
struct BoardStatusBody {
    status: String,
}

async fn board_status_handler(
    State(_state): State<DaemonState>,
    Path((team_id, item_id)): Path<(String, String)>,
    Json(body): Json<BoardStatusBody>,
) -> Result<StatusCode, StatusCode> {
    let store = open_store(&team_id)?;
    let status = match body.status.as_str() {
        "open" => TaskBoardStatus::Open,
        "claimed" => TaskBoardStatus::Claimed,
        "in_progress" => TaskBoardStatus::InProgress,
        "done" => TaskBoardStatus::Done,
        "abandoned" => TaskBoardStatus::Abandoned,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    store
        .board_set_status(&team_id, &item_id, status)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct MailboxSendBody {
    #[serde(rename = "fromPeer")]
    from_peer: String,
    #[serde(rename = "toPeer", default)]
    to_peer: Option<String>,
    #[serde(default)]
    body: Value,
}

#[derive(Debug, Deserialize)]
struct MailboxAckQuery {
    #[serde(rename = "peerId")]
    peer_id: String,
}

async fn mailbox_send_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Json(req): Json<MailboxSendBody>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let msg = store
        .mailbox_send(&team_id, &req.from_peer, req.to_peer.as_deref(), req.body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "message": msg })))
}

#[derive(Debug, Deserialize)]
struct MailboxPeekQuery {
    #[serde(rename = "peerId")]
    peer_id: String,
}

async fn mailbox_peek_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Query(q): Query<MailboxPeekQuery>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let messages = store
        .mailbox_peek(&team_id, &q.peer_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "messages": messages })))
}

async fn mailbox_ack_handler(
    State(_state): State<DaemonState>,
    Path((team_id, message_id)): Path<(String, String)>,
    Query(q): Query<MailboxAckQuery>,
) -> Result<StatusCode, StatusCode> {
    let store = open_store(&team_id)?;
    store
        .mailbox_ack_for_peer(&team_id, &message_id, &q.peer_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct MissionLogAppendBody {
    #[serde(rename = "peerId")]
    peer_id: String,
    kind: String,
    #[serde(default)]
    detail: Value,
}

async fn missionlog_append_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Json(body): Json<MissionLogAppendBody>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let entry = store
        .missionlog_append(&team_id, &body.peer_id, &body.kind, body.detail)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "entry": entry })))
}

#[derive(Debug, Deserialize)]
struct MissionLogSinceQuery {
    #[serde(rename = "sinceSeq", default)]
    since_seq: i64,
}

async fn missionlog_since_handler(
    State(_state): State<DaemonState>,
    Path(team_id): Path<String>,
    Query(q): Query<MissionLogSinceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let store = open_store(&team_id)?;
    let entries = store
        .missionlog_since(&team_id, q.since_seq)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "entries": entries })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_team_id_rejects_path_traversal() {
        assert!(sanitize_team_id("../etc/passwd").is_err());
        assert!(sanitize_team_id("team a").is_err());
        assert!(sanitize_team_id("").is_err());
        assert!(sanitize_team_id("team-a_1").is_ok());
    }
}
