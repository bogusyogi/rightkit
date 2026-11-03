//! OAuth 2.0 refresh for the platforms whose access token a refresh grant can renew after it has expired
//! (#1496). The decision per platform, with the repo evidence (postright `docs/p1/capability-matrix.md`,
//! "Verified" lines of 2026-10-16):
//!
//! | Platform | Policy |
//! |---|---|
//! | Bluesky | Refreshed by `machine::bluesky_authed` (`com.atproto.server.refreshSession`, the refresh JWT is the bearer). |
//! | X | Refreshed here: `POST /2/oauth2/token`, `grant_type=refresh_token`. The refresh token exists only with the `offline.access` scope; the access token lasts two hours. |
//! | TikTok | Refreshed here: `POST /v2/oauth/token/`, `grant_type=refresh_token`; the access token lasts 24 hours. The answer may carry a different refresh token, which replaces the old one. |
//! | Pinterest | Refreshed here: `POST /v5/oauth/token`, `grant_type=refresh_token`, client credentials as Basic auth. Continuous refresh: the answer carries the next refresh token. |
//! | YouTube | Refreshed here when a token endpoint is configured: Google's token endpoint is on another host than the API, so the base URL comes from the `youtube_token` entry of `platform_endpoints` (same loopback / `allow_remote` rule as any base URL); with none configured no refresh is attempted. |
//! | Threads, Instagram | Not refreshed here. Their refresh (`refresh_access_token`) needs a token that is at least 24 hours old and still UNEXPIRED, so it can never repair the expired token a 401 reports. The host refreshes ahead of `token_expires_at`; an expired token blocks readiness and a publish that meets it marks the account `reauth_required`. |
//! | Facebook | No refresh grant: a Page token derived from a long-lived user token has no expiry; a 401 means the person must reconnect (`reauth_required`). |
//! | Mastodon | No refresh: the token entity documents no `refresh_token` or `expires_in` (docs.joinmastodon.org/entities/Token). Reconnect. |
//! | LinkedIn | Programmatic refresh tokens are limited to approved Marketing Developer Platform partners (learn.microsoft.com/linkedin/shared/authentication/programmatic-refresh-tokens). Not a general grant: reconnect. |
//!
//! A refresh is attempted only after a platform answered 401, which means the request was NOT applied, so the
//! machine may send the same request once more. Client credentials and refresh tokens arrive in [`OAuthRefresh`]
//! for the command, are never stored or logged, and a refresh that fails leaves the original 401 (so the job
//! fails `token_expired` and the account is marked `reauth_required`); the refresh error text is dropped, never
//! put in a message. X and Pinterest rotate the refresh token: the new one is kept for the rest of the command
//! and handed back through `Drive::take_rotated_refresh`, because the host must persist it.
use rightkit_http::{form_encode, Method};
use serde_json::Value;

use crate::http::{self, Extra, Reply};
use crate::machine::Drive;
use crate::platforms::{pinterest, tiktok, x, youtube};

const X_TOKEN_PATH: &str = "/2/oauth2/token";
const TIKTOK_TOKEN_PATH: &str = "/v2/oauth/token/";
const PINTEREST_TOKEN_PATH: &str = "/v5/oauth/token";
const YOUTUBE_TOKEN_PATH: &str = "/token";
/// The `platform_endpoints` key that names Google's token endpoint host.
pub const YOUTUBE_TOKEN_ENDPOINT: &str = "youtube_token";

/// What a command carries to renew an expired access token. Per command, never stored.
#[derive(Clone)]
pub struct OAuthRefresh {
    pub refresh_token: String,
    /// The app's client id (TikTok: the client key).
    pub client_id: String,
    /// The app's client secret. Required for TikTok and Pinterest; X and YouTube accept a public client.
    pub client_secret: Option<String>,
}

/// True when the engine can renew this platform's expired access token itself.
pub fn supports(platform: &str) -> bool {
    matches!(
        platform,
        x::PLATFORM | tiktok::PLATFORM | pinterest::PLATFORM | youtube::PLATFORM
    )
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Tries to renew the access token of `platform` once. On success the new access token replaces the
/// command's token for every later request (`Drive::access`) and a rotated refresh token is kept. Returns
/// whether a new token was minted and how many requests were sent (0 when nothing was attempted).
pub fn refresh(platform: &str, d: &Drive) -> (bool, i64) {
    let Some(oauth) = d.oauth.as_ref() else {
        return (false, 0);
    };
    let refresh_token = d
        .rotated_refresh
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| oauth.refresh_token.clone());
    let secret = oauth.client_secret.as_deref().filter(|s| !s.is_empty());
    let (base, path, basic, body) = match platform {
        x::PLATFORM => {
            let pairs = [
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
                ("client_id", oauth.client_id.as_str()),
            ];
            (
                d.base.clone(),
                X_TOKEN_PATH,
                secret.map(|s| (oauth.client_id.as_str(), s)),
                form_encode(&pairs),
            )
        }
        pinterest::PLATFORM => {
            let Some(secret) = secret else {
                return (false, 0);
            };
            (
                d.base.clone(),
                PINTEREST_TOKEN_PATH,
                Some((oauth.client_id.as_str(), secret)),
                form_encode(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token.as_str()),
                ]),
            )
        }
        tiktok::PLATFORM => {
            let Some(secret) = secret else {
                return (false, 0);
            };
            (
                d.base.clone(),
                TIKTOK_TOKEN_PATH,
                None,
                form_encode(&[
                    ("client_key", oauth.client_id.as_str()),
                    ("client_secret", secret),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token.as_str()),
                ]),
            )
        }
        youtube::PLATFORM => {
            let Ok(base) = http::base_url(&d.endpoints, YOUTUBE_TOKEN_ENDPOINT, None) else {
                return (false, 0);
            };
            let mut pairs = vec![
                ("client_id", oauth.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
            ];
            if let Some(secret) = secret {
                pairs.push(("client_secret", secret));
            }
            (base, YOUTUBE_TOKEN_PATH, None, form_encode(&pairs))
        }
        _ => return (false, 0),
    };
    let authorization =
        basic.map(|(id, secret)| format!("Basic {}", base64(format!("{id}:{secret}").as_bytes())));
    let headers: Vec<(&str, &str)> = authorization
        .as_deref()
        .map(|value| vec![("Authorization", value)])
        .unwrap_or_default();
    // The empty token sends no bearer: the grant carries its own credentials.
    let reply = http::send_body(
        &d.client,
        Method::Post,
        &format!("{base}{path}"),
        "",
        None,
        Some(("application/x-www-form-urlencoded", body.into_bytes())),
        &Extra {
            headers: &headers,
            echo: &[],
        },
    );
    let Reply::Ok(answer) = reply else {
        return (false, 1);
    };
    let Some(access) = answer
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    else {
        return (false, 1);
    };
    if let Ok(mut guard) = d.access.lock() {
        *guard = Some(access.to_string());
    }
    if let Some(next) = answer
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        if let Ok(mut guard) = d.rotated_refresh.lock() {
            *guard = Some(next.to_string());
        }
    }
    (true, 1)
}
