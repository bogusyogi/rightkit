//! Readable text, title and redirect parsing for saved or fetched HTML.

use crate::filters::apply_article_filters;
use crate::{ArticleExtract, ArticleSource, FetchMetadata, ResourcePolicy, MIN_READABLE_CHARS};
use scraper::{Html, Selector};
use std::collections::HashSet;

/// Resolve a `Location` header against the URL that returned it.
pub fn resolve_redirect(base: &str, location: &str) -> Result<String, String> {
    let loc = location.trim();
    if loc.is_empty() {
        return Err("empty redirect Location".to_string());
    }
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(loc.to_string());
    }
    let scheme = if base.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    if let Some(rest) = loc.strip_prefix("//") {
        return Ok(format!("{scheme}://{rest}"));
    }
    let after = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .ok_or_else(|| "redirect base is not an http(s) URL".to_string())?;
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("redirect base has no host".to_string());
    }
    if loc.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{loc}"));
    }
    let base_path = &after[authority.len()..];
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    Ok(format!("{scheme}://{authority}{dir}{loc}"))
}

/// Readable main text of `html`: the densest `article`, `main`, `[role='main']` or `body`
/// with navigation and other chrome removed, prefixed with the title. Refuses blocked pages.
pub fn readable_text(html: &str) -> Result<String, String> {
    let doc = Html::parse_document(html);
    let title = extract_title(html);
    let remove_selectors = [
        "script",
        "style",
        "nav",
        "footer",
        "header",
        "aside",
        "noscript",
        "form",
        "iframe",
        "svg",
        "button",
        ".navbar",
        ".sidebar",
        ".ad",
        ".ads",
        ".menu",
        ".breadcrumbs",
        ".cookie",
        ".cookie-banner",
    ];
    let mut best = String::new();
    for selector in ["article", "main", "[role='main']", "body"] {
        let sel = Selector::parse(selector).map_err(|e| e.to_string())?;
        for element in doc.select(&sel) {
            let text = collect_text_excluding(&element, &remove_selectors);
            let text = normalize_ws(&text);
            if readable_score(&text) > readable_score(&best) {
                best = text;
            }
        }
        if best.len() >= MIN_READABLE_CHARS && selector != "body" {
            break;
        }
    }
    let best = validate_readable_article_text(best)?;
    if best.len() < 20 {
        return Err("no readable text found on page".to_string());
    }
    let mut out = String::new();
    if let Some(title) = title.filter(|title| !best.starts_with(title)) {
        out.push_str(title.trim());
        out.push_str("\n\n");
    }
    out.push_str(best.trim());
    Ok(out)
}

/// Text of `element`, skipping descendants matched by `remove_selectors` (tag or `.class`).
pub fn collect_text_excluding(
    element: &scraper::ElementRef<'_>,
    remove_selectors: &[&str],
) -> String {
    let mut text = String::new();
    for child in element.children() {
        if let Some(child_elem) = scraper::ElementRef::wrap(child) {
            if is_unwanted_element(&child_elem, remove_selectors) {
                continue;
            }
            text.push_str(&collect_text_excluding(&child_elem, remove_selectors));
            text.push(' ');
        } else if let Some(text_node) = child.value().as_text() {
            text.push_str(text_node);
            text.push(' ');
        }
    }
    text
}

/// True when `element` matches one of `remove_selectors` (tag name or `.class`).
pub fn is_unwanted_element(element: &scraper::ElementRef<'_>, remove_selectors: &[&str]) -> bool {
    let name = element.value().name();
    let classes = element
        .value()
        .attr("class")
        .map(|value| value.split_whitespace().collect::<HashSet<_>>())
        .unwrap_or_default();
    remove_selectors.iter().any(|selector| {
        if let Some(class_name) = selector.strip_prefix('.') {
            classes.contains(class_name)
        } else {
            name.eq_ignore_ascii_case(selector)
        }
    })
}

/// Trim `text` and refuse it when it is a blocked or challenge page.
pub fn validate_readable_article_text(text: String) -> Result<String, String> {
    let text = text.trim().to_string();
    if let Some(reason) = blocked_page_reason(&text) {
        return Err(reason);
    }
    Ok(text)
}

/// Why `text` looks like a blocked or challenge page, if it does.
pub fn blocked_page_reason(text: &str) -> Option<String> {
    let compact = text.to_ascii_lowercase();
    let cloudfront = compact.contains("403 error")
        && compact.contains("the request could not be satisfied")
        && compact.contains("cloudfront");
    if cloudfront {
        return Some("blocked page detected: CloudFront 403".to_string());
    }
    let access_denied = compact.contains("access denied")
        || compact.contains("request blocked")
        || compact.contains("captcha");
    if access_denied && text.len() < 2_000 {
        return Some("blocked page detected: access denied or challenge page".to_string());
    }
    None
}

