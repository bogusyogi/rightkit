//! Daemon HTTP auth + LAN rate limiting.
//!
//! The owner listener's Host check, CORS, bearer check for the owner and request-rate limit are
//! the loopback host's (rightkit-daemon). A single `require_token` layer gates every route
//! (applied in `api::build_router`; the loopback host calls the router behind it), with a small,
//! explicit exemption set (`/health`, CORS preflight, and the signed webhook delivery endpoints).
//! It also resolves guest links. This file holds the LAN listener's request-rate limiter.
//! Extracted from `api.rs` so the auth surface, the entire unauthenticated boundary, is
//! auditable in isolation.

use crate::api::lock_recover;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::{Arc, Mutex};

// The exact set of GET routes that open an `EventSource`/SSE stream and
// therefore cannot set an `Authorization` header. This is the ONLY route
// family allowed to carry the bearer token as a `?access_token=` query
// param — enumerated from the router assembly in `api_sessions.rs`
// (`sessions_routes()`), not substring-matched. Keep in sync with that file.

/// Returns true for a request path that is a real SSE stream route (matches
/// the router's actual `/sessions/{id}/events` shape — a path ending in
/// `/events` with no further segments, never `/events/history` or any other
/// suffixed variant).
fn is_sse_route(path: &str) -> bool {
    let segments = path
        .strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .collect::<Vec<_>>();
    matches!(segments.as_slice(), ["sessions", id, "events"] if !id.is_empty())
}

/// Only these exact POST shapes may reach a handler without the daemon bearer.
/// The handler then requires the trigger-specific HMAC and fails closed. Admin
/// routes and lookalike/suffixed paths remain behind the bearer token.
fn is_signed_trigger_delivery_route(method: &Method, path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    let segments = path
        .strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .collect::<Vec<_>>();
    matches!(
        segments.as_slice(),
        ["triggers", id, "deliver", "github" | "generic"] if !id.is_empty()
    )
}

/// The header the daemon sets on a request that carried the owner bearer; routes that need proof
/// of the owner's identity (`/adapt/proposals/apply`) read it. Never set for a guest or on the
/// LAN. Named in the host's namespace: `x-{app}-authenticated`.
pub(crate) fn authenticated_header() -> Option<axum::http::HeaderName> {
    axum::http::HeaderName::from_bytes(crate::host_identity::header("authenticated").as_bytes())
        .ok()
}

