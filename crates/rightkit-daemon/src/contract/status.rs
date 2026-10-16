use crate::error::*;
use crate::ServiceError;

/// Error code -> HTTP-style status, for transports that carry one. Local transport ignores it
/// and sends the [`ServiceError`] as is. Unknown codes are 500, or 503 when retryable.
pub const STATUS_TABLE: &[(&str, u16)] = &[
    ("bad_request", 400),
    (ERROR_UNAUTHENTICATED, 401),
    (ERROR_FORBIDDEN, 403),
    (ERROR_NOT_ALLOWED, 403),
    (ERROR_REVISION_CONFLICT, 409),
    (ERROR_RESPONSE_TOO_LARGE, 502),
    (ERROR_CONNECTION_CLOSED, 502),
    (ERROR_HOST_HANDOFF, 503),
    (ERROR_UNAVAILABLE, 503),
    (ERROR_TIMEOUT, 504),
    (ERROR_HOST_IDENTITY, 502),
    (ERROR_SUITE_UNRESOLVED, 500),
];

/// Status for a code; `None` when the table does not know it.
pub fn status_for_code(code: &str) -> Option<u16> {
    STATUS_TABLE
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, s)| *s)
}

/// Status for an error: the table, else 503 when retryable, else 500.
pub fn status_for_error(e: &ServiceError) -> u16 {
    status_for_code(&e.code).unwrap_or(if e.retryable { 503 } else { 500 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_map_and_unknown_fall_back() {
        assert_eq!(status_for_code("unauthenticated"), Some(401));
        assert_eq!(status_for_code("forbidden"), Some(403));
        assert_eq!(status_for_code("revision_conflict"), Some(409));
        assert_eq!(status_for_code(ERROR_NOT_ALLOWED), Some(403));
        assert_eq!(status_for_code("nope"), None);
        assert_eq!(status_for_error(&ServiceError::new("nope", "x")), 500);
        assert_eq!(status_for_error(&ServiceError::retryable("nope", "x")), 503);
        assert_eq!(status_for_error(&ServiceError::handoff()), 503);
    }

    #[test]
    fn codes_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (c, _) in STATUS_TABLE {
            assert!(seen.insert(*c), "duplicate {c}");
        }
    }
}
