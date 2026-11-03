//! Per-host circuit breaker over the always-on provider request path.
//!
//! P2-4 / P0-C 2g: a provider that is failing hard is not hammered request after
//! request mid-session. The state machine (count window, error-ratio trip,
//! half-open single probe) is `crate::breaker::CircuitBreaker`; its
//! defaults (window 20, min samples 5, ratio 0.5, open 60 s) are the values this
//! module used to carry. This module keeps only the the host app wiring: one
//! process-wide breaker keyed by base URL, and the classification of
//! `ProviderError` into "counts against the host".
//!
//! Behaviour kept: a failure that is not breaker-relevant (auth, malformed
//! request, one bad SSE chunk) is recorded as a success sample, exactly as
//! before, so it still closes a half-open probe.

use crate::agent_providers::transport::ProviderError;
use crate::breaker::{BreakerConfig, CircuitBreaker, Outcome};

/// Whether `error` counts against a host's circuit breaker (P2-4 / P0-C
/// 2g). Scoped to the transient/provider-side class — rate limiting, 5xx,
/// and raw network/connection failures — never a permanent per-request
/// failure (auth, malformed request, an SSE decode error specific to one
/// response). Mirrors the "server" preset failure-code set in
/// repos/cli/grok-build/crates/common/xai-circuit-breaker/src/config.rs
/// (`[429, 500, 502, 503, 504]`) plus this crate's own `Network`/
/// `RateLimited` variants, which that reference expresses as separate
/// transport-level errors rather than HTTP status codes.
pub(crate) fn is_breaker_relevant_failure(error: Option<&ProviderError>) -> bool {
    match error {
        None => false,
        Some(ProviderError::Http { status, .. }) => *status == 429 || (500..=599).contains(status),
        Some(ProviderError::Network(_))
        | Some(ProviderError::NotDelivered(_))
        | Some(ProviderError::DeliveryUncertain { .. })
        | Some(ProviderError::RateLimited { .. }) => true,
        Some(ProviderError::Sse(_))
        | Some(ProviderError::Auth)
        | Some(ProviderError::Provider(_))
        | Some(ProviderError::Capacity(_))
        | Some(ProviderError::Authority(_))
        | Some(ProviderError::IncompleteOutput { .. }) => false,
    }
}

static BREAKER: std::sync::OnceLock<CircuitBreaker> = std::sync::OnceLock::new();

fn breaker() -> &'static CircuitBreaker {
    BREAKER.get_or_init(|| CircuitBreaker::new(BreakerConfig::default()))
}

/// True when a request to `base_url` is currently allowed through the
/// breaker. Pub(crate) so `anthropic_client.rs` can gate its own requests
/// through the same per-host state.
pub(crate) fn breaker_allows_request(base_url: &str) -> bool {
    breaker().allow(base_url)
}

/// Record the outcome of a request that `breaker_allows_request` admitted.
pub(crate) fn breaker_record_outcome(base_url: &str, success: bool) {
    breaker().record(
        base_url,
        if success {
            Outcome::Success
        } else {
            Outcome::Failure
        },
    );
}
