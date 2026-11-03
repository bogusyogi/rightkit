//! The fetch half of the article ladder: the direct HTTP tier, the browser-header tier, the
//! TLS-fingerprint tier and the headless-browser tier, tried in that order until one yields
//! readable text. The host supplies its network policy ([`ArticleNet`]), its browser
//! ([`BrowserHost`]) and, optionally, a tls-client library ([`TlsClientConfig`]).

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::browser::{BrowserConfig, BrowserHost, HeadlessBrowser};
use crate::filters::apply_article_filters;
use crate::net::{read_body_limited, ArticleNet};
use crate::parse::{
    article_extract_from_html_filtered, resolve_redirect, resource_policy,
    validate_readable_article_text,
};
use crate::tls::{fetch_article_tls_fingerprint, TlsClientConfig};
use crate::{
    browser_beats_static, is_hard_article_error, static_text_needs_browser, ArticleExtract,
    ArticleSource, FetchMetadata, BROWSER_HEADERS_TIMEOUT, BROWSER_SCRAPE_TIMEOUT,
    LADDER_CANCELLED, MAX_ARTICLE_HTML_BYTES, MAX_ARTICLE_REDIRECTS, MIN_READABLE_CHARS,
    THREADS_TEXT_JS, THREADS_TEXT_TIMEOUT,
};

/// How long the browser rung waits for a client-rendered page to produce readable text.
pub const ARTICLE_RENDER_WAIT: Duration = Duration::from_secs(15);

/// Encodings the header tier can decode. `br` is left out: the HTTP stack has no brotli decoder,
/// so a brotli body would fail this tier for nothing.
pub const ARTICLE_ACCEPT_ENCODING: &str = "gzip, deflate";

/// Visible text length of the element the extractor will pick (article, main, else body).
const RENDERED_TEXT_LEN_JS: &str = "(()=>{const p=document.querySelector('article')||\
    document.querySelector('main')||document.body;return String(p?(p.innerText||'').trim().length:0)})()";

const BROWSER_HEADERS_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Why a signed-in Threads scrape failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadsScrapeError {
    /// The page is behind a login wall; the profile has no session.
    NeedsAuth,
    /// Any other failure, with its message.
    Other(String),
}

/// The article fetch ladder over a host's network policy and browser.
pub struct ArticleFetcher<'a> {
    /// The network policy and HTTP client.
    pub net: &'a dyn ArticleNet,
    /// The browser factory.
    pub browsers: &'a dyn BrowserHost,
    /// The tls-client library to use for the TLS-fingerprint tier; `None` skips that tier's
    /// library (the tier then fails with "not found" and the ladder moves on).
    pub tls: Option<&'a TlsClientConfig>,
    /// Product token for the user agents of the direct and preflight tiers, for example
    /// `Product/0.1`.
    pub product_user_agent: &'a str,
    /// Apply the pruning and BM25 fit filters to extracted text.
    pub fit_filter: bool,
    /// Called as `(stage, error)` when a tier fails and the ladder moves on.
    pub on_tier_failure: &'a dyn Fn(&str, &str),
}

