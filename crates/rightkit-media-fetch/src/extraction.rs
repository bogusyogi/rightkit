//! Pipeline support shared by the extraction flows: the typed [`ExtractionError`], error
//! classification, retrying filesystem steps, scratch-folder guards, media staging and publish,
//! ffmpeg probe/trim/remux helpers, and the routing and gating rules for audio, image, carousel
//! and social jobs.
//!
//! The flows themselves (events, settings, ASR engines, history writes) stay with the host; each
//! piece here takes what it needs as a parameter: the public-web policy as [`PublicWeb`], the
//! persist gate as a closure, the output stem as a string.

use std::path::{Path, PathBuf};

use rightkit_web_extract::browser::{BrowserConfig, HostPin};

use crate::carousel::ScrapeErr;
use crate::protocol::{EntitlementsSnapshot, ErrorKind, JobInfo, Kind};
use crate::source_auth::SourcePlatform;
use crate::web::PublicWeb;
use crate::x_post::is_x_post_url;
use crate::{verify_ffmpeg_path, FetchPlan, MediaAction, MediaDownload, MediaSpec};

/// A failed extraction: the message, its UI kind and, for sign-in walls, the platform.
#[derive(Debug, Clone)]
pub struct ExtractionError {
    pub error: String,
    pub error_kind: ErrorKind,
    pub needs_auth: Option<String>,
}

impl ExtractionError {
    pub fn other(error: impl Into<String>) -> Self {
        let error = error.into();
        Self {
            error_kind: classify_error_kind(&error),
            error,
            needs_auth: None,
        }
    }

    pub fn unsupported(kind: Kind) -> Self {
        Self {
            error: format!(
                "sidecar extraction not implemented for {} jobs yet",
                kind.as_str()
            ),
            error_kind: ErrorKind::Unsupported,
            needs_auth: None,
        }
    }

    pub fn no_text_layer(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            error_kind: ErrorKind::NoTextLayer,
            needs_auth: None,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            error: "cancelled".to_string(),
            error_kind: ErrorKind::Cancelled,
            needs_auth: None,
        }
    }

    pub fn needs_auth(platform: impl Into<String>) -> Self {
        let platform = platform.into();
        Self {
            error: format!("{platform} sign-in required"),
            error_kind: ErrorKind::NeedsAuth,
            needs_auth: Some(platform),
        }
    }
}

/// Map a free-form error message to a stable `ErrorKind` for the UI.
pub fn classify_error_kind(msg: &str) -> ErrorKind {
    let m = msg.to_ascii_lowercase();
    if m == "cancelled" {
        ErrorKind::Cancelled
    } else if m.contains("timeout") || m.contains("timed out") {
        ErrorKind::Timeout
    } else if m.contains("rate limit")
        || m.contains("too many requests")
        || m.contains("status 429")
        || m.contains("http error 429")
        // YouTube's bot wall ("Sign in to confirm you're not a bot"): the app
        // has no YouTube sign-in, and the wall lifts by itself, so it is a
        // wait-and-retry condition. Platforms with a sign-in flow set
        // `needs_auth` themselves, which outranks this kind.
        || (m.contains("sign in to confirm") && m.contains("not a bot"))
    {
        ErrorKind::RateLimited
    } else if m.contains("blocked page") || m.contains("access denied") || m.contains("captcha") {
        ErrorKind::Blocked
    } else if m.contains("unsupported") || m.contains("unrecognized") {
        ErrorKind::Unsupported
    } else {
        ErrorKind::Other
    }
}

