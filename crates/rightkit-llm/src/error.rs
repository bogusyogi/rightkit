use std::fmt;

use rightkit_http::{HttpError, Phase};

use crate::raw::RawHandle;
use crate::types::{AttemptBilling, Metering, Spend};

/// Longest provider error text kept in [`LlmError::Status`], in bytes (cut on a character
/// boundary). The full body is never copied into an error.
pub const MAX_PROVIDER_MESSAGE_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub route: String,
    pub reason: String,
    /// What the attempt cost or may have cost. See [`AttemptBilling`].
    pub billing: AttemptBilling,
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
    /// The provider returned no text and no tool call, and either a refusal or a
    /// `content_filter` finish reason. `message` is the refusal text when the provider sent
    /// one, otherwise a fixed description. Not a failover trigger.
    Refused {
        message: String,
    },
    /// A command-line assistant route failed (feature `cli`).
    #[cfg(feature = "cli")]
    Cli(crate::provider::cli::CliError),
    /// Every route failed; one entry per route tried.
    AllRoutesFailed(Vec<Attempt>),
    /// A [`LlmError::Truncated`] or [`LlmError::Malformed`] together with the provider's raw
    /// response, returned only when raw capture is on (`with_raw_capture` on the provider).
    /// With capture off these two errors are returned bare, exactly as before.
    ///
    /// Display, [`LlmError::status`] and [`LlmError::is_failover_trigger`] answer for the inner
    /// error, and failover policies are applied to it. To match the inner variant, look through
    /// the wrapper with [`LlmError::root`], [`LlmError::is_truncated`] or
    /// [`LlmError::is_malformed`]; [`LlmError::raw`] returns the captured exchange, the same
    /// handle the observer received for the call. See [`crate::raw`] for what it holds.
    Captured {
        error: Box<LlmError>,
        raw: RawHandle,
    },
    /// A failure after the provider had reported token counts, returned only for a request
    /// that asked for it ([`crate::ChatRequest::report_failed_usage`]). Display, `status`
    /// and `is_failover_trigger` answer for the inner error; [`LlmError::root`] looks
    /// through it and [`LlmError::billing`] returns the reported usage.
    Billed {
        error: Box<LlmError>,
        billing: AttemptBilling,
    },
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
            LlmError::Captured { error, .. } | LlmError::Billed { error, .. } => error.status(),
            _ => None,
        }
    }

    /// The error with any raw-capture wrapper removed ([`LlmError::Captured`]); every other
    /// error is returned as it is. Match on this when raw capture may be on.
    pub fn root(&self) -> &LlmError {
        match self {
            LlmError::Captured { error, .. } | LlmError::Billed { error, .. } => error.root(),
            other => other,
        }
    }

    /// The raw provider response captured for this error, when the provider has raw capture on
    /// and this is a [`LlmError::Truncated`] or [`LlmError::Malformed`] outcome.
    pub fn raw(&self) -> Option<&RawHandle> {
        match self {
            LlmError::Captured { raw, .. } => Some(raw),
            LlmError::Billed { error, .. } => error.raw(),
            _ => None,
        }
    }

    /// What the failed attempt cost or may have cost. Reported usage is returned when the
    /// error carries it ([`LlmError::Billed`]); otherwise the state is classified from the
    /// error alone: `NotSpent` when nothing was sent or the provider rejected the request
    /// (HTTP 4xx other than 408, a local validation error), `Unknown` once the request was
    /// dispatched without counts. A command-line route is `Metering::Subscription`.
    pub fn billing(&self) -> AttemptBilling {
        let per_token = |spend| AttemptBilling::new(spend, Metering::PerToken);
        match self {
            LlmError::Billed { billing, .. } => *billing,
            LlmError::Captured { error, .. } => error.billing(),
            LlmError::MissingCredential(_)
            | LlmError::InvalidRequest(_)
            | LlmError::Auth { .. } => per_token(Spend::NotSpent),
            // Nothing can have reached the provider: the request was never built, or it failed during name
            // lookup, TCP connect or the TLS handshake (`Phase::Connect`, rightkit-http's own definition).
            LlmError::Http(HttpError::InvalidRequest(_))
            | LlmError::Http(HttpError::Transport {
                phase: Phase::Connect,
                ..
            })
            | LlmError::Http(HttpError::Timeout {
                phase: Phase::Connect,
                ..
            }) => per_token(Spend::NotSpent),
            LlmError::Status { status, .. } => per_token(status_spend(*status)),
            LlmError::Http(HttpError::Status { status, .. }) => per_token(status_spend(*status)),
            #[cfg(feature = "cli")]
            LlmError::Cli(e) => AttemptBilling::new(e.spend(), Metering::Subscription),
            _ => per_token(Spend::Unknown),
        }
    }

    /// True for [`LlmError::Truncated`], with or without a raw-capture wrapper.
    pub fn is_truncated(&self) -> bool {
        matches!(self.root(), LlmError::Truncated { .. })
    }

    /// True for [`LlmError::Malformed`], with or without a raw-capture wrapper.
    pub fn is_malformed(&self) -> bool {
        matches!(self.root(), LlmError::Malformed(_))
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
            LlmError::Captured { error, .. } | LlmError::Billed { error, .. } => {
                error.is_failover_trigger()
            }
            _ => false,
        }
    }
}

fn status_spend(status: u16) -> Spend {
    if (400..500).contains(&status) && status != 408 {
        Spend::NotSpent
    } else {
        Spend::Unknown
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
            LlmError::Refused { message } => write!(f, "completion refused or filtered: {message}"),
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
            LlmError::Captured { error, .. } | LlmError::Billed { error, .. } => {
                write!(f, "{error}")
            }
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

#[cfg(test)]
mod connect_phase_billing_tests {
    use super::*;

    #[test]
    fn nothing_sent_is_not_spent_and_a_dispatched_request_stays_unknown() {
        let transport = |phase| {
            LlmError::Http(HttpError::Transport {
                message: "x".into(),
                connect_phase: phase == Phase::Connect,
                phase,
            })
        };
        let timeout = |phase| {
            LlmError::Http(HttpError::Timeout {
                message: "x".into(),
                phase,
            })
        };
        assert_eq!(transport(Phase::Connect).billing().spend, Spend::NotSpent);
        assert_eq!(timeout(Phase::Connect).billing().spend, Spend::NotSpent);
        assert_eq!(
            LlmError::Http(HttpError::InvalidRequest("bad".into()))
                .billing()
                .spend,
            Spend::NotSpent
        );
        for phase in [Phase::Headers, Phase::Body, Phase::Redirect] {
            assert_eq!(
                transport(phase).billing().spend,
                Spend::Unknown,
                "{phase:?}"
            );
            assert_eq!(timeout(phase).billing().spend, Spend::Unknown, "{phase:?}");
        }
    }
}
