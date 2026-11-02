//! Errors from the resident endpoint client.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No resident answered within the connect budget. The caller runs the
    /// request in process. Nothing was sent, so this is always safe to fall
    /// back from, including for writes.
    NotRunning {
        reason: String,
    },
    /// The resident was reached but the request outcome is unknown (the
    /// deadline passed or the connection dropped). A write may have run, so
    /// the caller must not replay it in process.
    Timeout {
        message: String,
    },
    Transport {
        message: String,
    },
    /// The resident answered `ok: false`.
    Refused {
        code: String,
        message: String,
        retryable: bool,
    },
    /// The answer was not a well-formed `{v:1, ok, data|error}`.
    Protocol {
        message: String,
    },
}

impl ClientError {
    pub fn code(&self) -> &str {
        match self {
            Self::NotRunning { .. } => "not_running",
            Self::Timeout { .. } => "deadline_exceeded",
            Self::Transport { .. } => "transport_failed",
            Self::Refused { code, .. } => code,
            Self::Protocol { .. } => "protocol_error",
        }
    }

    pub fn is_not_running(&self) -> bool {
        matches!(self, Self::NotRunning { .. })
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning { reason } => write!(f, "membrane resident not running: {reason}"),
            Self::Timeout { message } => {
                write!(f, "membrane resident deadline exceeded: {message}")
            }
            Self::Transport { message } => {
                write!(f, "membrane resident transport failed: {message}")
            }
            Self::Refused { code, message, .. } => {
                write!(f, "membrane resident refused ({code}): {message}")
            }
            Self::Protocol { message } => write!(f, "membrane resident protocol error: {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_has_its_code_and_message() {
        let cases = [
            (
                ClientError::NotRunning { reason: "r".into() },
                "not_running",
                "membrane resident not running: r",
            ),
            (
                ClientError::Timeout {
                    message: "t".into(),
                },
                "deadline_exceeded",
                "membrane resident deadline exceeded: t",
            ),
            (
                ClientError::Transport {
                    message: "x".into(),
                },
                "transport_failed",
                "membrane resident transport failed: x",
            ),
            (
                ClientError::Refused {
                    code: "busy".into(),
                    message: "later".into(),
                    retryable: true,
                },
                "busy",
                "membrane resident refused (busy): later",
            ),
            (
                ClientError::Protocol {
                    message: "p".into(),
                },
                "protocol_error",
                "membrane resident protocol error: p",
            ),
        ];
        for (error, code, text) in cases {
            assert_eq!(error.code(), code);
            assert_eq!(error.to_string(), text);
            assert_eq!(error.is_not_running(), code == "not_running");
        }
    }
}
