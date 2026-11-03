//! X/Twitter post text extraction.
//!
//! Logged-out X serves a logged-out SPA shell whose generic readable-text
//! pass yields only `"<name> (@handle) on X"` — the probe evidence
//! (`.cache/dogfood/ws8-xtiktok-*/report.json`, `probe-x-text`) shows the real
//! post text lives in `og:description` and in the embedded GraphQL/RSC stream
//! (`full_text:"…"` next to the focal `rest_id`). This module parses both.

use scraper::{Html, Selector};

use crate::source_auth::is_x_host;

/// `/status/<digits>` marks a post URL (also covers `/photo/1` suffixes and
/// `twitter.com`/`mobile.twitter.com` forms).
pub fn is_x_post_url(url: &str) -> bool {
    if !is_x_host(url) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    let mut segments = parsed.path().split('/');
    segments.any(|s| s == "status") && x_status_id(url).is_some()
}

/// Numeric status id from `…/status/<id>` in the URL path.
pub fn x_status_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let segments: Vec<&str> = parsed.path().split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        if *seg == "status" {
            let id: String = segments
                .get(i + 1)
                .unwrap_or(&"")
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

/// Extracted post fields — `text` is the post body; the rest is provenance
/// for the output artifact.
#[derive(Debug, Default, Clone)]
pub struct XPost {
    pub title: Option<String>,
    pub handle: Option<String>,
    pub posted_at: Option<String>,
    pub text: String,
}

/// Parse an X status page's HTML into a post. Returns `None` when no real
/// post text can be found (login wall, deleted post, non-status page) — the
/// caller then treats the fetch as an auth/unavailable failure rather than
/// persisting a hollow title-only artifact.
pub fn parse_x_post(html: &str, url: &str) -> Option<XPost> {
    let doc = Html::parse_document(html);
    let title = meta_content(&doc, "meta[property='og:title']")
        .or_else(|| meta_content(&doc, "meta[name='twitter:title']"));
    let handle = meta_content(&doc, "meta[name='twitter:creator']")
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| {
            meta_content(&doc, "meta[property='article:author']").and_then(|author| {
                author
                    .rsplit('/')
                    .next()
                    .map(|h| format!("@{}", h.trim()))
                    .filter(|h| h.len() > 1)
            })
        });
    let posted_at = meta_content(&doc, "meta[property='article:published_time']");

    // Primary: og:description carries the focal post's text verbatim on
    // logged-out X renders (evidence: probe-x-text source.html).
    let mut text = meta_content(&doc, "meta[property='og:description']")
        .or_else(|| meta_content(&doc, "meta[name='twitter:description']"))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && !is_x_boilerplate(t));

    // Fallback: the embedded RSC/GraphQL stream — `full_text:"…"` near the
    // focal `rest_id:"<status id>"` (replies carry their own rest_ids, so the
    // windowed match avoids picking up a reply's text).
    if text.is_none() {
        text = x_status_id(url).and_then(|id| embedded_full_text(html, &id));
    }

    let text = text?;
    Some(XPost {
        title,
        handle,
        posted_at,
        text,
    })
}

/// Final artifact body: the post text plus a short attribution line when the
/// page carried provenance (kept off the first paragraph so the .txt opens
/// with the post itself).
pub fn compose_text(post: &XPost) -> String {
    let mut out = post.text.trim().to_string();
    let mut attribution = Vec::new();
    if let Some(handle) = post.handle.as_deref().filter(|h| !h.is_empty()) {
        attribution.push(handle.to_string());
    }
    if let Some(date) = post
        .posted_at
        .as_deref()
        .and_then(|ts| ts.split('T').next())
        .filter(|d| !d.is_empty())
    {
        attribution.push(date.to_string());
    }
    if !attribution.is_empty() {
        out.push_str(&format!("\n\n— {} · X", attribution.join(" · ")));
    }
    out
}

