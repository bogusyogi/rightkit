//! Raw provider response capture for non-streaming calls.
//!
//! Off by default. `with_raw_capture(max_body_bytes)` on [`crate::openai::OpenAiCompatProvider`]
//! or [`crate::anthropic::AnthropicProvider`] makes `complete()` keep the provider's response
//! as received: its status, its request id header, and up to `max_body_bytes` of the body. The
//! result is one [`RawExchange`] per call, shared as a [`RawHandle`] with
//!
//! - the observer, in [`crate::CallRecord::raw`], for every call that got an HTTP response,
//!   whether the call then succeeded or failed; and
//! - the error, for the two outcomes where the bytes are the evidence: [`LlmError::Truncated`]
//!   and [`LlmError::Malformed`] come back as [`LlmError::Captured`], and
//!   [`LlmError::raw`] returns the same handle the observer saw.
//!
//! **What it holds.** The body is the provider's answer to the prompt. It contains the model's
//! output (which usually repeats or derives from the user's text), tool-call arguments, token
//! usage and provider metadata, and on an error status the provider's error text. The crate
//! does not keep it anywhere itself; once the call returns, only the handles the app holds keep
//! it alive. The app owns that data from then on: storing it, rotating it and scrubbing it are
//! the app's job.
//!
//! **Secrets.** The request side is unchanged: the credential and the auth headers are never
//! part of a [`crate::CallRecord`] or a [`RawExchange`]. A provider can still echo a credential
//! back in an error body, so the exact credential text (and any value in the client's
//! `redact_values`) is replaced with `[redacted]` in the retained bytes, and
//! [`RawExchange::credential_removed`] says so. Apart from that replacement the bytes are exactly
//! what the provider sent. A masked or partial key that a provider echoes is not the credential
//! text and is not touched.
//!
//! **Bounds.** The retained copy is at most `max_body_bytes` (0 keeps status and request id only);
//! [`RawExchange::truncated`] is set when bytes were left out. The transport still reads the
//! whole response into memory to parse it, as it did before, bounded by the client's
//! `max_response_bytes`; capture adds no unbounded copy.
//!
//! **Streaming is not covered.** `stream()` reports `raw: None` to the observer and attaches
//! nothing to its errors, whatever the capture setting: the body of a stream is incremental and
//! may be abandoned by the caller at any point, so there is no single response body to keep.
//!
//! **Retries.** `rightkit-http` retries inside one call and returns only the final response,
//! so a [`RawExchange`] describes the final attempt of the call (`attempts` says how many were
//! made). To see every attempt's own response, make each attempt its own call with
//! `ChatRequest::without_retries()` and retry in the app; the observer then runs once per attempt.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use rightkit_http::{Client, Request, Response};
use serde_json::Value;

use crate::error::{provider_message, LlmError};
use crate::types::{check_output, ChatRequest, ChatResponse};

/// Response headers that carry the provider's request id, in the order they are tried:
/// `x-request-id` (OpenAI and most compatible endpoints) and `request-id` (Anthropic).
pub const REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "request-id"];

/// Longest request id kept, in characters.
const MAX_REQUEST_ID_CHARS: usize = 256;

const REDACTED: &str = "[redacted]";

/// Shared handle to one captured exchange. The observer and the error of one call hold the same
/// allocation; clone it to keep it.
pub type RawHandle = Arc<RawExchange>;

/// The provider's response to one non-streaming call, as received. See the module docs for what
/// it can contain and who owns it.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawExchange {
    /// HTTP status of the final response.
    pub status: u16,
    /// The provider's request id (`x-request-id`, else `request-id`), cut to 256 characters.
    pub request_id: Option<String>,
    /// HTTP requests the client made for this call, counting retries.
    pub attempts: u32,
    /// The retained body bytes: at most the capture cap, possibly ending inside a UTF-8
    /// character when `truncated` is set. Not guaranteed to be valid UTF-8 or JSON.
    pub body: Vec<u8>,
    /// Length in bytes of the body the provider sent.
    pub received_bytes: usize,
    /// True when `body` is shorter than the body the provider sent because of the cap.
    pub truncated: bool,
    /// True when the credential (or a configured redaction value) appeared in the body and was
    /// replaced with `[redacted]` in `body`. Then `body` is not byte-identical to what arrived.
    pub credential_removed: bool,
}

