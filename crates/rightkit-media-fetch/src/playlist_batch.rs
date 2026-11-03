//! #251: playlist and channel downloads as jobs.
//!
//! A YouTube (or other yt-dlp-supported) playlist or channel URL is never one
//! job. The engine enumerates it with `yt-dlp --flat-playlist`, bounded by a
//! limit (default 50, max 500), and queues one child job per entry carrying
//! the parent's `MediaSpec`, through the ordinary queue submit path. Each
//! child is a single video, so the per-video download still runs
//! `--no-playlist`.
//!
//! The listing sits behind [`PlaylistLister`] so limit, de-dup and safety
//! filtering are unit-tested without yt-dlp or a network (same shape as
//! `profile_batch`).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::detect::{classify, classify_playlist, playlist_listing_url, JobKind};
use crate::protocol::{
    validate_media_spec, EngineError, EntitlementsSnapshot, JobInfo, Kind, PlaylistResponse, Stage,
    PLAYLIST_DEFAULT_LIMIT, PLAYLIST_MAX_LIMIT,
};
use crate::MediaSpec;

/// What a batch run needs from its host besides the lister and the queue.
pub struct BatchContext<'a> {
    /// The product's display name, used in "needs `<product>` Pro" messages.
    pub product: &'a str,
    /// Mints the id of one queued child job.
    pub new_id: &'a dyn Fn() -> String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlaylistErr {
    /// yt-dlp could not list the playlist (message for the user).
    Failed(String),
    /// The user cancelled the listing.
    Cancelled,
}

