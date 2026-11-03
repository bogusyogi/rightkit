//! Authenticated, session-scoped recoverable context-fold control surface.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::path::{Path as FsPath, PathBuf};

use crate::api::DaemonState;

const MAX_SUBTASK_ID_BYTES: usize = 128;
const MAX_MARKER_BYTES: usize = 512;
const MAX_SUMMARY_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub struct ContextFoldCommand {
    pub subtask_id: String,
    pub start_marker: String,
    pub end_marker: String,
    pub summary: String,
    pub response: tokio::sync::oneshot::Sender<
        Result<
            rightkit_agent_engine_core::FoldedTrajectory,
            rightkit_agent_engine_core::ContextFoldError,
        >,
    >,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FoldRequest {
    subtask_id: String,
    start_marker: String,
    end_marker: String,
    summary: String,
}

pub(crate) fn context_fold_routes() -> Router<DaemonState> {
    Router::new()
        .route("/sessions/{id}/context-fold", post(fold_context))
        .route(
            "/sessions/{id}/context-folds/{archive_ref}",
            get(get_context_fold),
        )
}

pub(crate) fn session_fold_archive_dir(
    database_path: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    let database = FsPath::new(database_path);
    let parent = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "context-fold archive root is unavailable".to_string())?;
    Ok(rightkit_agent_engine_core::session_archive_dir(
        &parent.join("context-folds"),
        session_id,
    ))
}

fn validate_request(body: &FoldRequest) -> Result<(), StatusCode> {
    let valid = |value: &str, limit: usize| !value.trim().is_empty() && value.len() <= limit;
    if valid(&body.subtask_id, MAX_SUBTASK_ID_BYTES)
        && valid(&body.start_marker, MAX_MARKER_BYTES)
        && valid(&body.end_marker, MAX_MARKER_BYTES)
        && valid(&body.summary, MAX_SUMMARY_BYTES)
        && body.start_marker != body.end_marker
    {
        Ok(())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

async fn fold_context(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(body): Json<FoldRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    validate_request(&body)?;
    let session = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let tx = session
        .context_fold_tx
        .as_ref()
        .ok_or(StatusCode::CONFLICT)?;
    let (response, receiver) = tokio::sync::oneshot::channel();
    tx.send(ContextFoldCommand {
        subtask_id: body.subtask_id,
        start_marker: body.start_marker,
        end_marker: body.end_marker,
        summary: body.summary,
        response,
    })
    .await
    .map_err(|_| StatusCode::CONFLICT)?;

    let folded = tokio::time::timeout(std::time::Duration::from_secs(30), receiver)
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        .map_err(|_| StatusCode::CONFLICT)?
        .map_err(map_fold_error)?;
    Ok(Json(serde_json::json!({
        "archiveRef": folded.ref_id,
        "droppedCount": folded.dropped_count,
        "summary": folded.summary,
    })))
}

async fn get_context_fold(
    State(state): State<DaemonState>,
    Path((id, archive_ref)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Session existence is checked before any archive read. A caller cannot use
    // this route as an oracle for deleted or foreign-session archive content.
    let exists_live = state.get_session(&id).is_some();
    let exists_durable = if exists_live {
        true
    } else if let Some(storage) = &state.storage {
        let storage = storage.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            storage
                .blocking_lock()
                .get_session(&id)
                .ok()
                .flatten()
                .is_some()
        })
        .await
        .unwrap_or(false)
    } else {
        false
    };
    if !exists_durable {
        return Err(StatusCode::NOT_FOUND);
    }

    let dir = session_fold_archive_dir(&state.database_path, &id)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let archive_ref_for_read = archive_ref.clone();
    let messages = tokio::task::spawn_blocking(move || {
        rightkit_agent_engine_core::read_folded_trajectory(&dir, &archive_ref_for_read)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(map_fold_error)?;
    Ok(Json(serde_json::json!({
        "archiveRef": archive_ref,
        "messages": messages,
    })))
}

fn map_fold_error(error: rightkit_agent_engine_core::ContextFoldError) -> StatusCode {
    use rightkit_agent_engine_core::ContextFoldError;
    match error {
        ContextFoldError::InvalidReference => StatusCode::BAD_REQUEST,
        ContextFoldError::MarkersNotFound | ContextFoldError::StablePrefix => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ContextFoldError::TooManyMessages
        | ContextFoldError::ArchiveTooLarge
        | ContextFoldError::ArchiveCapacity => StatusCode::PAYLOAD_TOO_LARGE,
        ContextFoldError::ArchiveUnavailable => StatusCode::NOT_FOUND,
        ContextFoldError::Disabled => StatusCode::CONFLICT,
        ContextFoldError::Io(_) | ContextFoldError::Serialization(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub(crate) fn fold_request_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["subtaskId", "startMarker", "endMarker", "summary"],
        "additionalProperties": false,
        "properties": {
            "subtaskId": { "type": "string", "minLength": 1, "maxLength": MAX_SUBTASK_ID_BYTES },
            "startMarker": { "type": "string", "minLength": 1, "maxLength": MAX_MARKER_BYTES },
            "endMarker": { "type": "string", "minLength": 1, "maxLength": MAX_MARKER_BYTES },
            "summary": { "type": "string", "minLength": 1, "maxLength": MAX_SUMMARY_BYTES }
        }
    })
}

pub(crate) fn fold_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["archiveRef", "droppedCount", "summary"],
        "properties": {
            "archiveRef": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
            "droppedCount": { "type": "integer", "minimum": 1 },
            "summary": { "type": "string" }
        }
    })
}

pub(crate) fn fold_archive_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["archiveRef", "messages"],
        "properties": {
            "archiveRef": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
            "messages": { "type": "array", "maxItems": rightkit_agent_engine_core::context_fold::MAX_FOLD_MESSAGES }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bounds_fail_closed() {
        let request = FoldRequest {
            subtask_id: "task".into(),
            start_marker: "same".into(),
            end_marker: "same".into(),
            summary: "done".into(),
        };
        assert_eq!(validate_request(&request), Err(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn archive_path_hashes_session_identity() {
        let path = session_fold_archive_dir("/tmp/coderight/data.db", "../foreign").unwrap();
        assert_eq!(
            path.parent().unwrap(),
            FsPath::new("/tmp/coderight/context-folds")
        );
        assert_eq!(path.file_name().unwrap().to_string_lossy().len(), 64);
    }
}
