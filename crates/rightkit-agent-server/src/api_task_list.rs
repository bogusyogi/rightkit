//! HTTP surface for PL-006 — lightweight task-list projection over the
//! canonical TaskGraph (`rightkit_agent_orchestration::coding::task_list_projection`).
//!
//! Route-function shape matches `api_teams.rs::peer_team_routes()` exactly
//! (per the lane packet): a `pub fn ..._routes() -> Router<DaemonState>`
//! that this module does not mount anywhere itself. No route is merged
//! into the daemon's `Router`, and no `pub mod api_task_list;` line exists
//! in `main_runtime_server.rs` yet — wiring those hub files is
//! orchestrator-owned; see
//! `docs/canon/daemon-master-canon-atoms.md`
//! for the exact lines to add.
//!
//! Tasks are per-session: the canonical flat list lives on
//! `LiveSession::tasks` (`Arc<std::sync::Mutex<Vec<TaskNode>>>`,
//! `engine/src/api_state_types.rs`), populated from
//! `TaskGraphUpdated` events — not on `DaemonState`. Every route therefore
//! carries the session id in its path; each handler resolves the session
//! via `DaemonState::get_session`, returns `404 NOT_FOUND` for an unknown
//! session (matching `api_sessions/orchestration.rs::get_session_agents`),
//! then locks that session's `tasks` and calls straight through to
//! `task_list_projection`'s functions, which build a canonical `TaskGraph`
//! from that list, mutate it via `TaskGraph`'s own methods, and write the
//! flattened result back. No new persistence is introduced by this file.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_orchestration::coding::task_list_projection::{
    self, CreateTaskInput, ProjectionError, TaskPatch,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::DaemonState;

/// Matches the `now_iso()` convention used elsewhere in this crate
/// (`api_attention.rs`, `api_delivery.rs`, `api_environments.rs`) rather
/// than introducing a `chrono` dependency this crate does not otherwise
/// have.
fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

fn projection_error_status(err: &ProjectionError) -> StatusCode {
    match err {
        ProjectionError::NotFound(_) => StatusCode::NOT_FOUND,
        ProjectionError::ReorderTargetNotFound(_) => StatusCode::NOT_FOUND,
        ProjectionError::Graph(rightkit_agent_engine_tasks::TaskGraphError::DuplicateId(_)) => {
            StatusCode::CONFLICT
        }
        ProjectionError::Graph(_) => StatusCode::BAD_REQUEST,
    }
}

pub fn task_list_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/sessions/{session_id}/tasks",
            get(list_handler).post(create_handler),
        )
        .route("/sessions/{session_id}/tasks/ready", get(ready_handler))
        .route("/sessions/{session_id}/tasks/blocked", get(blocked_handler))
        .route(
            "/sessions/{session_id}/tasks/{id}",
            get(get_handler).patch(patch_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/active",
            post(mark_active_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/done",
            post(mark_done_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/failed",
            post(mark_failed_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/cancel",
            post(cancel_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/owner",
            post(assign_owner_handler),
        )
        .route(
            "/sessions/{session_id}/tasks/{id}/reorder",
            post(reorder_handler),
        )
}

async fn list_handler(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let tasks = live.tasks.lock().unwrap().clone();
    Ok(Json(json!({ "tasks": tasks })))
}

async fn get_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let tasks = live.tasks.lock().unwrap();
    let index = task_list_projection::index_by_id(&tasks);
    let node = index.get(id.as_str()).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "task": node })))
}

async fn ready_handler(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let tasks = live.tasks.lock().unwrap();
    let ready = task_list_projection::ready_listing(&tasks)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "tasks": ready })))
}

async fn blocked_handler(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let tasks = live.tasks.lock().unwrap();
    let blocked = task_list_projection::blocked_listing(&tasks)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "tasks": blocked })))
}