/// The page title from `og:title`, `twitter:title` or `title`.
pub fn extract_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    for selector in ["meta[property='og:title']", "meta[name='twitter:title']"] {
        if let Ok(sel) = Selector::parse(selector) {
            for element in doc.select(&sel) {
                if let Some(content) = element.value().attr("content") {
                    let title = normalize_ws(content);
                    if !title.is_empty() {
                        return Some(title);
                    }
                }
            }
        }
    }
    let sel = Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|element| normalize_ws(&element.text().collect::<Vec<_>>().join(" ")))
        .filter(|title| !title.is_empty())
}

/// Build an [`ArticleExtract`] from a fetched HTML page, with the fit filters applied.
pub fn article_extract_from_html(
    request_url: &str,
    final_url: String,
    status: u16,
    content_type: Option<String>,
    elapsed_ms: u128,
    bytes_read: usize,
    html: String,
) -> Result<ArticleExtract, String> {
    article_extract_from_html_filtered(
        request_url,
        final_url,
        status,
        content_type,
        elapsed_ms,
        bytes_read,
        html,
        true,
    )
}

/// [`article_extract_from_html`] with the fit filters switched by `fit_filter`.
#[allow(clippy::too_many_arguments)]
pub fn article_extract_from_html_filtered(
    request_url: &str,
    final_url: String,
    status: u16,
    content_type: Option<String>,
    elapsed_ms: u128,
    bytes_read: usize,
    html: String,
    fit_filter: bool,
) -> Result<ArticleExtract, String> {
    let title = extract_title(&html);
    let text = readable_text(&html)?;
    let (pruned_text, fit_text) = apply_article_filters(&text, fit_filter);
    Ok(ArticleExtract {
        title,
        text: pruned_text,
        fit_text: Some(fit_text),
        source: ArticleSource::Http,
        metadata: FetchMetadata {
            url: final_url,
            status: Some(status),
            content_type,
            elapsed_ms,
            bytes_read,
            resource_policy: resource_policy(request_url),
        },
        html: Some(html),
    })
}

/// Collapse runs of whitespace to single spaces.
pub fn normalize_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Density score used to pick the best text candidate.
pub fn readable_score(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let punctuation = text.matches(['.', '!', '?']).count();
    text.len() + words * 8 + punctuation * 40
}

/// The [`ResourcePolicy`] for `url`.
pub fn resource_policy(url: &str) -> ResourcePolicy {
    ResourcePolicy {
        blocked_kinds: if domain_allows_resources(url) {
            Vec::new()
        } else {
            vec!["ads", "trackers", "images", "video", "fonts"]
        },
        domain_override: domain_allows_resources(url),
    }
}

/// True for domains whose pages need their media resources to render.
pub fn domain_allows_resources(url: &str) -> bool {
    ["instagram.com", "threads.com", "youtube.com"]
        .iter()
        .any(|domain| url.contains(domain))
}

/// Remove NUL bytes from `html`.
pub fn sanitize_html(html: &str) -> String {
    html.replace('\0', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_resolve_against_the_base() {
        let base = "https://example.com/a/b?q=1";
        assert_eq!(
            resolve_redirect(base, "/c").unwrap(),
            "https://example.com/c"
        );
        assert_eq!(
            resolve_redirect(base, "c").unwrap(),
            "https://example.com/a/c"
        );
        assert_eq!(
            resolve_redirect(base, "//cdn.example.com/x").unwrap(),
            "https://cdn.example.com/x"
        );
        assert!(resolve_redirect(base, "  ").is_err());
    }

    #[test]
    fn readable_text_prefers_the_article_and_keeps_the_title() {
        let body = "Real article sentence that carries the story forward. ".repeat(8);
        let html = format!(
            "<html><head><title>Story</title></head><body><nav>Home Menu</nav><article><p>{body}</p></article></body></html>"
        );
        let text = readable_text(&html).unwrap();
        assert!(text.starts_with("Story"));
        assert!(!text.contains("Home Menu"));
    }

    #[test]
    fn challenge_pages_are_refused() {
        assert!(blocked_page_reason("Access denied - please complete the captcha").is_some());
        assert!(blocked_page_reason("An ordinary paragraph.").is_none());
    }
}
