//! Instagram carousel extraction.
//!
//! Browser mechanics live behind the host's [`BrowserHost`]; this module stays focused on
//! carousel flow: open the post, advance slides, screenshot, and return slide paths plus
//! caption for the caller's OCR and persistence.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rightkit_web_extract::browser::{BrowserConfig, BrowserHost, HeadlessBrowser, HostPin};
use rightkit_web_extract::ArticleFetcher;

use crate::web::PublicWeb;

/// What a carousel scrape needs from its host.
pub struct CarouselEnv<'a> {
    /// The article fetcher, used to resolve and vet redirects before the browser opens a page
    /// and to reach the host's browser factory.
    pub fetcher: &'a ArticleFetcher<'a>,
    /// The host's public-web policy, used to download original slide images.
    pub web: &'a dyn PublicWeb,
    /// Where the per-domain selector memory is kept.
    pub selector_memory_path: PathBuf,
}

impl CarouselEnv<'_> {
    fn browsers(&self) -> &dyn BrowserHost {
        self.fetcher.browsers
    }
}

/// Publish `from` at `to`, replacing an existing file. On Windows the replace retries a sharing
/// violation or access-denied error (five attempts, 100 ms apart); inert elsewhere.
fn publish_replacing(from: &Path, to: &Path) -> rightkit_fs::Result<()> {
    use rightkit_fs::atomic::{
        publish_file_with_access_denied_retry, AccessDeniedRetry, AtomicWriteOptions, ReplaceMode,
        RetryPolicy,
    };
    publish_file_with_access_denied_retry(
        from,
        to,
        AtomicWriteOptions {
            replace: ReplaceMode::MoveWriteThrough,
            ..AtomicWriteOptions::default()
        },
        AccessDeniedRetry::new(RetryPolicy::new(4, std::time::Duration::from_millis(100))),
    )
}

const MAX_SLIDES: usize = 30;
const SETTLE: Duration = Duration::from_millis(700);
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub enum ScrapeErr {
    NeedsAuth(String),
    Other(String),
}

pub struct CarouselOut {
    pub slides: Vec<PathBuf>,
    pub caption: Option<String>,
    /// Originals mode only: 0-based indexes into `slides` that stayed a
    /// browser screenshot because no image URL resolved or its download failed.
    pub fallback: Vec<u32>,
}

struct SelectorMemory {
    path: PathBuf,
    entries: serde_json::Value,
}

impl SelectorMemory {
    fn load_from(path: PathBuf) -> Self {
        // Corrupt selector_memory.json must NEVER panic the engine. If the
        // file fails to read OR fails to parse as JSON, log a loud warning
        // and start with an empty in-memory map. The next save() call
        // (triggered by record()) will overwrite the corrupt file with a
        // freshly-serialized empty object, recovering without losing the
        // session. The original disk file is left in place until that
        // overwrite — never silently dropped.
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                eprintln!(
                    "[rightkit-media-fetch] selector memory read {} failed: {e} — starting empty",
                    path.display()
                );
                None
            }
        };
        let entries = match raw.as_deref() {
            None => serde_json::Value::Object(serde_json::Map::new()),
            Some(text) => match serde_json::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "[rightkit-media-fetch] selector memory {} is corrupt ({e}) — starting empty, next save() will overwrite",
                        path.display()
                    );
                    serde_json::Value::Object(serde_json::Map::new())
                }
            },
        };
        Self { path, entries }
    }

    fn get(&self, domain: &str, context: &str) -> Option<&str> {
        self.entries.get(domain)?.get(context)?.as_str()
    }

    fn record(&mut self, domain: &str, context: &str, selector: &str) {
        // Defensive: if the on-disk JSON happened to be an array/scalar
        // instead of an object (corrupt shape), reset to a fresh object
        // rather than panicking. The next save() will overwrite the bad
        // file with the corrected empty map.
        if !self.entries.is_object() {
            eprintln!(
                "[rightkit-media-fetch] selector memory shape invalid at {} — resetting to empty object, next save() will overwrite",
                self.path.display()
            );
            self.entries = serde_json::Value::Object(serde_json::Map::new());
        }
        let obj = self
            .entries
            .as_object_mut()
            .expect("entries is guaranteed to be a JSON object by the reset above");
        let domain_entry = obj
            .entry(domain)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(map) = domain_entry.as_object_mut() {
            map.insert(
                context.to_string(),
                serde_json::Value::String(selector.to_string()),
            );
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.entries) {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = rightkit_fs::atomic_write(&self.path, json.as_bytes()) {
                tracing::warn!(
                    target: "rightkit_media_fetch::carousel",
                    path = %self.path.display(),
                    error = %e,
                    "selector memory write failed"
                );
            }
        }
    }
}