/// Source of raw entry URLs for a playlist, in playlist order.
pub trait PlaylistLister {
    /// Return entry links of `listing_url`, at most `limit`. May include
    /// links that are not videos; [`enumerate_entries`] filters.
    fn list(
        &self,
        listing_url: &str,
        limit: usize,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, PlaylistErr>;
}

/// Resolve a requested limit: absent or zero means the default, and the
/// result never exceeds [`PLAYLIST_MAX_LIMIT`].
pub fn resolve_limit(requested: Option<u32>) -> u32 {
    match requested {
        None | Some(0) => PLAYLIST_DEFAULT_LIMIT,
        Some(n) => n.min(PLAYLIST_MAX_LIMIT),
    }
}

/// Whether `input` is a playlist or channel URL the engine can expand.
pub fn is_playlist_url(input: &str) -> bool {
    classify_playlist(input).is_some()
}

/// List `playlist_url`, keep entries that are public video links, drop
/// duplicates (first sighting wins, so playlist order is kept) and cut to
/// `limit`.
pub fn enumerate_entries(
    lister: &dyn PlaylistLister,
    playlist_url: &str,
    limit: u32,
    should_cancel: &dyn Fn() -> bool,
) -> Result<Vec<String>, PlaylistErr> {
    if classify_playlist(playlist_url).is_none() {
        return Err(PlaylistErr::Failed(
            "that link is not a playlist or channel".into(),
        ));
    }
    let limit = limit as usize;
    let listing = playlist_listing_url(playlist_url);
    let raw = lister.list(&listing, limit, should_cancel)?;
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for link in raw {
        let link = link.trim().to_string();
        // Entry links come from a tool's output: only public video hosts pass
        // (this also drops local paths and private-network addresses).
        if !matches!(
            classify(&link),
            JobKind::YouTube | JobKind::VideoUrl | JobKind::MediaUrl
        ) {
            continue;
        }
        if seen.insert(link.clone()) {
            entries.push(link);
            if entries.len() == limit {
                break;
            }
        }
    }
    Ok(entries)
}

/// The child job for one entry, carrying the parent's media spec.
pub fn entry_job(entry_url: &str, id: String, media: Option<MediaSpec>) -> JobInfo {
    JobInfo {
        id,
        input: entry_url.to_string(),
        kind: Kind::from(classify(entry_url)),
        stage: Stage::Queued,
        pct: 0,
        title: None,
        error: None,
        error_kind: None,
        needs_auth: None,
        media,
        attach_to: None,
        prepared: None,
    }
}

/// List a playlist and queue one child job per entry through `submit` (the
/// queue's public submit). Pure orchestration: the lister and `submit` are
/// injected.
#[allow(clippy::too_many_arguments)]
pub fn run_playlist_batch(
    lister: &dyn PlaylistLister,
    entitlements: &EntitlementsSnapshot,
    ctx: &BatchContext<'_>,
    url: &str,
    limit: Option<u32>,
    media: Option<MediaSpec>,
    should_cancel: &dyn Fn() -> bool,
    mut submit: impl FnMut(JobInfo) -> Result<(), String>,
) -> Result<PlaylistResponse, EngineError> {
    if classify_playlist(url).is_none() {
        return Err(EngineError::new(
            "playlist_unsupported",
            "that link is not a playlist or channel",
        ));
    }
    // Defense in depth: the shell gates this, a crafted request must not skip it.
    validate_media_spec(media.as_ref(), entitlements, ctx.product)
        .map_err(|message| EngineError::new("license_required", message))?;
    let limit = resolve_limit(limit);
    let entries =
        enumerate_entries(lister, url.trim(), limit, should_cancel).map_err(|err| match err {
            PlaylistErr::Failed(message) => EngineError::new("playlist_list", message),
            PlaylistErr::Cancelled => EngineError::new("playlist_list", "listing was cancelled"),
        })?;
    if entries.is_empty() {
        return Err(EngineError::new(
            "playlist_empty",
            "no videos were found in that playlist",
        ));
    }
    let found = entries.len() as u32;
    let mut job_ids = Vec::new();
    let mut rejected = Vec::new();
    for entry in &entries {
        let id = (ctx.new_id)();
        match submit(entry_job(entry, id.clone(), media)) {
            Ok(()) => job_ids.push(id),
            Err(err) => rejected.push(err),
        }
    }
    Ok(PlaylistResponse {
        playlist: url.trim().to_string(),
        limit,
        found,
        accepted: job_ids.len() as u32,
        job_ids,
        rejected,
    })
}

/// One enumeration at a time: each runs a yt-dlp process.
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

/// Lists a playlist with the installed yt-dlp.
pub struct YtDlpLister {
    ytdlp_cmd: Vec<String>,
}

impl YtDlpLister {
    /// A lister that runs `ytdlp_cmd` (the yt-dlp command and its leading arguments).
    pub fn with_command(ytdlp_cmd: Vec<String>) -> Self {
        Self { ytdlp_cmd }
    }
}

impl PlaylistLister for YtDlpLister {
    fn list(
        &self,
        listing_url: &str,
        limit: usize,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, PlaylistErr> {
        let limit = u32::try_from(limit).unwrap_or(PLAYLIST_MAX_LIMIT);
        crate::playlist::list_playlist_entries(&self.ytdlp_cmd, listing_url, limit, &|| {
            should_cancel()
        })
        .map_err(|message| {
            if should_cancel() {
                PlaylistErr::Cancelled
            } else {
                PlaylistErr::Failed(message)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{entitlements_snapshot_for_tier, LicenseTier};
    use crate::{AudioQuality, MediaAction};
    use std::cell::{Cell, RefCell};

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

    struct Fake {
        links: Vec<String>,
        asked: Cell<usize>,
        asked_url: RefCell<String>,
        fail: Option<PlaylistErr>,
    }

    impl Fake {
        fn new(links: &[&str]) -> Self {
            Self {
                links: links.iter().map(|s| s.to_string()).collect(),
                asked: Cell::new(0),
                asked_url: RefCell::new(String::new()),
                fail: None,
            }
        }
    }

    impl PlaylistLister for Fake {
        fn list(
            &self,
            listing_url: &str,
            limit: usize,
            _: &dyn Fn() -> bool,
        ) -> Result<Vec<String>, PlaylistErr> {
            self.asked.set(limit);
            *self.asked_url.borrow_mut() = listing_url.to_string();
            match &self.fail {
                Some(PlaylistErr::Cancelled) => Err(PlaylistErr::Cancelled),
                Some(PlaylistErr::Failed(m)) => Err(PlaylistErr::Failed(m.clone())),
                None => Ok(self.links.clone()),
            }
        }
    }

    fn watch(id: &str) -> String {
        format!("https://www.youtube.com/watch?v={id}")
    }

    const PLAYLIST: &str = "https://www.youtube.com/playlist?list=PL1";

    fn entitlements() -> EntitlementsSnapshot {
        entitlements_snapshot_for_tier(LicenseTier::Free)
    }

    #[test]
    fn limit_defaults_clamps_and_treats_zero_as_default() {
        assert_eq!(resolve_limit(None), 50);
        assert_eq!(resolve_limit(Some(0)), 50);
        assert_eq!(resolve_limit(Some(7)), 7);
        assert_eq!(resolve_limit(Some(500)), 500);
        assert_eq!(resolve_limit(Some(501)), 500);
        assert_eq!(resolve_limit(Some(u32::MAX)), 500);
    }

    #[test]
    fn entries_are_deduped_filtered_and_capped_in_order() {
        let a = watch("a");
        let b = watch("b");
        let c = watch("c");
        let lister = Fake::new(&[
            &a,
            "file:///etc/passwd",
            "http://127.0.0.1/x",
            "https://example.com/some-article",
            &a,
            &b,
            &c,
        ]);
        let out = enumerate_entries(&lister, PLAYLIST, 2, &|| false).unwrap();
        assert_eq!(out, vec![a.clone(), b.clone()]);
        assert_eq!(lister.asked.get(), 2);
        let all = enumerate_entries(&lister, PLAYLIST, 10, &|| false).unwrap();
        assert_eq!(all, vec![a, b, c]);
    }

    #[test]
    fn a_bare_channel_is_listed_through_its_videos_tab() {
        let lister = Fake::new(&[&watch("a")]);
        enumerate_entries(&lister, "https://www.youtube.com/@someone", 5, &|| false).unwrap();
        assert_eq!(
            lister.asked_url.borrow().as_str(),
            "https://www.youtube.com/@someone/videos"
        );
    }

    #[test]
    fn a_single_video_link_is_not_enumerated() {
        let lister = Fake::new(&[&watch("a")]);
        let err = run_playlist_batch(
            &lister,
            &entitlements(),
            &ctx(),
            "https://www.youtube.com/watch?v=abc&list=PL1",
            None,
            None,
            &|| false,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, "playlist_unsupported");
        assert_eq!(lister.asked.get(), 0);
    }

    #[test]
    fn every_entry_becomes_a_child_job_with_the_parent_media_spec() {
        let lister = Fake::new(&[&watch("a"), &watch("b"), &watch("c")]);
        let spec = MediaSpec {
            action: MediaAction::Download,
            video: None,
            audio: Some(AudioQuality::Best),
        };
        let mut jobs = Vec::new();
        let done = run_playlist_batch(
            &lister,
            &entitlements(),
            &ctx(),
            &format!("  {PLAYLIST}  "),
            Some(2),
            Some(spec),
            &|| false,
            |job| {
                jobs.push(job);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(done.limit, 2);
        assert_eq!(done.found, 2);
        assert_eq!(done.accepted, 2);
        assert_eq!(done.playlist, PLAYLIST);
        assert!(done.rejected.is_empty());
        let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
        assert_eq!(done.job_ids, ids);
        assert_eq!(jobs[0].input, watch("a"));
        assert_eq!(jobs[1].input, watch("b"));
        for job in &jobs {
            assert_eq!(job.media, Some(spec));
            assert_eq!(job.stage, Stage::Queued);
            assert_eq!(job.kind, Kind::from(JobKind::YouTube));
        }
    }

    #[test]
    fn queue_rejections_are_reported_without_aborting_the_batch() {
        let lister = Fake::new(&[&watch("a"), &watch("b")]);
        let mut calls = 0;
        let done = run_playlist_batch(
            &lister,
            &entitlements(),
            &ctx(),
            PLAYLIST,
            None,
            None,
            &|| false,
            |_| {
                calls += 1;
                if calls == 1 {
                    Err("queue full".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!((done.found, done.accepted), (2, 1));
        assert_eq!(done.rejected, vec!["queue full".to_string()]);
    }

    #[test]
    fn typed_errors_for_empty_failed_and_cancelled_listings() {
        let empty = Fake::new(&[]);
        let err = run_playlist_batch(
            &empty,
            &entitlements(),
            &ctx(),
            PLAYLIST,
            None,
            None,
            &|| false,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, "playlist_empty");

        let mut failing = Fake::new(&[]);
        failing.fail = Some(PlaylistErr::Failed("HTTP 404".into()));
        let err = run_playlist_batch(
            &failing,
            &entitlements(),
            &ctx(),
            PLAYLIST,
            None,
            None,
            &|| false,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, "playlist_list");
        assert!(err.message.contains("404"), "{}", err.message);

        let mut cancelled = Fake::new(&[]);
        cancelled.fail = Some(PlaylistErr::Cancelled);
        let err = run_playlist_batch(
            &cancelled,
            &entitlements(),
            &ctx(),
            PLAYLIST,
            None,
            None,
            &|| false,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, "playlist_list");
    }

    #[test]
    fn a_pro_only_media_spec_is_refused_before_any_listing() {
        let lister = Fake::new(&[&watch("a")]);
        let both = MediaSpec {
            action: MediaAction::Both,
            video: None,
            audio: None,
        };
        let err = run_playlist_batch(
            &lister,
            &entitlements(),
            &ctx(),
            PLAYLIST,
            None,
            Some(both),
            &|| false,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, "license_required");
        assert_eq!(lister.asked.get(), 0);
    }

    #[test]
    fn only_one_listing_runs_at_a_time() {
        let first = ListingGuard::try_begin().expect("first listing starts");
        assert!(ListingGuard::try_begin().is_none());
        drop(first);
        assert!(ListingGuard::try_begin().is_some());
    }
}
