//! `rightkit-media-fetch`: media download and normalize, plus the pure job classifier ([`detect`]).
//!
//! Fetches media from a URL (via yt-dlp) or accepts a local file path, then
//! converts it to a **16 kHz mono PCM s16le WAV** file readable by the
//! downstream ASR engine (e.g. `hound`).
//!
//! # Design constraints
//!
//! - Child processes through `rightkit-child-process` only — no async, no GPU, no Tauri coupling.
//! - All paths passed to native tools use the host-native separator (Windows
//!   backslash) so that `ffmpeg.exe` and yt-dlp receive paths they understand.
//! - yt-dlp is invoked via a configurable command (default: `["yt-dlp"]`)
//!   rather than a hard-coded binary path so that the caller can substitute a
//!   bundled `.exe` without changing this crate.
//!
//! # Quick example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use rightkit_media_fetch::{FetchOpts, FetchPlan, Tools, fetch_media_cancelable};
//!
//! let tools = Tools {
//!     ffmpeg:    PathBuf::from(r"D:\Claude\bin\ffmpeg.exe"),
//!     deno:      None,
//!     ytdlp_cmd: vec!["yt-dlp".into()],
//! };
//!
//! let opts = FetchOpts {
//!     is_local: false,
//!     cookies:  None,
//!     cookies_from_browser: None,
//!     work_dir: PathBuf::from(r"C:\Temp\rightkit_media_work"),
//! };
//!
//! let fetched = fetch_media_cancelable(
//!     "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
//!     &opts,
//!     &tools,
//!     &FetchPlan::transcribe(),
//!     |stage, pct, msg| eprintln!("[{stage} {pct}%] {msg}"),
//!     || false,
//! ).expect("fetch failed");
//!
//! println!("WAV at {:?}", fetched.wav_path);
//! ```

mod args;
pub mod detect;
pub mod playlist;
pub mod process;
mod spec;
mod ytdlp;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub use args::{build_ytdlp_args_for_plan, ytdlp_cmd_from_env_parts};

use rightkit_ffmpeg::{bundled::verify_bundled_ffmpeg, MediaError, VerifiedFfmpeg};
pub use spec::{AudioQuality, MediaAction, MediaSpec, VideoQuality};

use process::run_ffmpeg_cancelable;
use rightkit_child_process::{run_streaming, StreamOptions};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Paths / commands for the external tools this crate shells out to.
#[derive(Debug, Clone)]
pub struct Tools {
    /// Absolute path to `ffmpeg.exe` (Windows) or `ffmpeg` (Unix).
    ///
    /// Example: `PathBuf::from(r"D:\Claude\bin\ffmpeg.exe")`
    pub ffmpeg: PathBuf,
    /// Optional Deno runtime used by yt-dlp's YouTube challenge solver.
    pub deno: Option<PathBuf>,

    /// The command vector used to invoke yt-dlp.  The first element is the
    /// executable and the rest are leading arguments.  The URL and download
    /// flags are appended by [`build_ytdlp_args_for_plan`].
    ///
    /// Default: `["yt-dlp"]`
    /// Bundled binary: `[r"D:\Claude\bin\yt-dlp.exe"]`
    pub ytdlp_cmd: Vec<String>,
}

/// Options controlling how a single fetch job is executed.
#[derive(Debug, Clone)]
pub struct FetchOpts {
    /// If `true`, `input` is treated as a local file path and the yt-dlp
    /// download step is skipped entirely.
    pub is_local: bool,

    /// Optional path to a Netscape-format cookies file.  Forwarded to yt-dlp
    /// via `--cookies`.  Required for private/age-gated Instagram content.
    pub cookies: Option<PathBuf>,

    /// Optional yt-dlp `--cookies-from-browser` spec for a signed-in profile,
    /// e.g. `chrome:C:\...\auth\instagram` (the caller picks the keyword to
    /// match the browser that created the profile). When set it takes precedence
    /// over `cookies`. yt-dlp reads + decrypts the profile's cookie store
    /// natively (DPAPI/Keychain); the profile's browser must not be running
    /// (mandatory file lock on Windows), so the caller closes it first.
    pub cookies_from_browser: Option<String>,

