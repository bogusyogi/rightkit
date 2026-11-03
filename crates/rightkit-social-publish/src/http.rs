//! The publish core's one-request HTTP layer on rightkit-http =0.3.0.
//!
//! API used: `Client::new(ClientConfig { retry: RetryPolicy::none(), .. })` and
//! `Client::send_any(&Request)`. `send_any` returns every final status as a `Response` (status,
//! lower-cased headers, body), so this module sees the raw 429 and its `Retry-After`; `send` would turn
//! them into an `HttpError::Status` whose `retry_after` is parsed as seconds only. Retries stay out of
//! the client twice over: the client config and each request carry `RetryPolicy::none()`
//! (`max_attempts = 1`), so rightkit-http never sleeps or resends. The retry policy is in machine.rs.
//! Redirects are never followed (`max_redirects = 0`), so a platform cannot bounce a request, or the
//! bearer token, to another host.
//!
//! Also here: the loopback-only base URL rule and the `Retry-After` parser (seconds and HTTP-date).
use std::time::Duration;

use rightkit_http::{AddressPolicy, Client, ClientConfig, HttpError, Method, Request, RetryPolicy};
use serde_json::{Map, Value};

use crate::Fail;

/// Longest wait a platform can ask for in one `Retry-After`. A larger value is clamped.
pub const MAX_RETRY_AFTER_SECS: i64 = 24 * 3_600;

/// What the platform asked for in `Retry-After`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfter {
    /// Delta-seconds form.
    Seconds(i64),
    /// HTTP-date form, as epoch seconds.
    At(i64),
    /// Header absent or unparseable.
    Unspecified,
}

/// One classified platform answer. No token is ever part of a value here.
#[derive(Debug)]
pub enum Reply {
    /// 2xx with the JSON body (Null when the body is not JSON).
    Ok(Value),
    /// 429.
    RateLimited(RetryAfter),
    /// 401.
    Unauthorized,
    /// 5xx.
    ServerError(u16),
    /// Any other non-2xx: the platform understood and refused. The message is capped and scrubbed.
    Rejected { status: u16, message: String },
    /// No usable answer. `connect_phase` is true when nothing can have reached the platform.
    Transport {
        connect_phase: bool,
        message: String,
    },
    /// A configured timeout elapsed after the request may have been sent.
    Timeout,
    /// The request could not be built (bad URL or header). Nothing was sent.
    Invalid(String),
}

/// A blocking client with retries disabled.
pub fn client() -> Client {
    Client::new(ClientConfig {
        connect_timeout: Some(Duration::from_secs(5)),
        request_timeout: Duration::from_secs(30),
        response_timeout: Some(Duration::from_secs(30)),
        max_redirects: 0,
        retry: RetryPolicy::none(),
        // rightkit-http 0.3 denies non-public addresses by default. The dev endpoints are loopback
        // (the base URL rule below enforces it), so exactly these hosts may resolve to loopback.
        address_policy: AddressPolicy::public_web().allow_if(
            vec![
                "127.0.0.1".into(),
                "localhost".into(),
                "::1".into(),
                "[::1]".into(),
            ],
            |ip| ip.is_loopback(),
        ),
        ..ClientConfig::default()
    })
}

fn scrub(text: &str, token: &str) -> String {
    // The token's exact, percent-encoded and JSON/Debug-escaped forms (rightkit-logs 0.3.5 KnownSecrets; an empty value is ignored).
    let cleaned = rightkit_logs::KnownSecrets::from_values([token]).redact(text);
    // Query strings and OAuth values go first, so a cut at 200 characters cannot leave half of one.
    crate::redact::redact_text(&cleaned)
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

/// What a provider needs beyond the common request: extra request headers, and response headers to
/// carry back. A 2xx answer then has those response headers under `headers` in its body value (the
/// header names are the lower-case ones given here), because a `Reply::Ok` otherwise holds only the
/// JSON body. LinkedIn needs both: its version headers go out, and the new post's URN comes back in the
/// `x-restli-id` header of an empty-bodied 201.
#[derive(Clone, Copy)]
pub struct Extra<'a> {
    /// Request headers sent in addition to the standard ones. Values must not hold a secret.
    pub headers: &'a [(&'a str, &'a str)],
    /// Lower-case names of response headers to copy into a 2xx body value.
    pub echo: &'a [&'a str],
}

impl Extra<'static> {
    /// No extra headers and nothing echoed: what [`send`] uses.
    pub const NONE: Extra<'static> = Extra {
        headers: &[],
        echo: &[],
    };
}