/// Attempts for a filesystem step that Windows refuses while another process
/// still holds the file open (ffmpeg, a reader, a player). The first attempt
/// succeeds everywhere else, so the retry costs nothing on macOS.
const IO_RETRY_ATTEMPTS: u32 = 5;
const IO_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Run `op`, retrying while `retryable(error)` holds, at most
/// `IO_RETRY_ATTEMPTS` attempts with `IO_RETRY_DELAY` between them (about 0.4 s
/// of sleeping). Returns the first success or the last error.
pub fn retry_io<T>(
    mut op: impl FnMut() -> std::io::Result<T>,
    retryable: impl Fn(&std::io::Error) -> bool,
) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(error) if attempt < IO_RETRY_ATTEMPTS && retryable(&error) => {
                attempt += 1;
                std::thread::sleep(IO_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

/// `remove_dir_all` with a short bounded retry. A directory that is already
/// gone is success; otherwise the last error is returned. Never call while
/// holding a queue lock.
pub fn remove_dir_all_retrying(path: &Path) -> std::io::Result<()> {
    match retry_io(
        || std::fs::remove_dir_all(path),
        |error| error.kind() != std::io::ErrorKind::NotFound,
    ) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Publish `from` at `to`, replacing an existing file (bounded Windows retry). Re-exported from
/// `rightkit-fs`, which holds the one implementation.
pub use rightkit_fs::publish_replacing;

/// Removes a `_work` scratch dir on drop unless `disarm`ed — covers the
/// error/cancel returns that used to leave `<id>`-named dirs behind.
pub struct WorkDirGuard(pub PathBuf);

impl WorkDirGuard {
    pub fn disarm(mut self) {
        self.0 = PathBuf::new();
    }
}

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            if let Err(error) = remove_dir_all_retrying(&self.0) {
                tracing::warn!(
                    target: "rightkit_media_fetch::extraction",
                    path = %self.0.display(),
                    error = %error,
                    "work directory not removed; the next-start sweep retries it"
                );
            }
        }
    }
}

#[derive(Debug)]
pub struct PersistedMedia {
    pub path: PathBuf,
    pub meta: String,
    pub action: String,
}

/// Media prepared for publication: `items` are (current location, final
/// location) pairs; `media` describes the final state recorded in history.
pub struct StagedMedia {
    pub media: PersistedMedia,
    pub items: Vec<(PathBuf, PathBuf)>,
}

pub fn stage_downloaded_media(
    media_dir: &Path,
    source: &Path,
    job: &JobInfo,
    file_stem: &str,
    plan: FetchPlan,
    ffmpeg: &Path,
) -> Result<StagedMedia, ExtractionError> {
    let source_ext = source
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("media")
        .to_ascii_lowercase();
    let destination = media_dir.join(format!("{file_stem}.{source_ext}"));
    let mut items = vec![(source.to_path_buf(), destination.clone())];
    // WS-7: a merged .mkv gets a WebView-playable sibling when its codecs
    // allow — the original stays, media_path points at the playable file. The
    // remux runs beside the source (scratch), not in the output folder.
    let mut recorded = (source.to_path_buf(), destination);
    if let Some(remuxed) = remux_for_webview(ffmpeg, source) {
        let final_remuxed = recorded.1.with_extension(
            remuxed
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        );
        items.push((remuxed.clone(), final_remuxed.clone()));
        recorded = (remuxed, final_remuxed);
    }
    let extension = recorded
        .1
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or(source_ext);
    let bytes = std::fs::metadata(&recorded.0)
        .map_err(|e| ExtractionError::other(format!("inspect downloaded media: {e}")))?
        .len();
    let (kind, quality) = match plan.download {
        MediaDownload::Audio(quality) => ("audio", format!("{quality:?}").to_ascii_lowercase()),
        MediaDownload::Video(quality) => ("video", format!("{quality:?}").to_ascii_lowercase()),
        MediaDownload::None => ("none", "none".to_string()),
    };
    let action = match (job.media.map(|spec| spec.action), kind) {
        (Some(MediaAction::Both), "audio") => "both_audio",
        (Some(MediaAction::Both), "video") => "both_video",
        (_, "audio") => "download_audio",
        (_, "video") => "download_video",
        _ => "transcribe",
    }
    .to_string();
    let meta = serde_json::json!({
        "container": extension,
        "bytes": bytes,
        "kind": kind,
        "quality": quality,
        "stream_copy": kind == "video",
    })
    .to_string();
    Ok(StagedMedia {
        media: PersistedMedia {
            path: recorded.1,
            meta,
            action,
        },
        items,
    })
}

/// Files this run placed in the output folder, with the backup of any file it
/// replaced. Rollback touches only these; nothing else is ever deleted.
pub struct PublishedMedia {
    placed: Vec<(PathBuf, Option<PathBuf>)>,
}

