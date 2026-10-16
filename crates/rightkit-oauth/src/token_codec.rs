//! Shared private token serialization; never log encoded values.
use crate::{OAuthError, TokenSet};
use rightkit_http::Secret;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    Ok(TokenSet {
        access_token: Secret::new(access),
        token_type: v
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: v
            .get("expires_in")
            .and_then(|x| x.as_u64())
            .map(Duration::from_secs),
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
            .checked_add(Duration::from_secs(
                v.get("obtained_at").and_then(|x| x.as_u64()).unwrap_or(0),
            ))
            .ok_or_else(|| storage("stored sign-in timestamp is invalid"))?,
    })
}
