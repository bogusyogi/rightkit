//! Typed ffprobe boundary: run ffprobe without a shell and read its JSON through the
//! RightKit media probe into [`ProbeSummary`]. Duration arithmetic stays exact (seconds
//! -> ticks at nanosecond precision).

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use rightkit_ffmpeg as rk;
use rightkit_ffmpeg::probe::{ProbeJson, ProbeStream, Rational};
use rightkit_timeline::{Ticks, VideoStreamInfo};

use crate::error::ProjectError;
use crate::tools::MediaTools;
use crate::worker::{spawn_error_to_io, OwnedChild, StdioPolicy};

/// A probe is bounded so a hung ffprobe cannot stall a render or ingest forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Display rotation of a video stream in degrees, normalised to 0/90/180/270. RightKit
/// reads the display matrix (ffprobe's counter-clockwise convention). A legacy file with
/// only a `rotate` tag (clockwise, so negated) falls back to that tag.
pub(crate) fn stream_rotation(s: &ProbeStream) -> i32 {
    if s.rotation != 0 {
        return s.rotation;
    }
    let degrees = s
        .tags
        .get("rotate")
        .and_then(|t| t.trim().parse::<f64>().ok())
        .map_or(0.0, |r| -r);
    (degrees.round() as i64).rem_euclid(360) as i32
}

/// What registration needs from a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSummary {
    pub duration: Ticks,
    pub video: Option<VideoStreamInfo>,
    /// `(num, den)` of the video stream's frame rate, when ffprobe reports a sane one.
    pub fps: Option<(u32, u32)>,
    pub has_audio: bool,
}

/// Reads ffprobe `-show_format -show_streams` JSON through RightKit's typed view. Invalid
/// JSON is `probe_failed`.
pub(crate) fn read_document(json: &[u8]) -> Result<ProbeJson, ProjectError> {
    let text = String::from_utf8_lossy(json);
    rk::probe::parse_ffprobe_json(&text).map_err(|e| match e {
        rk::probe::ProbeError::Unreadable(why) => {
            ProjectError::ProbeFailed(format!("unreadable ffprobe output: {why}"))
        }
        other => ProjectError::ProbeFailed(format!("unreadable ffprobe output: {other}")),
    })
}

/// Index of the first real video stream (cover art is not video).
pub(crate) fn video_index(typed: &ProbeJson) -> Option<usize> {
    typed
        .streams
        .iter()
        .position(|s| s.is_video() && !s.attached_picture)
}