impl PublishedMedia {
    pub fn commit(self) {
        for (_, backup) in self.placed {
            if let Some(backup) = backup {
                let _ = std::fs::remove_file(backup);
            }
        }
    }

    pub fn rollback(self) {
        for (destination, backup) in self.placed.into_iter().rev() {
            let _ = std::fs::remove_file(&destination);
            if let Some(backup) = backup {
                let _ = std::fs::rename(backup, &destination);
            }
        }
    }
}

/// Windows `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`: the file is
/// open in another process. Compared by number so no `cfg` is needed; no other
/// platform produces these codes from a rename.
const WINDOWS_ERROR_SHARING_VIOLATION: i32 = 32;
const WINDOWS_ERROR_LOCK_VIOLATION: i32 = 33;

const EXISTING_FILE_IN_USE: &str =
    "The existing file is open in another app. Close it and try again.";

/// A rename refused because the file is held open elsewhere.
fn is_file_in_use_error(error: &std::io::Error) -> bool {
    // Windows only: elsewhere these raw codes mean other things and a denied
    // rename is a real permission problem, reported as such.
    cfg!(windows)
        && (error.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(
                error.raw_os_error(),
                Some(WINDOWS_ERROR_SHARING_VIOLATION | WINDOWS_ERROR_LOCK_VIOLATION)
            ))
}

fn discard_staged(tmps: &[(PathBuf, PathBuf)]) {
    for (tmp, _) in tmps {
        let _ = std::fs::remove_file(tmp);
    }
}

/// Publish last and atomically per file: copy/move into hidden `.part` names
/// beside the destinations, then gate-check and rename into place, backing up
/// any file being replaced. A closed gate rolls back what this run placed.
/// The gap between the last gate check and each rename is not lockable; it is
/// kept to a single rename, as in the compute-stage writers.
pub fn publish_staged_media(
    staged: &StagedMedia,
    job_id: &str,
    persist_gate_closed: &dyn Fn() -> bool,
) -> Result<PublishedMedia, ExtractionError> {
    let mut tmps: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (source, destination) in &staged.items {
        if persist_gate_closed() {
            discard_staged(&tmps);
            return Err(ExtractionError::cancelled());
        }
        let dir = destination.parent().unwrap_or_else(|| Path::new("."));
        let name = destination
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{name}.{job_id}.part"));
        let staged_ok = std::fs::create_dir_all(dir)
            .map_err(|e| ExtractionError::other(e.to_string()))
            .and_then(
                |()| match rightkit_fs::atomic::rename_exclusive(source, &tmp) {
                    Ok(()) => Ok(()),
                    Err(e) if e.target_replaced() || !source.exists() => Err(
                        ExtractionError::other(format!("save downloaded media: {e}")),
                    ),
                    Err(_) => {
                        rightkit_fs::verified::copy_verified(
                            source,
                            &tmp,
                            rightkit_fs::atomic::AtomicWriteOptions::default(),
                            None,
                        )
                        .map_err(|e| {
                            ExtractionError::other(format!("save downloaded media: {e}"))
                        })?;
                        std::fs::remove_file(source).map_err(|e| {
                            ExtractionError::other(format!("clean downloaded media: {e}"))
                        })
                    }
                },
            );
        tmps.push((tmp, destination.clone()));
        if let Err(err) = staged_ok {
            discard_staged(&tmps);
            return Err(err);
        }
    }
    let mut published = PublishedMedia { placed: Vec::new() };
    for (index, (tmp, destination)) in tmps.iter().enumerate() {
        if persist_gate_closed() {
            discard_staged(&tmps[index..]);
            published.rollback();
            return Err(ExtractionError::cancelled());
        }
        let backup = if destination.exists() {
            let name = destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let backup = destination.with_file_name(format!(".{name}.{job_id}.bak"));
            if let Err(e) = std::fs::rename(destination, &backup) {
                discard_staged(&tmps[index..]);
                published.rollback();
                if is_file_in_use_error(&e) {
                    return Err(ExtractionError::other(EXISTING_FILE_IN_USE));
                }
                return Err(ExtractionError::other(format!(
                    "save downloaded media: {e}"
                )));
            }
            Some(backup)
        } else {
            None
        };
        if let Err(e) = publish_replacing(tmp, destination) {
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, destination);
            }
            discard_staged(&tmps[index..]);
            published.rollback();
            return Err(ExtractionError::other(format!(
                "save downloaded media: {e}"
            )));
        }
        published.placed.push((destination.clone(), backup));
    }
    Ok(published)
}

