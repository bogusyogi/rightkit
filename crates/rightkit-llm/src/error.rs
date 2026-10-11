use std::fmt;

use rightkit_http::HttpError;

/// Longest provider error text kept in [`LlmError::Status`], in bytes (cut on a character
/// boundary). The full body is never copied into an error.
pub const MAX_PROVIDER_MESSAGE_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub route: String,
    pub reason: String,
}

#[derive(Debug)]
pub enum LlmError {
    Http(HttpError),
    /// The provider answered with a non-2xx status. `message` is the provider's error text,
    /// bounded to [`MAX_PROVIDER_MESSAGE_BYTES`] and with the request's credential removed.
    /// Apps can match on `status` and keep their own wording.
    Status {
        status: u16,
        message: String,
    },
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
    /// The provider rejected the credential (HTTP 401 or 403) on a key-validation call
    /// (`OpenAiCompatProvider::list_models` / `validate_credential`). `message` is bounded and
    /// redacted, as for [`LlmError::Status`].
    Auth {
        status: u16,
        message: String,
    },
    /// The request was rejected before it was sent (for example an extra body field that
    /// collides with a field the crate sets). Nothing reached the network.
    InvalidRequest(String),
    /// The provider stopped at its output limit and the request asked for that to be an
    /// error ([`crate::ChatRequest::reject_truncated_output`]). `output_tokens` is the count
    /// the route reported, or `None` when it reported none.
    Truncated {
        output_tokens: Option<u64>,
    },
    /// A command-line assistant route failed (feature `cli`).
    #[cfg(feature = "cli")]
    Cli(crate::provider::cli::CliError),
    /// Every route failed; one entry per route tried.
    AllRoutesFailed(Vec<Attempt>),
}

impl LlmError {
    /// Convert a transport-level error from a provider call. A non-2xx response becomes
    /// [`LlmError::Status`] with the provider's message; `secret` (the request's credential,
    /// when known) is removed from that message.
    pub(crate) fn from_http(e: HttpError, secret: Option<&str>) -> Self {
        match e {
            HttpError::Status { status, body, .. } => LlmError::Status {
                status,
                message: provider_message(&body, secret),
            },
            other => LlmError::Http(other),
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            LlmError::Status { status, .. } => Some(*status),
            LlmError::Http(e) => e.status(),
            LlmError::Auth { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Whether another route could plausibly succeed: transport failures,
    /// timeouts, 408/425/429/5xx (incl. 529), or a 4xx whose body says the
    /// provider is overloaded/unavailable. Auth and validation errors are not.
    pub fn is_failover_trigger(&self) -> bool {
        match self {
            LlmError::Status { status, message } => failover_status(*status, message),
            LlmError::Http(HttpError::Status { status, body, .. }) => {
                failover_status(*status, body)
            }
            LlmError::Http(HttpError::Transport { .. })
            | LlmError::Http(HttpError::Timeout { .. }) => true,
            LlmError::Provider { kind, message } => {
                matches!(
                    kind.as_str(),
                    "overloaded_error" | "rate_limit_error" | "api_error"
                ) || says_unavailable(message)
            }
            #[cfg(feature = "cli")]
            LlmError::Cli(e) => e.is_failover_trigger(),
            _ => false,
        }
    }
}

fn failover_status(status: u16, text: &str) -> bool {
    matches!(status, 408 | 425 | 429 | 500..=599)
        || ((400..500).contains(&status) && says_unavailable(text))
}

/// The provider's `error.message` (OpenAI and Anthropic shape), a string `error`, or a
/// top-level `message` when the body is JSON; otherwise the body text. Bounded and with
/// `secret` removed.
pub(crate) fn provider_message(body: &str, secret: Option<&str>) -> String {
    let parsed: Option<String> = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .or_else(|| v["message"].as_str())
                .map(str::to_owned)
        });
    let text = parsed.unwrap_or_else(|| body.to_owned());
    truncate_utf8(
        redact_secret(&text, secret).trim(),
        MAX_PROVIDER_MESSAGE_BYTES,
    )
}

/// Remove the request's credential from text that came from the provider.
pub(crate) fn redact_secret(text: &str, secret: Option<&str>) -> String {
    match secret.filter(|s| !s.is_empty()) {
        Some(s) => text.replace(s, "[redacted]"),
        None => text.to_owned(),
    }
}

fn truncate_utf8(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
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
            LlmError::Status { status, message } if message.is_empty() => {
                write!(f, "HTTP {status}")
            }
            LlmError::Status { status, message } => write!(f, "HTTP {status}: {message}"),
            LlmError::Http(e) => write!(f, "{e}"),
            LlmError::MissingCredential(p) => write!(f, "no credential for provider '{p}'"),
            LlmError::Malformed(m) => write!(f, "malformed provider response: {m}"),
            LlmError::MidStream(m) => write!(f, "stream failed after first token: {m}"),
            LlmError::Provider { kind, message } => write!(f, "provider error {kind}: {message}"),
            LlmError::Cancelled => write!(f, "cancelled"),
            LlmError::Auth { status, message } if message.is_empty() => {
                write!(f, "credential rejected: HTTP {status}")
            }
            LlmError::Auth { status, message } => {
                write!(f, "credential rejected: HTTP {status}: {message}")
            }
            LlmError::InvalidRequest(m) => write!(f, "invalid request: {m}"),
            LlmError::Truncated {
                output_tokens: Some(n),
            } => write!(f, "output truncated at the output limit after {n} tokens"),
            LlmError::Truncated {
                output_tokens: None,
            } => {
                write!(f, "output truncated at the output limit")
            }
            #[cfg(feature = "cli")]
            LlmError::Cli(e) => write!(f, "{e}"),
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
        LlmError::from_http(e, None)
    }
}
