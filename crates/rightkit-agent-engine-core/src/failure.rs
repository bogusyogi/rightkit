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
    let lowered = error.to_lowercase();
    // A provider HTTP error is `HTTP error <status>: <body>`. The status decides; the body is
    // the provider's free text ("429 tokens", "auth_token") and must not be matched for keywords.
    if let Some(at) = lowered.find("http error ") {
        let after = &lowered[at + "http error ".len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if after[digits.len()..].starts_with(':') {
            match digits.as_str() {
                "429" => return FailureKind::RateLimited,
                "401" | "403" => return FailureKind::Auth,
                "408" => return FailureKind::Timeout,
                code if code.starts_with('5') => return FailureKind::ProviderError,
                _ => return FailureKind::Other,
            }
        }
    }
    let e = lowered;
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

/// True when a stream failed because the connection or provider failed and the same request
/// may succeed later; never for a refusal of the request itself.
pub fn is_retryable_stream_failure(error: &str) -> bool {
    let lowered = error.to_lowercase();
    const REFUSALS: [&str; 7] = [
        "context length exceeded",
        "provider quota or billing limit reached",
        "auth failed",
        "no visible content or tool calls",
        "declined to answer",
        "provider output incomplete",
        "budget",
    ];
    if REFUSALS.iter().any(|needle| lowered.contains(needle)) {
        return false;
    }
    if lowered.starts_with("network error:")
        || lowered.starts_with("network error (request not sent)")
        || lowered.contains("sse parse error:")
        || lowered.contains("provider stream outcome uncertain")
        || lowered.contains("stream ended before a terminal event")
        || lowered.contains("provider stream ended before completion")
        || lowered.contains("rate limited")
        || lowered.contains("overloaded")
    {
        return true;
    }
    if let Some(at) = lowered.find("http error ") {
        let after = &lowered[at + "http error ".len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if after[digits.len()..].starts_with(':') {
            return matches!(
                digits.as_str(),
                "408" | "425" | "429" | "500" | "502" | "503" | "504" | "529"
            );
        }
    }
    false
}

/// Wait before stream retry `attempt` (1-based): 1 s, 2 s, 4 s, then 8 s.
pub fn stream_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(1000u64 << attempt.saturating_sub(1).min(3))
}
