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

#[cfg(test)]
mod tests {
    use super::host_of_url;

    #[test]
    fn host_of_url_strips_scheme_port_credentials_and_brackets() {
        assert_eq!(
            host_of_url("http://127.0.0.1:8787/x").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(host_of_url("https://a.b/c?d").as_deref(), Some("a.b"));
        assert_eq!(host_of_url("https://u:p@a.b:1/c").as_deref(), Some("a.b"));
        assert_eq!(host_of_url("http://[::1]:80/").as_deref(), Some("::1"));
    }

    #[test]
    fn host_of_url_lowercases_and_refuses_text_without_a_host() {
        assert_eq!(
            host_of_url("https://API.Groq.COM/openai/v1").as_deref(),
            Some("api.groq.com")
        );
        assert_eq!(host_of_url("not a url"), None);
        assert_eq!(host_of_url("/only/a/path"), None);
    }
}