/// Logged-out X shells and error pages that produce description text which is
/// NOT the post — a hollow-but-plausible wrong answer, so these count as
/// "no post text" and route to the sign-in/unavailable path instead.
fn is_x_boilerplate(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "log in to x",
        "sign in to x",
        "log in to twitter",
        "sign in to twitter",
        "javascript is not available",
        "something went wrong",
        "don't miss what's happening",
        "privacy-related extension",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Image URLs of an X status page, in post order, upgraded to the original
/// size. Reads every `og:image` / `twitter:image` meta tag and keeps only post
/// media on the `pbs.twimg.com/media/` CDN, so avatars, emoji and video
/// thumbnails are never saved as the post's images (#248).
pub fn parse_x_post_images(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut out: Vec<String> = Vec::new();
    for selector in ["meta[property='og:image']", "meta[name='twitter:image']"] {
        let Ok(sel) = Selector::parse(selector) else {
            continue;
        };
        for el in doc.select(&sel) {
            let Some(url) = el.value().attr("content").and_then(x_original_image_url) else {
                continue;
            };
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

/// `https://pbs.twimg.com/media/<id>?format=jpg&name=small` becomes the same
/// path with `name=orig`; `None` for anything that is not post media.
pub fn x_original_image_url(raw: &str) -> Option<String> {
    let mut parsed = url::Url::parse(raw.trim()).ok()?;
    if parsed.host_str() != Some("pbs.twimg.com") || !parsed.path().starts_with("/media/") {
        return None;
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| k != "name")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut q = parsed.query_pairs_mut();
        q.clear();
        for (k, v) in &pairs {
            q.append_pair(k, v);
        }
        q.append_pair("name", "orig");
    }
    Some(parsed.to_string())
}

fn meta_content(doc: &Html, selector: &str) -> Option<String> {
    Selector::parse(selector).ok().and_then(|sel| {
        doc.select(&sel)
            .find_map(|el| el.value().attr("content").map(str::to_string))
    })
}

/// `full_text:"…"` in the embedded stream nearest the focal
/// `rest_id:"<status id>"`. JS object-literal and JSON-encoded shapes both
/// appear in the stream; try each rest_id spelling, then widen to the first
/// `full_text` on the page as a last resort (the focal tweet serializes first
/// in TweetDetail streams).
fn embedded_full_text(html: &str, status_id: &str) -> Option<String> {
    const WINDOW: isize = 4096;
    let needles = [
        format!("rest_id:\"{status_id}\""),
        format!("\"rest_id\":\"{status_id}\""),
        format!("rest_id&quot;:&quot;{status_id}&quot;"),
    ];
    // Every position of a focal `rest_id` match, then every `full_text` within
    // a window of it. Replies carry their own rest_ids, so pick the candidate
    // NEAREST a focal rest_id — the focal tweet's details object places its
    // full_text a few dozen bytes from its rest_id while a reply's text is
    // a whole tweet-object away.
    let mut rest_id_positions = Vec::new();
    for needle in &needles {
        let mut from = 0usize;
        while let Some(rel) = html[from..].find(needle.as_str()) {
            rest_id_positions.push(from + rel);
            from += rel + needle.len();
        }
    }
    let mut candidates: Vec<(usize, String)> = Vec::new();
    for pos in &rest_id_positions {
        let start = pos.saturating_sub(WINDOW as usize);
        let end = (*pos + WINDOW as usize).min(html.len());
        for (off, value) in full_text_strings(&html[start..end]) {
            candidates.push((start + off, value));
        }
    }
    candidates.sort_by_key(|(pos, _)| {
        rest_id_positions
            .iter()
            .map(|r| pos.abs_diff(*r))
            .min()
            .unwrap_or(usize::MAX)
    });
    if let Some((_, value)) = candidates.into_iter().find(|(_, v)| !v.trim().is_empty()) {
        return Some(value);
    }
    full_text_strings(html)
        .into_iter()
        .map(|(_, v)| v)
        .find(|v| !v.trim().is_empty())
}

/// All `full_text:"…"` occurrences in `hay`, as `(byte_offset, unescaped)`.
/// The stream is JS object literals inside <script> bodies, so the value is a
/// JS string (not JSON): handle `\\`, `\"`, `\'`, `\n`, `\t`, `\uXXXX`.
fn full_text_strings(hay: &str) -> Vec<(usize, String)> {
    const KEY: &str = "full_text:\"";
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(KEY) {
        let value_start = from + rel + KEY.len();
        let mut i = value_start;
        let mut value = String::new();
        let bytes = hay.as_bytes();
        while i < bytes.len() {
            match bytes[i] {
                b'"' => break,
                b'\\' if i + 1 < bytes.len() => {
                    i += 1;
                    match bytes[i] {
                        b'n' => value.push('\n'),
                        b't' => value.push('\t'),
                        b'r' => value.push('\r'),
                        b'u' if i + 4 < bytes.len() => {
                            let hex = &hay[i + 1..i + 5];
                            if let Ok(code) = u32::from_str_radix(hex, 16) {
                                if let Some(c) = char::from_u32(code) {
                                    value.push(c);
                                }
                            }
                            i += 4;
                        }
                        other => value.push(other as char),
                    }
                    i += 1;
                }
                _ => {
                    // Copy the whole UTF-8 char, not just the byte.
                    let ch_len = utf8_len(bytes[i]);
                    let end = (i + ch_len).min(bytes.len());
                    value.push_str(&hay[i..end]);
                    i = end;
                }
            }
        }
        if i < bytes.len() {
            out.push((value_start, value));
        }
        from = value_start;
    }
    out
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// JS evaluated inside the signed-in X profile browser. Returns the post
/// text: og:description when the logged-out shell rendered it, else the first
/// rendered `[data-testid="tweetText"]` node (the focal tweet in a status
/// timeline). Shape mirrors THREADS_TEXT_JS.
pub const X_POST_TEXT_JS: &str = r#"
(() => {
  const meta = (sel) => {
    const el = document.querySelector(sel);
    return el ? (el.content || '').trim() : '';
  };
  const desc = meta('meta[property="og:description"]') || meta('meta[name="twitter:description"]');
  const tweets = [...document.querySelectorAll('[data-testid="tweetText"]')]
    .map((e) => (e.innerText || '').trim())
    .filter(Boolean);
  const title = meta('meta[property="og:title"]') || (document.title || '').trim();
  const parts = [];
  if (desc) parts.push(desc);
  for (const t of tweets) if (!parts.includes(t)) parts.push(t);
  return JSON.stringify({ title, text: parts.join('\n\n'), href: location.href });
})()
"#;
