use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use crate::netpolicy::{AddressClass, AddressDenial, RedirectRefusal};

/// Maximum error-body bytes retained on a [`HttpError::Status`].
pub const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Where in a call an error happened. Carried by [`HttpError::Timeout`] and
/// [`HttpError::Transport`] so apps can keep their own per-phase messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Phase {
    /// Name lookup, TCP connect or TLS handshake.
    Connect,
    /// Sending the request and waiting for the response headers.
    Headers,
    /// Reading the response body.
    Body,
    /// Any phase of a redirect hop after the first request, or an exhausted redirect limit.
    Redirect,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Connect => "connect",
            Phase::Headers => "headers",
            Phase::Body => "body",
            Phase::Redirect => "redirect",
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum HttpError {
    /// URL, header, or request construction failed before anything was sent.
    InvalidRequest(String),
    /// Connection, TLS, socket failure, or exhausted redirect limit.
    /// `connect_phase` is true when nothing
    /// can have reached the server (safe to retry any method); it is true exactly when
    /// `phase` is [`Phase::Connect`].
    Transport {
        message: String,
        connect_phase: bool,
        phase: Phase,
    },
    /// A configured timeout elapsed. `message` names the elapsed timeout.
    Timeout { message: String, phase: Phase },
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
    /// The call was cancelled through a [`crate::CancelToken`].
    Cancelled,
    /// [`ProxyMode::System`](crate::ProxyMode::System): the Windows proxy settings hold
    /// something this version cannot follow exactly, named in `element`: different proxy
    /// routes for HTTP and HTTPS, a bypass entry that is not a plain host (`<local>`, an entry
    /// limited to one scheme, an inner wildcard), or a proxy auto-config script. The client
    /// sends nothing: it never drops a bypass entry while still using the proxy, and never
    /// sends one scheme to the other's proxy. "Automatically detect settings" alone is not
    /// refused (since 0.3.1; see `ProxyModeUsed`). Returned by
    /// `Client::try_new` and by every request of a client built with `Client::new`. An app
    /// catches it and uses the mode it used before (`ProxyMode::Environment`, or
    /// `ProxyMode::Explicit`). Never retried.
    SystemProxyUnsupported { element: String },
    /// The outbound address policy refused the destination (see [`crate::netpolicy`]).
    /// `addr` and `class` are `None` when the denial is about the name or scheme (no
    /// address, a metadata host name, or a host outside a first-party list). `reason`
    /// carries the detail, including custom `deny_if` text. Never retried.
    AddressDenied {
        host: String,
        addr: Option<IpAddr>,
        class: Option<AddressClass>,
        reason: String,
    },
    /// [`crate::ClientConfig::https_only`] is set and the URL (or a redirect target when
    /// `redirect` is true) is not `https`. `scheme` is lowercase, `host` has no
    /// credentials. Nothing was sent to that URL. Never retried.
    HttpsRequired {
        host: String,
        scheme: String,
        redirect: bool,
    },
    /// A redirect target was refused before anything was sent to it: its origin is outside
    /// the request's [`crate::ApprovedOrigins`], or its URL carries userinfo or an unsafe
    /// shape (see [`RedirectRefusal`]). `host` is the target's host, without userinfo, path
    /// or query. The earlier hops were sent. Never retried, since the same redirect would be
    /// refused again.
    RedirectRefused {
        host: String,
        reason: RedirectRefusal,
    },
}

impl From<AddressDenial> for HttpError {
    fn from(d: AddressDenial) -> Self {
        HttpError::AddressDenied {
            host: d.host,
            addr: d.addr,
            class: d.class,
            reason: d.reason,
        }
    }
}

/// Coarse class of an [`HttpError`], from [`HttpError::class`]. Apps map their own error
/// classes and retry sets from this one table instead of matching statuses themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorClass {
    /// HTTP 401, 403 or 407.
    Auth,
    /// HTTP 429. `retry_after` is the parsed `Retry-After` delay, when the server sent one.
    RateLimit { retry_after: Option<Duration> },
    /// HTTP 5xx.
    Server,
    /// Any other status: 4xx not listed above, or an unfollowed 3xx.
    Client,
    /// A configured timeout elapsed, or the server answered HTTP 408 Request Timeout.
    Timeout,
    /// Connection, TLS or socket failure, or an exhausted redirect limit.
    Transport,
    /// The call was cancelled.
    Cancelled,
    /// The outbound address policy refused the destination.
    AddressDenied,
    /// The request could not be built, or the response body was malformed or oversized
    /// (`InvalidRequest`, `Json`, `Stream`, `BodyTooLarge`), `https_only` refused a URL
    /// (`HttpsRequired`), or a redirect target was refused (`RedirectRefused`).
    Invalid,
}