/// A 2xx body value with the echoed response headers added under `headers`. A null body (an empty one)
/// becomes an object; a body that is already some other kind of JSON is returned unchanged, so a caller
/// that needs the headers finds none and treats the answer as unusable.
fn with_echoed(body: Value, echoed: Vec<(String, String)>) -> Value {
    if echoed.is_empty() {
        return body;
    }
    let mut object = match body {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        other => return other,
    };
    let headers: Map<String, Value> = echoed
        .into_iter()
        .map(|(name, value)| (name, Value::String(value)))
        .collect();
    object.insert("headers".to_string(), Value::Object(headers));
    Value::Object(object)
}

/// Send one request and classify the answer. `idempotency_key` is sent as `Idempotency-Key`.
pub fn send(
    client: &Client,
    method: Method,
    url: &str,
    token: &str,
    idempotency_key: Option<&str>,
    body: Option<&Value>,
) -> Reply {
    send_with(
        client,
        method,
        url,
        token,
        idempotency_key,
        body,
        &Extra::NONE,
    )
}

/// [`send`] with extra request headers and echoed response headers (see [`Extra`]). The token is still
/// scrubbed from every message.
pub fn send_with(
    client: &Client,
    method: Method,
    url: &str,
    token: &str,
    idempotency_key: Option<&str>,
    body: Option<&Value>,
    extra: &Extra<'_>,
) -> Reply {
    let payload = body.map(|value| ("application/json", value.to_string().into_bytes()));
    send_body(client, method, url, token, idempotency_key, payload, extra)
}

/// [`send_with`] with a raw body: `payload` is the `Content-Type` and the body bytes (a Bluesky blob upload
/// sends the file itself, Mastodon and X send `multipart/form-data`). rightkit-http's `Request::body` takes
/// `impl Into<Vec<u8>>`, so bytes need no RightKit change. Every answer is classified exactly as in
/// [`send_with`], and the token is scrubbed from every message.
pub fn send_body(
    client: &Client,
    method: Method,
    url: &str,
    token: &str,
    idempotency_key: Option<&str>,
    payload: Option<(&str, Vec<u8>)>,
    extra: &Extra<'_>,
) -> Reply {
    let mut request = Request::new(method, url)
        .header("Accept", "application/json")
        .retry(RetryPolicy::none());
    // An empty token means a public read (a DID document, a handle resolution): no bearer is sent, so the
    // token can never reach a host other than the platform's own.
    if !token.is_empty() {
        request = request.bearer(token);
    }
    if let Some(key) = idempotency_key {
        request = request.header("Idempotency-Key", key);
    }
    for (name, value) in extra.headers {
        request = request.header(*name, *value);
    }
    if let Some((content_type, bytes)) = payload {
        request = request.header("Content-Type", content_type).body(bytes);
    }
    match client.send_any(&request) {
        Ok(response) => {
            let status = response.status;
            match status {
                200..=299 => {
                    let body = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
                    let echoed: Vec<(String, String)> = extra
                        .echo
                        .iter()
                        .filter_map(|name| {
                            response
                                .header(name)
                                .map(|value| ((*name).to_string(), value.to_string()))
                        })
                        .collect();
                    Reply::Ok(with_echoed(body, echoed))
                }
                // `X-RateLimit-Reset` (an ISO 8601 instant, sent by Mastodon) names the instant the limit
                // lifts, so it wins when it parses; otherwise `Retry-After` is used as before.
                // `308 Resume Incomplete` is the answer of a resumable upload to a chunk or a probe that left the
                // upload unfinished (YouTube). It is not an error: it arrives as `Reply::Ok` with
                // `resume_incomplete` set and the echoed headers (`range`) under `headers`. Only a request that
                // names headers to echo gets it; any other 308 stays a refusal.
                308 if !extra.echo.is_empty() => {
                    let echoed: Vec<(String, String)> = extra
                        .echo
                        .iter()
                        .filter_map(|name| {
                            response
                                .header(name)
                                .map(|value| ((*name).to_string(), value.to_string()))
                        })
                        .collect();
                    Reply::Ok(with_echoed(
                        serde_json::json!({ "resume_incomplete": true }),
                        echoed,
                    ))
                }
                429 => Reply::RateLimited(
                    response
                        .header("x-ratelimit-reset")
                        .map(parse_rate_limit_reset)
                        .filter(|wait| *wait != RetryAfter::Unspecified)
                        // `x-rate-limit-reset` (epoch seconds, sent by X) is the next-best stated instant.
                        .or_else(|| {
                            response
                                .header("x-rate-limit-reset")
                                .map(parse_epoch_reset)
                                .filter(|wait| *wait != RetryAfter::Unspecified)
                        })
                        .or_else(|| response.header("retry-after").map(parse_retry_after))
                        .unwrap_or(RetryAfter::Unspecified),
                ),
                401 => Reply::Unauthorized,
                500..=599 => Reply::ServerError(status),
                _ => Reply::Rejected {
                    status,
                    message: scrub(&response.text(), token),
                },
            }
        }
        Err(HttpError::Transport {
            message,
            connect_phase,
            ..
        }) => Reply::Transport {
            connect_phase,
            message: scrub(&message, token),
        },
        Err(HttpError::Timeout { .. }) => Reply::Timeout,
        Err(HttpError::InvalidRequest(message)) => Reply::Invalid(scrub(&message, token)),
        // A response arrived but could not be read whole: treat it like a dropped connection.
        Err(other) => Reply::Transport {
            connect_phase: false,
            message: scrub(&other.to_string(), token),
        },
    }
}