/// Auth middleware: every API request must carry the daemon's launch token.
/// `Authorization: Bearer <token>` is the canonical HTTP carrier.
/// The only other carrier is the `access_token` query parameter, ONLY on the SSE stream route
///   (`/sessions/{id}/events`), which is opened by `EventSource` and cannot
///   set headers, and only for a host that keeps it
///   (`host_options::query_token_on_sse_route`). Every other route is header-only: a
///   query-param token on a non-SSE route is rejected with the same 401 shape as a missing token,
///   because query strings leak into logs/history/proxies.
///
/// Exactly three things pass without the token: CORS preflight (`OPTIONS`, which carries no
/// state), `GET /health` (the readiness probe that runs before a client holds a token) and the
/// two signed trigger delivery paths (the handler then requires the HMAC). `POST /health` and
/// every other method on `/health` need the token. An empty configured token disables the gate
/// (used only by tests that call handlers directly; the real daemon always generates one).
///
/// A request that carried the owner bearer is stamped with [`authenticated_header`]. Any stamp a
/// client sent is dropped first, so only this layer can set it. The owner token is refused on the
/// LAN listener, which serves guest links only (`crate::guest_shares`).
pub(crate) async fn require_token(
    State(expected): State<String>,
    mut req: Request,
    next: Next,
) -> Response {
    // A client may never carry the daemon's own "authenticated" stamp: drop it from every
    // request before anything else, then set it again only after the owner bearer check below.
    let stamp = authenticated_header();
    if let Some(name) = &stamp {
        req.headers_mut().remove(name);
    }
    if expected.is_empty()
        || req.method() == Method::OPTIONS
        || is_health_probe(req.method(), req.uri().path())
        || is_signed_trigger_delivery_route(req.method(), req.uri().path())
    {
        return next.run(req).await;
    }
    let path_is_sse = is_sse_route(req.uri().path());
    let query_token_allowed = path_is_sse && crate::host_options::query_token_on_sse_route();
    // Extract the token into an owned value so the borrows on `req` end before
    // `req` is moved into `next.run`.
    let provided: Option<String> = {
        let headers = req.headers();
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        header("authorization")
            .and_then(|a| a.strip_prefix("Bearer ").map(str::to_owned))
            .or_else(|| {
                if !query_token_allowed {
                    return None;
                }
                req.uri().query().and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("access_token="))
                        .map(str::to_owned)
                })
            })
    };
    let on_lan = req
        .extensions()
        .get::<crate::guest_shares::LanOrigin>()
        .is_some();
    match provided {
        Some(token) if rightkit_crypto::ct_eq(token.as_bytes(), expected.as_bytes()) => {
            // The owner token never crosses the network in clear text: the LAN
            // listener (`serve --lan`) serves guest links only.
            if on_lan {
                return (
                    StatusCode::FORBIDDEN,
                    "the owner token is refused on the LAN listener; use the loopback address or a guest link",
                )
                    .into_response();
            }
            if let Some(name) = stamp {
                req.headers_mut()
                    .insert(name, axum::http::HeaderValue::from_static("true"));
            }
            next.run(req).await
        }
        // Guest links: one session, read and (if granted) approve, nothing else.
        Some(token) => match crate::guest_shares::resolve(&token) {
            Some(guest) => {
                if !crate::guest_shares::guest_allows(req.method(), req.uri().path(), &guest) {
                    return (StatusCode::FORBIDDEN, "a guest link cannot do this").into_response();
                }
                req.extensions_mut().insert(guest);
                next.run(req).await
            }
            None => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
        },
        None => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

/// The one unauthenticated read: `GET /health`.
fn is_health_probe(method: &Method, path: &str) -> bool {
    *method == Method::GET && path == "/health"
}

struct Bucket {
    tokens: f64,
    last: std::time::Instant,
}

const RATE_PER_SEC: f64 = 200.0;
const BURST: f64 = 400.0;

/// A coarse token-bucket rate limiter for the LAN listener only: 200 requests per second with a
/// burst of 400. It exists to stop a runaway or hostile network client from pinning the daemon.
/// The owner listener's limiter is the loopback host's.
#[derive(Clone)]
pub(crate) struct LanRateLimiter {
    inner: Arc<Mutex<Bucket>>,
}

impl LanRateLimiter {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Bucket {
                tokens: BURST,
                last: std::time::Instant::now(),
            })),
        }
    }
}

pub(crate) async fn lan_rate_limit(
    State(rl): State<LanRateLimiter>,
    req: Request,
    next: Next,
) -> Response {
    {
        let mut b = lock_recover(&rl.inner, "rl.inner");
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.last = now;
        b.tokens = (b.tokens + elapsed * RATE_PER_SEC).min(BURST);
        if b.tokens < 1.0 {
            return (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
        }
        b.tokens -= 1.0;
    }
    next.run(req).await
}

#[cfg(test)]
mod owner_stamp_tests {
    use super::{authenticated_header, require_token};
    use axum::{body::Body, http::Request, middleware, routing::post, Router};
    use tower::ServiceExt;

    /// A route that answers the stamp it saw, behind the real auth layer.
    fn app() -> Router {
        Router::new()
            .route(
                "/stamp",
                post(|headers: axum::http::HeaderMap| async move {
                    let name = authenticated_header().expect("header name");
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("none")
                        .to_string()
                }),
            )
            .layer(middleware::from_fn_with_state(
                "owner-token".to_string(),
                require_token,
            ))
    }

    async fn call(token: Option<&str>, spoof: bool) -> (u16, String) {
        let mut request = Request::builder().method("POST").uri("/stamp");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if spoof {
            request = request.header(authenticated_header().expect("header name"), "true");
        }
        let response = app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    #[tokio::test]
    async fn only_the_owner_bearer_earns_the_authenticated_stamp() {
        assert_eq!(call(None, false).await.0, 401);
        assert_eq!(call(None, true).await.0, 401);
        assert_eq!(call(Some("wrong"), true).await.0, 401);
        assert_eq!(
            call(Some("owner-token"), false).await,
            (200, "true".to_string())
        );
        // A client-supplied stamp is dropped before the check and set again from the real token.
        assert_eq!(
            call(Some("owner-token"), true).await,
            (200, "true".to_string())
        );
    }
}
