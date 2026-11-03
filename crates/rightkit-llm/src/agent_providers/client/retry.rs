//! `ra-018` decomposition: connection/HTTP retry ladders, `Retry-After`
//! parsing, rate-limit header parsing, and stream-establishment/breaker
//! observation helpers — moved verbatim out of `client.rs`. Timing, caps,
//! and error classification are unchanged; only the module boundary is new.
//! See `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.

use super::breaker::{breaker_record_outcome, is_breaker_relevant_failure};
use super::{ProviderResponseStream, ProviderStreamEvent};
use crate::agent_providers::transport::{ProviderError, RateLimitMetadata};
use futures::StreamExt;

/// A deterministic doubling ladder as a `rightkit_http::RetryPolicy` (no jitter, no
/// Retry-After handling: the ladders own those). Only its `backoff(retry)` delay is used; the
/// attempt counting, notices and stream-phase reconnects stay in the ladders.
fn ladder_policy(
    initial: std::time::Duration,
    max: std::time::Duration,
) -> rightkit_http::RetryPolicy {
    rightkit_http::RetryPolicy {
        initial_backoff: initial,
        max_backoff: max,
        multiplier: 2.0,
        jitter: 0.0,
        ..rightkit_http::RetryPolicy::none()
    }
}

/// Short provider label for retry notices: the route host.
pub(crate) fn provider_label(base_url: &str) -> String {
    let rest = base_url.split("://").nth(1).unwrap_or(base_url);
    rest.split(['/', ':']).next().unwrap_or(rest).to_string()
}

/// Tell the caller a retry is about to run after `delay`.
pub(crate) fn notify_retry(
    notifier: &rightkit_agent_engine_protocol::RetryNotifier,
    base_url: &str,
    reason: impl Into<String>,
    attempt: u32,
    max_attempts: u32,
    delay: std::time::Duration,
) {
    notifier.notify(rightkit_agent_engine_protocol::RetryNotice {
        provider: provider_label(base_url),
        reason: reason.into(),
        attempt,
        max_attempts,
        delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
    });
}

/// Reconnect-with-backoff for pure connection failures (P2-4 / P0-C 2g):
/// 5s, 10s, 20s, ... doubling, capped at 60s — distinct from the faster
/// generic `backoff` above, matching
/// repos/cli/codex/codex-rs/core/src/responses_retry.rs's
/// `INITIAL_CONNECTION_RETRY_DELAY` / `MAX_CONNECTION_RETRY_DELAY` shape.
pub(crate) async fn connection_backoff(attempt: u32) {
    tokio::time::sleep(connection_backoff_duration(attempt)).await;
}

/// Pure duration formula behind [`connection_backoff`], split out so the
/// 5s/10s/20s/.../60s-cap shape is unit-testable without waiting on a real
/// sleep (this crate has no `tokio` `test-util` feature to pause the clock).
pub(crate) fn connection_backoff_duration(attempt: u32) -> std::time::Duration {
    ladder_policy(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(60),
    )
    .backoff(attempt)
}

/// Parse a `Retry-After` header off a 429 response (parsing is
/// `rightkit_http::parse_retry_after`: delta-seconds or HTTP-date), capped at
/// 60s so a misbehaving/malicious provider can't stall the caller
/// indefinitely with an oversized value.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 60;

pub(crate) fn retry_after_secs(
    resp: &crate::agent_providers::http::ProviderHttpResponse,
) -> Option<u64> {
    if resp.status() != 429 {
        return None;
    }
    let raw = resp.header("retry-after")?;
    let secs = rightkit_http::parse_retry_after(raw)?.as_secs();
    Some(secs.min(MAX_RETRY_AFTER_SECS))
}

/// `S:M2`/`S:M3` amendment item C: OpenAI-compatible routes commonly report
/// remaining budget via `x-ratelimit-remaining-{requests,tokens}` and
/// `x-ratelimit-reset-{requests,tokens}` (the reset headers are usually a
/// duration string like `"6m0s"` or a plain integer-seconds value; only the
/// plain-integer form is parsed here — an unparsable reset value is left
/// `None` rather than guessed at). Returns `None` fields, never an error,
/// when a header is absent or malformed — this is best-effort metadata, not
/// load-bearing for correctness.
pub(crate) fn parse_openai_rate_limit_headers(
    headers: &[(String, String)],
) -> Option<RateLimitMetadata> {
    use crate::agent_providers::http::header_u64;
    let metadata = RateLimitMetadata {
        remaining_requests: header_u64(headers, "x-ratelimit-remaining-requests"),
        remaining_tokens: header_u64(headers, "x-ratelimit-remaining-tokens"),
        reset_requests_secs: header_u64(headers, "x-ratelimit-reset-requests"),
        reset_tokens_secs: header_u64(headers, "x-ratelimit-reset-tokens"),
    };
    (!metadata.is_empty()).then_some(metadata)
}

/// Record a failed stream establishment before returning it to the caller.
///
/// A stream's successful outcome is intentionally recorded by
/// [`observe_stream_breaker_outcome`] only after the stream emits `Finished`.
/// Establishment failures happen before that wrapper exists, so callers must
/// route the result through this helper rather than using `?` directly. The
/// retry helper has already normalized transport and HTTP failures into the
/// shared [`ProviderError`] taxonomy by this point.
pub(crate) fn record_stream_establishment_outcome(
    base_url: &str,
    result: Result<crate::agent_providers::http::ProviderHttpResponse, ProviderError>,
) -> Result<crate::agent_providers::http::ProviderHttpResponse, ProviderError> {
    match result {
        Ok(response) => Ok(response),
        Err(error) => {
            let counts_as_success = !is_breaker_relevant_failure(Some(&error));
            breaker_record_outcome(base_url, counts_as_success);
            Err(error)
        }
    }
}

/// CR-12: shared breaker-outcome taxonomy for a provider *stream*, used by
/// both wire protocols. Previously each stream implementation recorded a
/// breaker success only on its own explicit `Finished` yield and never
/// recorded a mid-stream failure at all — a stream that failed after
/// yielding some events (SSE framing error, provider `error` event, network
/// drop) told the per-host breaker nothing, the same gap
/// `complete_with_tools_metadata`'s `is_breaker_relevant_failure` scoping
/// already closed for the non-streaming path. This closes it for streaming
/// too, identically for both providers, instead of drifting per-file.
pub(crate) fn observe_stream_breaker_outcome(
    base_url: String,
    inner: ProviderResponseStream,
) -> ProviderResponseStream {
    Box::pin(async_stream::stream! {
        futures::pin_mut!(inner);
        while let Some(item) = inner.next().await {
            match item {
                Ok(event) => {
                    let finished = matches!(event, ProviderStreamEvent::Finished(_));
                    yield Ok(event);
                    if finished {
                        breaker_record_outcome(&base_url, true);
                    }
                }
                Err(error) => {
                    let counts_as_success = !is_breaker_relevant_failure(Some(&error));
                    breaker_record_outcome(&base_url, counts_as_success);
                    yield Err(error);
                    return;
                }
            }
        }
    })
}
