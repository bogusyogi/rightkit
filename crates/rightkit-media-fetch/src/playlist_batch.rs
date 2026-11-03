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
