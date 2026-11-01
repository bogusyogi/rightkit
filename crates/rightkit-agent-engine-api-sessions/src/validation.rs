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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_bounds() {
        assert!(validate_session_id("abc").is_ok());
        assert_eq!(validate_session_id("   "), Err(StatusCode::BAD_REQUEST));
        assert!(validate_session_id(&"a".repeat(128)).is_ok());
        assert_eq!(
            validate_session_id(&"a".repeat(129)),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn workspace_root_and_tag_bounds() {
        assert!(validate_workspace_root("/work").is_ok());
        assert!(validate_workspace_root("").is_err());
        assert!(validate_workspace_root(&"p".repeat(1025)).is_err());
        assert!(validate_tag("release").is_ok());
        assert!(validate_tag(" ").is_err());
        assert!(validate_tag(&"t".repeat(65)).is_err());
    }

    #[test]
    fn fork_id_is_optional_but_bounded() {
        assert!(validate_fork_id(None).is_ok());
        assert!(validate_fork_id(Some("s1")).is_ok());
        assert!(validate_fork_id(Some("")).is_err());
        assert!(validate_fork_id(Some(&"x".repeat(129))).is_err());
    }
}
