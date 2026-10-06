use serde_json::{json, Value};
use std::fmt;

/// The host is handing the service to a higher-ranked host. Retryable: re-read
/// discovery and resubmit the whole request.
pub const ERROR_HOST_HANDOFF: &str = "host_handoff";
/// The calling app may not call this method (or is not on the allowlist).
pub const ERROR_NOT_ALLOWED: &str = "request_not_allowed";
/// The connection dropped before a response arrived. Retryable.
pub const ERROR_CONNECTION_CLOSED: &str = "connection_closed";
/// The per-request deadline (connect + write + read) expired and the
/// connection was aborted. Not retryable: the request may already have run on
/// the host, so it is never replayed automatically.
pub const ERROR_TIMEOUT: &str = "request_timeout";
/// No live host for the service. Retryable.
pub const ERROR_UNAVAILABLE: &str = "service_unavailable";

/// Typed wire error. `retryable` means resubmitting the request can succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), retryable: false }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), retryable: true }
    }
    pub fn handoff() -> Self {
        Self::retryable(ERROR_HOST_HANDOFF, "host is handing the service to a higher-ranked host; retry")
    }
    pub fn to_value(&self) -> Value {
        json!({"code": self.code, "message": self.message, "retryable": self.retryable})
    }
    pub fn from_value(v: &Value) -> Self {
        Self {
            code: v["code"].as_str().unwrap_or("error").into(),
            message: v["message"].as_str().unwrap_or("").into(),
            retryable: v["retryable"].as_bool().unwrap_or(false),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ServiceError {}
impl From<std::io::Error> for ServiceError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", e.to_string())
    }
}
