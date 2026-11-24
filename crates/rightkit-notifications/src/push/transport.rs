//! The host's transport and what to do with APNs answers.

use std::time::Duration;

use super::{
    build_request, ApnsEnvironment, DeviceToken, DeviceTokenStore, PhoneAlert, Priority,
    ProviderTokenCache, PushError, PushRequest,
};

/// Longest backoff hint.
pub const MAX_BACKOFF_SECS: u64 = 300;

/// What the transport got back, already reduced to the three facts the policy needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushResponse {
    pub status: u16,
    /// APNs `reason` from the JSON body, when one was sent.
    pub reason: Option<String>,
    /// A `retry-after` hint in seconds, if the server gave one.
    pub retry_after_secs: Option<u64>,
}

impl PushResponse {
    /// Reduce a raw answer: `body` is the response body (searched for `"reason"`), and
    /// `retry_after` the `retry-after` header value when it is a number of seconds.
    pub fn from_parts(status: u16, body: &[u8], retry_after: Option<&str>) -> Self {
        Self {
            status,
            reason: parse_reason(body),
            retry_after_secs: retry_after.and_then(|v| v.trim().parse().ok()),
        }
    }
}

/// APNs reasons are short alphanumeric identifiers; anything else is dropped so a hostile body
/// cannot inject text into logs.
fn parse_reason(body: &[u8]) -> Option<String> {
    const KEY: &str = "\"reason\"";
    let text = std::str::from_utf8(body).ok()?;
    let rest = &text[text.find(KEY)? + KEY.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let reason = &rest[..rest.find('"')?];
    (!reason.is_empty() && reason.len() <= 64 && reason.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| reason.to_owned())
}

/// The host implements this to send one request over HTTP/2 to APNs and return the answer. It must
/// not log the request (its `Debug` is already redacted) and returns `Err` only when no HTTP answer
/// arrived (connect, TLS, timeout, stream reset); the message must not contain request content.
pub trait PushTransport: Send + Sync {
    fn send(&self, request: &PushRequest) -> Result<PushResponse, String>;
}

/// What happened to one send, and what the caller does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// 200: APNs accepted it.
    Delivered,
    /// 410 or `BadDeviceToken`: the token is dead. [`send_alert`] already removed it from the store.
    Unregistered,
    /// 429, 5xx or no answer: try again after `after`. The caller owns the retry loop and passes
    /// the next `attempt` number to [`send_alert`].
    Retry { after: Duration },
    /// Anything else (other 4xx). Not retried; reported with APNs' reason and the status.
    Rejected { status: u16, reason: Option<String> },
}

/// Exponential backoff hint: 1 s, 2 s, 4 s ... capped at [`MAX_BACKOFF_SECS`]. `attempt` counts
/// from 0. No jitter (this crate has no RNG dependency); the caller may add its own.
pub fn backoff_hint(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(16)).min(MAX_BACKOFF_SECS))
}

/// Classify a response. A server `retry-after` wins over the computed backoff (still capped).
pub fn classify(response: &PushResponse, attempt: u32) -> PushOutcome {
    match response.status {
        200 => PushOutcome::Delivered,
        410 => PushOutcome::Unregistered,
        _ if response.reason.as_deref() == Some("BadDeviceToken") => PushOutcome::Unregistered,
        429 | 500..=599 => PushOutcome::Retry {
            after: match response.retry_after_secs {
                Some(s) => Duration::from_secs(s.min(MAX_BACKOFF_SECS)),
                None => backoff_hint(attempt),
            },
        },
        status => PushOutcome::Rejected {
            status,
            reason: response.reason.clone(),
        },
    }
}

/// Fixed APNs settings for one app.
#[derive(Debug, Clone)]
pub struct ApnsConfig {
    pub environment: ApnsEnvironment,
    /// The app's bundle id (`apns-topic`).
    pub topic: String,
    pub priority: Priority,
}

/// One send attempt for `device_id`: look up its token, build the request with the cached provider
/// token, hand it to `transport`, classify the answer. A dead token is removed from `store` (only
/// if it is still the current one). A rejected provider token (`ExpiredProviderToken` or
/// `InvalidProviderToken`) is dropped from the cache and reported as a retry, so the next attempt
/// mints a fresh one. `now_unix` is seconds since the epoch.
#[allow(clippy::too_many_arguments)]
pub fn send_alert(
    transport: &dyn PushTransport,
    tokens: &ProviderTokenCache,
    store: &dyn DeviceTokenStore,
    config: &ApnsConfig,
    device_id: &str,
    alert: &PhoneAlert,
    now_unix: u64,
    attempt: u32,
) -> Result<PushOutcome, PushError> {
    let device_token: DeviceToken = store.get(device_id)?.ok_or(PushError::NoDeviceToken)?;
    let jwt = tokens.token(now_unix)?;
    let request = build_request(
        config.environment,
        &config.topic,
        &device_token,
        &jwt,
        alert,
        config.priority,
    )?;
    let response = match transport.send(&request) {
        Ok(response) => response,
        Err(_) => {
            return Ok(PushOutcome::Retry {
                after: backoff_hint(attempt),
            })
        }
    };
    let outcome = classify(&response, attempt);
    match &outcome {
        PushOutcome::Unregistered => {
            store.remove_if_current(device_id, &device_token)?;
        }
        PushOutcome::Rejected {
            status: 403,
            reason: Some(reason),
        } if reason == "ExpiredProviderToken" || reason == "InvalidProviderToken" => {
            tokens.invalidate();
            return Ok(PushOutcome::Retry {
                after: backoff_hint(attempt),
            });
        }
        _ => {}
    }
    Ok(outcome)
}
