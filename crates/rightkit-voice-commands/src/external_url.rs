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

#[cfg(test)]
mod tests {
    use super::is_allowed_external_url;

    const ALLOWED: &[&str] = &[
        "https://example.app",
        "https://example.app/",
        "https://example.app/pricing",
        "https://example.app/legal/eula",
        "https://store.example.app/account/",
        "https://www.apache.org/licenses/LICENSE-2.0",
        "mailto:hello@example.app",
    ];

    fn allowed(url: &str) -> bool {
        is_allowed_external_url(url, ALLOWED)
    }

    #[test]
    fn allows_listed_destinations() {
        for url in ALLOWED {
            assert!(allowed(url), "expected allowed: {url}");
        }
    }

    #[test]
    fn allows_the_listed_contact_address_and_only_that_one() {
        assert!(allowed("mailto:hello@example.app"));
        for url in [
            "mailto:evil@example.com",
            "mailto:hello@example.app?subject=hi",
            "mailto:hello@example.app,evil@example.com",
            "mailto:Hello@example.app",
            "MAILTO:hello@example.app",
            "mailto:",
            "mailto://hello@example.app",
        ] {
            assert!(!allowed(url), "expected rejected: {url}");
        }
    }

    #[test]
    fn rejects_wrong_scheme_authority_and_unrelated_destinations() {
        for url in [
            "http://example.app/pricing",
            "https://example.app.evil.example/pricing",
            "https://example.app@evil.example/pricing",
            "https://example.app:443/pricing",
            "https://evil.example/https://example.app/pricing",
            "https://example.app/pricing/",
            "https://example.app/pricing?source=app",
            "https://example.app/pricing#buy",
            "https://example.app/%70ricing",
            "https://example.app/pricing%00ignored",
            "https://example.app/pricing/%2e%2e/account",
            "https://example.app//evil.example",
            "https://store.example.app/account",
            "https://www.apache.org/licenses/LICENSE-2.0/extra",
        ] {
            assert!(!allowed(url), "expected rejected: {url}");
        }
    }

    #[test]
    fn rejects_nuls_and_ambiguous_whitespace() {
        for url in [
            "https://example.app/pricing\0https://evil.example",
            "https://example.app/\r\nhttps://evil.example",
            "https://example.app/price\ttracking",
            " https://example.app/pricing",
            "https://example.app/pricing ",
            "",
        ] {
            assert!(!allowed(url), "expected rejected: {url:?}");
        }
    }

    #[test]
    fn an_empty_list_allows_nothing() {
        assert!(!is_allowed_external_url("https://example.app", &[]));
    }
}
