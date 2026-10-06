//! Typed media probe through the verified FFmpeg binary.
//!
//! The artifact catalog ships `ffmpeg` only (no `ffprobe`), so the probe runs
//! `ffmpeg -i <file>` and parses the stream summary FFmpeg prints on stderr. Donor
//! behavior reconciled from CutRight `cutright-project/src/probe.rs` (display
//! rotation swaps coded width/height, attached pictures are not video, frame rate
//! from `fps`/`tbr`) which parsed ffprobe JSON; the GenRight pure-Rust probe lives in
//! [`crate::quickprobe`] for exact WAV/MP4 duration without any process.

use std::{path::Path, time::Duration};

use crate::{
    runner::{run_capture, RunError},
    CancellationToken, FfmpegBinary, FfmpegCommand,
};

#[derive(Clone, Debug, PartialEq)]
pub struct VideoStream {
    pub index: u32,
    pub codec: String,
    /// Coded width/height as stored.
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    /// Display rotation in degrees, normalised to 0/90/180/270.
    pub rotation: i32,
    /// The stream is cover art, not moving video.
    pub attached_picture: bool,
}

impl VideoStream {
    /// Dimensions as displayed (a 90/270 rotation swaps width and height).
    pub fn display_size(&self) -> (u32, u32) {
        if matches!(self.rotation, 90 | 270) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioStream {
    pub index: u32,
    pub codec: String,
    pub sample_rate: u32,
    /// Channel count derived from the layout text (`mono`, `stereo`, `5.1`, `N channels`).
    pub channels: u32,
    pub layout: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaProbe {
    /// Container duration in milliseconds (FFmpeg prints centiseconds).
    pub duration_ms: Option<u64>,
    pub bitrate_kbps: Option<u64>,
    pub format_names: Vec<String>,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
}

impl MediaProbe {
    pub fn first_video(&self) -> Option<&VideoStream> {
        self.video.iter().find(|v| !v.attached_picture)
    }

    pub fn first_audio(&self) -> Option<&AudioStream> {
        self.audio.first()
    }
}

#[derive(Debug)]
pub enum ProbeError {
    Run(RunError),
    /// FFmpeg reported the input unreadable.
    Unreadable(String),
    /// No audio or video stream was found.
    NoStreams,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(e) => write!(f, "probe could not run: {e}"),
            Self::Unreadable(m) => write!(f, "input is unreadable: {m}"),
            Self::NoStreams => f.write_str("no audio or video stream found"),
        }
    }
}

impl std::error::Error for ProbeError {}

impl From<RunError> for ProbeError {
    fn from(e: RunError) -> Self {
        Self::Run(e)
    }
}

/// Probe `input` (no output is written).
pub fn probe_media(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    cancel: &CancellationToken,
) -> Result<MediaProbe, ProbeError> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostdin"]).input(input);
    let out = run_capture(ffmpeg, &cmd, Duration::from_secs(60), cancel)?;
    parse_ffmpeg_info(&out.stderr)
}

/// Parse the stderr of `ffmpeg -hide_banner -i <file>`.
pub fn parse_ffmpeg_info(stderr: &str) -> Result<MediaProbe, ProbeError> {
    let mut p = MediaProbe {
        duration_ms: None,
        bitrate_kbps: None,
        format_names: Vec::new(),
        video: Vec::new(),
        audio: Vec::new(),
    };
    let mut saw_input = false;
    for line in stderr.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Input #0, ") {
            saw_input = true;
            if let Some((names, _)) = rest.split_once(", from ") {
                p.format_names = names.split(',').map(|s| s.trim().to_string()).collect();
            }
        } else if let Some(rest) = t.strip_prefix("Duration: ") {
            p.duration_ms = parse_duration_ms(rest.split(',').next().unwrap_or(""));
            p.bitrate_kbps = rest
                .split("bitrate: ")
                .nth(1)
                .and_then(|b| b.split_whitespace().next())
                .and_then(|n| n.parse().ok());
        } else if t.starts_with("Stream #0:") {
            parse_stream(t, &mut p);
        } else if let Some(rest) = t
            .strip_prefix("displaymatrix: rotation of ")
            .or_else(|| t.strip_prefix("Display Matrix: rotation of "))
        {
            // Side data belongs to the most recent video stream. FFmpeg reports it
            // counter-clockwise; the display rotation is its normalisation.
            if let (Some(v), Some(deg)) = (
                p.video.last_mut(),
                rest.split_whitespace().next().and_then(|d| d.parse::<f64>().ok()),
            ) {
                v.rotation = (deg.round() as i64).rem_euclid(360) as i32;
            }
        }
    }
    if !saw_input {
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        return Err(ProbeError::Unreadable(
            tail.into_iter().rev().collect::<Vec<_>>().join(" | "),
        ));
    }
    if p.video.is_empty() && p.audio.is_empty() {
        return Err(ProbeError::NoStreams);
    }
    Ok(p)
}

