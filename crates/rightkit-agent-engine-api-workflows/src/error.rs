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

#[cfg(test)]
mod tests {
    use super::*;

    fn body_field<'a>(error: &'a WorkflowApiError, key: &str) -> Option<&'a Value> {
        error.body().get(key)
    }

    #[test]
    fn feature_locked_maps_to_423_with_stable_code() {
        let error = WorkflowApiError::feature_locked();
        assert_eq!(error.status(), StatusCode::LOCKED);
        assert_eq!(
            body_field(&error, "code").and_then(Value::as_str),
            Some(code::FEATURE_LOCKED)
        );
        assert_eq!(body_field(&error, "requestId"), Some(&Value::Null));
    }

    #[test]
    fn import_disabled_maps_to_423_with_import_disabled_code() {
        let error = WorkflowApiError::import_disabled();
        assert_eq!(error.status(), StatusCode::LOCKED);
        assert_eq!(
            body_field(&error, "code").and_then(Value::as_str),
            Some(code::IMPORT_DISABLED)
        );
    }

    #[test]
    fn not_found_bad_request_conflict_internal_map_to_expected_status_and_code() {
        let cases: Vec<(WorkflowApiError, StatusCode, &str)> = vec![
            (
                WorkflowApiError::not_found("missing"),
                StatusCode::NOT_FOUND,
                code::NOT_FOUND,
            ),
            (
                WorkflowApiError::bad_request("bad"),
                StatusCode::BAD_REQUEST,
                code::BAD_REQUEST,
            ),
            (
                WorkflowApiError::conflict("conflict"),
                StatusCode::CONFLICT,
                code::CONFLICT,
            ),
            (
                WorkflowApiError::internal("boom"),
                StatusCode::INTERNAL_SERVER_ERROR,
                code::INTERNAL,
            ),
            (
                WorkflowApiError::service_unavailable("down"),
                StatusCode::SERVICE_UNAVAILABLE,
                code::SERVICE_UNAVAILABLE,
            ),
        ];
        for (error, expected_status, expected_code) in cases {
            assert_eq!(error.status(), expected_status);
            assert_eq!(
                body_field(&error, "code").and_then(Value::as_str),
                Some(expected_code)
            );
        }
    }

    #[test]
    fn with_request_id_sets_body_field_when_some_and_leaves_null_when_none() {
        let error = WorkflowApiError::bad_request("bad").with_request_id(Some("req-1".into()));
        assert_eq!(
            body_field(&error, "requestId").and_then(Value::as_str),
            Some("req-1")
        );
        let error = WorkflowApiError::bad_request("bad").with_request_id(None);
        assert_eq!(body_field(&error, "requestId"), Some(&Value::Null));
    }

    #[test]
    fn display_surfaces_the_error_message() {
        let error = WorkflowApiError::not_found("run wfr_123 not found");
        assert_eq!(error.to_string(), "run wfr_123 not found");
    }

    #[test]
    fn from_storage_maps_unique_constraint_violation_to_conflict() {
        let error = WorkflowApiError::from_storage(StorageError::Serde(
            "UNIQUE constraint failed: workflows.id".into(),
        ));
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_field(&error, "code").and_then(Value::as_str),
            Some(code::CONFLICT)
        );
    }

    #[test]
    fn from_storage_maps_foreign_key_violation_to_conflict() {
        let error = WorkflowApiError::from_storage(StorageError::Serde(
            "FOREIGN KEY constraint failed on workflow_versions".into(),
        ));
        assert_eq!(error.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn from_storage_maps_other_errors_to_internal_storage_error() {
        let error =
            WorkflowApiError::from_storage(StorageError::NotFound("workflow wfd_123".into()));
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body_field(&error, "code").and_then(Value::as_str),
            Some(code::STORAGE)
        );
    }

    #[test]
    fn from_impl_delegates_to_from_storage() {
        let storage_error = StorageError::Serde("UNIQUE constraint failed: x".into());
        let error: WorkflowApiError = storage_error.into();
        assert_eq!(error.status(), StatusCode::CONFLICT);
    }
}
