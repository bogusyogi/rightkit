//! Article-quality corpus scoring: how much readable main content an extractor keeps
//! from a saved HTML page, and how much boilerplate it drops.
//!
//! The corpus (saved HTML pages) belongs to the caller; [`run_corpus`] reads each case
//! from a directory, runs the caller's extractor, and scores the result. [`readable_text`]
//! is the reference heuristic extractor the corpus baselines were first produced with.

use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

const MIN_READABLE_CHARS: usize = 280;

/// One page in an article corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleCase {
    /// Stable case id, also the baseline file stem.
    pub id: String,
    /// The page's original URL (reporting only; nothing is fetched).
    pub url: String,
    /// File name of the saved HTML inside the corpus directory.
    pub html_file: String,
}

/// The score of one extracted page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArticleScore {
    /// Case id.
    pub id: String,
    /// Bytes of extracted main content.
    pub main_bytes: usize,
    /// Characters of extracted main content.
    pub main_chars: usize,
    /// Estimated share of the page's body text that the extractor dropped, 0.0 to 1.0.
    pub boilerplate_ratio: f64,
}

/// Averages over a scored corpus.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CorpusSummary {
    /// Number of scored cases.
    pub cases: usize,
    /// Mean main-content bytes.
    pub avg_main_bytes: usize,
    /// Mean boilerplate ratio, 0.0 to 1.0.
    pub avg_boilerplate_ratio: f64,
}

/// What happened to one case in [`run_corpus`].
#[derive(Debug, Clone, PartialEq)]
pub enum CaseOutcome {
    /// The page was extracted and scored.
    Scored(ArticleScore),
    /// The HTML file is missing or empty.
    Skipped {
        /// Case id.
        id: String,
        /// Why it was skipped.
        reason: String,
    },
    /// The extractor refused the page.
    Failed {
        /// Case id.
        id: String,
        /// The extractor's error.
        error: String,
    },
}

/// Score `extracted` main content against the page `html` it came from.
pub fn score_article(id: &str, html: &str, extracted: &str) -> ArticleScore {
    let main_bytes = extracted.len();
    let total_text = estimate_total_text_chars(html);
    let boilerplate_ratio = if total_text > main_bytes {
        (total_text - main_bytes) as f64 / total_text as f64
    } else {
        0.0
    };
    ArticleScore {
        id: id.to_string(),
        main_bytes,
        main_chars: extracted.chars().count(),
        boilerplate_ratio,
    }
}

/// Average the scored cases; `None` when nothing was scored.
pub fn summarize(scores: &[ArticleScore]) -> Option<CorpusSummary> {
    if scores.is_empty() {
        return None;
    }
    let n = scores.len();
    Some(CorpusSummary {
        cases: n,
        avg_main_bytes: scores.iter().map(|s| s.main_bytes).sum::<usize>() / n,
        avg_boilerplate_ratio: scores.iter().map(|s| s.boilerplate_ratio).sum::<f64>() / n as f64,
    })
}

/// Run `extract` over every case whose HTML lives in `dir`, scoring each page.
/// When `write_baselines` is true the extracted text is written to `<dir>/<id>.baseline.txt`.
pub fn run_corpus<F>(
    dir: &Path,
    cases: &[ArticleCase],
    write_baselines: bool,
    mut extract: F,
) -> Vec<CaseOutcome>
where
    F: FnMut(&str) -> Result<String, String>,
{
    cases
        .iter()
        .map(|case| {
            let path = dir.join(&case.html_file);
            let html = match std::fs::read_to_string(&path) {
                Ok(h) if !h.trim().is_empty() => h,
                Ok(_) => {
                    return CaseOutcome::Skipped {
                        id: case.id.clone(),
                        reason: format!("{} is empty", path.display()),
                    }
                }
                Err(e) => {
                    return CaseOutcome::Skipped {
                        id: case.id.clone(),
                        reason: format!("read {}: {e}", path.display()),
                    }
                }
            };
            match extract(&html) {
                Ok(text) => {
                    if write_baselines {
                        let _ =
                            std::fs::write(dir.join(format!("{}.baseline.txt", case.id)), &text);
                    }
                    CaseOutcome::Scored(score_article(&case.id, &html, &text))
                }
                Err(error) => CaseOutcome::Failed {
                    id: case.id.clone(),
                    error,
                },
            }
        })
        .collect()
}

/// The reference heuristic extractor: the densest of `article`, `main`, `[role='main']` or
/// `body` with navigation, scripts and other chrome removed, prefixed with the page title.
/// Refuses blocked or challenge pages.
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
    if let Some(title) = title.filter(|t| !best.starts_with(t)) {
        out.push_str(title.trim());
        out.push_str("\n\n");
    }
    out.push_str(best.trim());
    Ok(out)
}

fn collect_text_excluding(element: &scraper::ElementRef<'_>, remove_selectors: &[&str]) -> String {
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

fn is_unwanted_element(element: &scraper::ElementRef<'_>, remove_selectors: &[&str]) -> bool {
    let name = element.value().name();
    let classes = element
        .value()
        .attr("class")
        .map(|v| v.split_whitespace().collect::<HashSet<_>>())
        .unwrap_or_default();
    remove_selectors.iter().any(|selector| {
        if let Some(class_name) = selector.strip_prefix('.') {
            classes.contains(class_name)
        } else {
            name.eq_ignore_ascii_case(selector)
        }
    })
}

fn validate_readable_article_text(text: String) -> Result<String, String> {
    let text = text.trim().to_string();
    if let Some(reason) = blocked_page_reason(&text) {
        return Err(reason);
    }
    Ok(text)
}

fn blocked_page_reason(text: &str) -> Option<String> {
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

fn extract_title(html: &str) -> Option<String> {
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

fn normalize_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn readable_score(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let punctuation = text.matches(['.', '!', '?']).count();
    text.len() + words * 8 + punctuation * 40
}

/// Simple total body-text estimate (upper bound including boilerplate), the denominator of
/// the boilerplate ratio.
pub fn estimate_total_text_chars(html: &str) -> usize {
    let doc = Html::parse_document(html);
    let mut buf = String::new();
    if let Ok(sel) = Selector::parse("body") {
        for el in doc.select(&sel) {
            for t in el.text() {
                buf.push_str(t);
                buf.push(' ');
            }
        }
    } else {
        for t in doc.root_element().text() {
            buf.push_str(t);
            buf.push(' ');
        }
    }
    buf.split_whitespace().collect::<Vec<_>>().join(" ").len()
}