#[derive(Debug, Deserialize)]
struct CreateTaskBody {
    id: String,
    title: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default, rename = "doneWhen")]
    done_when: String,
    #[serde(default)]
    category: String,
    #[serde(default, rename = "estimatedDuration")]
    estimated_duration: String,
    #[serde(default, rename = "assignedRole")]
    assigned_role: String,
    #[serde(default, rename = "escalationPath")]
    escalation_path: String,
}

async fn create_handler(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    Json(body): Json<CreateTaskBody>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let created = task_list_projection::create_task(
        &mut tasks,
        CreateTaskInput {
            id: body.id,
            title: body.title,
            deps: body.deps,
            done_when: body.done_when,
            category: body.category,
            estimated_duration: body.estimated_duration,
            assigned_role: body.assigned_role,
            escalation_path: body.escalation_path,
            now: now_iso(),
        },
    )
    .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": created })))
}

#[derive(Debug, Deserialize, Default)]
struct PatchTaskBody {
    title: Option<String>,
    category: Option<String>,
    #[serde(default, rename = "estimatedDuration")]
    estimated_duration: Option<String>,
    #[serde(default, rename = "escalationPath")]
    escalation_path: Option<String>,
    #[serde(default, rename = "openQuestions")]
    open_questions: Option<Vec<String>>,
}

async fn patch_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
    Json(body): Json<PatchTaskBody>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let updated = task_list_projection::update_task(
        &mut tasks,
        &id,
        TaskPatch {
            title: body.title,
            category: body.category,
            estimated_duration: body.estimated_duration,
            escalation_path: body.escalation_path,
            open_questions: body.open_questions,
            now: now_iso(),
        },
    )
    .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": updated })))
}

async fn mark_active_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let updated = task_list_projection::mark_active(&mut tasks, &id, now_iso())
        .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": updated })))
}

async fn mark_done_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let updated = task_list_projection::mark_done(&mut tasks, &id, now_iso())
        .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": updated })))
}

async fn mark_failed_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let updated = task_list_projection::mark_failed(&mut tasks, &id, now_iso())
        .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": updated })))
}

async fn cancel_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap_or_else(|error| error.into_inner());
    let cancelled = task_list_projection::mark_cancelled(&mut tasks, &id, now_iso())
        .map_err(|error| projection_error_status(&error))?;
    let snapshot = tasks.clone();
    drop(tasks);
    let seq = live.conduit.next_seq();
    live.conduit.publish(
        rightkit_agent_engine_protocol::ProtocolEvent::TaskGraphUpdated {
            session_id: session_id.clone(),
            seq,
            created_at: now_iso(),
            summary: format!("Task {} cancelled by user", cancelled.id),
            tasks: snapshot,
        },
    );
    let seq = live.conduit.next_seq();
    live.conduit.publish(
        rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
            session_id,
            seq,
            created_at: now_iso(),
            text: format!(
                "Cancelled task {} from the TUI Running panel.",
                cancelled.id
            ),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        },
    );
    Ok(Json(json!({ "task": cancelled })))
}

#[derive(Debug, Deserialize)]
struct AssignOwnerBody {
    owner: String,
}

async fn assign_owner_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
    Json(body): Json<AssignOwnerBody>,
) -> Result<Json<Value>, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    let updated = task_list_projection::assign_owner(&mut tasks, &id, body.owner, now_iso())
        .map_err(|e| projection_error_status(&e))?;
    Ok(Json(json!({ "task": updated })))
}

#[derive(Debug, Deserialize, Default)]
struct ReorderBody {
    #[serde(default, rename = "beforeId")]
    before_id: Option<String>,
}

async fn reorder_handler(
    State(state): State<DaemonState>,
    Path((session_id, id)): Path<(String, String)>,
    Json(body): Json<ReorderBody>,
) -> Result<StatusCode, StatusCode> {
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tasks = live.tasks.lock().unwrap();
    task_list_projection::reorder(&mut tasks, &id, body.before_id.as_deref())
        .map_err(|e| projection_error_status(&e))?;
    Ok(StatusCode::NO_CONTENT)
}