/// Probe media duration via the shipped ffmpeg: `ffmpeg -i <file>` prints
/// `Duration: HH:MM:SS.xx` to stderr even though it exits non-zero (no output
/// requested). Used for local files, where yt-dlp never runs and the fetch
/// result carries no `duration_s`.
pub fn probe_media_duration_s(ffmpeg: &Path, input: &Path) -> Option<f64> {
    let ffmpeg = verify_ffmpeg_path(ffmpeg).ok()?;
    let probe = rightkit_ffmpeg::probe::probe_media_with(
        &ffmpeg,
        input,
        None,
        &rightkit_ffmpeg::CancellationToken::new(),
    )
    .ok()?;
    probe.duration_ms.map(|ms| ms as f64 / 1000.0)
}

/// Trim a WAV to `seconds` via stream copy — PCM WAV copies are cheap and
/// lossless, so this is far faster than re-decoding through the ASR window
/// path. The trimmed file lands in the job's `_work` dir and is cleaned up
/// with it.
pub fn trim_wav_to_seconds(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    seconds: f64,
) -> Result<(), ExtractionError> {
    let ffmpeg = verify_ffmpeg_path(ffmpeg).map_err(ExtractionError::other)?;
    let mut command = rightkit_ffmpeg::FfmpegCommand::new();
    command.texts(["-y", "-hide_banner", "-loglevel", "error"]);
    command.input(input);
    command.texts(["-t"]);
    command.text(format!("{seconds:.3}"));
    command.texts(["-c", "copy"]);
    command.path(output);
    let captured = rightkit_ffmpeg::runner::run_capture_with(
        &ffmpeg,
        &command,
        rightkit_ffmpeg::RunOptions::unbounded(rightkit_ffmpeg::CancellationToken::new()),
    )
    .map_err(|e| ExtractionError::other(format!("ffmpeg trim failed to start: {e}")))?;
    if !captured.status.success() {
        return Err(ExtractionError::other(format!(
            "ffmpeg trim exited with {}",
            captured.status
        )));
    }
    if !output.is_file() {
        return Err(ExtractionError::other(
            "ffmpeg trim completed but produced no output",
        ));
    }
    Ok(())
}

/// WebView-playable container for a downloaded video: Chromium (Windows
/// WebView2) plays H.264/HEVC+AAC and AV1/VP9+Opus; macOS WKWebView only
/// takes H.264/HEVC+AAC. Anything else keeps the original .mkv — the
/// reader's player then falls back to "Open in default player".
pub fn playable_container(vcodec: &str, acodec: &str) -> Option<&'static str> {
    let v = vcodec.trim().to_ascii_lowercase();
    let a = acodec.trim().to_ascii_lowercase();
    if matches!(v.as_str(), "h264" | "hevc") && a == "aac" {
        return Some("mp4");
    }
    #[cfg(not(target_os = "macos"))]
    {
        if v == "av1" && a == "opus" {
            return Some("mp4");
        }
        if matches!(v.as_str(), "vp9" | "av1") && matches!(a.as_str(), "opus" | "vorbis") {
            return Some("webm");
        }
    }
    None
}

/// First video + audio codec names from the owner's probe. Differs from the old stderr
/// scan: cover-art streams are not counted as video (`first_video` skips them).
fn probe_media_codecs(ffmpeg: &Path, input: &Path) -> Option<(String, String)> {
    let ffmpeg = verify_ffmpeg_path(ffmpeg).ok()?;
    let probe = rightkit_ffmpeg::probe::probe_media_with(
        &ffmpeg,
        input,
        None,
        &rightkit_ffmpeg::CancellationToken::new(),
    )
    .ok()?;
    Some((
        probe.first_video()?.codec.clone(),
        probe.first_audio()?.codec.clone(),
    ))
}

