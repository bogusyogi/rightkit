//! A webview `fetch` executed natively through rightkit-http, so an app needs neither
//! `tauri-plugin-http` nor a webview `connect-src` for its own servers (#448 item 3).
//!
//! The app compiles a [`FetchPolicy`] natively (exact https hosts, optional path prefixes per
//! host); the webview never carries one. Each call is a [`FetchRequest`] DTO (URL, method,
//! headers, base64 body) and answers a [`FetchResponse`] DTO (status, headers, base64 body) or a
//! [`FetchError`] with a fixed code and fixed text: the transport's own error text never reaches
//! the webview, because it can name a URL, a header or a secret.
//!
//! The request runs on a first-party [`rightkit_http::Client`]: https only, only the listed hosts
//! and prefixes on the first request and on every redirect hop, address classes vetted (a listed
//! name that resolves to a private address is refused), no retry, a bounded response. Non-2xx
//! statuses are answers, as with `fetch`. Cookies, `Host`, `Content-Length` and the other
//! connection-level headers cannot be set by the caller, and `Set-Cookie` is never returned.
//!
//! Registering it as a Tauri command (caller-window policy included) is the shell host's job; the
//! JavaScript `fetch`-compatible export is `@rightkit/tauri`'s. This module is Tauri-free and
//! blocking: call it from a blocking task.

use std::time::Duration;

use rightkit_crypto::{base64_decode, base64_encode};
use rightkit_http::{
    AddressPolicy, Client, ClientConfig, ErrorClass, HttpError, Method, Request, RetryPolicy,
};
use serde::{Deserialize, Serialize};

const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: u32 = 5;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Header names the caller may not set: the HTTP stack owns them, or the webview must not
/// smuggle a cookie jar through.
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "expect",
    "cookie",
    "cookie2",
    "set-cookie",
    "proxy-authorization",
    "proxy-connection",
];

/// Why a fetch was refused or failed. The code is the whole answer; the text is fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FetchErrorCode {
    /// The URL, host, path, scheme or a redirect target is outside the policy.
    Refused,
    /// The request itself is malformed (method, header, body, URL).
    Invalid,
    /// A configured timeout elapsed.
    Timeout,
    /// A request or response body is over its limit.
    TooLarge,
    /// A connection, TLS or socket failure.
    Network,
    /// Anything else.
    Unknown,
}

/// A fetch failure: a closed code and fixed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct FetchError {
    pub code: FetchErrorCode,
    pub message: &'static str,
}

impl FetchError {
    fn new(code: FetchErrorCode) -> Self {
        let message = match code {
            FetchErrorCode::Refused => "The request is not allowed for this app.",
            FetchErrorCode::Invalid => "The request is malformed.",
            FetchErrorCode::Timeout => "The request timed out.",
            FetchErrorCode::TooLarge => "The request or response is too large.",
            FetchErrorCode::Network => "The server could not be reached.",
            FetchErrorCode::Unknown => "The request failed.",
        };
        Self { code, message }
    }

    fn from_http(err: &HttpError) -> Self {
        let code = match err {
            HttpError::AddressDenied { .. }
            | HttpError::HttpsRequired { .. }
            | HttpError::RedirectRefused { .. } => FetchErrorCode::Refused,
            HttpError::InvalidRequest(_) => FetchErrorCode::Invalid,
            HttpError::Timeout { .. } => FetchErrorCode::Timeout,
            HttpError::BodyTooLarge { .. } => FetchErrorCode::TooLarge,
            HttpError::Transport { .. } => FetchErrorCode::Network,
            other => match other.class() {
                ErrorClass::Timeout => FetchErrorCode::Timeout,
                ErrorClass::Transport => FetchErrorCode::Network,
                ErrorClass::AddressDenied => FetchErrorCode::Refused,
                _ => FetchErrorCode::Unknown,
            },
        };
        Self::new(code)
    }
}

/// What the app lets the webview reach. Compiled natively, never sent from the webview.
#[derive(Clone, Debug)]
pub struct FetchPolicy {
    hosts: Vec<String>,
    prefixes: Vec<(String, String)>,
    max_request_bytes: usize,
    max_response_bytes: usize,
    timeout: Duration,
}

