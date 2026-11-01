//! Job, queue and batch wire types the extraction modules exchange with the host's queue.
//!
//! The serde shapes are the host's wire contract: field names, tags and defaults do not change.

use serde::{Deserialize, Serialize};

use crate::detect::JobKind;
use crate::{MediaAction, MediaSpec, VideoQuality};

/// What a job was classified as. The snake_case tag is what a library store persists.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Reel,
    InstagramPost,
    Carousel,
    #[serde(rename = "youtube")]
    YouTube,
    #[serde(rename = "tiktok")]
    TikTok,
    Threads,
    Facebook,
    VideoUrl,
    MediaUrl,
    Article,
    LocalFile,
    Unknown,
}

impl Kind {
    /// snake_case tag stored in the history DB (matches the serde rename).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Reel => "reel",
            Kind::InstagramPost => "instagram_post",
            Kind::Carousel => "carousel",
            Kind::YouTube => "youtube",
            Kind::TikTok => "tiktok",
            Kind::Threads => "threads",
            Kind::Facebook => "facebook",
            Kind::VideoUrl => "video_url",
            Kind::MediaUrl => "media_url",
            Kind::Article => "article",
            Kind::LocalFile => "local_file",
            Kind::Unknown => "unknown",
        }
    }

    pub fn from_tag(s: &str) -> Kind {
        match s {
            "reel" => Kind::Reel,
            "instagram_post" => Kind::InstagramPost,
            "carousel" => Kind::Carousel,
            "youtube" => Kind::YouTube,
            "tiktok" => Kind::TikTok,
            "threads" => Kind::Threads,
            "facebook" => Kind::Facebook,
            "video_url" => Kind::VideoUrl,
            "media_url" => Kind::MediaUrl,
            "article" => Kind::Article,
            "local_file" => Kind::LocalFile,
            _ => Kind::Unknown,
        }
    }
}

impl From<JobKind> for Kind {
    fn from(k: JobKind) -> Self {
        match k {
            JobKind::InstagramReel => Kind::Reel,
            JobKind::InstagramPostMaybe => Kind::InstagramPost,
            JobKind::YouTube => Kind::YouTube,
            JobKind::TikTok => Kind::TikTok,
            JobKind::Threads => Kind::Threads,
            JobKind::Facebook => Kind::Facebook,
            JobKind::VideoUrl => Kind::VideoUrl,
            JobKind::MediaUrl => Kind::MediaUrl,
            JobKind::Article => Kind::Article,
            JobKind::LocalFile => Kind::LocalFile,
            JobKind::Unknown => Kind::Unknown,
        }
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Queued,
    Detecting,
    Downloading,
    /// WS-3: the fetch pool finished the network stage and the job waits for
    /// the single compute worker. `JobInfo.prepared` carries the resumable
    /// artifact record.
    Fetched,
    Transcribing,
    Writing,
    Done,
    Error,
    Cancelled,
}

/// Why a job failed, as the client groups it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NeedsAuth,
    Blocked,
    RateLimited,
    NoMedia,
    NoTextLayer,
    Unsupported,
    Timeout,
    Cancelled,
    Other,
}

/// Fetch-stage output that lets a `Fetched` job resume at the compute worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreparedArtifact {
    /// Compute input — the normalized wav a transcribe job transcribes.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    /// Owning scratch dir (`<output>/_work/<id>`); absolute so a later
    /// output-dir change still finds it for resume and cleanup.
    pub work_dir: String,
    /// User-facing media already persisted into the downloads dir, when the
    /// job also downloaded media (download-only jobs never reach Prepared).
    #[serde(default)]
    pub media_path: Option<String>,
    #[serde(default)]
    pub media_meta: Option<String>,
    #[serde(default)]
    pub media_action: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub duration_s: Option<f64>,
}

/// One queued job as the client sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub input: String,
    pub kind: Kind,
    pub stage: Stage,
    pub pct: u8,
    pub title: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub error_kind: Option<ErrorKind>,
    #[serde(default)]
    pub needs_auth: Option<String>,
    /// User-requested output. Absent always means today's transcribe-only path.
    #[serde(default)]
    pub media: Option<MediaSpec>,
    /// WS-6: when set, this job is a transcription *attempt* whose result
    /// writes into the existing history row named here (the stable library
    /// item id), instead of creating a new row keyed on the attempt's own
    /// `id`. Only valid for `Kind::LocalFile` media-transcription jobs.
    pub attach_to: Option<String>,
    /// WS-3: fetch-stage output that lets a `Stage::Fetched` job resume at
    /// the compute worker after a restart. Present only between the fetch
    /// pool completing the network stage and the compute worker consuming
    /// it.
    pub prepared: Option<PreparedArtifact>,
}

/// The platform sign-in the host last pushed (one platform at a time).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthStateSnapshot {
    pub platform: Option<String>,
    pub profile_dir: Option<String>,
    #[serde(default)]
    pub profile_present: bool,
    pub exported_cookies_path: Option<String>,
    #[serde(default)]
    pub exported_cookies_present: bool,
}

/// Free or paid licence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseTier {
    Free,
    Pro,
}

/// What the current licence entitles the user to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementsSnapshot {
    pub tier: LicenseTier,
    pub scanned_pdf_ocr: bool,
    /// Signed-in sources: Instagram, Threads, X, TikTok.
    #[serde(default, alias = "instagram_auth")]
    pub source_auth: bool,
    pub pro_enrichment: bool,
    /// Longest transcript in seconds; `None` is unlimited.
    #[serde(default)]
    pub max_transcript_seconds: Option<u32>,
    #[serde(default)]
    pub premium_video_quality: bool,
    #[serde(default)]
    pub combined_media: bool,
    #[serde(default = "default_concurrent_downloads")]
    pub concurrent_downloads: u8,
    #[serde(default)]
    pub timed_formats: bool,
}

