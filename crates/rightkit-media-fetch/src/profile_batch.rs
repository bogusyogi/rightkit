//! #248: save the images of a signed-in profile's newest posts.
//!
//! The engine lists a profile through the signed-in browser (never raw HTTP),
//! keeps only genuine post links for that platform, applies a configurable cap
//! (default 50, newest first as the page orders them) and queues one
//! `MediaAction::Images` job per post through the ordinary queue submit path.
//! Needs sign-in, so Pro. Instagram, TikTok (photo posts) and X are supported.
//!
//! The listing sits behind [`ProfileLister`] so the limit, ordering, de-dup and
//! safety filtering are unit-tested without a browser or network.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rightkit_web_extract::browser::{BrowserConfig, BrowserHost, HeadlessBrowser, HostPin};

use crate::detect::{classify, classify_profile, ProfileKind};
use crate::playlist_batch::BatchContext;
use crate::protocol::{
    AuthStateSnapshot, EngineError, EntitlementsSnapshot, JobInfo, Kind, ProfileImagesResponse,
    Stage, PROFILE_DEFAULT_LIMIT, PROFILE_MAX_LIMIT,
};
use crate::source_auth::{platform_profile_dir, AuthLayout, SourcePlatform};
use crate::{MediaAction, MediaSpec};

/// Wall-clock cap on one profile listing.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ROUNDS: usize = 40;
const SETTLE: Duration = Duration::from_millis(1200);
/// Rounds in a row without a new post before the listing stops.
const STALL_ROUNDS: usize = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum ListErr {
    /// No usable sign-in for this platform (label as the shell expects it).
    NeedsAuth(String),
    Other(String),
}

/// Source of raw post links for a profile page, in page order.
pub trait ProfileLister {
    /// Return links found on `profile_url`, newest first as the page orders
    /// them, until at least `want` post links are known or the page is
    /// exhausted. May include non-post links; [`enumerate_posts`] filters.
    fn list(
        &self,
        kind: ProfileKind,
        profile_url: &str,
        want: usize,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, ListErr>;
}

/// Resolve a requested limit: absent or zero means the default, and the
/// result never exceeds [`PROFILE_MAX_LIMIT`].
pub fn resolve_limit(requested: Option<u32>) -> u32 {
    match requested {
        None | Some(0) => PROFILE_DEFAULT_LIMIT,
        Some(n) => n.min(PROFILE_MAX_LIMIT),
    }
}

/// Sign-in label the shell compares (`needs_auth`, `auth-signed-in`).
pub fn platform_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Instagram => "Instagram",
        ProfileKind::TikTok => SourcePlatform::TikTok.label(),
        ProfileKind::X => SourcePlatform::X.label(),
    }
}

/// A post link that passed the strict shape check.
#[derive(Debug, PartialEq, Eq)]
struct Post {
    /// Account named in the link, where the platform puts one there.
    user: Option<String>,
    canonical: String,
}

