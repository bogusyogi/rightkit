//! Exact-match allowlist for renderer-authored external links.
//!
//! A desktop shell that opens links clicked in its webview should open only the destinations the
//! app itself authored. The host passes its own list of exact link forms (the normalised
//! `URL.href` a link guard hands the opener, plus any `mailto:` contact address); this module
//! refuses everything else, including whitespace and control characters that could smuggle a
//! second destination past a naive opener.

/// Return whether `url` is exactly one of the `allowed` destinations.
///
/// Every allowed destination is an exact link: userinfo, ports, alternate paths, query or fragment
/// suffixes and encoded delimiter tricks cannot pass without being listed verbatim. Empty input
/// and any whitespace or control character are refused before the list is consulted.
pub fn is_allowed_external_url(url: &str, allowed: &[&str]) -> bool {
    if url.is_empty()
        || url
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return false;
    }

    allowed.contains(&url)
}
