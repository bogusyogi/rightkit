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

#[cfg(test)]
mod tests {
    #[test]
    fn profile_listing_config_carries_the_vetted_pin() {
        use std::net::{IpAddr, Ipv4Addr};
        let pin = HostPin::new(
            "www.instagram.com",
            IpAddr::V4(Ipv4Addr::new(157, 240, 1, 1)),
        )
        .unwrap();
        let dir = std::path::PathBuf::from("profile");
        let config = super::profile_listing_config(dir.clone(), Some(pin.clone()));
        assert_eq!(config.host_pin, Some(pin));
        assert_eq!(config.profile_dir, Some(dir));
    }

    use super::*;
    use std::cell::RefCell;

    struct Fake {
        links: Vec<&'static str>,
        err: Option<ListErr>,
        asked: RefCell<Option<(ProfileKind, usize)>>,
    }

    impl Fake {
        fn with(links: Vec<&'static str>) -> Self {
            Fake {
                links,
                err: None,
                asked: RefCell::new(None),
            }
        }
    }

    impl ProfileLister for Fake {
        fn list(
            &self,
            kind: ProfileKind,
            _url: &str,
            want: usize,
            _c: &dyn Fn() -> bool,
        ) -> Result<Vec<String>, ListErr> {
            *self.asked.borrow_mut() = Some((kind, want));
            match &self.err {
                Some(ListErr::NeedsAuth(p)) => Err(ListErr::NeedsAuth(p.clone())),
                Some(ListErr::Other(m)) => Err(ListErr::Other(m.clone())),
                None => Ok(self.links.iter().map(|s| s.to_string()).collect()),
            }
        }
    }

    fn make_id() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!("id-{}", NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn ctx() -> BatchContext<'static> {
        BatchContext {
            product: "Product",
            new_id: &make_id,
        }
    }

    fn pro() -> EntitlementsSnapshot {
        crate::protocol::entitlements_snapshot_for_tier(crate::protocol::LicenseTier::Pro)
    }

    fn free() -> EntitlementsSnapshot {
        crate::protocol::entitlements_snapshot_for_tier(crate::protocol::LicenseTier::Free)
    }

    const PROFILE: &str = "https://www.instagram.com/some.user/";
    const TT_PROFILE: &str = "https://www.tiktok.com/@some.user";
    const X_PROFILE: &str = "https://x.com/someuser";

    #[test]
    fn pro_snapshot_has_source_auth_and_free_does_not() {
        assert!(pro().source_auth);
        assert!(!free().source_auth);
    }

    #[test]
    fn limit_defaults_to_fifty_and_is_clamped() {
        assert_eq!(resolve_limit(None), 50);
        assert_eq!(resolve_limit(Some(0)), 50);
        assert_eq!(resolve_limit(Some(7)), 7);
        assert_eq!(resolve_limit(Some(100_000)), PROFILE_MAX_LIMIT);
    }

    #[test]
    fn enumeration_keeps_page_order_dedupes_and_applies_the_limit() {
        let lister = Fake::with(vec![
            "https://www.instagram.com/p/AAA/",
            "https://www.instagram.com/some.user/p/BBB/?img_index=1",
            "https://www.instagram.com/p/AAA/?utm=x",
            "https://www.instagram.com/p/CCC/",
            "https://www.instagram.com/p/DDD/",
        ]);
        let posts = enumerate_posts(&lister, PROFILE, 3, &|| false).unwrap();
        assert_eq!(
            posts,
            vec![
                "https://www.instagram.com/p/AAA/",
                "https://www.instagram.com/p/BBB/",
                "https://www.instagram.com/p/CCC/",
            ]
        );
        assert_eq!(*lister.asked.borrow(), Some((ProfileKind::Instagram, 3)));
    }

    #[test]
    fn enumeration_drops_reels_and_foreign_links() {
        let lister = Fake::with(vec![
            "https://www.instagram.com/reel/RRR/",
            "https://www.instagram.com/some.user/reel/SSS/",
            "https://evil.example/p/XXX/",
            "http://www.instagram.com/p/INSECURE/",
            "https://www.instagram.com.evil.example/p/YYY/",
            "https://www.instagram.com/p/OK1/",
            "javascript:alert(1)",
        ]);
        let posts = enumerate_posts(&lister, PROFILE, 50, &|| false).unwrap();
        assert_eq!(posts, vec!["https://www.instagram.com/p/OK1/"]);
    }

