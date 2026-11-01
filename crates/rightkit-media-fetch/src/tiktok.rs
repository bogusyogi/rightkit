//! WS-8: TikTok photo-slideshow support.
//!
//! `/@user/photo/<id>` URLs are TikTok's image-carousel ("photo mode") posts.
//! yt-dlp's TikTok extractor does not match the `/photo/` shape and dies with
//! `Unsupported URL` (probe `probe-tt-photo` in
//! `.cache/dogfood/ws8-xtiktok-*/report.json`), while the same post at
//! `/video/<id>` yields a playable slideshow video (`probe-tt-photo-video-form`).
//! This module owns both fixes: parse the post page's embedded JSON for the
//! slide image list (the OCR path, mirroring Instagram carousels), and the
//! `/photo/`→`/video/` rewrite for the media pipeline.

use std::path::Path;

use scraper::{Html, Selector};

use crate::source_auth::is_tiktok_host;
use crate::web::{ImageRequest, PublicWeb, WebError};

/// Same slide cap as the Instagram carousel path (`carousel.rs`).
pub const MAX_SLIDES: usize = 30;

/// True for `tiktok.com/@user/photo/<id>` post URLs.
pub fn is_tiktok_photo_url(url: &str) -> bool {
    if !is_tiktok_host(url) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    let segments: Vec<&str> = parsed.path().split('/').collect();
    segments
        .iter()
        .position(|s| *s == "photo")
        .is_some_and(|i| {
            segments
                .get(i + 1)
                .is_some_and(|id| id.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
}

/// `/photo/<id>` → `/video/<id>`: the evidence-backed rewrite that makes
/// yt-dlp accept a photo-mode post (the same post id serves a slideshow
/// video at the /video/ shape).
pub fn tiktok_photo_as_video_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url.trim()) else {
        return url.to_string();
    };
    let path = parsed.path().to_string();
    if let Some(i) = path.find("/photo/") {
        parsed.set_path(&format!(
            "{}/video/{}",
            &path[..i],
            &path[i + "/photo/".len()..]
        ));
        parsed.to_string()
    } else {
        url.to_string()
    }
}

/// Numeric post id after a `/photo/` or `/video/` path segment — used for the
/// slide directory name, mirroring the Instagram `shortcode` role.
pub fn tiktok_post_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let segments: Vec<&str> = parsed.path().split('/').collect();
    for marker in ["photo", "video"] {
        if let Some(i) = segments.iter().position(|s| *s == marker) {
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

/// Slides + caption recovered from a TikTok photo post's HTML.
#[derive(Debug, Default)]
pub struct TikTokSlideshow {
    /// Direct image URLs in display order.
    pub images: Vec<String>,
    /// Post caption (`itemStruct.desc`) when present.
    pub caption: Option<String>,
}

/// Extract the slide image list from a post page's embedded JSON. TikTok
/// ships item data in `<script type="application/json">` blobs — today
/// `__UNIVERSAL_DATA_FOR_REHYDRATION__`, older `SIGI_STATE` — so rather than
/// hard-coding one blob id we parse every JSON script and walk for an
/// `imagePost.images[].imageURL.urlList` shape.
pub fn parse_tiktok_slideshow(html: &str) -> Option<TikTokSlideshow> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("script").ok()?;
    for script in doc.select(&sel) {
        let body: String = script.text().collect();
        let body = body.trim();
        if !body.contains("\"imagePost\"") || !body.starts_with('{') {
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
            continue;
        };
        if let Some(show) = find_image_post(&json) {
            return Some(show);
        }
    }
    None
}

/// Depth-first search for an object carrying `imagePost.images[]`; the
/// containing object also holds `desc` (the post caption) in both blob shapes.
fn find_image_post(value: &serde_json::Value) -> Option<TikTokSlideshow> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(post)) = map.get("imagePost") {
                if let Some(serde_json::Value::Array(images)) = post.get("images") {
                    let mut urls = Vec::new();
                    for image in images {
                        if let Some(url) = first_url_list_entry(image) {
                            urls.push(url);
                        }
                    }
                    if !urls.is_empty() {
                        return Some(TikTokSlideshow {
                            images: urls,
                            caption: map
                                .get("desc")
                                .and_then(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string),
                        });
                    }
                }
            }
            map.values().find_map(find_image_post)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_image_post),
        _ => None,
    }
}

/// First URL string inside a `urlList` array anywhere under `image`,
/// preferring a `.jpeg`/`.jpg` entry (OCR decoders; webp is not enabled).
fn first_url_list_entry(image: &serde_json::Value) -> Option<String> {
    fn collect<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Array(list)) = map.get("urlList") {
                    for item in list {
                        if let Some(s) = item.as_str() {
                            out.push(s);
                        }
                    }
                }
                for (k, v) in map {
                    if k != "urlList" {
                        collect(v, out);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
            _ => {}
        }
    }
    let mut urls = Vec::new();
    collect(image, &mut urls);
    urls.iter()
        .find(|u| {
            let l = u.to_ascii_lowercase();
            l.contains(".jpeg") || l.contains(".jpg")
        })
        .or_else(|| urls.first())
        .map(|s| s.to_string())
}

/// Download one slide image to `dest`. TikTok CDN URLs are public but reject
/// bare/robotic clients, so the request carries a browser UA and a tiktok.com
/// Referer. Returns the file extension the caller should use (from
/// Content-Type, defaulting to jpg).
pub fn download_slide_image(web: &dyn PublicWeb, url: &str, dest: &Path) -> Result<String, String> {
    download_image_with_referer(web, url, dest, "https://www.tiktok.com/")
}

/// `download_slide_image` with a caller-chosen Referer (X image CDN, #248).
pub fn download_image_with_referer(
    web: &dyn PublicWeb,
    url: &str,
    dest: &Path,
    referer: &str,
) -> Result<String, String> {
    web.ensure_public_http_url(url)?;
    // Redirects: up to 10 hops, each resolved and vetted against the public-web policy.
    let resp = web
        .get_image(&ImageRequest {
            url,
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            timeout: std::time::Duration::from_secs(30),
            max_redirects: 10,
            referer,
            accept: "image/avif,image/webp,image/*,*/*;q=0.8",
            max_bytes: 20 * 1024 * 1024,
        })
        .map_err(|e| match e {
            WebError::Body(text) => format!("read slide image: {text}"),
            WebError::Fetch(text) => format!("slide image fetch failed: {text}"),
        })?;
    if resp.status >= 400 {
        return Err(format!(
            "slide image fetch failed: http status: {}",
            resp.status
        ));
    }
    let content_type = resp
        .content_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = resp.body;
    std::fs::write(dest, &bytes).map_err(|e| format!("write slide image: {e}"))?;
    let ext = if content_type.contains("webp") {
        "webp"
    } else if content_type.contains("png") {
        "png"
    } else if content_type.contains("gif") {
        "gif"
    } else {
        "jpg"
    };
    Ok(ext.to_string())
}