/// Remux a merged .mkv into a WebView-playable container without re-encoding
/// (WS-7): the original stays on disk for "Open in default player" and
/// reveal-in-folder, while `media_path` points at the playable sibling. A
/// `.part`-style partial or any ffmpeg failure returns `None` and the .mkv
/// simply stays the recorded path.
pub fn remux_for_webview(ffmpeg: &Path, source: &Path) -> Option<PathBuf> {
    if source.extension().and_then(|e| e.to_str()) != Some("mkv") {
        return None;
    }
    let (vcodec, acodec) = probe_media_codecs(ffmpeg, source)?;
    let ext = playable_container(&vcodec, &acodec)?;
    let destination = source.with_extension(ext);
    if destination.is_file() {
        return Some(destination);
    }
    let mut command = rightkit_ffmpeg::FfmpegCommand::new();
    command.texts(["-y", "-hide_banner", "-loglevel", "error"]);
    command.input(source);
    command.texts(["-map", "0", "-c", "copy"]);
    if ext == "mp4" {
        // faststart moves the moov box up front so the webview can start
        // streaming before the whole file is mapped.
        command.texts(["-movflags", "+faststart"]);
    }
    command.path(&destination);
    let ffmpeg = verify_ffmpeg_path(ffmpeg).ok()?;
    let captured = rightkit_ffmpeg::runner::run_capture_with(
        &ffmpeg,
        &command,
        rightkit_ffmpeg::RunOptions::unbounded(rightkit_ffmpeg::CancellationToken::new()),
    )
    .ok()?;
    if captured.status.success() && destination.is_file() {
        Some(destination)
    } else {
        let _ = std::fs::remove_file(&destination);
        None
    }
}

/// SSRF guard for the yt-dlp/audio-fetch path. Local file inputs are exempt
/// (never a URL); any http(s) URL handed to yt-dlp is checked against the host's public-host
/// policy, closing the gap where a crafted job could point yt-dlp at a loopback or
/// private-network address.
pub fn guard_remote_audio_input(
    web: &dyn PublicWeb,
    is_local: bool,
    input: &str,
) -> Result<(), String> {
    if is_local {
        return Ok(());
    }
    // Resolve-and-check before launch: yt-dlp does its own DNS and cannot be pinned to the
    // vetted addresses, so the rebinding window remains (see the host's security notes).
    web.ensure_public_and_resolved(input)
}