/// Decimal seconds text (`"12.345"`) -> ticks, exactly, rounding at nanoseconds.
pub fn parse_seconds(text: &str) -> Option<Ticks> {
    let text = text.trim();
    let (int_part, frac_part) = text.split_once('.').unwrap_or((text, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let secs: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac = frac_part.chars().take(9).collect::<String>();
    while frac.len() < 9 {
        frac.push('0');
    }
    let nanos = secs * 1_000_000_000 + frac.parse::<i128>().ok()?;
    let ticks = rightkit_timeline::time::round_div_half_down(
        nanos * rightkit_timeline::TICKS_PER_SECOND as i128,
        1_000_000_000,
    );
    i64::try_from(ticks).ok().map(Ticks)
}

/// Exact nanoseconds -> ticks, rounding half down on the tick grid: the same arithmetic
/// as [`parse_seconds`].
pub(crate) fn ticks_from_ns(nanos: u64) -> Option<Ticks> {
    let ticks = rightkit_timeline::time::round_div_half_down(
        nanos as i128 * rightkit_timeline::TICKS_PER_SECOND as i128,
        1_000_000_000,
    );
    i64::try_from(ticks).ok().map(Ticks)
}

/// An exact rational frame rate as `(num, den)`. RightKit already rejects zero and
/// unknown (`0/0`) rates, so only the `u32` range remains to check.
pub(crate) fn rate_pair(rate: Rational) -> Option<(u32, u32)> {
    Some((u32::try_from(rate.num).ok()?, u32::try_from(rate.den).ok()?))
}

/// The probe summary of an already-read document.
pub(crate) fn summarize(typed: &ProbeJson) -> Result<ProbeSummary, ProjectError> {
    let video_at = video_index(typed);
    let video = video_at.map(|i| &typed.streams[i]);
    let has_audio = typed.first_audio().is_some();
    if video.is_none() && !has_audio {
        return Err(ProjectError::ProbeFailed(
            "no audio or video stream found".into(),
        ));
    }
    let video_info = match video {
        Some(v) => match (v.width, v.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => {
                // The timeline sees the DISPLAY orientation: a 90/270 degree
                // rotation (iPhone portrait) swaps the coded width and height.
                // ffmpeg's default autorotate makes renders honour it.
                let rotation = stream_rotation(v);
                let duration = v.duration_ns.and_then(ticks_from_ns);
                if matches!(rotation, 90 | 270) {
                    Some(VideoStreamInfo {
                        width: height,
                        height: width,
                        duration,
                    })
                } else {
                    Some(VideoStreamInfo {
                        width,
                        height,
                        duration,
                    })
                }
            }
            _ => {
                return Err(ProjectError::ProbeFailed(
                    "video stream has no dimensions".into(),
                ))
            }
        },
        None => None,
    };
    let fps = video.and_then(|v| {
        v.avg_frame_rate
            .and_then(rate_pair)
            .or_else(|| v.r_frame_rate.and_then(rate_pair))
    });
    let duration = typed
        .format
        .duration_ns
        .and_then(ticks_from_ns)
        .or_else(|| {
            typed
                .streams
                .iter()
                .filter(|s| s.is_video() || s.is_audio())
                .filter_map(|s| s.duration_ns.and_then(ticks_from_ns))
                .max()
        })
        .filter(|t| t.0 > 0)
        .ok_or_else(|| ProjectError::ProbeFailed("no usable duration".into()))?;
    Ok(ProbeSummary {
        duration,
        video: video_info,
        fps,
        has_audio,
    })
}

/// Parses ffprobe `-print_format json -show_format -show_streams` output.
pub fn parse_ffprobe(json: &[u8]) -> Result<ProbeSummary, ProjectError> {
    summarize(&read_document(json)?)
}

/// Spawns ffprobe with the given argv (no shell) and returns the owned child. This is the
/// single typed spawn point for ffprobe; the caller chooses the stdio policy, owns the
/// wait, and maps the error. Each argument is kept as a path so non-UTF-8 arguments pass
/// through unchanged.
pub fn spawn_ffprobe<I, S>(
    ffprobe: &Path,
    args: I,
    policy: StdioPolicy,
) -> std::io::Result<OwnedChild>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = rk::FfmpegCommand::new();
    for arg in args {
        cmd.path(Path::new(arg.as_ref()));
    }
    rk::runner::spawn_streaming(
        &rk::verify_ffprobe_or_development(
            ffprobe,
            rk::DECODE_VARIANT,
            crate::tools::VERIFY_TIMEOUT,
        )
        .map_err(std::io::Error::other)?,
        &cmd,
        policy,
    )
    .map_err(spawn_error_to_io)
}

/// Runs ffprobe on `file` through RightKit's typed JSON probe (no shell, bounded) and returns
/// the parsed document. A program that cannot start is `media_tool_missing`; an unreadable
/// file or JSON is `probe_failed`.
pub(crate) fn run_probe_json(tools: &MediaTools, file: &Path) -> Result<ProbeJson, ProjectError> {
    let ffprobe = rk::verify_ffprobe_or_development(
        &tools.ffprobe,
        rk::DECODE_VARIANT,
        crate::tools::VERIFY_TIMEOUT,
    )
    .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))?;
    rk::probe::run_ffprobe_json(
        &ffprobe,
        file,
        Some(PROBE_TIMEOUT),
        &rk::CancellationToken::new(),
    )
    .map_err(|e| match e {
        rk::probe::ProbeError::Run(rk::RunError::Spawn(e) | rk::RunError::Io(e)) => {
            ProjectError::MediaToolMissing(format!(
                "cannot run ffprobe at {}: {e}",
                tools.ffprobe.display()
            ))
        }
        rk::probe::ProbeError::Run(rk::RunError::TimedOut(_)) => ProjectError::ProbeFailed(
            format!("ffprobe timed out after {}s", PROBE_TIMEOUT.as_secs()),
        ),
        other => ProjectError::ProbeFailed(format!("ffprobe: {other}")),
    })
}

/// Runs ffprobe on `file` (no shell) and summarises the result.
pub fn probe_file(tools: &MediaTools, file: &Path) -> Result<ProbeSummary, ProjectError> {
    summarize(&run_probe_json(tools, file)?)
}
