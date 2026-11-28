//! Guest links (`{app} share`): a token scoped to one session.
//!
//! The owner (the daemon bearer) mints a share for one session. Its token lets
//! a guest follow that session's events and, only when granted, answer its
//! tool approvals. Nothing else: no other session, no message, no setting.
//!
//! Shares live in memory only. The daemon keeps the SHA-256 of each token,
//! never the token, and every share dies with the daemon process (a restart
//! revokes them all, as it rotates the owner token). A share expires at its
//! deadline and can be revoked early. The routes exist only on a host that turns
//! `host_options::guest_shares` on.

use axum::extract::{Extension, Path};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest a share may live: 7 days.
pub(crate) const MAX_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// The shortest a share may live: 1 minute.
pub(crate) const MIN_TTL_SECS: u64 = 60;
/// The default lifetime: 1 hour.
pub(crate) const DEFAULT_TTL_SECS: u64 = 60 * 60;

#[derive(Clone, Debug)]
struct Share {
    id: String,
    session_id: String,
    token_hash: String,
    can_approve: bool,
    deadline: Instant,
    expires_at: String,
    created_at: String,
}

/// What a guest request may do, attached to the request by `require_token`.
#[derive(Clone, Debug)]
pub(crate) struct GuestContext {
    pub(crate) share_id: String,
    pub(crate) session_id: String,
    pub(crate) can_approve: bool,
    pub(crate) expires_at: String,
}

/// Marks a request that arrived on the LAN listener (`serve --lan`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LanOrigin;

static SHARES: Mutex<Vec<Share>> = Mutex::new(Vec::new());

fn shares() -> std::sync::MutexGuard<'static, Vec<Share>> {
    SHARES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn hash(token: &str) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(token.as_bytes()))
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|error| format!("no randomness: {error}"))?;
    Ok(rightkit_crypto::hex_encode(&buf))
}

fn iso_after(ttl: Duration) -> String {
    let secs = SystemTime::now()
        .checked_add(ttl)
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}

fn prune(list: &mut Vec<Share>) {
    let now = Instant::now();
    list.retain(|share| share.deadline > now);
}

/// Mint a share; returns (share id, token, expiry). The token is shown once.
pub(crate) fn mint(
    session_id: &str,
    ttl_secs: u64,
    can_approve: bool,
) -> Result<(String, String, String), String> {
    let ttl = Duration::from_secs(ttl_secs.clamp(MIN_TTL_SECS, MAX_TTL_SECS));
    // A `g_` prefix tells a guest token from the owner token in logs and UIs.
    let token = format!("g_{}", random_hex(32)?);
    let id = format!("share-{}", random_hex(6)?);
    let expires_at = iso_after(ttl);
    let mut list = shares();
    prune(&mut list);
    list.push(Share {
        id: id.clone(),
        session_id: session_id.to_string(),
        token_hash: hash(&token),
        can_approve,
        deadline: Instant::now() + ttl,
        expires_at: expires_at.clone(),
        created_at: rightkit_agent_engine_protocol::now_iso(),
    });
    Ok((id, token, expires_at))
}

/// The live share a presented token belongs to, if any. The comparison is constant-time over
/// the token's hash, so neither the token nor a prefix of it is learnable by timing.
pub(crate) fn resolve(token: &str) -> Option<GuestContext> {
    if !token.starts_with("g_") {
        return None;
    }
    let presented = hash(token);
    let mut list = shares();
    prune(&mut list);
    list.iter()
        .find(|share| rightkit_crypto::ct_eq(share.token_hash.as_bytes(), presented.as_bytes()))
        .map(|share| GuestContext {
            share_id: share.id.clone(),
            session_id: share.session_id.clone(),
            can_approve: share.can_approve,
            expires_at: share.expires_at.clone(),
        })
}