impl ArticleFetcher<'_> {
    /// Resolve redirects with header-only requests so the browser is only ever pointed at a
    /// vetted final URL.
    pub fn resolve_public_redirects_for_browser(&self, url: &str) -> Result<String, String> {
        self.net.ensure_public_http_url(url)?;
        let client = self.net.client(
            &format!("{} redirect-preflight", self.product_user_agent),
            BROWSER_HEADERS_TIMEOUT,
        );
        let final_url = walk_public_redirects(self.net, url, |current| {
            // Status + Location only; no body is downloaded.
            let (status, location) =
                client.probe(current, &[("Accept", "text/html,application/xhtml+xml")])?;
            if status >= 400 {
                return Err(format!("http status: {status}"));
            }
            Ok((status, location))
        })?;
        // The browser resolves the name itself; vet the URL it will open before launch. A check,
        // not a pin.
        self.net.vet_resolved_host(&final_url)?;
        Ok(final_url)
    }

    /// The direct tier: a plain GET.
    pub fn fetch_article_http(
        &self,
        url: &str,
        on_stage: &mut dyn FnMut(u8, &'static str),
    ) -> Result<ArticleExtract, String> {
        self.net.ensure_public_http_url(url)?;
        let started = Instant::now();
        let client = self.net.client(
            &format!("{} article-fetch", self.product_user_agent),
            Duration::from_secs(20),
        );
        let mut current = url.to_string();
        let mut redirects = 0;
        let response = loop {
            on_stage(25, "requesting page");
            let resp = client
                .stream(&current, &[("Accept", "text/html,application/xhtml+xml")])
                .map_err(|e| format!("http article fetch failed: {e}"))?;
            let status = resp.status;
            if (300..400).contains(&status) {
                redirects += 1;
                if redirects > MAX_ARTICLE_REDIRECTS {
                    return Err("http article fetch: too many redirects".to_string());
                }
                let location = resp
                    .location
                    .ok_or_else(|| format!("http redirect {status} without a Location header"))?;
                let next = resolve_redirect(&current, &location)?;
                self.net.ensure_public_http_url(&next)?;
                current = next;
                continue;
            }
            if status >= 400 {
                return Err(format!("http article fetch failed: http status: {status}"));
            }
            break resp;
        };
        let status = response.status;
        if !(200..400).contains(&status) {
            return Err(format!("http article fetch returned status {status}"));
        }
        let content_type = response.content_type.clone();
        if !content_type
            .as_deref()
            .unwrap_or("text/html")
            .to_ascii_lowercase()
            .contains("html")
        {
            return Err(format!(
                "http article fetch returned non-html content: {}",
                content_type.unwrap_or_else(|| "unknown".to_string())
            ));
        }
        on_stage(45, "reading page");
        // Hops above only read status + Location (the stream is dropped unread); the body is
        // read once, for the final response.
        let bytes = read_body_limited(response.body, MAX_ARTICLE_HTML_BYTES)
            .map_err(|e| format!("read article body: {e}"))?;
        let bytes_read = bytes.len();
        let html = String::from_utf8_lossy(&bytes).into_owned();
        article_extract_from_html_filtered(
            url,
            current,
            status,
            content_type,
            started.elapsed().as_millis(),
            bytes_read,
            html,
            self.fit_filter,
        )
    }

    /// The browser-header tier: a GET with a browser's request headers.
    pub fn fetch_article_browser_headers(
        &self,
        url: &str,
        on_stage: &mut dyn FnMut(u8, &'static str),
    ) -> Result<ArticleExtract, String> {
        self.net.ensure_public_http_url(url)?;
        let started = Instant::now();
        let client = self
            .net
            .client(BROWSER_HEADERS_USER_AGENT, BROWSER_HEADERS_TIMEOUT);
        let headers = [
            (
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
            ),
            ("Accept-Language", "en-US,en;q=0.9"),
            ("Accept-Encoding", ARTICLE_ACCEPT_ENCODING),
            ("Cache-Control", "no-cache"),
            ("Sec-Fetch-Dest", "document"),
            ("Sec-Fetch-Mode", "navigate"),
            ("Sec-Fetch-Site", "none"),
            ("Sec-Fetch-User", "?1"),
            ("Upgrade-Insecure-Requests", "1"),
        ];
        let mut current = url.to_string();
        let mut redirects = 0;
        let response = loop {
            on_stage(35, "requesting page");
            let resp = client
                .stream(&current, &headers)
                .map_err(|e| format!("browser-header article fetch failed: {e}"))?;
            let status = resp.status;
            if (300..400).contains(&status) {
                redirects += 1;
                if redirects > MAX_ARTICLE_REDIRECTS {
                    return Err("browser-header article fetch: too many redirects".to_string());
                }
                let location = resp.location.ok_or_else(|| {
                    format!("browser-header redirect {status} without a Location header")
                })?;
                let next = resolve_redirect(&current, &location)?;
                self.net.ensure_public_http_url(&next)?;
                current = next;
                continue;
            }
            if status >= 400 {
                return Err(format!(
                    "browser-header article fetch failed: http status: {status}"
                ));
            }
            break resp;
        };
        let status = response.status;
        if !(200..400).contains(&status) {
            return Err(format!(
                "browser-header article fetch returned status {status}"
            ));
        }
        let content_type = response.content_type.clone();
        on_stage(45, "reading page");
        let bytes = read_body_limited(response.body, MAX_ARTICLE_HTML_BYTES)
            .map_err(|e| format!("read browser-header article body: {e}"))?;
        let bytes_read = bytes.len();
        let html = String::from_utf8_lossy(&bytes).into_owned();
        article_extract_from_html_filtered(
            url,
            current,
            status,
            content_type,
            started.elapsed().as_millis(),
            bytes_read,
            html,
            self.fit_filter,
        )
    }

    /// The browser tier: render the page in a headless browser and read its visible text.
    pub fn scrape_article_cancelable(
        &self,
        url: &str,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        let safe_url = self.resolve_public_redirects_for_browser(url)?;
        let host_pin = self.browsers.resolve_host_pin(&safe_url)?;
        let browser = self.browsers.new_backend(BrowserConfig {
            host_pin,
            ..Default::default()
        });
        let started = Instant::now();
        let is_cancelled = || should_cancel() || started.elapsed() >= BROWSER_SCRAPE_TIMEOUT;
        if let Err(err) = browser.open(&safe_url, &is_cancelled) {
            let _ = browser.close();
            return Err(err);
        }
        check_browser_timeout(started)?;
        let _ = browser.wait_for_selector("body", &is_cancelled);
        let _ = browser.eval(
            "(()=>new Promise(r=>{let t;const o=new PerformanceObserver(()=>{clearTimeout(t);t=setTimeout(r,500)});\
             o.observe({entryTypes:['resource']});t=setTimeout(()=>{o.disconnect();r()},8000)}))()",
            &is_cancelled,
        );

        // JS-rendered pages (TGA, PMDA) paint their text after the resource burst settles, so
        // poll the visible text length until real content exists or the render wait ends.
        let render_started = Instant::now();
        while render_started.elapsed() < ARTICLE_RENDER_WAIT && !is_cancelled() {
            let len = browser
                .eval(RENDERED_TEXT_LEN_JS, &is_cancelled)
                .unwrap_or_default();
            if rendered_text_ready(&len) {
                break;
            }
            sleep(Duration::from_millis(500));
        }

        let js = "(()=>{const R=['script','style','nav','footer','header','aside',\
            'noscript','form','iframe','svg','button'];\
            R.forEach(t=>document.querySelectorAll(t).forEach(e=>e.remove()));\
            const pick=document.querySelector('article')||document.querySelector('main')||document.body;\
            const h1=document.querySelector('h1');\
            const title=((h1&&h1.innerText)||document.title||'').trim();\
            let body=(pick?pick.innerText:'').replace(/[\\t ]+\\n/g,'\\n').replace(/\\n{3,}/g,'\\n\\n').trim();\
            if(body.length>120000)body=body.slice(0,120000);\
            return (title?title+'\\n\\n':'')+body;})()";
        let text = browser.eval(js, &is_cancelled)?;
        let _ = browser.close();
        let text = text.trim().to_string();
        if text.len() < 20 {
            return Err("no readable text found on page (may be JS-gated or empty)".to_string());
        }
        Ok(text)
    }

    /// Render a Threads post in a headless browser, optionally with a signed-in profile.
    pub fn scrape_threads_cancelable(
        &self,
        url: &str,
        profile_dir: Option<&Path>,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<String, ThreadsScrapeError> {
        let safe_url = self
            .resolve_public_redirects_for_browser(url)
            .map_err(ThreadsScrapeError::Other)?;
        let host_pin = self
            .browsers
            .resolve_host_pin(&safe_url)
            .map_err(ThreadsScrapeError::Other)?;
        // `profile_dir` carries the signed-in session (cookie store); without it the headless
        // browser is anonymous and hits the login wall this function is meant to avoid.
        let browser = self.browsers.new_backend(BrowserConfig {
            profile_dir: profile_dir.map(PathBuf::from),
            host_pin,
            ..Default::default()
        });
        let started = Instant::now();
        let is_cancelled = || should_cancel() || started.elapsed() >= BROWSER_SCRAPE_TIMEOUT;
        if let Err(err) = browser.open(&safe_url, &is_cancelled) {
            let _ = browser.close();
            return Err(ThreadsScrapeError::Other(err));
        }
        check_browser_timeout(started).map_err(ThreadsScrapeError::Other)?;
        let _ = browser.wait_for_selector("body", &is_cancelled);
        if is_login_wall(&*browser, &is_cancelled) {
            let _ = browser.close();
            return Err(ThreadsScrapeError::NeedsAuth);
        }
        let text =
            wait_for_threads_text(&*browser, &is_cancelled).map_err(ThreadsScrapeError::Other)?;
        let _ = browser.close();
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(ThreadsScrapeError::Other(
                "no text extracted from Threads post".to_string(),
            ));
        }
        Ok(text)
    }

    fn tls_tier(&self, url: &str) -> Result<ArticleExtract, String> {
        match self.tls {
            Some(config) => fetch_article_tls_fingerprint(self.net, config, url),
            None => Err("tls-client shared library not found".to_string()),
        }
    }

    /// Run the ladder: direct, browser-header, TLS-fingerprint, then the headless browser. A
    /// hard error ends it at once; a static tier whose text is too thin is kept while the
    /// browser tries to improve on it.
    pub fn fetch_article_with_fallbacks(
        &self,
        url: &str,
        should_cancel: &dyn Fn() -> bool,
        on_stage: &mut dyn FnMut(u8, &'static str),
    ) -> Result<ArticleExtract, String> {
        if should_cancel() {
            return Err(LADDER_CANCELLED.to_string());
        }
        let mut errors = Vec::new();
        // A static tier that returned too little text; the browser rung is tried before it is used.
        let mut thin: Option<ArticleExtract> = None;
        'static_tiers: {
            match self.fetch_article_http(url, on_stage) {
                Ok(extract) => {
                    if !static_text_needs_browser(&extract.text) {
                        return Ok(extract);
                    }
                    thin = Some(extract);
                    break 'static_tiers;
                }
                Err(http_err) => {
                    (self.on_tier_failure)("direct", &http_err);
                    if is_hard_article_error(&http_err) {
                        // Hard failure (policy refusal, DNS, refused, 404/410, non-HTML): stop here.
                        return Err(http_err);
                    }
                    errors.push(format!("direct: {http_err}"));
                }
            }

            if should_cancel() {
                return Err(LADDER_CANCELLED.to_string());
            }
            on_stage(30, "browser-header retry");
            match self.fetch_article_browser_headers(url, on_stage) {
                Ok(extract) => {
                    if !static_text_needs_browser(&extract.text) {
                        return Ok(extract);
                    }
                    thin = Some(extract);
                    break 'static_tiers;
                }
                Err(header_err) => {
                    (self.on_tier_failure)("browser-header", &header_err);
                    if is_hard_article_error(&header_err) {
                        return Err(header_err);
                    }
                    errors.push(format!("browser-header: {header_err}"));
                }
            }

            if should_cancel() {
                return Err(LADDER_CANCELLED.to_string());
            }
            on_stage(37, "TLS fingerprint retry");
            match self.tls_tier(url) {
                Ok(extract) => {
                    if !static_text_needs_browser(&extract.text) {
                        return Ok(extract);
                    }
                    thin = Some(extract);
                    break 'static_tiers;
                }
                Err(tls_err) => {
                    (self.on_tier_failure)("tls-client", &tls_err);
                    if is_hard_article_error(&tls_err) {
                        return Err(tls_err);
                    }
                    errors.push(format!("tls-client: {tls_err}"));
                }
            }
        }

        if should_cancel() {
            return Err(LADDER_CANCELLED.to_string());
        }
        on_stage(40, "browser fallback");
        let browser_started = Instant::now();
        match self
            .scrape_article_cancelable(url, should_cancel)
            .and_then(|text| {
                extract_article_from_browser_text(
                    url,
                    text,
                    browser_started.elapsed(),
                    self.fit_filter,
                )
            }) {
            Ok(extract) => match thin {
                Some(static_extract)
                    if !browser_beats_static(
                        static_extract.text.trim().chars().count(),
                        extract.text.trim().chars().count(),
                    ) =>
                {
                    Ok(static_extract)
                }
                _ => Ok(extract),
            },
            Err(browser_err) => {
                (self.on_tier_failure)("browser", &browser_err);
                if let Some(static_extract) = thin {
                    if !static_extract.text.trim().is_empty() {
                        // The browser rung could not improve on a short but real static page.
                        return Ok(static_extract);
                    }
                    errors.push("static: page has no text without JavaScript".to_string());
                }
                errors.push(format!("browser: {browser_err}"));
                Err(format!("article fetch failed: {}", errors.join("; ")))
            }
        }
    }
}

/// Redirect-walking core of [`ArticleFetcher::resolve_public_redirects_for_browser`], split out
/// so the policy is testable without a network. `probe` returns `(status, Location)` for one
/// request against a URL that already passed the public-URL guard. Probe failures and malformed
/// redirect metadata are hard errors: returning the last URL would hand an unverified redirect
/// tail to the browser. The public-URL guard runs on every resolved hop and stays a hard error:
/// SSRF is never advisory. Exceeding [`MAX_ARTICLE_REDIRECTS`] is also a hard error.
pub fn walk_public_redirects(
    net: &dyn ArticleNet,
    url: &str,
    mut probe: impl FnMut(&str) -> Result<(u16, Option<String>), String>,
) -> Result<String, String> {
    let mut current = url.to_string();
    for _ in 0..MAX_ARTICLE_REDIRECTS {
        let (status, location) = match probe(&current) {
            Ok(result) => result,
            Err(e) => return Err(format!("redirect preflight failed for {current}: {e}")),
        };
        if !(300..400).contains(&status) {
            return Ok(current);
        }
        let Some(location) = location else {
            return Err(format!(
                "redirect preflight received {status} without a Location header"
            ));
        };
        let next = match resolve_redirect(&current, &location) {
            Ok(next) => next,
            Err(e) => {
                return Err(format!(
                    "redirect preflight received malformed Location for {current}: {e}"
                ))
            }
        };
        // SSRF is never advisory: a redirect to a private/loopback target is
        // still a hard failure, not a fall-back-to-last-URL case.
        net.ensure_public_http_url(&next)?;
        current = next;
    }
    Err(format!(
        "article redirect preflight exceeded {MAX_ARTICLE_REDIRECTS} hops"
    ))
}

/// True once the page's rendered text length (as returned by the length probe script) reaches
/// the readable minimum.
pub fn rendered_text_ready(len: &str) -> bool {
    len.trim()
        .trim_matches('"')
        .parse::<usize>()
        .is_ok_and(|n| n >= MIN_READABLE_CHARS)
}

/// Build an extract from the text a headless browser rendered.
pub fn extract_article_from_browser_text(
    url: &str,
    text: String,
    elapsed: Duration,
    fit_filter: bool,
) -> Result<ArticleExtract, String> {
    let text = validate_readable_article_text(text)?;
    let title = text
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string);
    let (pruned_text, fit_text) = apply_article_filters(&text, fit_filter);
    Ok(ArticleExtract {
        title,
        text: pruned_text,
        fit_text: Some(fit_text),
        source: ArticleSource::BrowserFallback,
        metadata: FetchMetadata {
            url: url.to_string(),
            status: None,
            content_type: None,
            elapsed_ms: elapsed.as_millis(),
            bytes_read: 0,
            resource_policy: resource_policy(url),
        },
        html: None,
    })
}

