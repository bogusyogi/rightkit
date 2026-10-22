use crate::host_identity::HostIdentityError;
use serde_json::{json, Value};
use std::fmt;

/// The host is handing the service to a higher-ranked host. Retryable: re-read
/// discovery and resubmit the whole request.
pub const ERROR_HOST_HANDOFF: &str = "host_handoff";
/// The process behind the endpoint is not the host the caller expected
/// ([`Client::connect_expecting`](crate::Client::connect_expecting)). Not retryable: reconnecting
/// reaches the same impostor, and no application byte was sent to it.
pub const ERROR_HOST_IDENTITY: &str = "host_identity";
/// The suite root could not be resolved, or a [`ClientConfig`](crate::ClientConfig) that ignores
/// environment overrides was handed a suite that came from the environment. Not retryable.
pub const ERROR_SUITE_UNRESOLVED: &str = "suite_unresolved";
/// The calling app may not call this method (or is not on the allowlist).
pub const ERROR_NOT_ALLOWED: &str = "request_not_allowed";
/// The handler's result did not fit the host's frame limit, so it was not sent. Not retryable:
/// the same request produces the same result. Ask for less (a page, a smaller body).
pub const ERROR_RESPONSE_TOO_LARGE: &str = "response_too_large";
/// A request is larger than the host will assemble from chunks
/// ([`HostOptions::max_message`](crate::HostOptions::max_message)), or than one frame when the
/// host does not chunk. Not retryable. Ask for less.
pub const ERROR_REQUEST_TOO_LARGE: &str = "request_too_large";
/// A chunk frame (`rk.upload`, `rk.fetch`) was malformed, out of order, repeated or for an upload
/// or result that is gone. Not retryable as sent: start the whole transfer again.
pub const ERROR_BAD_CHUNK: &str = "bad_chunk";
/// The connection dropped before a response arrived. Retryable.
pub const ERROR_CONNECTION_CLOSED: &str = "connection_closed";
/// The per-request deadline (connect + write + read) expired and the
/// connection was aborted. Not retryable: the request may already have run on
/// the host, so it is never replayed automatically.
pub const ERROR_TIMEOUT: &str = "request_timeout";
/// No live host for the service. Retryable.
pub const ERROR_UNAVAILABLE: &str = "service_unavailable";

/// The caller presented no valid credential (a device that is unknown, or whose signature failed).
pub const ERROR_UNAUTHENTICATED: &str = "unauthenticated";
/// The caller is known but lacks the scope for this method.
pub const ERROR_FORBIDDEN: &str = "forbidden";
/// A write named a stale revision.
pub const ERROR_REVISION_CONFLICT: &str = "revision_conflict";
/// Too many failed authentications from this connection, or from all callers together, inside
/// the failure window (`crate::fail_limit`). Not retryable on the same connection; the block
/// lapses on its own, so wait before reconnecting.
pub const ERROR_RATE_LIMITED: &str = "rate_limited";

/// Typed wire error. `retryable` means resubmitting the request can succeed.
///
/// Equality compares `code`, `message`, `retryable` and `details`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// Optional structured data (e.g. the platform of a `needs_auth`). Absent
    /// or `null` on the wire reads as `None`; written only when present, so
    /// older peers see the frame they always saw. Set with
    /// [`ServiceError::with_details`], read with [`ServiceError::details`].
    pub details: Option<Value>,
}

impl Eq for ServiceError {}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: None,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            details: None,
        }
    }
    /// Attaches structured `details`; `Value::Null` clears them.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = if details.is_null() {
            None
        } else {
            Some(details)
        };
        self
    }
    /// Structured data attached with [`ServiceError::with_details`], or `None`.
    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
    pub fn handoff() -> Self {
        Self::retryable(
            ERROR_HOST_HANDOFF,
            "host is handing the service to a higher-ranked host; retry",
        )
    }
    pub fn to_value(&self) -> Value {
        let mut v =
            json!({"code": self.code, "message": self.message, "retryable": self.retryable});
        if let (Some(d), Some(o)) = (&self.details, v.as_object_mut()) {
            o.insert("details".to_string(), d.clone());
        }
        v
    }
    pub fn from_value(v: &Value) -> Self {
        Self {
            code: v["code"].as_str().unwrap_or("error").into(),
            message: v["message"].as_str().unwrap_or("").into(),
            retryable: v["retryable"].as_bool().unwrap_or(false),
            details: v.get("details").filter(|d| !d.is_null()).cloned(),
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
impl From<HostIdentityError> for ServiceError {
    /// The wire-style form of a host identity refusal: code [`ERROR_HOST_IDENTITY`], not retryable.
    fn from(e: HostIdentityError) -> Self {
        Self::new(ERROR_HOST_IDENTITY, e.to_string())
    }
}

/// Why [`Client::connect_expecting`](crate::Client::connect_expecting) did not return a client.
///
/// The two variants are different facts. [`ConnectError::Service`] is the ordinary connection
/// story (no record, host unreachable, protocol mismatch, hello refused, timeout) and keeps its
/// `retryable` flag. [`ConnectError::HostIdentity`] means something answered at the endpoint and
/// the OS says it is not the host the caller named; no application byte was sent to it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectError {
    Service(ServiceError),
    HostIdentity(HostIdentityError),
}

impl ConnectError {
    /// Collapse into the single wire-style error. A host identity refusal becomes code
    /// [`ERROR_HOST_IDENTITY`], not retryable, with the typed detail in the message.
    pub fn into_service_error(self) -> ServiceError {
        match self {
            ConnectError::Service(e) => e,
            ConnectError::HostIdentity(e) => e.into(),
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Service(e) => fmt::Display::fmt(e, f),
            ConnectError::HostIdentity(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConnectError::Service(e) => Some(e),
            ConnectError::HostIdentity(e) => Some(e),
        }
    }
}

impl From<ServiceError> for ConnectError {
    fn from(e: ServiceError) -> Self {
        ConnectError::Service(e)
    }
}

impl From<HostIdentityError> for ConnectError {
    fn from(e: HostIdentityError) -> Self {
        ConnectError::HostIdentity(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_roundtrip_and_absent_or_null_read_as_none() {
        let plain = ServiceError::new("nope", "x");
        assert!(plain.details().is_none());
        assert!(plain.to_value().get("details").is_none());
        assert_eq!(ServiceError::from_value(&plain.to_value()), plain);
        let nulled = json!({"code": "nope", "message": "x", "retryable": false, "details": null});
        assert_eq!(ServiceError::from_value(&nulled), plain);
        let detailed = ServiceError::retryable("needs_auth", "sign in")
            .with_details(json!({"platform": "x", "nested": {"a": [1, 2]}}));
        assert_eq!(detailed.to_value()["details"]["nested"]["a"], json!([1, 2]));
        assert_eq!(ServiceError::from_value(&detailed.to_value()), detailed);
        assert!(detailed.with_details(Value::Null).details().is_none());
    }
}