/// Whether a guest may make this request: its own session's event stream,
/// history and replay, a resume of that session (it loads stored history, no
/// model call), its approvals when granted, and `GET /guest/whoami`.
pub(crate) fn guest_allows(method: &Method, path: &str, guest: &GuestContext) -> bool {
    if *method == Method::GET && path == "/guest/whoami" {
        return true;
    }
    let Some(rest) = path.strip_prefix("/sessions/") else {
        return false;
    };
    let Some((id, tail)) = rest.split_once('/') else {
        return false;
    };
    let id = percent_decode(id);
    if id != guest.session_id {
        return false;
    }
    match (method, tail) {
        (&Method::GET, "events" | "events/history" | "events/replay") => true,
        (&Method::POST, "resume") => true,
        (&Method::POST, "tool-approval") => guest.can_approve,
        // After a reload the waiting card is fetched again; it lists only this session's
        // own pending calls, the same ones its events already carry.
        (&Method::GET, "pending") => guest.can_approve,
        _ => false,
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Remove a share now. `false` when no live share has that id.
fn revoke(id: &str) -> bool {
    let mut list = shares();
    let before = list.len();
    list.retain(|share| share.id != id);
    list.len() != before
}

// ---------- routes (owner only: `guest_allows` refuses them to guests) ----------

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CreateShareRequest {
    expires_in_secs: Option<u64>,
    can_approve: bool,
}

/// The LAN listener's address (`serve --lan`), when one is up.
pub(crate) static LAN_ADDR: std::sync::OnceLock<std::net::SocketAddr> = std::sync::OnceLock::new();

fn share_url(base: &str, token: &str, session_id: &str) -> String {
    format!(
        "{}/web#token={token}&session={}",
        base.trim_end_matches('/'),
        encode_component(session_id)
    )
}

fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

async fn create_share(
    Path(id): Path<String>,
    axum::extract::State(state): axum::extract::State<crate::api::DaemonState>,
    body: Option<Json<CreateShareRequest>>,
) -> Response {
    // The routes are mounted only on a host that turned shares on; a second check here keeps a
    // router assembled some other way from minting a credential.
    if !crate::host_options::guest_shares() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if state.get_session(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": {"code": "not_found", "message": "no live session with that id; open or resume it first"}})),
        )
            .into_response();
    }
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let ttl = body.expires_in_secs.unwrap_or(DEFAULT_TTL_SECS);
    match mint(&id, ttl, body.can_approve) {
        Ok((share_id, token, expires_at)) => {
            // The token is never logged: only the share id.
            tracing::info!(
                event = "guest_share_created",
                share = %share_id,
                session = %id,
                can_approve = body.can_approve,
                "guest share"
            );
            // The LAN listener is plain HTTP and refuses approval links, so
            // none is offered for one.
            let lan_url = LAN_ADDR
                .get()
                .filter(|_| !body.can_approve)
                .map(|addr| share_url(&format!("http://{addr}"), &token, &id));
            Json(serde_json::json!({
                "shareId": share_id,
                "sessionId": id,
                "token": token,
                "canApprove": body.can_approve,
                "expiresAt": expires_at,
                "path": share_url("", &token, &id),
                "lanUrl": lan_url,
            }))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": {"code": "share_failed", "message": message}})),
        )
            .into_response(),
    }
}

async fn list_shares() -> Json<serde_json::Value> {
    let mut list = shares();
    prune(&mut list);
    Json(serde_json::json!({
        "shares": list.iter().map(|share| serde_json::json!({
            "shareId": share.id,
            "sessionId": share.session_id,
            "canApprove": share.can_approve,
            "createdAt": share.created_at,
            "expiresAt": share.expires_at,
        })).collect::<Vec<_>>(),
        "lanAddress": LAN_ADDR.get().map(|addr| addr.to_string()),
    }))
}

async fn revoke_share(Path(id): Path<String>) -> Response {
    if !revoke(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    tracing::info!(event = "guest_share_revoked", share = %id, "guest share");
    Json(serde_json::json!({"revoked": true, "shareId": id})).into_response()
}

async fn whoami(guest: Option<Extension<GuestContext>>) -> Json<serde_json::Value> {
    match guest {
        Some(Extension(guest)) => Json(serde_json::json!({
            "guest": true,
            "shareId": guest.share_id,
            "sessionId": guest.session_id,
            "canApprove": guest.can_approve,
            "expiresAt": guest.expires_at,
        })),
        None => Json(serde_json::json!({"guest": false})),
    }
}

pub(crate) fn share_routes() -> Router<crate::api::DaemonState> {
    Router::new()
        .route("/sessions/{id}/shares", post(create_share))
        .route("/shares", get(list_shares))
        .route("/shares/{id}", delete(revoke_share))
        .route("/guest/whoami", get(whoami))
}

#[cfg(test)]
mod guest_pending_tests {
    use super::*;

    fn guest(can_approve: bool) -> GuestContext {
        GuestContext {
            share_id: "s".into(),
            session_id: "sess-1".into(),
            can_approve,
            expires_at: String::new(),
        }
    }

    #[test]
    fn only_an_approving_guest_may_fetch_its_own_sessions_pending_list() {
        assert!(guest_allows(
            &Method::GET,
            "/sessions/sess-1/pending",
            &guest(true)
        ));
        assert!(!guest_allows(
            &Method::GET,
            "/sessions/sess-1/pending",
            &guest(false)
        ));
        assert!(!guest_allows(
            &Method::GET,
            "/sessions/other/pending",
            &guest(true)
        ));
        assert!(!guest_allows(
            &Method::POST,
            "/sessions/sess-1/pending",
            &guest(true)
        ));
        // Read-only links still follow events and nothing more.
        assert!(guest_allows(
            &Method::GET,
            "/sessions/sess-1/events",
            &guest(false)
        ));
    }

    #[test]
    fn a_guest_reaches_nothing_outside_its_own_session() {
        let guest = guest(true);
        assert!(guest_allows(&Method::GET, "/guest/whoami", &guest));
        assert!(!guest_allows(&Method::POST, "/guest/whoami", &guest));
        assert!(!guest_allows(&Method::GET, "/shares", &guest));
        assert!(!guest_allows(&Method::DELETE, "/shares/x", &guest));
        assert!(!guest_allows(
            &Method::POST,
            "/sessions/sess-1/shares",
            &guest
        ));
        assert!(!guest_allows(
            &Method::POST,
            "/sessions/sess-1/messages",
            &guest
        ));
        assert!(!guest_allows(&Method::GET, "/sessions/sess-1", &guest));
        assert!(!guest_allows(&Method::GET, "/sessions//events", &guest));
        // The id is compared after percent-decoding, so an encoded other id is still refused.
        assert!(!guest_allows(
            &Method::GET,
            "/sessions/sess-%32/events",
            &guest
        ));
        assert!(guest_allows(
            &Method::GET,
            "/sessions/sess%2D1/events",
            &guest
        ));
    }

    #[test]
    fn a_minted_token_resolves_until_revoked_and_is_not_stored() {
        let (id, token, _) = mint("sess-mint", 3600, false).expect("mint");
        assert!(token.starts_with("g_"));
        let found = resolve(&token).expect("a live token resolves");
        assert_eq!(found.share_id, id);
        assert_eq!(found.session_id, "sess-mint");
        assert!(!found.can_approve);
        assert!(shares().iter().all(|share| share.token_hash != token));
        assert!(resolve("g_not-a-real-token").is_none());
        assert!(resolve(token.trim_start_matches("g_")).is_none());
        assert!(revoke(&id));
        assert!(resolve(&token).is_none());
        assert!(!revoke(&id));
    }

    #[test]
    fn a_lifetime_is_clamped_to_the_allowed_range() {
        mint("sess-ttl", 1, false).expect("mint");
        let list = shares();
        let share = list
            .iter()
            .find(|share| share.session_id == "sess-ttl")
            .expect("share");
        let left = share.deadline.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_secs(MIN_TTL_SECS - 5));
        assert!(left <= Duration::from_secs(MIN_TTL_SECS));
    }
}
