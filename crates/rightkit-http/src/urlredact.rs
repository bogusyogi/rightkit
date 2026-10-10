//! URL redaction for error text, retry events and `Debug` output.
//!
//! Apps carry API keys in query strings (`?key=...`) and occasionally in userinfo, so no
//! error, event or log line from this crate may echo either. URLs are rendered as
//! `scheme://host[:port]/path`, with a query replaced by `?[redacted]` and userinfo and
//! fragment dropped.

const QUERY_MARK: &str = "?[redacted]";

/// Render one URL string without userinfo, query or fragment.
pub(crate) fn redact_url(url: &str) -> String {
    let (before_fragment, _) = url.split_once('#').unwrap_or((url, ""));
    let (head, has_query) = match before_fragment.split_once('?') {
        Some((head, _)) => (head, true),
        None => (before_fragment, false),
    };
    let mut out = String::with_capacity(head.len() + QUERY_MARK.len());
    match head.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(end);
            let host_port = authority.rsplit('@').next().unwrap_or(authority);
            out.push_str(scheme);
            out.push_str("://");
            out.push_str(host_port);
            out.push_str(path);
        }
        None => out.push_str(head),
    }
    if has_query {
        out.push_str(QUERY_MARK);
    }
    out
}

/// Replace every `scheme://...` URL found in free text with its redacted form.
pub(crate) fn redact_urls_in_text(text: &str) -> String {
    if !text.contains("://") {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut search = 0usize;
    while let Some(rel) = text[search..].find("://") {
        let sep = search + rel;
        // Walk back over the scheme characters.
        let mut start = sep;
        while start > copied {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') {
                start -= 1;
            } else {
                break;
            }
        }
        if start == sep {
            // No scheme in front of "://": leave it alone.
            search = sep + 3;
            continue;
        }
        // Walk forward to the end of the URL.
        let mut end = sep + 3;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b'`') {
                break;
            }
            end += 1;
        }
        // Trailing sentence punctuation belongs to the prose, not the URL.
        while end > sep + 3
            && matches!(
                bytes[end - 1],
                b'.' | b',' | b';' | b':' | b')' | b']' | b'}' | b'!'
            )
        {
            end -= 1;
        }
        out.push_str(&text[copied..start]);
        out.push_str(&redact_url(&text[start..end]));
        copied = end;
        search = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Query-string and userinfo parts of a request URL. They join the value list that scrubs
/// error text, so a malformed URL echoed back by a lower layer is scrubbed too.
pub(crate) fn url_secret_parts(url: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let no_fragment = url.split('#').next().unwrap_or(url);
    match no_fragment.split_once('?') {
        Some((head, query)) => {
            if !query.is_empty() {
                parts.push(query.to_string());
            }
            push_userinfo(head, &mut parts);
        }
        None => push_userinfo(no_fragment, &mut parts),
    }
    parts
}

fn push_userinfo(head: &str, parts: &mut Vec<String>) {
    if let Some((_, rest)) = head.split_once("://") {
        let end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..end];
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            if !userinfo.is_empty() {
                parts.push(userinfo.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_drops_userinfo_query_and_fragment() {
        assert_eq!(
            redact_url("https://user:pass@api.example.com:8443/v1/x?key=KEYVAL&a=b#frag"),
            "https://api.example.com:8443/v1/x?[redacted]"
        );
        assert_eq!(
            redact_url("http://example.com/plain"),
            "http://example.com/plain"
        );
        assert_eq!(
            redact_url("http://example.com?key=1"),
            "http://example.com?[redacted]"
        );
        assert_eq!(redact_url("not a url?key=KEYVAL"), "not a url?[redacted]");
    }

    #[test]
    fn text_scrub_finds_embedded_urls() {
        let text =
            "error sending https://h.example/p?key=KEYVAL: refused (http://u:p@x.test/a?b=c)";
        let out = redact_urls_in_text(text);
        assert!(
            !out.contains("KEYVAL") && !out.contains("u:p") && !out.contains("b=c"),
            "{out}"
        );
        assert!(out.contains("https://h.example/p?[redacted]:"), "{out}");
        assert_eq!(redact_urls_in_text("no urls here"), "no urls here");
    }

    #[test]
    fn secret_parts_cover_query_and_userinfo() {
        let parts = url_secret_parts("https://u:pass@h.example/p?key=KEYVAL#f");
        assert!(parts.contains(&"key=KEYVAL".to_string()));
        assert!(parts.contains(&"u:pass".to_string()));
    }
}