/// Where a scrape writes before it replaces `slide_dir`: a sibling folder, so a
/// cancelled or failed run never touches the slides already on disk.
fn staging_dir_for(slide_dir: &Path) -> PathBuf {
    let name = slide_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    slide_dir.with_file_name(format!("{name}.staging"))
}

/// Runs `scrape` into a staging folder and swaps it in for `slide_dir` only on
/// success. On any error the staging folder is removed and `slide_dir` keeps
/// whatever it held (a cancelled re-run keeps the earlier slides). Returned
/// slide paths point into `slide_dir`.
fn run_staged(
    slide_dir: &Path,
    scrape: impl FnOnce(&Path) -> Result<CarouselOut, ScrapeErr>,
) -> Result<CarouselOut, ScrapeErr> {
    let staging = staging_dir_for(slide_dir);
    // A leftover from an interrupted run is never trusted.
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| ScrapeErr::Other(e.to_string()))?;
    let out = match scrape(&staging) {
        Ok(out) => out,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    if let Err(err) = rightkit_fs::publish_dir::publish_dir(&staging, slide_dir)
        .map_err(|e| ScrapeErr::Other(e.to_string()))
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    let slides = out
        .slides
        .iter()
        .map(|p| slide_dir.join(p.file_name().unwrap_or_default()))
        .collect();
    Ok(CarouselOut {
        slides,
        caption: out.caption,
        fallback: out.fallback,
    })
}

/// Scrape a carousel post into `slide_dir`: one screenshot per slide plus the caption.
pub fn scrape_carousel_cancelable(
    env: &CarouselEnv<'_>,
    url: &str,
    slide_dir: &Path,
    profile_dir: Option<&Path>,
    should_cancel: impl Fn() -> bool + Copy,
    progress: impl FnMut(u8, &str),
) -> Result<CarouselOut, ScrapeErr> {
    run_staged(slide_dir, |staging| {
        scrape_into(
            env,
            url,
            staging,
            profile_dir,
            false,
            should_cancel,
            progress,
        )
    })
}

/// Image-download variant (#248): same scrape, but each slide is saved as the
/// original image file read from the live page; a slide whose URL does not
/// resolve keeps its screenshot and is listed in `CarouselOut::fallback`.
pub fn scrape_carousel_originals_cancelable(
    env: &CarouselEnv<'_>,
    url: &str,
    slide_dir: &Path,
    profile_dir: Option<&Path>,
    should_cancel: impl Fn() -> bool + Copy,
    progress: impl FnMut(u8, &str),
) -> Result<CarouselOut, ScrapeErr> {
    run_staged(slide_dir, |staging| {
        scrape_into(
            env,
            url,
            staging,
            profile_dir,
            true,
            should_cancel,
            progress,
        )
    })
}

/// What the page shows for the slide currently on screen.
#[derive(Debug, Default, serde::Deserialize)]
struct SlideProbe {
    #[serde(default)]
    srcset: String,
    #[serde(default)]
    src: String,
}

/// Visible slide image (largest on-screen `<img>`) as JSON.
const VISIBLE_SLIDE_JS: &str = r#"(() => {
  const vw = innerWidth, vh = innerHeight;
  let best = null, bestArea = 0;
  for (const i of document.querySelectorAll('article img, main img')) {
    const r = i.getBoundingClientRect();
    const w = Math.min(r.right, vw) - Math.max(r.left, 0);
    const h = Math.min(r.bottom, vh) - Math.max(r.top, 0);
    if (r.width < 200 || r.height < 200 || w <= 0 || h <= 0) continue;
    if (w * h > bestArea) { best = i; bestArea = w * h; }
  }
  return JSON.stringify(best ? { srcset: best.srcset || '', src: best.currentSrc || best.src || '' } : {});
})()"#;

/// Embedded page JSON blobs that carry media URLs, as a JSON array of strings.
const EMBEDDED_JSON_JS: &str = r#"JSON.stringify([...document.querySelectorAll('script[type="application/json"]')].map(s => s.textContent).filter(t => t.includes('image_versions2') || t.includes('display_url')))"#;

