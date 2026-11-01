//! Authenticated loopback transport for one restricted background semantic
//! provider call (daemon-local HTTP; no Membrane residency or holder is involved). Global daemon middleware enforces bearer authentication;
//! `main_runtime_server` binds this router only to `127.0.0.1`.
//!
//! This handler performs no event discovery, scheduling, proposal admission,
//! durable write, or cursor advancement.

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Serialize;

use crate::api::DaemonState;
use crate::background_provider::{provider_unavailable_result, ConfiguredBackgroundProvider};
use crate::membrane_binding::BackgroundSemanticReviewRequestV1;

const BACKGROUND_REVIEW_BODY_LIMIT: usize = 512 * 1024;

pub fn membrane_background_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/runtime/membrane/background-review",
            post(background_review_handler),
        )
        .layer(DefaultBodyLimit::max(BACKGROUND_REVIEW_BODY_LIMIT))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundReviewApiErrorV1 {
    schema_version: u32,
    code: &'static str,
    message: String,
}

impl BackgroundReviewApiErrorV1 {
    fn invalid_request(message: String) -> Self {
        Self {
            schema_version: 1,
            code: "invalid_background_semantic_review_request",
            message,
        }
    }

    fn execution_failed(message: String) -> Self {
        Self {
            schema_version: 1,
            code: "background_semantic_review_execution_failed",
            message,
        }
    }
}

async fn background_review_handler(
    State(state): State<DaemonState>,
    Json(request): Json<BackgroundSemanticReviewRequestV1>,
) -> Response {
    if let Err(error) = request.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(BackgroundReviewApiErrorV1::invalid_request(
                error.to_string(),
            )),
        )
            .into_response();
    }

    let Some(config) = state.engine_cfg.as_ref() else {
        return (StatusCode::OK, Json(provider_unavailable_result(&request))).into_response();
    };
    let provider = ConfiguredBackgroundProvider::from_engine_config(config);
    match provider.execute(&request).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(BackgroundReviewApiErrorV1::execution_failed(
                error.to_string(),
            )),
        )
            .into_response(),
    }
}