impl RawExchange {
    /// The retained body as text, with invalid UTF-8 replaced.
    pub fn body_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    fn from_response(resp: &Response, cap: usize, secrets: &[String]) -> Self {
        let request_id = REQUEST_ID_HEADERS
            .iter()
            .find_map(|name| resp.header(name))
            .map(|v| v.chars().take(MAX_REQUEST_ID_CHARS).collect::<String>())
            .filter(|v| !v.is_empty());
        let scrubbed = scrub_bytes(&resp.body, secrets);
        let full: &[u8] = scrubbed.as_deref().unwrap_or(resp.body.as_slice());
        let keep = full.len().min(cap);
        Self {
            status: resp.status,
            request_id,
            attempts: resp.attempts,
            body: full[..keep].to_vec(),
            received_bytes: resp.body.len(),
            truncated: full.len() > keep,
            credential_removed: scrubbed.is_some(),
        }
    }
}

impl fmt::Debug for RawExchange {
    /// The body is the model's output and may hold user text, so `Debug` prints sizes only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawExchange")
            .field("status", &self.status)
            .field("request_id", &self.request_id)
            .field("attempts", &self.attempts)
            .field("retained_bytes", &self.body.len())
            .field("received_bytes", &self.received_bytes)
            .field("truncated", &self.truncated)
            .field("credential_removed", &self.credential_removed)
            .finish()
    }
}

/// One non-streaming call. With `capture` unset this is exactly the call both providers made
/// before capture existed (`Client::send`, then parse). With it set, `Client::send_any` returns
/// the response with its headers for any status; the error mapping for a non-2xx answer is the
/// same one `send` applies (`LlmError::Status`, bounded and redacted).
pub(crate) fn exchange(
    client: &Client,
    http: &Request,
    secret: Option<&str>,
    capture: Option<usize>,
    req: &ChatRequest,
    parse: fn(&Value) -> Result<ChatResponse, LlmError>,
) -> (Result<ChatResponse, LlmError>, Option<RawHandle>) {
    let Some(cap) = capture else {
        let result = client
            .send(http)
            .map_err(|e| LlmError::from_http(e, secret))
            .and_then(|resp| parse_body(&resp, parse))
            .and_then(|r| check_output(r, req));
        return (result, None);
    };
    let resp = match client.send_any(http) {
        Ok(resp) => resp,
        Err(e) => return (Err(LlmError::from_http(e, secret)), None),
    };
    let raw: RawHandle = Arc::new(RawExchange::from_response(
        &resp,
        cap,
        &secrets_for(client, secret),
    ));
    if !(200..300).contains(&resp.status) {
        let error = LlmError::Status {
            status: resp.status,
            message: status_message(&resp, http, client, secret),
        };
        return (Err(error), Some(raw));
    }
    let result = parse_body(&resp, parse)
        .and_then(|r| check_output(r, req))
        .map_err(|e| attach(e, &raw));
    (result, Some(raw))
}

fn parse_body(
    resp: &Response,
    parse: fn(&Value) -> Result<ChatResponse, LlmError>,
) -> Result<ChatResponse, LlmError> {
    let v: Value = resp
        .json()
        .map_err(|e| LlmError::Malformed(e.to_string()))?;
    parse(&v)
}

/// Wrap the two outcomes whose evidence is the body; every other error is returned as it is.
fn attach(error: LlmError, raw: &RawHandle) -> LlmError {
    if matches!(error, LlmError::Truncated { .. } | LlmError::Malformed(_)) {
        LlmError::Captured {
            error: Box::new(error),
            raw: Arc::clone(raw),
        }
    } else {
        error
    }
}

/// The credential and the client's configured redaction values, longest first.
fn secrets_for(client: &Client, secret: Option<&str>) -> Vec<String> {
    let mut values: Vec<String> = client
        .config()
        .redact_values
        .iter()
        .cloned()
        .chain(secret.map(str::to_owned))
        .filter(|s| !s.is_empty())
        .collect();
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    values.dedup();
    values
}