impl FetchPolicy {
    /// Allow https requests to exactly these hosts (ASCII letters, digits, `-` and `.`; no
    /// scheme, port, path or userinfo). An empty list or a malformed host is refused.
    pub fn new<I, S>(hosts: I) -> Result<Self, FetchError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Vec::new();
        for host in hosts {
            let host = host
                .as_ref()
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            let valid = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                && !host.starts_with('.')
                && !host.contains("..");
            if !valid {
                return Err(FetchError::new(FetchErrorCode::Invalid));
            }
            if !out.contains(&host) {
                out.push(host);
            }
        }
        if out.is_empty() {
            return Err(FetchError::new(FetchErrorCode::Invalid));
        }
        Ok(Self {
            hosts: out,
            prefixes: Vec::new(),
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Confine `host` to a path prefix (whole segments; see
    /// [`AddressPolicy::first_party_path_prefix`]). Call again for more prefixes. The host must
    /// be one of the allowed hosts or the call changes nothing.
    pub fn path_prefix(mut self, host: &str, prefix: &str) -> Self {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if self.hosts.contains(&host) {
            self.prefixes.push((host, prefix.to_owned()));
        }
        self
    }

    /// Most request body bytes after decoding (default 1 MiB).
    pub fn max_request_bytes(mut self, limit: usize) -> Self {
        self.max_request_bytes = limit;
        self
    }

    /// Most response body bytes (default 8 MiB).
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = limit;
        self
    }

    /// The ceiling for one request (default 30 s); a request's own `timeoutMs` may only lower it.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// A webview fetch request.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FetchRequest {
    pub url: String,
    /// Defaults to `GET`.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body, standard base64. Not allowed with `GET` or `HEAD`.
    #[serde(default)]
    pub body_base64: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The answer to a [`FetchRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    pub status: u16,
    /// Lower-case names, without `set-cookie`.
    pub headers: Vec<(String, String)>,
    pub body_base64: String,
    /// The URL after followed redirects.
    pub url: String,
}

/// The executor: one [`rightkit_http::Client`] under one [`FetchPolicy`].
pub struct FetchBridge {
    client: Client,
    policy: FetchPolicy,
}

fn method_of(name: &str) -> Option<Method> {
    Some(match name.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "PATCH" => Method::Patch,
        "DELETE" => Method::Delete,
        "HEAD" => Method::Head,
        _ => return None,
    })
}

fn token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

impl FetchBridge {
    pub fn new(policy: FetchPolicy) -> Self {
        let mut address = AddressPolicy::first_party(policy.hosts.clone());
        for (host, prefix) in &policy.prefixes {
            address = address.first_party_path_prefix(host.clone(), prefix);
        }
        let mut config = ClientConfig::default()
            .address_policy(address)
            .https_only(true);
        config.max_redirects = MAX_REDIRECTS;
        config.request_timeout = policy.timeout;
        config.max_response_bytes = policy.max_response_bytes;
        config.retry = RetryPolicy::none();
        Self {
            client: Client::new(config),
            policy,
        }
    }

    /// Validate and run one request. Blocking.
    pub fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let invalid = || FetchError::new(FetchErrorCode::Invalid);
        let method = match request.method.as_deref() {
            None => Method::Get,
            Some(name) => method_of(name).ok_or_else(invalid)?,
        };
        let authority = request
            .url
            .strip_prefix("https://")
            .ok_or_else(|| FetchError::new(FetchErrorCode::Refused))?
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if authority.contains('@') {
            return Err(FetchError::new(FetchErrorCode::Refused));
        }
        if request.headers.len() > MAX_HEADERS {
            return Err(invalid());
        }
        let mut header_bytes = 0usize;
        for (name, value) in &request.headers {
            header_bytes = header_bytes.saturating_add(name.len() + value.len());
            let bad_value = value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            if !token(name)
                || bad_value
                || FORBIDDEN_HEADERS.contains(&name.to_ascii_lowercase().as_str())
                || name.to_ascii_lowercase().starts_with("proxy-")
                || name.to_ascii_lowercase().starts_with("sec-")
            {
                return Err(invalid());
            }
        }
        if header_bytes > MAX_HEADER_BYTES {
            return Err(invalid());
        }
        let body = match request.body_base64.as_deref() {
            None | Some("") => Vec::new(),
            Some(text) => {
                if matches!(method, Method::Get | Method::Head) {
                    return Err(invalid());
                }
                // A base64 body is 4/3 of its bytes; refuse an oversized one before decoding.
                if text.len() > self.policy.max_request_bytes.saturating_mul(4) / 3 + 4 {
                    return Err(FetchError::new(FetchErrorCode::TooLarge));
                }
                let bytes = base64_decode(text).map_err(|_| invalid())?;
                if bytes.len() > self.policy.max_request_bytes {
                    return Err(FetchError::new(FetchErrorCode::TooLarge));
                }
                bytes
            }
        };
        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .map_or(self.policy.timeout, |t| t.min(self.policy.timeout));

        let mut out = Request::new(method, request.url)
            .timeout(timeout)
            .max_response_bytes(self.policy.max_response_bytes)
            .retry(RetryPolicy::none());
        for (name, value) in request.headers {
            out = out.header(name, value);
        }
        if !body.is_empty() {
            out = out.body(body);
        }
        let response = self
            .client
            .send_any(&out)
            .map_err(|e| FetchError::from_http(&e))?;
        let url = response.final_url().to_owned();
        let headers = response
            .headers
            .iter()
            .filter(|(name, _)| name != "set-cookie")
            .cloned()
            .collect();
        Ok(FetchResponse {
            status: response.status,
            headers,
            body_base64: base64_encode(&response.body),
            url,
        })
    }
}
