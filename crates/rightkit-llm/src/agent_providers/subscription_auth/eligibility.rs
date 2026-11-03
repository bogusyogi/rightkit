//! the host app-original invariant layered on the Pi port (no donor equivalent):
//! the tri-state subscription eligibility gate and the typed auth states.
//!
//! Canon "Product boundary" (binding): native inference may use
//! subscription-backed direct inference only where the provider explicitly
//! supports third-party harness subscription use, expressed as a tri-state
//! eligibility flag (`Supported`/`Unsupported`/`Unknown`) where **only
//! `Supported` may be enabled in core** — never reverse-engineered token
//! reuse. The Pi donor expresses this as the boolean `isSubscription`; the
//! tri-state vocabulary and the gate are the host app's.
//!
//! Typed auth states (`signed-out`/`authenticating`/`ready`/`expired`/
//! `refresh-failed`/`revoked`) are the status vocabulary for PL-001's
//! contract leg; they are metadata-only (a redacted message at most).

use serde::{Deserialize, Serialize};

use rightkit_oauth::TokenSet;

use super::types::now_ms;

/// Tri-state eligibility for subscription-backed direct inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionEligibility {
    /// The provider explicitly supports third-party harness subscription
    /// use. This is the only state that may enable subscription-backed
    /// inference in core.
    Supported,
    /// The provider explicitly does not support (or forbids) third-party
    /// harness subscription use.
    Unsupported,
    /// No explicit statement either way. Treated as not enabled: core never
    /// guesses.
    Unknown,
}

impl SubscriptionEligibility {
    pub fn as_str(self) -> &'static str {
        match self {
            SubscriptionEligibility::Supported => "supported",
            SubscriptionEligibility::Unsupported => "unsupported",
            SubscriptionEligibility::Unknown => "unknown",
        }
    }

    /// The gate itself: subscription-backed inference is enabled in core only
    /// for `Supported`. `Unknown` is deliberately not enabled — eligibility
    /// must be explicit, never assumed.
    pub fn enabled_in_core(self) -> bool {
        matches!(self, SubscriptionEligibility::Supported)
    }
}

/// Typed auth states for a provider's stored credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthState {
    /// Status cannot inspect the configured store. This is not signed-out,
    /// and it never permits credential fallback during inference.
    Unavailable {
        reason: String,
    },
    SignedOut,
    Authenticating,
    Ready {
        expires_at_ms: Option<i64>,
    },
    Expired,
    RefreshFailed {
        /// Redacted diagnostic — never contains credential bytes.
        message: String,
    },
    Revoked,
}

/// Refresh-before-expiry window for subscription sign-ins: a stored token with less
/// validity left than this is refreshed before use (the `Resolver` refresh skew) and is
/// reported as `Expired` by status.
pub const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;

/// Minimum remaining validity below which a stored sign-in is treated as `Expired` for
/// status purposes. Matches the resolver's refresh-before-use window.
pub const STATUS_MIN_VALIDITY_MS: i64 = DEFAULT_OAUTH_MINIMUM_VALIDITY_MS;

/// Expiry of a stored sign-in as epoch milliseconds, or `None` when it carries no lifetime.
pub fn token_expires_at_ms(tokens: &TokenSet) -> Option<i64> {
    let expires_in = tokens.expires_in?;
    let obtained = tokens
        .obtained_at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    Some(obtained.saturating_add(expires_in.as_millis() as i64))
}

/// Classify a stored sign-in into a typed auth state. `now` / `min_validity_ms` are
/// injectable for tests; pass [`now_ms()`] and [`STATUS_MIN_VALIDITY_MS`] in production.
pub fn classify_stored_auth_state(
    tokens: Option<&TokenSet>,
    now: i64,
    min_validity_ms: i64,
) -> AuthState {
    let Some(tokens) = tokens else {
        return AuthState::SignedOut;
    };
    match token_expires_at_ms(tokens) {
        None => AuthState::Ready {
            expires_at_ms: None,
        },
        Some(expires) if now.saturating_add(min_validity_ms) >= expires => AuthState::Expired,
        Some(expires) => AuthState::Ready {
            expires_at_ms: Some(expires),
        },
    }
}

/// Convenience wrapper classifying at wall-clock time.
pub fn auth_state_of_stored(tokens: Option<&TokenSet>) -> AuthState {
    classify_stored_auth_state(tokens, now_ms(), STATUS_MIN_VALIDITY_MS)
}
