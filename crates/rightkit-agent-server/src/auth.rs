//! Daemon HTTP auth + rate limiting.
//!
//! A single global `require_token` layer gates every route (applied in
//! `api::build_router`), with a small, explicit exemption set (`/health`, CORS
//! preflight, and the signed webhook delivery endpoints). Extracted from `api.rs` so the
//! auth surface — the entire unauthenticated boundary — is auditable in isolation.

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

/// Auth middleware: every API request must carry the daemon's launch token.
/// `Authorization: Bearer <token>` is the canonical HTTP carrier.
/// The only other carrier is the `access_token` query parameter — ONLY on the SSE stream route
///   (`/sessions/{id}/events`), which is opened by `EventSource` and cannot
///   set headers. Every other route is header-only: a query-param token on
///   a non-SSE route is rejected with the same 401 shape as a missing token,
///   because query strings leak into logs/history/proxies.
///
/// `/health` (the connectivity probe) and CORS preflight are exempt. An empty
/// configured token disables the gate (used only by tests that call handlers
/// directly; the real daemon always generates one).
pub(crate) async fn require_token(
    State(expected): State<String>,
    req: Request,
    next: Next,
) -> Response {
    if expected.is_empty()
        || req.method() == Method::OPTIONS
        || req.uri().path() == "/health"
        || is_signed_trigger_delivery_route(req.method(), req.uri().path())
    {
        return next.run(req).await;
    }
    let path_is_sse = is_sse_route(req.uri().path());
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
                if !path_is_sse {
                    return None;
                }
                req.uri().query().and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("access_token="))
                        .map(str::to_owned)
                })
            })
    };
    match provided {
        Some(token) if rightkit_crypto::ct_eq(token.as_bytes(), expected.as_bytes()) => {
            next.run(req).await
        }
        _ => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

/// A coarse global token-bucket rate limiter. The daemon is single-user on
/// localhost, so this exists only to stop a runaway client loop from pinning the
/// process — the limits are far above any legitimate burst (the UI's heaviest
/// action, loading every Runtime panel, is well under ten concurrent calls).
#[derive(Clone)]
pub(crate) struct RateLimiter {
    inner: Arc<Mutex<Bucket>>,
}

struct Bucket {
    tokens: f64,
    last: std::time::Instant,
}

const RATE_PER_SEC: f64 = 200.0;
const BURST: f64 = 400.0;

impl RateLimiter {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Bucket {
                tokens: BURST,
                last: std::time::Instant::now(),
            })),
        }
    }
}

pub(crate) async fn rate_limit(
    State(rl): State<RateLimiter>,
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
