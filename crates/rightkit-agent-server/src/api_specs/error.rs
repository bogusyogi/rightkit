#[derive(Debug)]
struct SpecError {
    status: StatusCode,
    message: String,
}

impl SpecError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal_error(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

fn enforce_spec_trusted_source_authority(headers: &HeaderMap) -> Result<(), SpecError> {
    enforce_trusted_source_authority(headers).map_err(|status| SpecError {
        status,
        message: "source is not authorized for spec control-plane mutations".to_string(),
    })
}

impl IntoResponse for SpecError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}
