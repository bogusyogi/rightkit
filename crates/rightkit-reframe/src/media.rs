//! ffprobe / ffmpeg boundary: probing, reduced-size raw frame sampling, BLAKE3 of the source.
//!
//! ffmpeg and ffprobe come from the editing bundle (paths are passed in, never discovered
//! from `PATH`).

use crate::error::{ReframeError, Result};
use crate::frame::RgbFrame;
use rightkit_ffmpeg::probe::parse_ffprobe_json;
use rightkit_ffmpeg::runner::{
    run_capture_with, spawn_streaming, RunError, RunOptions, StdioMode, StdioPolicy,
};
use rightkit_ffmpeg::{CancellationToken, FfmpegCommand};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }
}

pub(crate) fn run_error(err: RunError) -> ReframeError {
    match err {
        RunError::Spawn(e) | RunError::Io(e) => ReframeError::Io(e),
        other => ReframeError::Io(io::Error::other(other.to_string())),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    /// Display width/height (rotation applied).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_ms: u64,
}

/// Extract display geometry, frame rate and duration from typed ffprobe JSON.
pub(crate) fn parse_probe(text: &str) -> Result<VideoInfo> {
    let probe = parse_ffprobe_json(text).map_err(|e| ReframeError::Ffprobe(e.to_string()))?;
    let stream = probe
        .streams
        .first()
        .ok_or_else(|| ReframeError::Ffprobe("no video width".into()))?;
    let (mut w, mut h) = (
        stream
            .width
            .ok_or_else(|| ReframeError::Ffprobe("no video width".into()))?,
        stream
            .height
            .ok_or_else(|| ReframeError::Ffprobe("no video height".into()))?,
    );
    if stream.rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    let duration = probe
        .format
        .duration_s
        .ok_or_else(|| ReframeError::Ffprobe("no duration".into()))?;
    Ok(VideoInfo {
        width: w,
        height: h,
        fps: stream
            .avg_frame_rate_fps()
            .or_else(|| stream.r_frame_rate_fps())
            .unwrap_or(30.0),
        duration_ms: (duration * 1000.0).round() as u64,
    })
}

pub fn probe(tools: &Tools, media: &Path) -> Result<VideoInfo> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,avg_frame_rate,r_frame_rate:stream_side_data=rotation:format=duration",
        "-of",
        "json",
    ])
    .path(media);
    // No deadline and no cancel flag today (old `output()` had neither); kill grace zero.
    let opts = RunOptions::unbounded(CancellationToken::new()).with_kill_grace(Duration::ZERO);
    let out = run_capture_with(
        &rightkit_ffmpeg::verify_ffprobe_or_development(
            &tools.ffprobe,
            rightkit_ffmpeg::DECODE_VARIANT,
            Duration::from_secs(30),
        )
        .map_err(|e| ReframeError::Ffprobe(e.to_string()))?,
        &cmd,
        opts,
    )
    .map_err(run_error)?;
    if !out.status.success() {
        return Err(ReframeError::Ffprobe(out.stderr.trim().to_string()));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// Streaming BLAKE3 of a file (the source identity the plan is bound to).
pub fn blake3_file(path: &Path) -> Result<String> {
    rightkit_fs::digest::blake3_file(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            ReframeError::Missing(path.to_path_buf())
        } else {
            ReframeError::Io(e)
        }
    })
}

/// A span of source time to sample, and where it lands on the output timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SampleSpan {
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub output_start_ms: u64,
}

/// One sampled, reduced-size frame.
#[derive(Debug, Clone)]
pub struct SampledFrame {
    pub index: usize,
    pub source_ms: u64,
    pub output_ms: u64,
    /// First sample of a span: a cut the timeline already knows about.
    pub shot_boundary: bool,
    pub frame: RgbFrame,
}

/// Analysis frame size: longest edge at most `max_edge`, aspect preserved, no upscaling.
pub fn analysis_size(width: u32, height: u32, max_edge: u32) -> (usize, usize) {
    let scale = (max_edge as f64 / width.max(height) as f64).min(1.0);
    (
        ((width as f64 * scale).round() as usize).max(2),
        ((height as f64 * scale).round() as usize).max(2),
    )
}

