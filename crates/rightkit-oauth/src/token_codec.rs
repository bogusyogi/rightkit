//! Shared private token serialization; never log encoded values.
use crate::{OAuthError, TokenSet};
use rightkit_http::Secret;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Largest stored `expires_in` accepted on decode: 10 years in seconds. Real
/// providers issue lifetimes of hours to months; anything larger is treated as
/// a corrupt or hostile stored form and refused, never as a long-lived token.
pub(crate) const MAX_STORED_EXPIRES_IN_SECS: u64 = 10 * 365 * 24 * 60 * 60;

fn storage(e: impl fmt::Display) -> OAuthError {
    OAuthError::Storage(e.to_string())
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn encode(tokens: &TokenSet) -> String {
    let value = serde_json::json!({
        "v": 1,
        "access": tokens.access_token.expose(),
        "type": tokens.token_type,
        "expires_in": tokens.expires_in.map(|d| d.as_secs()),
        "renew": tokens.refresh_token.as_ref().map(|s| s.expose()),
        "scope": tokens.scope,
        "id": tokens.id_token.as_ref().map(|s| s.expose()),
        "obtained_at": unix(tokens.obtained_at),
    });
    // Server build wipes JSON-owned copies as well as the caller's encoded buffer.
    #[cfg(feature = "server")]
    {
        super::server::sensitive::from_value(value).to_string()
    }
    #[cfg(not(feature = "server"))]
    {
        value.to_string()
    }
}

pub(crate) fn decode(text: &str) -> Result<TokenSet, OAuthError> {
    #[cfg(not(feature = "server"))]
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|_| storage("stored sign-in is unreadable"))?;
    #[cfg(feature = "server")]
    let v = super::server::sensitive::SensitiveJson::parse(text.as_bytes())
        .map_err(|_| storage("stored sign-in is unreadable"))?;
    let access = v
        .get("access")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| storage("stored sign-in has no access value"))?;
    // A present-but-malformed expiry or timestamp refuses the stored form:
    // silently dropping it would make a bounded token look unexpired forever.
    let expires_in = match v.get("expires_in") {
        None | Some(serde_json::Value::Null) => None,
        Some(x) => {
            let secs = x
                .as_u64()
                .filter(|s| *s <= MAX_STORED_EXPIRES_IN_SECS)
                .ok_or_else(|| storage("stored sign-in expiry is invalid"))?;
            Some(Duration::from_secs(secs))
        }
    };
    let obtained_secs = match v.get("obtained_at") {
        None | Some(serde_json::Value::Null) => 0,
        Some(x) => x
            .as_u64()
            .ok_or_else(|| storage("stored sign-in timestamp is invalid"))?,
    };
    // The deadline must be representable: an overflowing sum would otherwise
    // read as "no deadline" and keep the token usable forever.
    if let Some(d) = expires_in {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(obtained_secs))
            .and_then(|at| at.checked_add(d))
            .ok_or_else(|| storage("stored sign-in expiry is invalid"))?;
    }
    Ok(TokenSet {
        access_token: Secret::new(access),
        token_type: v
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("Bearer")
            .to_string(),
        expires_in,
        refresh_token: v
            .get("renew")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(Secret::new),
        scope: v.get("scope").and_then(|x| x.as_str()).map(String::from),
        id_token: v
            .get("id")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(Secret::new),
        obtained_at: UNIX_EPOCH
            .checked_add(Duration::from_secs(obtained_secs))
            .ok_or_else(|| storage("stored sign-in timestamp is invalid"))?,
    })
}
