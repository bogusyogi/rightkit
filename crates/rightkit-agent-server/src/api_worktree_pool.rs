//! Workspace-bound routes over the shared pre-warmed worktree pool (VG-004).
//!
//! The pool is the same per-workspace pool subagent worktrees use
//! (`<workspace>/.{app}/subagent-worktree-pool`), so there is one pool
//! authority per workspace. Each pool is bound to the workspace of the session
//! that created it. Acquisition is owned by a session: the lease records the
//! owner and only an owner-matching release can return the checkout to the
//! warm pool. Readiness lives in the pool's durable registry, so it survives a
//! daemon restart.

use std::path::{Path, PathBuf};

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{governed_workspace_worktree_pool, open_subagent_worktree_pool, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn worktree_pool_routes() -> Router<DaemonState> {
    Router::new()
        .route("/worktrees/pool/status", get(pool_status_handler))
        .route("/worktrees/pool/acquire", post(pool_acquire_handler))
        .route("/worktrees/pool/{id}/release", post(pool_release_handler))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquireRequest {
    session_id: Option<String>,
    copy_dirty: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseRequest {
    session_id: Option<String>,
}

/// Empty bodies are valid for these routes (the contract declares no request
/// schema), so an absent body parses as the all-default request.
fn parse_body<T: serde::de::DeserializeOwned + Default>(body: &Bytes) -> Result<T, StatusCode> {
    if body.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)
}

/// `(session id, workspace root)` for every live and persisted session. Live
/// sessions are in memory only; after a restart the persisted rows are the
/// durable record of which workspaces own pools.
async fn session_workspaces(state: &DaemonState) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = state
        .list_sessions()
        .iter()
        .map(|session| (session.id.clone(), session.workspace_root.clone()))
        .collect();
    if let Some(storage) = state.storage.as_ref() {
        let store = storage.lock().await;
        let rows = store.list_sessions().unwrap_or_default();
        drop(store);
        pairs.extend(rows.into_iter().map(|row| (row.id, row.workspace_root)));
    }
    pairs.retain(|(_, root)| {
        let path = Path::new(root);
        !root.trim().is_empty() && path.is_absolute() && path.is_dir()
    });
    pairs
}

fn workspace_roots(pairs: &[(String, String)]) -> Vec<String> {
    let mut roots: Vec<String> = pairs.iter().map(|(_, root)| root.clone()).collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Fill a workspace's pool to one ready checkout without blocking the caller.
fn warm_in_background(workspace: PathBuf) {
    tokio::spawn(async move {
        match governed_workspace_worktree_pool(&workspace) {
            Ok(pool) => {
                if let Err(error) = pool.fill_async(1).await {
                    tracing::warn!(workspace = %workspace.display(), %error, "worktree pool pre-warm failed");
                }
            }
            Err(error) => {
                tracing::warn!(workspace = %workspace.display(), %error, "worktree pool unavailable for pre-warm");
            }
        }
    });
}

async fn pool_status_handler(State(state): State<DaemonState>) -> Result<Json<Value>, StatusCode> {
    let roots = workspace_roots(&session_workspaces(&state).await);
    let mut total_ready = 0usize;
    let mut workspaces = Vec::with_capacity(roots.len());
    for root in roots {
        let pool = open_subagent_worktree_pool(Path::new(&root))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let ready = pool
            .ready_count()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        total_ready += ready;
        if ready == 0 {
            // VG-004: keep one checkout pre-warmed so acquisition is fast.
            warm_in_background(PathBuf::from(&root));
        }
        workspaces.push(json!({"workspaceRoot": root, "readyCount": ready}));
    }
    Ok(Json(json!({
        "readyCount": total_ready,
        "workspaces": workspaces,
    })))
}

/// Typed failure body, so a refused acquisition says why.
fn pool_error(status: StatusCode, error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": error.to_string() })))
}

async fn pool_acquire_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|status| pool_error(status, "untrusted caller"))?;
    let request: AcquireRequest =
        parse_body(&body).map_err(|status| pool_error(status, "invalid request body"))?;
    let session_id = request
        .session_id
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| pool_error(StatusCode::BAD_REQUEST, "sessionId is required"))?;
    let workspace_root = session_workspaces(&state)
        .await
        .into_iter()
        .find(|(id, _)| *id == session_id)
        .map(|(_, root)| root)
        .ok_or_else(|| pool_error(StatusCode::NOT_FOUND, "session not found"))?;
    let workspace = PathBuf::from(&workspace_root);
    let pool = governed_workspace_worktree_pool(&workspace).map_err(|error| {
        pool_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("worktree pool unavailable: {error}"),
        )
    })?;
    let path = pool
        .acquire_async(request.copy_dirty.unwrap_or(false))
        .await
        .map_err(|error| {
            pool_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("acquire failed: {error}"),
            )
        })?;
    let worktree_id = pool.record_lease(&path, &session_id).map_err(|error| {
        pool_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("lease record failed: {error}"),
        )
    })?;
    let ready = pool.is_warm(&worktree_id).map_err(|error| {
        pool_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("readiness read failed: {error}"),
        )
    })?;
    // Replace the checkout just handed out so the next acquisition is warm.
    warm_in_background(workspace.clone());
    Ok(Json(json!({
        "worktreeId": worktree_id,
        "path": path.to_string_lossy(),
        "sessionId": session_id,
        "workspaceRoot": workspace_root,
        "ready": ready,
    })))
}

async fn pool_release_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(worktree_id): AxumPath<String>,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let request: ReleaseRequest = parse_body(&body)?;
    // Each workspace has its own pool registry, so find the one holding this lease.
    for root in workspace_roots(&session_workspaces(&state).await) {
        let workspace = PathBuf::from(&root);
        let plain_pool = open_subagent_worktree_pool(&workspace)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let Some(lease) = plain_pool
            .lease_of(&worktree_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        else {
            continue;
        };
        if let Some(expected) = request.session_id.as_deref() {
            if expected != lease.owner.as_str() {
                return Err(StatusCode::CONFLICT);
            }
        }
        let pool = governed_workspace_worktree_pool(&workspace)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        pool.release_async(&lease.path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        pool.clear_lease(&worktree_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let ready = pool
            .is_warm(&worktree_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(json!({
            "worktreeId": worktree_id,
            "path": lease.path.to_string_lossy(),
            "sessionId": lease.owner,
            "workspaceRoot": root,
            "ready": ready,
        })));
    }
    Err(StatusCode::NOT_FOUND)
}
