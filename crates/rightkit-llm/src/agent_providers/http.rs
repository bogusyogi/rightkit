//! Provider HTTP transport: a thin adapter over `rightkit-http`'s `AsyncClient`.
//!
//! All socket, TLS, proxy, redirect, address-policy and timeout plumbing lives in
//! `rightkit-http`. This module only wires its types into the provider crate's own
//! model: one client configuration shared by every provider client, and a
//! response wrapper that keeps the status, headers and a typed body stream so the
//! SSE decoders and the submission ladders (`crate::agent_providers::transport`, `crate::agent_providers::client`)
//! can stay protocol-specific.
//!
//! Retries stay in the host app's ladders (`crate::agent_providers::transport`, `crate::agent_providers::client`), which
//! run on `RetryPolicy::none()`. rightkit-http 0.3.6 already refuses POST status
//! retries by default and retries non-idempotent transport errors only in the connect
//! phase, so a delivered request is not replayed by the library. Its own retry is not
//! adopted here because the ladders also carry stream-phase reconnect and the
//! `notify_retry` events that rightkit-http does not emit.

use bytes::Bytes;
use futures::Stream;
use rightkit_http::{
    AddressPolicy, AsyncClient, AsyncStreamResponse, ClientConfig, HttpError, ProxyMode, Request,
    RetryPolicy,
};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

/// Body chunks with typed errors (deadline, cancellation and transport errors are
/// already mapped by `rightkit-http`).
pub type BodyChunks = Pin<Box<dyn Stream<Item = Result<Bytes, HttpError>> + Send>>;

/// Per-request ceiling kept from the previous reqwest client: a hung provider
/// call must fail loudly instead of blocking the engine (or a sub-agent's
/// conductor) forever.
const REQUEST_CEILING: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest error body kept for diagnostics.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Host named by a provider base URL, when it parses.
pub(crate) fn base_url_host(base_url: &str) -> Option<String> {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

/// How a provider client reaches its endpoint.
pub(crate) enum Route<'a> {
    /// Ordinary route: environment proxy honoured, destination resolved by name.
    Ambient,
    /// Like `Ambient`, but no proxy is ever used (credentials go to the host only).
    NoProxy,
    /// Direct connection: no proxy; the host resolves only to these addresses
    /// (already validated by the daemon's connection policy).
    Pinned(&'a [SocketAddr]),
}

/// Build the shared provider HTTP client for `base_url`.
///
/// The configured endpoint host is the operator's own choice (local models, LAN
/// gateways), so it is the one host allowed to resolve to a non-public address.
/// Redirects are never followed and retries are off: callers drive attempts.
pub(crate) fn provider_client(base_url: &str, route: Route<'_>) -> AsyncClient {
    provider_client_with_ceiling(base_url, route, REQUEST_CEILING)
}

/// [`provider_client`] with a caller-chosen whole-request ceiling.
pub(crate) fn provider_client_with_ceiling(
    base_url: &str,
    route: Route<'_>,
    ceiling: Duration,
) -> AsyncClient {
    let mut policy = AddressPolicy::public_web();
    let host = base_url_host(base_url);
    match &route {
        Route::Ambient | Route::NoProxy => {
            if let Some(host) = host.clone() {
                policy = policy.allow_if(vec![host], |_| true);
            }
        }
        Route::Pinned(addrs) => {
            let ips: Vec<IpAddr> = addrs.iter().map(SocketAddr::ip).collect();
            if let Some(host) = host.clone() {
                if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
                    // An IP-literal endpoint is connected as written; the daemon
                    // already validated it.
                    policy = policy.allow_if(vec![host], |_| true);
                } else {
                    let allowed = ips.clone();
                    policy = policy
                        .allow_if(vec![host], move |ip| allowed.contains(ip))
                        .with_resolver(move |_| ips.clone());
                }
            }
        }
    }
    let proxy = match route {
        Route::Ambient => ProxyMode::Environment,
        Route::NoProxy | Route::Pinned(_) => ProxyMode::None,
    };
    let config = ClientConfig {
        user_agent: "Agent/0.1".into(),
        connect_timeout: Some(CONNECT_TIMEOUT),
        request_timeout: ceiling,
        response_timeout: Some(ceiling),
        stream_timeout: Some(ceiling),
        retry: RetryPolicy::none(),
        address_policy: policy,
        proxy,
        ..ClientConfig::default()
    };
    AsyncClient::new(config.clone()).unwrap_or_else(|_| {
        // An unusable environment proxy setting must not take the provider down.
        AsyncClient::new(ClientConfig {
            proxy: ProxyMode::None,
            ..config
        })
        .expect("rightkit-http client builds with the platform verifier")
    })
}

/// One physical response: status, headers and an unread body stream.
pub struct ProviderHttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ProviderBody,
}

/// The unread body: the live response, or (tests only) fixed chunks.
enum ProviderBody {
    Live(AsyncStreamResponse),
    #[cfg(test)]
    Fixed(BodyChunks),
}

impl std::fmt::Debug for ProviderHttpResponse {
    /// Status and header count only: headers can carry credentials and the body is a stream.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderHttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers.len())
            .finish_non_exhaustive()
    }
}

impl ProviderHttpResponse {
    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Response headers, names lowercased.
    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        header_value(&self.headers, name)
    }

    /// The body as a chunk stream; the response is consumed.
    pub(crate) fn bytes_stream(self) -> BodyChunks {
        match self.body {
            ProviderBody::Live(response) => Box::pin(response.into_stream()),
            #[cfg(test)]
            ProviderBody::Fixed(chunks) => chunks,
        }
    }

    /// The body as text, bounded to 64 KiB (error bodies are diagnostics, never data).
    pub(crate) async fn text(self) -> Result<String, HttpError> {
        match self.body {
            ProviderBody::Live(response) => response.read_text(MAX_ERROR_BODY_BYTES).await,
            #[cfg(test)]
            ProviderBody::Fixed(mut chunks) => {
                let mut out: Vec<u8> = Vec::new();
                while let Some(chunk) = futures::StreamExt::next(&mut chunks).await {
                    out.extend_from_slice(&chunk?);
                }
                out.truncate(MAX_ERROR_BODY_BYTES);
                Ok(String::from_utf8_lossy(&out).into_owned())
            }
        }
    }

    /// A response with a fixed body, for tests that classify status and headers.
    #[cfg(test)]
    pub(crate) fn from_parts(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Self {
        let chunk = Bytes::copy_from_slice(body);
        Self {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), (*v).to_string()))
                .collect(),
            body: ProviderBody::Fixed(Box::pin(futures::stream::iter(vec![Ok(chunk)]))),
        }
    }
}

/// Send one attempt and return the response whatever its status; only transport,
/// policy, timeout and cancellation failures are errors.
pub(crate) async fn send(
    client: &AsyncClient,
    request: Request,
) -> Result<ProviderHttpResponse, HttpError> {
    let response = client.stream_any(&request).await?;
    let status = response.status;
    let headers = response.headers.clone();
    Ok(ProviderHttpResponse {
        status,
        headers,
        body: ProviderBody::Live(response),
    })
}

/// Case-insensitive header lookup over `(name, value)` pairs.
pub(crate) fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Header value as an unsigned integer.
pub(crate) fn header_u64(headers: &[(String, String)], name: &str) -> Option<u64> {
    header_value(headers, name)?.trim().parse().ok()
}