fn https_host_path(raw: &str, hosts: &[&str]) -> Option<Vec<String>> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if !hosts.contains(&host.as_str()) {
        return None;
    }
    Some(
        url.path()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn code_ok(code: &str, extra: &[char], max: usize) -> bool {
    !code.is_empty()
        && code.len() <= max
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

fn parse_post(kind: ProfileKind, raw: &str) -> Option<Post> {
    match kind {
        // `/p/<code>/` or `/<user>/p/<code>/`; reels are not image posts.
        ProfileKind::Instagram => {
            let segs = https_host_path(raw, &["instagram.com", "www.instagram.com"])?;
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let code = match segs.as_slice() {
                ["p", code] | [_, "p", code] => *code,
                _ => return None,
            };
            code_ok(code, &['-', '_'], 32).then(|| Post {
                user: None,
                canonical: format!("https://www.instagram.com/p/{code}/"),
            })
        }
        // `/@<user>/photo/<id>`; `/video/` posts are not image posts.
        ProfileKind::TikTok => {
            let segs = https_host_path(raw, &["tiktok.com", "www.tiktok.com"])?;
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let [user, "photo", id] = segs.as_slice() else {
                return None;
            };
            let handle = user.strip_prefix('@')?;
            (code_ok(handle, &['_', '.'], 40)
                && id.chars().all(|c| c.is_ascii_digit())
                && !id.is_empty()
                && id.len() <= 25)
                .then(|| Post {
                    user: Some(handle.to_ascii_lowercase()),
                    canonical: format!("https://www.tiktok.com/@{handle}/photo/{id}"),
                })
        }
        // `/<user>/status/<id>[/photo/n]`; `/i/status/..` is not a profile post.
        ProfileKind::X => {
            let segs = https_host_path(
                raw,
                &["x.com", "www.x.com", "twitter.com", "www.twitter.com"],
            )?;
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let (user, id) = match segs.as_slice() {
                [user, "status", id] | [user, "status", id, "photo", _] => (*user, *id),
                _ => return None,
            };
            (user != "i"
                && code_ok(user, &['_'], 15)
                && !id.is_empty()
                && id.len() <= 25
                && id.chars().all(|c| c.is_ascii_digit()))
            .then(|| Post {
                user: Some(user.to_ascii_lowercase()),
                canonical: format!("https://x.com/{user}/status/{id}"),
            })
        }
    }
}

/// The profile's account name, lowercased, from its URL.
fn profile_handle(profile_url: &str) -> Option<String> {
    let url = url::Url::parse(profile_url.trim()).ok()?;
    let first = url.path().split('/').find(|s| !s.is_empty())?;
    Some(first.trim_start_matches('@').to_ascii_lowercase())
}

/// Canonical post URL for a link on a `kind` profile, or `None` when it is
/// not a post of the right shape. Links come from a web page, so only a
/// strict shape on the platform's own hosts gets through.
pub fn canonical_post_url(kind: ProfileKind, raw: &str) -> Option<String> {
    parse_post(kind, raw).map(|p| p.canonical)
}

/// URL actually opened to list `profile_url`: X uses the media tab.
pub fn listing_url(kind: ProfileKind, profile_url: &str) -> String {
    match (kind, profile_handle(profile_url)) {
        (ProfileKind::X, Some(handle)) => format!("https://x.com/{handle}/media"),
        (ProfileKind::TikTok, Some(handle)) => format!("https://www.tiktok.com/@{handle}"),
        _ => profile_url.trim().to_string(),
    }
}

/// List `profile_url`, keep post links only, drop duplicates (first sighting
/// wins, so page order is kept) and cut to `limit`. TikTok and X posts must
/// belong to the profile's own account. On X, when the grid shows `/photo/`
/// tiles only those are kept; otherwise every status link is (the per-job
/// engine path rejects posts without images).
pub fn enumerate_posts(
    lister: &dyn ProfileLister,
    profile_url: &str,
    limit: u32,
    should_cancel: &dyn Fn() -> bool,
) -> Result<Vec<String>, ListErr> {
    let kind = classify_profile(profile_url)
        .ok_or_else(|| ListErr::Other("that link is not a profile page".into()))?;
    let limit = limit as usize;
    let raw = lister.list(kind, profile_url.trim(), limit, should_cancel)?;
    let handle = profile_handle(profile_url);
    let prefer_photos = kind == ProfileKind::X && raw.iter().any(|l| l.contains("/photo/"));
    let mut seen = std::collections::HashSet::new();
    let mut posts = Vec::new();
    for link in raw {
        if prefer_photos && !link.contains("/photo/") {
            continue;
        }
        let Some(post) = parse_post(kind, &link) else {
            continue;
        };
        if kind != ProfileKind::Instagram && post.user != handle {
            continue;
        }
        if seen.insert(post.canonical.clone()) {
            posts.push(post.canonical);
            if posts.len() == limit {
                break;
            }
        }
    }
    Ok(posts)
}

/// The Images job for one post. `kind` is what the post link classifies as,
/// the same routing a pasted link gets.
pub fn image_job(post_url: &str, id: String) -> JobInfo {
    JobInfo {
        id,
        input: post_url.to_string(),
        kind: Kind::from(classify(post_url)),
        stage: Stage::Queued,
        pct: 0,
        title: None,
        error: None,
        error_kind: None,
        needs_auth: None,
        media: Some(MediaSpec {
            action: MediaAction::Images,
            video: None,
            audio: None,
        }),
        attach_to: None,
        prepared: None,
    }
}

/// List a profile and queue one Images job per post through `submit` (the
/// queue's public submit). Pure orchestration: the lister and `submit` are
/// injected.
pub fn run_profile_images(
    lister: &dyn ProfileLister,
    entitlements: &EntitlementsSnapshot,
    ctx: &BatchContext<'_>,
    url: &str,
    limit: Option<u32>,
    should_cancel: &dyn Fn() -> bool,
    mut submit: impl FnMut(JobInfo) -> Result<(), String>,
) -> Result<ProfileImagesResponse, EngineError> {
    let Some(kind) = classify_profile(url) else {
        return Err(EngineError::new(
            "profile_unsupported",
            "that link is not a profile page",
        ));
    };
    // Defense in depth: the shell gates this, a crafted request must not skip it.
    if !entitlements.source_auth {
        return Err(EngineError::new(
            "license_required",
            format!(
                "Profile image downloads need {} Pro (they use your signed-in session).",
                ctx.product
            ),
        ));
    }
    let limit = resolve_limit(limit);
    let posts =
        enumerate_posts(lister, url.trim(), limit, should_cancel).map_err(|err| match err {
            ListErr::NeedsAuth(platform) => EngineError::new(
                "needs_auth",
                format!("Sign in to {platform} to list this profile."),
            )
            .with_details(serde_json::json!({ "platform": platform })),
            ListErr::Other(message) => EngineError::new("profile_list", message),
        })?;
    if posts.is_empty() {
        let what = match kind {
            ProfileKind::TikTok => "photo posts",
            _ => "image posts",
        };
        return Err(EngineError::new(
            "profile_empty",
            format!("no {what} were found on that profile"),
        ));
    }
    let found = posts.len() as u32;
    let mut job_ids = Vec::new();
    let mut rejected = Vec::new();
    for post in &posts {
        let id = (ctx.new_id)();
        match submit(image_job(post, id.clone())) {
            Ok(()) => job_ids.push(id),
            Err(err) => rejected.push(err),
        }
    }
    Ok(ProfileImagesResponse {
        profile: url.trim().to_string(),
        limit,
        found,
        accepted: job_ids.len() as u32,
        job_ids,
        rejected,
    })
}

/// One listing at a time: each opens a signed-in browser.
static LISTING: AtomicBool = AtomicBool::new(false);

pub struct ListingGuard;

impl ListingGuard {
    pub fn try_begin() -> Option<ListingGuard> {
        LISTING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ListingGuard)
    }
}

impl Drop for ListingGuard {
    fn drop(&mut self) {
        LISTING.store(false, Ordering::Release);
    }
}

/// Lists a profile in the platform's signed-in browser profile.
pub struct BrowserLister<'a> {
    auth: AuthStateSnapshot,
    layout: AuthLayout,
    browsers: &'a dyn BrowserHost,
}

