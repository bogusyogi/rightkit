#[derive(Debug)]
struct RecipeApiError {
    status: StatusCode,
    body: serde_json::Value,
}

impl RecipeApiError {
    fn bad_request(body: serde_json::Value) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: serde_json::json!({ "error": message.into() }),
        }
    }

    fn server_error(body: serde_json::Value) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body,
        }
    }
}

fn enforce_recipe_trusted_source(headers: &HeaderMap) -> Result<(), RecipeApiError> {
    enforce_trusted_source_authority(headers).map_err(|status| RecipeApiError {
        status,
        body: serde_json::json!({
            "error": "lower-trust source cannot install, run, or schedule recipes"
        }),
    })
}

impl IntoResponse for RecipeApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(self.body)).into_response()
    }
}