fn parse_duration_ms(text: &str) -> Option<u64> {
    // HH:MM:SS.cc ("N/A" for unknown)
    let mut it = text.trim().split(':');
    let h: u64 = it.next()?.parse().ok()?;
    let m: u64 = it.next()?.parse().ok()?;
    let s: f64 = it.next()?.parse().ok()?;
    Some((h * 3600 + m * 60) * 1000 + (s * 1000.0).round() as u64)
}

fn parse_stream(line: &str, p: &mut MediaProbe) {
    // "Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6d703461), 48000 Hz, stereo, fltp, 128 kb/s"
    let Some(rest) = line.strip_prefix("Stream #0:") else {
        return;
    };
    let index: u32 = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let Some((_, after)) = rest.split_once(": ") else {
        return;
    };
    let Some((kind, desc)) = after.split_once(": ") else {
        return;
    };
    let fields = split_top_level(desc);
    let codec = fields
        .first()
        .and_then(|f| f.split_whitespace().next())
        .unwrap_or("")
        .to_string();
    match kind {
        "Video" => {
            let (mut width, mut height) = (0, 0);
            for f in &fields {
                // "320x240 [SAR 1:1 DAR 4:3]" or "1280x720"
                let head = f.split_whitespace().next().unwrap_or("");
                if let Some((w, h)) = head.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                        width = w;
                        height = h;
                        break;
                    }
                }
            }
            let rate = |unit: &str| {
                fields.iter().find_map(|f| {
                    f.trim()
                        .strip_suffix(unit)
                        .and_then(|n| n.trim().parse::<f64>().ok())
                })
            };
            let fps = rate(" fps").or_else(|| rate(" tbr"));
            p.video.push(VideoStream {
                index,
                codec,
                width,
                height,
                fps,
                rotation: 0,
                attached_picture: line.contains("(attached pic)"),
            });
        }
        "Audio" => {
            let sample_rate = fields
                .iter()
                .find_map(|f| f.trim().strip_suffix(" Hz").and_then(|n| n.trim().parse().ok()))
                .unwrap_or(0);
            let layout = fields
                .iter()
                .map(|f| f.trim())
                .find(|f| {
                    matches!(*f, "mono" | "stereo")
                        || f.contains("channels")
                        || (f.chars().next().is_some_and(|c| c.is_ascii_digit()) && f.contains('.'))
                })
                .unwrap_or("")
                .to_string();
            p.audio.push(AudioStream {
                index,
                codec,
                sample_rate,
                channels: channels_from_layout(&layout),
                layout,
            });
        }
        _ => {}
    }
}

fn channels_from_layout(layout: &str) -> u32 {
    match layout {
        "mono" => 1,
        "stereo" => 2,
        l => {
            if let Some(n) = l.strip_suffix(" channels").and_then(|n| n.parse().ok()) {
                return n;
            }
            // "5.1", "7.1", "2.1", "5.1(side)"
            let base = l.split('(').next().unwrap_or(l);
            match base.split_once('.') {
                Some((a, b)) => a.parse::<u32>().unwrap_or(0) + b.parse::<u32>().unwrap_or(0),
                None => 0,
            }
        }
    }
}

/// Split on commas that are not inside parentheses or brackets.
fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}
