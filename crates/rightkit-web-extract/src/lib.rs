//! `rightkit-web-extract`: the transport-free half of an article extraction ladder.
//!
//! - [`parse`]: readable main text from HTML (densest `article`/`main`/`body` with chrome
//!   removed), title, redirect resolution, blocked-page detection and the per-domain
//!   resource policy.
//! - [`filters`]: pruning plus a BM25 "fit" selection of the extracted text for LLM use.
//! - Ladder decisions: when an error is final ([`is_hard_article_error`]), when static text
//!   is too thin and the browser rung should run ([`static_text_needs_browser`]), and which
//!   of two extracts wins ([`browser_beats_static`]).
//!
//! The fetch tiers ([`fetch`]) run over the host's network policy ([`net::ArticleNet`]) and
//! browser ([`browser::BrowserHost`]); the optional TLS-fingerprint tier ([`tls`]) loads a
//! tls-client shared library the host locates and trusts. The host owns its network policy;
//! this crate only decides, parses and drives the tiers.

use serde::Serialize;
use std::time::Duration;

pub mod browser;
pub mod fetch;
pub mod filters;
pub mod net;
pub mod parse;
pub mod tls;
pub mod tls_ffi;

pub use fetch::{ArticleFetcher, ThreadsScrapeError};
pub use parse::{article_extract_from_html, readable_text};

/// Prefix of an address-policy refusal message. Such a failure is final: a refused
/// destination is never retried through another transport.
pub const POLICY_REFUSAL_PREFIX: &str = "refusing to fetch";

/// Below this many characters a static extract is treated as a JS shell, not the article.
pub const MIN_READABLE_CHARS: usize = 280;
/// Largest HTML body a fetch tier should read.
pub const MAX_ARTICLE_HTML_BYTES: u64 = 5 * 1024 * 1024;
/// Most redirects a fetch tier should follow.
pub const MAX_ARTICLE_REDIRECTS: usize = 5;
/// Time a browser tier should wait for response headers.
pub const BROWSER_HEADERS_TIMEOUT: Duration = Duration::from_secs(15);
/// Time a browser tier may spend rendering and scraping one page.
pub const BROWSER_SCRAPE_TIMEOUT: Duration = Duration::from_secs(120);
/// Time to wait for a Threads post to render its text.
pub const THREADS_TEXT_TIMEOUT: Duration = Duration::from_secs(20);
/// In-page script that collects a rendered Threads post's text, skipping UI chrome.
pub const THREADS_TEXT_JS: &str = r#"
(() => {
  const clean = (value) => (value || '').replace(/\s+/g, ' ').trim();
  const seen = new Set();
  const out = [];
  const isMetric = (text) => /^(\d+([,.]\d+)?[KMB]?|\d+([,.]\d+)?[KMB]?\s+views)$/i.test(text);
  const isDate = (text) => /^\d{1,2}\/\d{1,2}\/\d{2,4}$/.test(text);
  const isUi = (text) => /^(Thread|Author|Translate|Related threads)$/i.test(text);
  const looksLikeHandle = (text) => /^[a-z0-9._]{2,30}$/i.test(text);
  const push = (value, source) => {
    const text = clean(value);
    if (!text || seen.has(text)) return;
    const lower = text.toLowerCase();
    if (source === 'title') {
      if (lower === 'threads' || lower === 'thread' || lower.startsWith('log in') || lower.startsWith('sign up')) return;
    } else if (isUi(text) || isMetric(text) || isDate(text) || looksLikeHandle(text)) {
      return;
    }
    seen.add(text);
    out.push(text);
  };
  push(document.title, 'title');
  for (const el of document.querySelectorAll('[dir="auto"]')) {
    push(el.innerText || el.textContent || '', 'node');
  }
  const meta = document.querySelector('meta[property="og:description"], meta[name="description"]')?.content;
  push(meta, 'meta');
  return out.join('\n\n');
})()
"#;

/// One extracted article.
#[derive(Debug, Clone, Serialize)]
pub struct ArticleExtract {
    pub title: Option<String>,
    pub text: String,
    /// "Fit" (pruned for LLM consumption) variant when filters enabled.
    pub fit_text: Option<String>,
    pub source: ArticleSource,
    pub metadata: FetchMetadata,
    pub html: Option<String>,
}

/// Which ladder tier produced an extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArticleSource {
    Http,
    TlsFingerprint,
    BrowserFallback,
}

/// What the fetch tier observed.
#[derive(Debug, Clone, Serialize)]
pub struct FetchMetadata {
    pub url: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub elapsed_ms: u128,
    pub bytes_read: usize,
    pub resource_policy: ResourcePolicy,
}

/// Which resource kinds a fetch blocks for a domain.
#[derive(Debug, Clone, Serialize)]
pub struct ResourcePolicy {
    pub blocked_kinds: Vec<&'static str>,
    pub domain_override: bool,
}

/// True when a fetch error is final and no further ladder tier should be tried:
/// address-policy refusals, missing or gone pages, non-HTML content, DNS and refused connections.
pub fn is_hard_article_error(message: &str) -> bool {
    if message.contains(POLICY_REFUSAL_PREFIX) {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    const HARD_MARKERS: &[&str] = &[
        "status 404",
        "status 410",
        "non-html content",
        "dns error",
        "failed to lookup address",
        "no such host",
        "name or service not known",
        "nodename nor servname",
        "could not resolve",
        "connection refused",
        "actively refused",
    ];
    HARD_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Error text for a fetch the caller cancelled between ladder tiers.
pub const LADDER_CANCELLED: &str = "cancelled";

/// True when a static (non-browser) tier "succeeded" but produced too little text to be the
/// article: a JS-rendered shell (TGA, PMDA) parses to a title and a line of navigation, or
/// prunes to nothing. The ladder then escalates to the browser rung instead of returning it.
pub fn static_text_needs_browser(text: &str) -> bool {
    text.trim().chars().count() < MIN_READABLE_CHARS
}

/// Choose between a thin static extract and the browser's extract: the browser's wins only
/// when it carries more text, so a short but genuine page is never replaced by a worse render.
pub fn browser_beats_static(static_len: usize, browser_len: usize) -> bool {
    browser_len > static_len
}