impl<'a> BrowserLister<'a> {
    /// A lister over the host's browser factory and on-disk sign-in layout.
    pub fn from_auth(
        auth: &AuthStateSnapshot,
        layout: &AuthLayout,
        browsers: &'a dyn BrowserHost,
    ) -> Self {
        Self {
            auth: auth.clone(),
            layout: layout.clone(),
            browsers,
        }
    }

    /// Sign-in profile folder for `kind`: the pushed snapshot's when it names
    /// the platform, else the canonical `<data_root>/auth/<platform>` folder.
    fn profile_dir(&self, kind: ProfileKind) -> Option<PathBuf> {
        match kind {
            ProfileKind::Instagram => Some(instagram_profile_dir(&self.layout, &self.auth)),
            ProfileKind::TikTok => {
                platform_profile_dir(&self.layout, &self.auth, SourcePlatform::TikTok)
            }
            ProfileKind::X => platform_profile_dir(&self.layout, &self.auth, SourcePlatform::X),
        }
    }
}

fn instagram_profile_dir(layout: &AuthLayout, auth: &AuthStateSnapshot) -> PathBuf {
    if auth
        .platform
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case("instagram"))
    {
        if let Some(dir) = auth
            .profile_dir
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            return PathBuf::from(dir);
        }
    }
    layout.auth_dir().join("instagram")
}

