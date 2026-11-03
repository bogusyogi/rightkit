//! Revoked entitlement-id denylist and the unverified-claims peek.
//!
//! * [`RevokedIds`] is a caller-supplied set of entitlement ids (`Entitlement::id`) that must
//!   never verify, for an incident where one token is known bad before it expires.
//!   [`verify_token_not_revoked`] runs the normal [`verify_token_any_device`] checks and then
//!   refuses a token whose authenticated id is listed. The id is read only after the
//!   signature verified, so a forged token cannot be used to probe or poison the list.
//! * [`peek_unverified_claims`] reads a few claims from a token **without** checking the
//!   signature. It exists for decisions made before verification (for example choosing
//!   whether to re-activate when a stored token looks old). Its result must never grant
//!   anything; verify the token before trusting any value.

use std::collections::HashSet;

use base64::Engine;
use serde::Deserialize;

use crate::{authenticate_token_any_device, check_time_and_lifecycle, Entitlement, VerifyError};

/// Largest token [`peek_unverified_claims`] will decode (16 KiB), so a hostile file cannot
/// make the peek allocate without bound.
const MAX_PEEK_TOKEN_BYTES: usize = 16 * 1024;

/// A set of revoked entitlement ids. Matching is exact (no trimming or case folding).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevokedIds {
    ids: HashSet<String>,
}

impl RevokedIds {
    /// An empty list: nothing is revoked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one id; returns the list for chaining.
    #[must_use]
    pub fn with(mut self, id: impl Into<String>) -> Self {
        self.ids.insert(id.into());
        self
    }

    /// Whether `id` is revoked.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Number of listed ids.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True when no id is listed.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

impl<S: Into<String>> FromIterator<S> for RevokedIds {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self {
            ids: iter.into_iter().map(Into::into).collect(),
        }
    }
}

/// Why [`verify_token_not_revoked`] refused a token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RevocationError {
    /// The token failed ordinary verification.
    Verify(VerifyError),
    /// The token is authentic but its entitlement id is on the denylist.
    Revoked {
        /// The revoked entitlement id.
        id: String,
    },
}

impl std::fmt::Display for RevocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verify(e) => e.fmt(f),
            Self::Revoked { id } => write!(f, "license '{id}' has been revoked"),
        }
    }
}

impl std::error::Error for RevocationError {}

impl From<VerifyError> for RevocationError {
    fn from(e: VerifyError) -> Self {
        Self::Verify(e)
    }
}

/// [`crate::verify_token_any_device`] plus a denylist. Order: signature, app, device (any of
/// `device_ids`), **revocation**, then time and lifecycle, so a revoked token reports
/// [`RevocationError::Revoked`] even after it expired. Fails closed: any error is a refusal.
pub fn verify_token_not_revoked(
    token: &str,
    pubkey: &[u8; 32],
    expected_app: &str,
    device_ids: &[&str],
    now_unix: i64,
    revoked: &RevokedIds,
) -> Result<Entitlement, RevocationError> {
    let ent = authenticate_token_any_device(token, pubkey, expected_app, device_ids)?;
    if revoked.contains(&ent.id) {
        return Err(RevocationError::Revoked { id: ent.id });
    }
    Ok(check_time_and_lifecycle(ent, now_unix)?)
}

/// Claims read from a token without verifying its signature. **Untrusted.**
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct UnverifiedClaims {
    /// Entitlement id.
    pub id: String,
    /// appKey claimed by the token.
    pub app: String,
    /// Tier claimed by the token.
    pub tier: String,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

/// Decode `id`, `app`, `tier`, `iat` and `exp` from a token **without checking the
/// signature, app, device or time**. Anyone can craft a token that peeks as anything, so the
/// result may only steer non-security decisions (which screen to show, whether to try an
/// online re-activation first); access must still go through [`crate::verify_token`] or
/// [`crate::evaluate_lifecycle`]. Tokens over 16 KiB are refused.
pub fn peek_unverified_claims(token: &str) -> Result<UnverifiedClaims, VerifyError> {
    if token.len() > MAX_PEEK_TOKEN_BYTES {
        return Err(VerifyError::Malformed);
    }
    let dot = token.find('.').ok_or(VerifyError::Malformed)?;
    let (payload_b64, sig_b64) = (&token[..dot], &token[dot + 1..]);
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(VerifyError::Malformed);
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| VerifyError::BadEncoding)?;
    serde_json::from_slice(&payload).map_err(|e| VerifyError::Parse(e.to_string()))
}
