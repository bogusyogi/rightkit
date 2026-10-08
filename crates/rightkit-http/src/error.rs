use std::fmt;
use std::time::Duration;

/// Maximum error-body bytes retained on a [`HttpError::Status`].
pub const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum HttpError {
    /// URL, header, or request construction failed before anything was sent.
    InvalidRequest(String),
    /// Connection, TLS, socket failure, or exhausted redirect limit.
    /// `connect_phase` is true when nothing
    /// can have reached the server (safe to retry any method).
    Transport {
        message: String,
        connect_phase: bool,
    },
    /// A configured timeout elapsed.
    Timeout(String),
    /// Non-2xx response (after retries were exhausted or not applicable).
    Status {
        status: u16,
        body: String,
        retry_after: Option<Duration>,
        attempts: u32,
    },
    /// Response body exceeded the configured limit.
    BodyTooLarge { limit: usize },
    /// Response was not valid JSON for the requested type.
    Json(String),
    /// Streaming protocol violation (oversized SSE line, bad UTF-8, ...).
    Stream(String),
}

impl HttpError {
    pub fn status(&self) -> Option<u16> {
        match self {
            HttpError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidRequest(m) => write!(f, "invalid request: {m}"),
            HttpError::Transport { message, .. } => write!(f, "transport error: {message}"),
            HttpError::Timeout(m) => write!(f, "timeout: {m}"),
            HttpError::Status {
                status, attempts, ..
            } => {
                write!(f, "HTTP {status} after {attempts} attempt(s)")
            }
            HttpError::BodyTooLarge { limit } => write!(f, "response body exceeds {limit} bytes"),
            HttpError::Json(m) => write!(f, "invalid JSON: {m}"),
            HttpError::Stream(m) => write!(f, "stream error: {m}"),
        }
    }
}

impl std::error::Error for HttpError {}
