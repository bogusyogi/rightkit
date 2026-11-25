//! The APNs provider token: an ES256 JWT, cached and refreshed under Apple's 60-minute rule.
//!
//! Apple rejects a token older than 60 minutes (`ExpiredProviderToken`) and throttles one that is
//! regenerated more often than every 20 minutes (`TooManyProviderTokenUpdates`). The cache reuses a
//! token until it is [`REFRESH_AFTER_SECS`] old, which sits between the two limits.

use std::sync::Mutex;

use rightkit_crypto::{base64url_encode, P256Signer};

use super::PushError;

/// Apple's hard limit on a provider token's age.
pub const MAX_TOKEN_AGE_SECS: u64 = 60 * 60;
/// Apple's floor on how often a new token may be minted.
pub const MIN_REFRESH_INTERVAL_SECS: u64 = 20 * 60;
/// The cache mints a new token once the current one is this old.
pub const REFRESH_AFTER_SECS: u64 = 50 * 60;

/// An Apple key id or team id: 1 to 32 ASCII alphanumerics (Apple issues 10), so it needs no JSON
/// escaping and cannot break the token structure.
fn check_apple_id(s: &str, what: &'static str) -> Result<(), PushError> {
    if s.is_empty() || s.len() > 32 || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(PushError::Invalid(what));
    }
    Ok(())
}

/// Build one provider JWT: header `{"alg":"ES256","kid":key_id}`, claims `{"iss":team_id,"iat":iat}`,
/// signature raw `r || s`, all base64url without padding.
pub fn build_provider_jwt(
    signer: &P256Signer,
    key_id: &str,
    team_id: &str,
    iat: u64,
) -> Result<String, PushError> {
    check_apple_id(key_id, "key id")?;
    check_apple_id(team_id, "team id")?;
    let header = format!("{{\"alg\":\"ES256\",\"kid\":\"{key_id}\"}}");
    let claims = format!("{{\"iss\":\"{team_id}\",\"iat\":{iat}}}");
    let mut input = base64url_encode(header.as_bytes());
    input.push('.');
    input.push_str(&base64url_encode(claims.as_bytes()));
    let sig = signer
        .sign(input.as_bytes())
        .map_err(|_| PushError::Signing)?;
    input.push('.');
    input.push_str(&base64url_encode(&sig));
    Ok(input)
}

/// Holds the signer and the current provider token. `Debug` prints neither.
pub struct ProviderTokenCache {
    signer: P256Signer,
    key_id: String,
    team_id: String,
    current: Mutex<Option<(String, u64)>>,
}

impl ProviderTokenCache {
    pub fn new(signer: P256Signer, key_id: &str, team_id: &str) -> Result<Self, PushError> {
        check_apple_id(key_id, "key id")?;
        check_apple_id(team_id, "team id")?;
        Ok(Self {
            signer,
            key_id: key_id.to_owned(),
            team_id: team_id.to_owned(),
            current: Mutex::new(None),
        })
    }

    /// The token to send at `now_unix` (seconds since the epoch): the cached one while it is under
    /// [`REFRESH_AFTER_SECS`] old, otherwise a fresh one with `iat = now_unix`. A clock that moved
    /// backwards past the token's `iat` also mints a fresh one.
    pub fn token(&self, now_unix: u64) -> Result<String, PushError> {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((jwt, iat)) = current.as_ref() {
            if now_unix >= *iat && now_unix - *iat < REFRESH_AFTER_SECS {
                return Ok(jwt.clone());
            }
        }
        let jwt = build_provider_jwt(&self.signer, &self.key_id, &self.team_id, now_unix)?;
        *current = Some((jwt.clone(), now_unix));
        Ok(jwt)
    }

    /// [`ProviderTokenCache::token`] at the system clock.
    pub fn token_now(&self) -> Result<String, PushError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| PushError::Invalid("system clock"))?
            .as_secs();
        self.token(now)
    }

    /// Drop the cached token (after Apple rejected it), so the next call mints a new one. Apple
    /// throttles minting more often than every 20 minutes, so callers invalidate only on
    /// `ExpiredProviderToken` or `InvalidProviderToken`.
    pub fn invalidate(&self) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl std::fmt::Debug for ProviderTokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderTokenCache(<redacted>)")
    }
}
