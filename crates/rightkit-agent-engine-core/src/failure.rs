//! Failure classification — maps an error string to a `RunFailed` kind + hint.

/// The failure class attached to a `RunFailed` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Network,
    Timeout,
    RateLimited,
    Auth,
    ProviderError,
    Budget,
    Other,
}

impl FailureKind {
    /// The wire string used in `RunFailed.kind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureKind::Network => "network",
            FailureKind::Timeout => "timeout",
            FailureKind::RateLimited => "rate_limited",
            FailureKind::Auth => "auth",
            FailureKind::ProviderError => "provider_error",
            FailureKind::Budget => "budget",
            FailureKind::Other => "other",
        }
    }

    /// A short, user-facing remediation hint.
    pub fn hint(&self) -> &'static str {
        match self {
            FailureKind::Network => "check your network connection and retry",
            FailureKind::Timeout => "the provider took too long; retry or lower max_tokens",
            FailureKind::RateLimited => "you are rate-limited; wait and retry, or switch routes",
            FailureKind::Auth => "check your API key in the key store",
            FailureKind::ProviderError => {
                "the provider returned an error; check model availability"
            }
            FailureKind::Budget => "the session budget was exceeded; raise the ceiling to continue",
            FailureKind::Other => "see the error detail for more information",
        }
    }
}

/// Classify an error message into a `FailureKind` by keyword.
pub fn classify_failure(error: &str) -> FailureKind {
    let e = error.to_lowercase();
    if e.contains("rate limit") || e.contains("429") || e.contains("too many requests") {
        FailureKind::RateLimited
    } else if e.contains("timeout") || e.contains("timed out") || e.contains("deadline") {
        FailureKind::Timeout
    } else if e.contains("request authority") || e.contains("federation") {
        // Context federation/request authority failures are not credential
        // problems ("authority" must not match the "auth" substring below).
        FailureKind::Other
    } else if e.contains("unauthorized")
        || e.contains("401")
        || e.contains("api key")
        || e.contains("auth")
    {
        FailureKind::Auth
    } else if e.contains("budget") || e.contains("ceiling") || e.contains("spend") {
        FailureKind::Budget
    } else if e.contains("connection")
        || e.contains("network")
        || e.contains("dns")
        || e.contains("refused")
    {
        FailureKind::Network
    } else if e.contains("provider") || e.contains("500") || e.contains("502") || e.contains("503")
    {
        FailureKind::ProviderError
    } else {
        FailureKind::Other
    }
}