/// Browser `eval` results arrive as JSON text, sometimes wrapped once more as
/// a JSON string.
fn parse_eval_json<T: serde::de::DeserializeOwned>(raw: &str) -> Option<T> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    match value {
        serde_json::Value::String(inner) => serde_json::from_str(&inner).ok(),
        other => serde_json::from_value(other).ok(),
    }
}

/// URL of the widest candidate in a `srcset` (`url 640w, url 1080w`); a bare
/// URL list without descriptors yields its last entry.
pub(crate) fn pick_largest_srcset(srcset: &str) -> Option<String> {
    let mut best: Option<(f64, &str)> = None;
    for candidate in srcset.split(',') {
        let mut parts = candidate.split_whitespace();
        let Some(url) = parts.next().filter(|u| u.starts_with("http")) else {
            continue;
        };
        let width = parts
            .next()
            .and_then(|d| d.trim_end_matches(['w', 'x']).parse::<f64>().ok())
            .unwrap_or(0.0);
        if best.is_none_or(|(w, _)| width >= w) {
            best = Some((width, url));
        }
    }
    best.map(|(_, url)| url.to_string())
}

/// Per-slide original image URLs from Instagram's embedded JSON: each
/// `carousel_media` item's widest `image_versions2` candidate (else its
/// `display_url`), or the single post's own image.
pub(crate) fn pick_json_image_urls(json_text: &str) -> Vec<String> {
    fn widest(item: &serde_json::Value) -> Option<String> {
        let best = item
            .pointer("/image_versions2/candidates")
            .and_then(|c| c.as_array())
            .and_then(|list| {
                list.iter()
                    .filter_map(|c| {
                        let url = c.get("url")?.as_str()?;
                        Some((c.get("width").and_then(|w| w.as_u64()).unwrap_or(0), url))
                    })
                    .max_by_key(|(w, _)| *w)
                    .map(|(_, url)| url.to_string())
            });
        best.or_else(|| {
            item.get("display_url")
                .and_then(|u| u.as_str())
                .map(str::to_string)
        })
    }
    fn walk(value: &serde_json::Value) -> Option<Vec<String>> {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(items) = map.get("carousel_media").and_then(|c| c.as_array()) {
                    let urls: Vec<String> = items.iter().filter_map(widest).collect();
                    if !urls.is_empty() {
                        return Some(urls);
                    }
                }
                if let Some(url) = widest(value) {
                    return Some(vec![url]);
                }
                map.values().find_map(walk)
            }
            serde_json::Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    serde_json::from_str::<serde_json::Value>(json_text)
        .ok()
        .and_then(|value| walk(&value))
        .unwrap_or_default()
}

/// The original-image URL for slide `index`: the visible `<img>`'s largest
/// srcset candidate (or its `src`), else the embedded JSON's entry.
fn pick_slide_url(probe: &SlideProbe, json_urls: &[String], index: usize) -> Option<String> {
    pick_largest_srcset(&probe.srcset)
        .or_else(|| Some(probe.src.clone()).filter(|s| s.starts_with("http")))
        .or_else(|| json_urls.get(index).cloned())
}

