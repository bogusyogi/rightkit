//! Typed error body for workflow API surface (WP-09 / ADR-0021).
#![forbid(unsafe_code)]

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rightkit_agent_engine_storage::StorageError;
use serde_json::{json, Value};

/// Stable machine-readable error codes.
pub mod code {
    pub const FEATURE_LOCKED: &str = "workflows_feature_locked";
    pub const NOT_FOUND: &str = "workflows_not_found";
    pub const BAD_REQUEST: &str = "workflows_bad_request";
    pub const CONFLICT: &str = "workflows_conflict";
    pub const STORAGE: &str = "workflows_storage_error";
    pub const INTERNAL: &str = "workflows_internal_error";
    pub const SERVICE_UNAVAILABLE: &str = "workflows_service_unavailable";
    pub const IMPORT_DISABLED: &str = "workflows_import_disabled";
}

/// Typed error carrying status + OpenAPI `ApiError` body.
#[derive(Debug, Clone)]
pub struct WorkflowApiError {
    status: StatusCode,
    body: Value,
}

impl std::fmt::Display for WorkflowApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("workflow API error")
        )
    }
}

impl WorkflowApiError {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({
                "error": message.into(),
                "code": code,
                "requestId": Value::Null,
            }),
        }
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        if let Some(rid) = request_id {
            if let Some(obj) = self.body.as_object_mut() {
                obj.insert("requestId".into(), Value::String(rid));
            }
        }
        self
    }

    pub fn feature_locked() -> Self {
        Self::new(StatusCode::LOCKED, code::FEATURE_LOCKED, "autonomous workflow control plane is disabled (set `features.workflows_enabled = true` in config to enable)")
    }

    pub fn import_disabled() -> Self {
        Self::new(
            StatusCode::LOCKED,
            code::IMPORT_DISABLED,
            "workflow import is disabled (the `imports_allowed` capability must be enabled)",
        )
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            code::SERVICE_UNAVAILABLE,
            message,
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code::NOT_FOUND, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code::BAD_REQUEST, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code::CONFLICT, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code::INTERNAL, message)
    }

    pub fn from_storage(err: StorageError) -> Self {
        let msg = err.to_string();
        if msg.contains("UNIQUE constraint failed") || msg.contains("FOREIGN KEY constraint failed")
        {
            Self::conflict(format!("storage constraint violated: {msg}"))
        } else {
            Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                code::STORAGE,
                format!("storage error: {msg}"),
            )
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn body(&self) -> &Value {
        &self.body
    }
}

impl IntoResponse for WorkflowApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<StorageError> for WorkflowApiError {
    fn from(err: StorageError) -> Self {
        Self::from_storage(err)
    }
}