/// Fetch the file at a media URL with a plain GET. NO `Authorization` header is ever sent: the URL names a
/// media host the job's owner chose, which is not the platform, so the platform token must never reach it.
/// Redirects are not followed (client config), so a 3xx is a refusal. The answer is classified like a platform
/// answer so the machine can retry or fail it, except that a 401 or 403 from the media host is a plain
/// rejection (never `token_expired`: the platform did not refuse the token). The client's own response limit
/// (32 MiB) bounds the read; the caller applies the platform's smaller limit to the bytes.
pub fn fetch(client: &Client, url: &str) -> Result<Vec<u8>, Reply> {
    let request = Request::new(Method::Get, url)
        .header("Accept", "*/*")
        .retry(RetryPolicy::none());
    match client.send_any(&request) {
        Ok(response) => match response.status {
            200..=299 => Ok(response.body),
            429 => Err(Reply::RateLimited(
                response
                    .header("retry-after")
                    .map(parse_retry_after)
                    .unwrap_or(RetryAfter::Unspecified),
            )),
            500..=599 => Err(Reply::ServerError(response.status)),
            status => Err(Reply::Rejected {
                status,
                message: format!("the media URL answered HTTP {status}"),
            }),
        },
        Err(HttpError::Transport {
            message,
            connect_phase,
            ..
        }) => Err(Reply::Transport {
            connect_phase,
            message: scrub(&message, ""),
        }),
        Err(HttpError::Timeout { .. }) => Err(Reply::Timeout),
        Err(HttpError::InvalidRequest(message)) => Err(Reply::Invalid(scrub(&message, ""))),
        Err(other) => Err(Reply::Transport {
            connect_phase: false,
            message: scrub(&other.to_string(), ""),
        }),
    }
}

/// Seconds of a duration, saturating.
fn secs_of(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn all_digits(v: &str) -> bool {
    !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())
}

/// `Retry-After`: delta-seconds, or an HTTP-date. Seconds are clamped to [`MAX_RETRY_AFTER_SECS`]; a date is
/// `At` its epoch second (the parser is `rightkit_http::parse_retry_after_at` read against the epoch, so the
/// machine applies its own clock). Fractional or non-finite numbers stay `Unspecified`, as before.
pub fn parse_retry_after(value: &str) -> RetryAfter {
    let v = value.trim();
    if all_digits(v) {
        return match v.parse::<i64>() {
            Ok(secs) => RetryAfter::Seconds(secs.min(MAX_RETRY_AFTER_SECS)),
            Err(_) => RetryAfter::Seconds(MAX_RETRY_AFTER_SECS),
        };
    }
    if v.parse::<f64>().is_ok() {
        return RetryAfter::Unspecified;
    }
    match rightkit_http::parse_retry_after_at(v, std::time::UNIX_EPOCH) {
        Some(since_epoch) => RetryAfter::At(secs_of(since_epoch)),
        None => RetryAfter::Unspecified,
    }
}

/// Whole seconds of a duration, rounded up: `RetryAfter::At` carries whole seconds, and a wait must never end
/// early, so any sub-second remainder counts as the next second.
fn ceil_secs_of(d: std::time::Duration) -> i64 {
    secs_of(d).saturating_add(i64::from(d.subsec_nanos() > 0))
}

/// `X-RateLimit-Reset`: an ISO 8601 / RFC 3339 instant such as `2026-09-21T14:13:50.000000Z`, as
/// `RetryAfter::At`. Anything else (a bare number, a date with no zone) is `Unspecified`, because a bare
/// number could be epoch seconds or delta-seconds and guessing would wait the wrong time.
// Parsing is `rightkit_http::parse_rate_limit_reset_at` (rightkit-http =0.3.8 keeps the fraction); only the
// whole-second round-up into `RetryAfter::At` is this engine's (unit `rate_limit_reset_is_an_iso_instant`).
pub fn parse_rate_limit_reset(value: &str) -> RetryAfter {
    let v = value.trim();
    if all_digits(v) {
        return RetryAfter::Unspecified;
    }
    match rightkit_http::parse_rate_limit_reset_at(v, std::time::UNIX_EPOCH) {
        Some(since_epoch) => RetryAfter::At(ceil_secs_of(since_epoch)),
        None => RetryAfter::Unspecified,
    }
}

