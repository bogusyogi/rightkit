//! Runs the daemon's axum `Router` behind RightKit's loopback host
//! (`rightkit_daemon::loopback`).
//!
//! The host owns the owner listener's Host check (a loopback name only), the Origin allow-list and
//! CORS, the bearer check, the per-caller-class request rate limit and connection handling. This
//! adapter hands every admitted request to the router (which keeps `require_token`: it
//! re-verifies the `Authorization` bearer, stamps the owner and resolves guest links), and
//! converts the router's answer back, streaming `text/event-stream` bodies chunk by chunk.
//!
//! Every host serves through this adapter. What differs per host: the guest-link verifier (only
//! on a host that turns `host_options::guest_shares` on), the legacy `x-{app}-token` header (only
//! on a host that turns `host_options::legacy_token_header` on), and the `?access_token=` carrier
//! on the event-stream route (only on a host that keeps `host_options::query_token_on_sse_route`).

use rightkit_daemon::loopback::{
    Authenticated, Body, CapabilityGrant, CapabilityVerifier, Limits, Loopback, LoopbackApp,
    LoopbackConfig, LoopbackToken, Method, OriginPolicy, Request, Response, RouteSpec, StreamSink,
};
use rightkit_daemon::ServiceError;

/// The largest response body the adapter collects (the host's `MAX_BODY_CEILING`).
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Response headers that describe one connection hop and are never forwarded to the host.
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Holds the daemon router once it is built; the host starts before the router exists.
pub(crate) struct RouterSlot {
    router: std::sync::OnceLock<axum::Router>,
    runtime: tokio::runtime::Handle,
    owner_token: String,
}

impl RouterSlot {
    pub(crate) fn new(runtime: tokio::runtime::Handle, owner_token: String) -> Self {
        Self {
            router: std::sync::OnceLock::new(),
            runtime,
            owner_token,
        }
    }

    /// Set the router once; later calls are ignored.
    pub(crate) fn bind(&self, router: axum::Router) {
        if self.router.set(router).is_err() {
            tracing::warn!("the daemon router was bound twice; the first one stays");
        }
    }

    /// Build the axum request the router serves from one host request.
    fn build_request(&self, request: &Request) -> Option<axum::http::Request<axum::body::Body>> {
        let mut builder = axum::http::Request::builder()
            .method(request.method.as_str())
            .uri(request.target.as_str());
        if let Some(headers) = builder.headers_mut() {
            for (name, value) in request.headers() {
                let (Ok(name), Ok(value)) = (
                    axum::http::HeaderName::from_bytes(name.as_bytes()),
                    axum::http::HeaderValue::from_str(value),
                ) else {
                    continue;
                };
                headers.append(name, value);
            }
            // A client can never supply the stamp. The adapter does not add it either:
            // `auth::require_token` re-verifies the bearer and stamps the owner itself, so a
            // Public or Capability request can never carry it.
            if let Some(name) = crate::auth::authenticated_header() {
                headers.remove(&name);
            }
        }
        builder
            .body(axum::body::Body::from(request.body.clone()))
            .ok()
    }

    /// Run one request through the router on the runtime. `handle` runs on a host thread, never
    /// on a runtime thread, so `block_on` is safe.
    fn call(
        &self,
        router: &axum::Router,
        req: axum::http::Request<axum::body::Body>,
    ) -> axum::response::Response {
        self.runtime
            .block_on(tower::ServiceExt::oneshot(router.clone(), req))
            .unwrap_or_else(|never| match never {})
    }
}

/// Resolves a presented guest link (`{app} share`) as the host's capability.
struct GuestVerifier;

impl CapabilityVerifier for GuestVerifier {
    fn verify(
        &self,
        _route: &str,
        presented: &str,
        request: &Request,
    ) -> Result<CapabilityGrant, ServiceError> {
        let Some(guest) = crate::guest_shares::resolve(presented) else {
            return Err(ServiceError::new("unauthenticated", "unauthorized"));
        };
        let Ok(method) = axum::http::Method::from_bytes(request.method.as_str().as_bytes()) else {
            return Err(ServiceError::new(
                "forbidden",
                "a guest link cannot do this",
            ));
        };
        if !crate::guest_shares::guest_allows(&method, &request.path, &guest) {
            return Err(ServiceError::new(
                "forbidden",
                "a guest link cannot do this",
            ));
        }
        Ok(CapabilityGrant {
            subject: guest.share_id.clone(),
            scopes: vec![guest.session_id.clone()],
        })
    }
}

fn response_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            if HOP_BY_HOP.contains(&name.as_str()) {
                return None;
            }
            Some((name, value.to_str().ok()?.to_owned()))
        })
        .collect()
}