fn scrape_into(
    env: &CarouselEnv<'_>,
    url: &str,
    slide_dir: &Path,
    _profile_dir: Option<&Path>,
    originals: bool,
    should_cancel: impl Fn() -> bool + Copy,
    mut progress: impl FnMut(u8, &str),
) -> Result<CarouselOut, ScrapeErr> {
    let safe_url = env
        .fetcher
        .resolve_public_redirects_for_browser(url)
        .map_err(ScrapeErr::Other)?;
    // Pin the browser to the vetted address of the final URL (#81).
    let host_pin = env
        .browsers()
        .resolve_host_pin(&safe_url)
        .map_err(ScrapeErr::Other)?;
    let browser: Box<dyn HeadlessBrowser> = env
        .browsers()
        .new_backend(carousel_browser_config(host_pin));
    let started = Instant::now();
    let is_cancelled = || should_cancel() || started.elapsed() >= SCRAPE_TIMEOUT;

    if let Err(err) = browser.open(&safe_url, &is_cancelled) {
        let _ = browser.close();
        return Err(ScrapeErr::Other(err));
    }
    check_timeout(started)?;
    let _ = browser.wait_for_selector("body", &is_cancelled);
    progress(30, "opened carousel page");
    check_timeout(started)?;
    if is_login_wall(&*browser, &is_cancelled) {
        let _ = browser.close();
        return Err(ScrapeErr::NeedsAuth("Instagram".into()));
    }
    ensure_instagram_content_available(&*browser, &is_cancelled)?;

    let _ = browser.click("svg[aria-label='Close']", &is_cancelled);
    progress(35, "checking carousel slides");
    sleep(SETTLE);

    let mut slides = Vec::new();
    let mut slide_urls: Vec<Option<String>> = Vec::new();
    let json_urls: Vec<String> = if originals {
        browser
            .eval(EMBEDDED_JSON_JS, &is_cancelled)
            .ok()
            .and_then(|raw| parse_eval_json::<Vec<String>>(&raw))
            .and_then(|blobs| {
                blobs
                    .iter()
                    .map(|b| pick_json_image_urls(b))
                    .find(|u| !u.is_empty())
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut last_hash: Option<u64> = None;
    let domain_owned = crate::detect::host(url).unwrap_or_default();
    let domain = domain_owned.as_str();
    let mut mem = SelectorMemory::load_from(env.selector_memory_path.clone());
    let default_next_sel = "[aria-label='Next']";
    let next_sel = mem
        .get(domain, "carousel_next")
        .unwrap_or(default_next_sel)
        .to_string();
    for i in 1..=MAX_SLIDES {
        check_timeout(started)?;
        let p = slide_dir.join(format!("slide_{i:02}.png"));
        let pct = (35 + (i as u8).saturating_mul(2)).min(55);
        progress(pct, "capturing carousel slide");
        browser
            .screenshot(&p, &is_cancelled)
            .map_err(ScrapeErr::Other)?;

        if let Ok(bytes) = std::fs::read(&p) {
            let h = quick_hash(&bytes);
            if last_hash == Some(h) {
                let _ = std::fs::remove_file(&p);
                break;
            }
            last_hash = Some(h);
        }
        if originals {
            let probe = browser
                .eval(VISIBLE_SLIDE_JS, &is_cancelled)
                .ok()
                .and_then(|raw| parse_eval_json::<SlideProbe>(&raw))
                .unwrap_or_default();
            slide_urls.push(pick_slide_url(&probe, &json_urls, slides.len()));
        }
        slides.push(p);

        let advanced = if next_sel.as_str() != default_next_sel {
            browser.click(&next_sel, &is_cancelled).is_ok()
        } else {
            false
        } || browser.click(default_next_sel, &is_cancelled).is_ok()
            || {
                let candidates = [
                    "button[aria-label='Next']",
                    "svg[aria-label='Next']",
                    "[data-testid='nextButton']",
                    "button:has(svg[aria-label='Next'])",
                ];
                let mut found = false;
                for sel in &candidates {
                    if browser.click(sel, &is_cancelled).is_ok() {
                        if *sel != next_sel {
                            mem.record(domain, "carousel_next", sel);
                        }
                        found = true;
                        break;
                    }
                }
                found
            };
        if !advanced {
            break;
        }
        progress(pct.saturating_add(1).min(56), "advanced carousel slide");
        sleep(SETTLE);
    }

    progress(58, "reading carousel caption");
    let caption = browser
        .eval(
            "document.querySelector('meta[property=\"og:description\"]')?.content || ''",
            &is_cancelled,
        )
        .ok()
        .filter(|s| !s.is_empty());

    let _ = browser.close();
    let mut fallback = Vec::new();
    if originals {
        progress(58, "saving original images");
        for (i, slide) in slides.iter_mut().enumerate() {
            check_timeout(started)?;
            if should_cancel() {
                return Err(ScrapeErr::Other("cancelled".into()));
            }
            let base = slide_dir.join(format!("slide_{:02}", i + 1));
            let saved = slide_urls
                .get(i)
                .and_then(|u| u.as_deref())
                .and_then(|u| {
                    crate::tiktok::download_image_with_referer(
                        env.web,
                        u,
                        &base,
                        "https://www.instagram.com/",
                    )
                    .ok()
                })
                .and_then(|ext| {
                    let dest = base.with_extension(ext);
                    // Owner durable replace (retries a Windows sharing violation).
                    publish_replacing(&base, &dest).ok().map(|()| dest)
                });
            match saved {
                Some(dest) => {
                    let _ = std::fs::remove_file(&*slide);
                    *slide = dest;
                }
                None => {
                    let _ = std::fs::remove_file(&base);
                    fallback.push(i as u32);
                }
            }
        }
    }
    Ok(CarouselOut {
        slides,
        caption,
        fallback,
    })
}

fn is_login_wall(browser: &dyn HeadlessBrowser, should_cancel: &dyn Fn() -> bool) -> bool {
    browser
        .eval("location.href", should_cancel)
        .map(|href| href.contains("/accounts/login") || href.contains("/login"))
        .unwrap_or(false)
}

fn ensure_instagram_content_available(
    browser: &dyn HeadlessBrowser,
    should_cancel: &dyn Fn() -> bool,
) -> Result<(), ScrapeErr> {
    let text = browser
        .eval("document.body?.innerText || ''", should_cancel)
        .unwrap_or_default();
    instagram_page_gate(&text).map_or(Ok(()), Err)
}

fn instagram_page_gate(text: &str) -> Option<ScrapeErr> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_ascii_lowercase();
    if lower.contains("sign in to instagram to continue") {
        return Some(ScrapeErr::NeedsAuth("Instagram".into()));
    }
    if lower.contains("log in")
        && lower.contains("sign up")
        && (lower.contains("can't see this content") || lower.contains("content unavailable"))
    {
        return Some(ScrapeErr::NeedsAuth("Instagram".into()));
    }
    if lower.contains("people under 23 can't see this content")
        || lower.contains("this account has set limits on who can see their profile and content")
    {
        return Some(ScrapeErr::Other(format!(
            "Instagram content unavailable: {normalized}"
        )));
    }
    if lower.contains("sorry, this page isn't available")
        || lower.contains("this content isn't available")
        || lower.contains("content unavailable")
        || lower.contains("sorry, something went wrong")
        || lower.contains("we're working on getting this fixed as soon as we can")
    {
        return Some(ScrapeErr::Other(format!(
            "Instagram content unavailable: {normalized}"
        )));
    }
    None
}

fn check_timeout(started: Instant) -> Result<(), ScrapeErr> {
    if started.elapsed() >= SCRAPE_TIMEOUT {
        Err(ScrapeErr::Other(format!(
            "browser scrape timed out after {}s",
            SCRAPE_TIMEOUT.as_secs()
        )))
    } else {
        Ok(())
    }
}

fn quick_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod staged_scrape_tests {
    use super::*;

    fn earlier_slides(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("slide_01.png"), b"earlier").unwrap();
    }

    fn out_with(staging: &Path, names: &[&str]) -> CarouselOut {
        CarouselOut {
            slides: names.iter().map(|n| staging.join(n)).collect(),
            caption: Some("caption".into()),
            fallback: Vec::new(),
        }
    }

    #[test]
    fn staging_folder_is_a_sibling_of_the_slide_folder() {
        let slide_dir = Path::new("out").join("ABC123");
        assert_eq!(
            staging_dir_for(&slide_dir),
            Path::new("out").join("ABC123.staging")
        );
    }

    #[test]
    fn cancelled_rerun_keeps_the_earlier_slides() {
        let root = tempfile::tempdir().unwrap();
        let slide_dir = root.path().join("ABC123");
        earlier_slides(&slide_dir);

        let result = run_staged(&slide_dir, |staging| {
            std::fs::write(staging.join("slide_01.png"), b"partial").unwrap();
            Err(ScrapeErr::Other("cancelled".into()))
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(slide_dir.join("slide_01.png")).unwrap(),
            b"earlier",
            "a cancelled re-run replaced the earlier slides"
        );
        assert!(!staging_dir_for(&slide_dir).exists(), "staging left behind");
    }

    #[test]
    fn successful_rerun_swaps_in_the_new_slides_and_rebases_paths() {
        let root = tempfile::tempdir().unwrap();
        let slide_dir = root.path().join("ABC123");
        earlier_slides(&slide_dir);

        let out = run_staged(&slide_dir, |staging| {
            std::fs::write(staging.join("slide_01.png"), b"fresh").unwrap();
            std::fs::write(staging.join("slide_02.png"), b"fresh").unwrap();
            Ok(out_with(staging, &["slide_01.png", "slide_02.png"]))
        })
        .expect("scrape");

        assert_eq!(
            std::fs::read(slide_dir.join("slide_01.png")).unwrap(),
            b"fresh"
        );
        assert!(slide_dir.join("slide_02.png").exists());
        assert!(!staging_dir_for(&slide_dir).exists(), "staging left behind");
        assert_eq!(
            out.slides,
            vec![
                slide_dir.join("slide_01.png"),
                slide_dir.join("slide_02.png")
            ]
        );
        assert_eq!(out.caption.as_deref(), Some("caption"));
    }

    #[test]
    fn leftover_staging_from_an_interrupted_run_is_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        let slide_dir = root.path().join("ABC123");
        let staging = staging_dir_for(&slide_dir);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("ghost.png"), b"stale").unwrap();

        let out = run_staged(&slide_dir, |staging| {
            std::fs::write(staging.join("slide_01.png"), b"fresh").unwrap();
            Ok(out_with(staging, &["slide_01.png"]))
        })
        .expect("scrape");

        assert!(!slide_dir.join("ghost.png").exists());
        assert_eq!(out.slides, vec![slide_dir.join("slide_01.png")]);
    }

    #[test]
    fn srcset_picks_the_widest_candidate() {
        let srcset =
            "https://c/a.jpg?x=1 640w, https://c/b.jpg?x=1 1080w, https://c/c.jpg?x=1 750w";
        assert_eq!(
            pick_largest_srcset(srcset).as_deref(),
            Some("https://c/b.jpg?x=1")
        );
        assert_eq!(pick_largest_srcset(""), None);
    }

    #[test]
    fn embedded_json_yields_one_widest_url_per_carousel_slide() {
        let json = r#"{"a":{"items":[{"carousel_media":[
          {"image_versions2":{"candidates":[{"url":"https://c/1s.jpg","width":320},{"url":"https://c/1l.jpg","width":1080}]}},
          {"display_url":"https://c/2.webp"}]}]}}"#;
        assert_eq!(
            pick_json_image_urls(json),
            vec![
                "https://c/1l.jpg".to_string(),
                "https://c/2.webp".to_string()
            ]
        );
        let single =
            r#"{"x":{"image_versions2":{"candidates":[{"url":"https://c/s.jpg","width":1}]}}}"#;
        assert_eq!(
            pick_json_image_urls(single),
            vec!["https://c/s.jpg".to_string()]
        );
        assert!(pick_json_image_urls("not json").is_empty());
    }

    #[test]
    fn slide_url_prefers_the_page_image_then_json() {
        let json = vec![
            "https://c/j0.jpg".to_string(),
            "https://c/j1.jpg".to_string(),
        ];
        let with_set = SlideProbe {
            srcset: "https://c/p.jpg 1080w".into(),
            src: "https://c/q.jpg".into(),
        };
        assert_eq!(
            pick_slide_url(&with_set, &json, 1).as_deref(),
            Some("https://c/p.jpg")
        );
        let src_only = SlideProbe {
            srcset: String::new(),
            src: "https://c/q.jpg".into(),
        };
        assert_eq!(
            pick_slide_url(&src_only, &json, 1).as_deref(),
            Some("https://c/q.jpg")
        );
        let none = SlideProbe::default();
        assert_eq!(
            pick_slide_url(&none, &json, 1).as_deref(),
            Some("https://c/j1.jpg")
        );
        assert_eq!(pick_slide_url(&none, &json, 5), None);
    }
}