    #[test]
    fn tiktok_keeps_only_own_photo_posts_in_order_with_the_limit() {
        let lister = Fake::with(vec![
            "https://www.tiktok.com/@some.user/video/111",
            "https://www.tiktok.com/@some.user/photo/222",
            "https://www.tiktok.com/@other/photo/333",
            "https://www.tiktok.com/@some.user/photo/222?lang=en",
            "https://www.tiktok.com/@SOME.user/photo/444",
            "https://www.tiktok.com/@some.user/photo/555",
            "https://evil.example/@some.user/photo/666",
            "http://www.tiktok.com/@some.user/photo/777",
            "https://www.tiktok.com/@some.user/photo/12ab",
        ]);
        let posts = enumerate_posts(&lister, TT_PROFILE, 2, &|| false).unwrap();
        assert_eq!(
            posts,
            vec![
                "https://www.tiktok.com/@some.user/photo/222",
                "https://www.tiktok.com/@SOME.user/photo/444",
            ]
        );
        assert_eq!(*lister.asked.borrow(), Some((ProfileKind::TikTok, 2)));
    }

    #[test]
    fn x_prefers_photo_tiles_and_strips_to_the_status_url() {
        let lister = Fake::with(vec![
            "https://x.com/someuser/status/1/video/1",
            "https://x.com/someuser/status/2/photo/1",
            "https://x.com/someuser/status/2/photo/2",
            "https://x.com/someuser/status/3/photo/1",
            "https://x.com/otheruser/status/4/photo/1",
            "https://x.com/i/status/5/photo/1",
            "https://evil.example/someuser/status/6/photo/1",
        ]);
        let posts = enumerate_posts(&lister, X_PROFILE, 10, &|| false).unwrap();
        assert_eq!(
            posts,
            vec![
                "https://x.com/someuser/status/2",
                "https://x.com/someuser/status/3",
            ]
        );
    }

    #[test]
    fn x_without_photo_tiles_keeps_all_own_status_links_up_to_the_limit() {
        let lister = Fake::with(vec![
            "https://twitter.com/someuser/status/1",
            "https://x.com/someuser/status/2",
            "https://x.com/someuser/status/3",
            "https://x.com/someuser/",
        ]);
        let posts = enumerate_posts(&lister, X_PROFILE, 2, &|| false).unwrap();
        assert_eq!(
            posts,
            vec![
                "https://x.com/someuser/status/1",
                "https://x.com/someuser/status/2",
            ]
        );
    }

    #[test]
    fn listing_urls_and_labels_per_platform() {
        assert_eq!(
            listing_url(ProfileKind::X, "https://twitter.com/SomeUser/with_replies"),
            "https://x.com/someuser/media"
        );
        assert_eq!(
            listing_url(ProfileKind::TikTok, "https://www.tiktok.com/@Some.User"),
            "https://www.tiktok.com/@some.user"
        );
        assert_eq!(listing_url(ProfileKind::Instagram, PROFILE), PROFILE);
        assert_eq!(platform_label(ProfileKind::Instagram), "Instagram");
        assert_eq!(platform_label(ProfileKind::TikTok), "TikTok");
        assert_eq!(platform_label(ProfileKind::X), "X");
    }

    #[test]
    fn logged_out_walls_are_recognised_per_platform() {
        assert!(looks_logged_out(
            ProfileKind::Instagram,
            "Log in Sign up See photos"
        ));
        assert!(looks_logged_out(ProfileKind::TikTok, "Log in to TikTok"));
        assert!(looks_logged_out(
            ProfileKind::X,
            "Don't miss what's happening. Log in. Sign up"
        ));
        assert!(!looks_logged_out(ProfileKind::X, "Posts Replies Media"));
    }

    #[test]
    fn run_queues_one_images_job_per_post_in_order() {
        let lister = Fake::with(vec![
            "https://www.instagram.com/p/AAA/",
            "https://www.instagram.com/p/BBB/",
        ]);
        let mut jobs = Vec::new();
        let out = run_profile_images(&lister, &pro(), &ctx(), PROFILE, None, &|| false, |job| {
            jobs.push(job);
            Ok(())
        })
        .unwrap();
        assert_eq!(out.limit, 50);
        assert_eq!(out.accepted, 2);
        assert_eq!(out.job_ids.len(), 2);
        assert_eq!(jobs[0].input, "https://www.instagram.com/p/AAA/");
        assert_eq!(jobs[1].input, "https://www.instagram.com/p/BBB/");
        for job in &jobs {
            assert_eq!(job.media.unwrap().action, MediaAction::Images);
            assert_eq!(job.kind, Kind::InstagramPost);
            assert_eq!(job.stage, Stage::Queued);
        }
        assert_eq!(jobs[0].id, out.job_ids[0]);
    }

