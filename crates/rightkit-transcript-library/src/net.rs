//! Host of an absolute URL, on the `http` crate's URI grammar (no hand-written parser).

/// Host part of an absolute URL: lower-cased, with no scheme, credentials, port or IPv6 brackets.
/// `None` when the text is not a URL with a host.
pub fn host_of_url(url: &str) -> Option<String> {
    let uri: http::Uri = url.parse().ok()?;
    let host = uri.host()?;
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}