/// The carousel browser's launch config: default, plus the vetted address pin (#81).
fn carousel_browser_config(host_pin: Option<HostPin>) -> BrowserConfig {
    BrowserConfig {
        host_pin,
        ..BrowserConfig::default()
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn carousel_browser_config_carries_the_vetted_pin() {
        let pin = HostPin::new("example.com", IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))).unwrap();
        let config = carousel_browser_config(Some(pin.clone()));
        assert_eq!(config.host_pin, Some(pin));
        assert_eq!(carousel_browser_config(None).host_pin, None);
    }
}

#[cfg(test)]
mod selector_memory_tests {
    use super::SelectorMemory;

    #[test]
    fn record_is_written_atomically_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("selector_memory.json");
        let mut mem = SelectorMemory::load_from(path.clone());
        assert_eq!(mem.get("instagram.com", "carousel_next"), None);
        mem.record(
            "instagram.com",
            "carousel_next",
            "button[aria-label='Next']",
        );
        let reloaded = SelectorMemory::load_from(path.clone());
        assert_eq!(
            reloaded.get("instagram.com", "carousel_next"),
            Some("button[aria-label='Next']")
        );
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["selector_memory.json".to_string()]);
    }

    #[test]
    fn corrupt_file_starts_empty_and_is_replaced_by_the_next_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selector_memory.json");
        std::fs::write(&path, "{not json").unwrap();
        let mut mem = SelectorMemory::load_from(path.clone());
        assert_eq!(mem.get("x.com", "k"), None);
        mem.record("x.com", "k", "a.b");
        assert_eq!(
            SelectorMemory::load_from(path).get("x.com", "k"),
            Some("a.b")
        );
    }
}
