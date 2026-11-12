//! Shared outbound HTTP for the `coderight` binary: a thin adapter over `rightkit-http`.
//!
//! Socket, TLS, proxy, redirect, address-policy, timeout and body-limit plumbing all
//! belong to `rightkit-http`'s `AsyncClient`. This module only picks the RightKit
//! settings for the three kinds of caller the binary has:
//!
//! - [`daemon_client`]: the CLI/TUI talking to a RightKit daemon (loopback or an
//!   operator-configured host). The endpoint's own host is the one host allowed to
//!   resolve to a non-public address; no proxy is ever used; no redirect is followed.
//! - [`endpoint_client`]: one operator-configured service (local model runtime,
//!   Telegram relay, GitHub forge): same host exemption, environment proxy honoured.
//! - [`public_client`]: arbitrary public web URLs; no host is exempt from the
//!   public-address policy, redirects (up to 10) are re-vetted per hop.
//!
//! Retries are off everywhere (`RetryPolicy::none()`): each call is one request, as
//! one buffered request per call.

use rightkit_http::{
    AddressPolicy, AsyncClient, ClientConfig, ProxyMode, Request, Response, RetryPolicy,
};
use std::time::Duration;

/// Whole-request ceiling for a buffered call when the caller names none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Ceiling for a buffered daemon call (a prompt or export can run long).
pub const DAEMON_CALL_TIMEOUT: Duration = Duration::from_secs(600);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Host named by `url`, when it parses.
pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
}

fn policy_for(url: &str) -> AddressPolicy {
    let mut policy = AddressPolicy::public_web();
    if let Some(host) = host_of(url) {
        policy = policy.allow_if(vec![host], |_| true);
    }
    policy
}

fn build(config: ClientConfig) -> Result<AsyncClient, String> {
    AsyncClient::new(config.clone())
        .or_else(|_| {
            // An unusable environment proxy setting must not disable the call.
            AsyncClient::new(ClientConfig {
                proxy: ProxyMode::None,
                ..config
            })
        })
        .map_err(|error| error.to_string())
}

fn config(url: &str, timeout: Duration, proxy: ProxyMode, max_redirects: u32) -> ClientConfig {
    ClientConfig {
        user_agent: "RightKit/0.1".into(),
        connect_timeout: Some(CONNECT_TIMEOUT),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        // Streams (SSE event feeds) have no total lifetime cap; only the time to
        // response headers is bounded.
        stream_timeout: None,
        retry: RetryPolicy::none(),
        address_policy: policy_for(url),
        proxy,
        max_redirects,
        ..ClientConfig::default()
    }
}

/// Client for a RightKit daemon at `base_url`.
pub fn daemon_client(base_url: &str) -> Result<AsyncClient, String> {
    daemon_client_with(base_url, DAEMON_CALL_TIMEOUT)
}

/// [`daemon_client`] with a caller-chosen whole-request budget (health probes).
pub fn daemon_client_with(base_url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    build(config(base_url, timeout, ProxyMode::None, 0))
}

/// [`daemon_client`] for liveness probes and presence calls: short, separate connect and
/// whole-request budgets.
pub fn daemon_client_probe(
    base_url: &str,
    connect: Duration,
    total: Duration,
) -> Result<AsyncClient, String> {
    build(ClientConfig {
        connect_timeout: Some(connect),
        ..config(base_url, total, ProxyMode::None, 0)
    })
}

/// Client for one operator-configured service at `url`; environment proxy honoured,
/// redirects never followed.
pub fn endpoint_client(url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    build(config(url, timeout, ProxyMode::Environment, 0))
}

/// [`endpoint_client`] that follows up to 10 redirects (the redirect budget the product has always used). The
/// named host stays exempt; every redirect hop to another host is vetted as public.
pub fn endpoint_client_following(url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    build(config(url, timeout, ProxyMode::Environment, 10))
}

/// Client for a Direct inference connection: no proxy, no redirects, and the host resolves
/// only to `addresses` (already validated by the daemon's connection policy). An
/// IP-literal endpoint is connected as written.
pub fn pinned_client(
    base_url: &str,
    addresses: &[std::net::SocketAddr],
    connect: Duration,
    total: Duration,
) -> Result<AsyncClient, String> {
    let ips: Vec<std::net::IpAddr> = addresses.iter().map(std::net::SocketAddr::ip).collect();
    let mut policy = AddressPolicy::public_web();
    if let Some(host) = host_of(base_url) {
        if host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
        {
            policy = policy.allow_if(vec![host], |_| true);
        } else {
            let allowed = ips.clone();
            policy = policy
                .allow_if(vec![host], move |ip| allowed.contains(ip))
                .with_resolver(move |_| ips.clone());
        }
    }
    build(ClientConfig {
        connect_timeout: Some(connect),
        address_policy: policy,
        ..config(base_url, total, ProxyMode::None, 0)
    })
}

/// Client for arbitrary public web URLs: public addresses only, up to 10 redirects,
/// every hop vetted.
pub fn public_client(timeout: Duration) -> Result<AsyncClient, String> {
    build(ClientConfig {
        address_policy: AddressPolicy::public_web(),
        ..config("", timeout, ProxyMode::Environment, 10)
    })
}

/// Send one buffered request; any status is a response, only transport, policy and
/// timeout failures are errors.
pub async fn send_any(client: &AsyncClient, request: &Request) -> Result<Response, String> {
    client
        .send_any(request)
        .await
        .map_err(|error| error.to_string())
}

/// `true` for a 2xx status.
pub fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// One buffered daemon call: `method` on `url` with the daemon bearer `token`; `body`,
/// when present, is sent as JSON. Any status is a response; only transport, policy
/// and timeout failures are errors (as a display string).
pub async fn daemon_call(
    method: rightkit_http::Method,
    url: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<Response, String> {
    let client = daemon_client(url)?;
    let mut request = Request::new(method, url).bearer(token);
    if let Some(body) = body {
        request = request.json(body).map_err(|error| error.to_string())?;
    }
    send_any(&client, &request).await
}

/// [`daemon_call`] with GET.
pub async fn daemon_get(url: &str, token: &str) -> Result<Response, String> {
    daemon_call(rightkit_http::Method::Get, url, token, None).await
}
