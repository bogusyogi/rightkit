use rightkit_http::{AddressPolicy, Client, ClientConfig, ProxyMode, Request};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ModelGrantRequest<'a> {
    pub worker_base: &'a str,
    pub route: &'a str,
    pub model: &'a str,
    pub platform: &'a str,
    pub sha256: &'a str,
    pub device_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantRequestBody<'a> {
    route: &'a str,
    model: &'a str,
    platform: &'a str,
    sha256: &'a str,
    device_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantResponse {
    token: String,
    expires_at: u64,
    route: String,
    sha256: String,
}

/// A worker-issued, route-and-hash-scoped download token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGrant {
    pub token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

impl ModelGrant {
    /// `Authorization` header value.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// True when the token is expired or expires within `skew` seconds.
    pub fn expires_within(&self, skew: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        self.expires_at <= now.saturating_add(skew)
    }
}

/// Host of an `https://` worker base, for pinning. Credentials in the URL, a
/// non-https scheme and an empty host are refused.
fn grant_host(worker_base: &str) -> Result<String, String> {
    let base = worker_base.trim();
    let rest = match base.get(..8) {
        Some(scheme) if scheme.eq_ignore_ascii_case("https://") => &base[8..],
        _ => return Err("model grant endpoint must use https".to_string()),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("model grant endpoint must not carry credentials".to_string());
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    if host.is_empty() {
        return Err("model grant endpoint has no host".to_string());
    }
    Ok(host.to_string())
}

/// First-party policy for the grant host of `worker_base`: that one host, over
/// HTTPS only, on every redirect hop, resolving to public addresses.
pub fn grant_policy(worker_base: &str) -> Result<AddressPolicy, String> {
    Ok(AddressPolicy::first_party(vec![grant_host(worker_base)?]))
}

/// Client for grant requests to `worker_base`: pinned to its host by
/// [`grant_policy`], environment proxies, redirects never followed (the default), and the grant
/// request is the only thing it sends.
pub fn grant_client(worker_base: &str) -> Result<Client, String> {
    let mut config = ClientConfig::default()
        .address_policy(grant_policy(worker_base)?)
        // Like downloads: environment proxies, destination still vetted.
        .proxy(ProxyMode::Environment);
    config.user_agent = concat!("rightkit-models/", env!("CARGO_PKG_VERSION")).to_string();
    Ok(Client::new(config))
}

/// Retired entry point. `agent` is ignored: the request goes through
/// [`grant_client`], pinned to the worker host over HTTPS.
#[deprecated(since = "0.2.9", note = "use `acquire_model_grant_pinned`")]
pub fn acquire_model_grant(
    _agent: &ureq::Agent,
    request: ModelGrantRequest<'_>,
) -> Result<String, String> {
    acquire_model_grant_pinned(request)
}

/// Retired entry point. `agent` is ignored: the request goes through
/// [`grant_client`], pinned to the worker host over HTTPS.
#[deprecated(since = "0.2.9", note = "use `acquire_grant_pinned`")]
pub fn acquire_grant(
    _agent: &ureq::Agent,
    request: ModelGrantRequest<'_>,
) -> Result<ModelGrant, String> {
    acquire_grant_pinned(request)
}

/// The bearer token of a grant for `request`; see [`acquire_grant_pinned`].
pub fn acquire_model_grant_pinned(request: ModelGrantRequest<'_>) -> Result<String, String> {
    acquire_grant_pinned(request).map(|grant| grant.token)
}

/// Ask the worker at `request.worker_base` for a grant, keeping the expiry so
/// callers can refresh. The request goes through a client pinned to that host
/// over HTTPS ([`grant_client`]).
pub fn acquire_grant_pinned(request: ModelGrantRequest<'_>) -> Result<ModelGrant, String> {
    let client = grant_client(request.worker_base)?;
    acquire_grant_with_client(&client, request)
}

/// As [`acquire_grant_pinned`] through a caller-supplied client. The caller owns
/// the address policy: build it with [`grant_client`] unless the worker is an
/// explicitly allowed private host.
pub fn acquire_grant_with_client(
    client: &Client,
    request: ModelGrantRequest<'_>,
) -> Result<ModelGrant, String> {
    let endpoint = format!(
        "{}/v1/model-grants",
        request.worker_base.trim_end_matches('/')
    );
    let body = serde_json::to_string(&GrantRequestBody {
        route: request.route,
        model: request.model,
        platform: request.platform,
        sha256: request.sha256,
        device_id: request.device_id,
    })
    .map_err(|error| format!("could not encode model grant request: {error}"))?;
    let response = client
        .send(
            &Request::post(&endpoint)
                .header("content-type", "application/json")
                .body(body),
        )
        .map_err(|error| format!("model grant request failed: {error}"))?;
    let response: GrantResponse = serde_json::from_slice(&response.body)
        .map_err(|error| format!("model grant response is malformed: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is invalid: {error}"))?
        .as_secs();
    if response.token.trim().is_empty() || response.expires_at <= now {
        return Err("model grant response is incomplete".to_string());
    }
    if response.route != request.route || !response.sha256.eq_ignore_ascii_case(request.sha256) {
        return Err("model grant response scope does not match the requested model".to_string());
    }
    Ok(ModelGrant {
        token: response.token,
        expires_at: response.expires_at,
    })
}