fn collect_js(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Instagram => {
            "JSON.stringify(Array.from(document.querySelectorAll('a[href*=\"/p/\"]')).map(a => a.href))"
        }
        ProfileKind::TikTok => {
            "JSON.stringify(Array.from(document.querySelectorAll('a[href*=\"/photo/\"]')).map(a => a.href))"
        }
        ProfileKind::X => {
            "JSON.stringify(Array.from(document.querySelectorAll('a[href*=\"/status/\"]')).map(a => a.href))"
        }
    }
}

const SCROLL_JS: &str = "window.scrollTo(0, document.body.scrollHeight) || 'ok'";

/// Whether an empty page's text is a logged-out wall for `kind`.
fn looks_logged_out(kind: ProfileKind, text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    match kind {
        ProfileKind::Instagram => t.contains("log in") && t.contains("sign up"),
        ProfileKind::TikTok => t.contains("log in to tiktok") || t.contains("log in to follow"),
        ProfileKind::X => (t.contains("log in") || t.contains("sign in")) && t.contains("sign up"),
    }
}

impl ProfileLister for BrowserLister<'_> {
    fn list(
        &self,
        kind: ProfileKind,
        profile_url: &str,
        want: usize,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, ListErr> {
        // No signed-in profile on disk: refuse before any browser or request
        // runs, so an unauthenticated scrape is never attempted.
        let profile = match self.profile_dir(kind) {
            Some(dir) if dir.is_dir() => dir,
            _ => return Err(ListErr::NeedsAuth(platform_label(kind).into())),
        };
        let url = listing_url(kind, profile_url);
        // Pin the browser to the vetted address of the listing host (#81).
        let host_pin = self
            .browsers
            .resolve_host_pin(&url)
            .map_err(ListErr::Other)?;
        let browser = self
            .browsers
            .new_backend(profile_listing_config(profile, host_pin));
        let started = Instant::now();
        let cancelled = || should_cancel() || started.elapsed() >= LIST_TIMEOUT;
        let result = collect_links(&*browser, kind, &url, want, &cancelled);
        let _ = browser.close();
        result
    }
}

fn collect_links(
    browser: &dyn HeadlessBrowser,
    kind: ProfileKind,
    url: &str,
    want: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<String>, ListErr> {
    let auth_err = || ListErr::NeedsAuth(platform_label(kind).into());
    browser.open(url, cancelled).map_err(ListErr::Other)?;
    let _ = browser.wait_for_selector("body", cancelled);
    if let Ok(href) = browser.eval("location.href", cancelled) {
        if href.contains("/accounts/login") || href.contains("/login") {
            return Err(auth_err());
        }
    }
    let mut links: Vec<String> = Vec::new();
    let mut known = std::collections::HashSet::new();
    let mut stalled = 0usize;
    for _ in 0..MAX_ROUNDS {
        if cancelled() {
            break;
        }
        sleep(SETTLE);
        let raw = browser
            .eval(collect_js(kind), cancelled)
            .map_err(ListErr::Other)?;
        let found: Vec<String> = serde_json::from_str(raw.trim()).unwrap_or_default();
        let before = known.len();
        for link in found {
            if let Some(post) = canonical_post_url(kind, &link) {
                if known.insert(post) {
                    links.push(link);
                }
            }
        }
        if known.len() >= want {
            break;
        }
        stalled = if known.len() == before {
            stalled + 1
        } else {
            0
        };
        if stalled >= STALL_ROUNDS {
            break;
        }
        let _ = browser.eval(SCROLL_JS, cancelled);
    }
    if links.is_empty() {
        // Nothing found: tell a login wall apart from an empty/private profile.
        let text = browser
            .eval("document.body?.innerText || ''", cancelled)
            .unwrap_or_default();
        if looks_logged_out(kind, &text) {
            return Err(auth_err());
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("isn't available")
            || lower.contains("is private")
            || lower.contains("account doesn't exist")
            || lower.contains("this account is private")
        {
            return Err(ListErr::Other(
                "that profile is private or unavailable".into(),
            ));
        }
    }
    Ok(links)
}

/// Launch config for a profile-listing browser, with the vetted pin (#81).
fn profile_listing_config(profile: PathBuf, host_pin: Option<HostPin>) -> BrowserConfig {
    BrowserConfig {
        profile_dir: Some(profile),
        host_pin,
        ..BrowserConfig::default()
    }
}