pub fn should_try_instagram_carousel(audio_error: &str) -> bool {
    let error = audio_error.to_ascii_lowercase();
    [
        "empty media response",
        "no video formats",
        "no media file found",
        "requested format is not available",
        "there's no video",
        "there is no video",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

/// True when a media job failed only because the URL turned out not to carry
/// media, so article extraction is the honest next attempt rather than an
/// error.
///
/// This is what makes `is_video_host()`'s allowlist safe to grow: a host on the
/// list routes to yt-dlp first, and an ordinary page on that host (an artist's
/// merch page, a show index, a profile) falls through to the readable-text path
/// instead of failing the job.
///
/// Deliberately narrow. Auth walls, rate limits, and cancellation must NOT fall
/// back — a login-walled page would yield the login screen's text, which is the
/// "plausible-looking wrong answer" this whole path exists to avoid.
pub fn should_fall_back_to_article(err: &ExtractionError) -> bool {
    if matches!(
        err.error_kind,
        ErrorKind::NeedsAuth | ErrorKind::Cancelled | ErrorKind::RateLimited | ErrorKind::Blocked
    ) || err.needs_auth.is_some()
    {
        return false;
    }
    let error = err.error.to_ascii_lowercase();
    [
        "no video formats",
        "no media file found",
        "requested format is not available",
        "there's no video",
        "there is no video",
        "unsupported url",
        "no video could be found",
        "does not have a video",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

pub fn is_instagram_job(kind: &Kind) -> bool {
    matches!(kind, Kind::InstagramPost | Kind::Reel | Kind::Carousel)
}

pub fn audio_error_for_job(error: impl Into<String>, job: &JobInfo) -> ExtractionError {
    let error = error.into();
    let mut error_kind = classify_error_kind(&error);
    let needs_auth = if is_instagram_job(&job.kind)
        && (matches!(error_kind, ErrorKind::NeedsAuth)
            || is_instagram_auth_recoverable_error(&error))
    {
        error_kind = ErrorKind::NeedsAuth;
        Some("Instagram".to_string())
    } else {
        None
    };
    ExtractionError {
        error,
        error_kind,
        needs_auth,
    }
}

pub fn is_instagram_auth_recoverable_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    // yt-dlp signatures for media Instagram gates behind login (private,
    // age-restricted, or logged-out rate-limit). Signing in and retrying with
    // the profile cookies recovers these, so surface a sign-in prompt instead of
    // the raw yt-dlp error. Reels go straight through yt-dlp with no carousel
    // fallback, so this is the only place their auth failure becomes needs_auth.
    let ytdlp_auth = m.contains("empty media response")
        || m.contains("use --cookies")
        || m.contains("login required")
        || m.contains("requested content is not available")
        || m.contains("rate-limit reached")
        || m.contains("sign in to confirm")
        // `--cookies-from-browser` could not copy the cookie DB because the
        // sign-in Chrome still holds the (Windows-mandatory) file lock. The
        // remedy — close the browser, retry — is the sign-in affordance.
        || m.contains("could not copy chrome cookie database");
    // The headless-browser fallback failing to launch or timing out against an auth wall.
    // Texts from rightkit-browser's `BrowserError` ("launch failed: ...") and the managed
    // wrapper's "browser launch timed out after ...".
    let browser_failure = (m.contains("launch failed")
        || m.contains("browser launch")
        || m.contains("chrome exited")
        || m.contains("devtoolsactiveport")
        || m.contains("current url"))
        && (m.contains("timed out")
            || m.contains("timeout")
            || m.contains("did not return")
            || m.contains("exited early")
            || m.contains("exit code")
            || m.contains("about:blank"));
    ytdlp_auth || browser_failure
}

/// Where an `Images` request goes, decided from the job kind and URL alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagesRoute {
    /// The job did not ask for images: the normal pipeline applies.
    NotImages,
    InstagramCarousel,
    TikTokPost,
    XPost,
    /// A source with no still images to save.
    Unsupported,
}

pub fn images_route(media: Option<MediaSpec>, kind: Kind, input: &str) -> ImagesRoute {
    if !media.is_some_and(|spec| spec.action == MediaAction::Images) {
        return ImagesRoute::NotImages;
    }
    match kind {
        Kind::InstagramPost | Kind::Carousel => ImagesRoute::InstagramCarousel,
        Kind::TikTok => ImagesRoute::TikTokPost,
        Kind::VideoUrl | Kind::Article if is_x_post_url(input) => ImagesRoute::XPost,
        _ => ImagesRoute::Unsupported,
    }
}

pub fn wants_images(job: &JobInfo) -> bool {
    job.media
        .is_some_and(|spec| spec.action == MediaAction::Images)
}

/// Typed "this source has no images to save" error.
pub fn images_unsupported(kind: Kind) -> ExtractionError {
    ExtractionError {
        error: format!(
            "Saving images isn't supported for {} sources; use an Instagram, TikTok photo or X image post",
            kind.as_str()
        ),
        error_kind: ErrorKind::Unsupported,
        needs_auth: None,
    }
}

/// Guard for the media (yt-dlp/ASR) pipelines: an `Images` job must never
/// start a video or audio fetch.
pub fn reject_images_for_media(job: &JobInfo) -> Result<(), ExtractionError> {
    if wants_images(job) {
        Err(images_unsupported(job.kind))
    } else {
        Ok(())
    }
}

/// Removes a per-job slide folder unless the job finished and kept it. A
/// cancel, a failure, or a gated (removed or cancelled) write at any point
/// after the folder exists leaves no folder behind. Used by carousel and
/// TikTok slideshow extraction, which both create `<output>/<name>` for slides.
pub struct SlideDirGuard(Option<PathBuf>);

impl SlideDirGuard {
    pub fn new(dir: PathBuf) -> Self {
        Self(Some(dir))
    }

    /// The job completed and its outputs are kept: the folder stays.
    pub fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for SlideDirGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            if let Err(error) = remove_dir_all_retrying(&dir) {
                tracing::warn!(
                    target: "rightkit_media_fetch::extraction",
                    path = %dir.display(),
                    error = %error,
                    "slide folder not removed"
                );
            }
        }
    }
}