/// Smallest and largest `x-rate-limit-reset` value taken as epoch seconds: 2001-09-09 to year 5138. A value
/// outside it (a delta in seconds, an epoch in milliseconds, nonsense) is not guessed at. The range is this
/// engine's policy; rightkit-http accepts any digits.
const EPOCH_RESET_RANGE: std::ops::RangeInclusive<i64> = 1_000_000_000..=99_999_999_999;

/// `x-rate-limit-reset`: a bare number of epoch seconds, as `RetryAfter::At`. Anything that is not all digits
/// or is outside `EPOCH_RESET_RANGE` is `Unspecified`, so the default backoff applies instead of a wait that
/// is a guess. The machine clamps the instant to its maximum wait.
pub fn parse_epoch_reset(value: &str) -> RetryAfter {
    let v = value.trim();
    if !all_digits(v)
        || !v
            .parse::<i64>()
            .is_ok_and(|n| EPOCH_RESET_RANGE.contains(&n))
    {
        return RetryAfter::Unspecified;
    }
    match rightkit_http::parse_rate_limit_reset_at(v, std::time::UNIX_EPOCH) {
        Some(since_epoch) => RetryAfter::At(secs_of(since_epoch)),
        None => RetryAfter::Unspecified,
    }
}

/// The endpoint settings a host app keeps for the publish core: whether a non-loopback base URL is
/// allowed, and the configured base URL per platform.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointSettings {
    /// Allow a base URL whose host is not loopback.
    pub allow_remote: bool,
    /// Configured base URL per platform name.
    pub platform_endpoints: std::collections::BTreeMap<String, String>,
}

impl EndpointSettings {
    /// Read `allow_remote` (bool) and `platform_endpoints` (platform to URL) from a settings map.
    pub fn from_settings(value: &Map<String, Value>) -> Self {
        let allow_remote = value
            .get("allow_remote")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let platform_endpoints = value
            .get("platform_endpoints")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            allow_remote,
            platform_endpoints,
        }
    }
}

/// The validated base URL for a platform: the command's `base_url`, else the `platform_endpoints`
/// setting. A host that is not loopback is refused unless the `allow_remote` setting is true.
pub fn base_url(
    settings: &EndpointSettings,
    platform: &str,
    given: Option<&str>,
) -> Result<String, Fail> {
    let allow_remote = settings.allow_remote;
    let endpoint = settings.platform_endpoints.get(platform).cloned();
    let raw = match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(url) => url.to_string(),
        None => endpoint.ok_or_else(|| {
            (
                "endpoint_missing",
                format!(
                    "no base_url in the input and no platform_endpoints setting for {platform}"
                ),
            )
        })?,
    };
    check_base_url(&raw, allow_remote)
}

/// Scheme, host and port checks. Returns the URL without a trailing slash.
pub fn check_base_url(raw: &str, allow_remote: bool) -> Result<String, Fail> {
    let bad = |msg: &str| -> Fail { ("bad_input", format!("base_url: {msg}")) };
    let url = raw.trim();
    if url.is_empty()
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(bad("must be a URL with no spaces or control characters"));
    }
    let lower = url.to_ascii_lowercase();
    let rest = if lower.starts_with("http://") {
        &url[7..]
    } else if lower.starts_with("https://") {
        &url[8..]
    } else {
        return Err(bad("must start with http:// or https://"));
    };
    if rest.contains(['?', '#']) {
        return Err(bad("must not carry a query or fragment"));
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(bad("must name a host, with no user or credentials"));
    }
    let (host, port) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => (&authority[..=end], &authority[end + 1..]),
            None => return Err(bad("has an unclosed [ in the host")),
        }
    } else {
        match authority.find(':') {
            Some(at) => (&authority[..at], &authority[at..]),
            None => (authority, ""),
        }
    };
    if !port.is_empty() {
        let digits = port.strip_prefix(':').unwrap_or("");
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("has a port that is not a number"));
        }
    }
    let loopback = host.eq_ignore_ascii_case("localhost")
        || rightkit_http::parse_host_ip(host).is_some_and(|parsed| {
            parsed.is_canonical
                && rightkit_http::netpolicy::classify_ip(parsed.addr)
                    == rightkit_http::AddressClass::Loopback
        });
    if !loopback && !allow_remote {
        return Err((
            "endpoint_not_loopback",
            format!(
                "host {host} is not loopback (127.0.0.1, localhost or [::1]); the publish core talks only to a local platform unless the allow_remote setting is true"
            ),
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}
