//! Session router — merges all session sub-routers.
//! This is the bounded router hub for `api_sessions/**`.

use axum::{
    body::Body,
    http::{header, Request},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};

use crate::api::DaemonState;

use super::approvals::{answer_question, compact_session, resolve_plan, resolve_tool_approval};
use super::credentials::resolve_credential_request;
use super::elicitation::resolve_elicitation;
use super::export::export_session_handler;
use super::lifecycle::{
    clone_session_handler, create_session_handler, delete_session_handler, fork_session_handler,
    get_away_summary, get_transcript, get_untrusted_content, interrupt_session, kill_session,
    list_sessions, patch_session, resume_session_handler, search_sessions_handler,
};
use super::messages::{
    clear_goal, edit_goal, get_goal_snapshot, pause_goal, post_message, resume_goal,
    rewind_points_handler, rewind_session, set_goal, steer_session,
};
use super::orchestration::{
    bind_session_spec_handler, clear_session_spec_handler, get_artifacts, get_session_agents,
    get_session_spec_completion_handler, get_taskgraph, get_tasks, orchestrate_session,
};
use super::sse::{get_events_history, get_events_replay_page, get_events_sse};

pub(crate) fn sessions_routes() -> Router<DaemonState> {
    Router::new()
        // lifecycle
        .route("/sessions", get(list_sessions).post(create_session_handler))
        .route("/sessions/search", get(search_sessions_handler))
        .route(
            "/sessions/{id}",
            delete(delete_session_handler).patch(patch_session),
        )
        .route("/sessions/{id}/kill", post(kill_session))
        .route("/sessions/{id}/transcript", get(get_transcript))
        .route("/sessions/{id}/export", get(export_session_handler))
        .route(
            "/sessions/{id}/untrusted-content",
            get(get_untrusted_content),
        )
        .route("/sessions/{id}/away-summary", get(get_away_summary))
        .route("/sessions/{id}/resume", post(resume_session_handler))
        .route("/sessions/{id}/fork", post(fork_session_handler))
        .route("/sessions/{id}/clone", post(clone_session_handler))
        .route("/sessions/{id}/interrupt", post(interrupt_session))
        // messages + steering
        .route("/sessions/{id}/messages", post(post_message))
        .route("/sessions/{id}/steer", post(steer_session))
        .route(
            "/sessions/{id}/goal",
            get(get_goal_snapshot).post(set_goal).delete(clear_goal),
        )
        .route("/sessions/{id}/goal/pause", post(pause_goal))
        .route("/sessions/{id}/goal/resume", post(resume_goal))
        .route("/sessions/{id}/goal/edit", post(edit_goal))
        .route("/sessions/{id}/rewind", post(rewind_session))
        .route("/sessions/{id}/rewind/points", get(rewind_points_handler))
        // SSE stream
        .route("/sessions/{id}/events", get(get_events_sse))
        .route("/sessions/{id}/events/history", get(get_events_history))
        .route("/sessions/{id}/events/replay", get(get_events_replay_page))
        // approvals / compaction
        .route("/sessions/{id}/compact", post(compact_session))
        .route("/sessions/{id}/tool-approval", post(resolve_tool_approval))
        .route("/sessions/{id}/answer-question", post(answer_question))
        .route(
            "/sessions/{id}/resolve-elicitation",
            post(resolve_elicitation),
        )
        .route(
            "/sessions/{id}/credential-request",
            post(resolve_credential_request),
        )
        .route("/sessions/{id}/resolve-plan", post(resolve_plan))
        // specs / orchestration / taskgraph
        .route(
            "/sessions/{id}/spec",
            post(bind_session_spec_handler).delete(clear_session_spec_handler),
        )
        .route(
            "/sessions/{id}/spec/completion",
            get(get_session_spec_completion_handler),
        )
        .route("/sessions/{id}/orchestrate", post(orchestrate_session))
        .route("/sessions/{id}/runtime/artifacts", get(get_artifacts))
        .route("/sessions/{id}/runtime/taskgraph", get(get_taskgraph))
        .route("/sessions/{id}/agents", get(get_session_agents))
        .route("/tasks", get(get_tasks))
        .layer(middleware::from_fn(normalize_session_error_envelope))
}

/// Convert legacy bare-status failures at the session boundary into the same
/// versioned machine envelope used by new handlers. Typed JSON errors pass
/// through unchanged, allowing incremental handler migration without forcing
/// machine clients to scrape endpoint-specific strings.
async fn normalize_session_error_envelope(request: Request<Body>, next: Next) -> Response {
    let mutation = !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    let response = next.run(request).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    if response.headers().contains_key(header::CONTENT_TYPE) {
        return response;
    }
    let status = response.status();
    let code = match status {
        axum::http::StatusCode::BAD_REQUEST => "invalid_request",
        axum::http::StatusCode::UNAUTHORIZED => "unauthorized",
        axum::http::StatusCode::FORBIDDEN => "forbidden",
        axum::http::StatusCode::NOT_FOUND => "resource_not_found",
        axum::http::StatusCode::CONFLICT => "conflict",
        axum::http::StatusCode::TOO_MANY_REQUESTS => "rate_limited",
        axum::http::StatusCode::SERVICE_UNAVAILABLE => "service_unavailable",
        _ if status.is_server_error() => "internal_error",
        _ => "request_failed",
    };
    let retryable = status.is_server_error()
        || matches!(
            status,
            axum::http::StatusCode::TOO_MANY_REQUESTS | axum::http::StatusCode::REQUEST_TIMEOUT
        );
    let effect_state = if mutation && status.is_server_error() {
        "unknown"
    } else {
        "none"
    };
    (
        status,
        axum::Json(serde_json::json!({
            "schemaVersion": crate::host_identity::schema("error.v1"),
            "code": code,
            "detail": status.canonical_reason().unwrap_or("request failed"),
            "correlationId": rightkit_agent_engine_protocol::uuid_v7(),
            "retryable": retryable,
            "effectState": effect_state,
            "remediation": serde_json::Value::Null,
        })),
    )
        .into_response()
}
