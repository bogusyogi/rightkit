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
use crate::breaker::{BreakerConfig, CircuitBreaker, Outcome, Phase};

/// Whether `error` counts against a host's circuit breaker (P2-4 / P0-C
/// 2g). Scoped to the transient/provider-side class — rate limiting, 5xx,
/// and raw network/connection failures — never a permanent per-request
/// failure (auth, malformed request, an SSE decode error specific to one
/// response). Mirrors the "server" preset failure-code set in
/// repos/cli/grok-build/crates/common/xai-circuit-breaker/src/config.rs
/// (`[429, 500, 502, 503, 504]`) plus this crate's own `Network`/
/// `RateLimited` variants, which that reference expresses as separate
/// transport-level errors rather than HTTP status codes.
///
/// `QuotaExceeded` is deliberately not breaker-relevant: the breaker protects a host from transient
/// overload, while a quota or billing refusal is a per-account state that is unbilled, answered at once,
/// and should stay visible to the user on every turn rather than be masked by an open breaker.
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
        | Some(ProviderError::QuotaExceeded(_))
        | Some(ProviderError::IncompleteOutput { .. }) => false,
    }
}

static BREAKER: std::sync::OnceLock<CircuitBreaker> = std::sync::OnceLock::new();

fn breaker() -> &'static CircuitBreaker {
    BREAKER.get_or_init(|| CircuitBreaker::new(BreakerConfig::default()))
}

/// Admit a request to `base_url` through the breaker, or `None` when the breaker refuses it.
/// Pub(crate) so `anthropic_client.rs` can gate its own requests through the same per-host
/// state.
///
/// The returned [`BreakerAdmission`] must report the request's outcome with
/// [`BreakerAdmission::record`]. If it is dropped first (the turn was cancelled, the request
/// was refused locally after admission, or a stream was abandoned before it finished), a
/// half-open probe is released with `Outcome::Ignored`, so the host is not refused forever.
///
/// The breaker is keyed by base URL, or by base URL and model when the host set
/// `breaker_per_model`: a rate limit on one model of a gateway must then not refuse a different
/// model on the same URL.
pub(crate) fn breaker_admit(base_url: &str, model_id: &str) -> Option<BreakerAdmission> {
    if super::super::host::host_options().breaker_per_model {
        admit_in(breaker(), &format!("{base_url}#{model_id}"))
    } else {
        admit_in(breaker(), base_url)
    }
}

fn admit_in(breaker: &'static CircuitBreaker, base_url: &str) -> Option<BreakerAdmission> {
    if !breaker.allow(base_url) {
        return None;
    }
    // In the half-open phase only the single probe is admitted, so a key that reads half-open
    // right after admission was admitted as that probe.
    let probe = breaker.phase(base_url) == Phase::HalfOpen;
    Some(BreakerAdmission {
        breaker,
        base_url: base_url.to_string(),
        probe,
        armed: true,
    })
}

/// One admitted request. Records its outcome once; releases a half-open probe on drop.
pub(crate) struct BreakerAdmission {
    breaker: &'static CircuitBreaker,
    base_url: String,
    probe: bool,
    armed: bool,
}

impl BreakerAdmission {
    /// Record the outcome of the admitted request. Only the first call counts.
    pub(crate) fn record(&mut self, success: bool) {
        if !std::mem::replace(&mut self.armed, false) {
            return;
        }
        self.breaker.record(
            &self.base_url,
            if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
        );
    }
}

impl Drop for BreakerAdmission {
    fn drop(&mut self) {
        if self.armed && self.probe {
            self.breaker.record(&self.base_url, Outcome::Ignored);
        }
    }
}