    #[test]
    fn tiktok_and_x_jobs_route_like_pasted_links() {
        let tt = Fake::with(vec!["https://www.tiktok.com/@some.user/photo/9"]);
        let mut jobs = Vec::new();
        run_profile_images(&tt, &pro(), &ctx(), TT_PROFILE, None, &|| false, |j| {
            jobs.push(j);
            Ok(())
        })
        .unwrap();
        assert_eq!(jobs[0].kind, Kind::TikTok);
        assert_eq!(jobs[0].media.unwrap().action, MediaAction::Images);

        let x = Fake::with(vec!["https://x.com/someuser/status/9/photo/1"]);
        let mut jobs = Vec::new();
        run_profile_images(&x, &pro(), &ctx(), X_PROFILE, None, &|| false, |j| {
            jobs.push(j);
            Ok(())
        })
        .unwrap();
        assert_eq!(jobs[0].input, "https://x.com/someuser/status/9");
        assert_eq!(
            jobs[0].kind,
            Kind::from(crate::detect::classify("https://x.com/someuser/status/9"))
        );
    }

    #[test]
    fn run_reports_queue_rejections_without_failing_the_batch() {
        let lister = Fake::with(vec![
            "https://www.instagram.com/p/AAA/",
            "https://www.instagram.com/p/BBB/",
        ]);
        let mut n = 0;
        let out = run_profile_images(
            &lister,
            &pro(),
            &ctx(),
            PROFILE,
            Some(10),
            &|| false,
            |_| {
                n += 1;
                if n == 1 {
                    Err("queue full".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!((out.accepted, out.found), (1, 2));
        assert_eq!(out.rejected, vec!["queue full".to_string()]);
    }

    #[test]
    fn free_tier_is_refused_before_any_listing() {
        let lister = Fake::with(vec!["https://www.instagram.com/p/AAA/"]);
        for url in [PROFILE, TT_PROFILE, X_PROFILE] {
            let err =
                run_profile_images(&lister, &free(), &ctx(), url, None, &|| false, |_| Ok(()))
                    .unwrap_err();
            assert_eq!(err.code, "license_required");
        }
        assert_eq!(*lister.asked.borrow(), None);
    }

    #[test]
    fn non_profile_urls_are_refused() {
        let lister = Fake::with(vec![]);
        for url in [
            "https://www.instagram.com/p/AAA/",
            "https://www.tiktok.com/@some.user/photo/1",
            "https://x.com/someuser/status/1",
            "not a url",
        ] {
            let err = run_profile_images(&lister, &pro(), &ctx(), url, None, &|| false, |_| Ok(()))
                .unwrap_err();
            assert_eq!(err.code, "profile_unsupported", "{url}");
        }
        assert_eq!(*lister.asked.borrow(), None);
    }

    #[test]
    fn needs_auth_and_empty_listings_map_to_distinct_errors() {
        let mut lister = Fake::with(vec![]);
        lister.err = Some(ListErr::NeedsAuth("X".into()));
        let err = run_profile_images(&lister, &pro(), &ctx(), X_PROFILE, None, &|| false, |_| {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err.code, "needs_auth");
        assert_eq!(err.details.unwrap()["platform"], serde_json::json!("X"));

        let empty = Fake::with(vec!["https://www.instagram.com/reel/R/"]);
        let err = run_profile_images(&empty, &pro(), &ctx(), PROFILE, None, &|| false, |_| Ok(()))
            .unwrap_err();
        assert_eq!(err.code, "profile_empty");
    }

    #[test]
    fn response_wire_shape_is_stable() {
        let resp = ProfileImagesResponse {
            profile: PROFILE.into(),
            limit: 50,
            found: 1,
            accepted: 1,
            job_ids: vec!["j".into()],
            rejected: vec![],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["job_ids"][0], "j");
        assert_eq!(v["accepted"], 1);
    }

    #[test]
    fn only_one_listing_runs_at_a_time() {
        let first = ListingGuard::try_begin().expect("first listing starts");
        assert!(ListingGuard::try_begin().is_none());
        drop(first);
        assert!(ListingGuard::try_begin().is_some());
    }
}