/// Fail once a browser scrape has run past [`BROWSER_SCRAPE_TIMEOUT`].
pub fn check_browser_timeout(started: Instant) -> Result<(), String> {
    if started.elapsed() >= BROWSER_SCRAPE_TIMEOUT {
        Err(format!(
            "browser scrape timed out after {}s",
            BROWSER_SCRAPE_TIMEOUT.as_secs()
        ))
    } else {
        Ok(())
    }
}

/// Poll a rendered Threads post for its text until it appears or [`THREADS_TEXT_TIMEOUT`].
pub fn wait_for_threads_text(
    browser: &dyn HeadlessBrowser,
    should_cancel: &dyn Fn() -> bool,
) -> Result<String, String> {
    let started = Instant::now();
    let mut last_error: Option<String> = None;
    loop {
        if should_cancel() {
            return Err("cancelled".into());
        }
        match browser.eval(THREADS_TEXT_JS, should_cancel) {
            Ok(text) => {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    return Ok(text);
                }
            }
            Err(err) => last_error = Some(err),
        }
        if started.elapsed() >= THREADS_TEXT_TIMEOUT {
            return match last_error {
                Some(err) => Err(err),
                None => Ok(String::new()),
            };
        }
        sleep(Duration::from_millis(500));
    }
}

/// True when the browser was sent to a login page.
pub fn is_login_wall(browser: &dyn HeadlessBrowser, should_cancel: &dyn Fn() -> bool) -> bool {
    browser
        .eval("location.href", should_cancel)
        .map(|href| href.contains("/accounts/login") || href.contains("/login"))
        .unwrap_or(false)
}

/// The signed-in browser profile directory for `platform`: the profile the auth state carries
/// when it is for that platform, else `<default_root>/auth/<platform>`.
///
/// `auth_platform` and `auth_profile_dir` are the platform and profile directory of the app's
/// current auth state.
pub fn platform_profile_dir(
    platform: &str,
    auth_platform: Option<&str>,
    auth_profile_dir: Option<&str>,
    default_root: &Path,
) -> PathBuf {
    let platform_matches = auth_platform.is_some_and(|p| p.eq_ignore_ascii_case(platform));
    if platform_matches {
        if let Some(profile_dir) = auth_profile_dir
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            return PathBuf::from(profile_dir);
        }
    }
    default_root.join("auth").join(platform)
}