pub fn carousel_text_gate(text: &str) -> Option<ExtractionError> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_ascii_lowercase();
    if lower.contains("sign in to instagram to continue") {
        return Some(ExtractionError::needs_auth("Instagram"));
    }
    if lower.contains("log in")
        && lower.contains("sign up")
        && (lower.contains("post isn't available")
            || lower.contains("link may be broken")
            || lower.contains("content unavailable"))
    {
        return Some(ExtractionError::needs_auth("Instagram"));
    }
    if lower.contains("post isn't available")
        || lower.contains("link may be broken")
        || lower.contains("profile may have been removed")
        || lower.contains("this content isn't available")
        || lower.contains("content unavailable")
    {
        return Some(ExtractionError::other(format!(
            "Instagram content unavailable: {normalized}"
        )));
    }
    None
}

pub fn carousel_err(err: ScrapeErr) -> ExtractionError {
    match err {
        ScrapeErr::NeedsAuth(platform) => ExtractionError::needs_auth(platform),
        ScrapeErr::Other(message) => ExtractionError::other(message),
    }
}
/// Build the terminal auth error for a platform. With `source_auth` (Pro) the
/// job reports `needs_auth` so the UI shows the working Sign In button; on
/// Free the same wall gets a plain upgrade error instead of a Sign In button
/// whose `sign_in` call would just be rejected.
pub fn auth_required_error(
    platform: SourcePlatform,
    entitlements: &EntitlementsSnapshot,
    product: &str,
) -> ExtractionError {
    if entitlements.source_auth {
        ExtractionError {
            error: format!("Sign in to {} in Settings → Accounts", platform.label()),
            error_kind: ErrorKind::NeedsAuth,
            needs_auth: Some(platform.label().to_string()),
        }
    } else {
        ExtractionError {
            error: format!(
                "{} sign-in required, which needs {product} Pro — upgrade in Settings",
                platform.label()
            ),
            error_kind: ErrorKind::Other,
            needs_auth: None,
        }
    }
}

/// `audio_error_for_job`'s social counterpart: auth-recoverable failures map
/// to needs_auth honoring `entitlements.source_auth`; everything else keeps
/// its classified kind unchanged.
pub fn social_error(
    error: impl Into<String>,
    platform: SourcePlatform,
    entitlements: &EntitlementsSnapshot,
    product: &str,
) -> ExtractionError {
    let error = error.into();
    let error_kind = classify_error_kind(&error);
    if matches!(error_kind, ErrorKind::NeedsAuth) || platform.auth_recoverable(&error) {
        return auth_required_error(platform, entitlements, product);
    }
    ExtractionError {
        error,
        error_kind,
        needs_auth: None,
    }
}

/// yt-dlp's `Unsupported URL` / generic-extractor failure on a TikTok link —
/// the signature that means "hand this to the page-JSON slideshow parser".
pub fn should_retry_as_slideshow(err: &ExtractionError) -> bool {
    if matches!(err.error_kind, ErrorKind::NeedsAuth | ErrorKind::Cancelled)
        || err.needs_auth.is_some()
    {
        return false;
    }
    let e = err.error.to_ascii_lowercase();
    e.contains("unsupported url")
        || e.contains("unable to extract")
        || e.contains("no video formats")
}

/// Launch config for the signed-in X profile browser, with the vetted pin (#81).
pub fn x_profile_browser_config(profile_dir: &Path, host_pin: Option<HostPin>) -> BrowserConfig {
    BrowserConfig {
        profile_dir: Some(profile_dir.to_path_buf()),
        host_pin,
        ..Default::default()
    }
}