impl HttpError {
    pub(crate) fn timeout(message: impl Into<String>, phase: Phase) -> Self {
        HttpError::Timeout {
            message: message.into(),
            phase,
        }
    }

    pub(crate) fn transport(message: impl Into<String>, phase: Phase) -> Self {
        HttpError::Transport {
            message: message.into(),
            connect_phase: phase == Phase::Connect,
            phase,
        }
    }

    /// The [`Phase`] of a `Timeout` or `Transport` error; `None` for every other kind.
    pub fn phase(&self) -> Option<Phase> {
        match self {
            HttpError::Timeout { phase, .. } | HttpError::Transport { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// True when the request may have reached the server, so a non-idempotent request (a paid
    /// submit) must not be assumed unsent. False only for errors that prove nothing was sent:
    /// invalid request construction, a connect-phase transport or timeout failure, and a
    /// refusal before any connection (address policy, https-only, proxy mode). Every other
    /// error, including any answer from the server, returns true.
    pub fn request_may_have_been_delivered(&self) -> bool {
        match self {
            HttpError::InvalidRequest(_)
            | HttpError::AddressDenied { .. }
            | HttpError::HttpsRequired { .. }
            | HttpError::SystemProxyUnsupported { .. } => false,
            HttpError::Transport { connect_phase, .. } => !*connect_phase,
            HttpError::Timeout { phase, .. } => *phase != Phase::Connect,
            _ => true,
        }
    }

    /// Re-label a `Timeout` or `Transport` error as happening during a redirect hop. A
    /// transport error is no longer a pure connect failure, because earlier hops reached servers.
    pub(crate) fn into_redirect_phase(self) -> Self {
        match self {
            HttpError::Timeout { message, .. } => HttpError::timeout(message, Phase::Redirect),
            HttpError::Transport { message, .. } => HttpError::Transport {
                message,
                connect_phase: false,
                phase: Phase::Redirect,
            },
            other => other,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            HttpError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The error's [`ErrorClass`]. See the README for the full table.
    pub fn class(&self) -> ErrorClass {
        match self {
            HttpError::Status {
                status,
                retry_after,
                ..
            } => match *status {
                401 | 403 | 407 => ErrorClass::Auth,
                408 => ErrorClass::Timeout,
                429 => ErrorClass::RateLimit {
                    retry_after: *retry_after,
                },
                500..=599 => ErrorClass::Server,
                _ => ErrorClass::Client,
            },
            HttpError::Timeout { .. } => ErrorClass::Timeout,
            HttpError::Transport { .. } => ErrorClass::Transport,
            HttpError::Cancelled => ErrorClass::Cancelled,
            HttpError::SystemProxyUnsupported { .. } => ErrorClass::Invalid,
            HttpError::AddressDenied { .. } => ErrorClass::AddressDenied,
            HttpError::HttpsRequired { .. } => ErrorClass::Invalid,
            HttpError::RedirectRefused { .. } => ErrorClass::Invalid,
            HttpError::InvalidRequest(_)
            | HttpError::Json(_)
            | HttpError::Stream(_)
            | HttpError::BodyTooLarge { .. } => ErrorClass::Invalid,
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidRequest(m) => write!(f, "invalid request: {m}"),
            HttpError::Transport { message, .. } => write!(f, "transport error: {message}"),
            HttpError::Timeout { message, .. } => write!(f, "timeout: {message}"),
            HttpError::Status {
                status, attempts, ..
            } => {
                write!(f, "HTTP {status} after {attempts} attempt(s)")
            }
            HttpError::BodyTooLarge { limit } => write!(f, "response body exceeds {limit} bytes"),
            HttpError::Json(m) => write!(f, "invalid JSON: {m}"),
            HttpError::Stream(m) => write!(f, "stream error: {m}"),
            HttpError::Cancelled => write!(f, "cancelled"),
            HttpError::SystemProxyUnsupported { element } => write!(
                f,
                "the Windows proxy settings cannot be followed exactly by this version \
                 ({element}); nothing was sent"
            ),
            HttpError::AddressDenied { host, reason, .. } => {
                write!(f, "address denied for host '{host}': {reason}")
            }
            HttpError::HttpsRequired {
                host,
                scheme,
                redirect,
            } => {
                let what = if *redirect {
                    "redirect to"
                } else {
                    "request to"
                };
                write!(
                    f,
                    "https required: {what} '{scheme}' URL for host '{host}' refused"
                )
            }
            HttpError::RedirectRefused { host, reason } => {
                write!(f, "redirect refused for host '{host}': {reason}")
            }
        }
    }
}

impl std::error::Error for HttpError {}
