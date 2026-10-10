use std::fmt;

use rightkit_http::HttpError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub route: String,
    pub reason: String,
}

#[derive(Debug)]
pub enum LlmError {
    Http(HttpError),
    /// No credential available for the provider.
    MissingCredential(String),
    /// Provider answered 2xx with a body we cannot interpret.
    Malformed(String),
    /// Error after the first token was delivered; never retried or failed over.
    MidStream(String),
    /// Provider reported an in-stream error event (before the first token).
    Provider {
        kind: String,
        message: String,
    },
    /// The caller's callback requested cancellation.
    Cancelled,
    /// Every route failed; one entry per route tried.
    AllRoutesFailed(Vec<Attempt>),
}

impl LlmError {
    pub fn status(&self) -> Option<u16> {
        match self {
            LlmError::Http(e) => e.status(),
            _ => None,
        }
    }

    /// Whether another route could plausibly succeed: transport failures,
    /// timeouts, 408/425/429/5xx (incl. 529), or a 4xx whose body says the
    /// provider is overloaded/unavailable. Auth and validation errors are not.
    pub fn is_failover_trigger(&self) -> bool {
        match self {
            LlmError::Http(HttpError::Status { status, body, .. }) => {
                matches!(*status, 408 | 425 | 429 | 500..=599)
                    || ((400..500).contains(status) && says_unavailable(body))
            }
            LlmError::Http(HttpError::Transport { .. }) | LlmError::Http(HttpError::Timeout(_)) => {
                true
            }
            LlmError::Provider { kind, message } => {
                matches!(
                    kind.as_str(),
                    "overloaded_error" | "rate_limit_error" | "api_error"
                ) || says_unavailable(message)
            }
            _ => false,
        }
    }
}

fn says_unavailable(text: &str) -> bool {
    let t = text.to_lowercase();
    ["overloaded", "unavailable", "capacity", "try again later"]
        .iter()
        .any(|k| t.contains(k))
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "{e}"),
            LlmError::MissingCredential(p) => write!(f, "no credential for provider '{p}'"),
            LlmError::Malformed(m) => write!(f, "malformed provider response: {m}"),
            LlmError::MidStream(m) => write!(f, "stream failed after first token: {m}"),
            LlmError::Provider { kind, message } => write!(f, "provider error {kind}: {message}"),
            LlmError::Cancelled => write!(f, "cancelled"),
            LlmError::AllRoutesFailed(a) => {
                write!(f, "all routes failed: ")?;
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{} ({})", x.route, x.reason)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LlmError {}

impl From<HttpError> for LlmError {
    fn from(e: HttpError) -> Self {
        LlmError::Http(e)
    }
}