fn default_concurrent_downloads() -> u8 {
    FREE_CONCURRENT_DOWNLOADS
}

pub const FREE_MAX_TRANSCRIPT_SECONDS: u32 = 1800;
pub const FREE_CONCURRENT_DOWNLOADS: u8 = 1;
pub const PRO_CONCURRENT_DOWNLOADS: u8 = 3;

pub fn entitlements_snapshot_for_tier(tier: LicenseTier) -> EntitlementsSnapshot {
    let paid = !matches!(tier, LicenseTier::Free);
    EntitlementsSnapshot {
        tier,
        scanned_pdf_ocr: paid,
        source_auth: paid,
        pro_enrichment: paid,
        max_transcript_seconds: if paid {
            None
        } else {
            Some(FREE_MAX_TRANSCRIPT_SECONDS)
        },
        premium_video_quality: paid,
        combined_media: paid,
        concurrent_downloads: if paid {
            PRO_CONCURRENT_DOWNLOADS
        } else {
            FREE_CONCURRENT_DOWNLOADS
        },
        timed_formats: paid,
    }
}

/// Shared Free/Pro gate for a job's media request, applied at submit time and re-checked by the
/// engine so headless callers cannot bypass it. The error message names `product` and its Pro
/// tier.
pub fn validate_media_spec(
    media: Option<&MediaSpec>,
    entitlements: &EntitlementsSnapshot,
    product: &str,
) -> Result<(), String> {
    let Some(spec) = media else {
        return Ok(());
    };
    match spec.action {
        MediaAction::Both if !entitlements.combined_media => {
            return Err(format!(
                "Transcript + download together needs {product} Pro. Upgrade in Settings."
            ));
        }
        // Image saves are a Free feature; signed-in sources stay Pro through the per-platform
        // `source_auth` gate in the engine, not here.
        MediaAction::Both
        | MediaAction::Transcribe
        | MediaAction::Download
        | MediaAction::Images => {}
    }
    if matches!(spec.video, Some(VideoQuality::P2160 | VideoQuality::Source))
        && !entitlements.premium_video_quality
    {
        return Err(format!(
            "4K and source-quality downloads need {product} Pro. Upgrade in Settings."
        ));
    }
    Ok(())
}

/// `HistoryRow::media_action` tag of a job that saved a post's images (#248).
pub const MEDIA_ACTION_DOWNLOAD_IMAGES: &str = "download_images";

/// Image outputs of an `images` job, carried as JSON in
/// `HistoryRow::media_meta` (with `media_action == MEDIA_ACTION_DOWNLOAD_IMAGES`,
/// `slide_dir` = the folder and `media_path` = the first file).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageDownloadMeta {
    /// Always `"images"`.
    pub kind: String,
    pub count: u32,
    /// Total bytes across all files.
    pub bytes: u64,
    /// Absolute paths of the saved image files, in post order.
    pub files: Vec<String>,
    /// 0-based indexes into `files` that are browser screenshots rather than
    /// the original image (no image URL resolved for that slide).
    pub fallback: Vec<u32>,
}

impl ImageDownloadMeta {
    /// The `media_meta` of a history row when it is an images job; `None` for
    /// any other row.
    pub fn from_row_meta(media_action: Option<&str>, media_meta: Option<&str>) -> Option<Self> {
        if media_action != Some(MEDIA_ACTION_DOWNLOAD_IMAGES) {
            return None;
        }
        serde_json::from_str(media_meta?).ok()
    }
}

/// A structured engine failure: a stable `code`, a message and optional details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

impl EngineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Default and maximum posts one profile batch queues.
pub const PROFILE_DEFAULT_LIMIT: u32 = 50;
pub const PROFILE_MAX_LIMIT: u32 = 200;
/// Default and maximum entries one playlist batch queues.
pub const PLAYLIST_DEFAULT_LIMIT: u32 = 50;
pub const PLAYLIST_MAX_LIMIT: u32 = 500;

/// Result of a playlist batch: counts and the queued child job ids.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaylistResponse {
    /// The playlist or channel URL that was enumerated, as normalised.
    pub playlist: String,
    /// The limit actually applied (default and ceiling resolved).
    pub limit: u32,
    /// Entries found within the limit.
    pub found: u32,
    /// Jobs accepted by the queue; equals `job_ids.len()`.
    pub accepted: u32,
    /// Ids of the queued child jobs, in playlist order.
    pub job_ids: Vec<String>,
    /// Per-entry queue rejections (message only); the batch is not atomic.
    #[serde(default)]
    pub rejected: Vec<String>,
}

/// Result of a profile images batch: counts and the queued per-post job ids.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileImagesResponse {
    /// The profile that was listed, as normalised by the engine.
    pub profile: String,
    /// The limit actually applied (default and ceiling resolved).
    pub limit: u32,
    /// Posts found, before the limit was applied.
    pub found: u32,
    /// Jobs accepted by the queue; equals `job_ids.len()`.
    pub accepted: u32,
    /// Ids of the queued per-post jobs, newest post first. They then flow as
    /// ordinary `JobUpdate` events and appear in `GetSnapshot`.
    pub job_ids: Vec<String>,
    /// Per-post queue rejections (message only); the batch is not atomic.
    #[serde(default)]
    pub rejected: Vec<String>,
}