impl LoopbackApp for RouterSlot {
    fn handle(&self, request: &Request, auth: &Authenticated) -> Response {
        let Some(router) = self.router.get() else {
            return Response::text(503, "daemon starting");
        };
        let Some(mut req) = self.build_request(request) else {
            return Response::text(400, "bad request");
        };
        match auth {
            Authenticated::Bearer => {
                // The host also admits the legacy `x-{app}-token` header and, on the event
                // stream route, `?access_token=`; the router only reads `Authorization`, so
                // present the owner bearer there.
                if !req
                    .headers()
                    .contains_key(axum::http::header::AUTHORIZATION)
                {
                    if let Ok(value) =
                        axum::http::HeaderValue::from_str(&format!("Bearer {}", self.owner_token))
                    {
                        req.headers_mut()
                            .insert(axum::http::header::AUTHORIZATION, value);
                    }
                }
            }
            // The guest's `Authorization` header stays as sent: `require_token` resolves the
            // same guest again and inserts `GuestContext`, so `guest_allows` is checked twice,
            // by design.
            Authenticated::Capability(_) => {}
            // The two signed trigger deliveries pass `require_token`'s own exemption and the
            // handler checks the HMAC and fails closed.
            Authenticated::Public => {}
        }
        let response = self.call(router, req);
        let (parts, body) = response.into_parts();
        let status = parts.status.as_u16();
        let headers = response_headers(&parts.headers);
        let is_stream = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if is_stream {
            let runtime = self.runtime.clone();
            return Response {
                status,
                headers,
                body: Body::Stream(Box::new(move |sink: &mut dyn StreamSink| {
                    let mut data = body.into_data_stream();
                    while let Some(Ok(chunk)) =
                        runtime.block_on(futures_util::StreamExt::next(&mut data))
                    {
                        if sink.send(&chunk).is_err() {
                            break;
                        }
                    }
                })),
            };
        }
        match self
            .runtime
            .block_on(axum::body::to_bytes(body, MAX_RESPONSE_BYTES))
        {
            Ok(bytes) => Response {
                status,
                headers,
                body: Body::Bytes(bytes.to_vec()),
            },
            Err(_) => Response::text(502, "the daemon could not read its own response"),
        }
    }

    /// The readiness body is the router's own `GET /health` answer (fixed by client contract
    /// R37); before the router is bound the daemon reports that it is starting.
    fn health(&self) -> serde_json::Value {
        let Some(router) = self.router.get() else {
            return serde_json::json!({"ok": true, "starting": true});
        };
        let Ok(req) = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .body(axum::body::Body::empty())
        else {
            return serde_json::json!({"ok": true});
        };
        let response = self.call(router, req);
        match self.runtime.block_on(axum::body::to_bytes(
            response.into_body(),
            MAX_RESPONSE_BYTES,
        )) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({"ok": true}))
            }
            Err(_) => serde_json::json!({"ok": true}),
        }
    }
}

/// The one route that may carry the bearer as `?access_token=` (a browser `EventSource` cannot
/// set a header), for a host that keeps that carrier.
const SSE_ROUTE: &str = "/sessions/{id}/events";

/// Start the owner listener on `port` with `token` as the owner bearer.
///
/// No `.rate_limit(..)` call: the host's default (on, 200 per second, burst 400, one bucket for
/// requests presenting the owner token and one shared by every other caller) applies. The LAN
/// listener keeps its own limiter (`auth::lan_rate_limit`). No record path, labels, presence or
/// busy probe: the daemon writes its own discovery record and keeps its own `/clients` routes
/// and idle exit.
pub(crate) fn start(
    port: u16,
    token: &str,
    slot: std::sync::Arc<RouterSlot>,
) -> std::io::Result<Loopback> {
    let mut config = LoopbackConfig::new()
        .port(port)
        .token(LoopbackToken::from_string(token).map_err(std::io::Error::other)?)
        .max_connections(1024)
        .route(RouteSpec::public(
            Method::Post,
            "/triggers/{id}/deliver/github",
        ))
        .route(RouteSpec::public(
            Method::Post,
            "/triggers/{id}/deliver/generic",
        ))
        .limits(Limits::default())
        .origin_policy(origin_policy());
    if crate::host_options::legacy_token_header() {
        config = config.legacy_token_header(&crate::host_identity::header("token"));
    }
    if crate::host_options::guest_shares() {
        // The owner bearer or a guest-link bearer on the same session routes.
        config = config
            .capability_verifier(std::sync::Arc::new(GuestVerifier))
            .bearer_grants(true);
    }
    if crate::host_options::query_token_on_sse_route() {
        config = config.route(RouteSpec::bearer_query_token(SSE_ROUTE));
    }
    Loopback::start(config, slot)
}

/// The browser origins and request headers the clients use; a header missing here fails the
/// browser preflight.
fn origin_policy() -> OriginPolicy {
    let mut policy = OriginPolicy::new();
    for origin in crate::api::allowed_origins() {
        policy = policy.allow_origin(origin);
    }
    for header in [
        "authorization".to_string(),
        "content-type".to_string(),
        "accept".to_string(),
        "last-event-id".to_string(),
        crate::host_identity::header("trusted-source"),
        crate::host_identity::header("capability-token"),
        crate::host_identity::header("driver-instance"),
        crate::host_identity::header("client-id"),
        crate::host_identity::header("source-trust"),
    ] {
        policy = policy.allow_request_header(&header);
    }
    if crate::host_options::legacy_token_header() {
        policy = policy.allow_request_header(&crate::host_identity::header("token"));
    }
    policy
}

#[cfg(test)]
mod tests {
    use super::{response_headers, HOP_BY_HOP};

    #[test]
    fn connection_hop_headers_are_not_forwarded() {
        let mut headers = axum::http::HeaderMap::new();
        for name in HOP_BY_HOP {
            headers.insert(
                axum::http::HeaderName::from_static(name),
                axum::http::HeaderValue::from_static("x"),
            );
        }
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        assert_eq!(
            response_headers(&headers),
            vec![("content-type".to_string(), "application/json".to_string())]
        );
    }
}
