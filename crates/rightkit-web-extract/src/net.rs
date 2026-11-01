//! The page-fetch seam: the host's public-web address policy and HTTP client, behind traits.

use std::io::Read;
use std::time::Duration;

/// A response whose body has not been read yet.
pub struct PageResponse {
    pub status: u16,
    /// The `Location` header, when present.
    pub location: Option<String>,
    /// The `Content-Type` header, when present.
    pub content_type: Option<String>,
    pub body: Box<dyn Read + Send>,
}

/// A blocking HTTP client that follows no redirects. Errors are the host's user-facing
/// transport text (for example `http status: 503`, `timeout: global`).
pub trait PageClient {
    /// Status and `Location` of one GET with `headers`; no body is downloaded.
    fn probe(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, Option<String>), String>;
    /// One GET with `headers`; a 3xx or 4xx is returned, not raised.
    fn stream(&self, url: &str, headers: &[(&str, &str)]) -> Result<PageResponse, String>;
}

/// The host's public-web network policy.
pub trait ArticleNet {
    /// String-level guard: only http(s) URLs with a public, canonical host. The error text of a
    /// refusal starts with [`crate::POLICY_REFUSAL_PREFIX`].
    fn ensure_public_http_url(&self, url: &str) -> Result<(), String>;
    /// Resolve the URL's host and refuse it when any address is denied. A check, not a pin.
    fn vet_resolved_host(&self, url: &str) -> Result<(), String>;
    /// A client for `user_agent` whose connect, header and body timeouts are `timeout`.
    fn client(&self, user_agent: &str, timeout: Duration) -> Box<dyn PageClient + '_>;
}

/// Read a response body to the end, failing when it exceeds `limit` bytes. The error text has no
/// prefix.
pub fn read_body_limited(reader: Box<dyn Read + Send>, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "the response body is larger than request limit: {limit}"
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::read_body_limited;

    #[test]
    fn body_at_the_limit_is_read_and_over_it_is_refused() {
        let ok = read_body_limited(Box::new(std::io::Cursor::new(vec![1u8; 10])), 10).unwrap();
        assert_eq!(ok.len(), 10);
        let err = read_body_limited(Box::new(std::io::Cursor::new(vec![1u8; 11])), 10).unwrap_err();
        assert!(err.starts_with("the response body is larger than request limit"));
    }
}
