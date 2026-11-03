//! Shared HTTP plumbing for the app's first-party calls, on `rightkit-http`.
//!
//! `parity_client` starts from `ClientConfig::legacy_ureq_parity` (environment proxy, bundled
//! Mozilla roots, 10 redirects erroring when exceeded, no retries) and adds what is specific
//! to the app's wire and error-text contract:
//! - user agent `ureq/<version>` (kept so the wire identity does not change);
//! - one global per-call timeout, no per-phase deadlines;
//! - a 10 MiB cap on buffered bodies (ureq's `read_to_string` limit);
//! - gzip comes from the `gzip` feature.
//!
//! The shared client is also built with the default public-web address policy.
//! A caller whose base URL can be pointed at a local/private development host
//! passes that host to `parity_client_allowing_host`.

use std::time::Duration;

use rightkit_http::{
    AddressPolicy, Client, ClientConfig, HttpError, Phase, ProxyMode, Request, ResponseHead,
};

/// User agent ureq 3.4 sent when a call set none (`ureq/<crate version>`).
const FORMER_USER_AGENT: &str = "ureq/3.4.2";
/// ureq's default `read_to_string` body limit.
const FORMER_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Client reproducing the former ureq defaults with `timeout` as the one global
/// per-call timeout (`ClientConfig::request_timeout` for buffered calls,
/// `ClientConfig::stream_timeout` for streamed ones).
pub fn parity_client(timeout: Duration) -> Result<Client, String> {
    build(timeout, None)
}

/// As `parity_client`, additionally letting `host` (a development override of a
/// base URL) reach non-public addresses. Call only when the override is active.
pub fn parity_client_allowing_host(timeout: Duration, host: &str) -> Result<Client, String> {
    build(timeout, Some(host))
}

fn build(timeout: Duration, private_host: Option<&str>) -> Result<Client, String> {
    let mut policy = AddressPolicy::default();
    if let Some(host) = private_host {
        policy = policy.allow_if(vec![host.to_string()], |_| true);
    }
    let config = ClientConfig {
        connect_timeout: None,
        response_timeout: None,
        recv_body_timeout: None,
        request_timeout: timeout,
        stream_timeout: Some(timeout),
        max_response_bytes: FORMER_MAX_BODY_BYTES,
        ..ClientConfig::legacy_ureq_parity(FORMER_USER_AGENT)
    }
    .address_policy(policy);
    match Client::try_new(config.clone()) {
        Ok(client) => Ok(client),
        // ureq ignores an unusable proxy variable and connects directly.
        Err(_) => Client::try_new(config.proxy(ProxyMode::None)).map_err(|error| error.to_string()),
    }
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The text ureq 3.4's `Display` produced for the failure `error` stands for. This mapping is
/// The app's own error-text contract (the strings its logs and receipts already carry), not an
/// owner capability. Statuses, timeouts, connect failures and size limits are reproduced exactly;
/// other transport text is prefixed `io: ` as ureq's `Io` variant was.
pub fn ureq_style_error(error: &HttpError) -> String {
    match error {
        HttpError::Status { status, .. } => format!("http status: {status}"),
        HttpError::Timeout { message, .. } => format!("timeout: {}", snake_case(message)),
        HttpError::BodyTooLarge { limit } => {
            format!("the response body is larger than request limit: {limit}")
        }
        HttpError::Transport { message, .. } => match message.as_str() {
            "host not found" | "connection failed" | "too many redirects" => message.clone(),
            other => format!("io: {other}"),
        },
        other => other.to_string(),
    }
}

fn is_body_failure(error: &HttpError) -> bool {
    matches!(error, HttpError::BodyTooLarge { .. }) || error.phase() == Some(Phase::Body)
}

/// ureq's `http_status_as_error(true)` raised an error for 4xx and 5xx only.
fn status_is_error(status: u16) -> bool {
    status >= 400
}

/// `http::StatusCode`'s `Display` ("202 Accepted"), as printed by the old `response.status()`
/// formatting: the `http` crate's own reason phrases, "`<unknown status code>`" for an unassigned one.
pub fn status_display(status: u16) -> String {
    match http::StatusCode::from_u16(status) {
        Ok(code) => code.to_string(),
        Err(_) => format!("{status} <unknown status code>"),
    }
}

/// Send `request` and read the body as UTF-8 text, returning `(status, text)`.
///
/// `status_as_error` mirrors ureq's `http_status_as_error`: when true a 4xx/5xx
/// answer is an error (`<send_context>: http status: N`) and its body is dropped.
/// A failure before the body is `<send_context>: <ureq text>`; a failure while
/// reading it is `<read_context>: <ureq text>`.
pub fn send_text(
    client: &Client,
    request: &Request,
    status_as_error: bool,
    send_context: &str,
    read_context: &str,
) -> Result<(u16, String), String> {
    let response = client.send_any(request).map_err(|error| {
        let context = if is_body_failure(&error) {
            read_context
        } else {
            send_context
        };
        format!("{context}: {}", ureq_style_error(&error))
    })?;
    if status_as_error && status_is_error(response.status) {
        return Err(format!("{send_context}: http status: {}", response.status));
    }
    let text = String::from_utf8(response.body)
        .map_err(|_| format!("{read_context}: io: stream did not contain valid UTF-8"))?;
    Ok((response.status, text))
}

/// Send `request` and return only the status and headers (the body is never
/// read, as the old code never read it). `status_as_error` as for `send_text`.
pub fn send_head(
    client: &Client,
    request: &Request,
    status_as_error: bool,
    send_context: &str,
) -> Result<ResponseHead, String> {
    let head = client
        .send_headers(request)
        .map_err(|error| format!("{send_context}: {}", ureq_style_error(&error)))?;
    if status_as_error && status_is_error(head.status) {
        return Err(format!("{send_context}: http status: {}", head.status));
    }
    Ok(head)
}