/// The text of a non-2xx answer as `LlmError::Status` carries it: the same cut and the same
/// secret removal `Client::send` applies to an error body (configured values, the credential,
/// and the query and userinfo of the request URL), then the provider's own message, bounded.
fn status_message(
    resp: &Response,
    http: &Request,
    client: &Client,
    secret: Option<&str>,
) -> String {
    let cut = resp
        .body
        .len()
        .min(rightkit_http::error::MAX_ERROR_BODY_BYTES);
    let mut text = String::from_utf8_lossy(&resp.body[..cut]).into_owned();
    let mut values = secrets_for(client, secret);
    values.extend(url_secret_parts(&http.url));
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for value in values.iter().filter(|v| !v.is_empty()) {
        text = text.replace(value.as_str(), REDACTED);
    }
    provider_message(&text, secret)
}

/// Query string and userinfo of a URL, the parts `rightkit-http` scrubs from error text.
fn url_secret_parts(url: &str) -> Vec<String> {
    let no_fragment = url.split('#').next().unwrap_or(url);
    let (head, query) = match no_fragment.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (no_fragment, None),
    };
    let mut parts = Vec::new();
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        parts.push(query.to_string());
    }
    if let Some((_, rest)) = head.split_once("://") {
        let end = rest.find('/').unwrap_or(rest.len());
        if let Some((userinfo, _)) = rest[..end].rsplit_once('@') {
            if !userinfo.is_empty() {
                parts.push(userinfo.to_string());
            }
        }
    }
    parts
}

/// `body` with every occurrence of each secret replaced by `[redacted]`, or `None` when none
/// occurs (the common case, which copies nothing). `secrets` are tried in the order given.
fn scrub_bytes(body: &[u8], secrets: &[String]) -> Option<Vec<u8>> {
    let mut current: Option<Vec<u8>> = None;
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        let needle = secret.as_bytes();
        let hay: &[u8] = current.as_deref().unwrap_or(body);
        if find(hay, needle, 0).is_none() {
            continue;
        }
        let mut out = Vec::with_capacity(hay.len());
        let mut at = 0;
        while let Some(pos) = find(hay, needle, at) {
            out.extend_from_slice(&hay[at..pos]);
            out.extend_from_slice(REDACTED.as_bytes());
            at = pos + needle.len();
        }
        out.extend_from_slice(&hay[at..]);
        current = Some(out);
    }
    current
}

/// First index at or after `from` where `needle` occurs in `hay`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        if hay[i] == needle[0] && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_bytes_replaces_every_occurrence_and_leaves_clean_bodies_alone() {
        let secrets = vec!["sk-abc".to_string()];
        assert_eq!(scrub_bytes(b"no key here", &secrets), None);
        assert_eq!(
            scrub_bytes(b"a sk-abc b sk-abc", &secrets).unwrap(),
            b"a [redacted] b [redacted]".to_vec()
        );
        assert_eq!(
            scrub_bytes(b"sk-abc", &secrets).unwrap(),
            b"[redacted]".to_vec()
        );
        assert_eq!(scrub_bytes(b"", &secrets), None);
        assert_eq!(scrub_bytes(b"sk-ab", &secrets), None);
    }

    #[test]
    fn scrub_bytes_handles_a_secret_that_contains_another() {
        let secrets = vec!["sk-abc-long".to_string(), "sk-abc".to_string()];
        assert_eq!(
            scrub_bytes(b"x sk-abc-long y sk-abc", &secrets).unwrap(),
            b"x [redacted] y [redacted]".to_vec()
        );
    }

    #[test]
    fn url_secret_parts_take_the_query_and_the_userinfo() {
        assert_eq!(
            url_secret_parts("https://u:p@host/v1/x?key=1&b=2#frag"),
            vec!["key=1&b=2".to_string(), "u:p".to_string()]
        );
        assert!(url_secret_parts("https://host/v1/x").is_empty());
    }

    #[test]
    fn find_respects_the_start_offset() {
        assert_eq!(find(b"abcabc", b"abc", 0), Some(0));
        assert_eq!(find(b"abcabc", b"abc", 1), Some(3));
        assert_eq!(find(b"abcabc", b"abc", 4), None);
        assert_eq!(find(b"ab", b"abc", 0), None);
    }
}