    /// Directory where temporary files (downloaded media, intermediate WAV)
    /// are written.  The caller is responsible for creating and cleaning up
    /// this directory.
    pub work_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDownload {
    None,
    Audio(AudioQuality),
    Video(VideoQuality),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchPlan {
    pub download: MediaDownload,
    pub needs_wav: bool,
}

impl FetchPlan {
    pub fn transcribe() -> Self {
        Self {
            download: MediaDownload::None,
            needs_wav: true,
        }
    }

    pub fn from_media_spec(spec: Option<MediaSpec>) -> Self {
        let Some(spec) = spec else {
            return Self::transcribe();
        };
        let download = match spec.video {
            Some(quality) => MediaDownload::Video(quality),
            None => MediaDownload::Audio(spec.audio.unwrap_or_default()),
        };
        match spec.action {
            MediaAction::Transcribe => Self::transcribe(),
            MediaAction::Download => Self {
                download,
                needs_wav: false,
            },
            MediaAction::Both => Self {
                download,
                needs_wav: true,
            },
            // Image saves never reach the audio pipeline: the engine routes
            // `Images` jobs before any fetch and rejects non-image sources
            // with a typed unsupported error. Fetch nothing here.
            MediaAction::Images => Self {
                download: MediaDownload::None,
                needs_wav: false,
            },
        }
    }

    pub fn format_selector(self) -> String {
        match self.download {
            MediaDownload::None | MediaDownload::Audio(AudioQuality::Best) => {
                "bestaudio/best".into()
            }
            MediaDownload::Audio(AudioQuality::Compact) => {
                "worstaudio[abr>=64]/bestaudio/best".into()
            }
            MediaDownload::Video(VideoQuality::Source) => "bv*+ba/b".into(),
            MediaDownload::Video(quality) => {
                let height = match quality {
                    VideoQuality::P720 => 720,
                    VideoQuality::P1080 => 1080,
                    VideoQuality::P2160 => 2160,
                    VideoQuality::Source => unreachable!(),
                };
                format!("bv*[height<={height}]+ba/b[height<={height}]/b")
            }
        }
    }

    pub fn max_filesize(self) -> &'static str {
        match self.download {
            MediaDownload::Video(VideoQuality::P2160 | VideoQuality::Source) => "8G",
            MediaDownload::Video(_) => "2G",
            _ => "500M",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaFetch {
    pub wav_path: Option<PathBuf>,
    pub media_path: Option<PathBuf>,
    pub title: Option<String>,
    pub duration_s: Option<f64>,
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Fetch media according to `plan`, optionally deriving one normalized WAV.
/// Video plans always request video plus audio; yt-dlp stream-copy merges split
/// sources with the app's bundled LGPL ffmpeg.
///
/// `progress` receives `(stage, percent_0_to_100, message)` with rough,
/// non-monotonic estimates. `should_cancel` is sampled while child processes run;
/// when it returns true the active child is killed and `Err("cancelled")` is returned.
///
/// Local inputs skip yt-dlp and only normalize. URL inputs download with yt-dlp
/// first, then normalize when the plan needs a WAV.
pub fn fetch_media_cancelable(
    input: &str,
    opts: &FetchOpts,
    tools: &Tools,
    plan: &FetchPlan,
    mut progress: impl FnMut(&str, u8, &str),
    should_cancel: impl Fn() -> bool,
) -> Result<MediaFetch, String> {
    let work_dir = if opts.work_dir.is_absolute() {
        opts.work_dir.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("resolve current dir: {e}"))?
            .join(&opts.work_dir)
    };

    // Validate work directory exists.
    if !work_dir.exists() {
        return Err(format!("work_dir does not exist: {}", work_dir.display()));
    }

    let out_wav = work_dir.join("normalized.wav");

    if opts.is_local {
        // -----------------------------------------------------------------
        // LOCAL branch — skip download, just normalize.
        // -----------------------------------------------------------------
        let local_path = Path::new(input);
        if !local_path.exists() {
            return Err(format!("local file not found: {input}"));
        }

        if plan.needs_wav {
            let ffmpeg = verify_ffmpeg_path(&tools.ffmpeg)?;
            progress("ffmpeg", 10, "normalizing local file to 16kHz mono WAV");
            run_ffmpeg_cancelable(&ffmpeg, local_path, &out_wav, &should_cancel)?;
            progress("ffmpeg", 100, "done");
            if !out_wav.exists() {
                return Err(format!(
                    "ffmpeg completed but output WAV not found: {}",
                    out_wav.display()
                ));
            }
        }

        Ok(MediaFetch {
            wav_path: plan.needs_wav.then_some(out_wav),
            media_path: None,
            title: None,
            duration_s: None,
        })
    } else {
        // -----------------------------------------------------------------
        // URL branch — yt-dlp download, then normalize.
        // -----------------------------------------------------------------
        progress("yt-dlp", 5, "starting download");

        // yt-dlp merges and remuxes with ffmpeg, so it gets the same verified
        // binary that the normalize step runs.
        let ffmpeg = verify_ffmpeg_path(&tools.ffmpeg)?;

        let out_tmpl = work_dir
            .join("media.%(ext)s")
            .to_string_lossy()
            .into_owned();

        let ytdlp_args = build_ytdlp_args_for_plan(
            input,
            &out_tmpl,
            opts.cookies.as_deref(),
            opts.cookies_from_browser.as_deref(),
            plan,
            Some(ffmpeg.path()),
            tools.deno.as_deref(),
        );

        progress("download", 10, "Starting download");

        // First try + one retry on transient download failures (5xx, socket
        // timeouts, connection resets, fragment stalls). Auth walls, rate
        // limits and permanent errors are not retried — retrying a 403/429
        // just burns the rate-limit window and hides the real failure.
        const YTDLP_MAX_ATTEMPTS: usize = 2;
        let mut attempt = 0usize;
        let ytdlp_out = loop {
            attempt += 1;

            let mut cmd =
                process::fetch_command(&tools.ytdlp_cmd).map_err(|e| format!("tools.{e}"))?;
            cmd.command_mut().args(&ytdlp_args);

            // Set cwd to work_dir so relative output paths land there.
            cmd.command_mut().current_dir(&work_dir);

            let mut stdout_buffer = String::new();
            let label = format!("yt-dlp ({})", tools.ytdlp_cmd.join(" "));
            let options = StreamOptions::new().stdout_tail_cap(process::FETCH_STDOUT_CAP);
            // The runner calls the stdout and stderr callbacks one at a time on this thread;
            // both report through the same `progress`.
            let shared_progress = std::cell::RefCell::new(&mut progress);
            let out = run_streaming(
                cmd,
                &options,
                &should_cancel,
                &mut |chunk| {
                    report_ytdlp_progress(
                        chunk,
                        &mut stdout_buffer,
                        &mut *shared_progress.borrow_mut(),
                    )
                },
                // The runner has already split the line and dropped its terminator.
                &mut |line| report_ytdlp_line(line, &mut *shared_progress.borrow_mut()),
            )
            .map_err(|e| process::stream_error(&label, &e))?;
            if out.cancelled {
                return Err("cancelled".into());
            }

            // Flush the final unterminated stdout record. Most yt-dlp versions emit a
            // final newline, but this keeps progress reliable across
            // wrappers/builds that terminate their last line directly at process
            // exit. Stderr needs no flush: the runner delivers its last line at end of stream.
            report_ytdlp_progress(b"\n", &mut stdout_buffer, &mut progress);

            if out.status.success() {
                break out;
            }
            let stderr = out.stderr_tail.as_str();
            if attempt < YTDLP_MAX_ATTEMPTS && is_transient_ytdlp_error(stderr) {
                progress("yt-dlp", 5, "download failed transiently; retrying");
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
            return Err(format!(
                "yt-dlp exited with {}: {}",
                out.status,
                stderr_tail(stderr)
            ));
        };

        // Parse stable prefixed values; ordinary yt-dlp logs can appear around
        // them without changing field order.
        let stdout = String::from_utf8_lossy(&ytdlp_out.stdout_tail);
        let (title, duration_s, reported_path) = parse_ytdlp_print_output(&stdout);

        progress("yt-dlp", 60, "download complete; locating media file");

        // Find the downloaded file (yt-dlp chose the extension).
        let media_path = reported_path
            .filter(|path| path.is_file())
            .or_else(|| find_downloaded_media(&work_dir))
            .ok_or_else(|| "yt-dlp succeeded but no media file found in work_dir".to_string())?;

        if plan.needs_wav {
            progress("ffmpeg", 65, "normalizing to 16kHz mono WAV");
            run_ffmpeg_cancelable(&ffmpeg, &media_path, &out_wav, &should_cancel)?;
            progress("ffmpeg", 100, "done");
            if !out_wav.exists() {
                return Err(format!(
                    "ffmpeg completed but output WAV not found: {}",
                    out_wav.display()
                ));
            }
        }

        Ok(MediaFetch {
            wav_path: plan.needs_wav.then_some(out_wav),
            media_path: Some(media_path),
            title,
            duration_s,
        })
    }
}

/// The catalogued FFmpeg build an app bundles: its own LGPL build with dav1d on
/// macOS (`scripts/ffmpeg-runtime.json`), the plain decode build elsewhere.
#[cfg(target_os = "macos")]
pub const FFMPEG_VARIANT: &str = rightkit_ffmpeg::DECODE_AV1_VARIANT;
#[cfg(not(target_os = "macos"))]
pub const FFMPEG_VARIANT: &str = rightkit_ffmpeg::DECODE_VARIANT;

/// Verify the bundled ffmpeg at `path` through rightkit-ffmpeg: the path must be the
/// approved LGPL build for this host (catalog digest), and it must pass the `-version`
/// build probe. Nothing is downloaded and no other path is tried.
pub fn verify_ffmpeg_path(path: &Path) -> Result<VerifiedFfmpeg, String> {
    const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
    verify_bundled_ffmpeg(path, FFMPEG_VARIANT, PROBE_TIMEOUT)
        .map_err(|e| ffmpeg_verification_error(path, e))
}

fn ffmpeg_verification_error(path: &Path, error: MediaError) -> String {
    match error {
        MediaError::ArtifactNotFound => {
            format!("failed to launch ffmpeg ({}): not found", path.display())
        }
        other => format!(
            "ffmpeg ({}) is not the verified LGPL build: {other}",
            path.display()
        ),
    }
}

/// Transient download failures worth one retry. Permanent conditions — 4xx
/// denials, rate limits, auth walls — are excluded deliberately.
fn is_transient_ytdlp_error(stderr: &str) -> bool {
    let e = stderr.to_ascii_lowercase();
    [
        "http error 500",
        "http error 502",
        "http error 503",
        "http error 504",
        "timed out",
        "connection reset",
        "connection aborted",
        "connection refused",
        "temporary failure in name resolution",
        "unable to download fragment",
        "eof occurred in violation",
    ]
    .iter()
    .any(|needle| e.contains(needle))
}

/// Failure text reported to the user: the tail of stderr, capped so a noisy
/// yt-dlp run can't bury the actual error line under progress spam.
pub(crate) fn stderr_tail(stderr: &str) -> String {
    const MAX: usize = 2048;
    if stderr.len() <= MAX {
        return stderr.to_string();
    }
    let mut start = stderr.len() - MAX;
    while !stderr.is_char_boundary(start) {
        start += 1;
    }
    stderr[start..].to_string()
}

/// Parse the stable `--print` markers emitted by yt-dlp stdout.
///
/// We declare title, duration, and final filepath markers. Ordinary logs and
/// progress lines may appear around them, so parsing is marker-based.
///
/// ```text
/// SR_TITLE=Some Video Title
/// SR_DURATION=142.5
/// SR_FILEPATH=C:\\work\\media.mkv
/// ```
///
/// Returns `(title, duration_seconds, filepath)`; any field may be `None` when
/// its marker is missing or malformed.
fn parse_ytdlp_print_output(stdout: &str) -> (Option<String>, Option<f64>, Option<PathBuf>) {
    let mut title = None;
    let mut duration_s = None;
    let mut filepath = None;
    for line in stdout.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("SR_TITLE=") {
            title = (!value.is_empty()).then(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("SR_DURATION=") {
            duration_s = value.parse::<f64>().ok();
        } else if let Some(value) = line.strip_prefix("SR_FILEPATH=") {
            filepath = (!value.is_empty()).then(|| PathBuf::from(value));
        }
    }
    (title, duration_s, filepath)
}

#[derive(Debug, Clone, PartialEq)]
struct YtdlpProgress {
    percent: u8,
    message: String,
}

/// Parse one yt-dlp `[download]` status line. yt-dlp writes these lines to
/// stderr. Callers pass one complete line at a time, without its terminator.
fn parse_ytdlp_progress_line(line: &str) -> Option<YtdlpProgress> {
    let line = line.trim();
    let marker = line.find("[download]")?;
    let rest = line[marker + "[download]".len()..].trim_start();
    let percent_end = rest.find('%')?;
    let value = rest[..percent_end].trim().parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let percent = value.min(100.0).floor() as u8;
    let message = format!("download {value:.1}%{}", &rest[percent_end + 1..]);
    Some(YtdlpProgress { percent, message })
}

/// Feed raw stdout chunks into the line parser. Both newline and carriage-return
/// delimiters are accepted because yt-dlp uses carriage returns for in-place progress
/// on some terminals. Stderr arrives already split into lines; see `report_ytdlp_line`.
fn report_ytdlp_progress(
    chunk: &[u8],
    buffer: &mut String,
    progress: &mut impl FnMut(&str, u8, &str),
) {
    buffer.push_str(&String::from_utf8_lossy(chunk));
    // A line that never ends must not grow the buffer without bound: keep only its newest
    // bytes once it passes the cap.
    const MAX_PENDING_LINE: usize = 4096;
    if buffer.len() > MAX_PENDING_LINE {
        let mut start = buffer.len() - MAX_PENDING_LINE;
        while !buffer.is_char_boundary(start) {
            start += 1;
        }
        buffer.drain(..start);
    }
    while let Some(delimiter) = buffer.find(['\r', '\n']) {
        let line = buffer[..delimiter].to_string();
        let delimiter_len = if buffer[delimiter..].starts_with("\r\n") {
            2
        } else {
            1
        };
        buffer.drain(..delimiter + delimiter_len);
        report_ytdlp_line(&line, progress);
    }
}

/// Report one complete yt-dlp output line when it is a `[download]` status line.
fn report_ytdlp_line(line: &str, progress: &mut impl FnMut(&str, u8, &str)) {
    if let Some(parsed) = parse_ytdlp_progress_line(line) {
        // 10..58 is reserved for the downloader; 60 means yt-dlp has
        // exited and the caller is locating its final media path.
        let stage_pct = 10 + (parsed.percent as u16 * 48 / 100) as u8;
        progress("download", stage_pct, &parsed.message);
    }
}

/// Scan `work_dir` for the media file downloaded by yt-dlp.
///
/// yt-dlp uses the output template `media.<ext>` so we look for any file
/// whose stem is `media` and whose extension is a known media format.
/// If no exact match is found we fall back to the first non-WAV file in
/// the directory (in case the extension is unusual).
fn find_downloaded_media(work_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(work_dir).ok()?;
    let mut fallback: Option<PathBuf> = None;

    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        // Skip the output WAV itself.
        if p.file_name().and_then(|n| n.to_str()) == Some("normalized.wav") {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if stem == "media" && ext != "wav" && crate::detect::is_media_ext(&ext) {
            return Some(p);
        }
        if ext != "wav" && crate::detect::is_media_ext(&ext) {
            fallback = Some(p);
        }
    }

    fallback
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
