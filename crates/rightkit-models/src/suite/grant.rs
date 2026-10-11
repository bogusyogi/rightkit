use rightkit_http::{AddressPolicy, Client, ClientConfig, HttpError, ProxyMode, Request};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A failed grant request, typed so callers can back off. The `Display` text is the
/// message that the `String`-returning entry points have always produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// The worker answered HTTP 429. `retry_after` is the server's `Retry-After` as a delay
    /// from now: delay-seconds, or an IMF-fixdate converted to seconds from now (zero when
    /// the date has passed). `None` when the header is absent or unreadable.
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },
    /// Any other failure: transport, a non-2xx status, a malformed or mismatched response.
    Failed(String),
}

impl GrantError {
    /// The server-advised wait before retrying, for [`GrantError::RateLimited`].
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            GrantError::RateLimited { retry_after, .. } => *retry_after,
            GrantError::Failed(_) => None,
        }
    }
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrantError::RateLimited { message, .. } | GrantError::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for GrantError {}

/// Parse a `Retry-After` header value: delay-seconds, or an IMF-fixdate (RFC 9110, for
/// example `Sun, 06 Nov 1994 08:49:37 GMT`) converted to a delay from now. The obsolete
/// RFC 850 and asctime date forms are not parsed; they yield `None`.
pub fn parse_retry_after_header(value: &str) -> Option<Duration> {
    if let Some(delay) = rightkit_http::parse_retry_after(value) {
        return Some(delay);
    }
    let at = parse_imf_fixdate(value.trim())?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(at.saturating_sub(now)))
}

/// Unix seconds of an IMF-fixdate. Only the fixed `GMT` form is accepted.
fn parse_imf_fixdate(value: &str) -> Option<u64> {
    let mut parts = value.split_ascii_whitespace();
    let weekday = parts.next()?;
    if !weekday.ends_with(',') {
        return None;
    }
    let day: u32 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    let mut hms = clock.split(':');
    let hour: u64 = hms.next()?.parse().ok()?;
    let minute: u64 = hms.next()?.parse().ok()?;
    let second: u64 = hms.next()?.parse().ok()?;
    if hms.next().is_some() || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let days = u64::try_from(days).ok()?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

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
    try_acquire_grant_pinned(request).map_err(|error| error.to_string())
}

/// As [`acquire_grant_pinned`], with a typed [`GrantError`] (HTTP 429 and its `Retry-After`).
pub fn try_acquire_grant_pinned(request: ModelGrantRequest<'_>) -> Result<ModelGrant, GrantError> {
    let client = grant_client(request.worker_base).map_err(GrantError::Failed)?;
    try_acquire_grant_with_client(&client, request)
}

/// As [`acquire_grant_pinned`] through a caller-supplied client. The caller owns
/// the address policy: build it with [`grant_client`] unless the worker is an
/// explicitly allowed private host.
pub fn acquire_grant_with_client(
    client: &Client,
    request: ModelGrantRequest<'_>,
) -> Result<ModelGrant, String> {
    try_acquire_grant_with_client(client, request).map_err(|error| error.to_string())
}

/// As [`acquire_grant_with_client`], with a typed [`GrantError`]. An HTTP 429 answer is
/// returned as [`GrantError::RateLimited`] with the parsed `Retry-After`; the grant POST is
/// never replayed, so the caller decides when to try again.
pub fn try_acquire_grant_with_client(
    client: &Client,
    request: ModelGrantRequest<'_>,
) -> Result<ModelGrant, GrantError> {
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
    .map_err(|error| {
        GrantError::Failed(format!("could not encode model grant request: {error}"))
    })?;
    // `send_any` returns the final status without the error mapping, so a 429 keeps its
    // `Retry-After` header, which the date form needs. Other non-2xx statuses still
    // produce the `HttpError::Status` text that `send` would have produced.
    let response = client
        .send_any(
            &Request::post(&endpoint)
                .header("content-type", "application/json")
                .body(body),
        )
        .map_err(|error| GrantError::Failed(format!("model grant request failed: {error}")))?;
    if !(200..300).contains(&response.status) {
        let retry_after = response
            .header("retry-after")
            .and_then(parse_retry_after_header);
        let error = HttpError::Status {
            status: response.status,
            body: String::new(),
            retry_after,
            attempts: response.attempts,
        };
        let message = format!("model grant request failed: {error}");
        return Err(if response.status == 429 {
            GrantError::RateLimited {
                retry_after,
                message,
            }
        } else {
            GrantError::Failed(message)
        });
    }
    let response: GrantResponse = serde_json::from_slice(&response.body).map_err(|error| {
        GrantError::Failed(format!("model grant response is malformed: {error}"))
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| GrantError::Failed(format!("system clock is invalid: {error}")))?
        .as_secs();
    if response.token.trim().is_empty() || response.expires_at <= now {
        return Err(GrantError::Failed(
            "model grant response is incomplete".to_string(),
        ));
    }
    if response.route != request.route || !response.sha256.eq_ignore_ascii_case(request.sha256) {
        return Err(GrantError::Failed(
            "model grant response scope does not match the requested model".to_string(),
        ));
    }
    Ok(ModelGrant {
        token: response.token,
        expires_at: response.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imf_fixdate_parses_to_unix_seconds() {
        // The RFC 9110 example date.
        assert_eq!(
            parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_imf_fixdate("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 PST"), None);
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 25:49:37 GMT"), None);
        assert_eq!(parse_imf_fixdate("Sun, 06 Foo 1994 08:49:37 GMT"), None);
        assert_eq!(parse_imf_fixdate("Sunday, 06-Nov-94 08:49:37 GMT"), None);
    }

    #[test]
    fn retry_after_accepts_seconds_and_dates() {
        assert_eq!(parse_retry_after_header("7"), Some(Duration::from_secs(7)));
        // A date long past is "retry now".
        assert_eq!(
            parse_retry_after_header("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(Duration::ZERO)
        );
        // A date far in the future is a positive delay.
        let future = parse_retry_after_header("Fri, 31 Dec 2100 23:59:59 GMT").unwrap();
        assert!(future > Duration::from_secs(3600));
        assert_eq!(parse_retry_after_header("soon"), None);
    }
}