/// Decode every span at `interval_ms` spacing to raw RGB at `(aw, ah)` and hand each frame
/// to `on_frame`. `on_frame` returning an error stops decoding.
pub fn sample_frames(
    tools: &Tools,
    media: &Path,
    spans: &[SampleSpan],
    interval_ms: u64,
    (aw, ah): (usize, usize),
    mut on_frame: impl FnMut(SampledFrame) -> Result<()>,
) -> Result<()> {
    let frame_bytes = aw * ah * 3;
    let mut index = 0usize;
    for span in spans {
        let dur_ms = span.source_end_ms.saturating_sub(span.source_start_ms);
        if dur_ms == 0 {
            continue;
        }
        let expected = dur_ms.div_ceil(interval_ms).max(1) as usize;
        let mut cmd = FfmpegCommand::new();
        cmd.texts(["-v", "error", "-nostdin", "-an", "-sn", "-dn", "-ss"])
            .text(format!("{:.3}", span.source_start_ms as f64 / 1000.0))
            .text("-t")
            .text(format!("{:.3}", dur_ms as f64 / 1000.0))
            .input(media)
            .text("-vf")
            .text(format!(
                "fps=1000/{interval_ms},scale={aw}:{ah}:flags=bilinear"
            ))
            .text("-frames:v")
            .text(expected.to_string())
            .texts(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"]);
        let mut child = spawn_streaming(
            &rightkit_ffmpeg::verify_ffmpeg_or_development(
                &tools.ffmpeg,
                rightkit_ffmpeg::EXPORT_ASS_ZIMG_VARIANT,
                Duration::from_secs(30),
            )
            .or_else(|_| {
                rightkit_ffmpeg::verify_ffmpeg_or_development(
                    &tools.ffmpeg,
                    rightkit_ffmpeg::EXPORT_ASS_VARIANT,
                    Duration::from_secs(30),
                )
            })
            .map_err(|e| ReframeError::Ffmpeg(e.to_string()))?,
            &cmd,
            StdioPolicy {
                stdin: StdioMode::Null,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Piped,
            },
        )
        .map_err(run_error)?;
        let mut stderr = child.take_stderr().expect("stderr piped");
        let drain = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let mut stdout = child.take_stdout().expect("stdout piped");
        let mut k = 0usize;
        let mut result = Ok(());
        loop {
            let mut buf = vec![0u8; frame_bytes];
            match read_full(&mut stdout, &mut buf) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    result = Err(e.into());
                    break;
                }
            }
            let source_ms = span.source_start_ms + k as u64 * interval_ms;
            let sf = SampledFrame {
                index,
                source_ms,
                output_ms: span.output_start_ms + k as u64 * interval_ms,
                shot_boundary: k == 0,
                frame: RgbFrame::new(aw, ah, buf),
            };
            index += 1;
            k += 1;
            if let Err(e) = on_frame(sf) {
                result = Err(e);
                break;
            }
        }
        drop(stdout);
        // Old code: Child::kill() (ignored on error) then wait(). terminate_tree is the
        // immediate forced kill, with no grace, and it reaps the child.
        let status = if result.is_err() {
            match child.terminate_tree() {
                Ok(status) => status,
                Err(_) => child.wait()?,
            }
        } else {
            child.wait()?
        };
        let err_text = drain.join().unwrap_or_default();
        result?;
        if !status.success() {
            return Err(ReframeError::Ffmpeg(err_text.trim().to_string()));
        }
        if k == 0 {
            return Err(ReframeError::Ffmpeg(format!(
                "no frames decoded at {} ms: {}",
                span.source_start_ms,
                err_text.trim()
            )));
        }
    }
    Ok(())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            return Ok(false);
        }
        filled += n;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_parsing_applies_rotation() {
        let info = parse_probe(
            r#"{"streams":[{"width":1920,"height":1080,"r_frame_rate":"30/1",
                "avg_frame_rate":"30000/1001","side_data_list":[{"rotation":-90}]}],
                "format":{"duration":"12.5"}}"#,
        )
        .unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
        assert!((info.fps - 29.97).abs() < 0.01);
        assert_eq!(info.duration_ms, 12_500);
    }

    #[test]
    fn analysis_size_never_upscales() {
        assert_eq!(analysis_size(1920, 1080, 640), (640, 360));
        assert_eq!(analysis_size(320, 240, 640), (320, 240));
        assert_eq!(analysis_size(1080, 1920, 640), (360, 640));
    }
}
