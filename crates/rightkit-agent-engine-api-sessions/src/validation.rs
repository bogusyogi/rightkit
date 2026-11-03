//! Bounded validation for session lifecycle routes.

use axum::http::StatusCode;

/// Validate a session id is non-empty and within bounded length.
pub fn validate_session_id(id: &str) -> Result<(), StatusCode> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Validate workspace_root is non-empty and looks like a path.
pub fn validate_workspace_root(root: &str) -> Result<(), StatusCode> {
    let trimmed = root.trim();
    if trimmed.is_empty() || trimmed.len() > 1024 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Validate a tag string.
pub fn validate_tag(tag: &str) -> Result<(), StatusCode> {
    if tag.trim().is_empty() || tag.len() > 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Validate fork request fields.
pub fn validate_fork_id(id: Option<&str>) -> Result<(), StatusCode> {
    if let Some(id) = id {
        if id.trim().is_empty() || id.len() > 128 {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}
