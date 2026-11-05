//! Shared outbound HTTP for the tools crate: a thin adapter over `rightkit-http`.
//!
//! Timeouts, address policy, proxy handling, redirect hops and body limits all
//! belong to `rightkit-http`; this module only picks the the host app settings for
//! "call one configured endpoint once":
//!
//! - the endpoint's own host is the one host allowed to resolve to a non-public
//!   address (local fakes and operator-configured gateways); every other
//!   destination, including redirect targets, passes the public-web policy;
//! - retries are off (`RetryPolicy::none()`), so a tool call is exactly one
//!   request, as before;
//! - the environment proxy is honoured, as the previous reqwest clients did.

use rightkit_http::{
    AddressPolicy, AsyncClient, ClientConfig, ProxyMode, Request, Response, RetryPolicy,
};
use std::time::Duration;

/// Whole-request budget used when a caller names none.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Host named by `url`, when it parses.
pub(crate) fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
}

/// `true` when `host` (as `Url::host_str` gives it) is `localhost` or a loopback IP literal.
fn names_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn config_for(url: &str, timeout: Duration, max_redirects: u32) -> ClientConfig {
    let mut policy = AddressPolicy::public_web();
    if let Some(host) = host_of(url) {
        policy = policy.allow_if(vec![host], |_| true);
    }
    ClientConfig {
        user_agent: "Agent/0.1".into(),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        retry: RetryPolicy::none(),
        address_policy: policy,
        proxy: ProxyMode::Environment,
        max_redirects,
        ..ClientConfig::default()
    }
}

fn build(config: ClientConfig) -> Result<AsyncClient, String> {
    AsyncClient::new(config.clone())
        .or_else(|_| {
            // An unusable environment proxy setting must not disable the tool.
            AsyncClient::new(ClientConfig {
                proxy: ProxyMode::None,
                ..config
            })
        })
        .map_err(|error| error.to_string())
}

/// Client for one endpoint; redirects are never followed.
pub(crate) fn client_for(url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    build(config_for(url, timeout, 0))
}

/// Client for one endpoint that follows up to 10 redirects (the earlier default).
pub(crate) fn client_following(url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    build(config_for(url, timeout, 10))
}

/// Client for one page of a crawl started from a user-supplied URL. The crawl's SSRF guard
/// (`browser_fetch::ssrf_guard_reject_reason`) allows loopback, and `browser_fetch` gives the
/// URL's own host the same local-dev carve-out; this client matches that. Loopback only, and
/// only the page's own host: every other private or public target keeps the default refusal.
pub(crate) fn crawl_page_client(url: &str, timeout: Duration) -> Result<AsyncClient, String> {
    let mut policy = AddressPolicy::public_web();
    // Only a URL that names loopback itself (`localhost` or a loopback literal) gets the
    // carve-out, so a public name that resolves to 127.0.0.1 (DNS rebinding) stays refused.
    if let Some(host) = host_of(url).filter(|host| names_loopback(host)) {
        policy = policy.allow_if(vec![host], |ip| ip.is_loopback());
    }
    build(ClientConfig {
        user_agent: "Agent/0.1".into(),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        retry: RetryPolicy::none(),
        address_policy: policy,
        proxy: ProxyMode::Environment,
        max_redirects: 10,
        ..ClientConfig::default()
    })
}

/// Send one buffered request and return the response whatever its status; only
/// transport, policy and timeout failures are errors.
pub(crate) async fn send_any(client: &AsyncClient, request: &Request) -> Result<Response, String> {
    client
        .send_any(request)
        .await
        .map_err(|error| error.to_string())
}

/// `true` for a 2xx status.
pub(crate) fn is_success(response: &Response) -> bool {
    (200..300).contains(&response.status)
}
