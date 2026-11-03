//! Typed media probe through the verified FFmpeg binary.
//!
//! The artifact catalog ships `ffmpeg` only (no `ffprobe`), so the probe runs
//! `ffmpeg -i <file>` and parses the stream summary FFmpeg prints on stderr. Donor
//! behavior reconciled from CutRight `cutright-project/src/probe.rs` (display
//! rotation swaps coded width/height, attached pictures are not video, frame rate
//! from `fps`/`tbr`) which parsed ffprobe JSON; the GenRight pure-Rust probe lives in
//! [`crate::quickprobe`] for exact WAV/MP4 duration without any process.

use std::{collections::BTreeMap, path::Path, time::Duration};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    runner::{run_capture_with, RunError, RunOptions},
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

#[non_exhaustive]
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

/// Probe `input` (no output is written), bounded by a 60 s deadline. See
/// [`probe_media_with`] for another deadline or none, and [`probe_media_opt`] for inputs
/// with no streams.
pub fn probe_media(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    cancel: &CancellationToken,
) -> Result<MediaProbe, ProbeError> {
    probe_media_with(ffmpeg, input, Some(Duration::from_secs(60)), cancel)
}

/// [`probe_media`] with an explicit deadline measured from spawn. `None` means no
/// deadline; cancellation still applies in every mode.
pub fn probe_media_with(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<MediaProbe, ProbeError> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostdin"]).input(input);
    let out = run_capture_with(
        ffmpeg,
        &cmd,
        RunOptions::unbounded(cancel.clone()).with_deadline(timeout),
    )?;
    parse_ffmpeg_info(&out.stderr)
}

/// [`probe_media`], except an input with no audio or video streams is `Ok(None)` rather
/// than [`ProbeError::NoStreams`]. Unreadable input is still an error. The deadline is
/// 60 s; see [`probe_media_opt_with`] for another deadline or none.
pub fn probe_media_opt(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    cancel: &CancellationToken,
) -> Result<Option<MediaProbe>, ProbeError> {
    probe_media_opt_with(ffmpeg, input, Some(Duration::from_secs(60)), cancel)
}

/// [`probe_media_opt`] with an explicit deadline measured from spawn. `None` means no
/// deadline; cancellation still applies in every mode.
pub fn probe_media_opt_with(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<Option<MediaProbe>, ProbeError> {
    match probe_media_with(ffmpeg, input, timeout, cancel) {
        Ok(probe) => Ok(Some(probe)),
        Err(ProbeError::NoStreams) => Ok(None),
        Err(other) => Err(other),
    }
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
                rest.split_whitespace()
                    .next()
                    .and_then(|d| d.parse::<f64>().ok()),
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
                .find_map(|f| {
                    f.trim()
                        .strip_suffix(" Hz")
                        .and_then(|n| n.trim().parse().ok())
                })
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

// ---------------------------------------------------------------------------
// ffprobe JSON
// ---------------------------------------------------------------------------

/// Frame rates closer than this (fps) count as equal. ffprobe prints exact rationals,
/// so this only absorbs floating-point noise.
const FRAME_RATE_EPSILON: f64 = 0.001;

/// An exact rational such as a frame rate: `30000/1001` stays `30000/1001`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// `30000/1001` or `25`. Zero, negative, and unparsable values (including `0/0`, which
    /// ffprobe prints for an unknown rate) are `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (num, den) = match text.split_once('/') {
            Some((num, den)) => (
                num.trim().parse::<i64>().ok()?,
                den.trim().parse::<i64>().ok()?,
            ),
            None => (text.parse::<i64>().ok()?, 1),
        };
        (num > 0 && den > 0).then_some(Self { num, den })
    }
}

/// Typed view of `ffprobe -v error -print_format json -show_format -show_streams
/// -show_chapters`. Numbers that ffprobe prints as strings are parsed; a field that is
/// absent or unparsable is `None`, and unknown fields are ignored so newer ffprobe
/// builds still parse. A file with no streams is `Ok` with empty `streams`: the caller
/// decides whether that is an error (unlike [`parse_ffmpeg_info`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeJson {
    pub format: ProbeFormat,
    pub streams: Vec<ProbeStream>,
    pub chapters: Vec<ProbeChapter>,
}

impl ProbeJson {
    /// The first video stream that is not cover art.
    pub fn first_video(&self) -> Option<&ProbeStream> {
        self.streams
            .iter()
            .find(|s| s.is_video() && !s.attached_picture)
    }

    pub fn first_audio(&self) -> Option<&ProbeStream> {
        self.streams.iter().find(|s| s.is_audio())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeFormat {
    /// Container names as ffprobe reports them, e.g. `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format_name: Option<String>,
    /// Container duration in seconds (`f64`, for display and rough maths).
    pub duration_s: Option<f64>,
    /// The duration exactly as ffprobe printed it, e.g. `12.500000`.
    pub duration_raw: Option<String>,
    /// The duration in nanoseconds, parsed exactly from [`ProbeFormat::duration_raw`].
    pub duration_ns: Option<u64>,
    pub size_bytes: Option<u64>,
    pub bit_rate: Option<u64>,
    /// `creation_time` tag (ISO 8601 text, as written).
    pub creation_time: Option<String>,
    /// `timecode` tag, e.g. `01:00:00:00`.
    pub timecode: Option<String>,
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeStream {
    pub index: u32,
    /// `video`, `audio`, `subtitle`, `data`, or `attachment`.
    pub codec_type: Option<String>,
    pub codec_name: Option<String>,
    /// Codec profile as ffprobe names it, e.g. `High 10` or `LC`.
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    /// Bits per coded sample for video (`bits_per_raw_sample`).
    pub bits_per_raw_sample: Option<u32>,
    /// Colour metadata as tagged by the encoder (`bt709`, `bt2020`, `smpte2084`, ...).
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    /// Display rotation in degrees, normalised to `0..360` from the display matrix.
    pub rotation: i32,
    /// `r_frame_rate`: the lowest common denominator rate, exact.
    pub r_frame_rate: Option<Rational>,
    /// `avg_frame_rate`: the average rate, exact (`None` for `0/0`).
    pub avg_frame_rate: Option<Rational>,
    /// `time_base`: the unit of this stream's packet timestamps, exact (`1/90000`).
    /// `None` when absent, `0/0`, or a zero denominator.
    pub time_base: Option<Rational>,
    pub duration_s: Option<f64>,
    /// The stream duration exactly as ffprobe printed it.
    pub duration_raw: Option<String>,
    /// The stream duration in nanoseconds, parsed exactly from [`ProbeStream::duration_raw`].
    pub duration_ns: Option<u64>,
    pub bit_rate: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    /// `disposition.attached_pic`: cover art, not moving video.
    pub attached_picture: bool,
    /// `creation_time` tag on this stream.
    pub creation_time: Option<String>,
    /// `timecode` tag on this stream (timecode data streams carry it here).
    pub timecode: Option<String>,
    pub tags: BTreeMap<String, String>,
}

impl ProbeStream {
    pub fn is_video(&self) -> bool {
        self.codec_type.as_deref() == Some("video")
    }

    pub fn is_audio(&self) -> bool {
        self.codec_type.as_deref() == Some("audio")
    }

    /// `r_frame_rate` in frames per second.
    pub fn r_frame_rate_fps(&self) -> Option<f64> {
        self.r_frame_rate.map(Rational::as_f64)
    }

    /// `avg_frame_rate` in frames per second.
    pub fn avg_frame_rate_fps(&self) -> Option<f64> {
        self.avg_frame_rate.map(Rational::as_f64)
    }

    /// HDR by transfer function: PQ (`smpte2084`) or HLG (`arib-std-b67`). Wide-gamut
    /// BT.2020 primaries on an SDR transfer are not counted as HDR.
    pub fn is_hdr(&self) -> bool {
        matches!(
            self.color_transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        )
    }

    /// Variable frame rate: both rates are known and differ. A stream whose average rate
    /// is unknown (`0/0`) is not reported as variable.
    pub fn is_variable_frame_rate(&self) -> bool {
        matches!(
            (self.r_frame_rate_fps(), self.avg_frame_rate_fps()),
            (Some(r), Some(a)) if (r - a).abs() > FRAME_RATE_EPSILON
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeChapter {
    pub id: i64,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub title: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawProbe {
    format: RawFormat,
    streams: Vec<RawStream>,
    chapters: Vec<RawChapter>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<Value>,
    size: Option<Value>,
    bit_rate: Option<Value>,
    tags: BTreeMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawStream {
    index: Option<Value>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    width: Option<Value>,
    height: Option<Value>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<Value>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    color_range: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    time_base: Option<String>,
    duration: Option<Value>,
    bit_rate: Option<Value>,
    sample_rate: Option<Value>,
    channels: Option<Value>,
    channel_layout: Option<String>,
    disposition: RawDisposition,
    side_data_list: Vec<RawSideData>,
    tags: BTreeMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDisposition {
    attached_pic: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSideData {
    rotation: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawChapter {
    id: Option<Value>,
    start_time: Option<Value>,
    end_time: Option<Value>,
    tags: BTreeMap<String, Value>,
}

/// A JSON number or a numeric string, as a finite `f64`.
fn value_f64(value: &Option<Value>) -> Option<f64> {
    let parsed = match value.as_ref()? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    parsed.filter(|v| v.is_finite())
}

/// A JSON integer or integer string, exact (no `f64` round trip).
fn value_i64(value: &Option<Value>) -> Option<i64> {
    match value.as_ref()? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn value_u64(value: &Option<Value>) -> Option<u64> {
    value_f64(value).filter(|v| *v >= 0.0).map(|v| v as u64)
}

fn value_u32(value: &Option<Value>) -> Option<u32> {
    value_u64(value).and_then(|v| u32::try_from(v).ok())
}

/// The value as printed (string or number), trimmed; empty is `None`.
fn value_text(value: &Option<Value>) -> Option<String> {
    match value.as_ref()? {
        Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn tags_as_text(tags: BTreeMap<String, Value>) -> BTreeMap<String, String> {
    tags.into_iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            (key, text)
        })
        .collect()
}

/// Case-insensitive tag lookup; empty values count as absent.
fn tag_lookup(tags: &BTreeMap<String, String>, key: &str) -> Option<String> {
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Seconds as printed (`12.500000`, `0.000000001`, `3`) to nanoseconds, exactly. Digits
/// past the ninth are truncated. Exponents, signs, and overflow are `None`.
fn parse_decimal_ns(text: &str) -> Option<u64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().ok()?
    };
    let mut fraction_ns = 0u64;
    for position in 0..9 {
        let digit = fraction
            .as_bytes()
            .get(position)
            .map_or(0, |b| u64::from(b - b'0'));
        fraction_ns = fraction_ns * 10 + digit;
    }
    whole.checked_mul(1_000_000_000)?.checked_add(fraction_ns)
}

/// Rotation from the first side-data entry that carries one, normalised to `0..360`.
fn rotation_of(side_data: &[RawSideData]) -> i32 {
    side_data
        .iter()
        .find_map(|entry| value_f64(&entry.rotation))
        .map_or(0, |degrees| (degrees.round() as i64).rem_euclid(360) as i32)
}

/// Parse the JSON printed by `ffprobe -v error -print_format json -show_format
/// -show_streams -show_chapters`. Invalid JSON is [`ProbeError::Unreadable`].
pub fn parse_ffprobe_json(json: &str) -> Result<ProbeJson, ProbeError> {
    let raw: RawProbe = serde_json::from_str(json)
        .map_err(|e| ProbeError::Unreadable(format!("ffprobe JSON is not valid: {e}")))?;

    let format_tags = tags_as_text(raw.format.tags);
    let format_duration_raw = value_text(&raw.format.duration);
    let format = ProbeFormat {
        format_name: raw.format.format_name,
        duration_s: value_f64(&raw.format.duration),
        duration_ns: format_duration_raw.as_deref().and_then(parse_decimal_ns),
        duration_raw: format_duration_raw,
        size_bytes: value_u64(&raw.format.size),
        bit_rate: value_u64(&raw.format.bit_rate),
        creation_time: tag_lookup(&format_tags, "creation_time"),
        timecode: tag_lookup(&format_tags, "timecode"),
        tags: format_tags,
    };

    let streams: Vec<ProbeStream> = raw
        .streams
        .into_iter()
        .enumerate()
        .map(|(position, s)| {
            let tags = tags_as_text(s.tags);
            let duration_raw = value_text(&s.duration);
            ProbeStream {
                index: value_u32(&s.index).unwrap_or(position as u32),
                codec_type: s.codec_type,
                codec_name: s.codec_name,
                profile: s.profile,
                width: value_u32(&s.width),
                height: value_u32(&s.height),
                pix_fmt: s.pix_fmt,
                bits_per_raw_sample: value_u32(&s.bits_per_raw_sample),
                color_primaries: s.color_primaries,
                color_transfer: s.color_transfer,
                color_space: s.color_space,
                color_range: s.color_range,
                rotation: rotation_of(&s.side_data_list),
                r_frame_rate: s.r_frame_rate.as_deref().and_then(Rational::parse),
                avg_frame_rate: s.avg_frame_rate.as_deref().and_then(Rational::parse),
                time_base: s.time_base.as_deref().and_then(Rational::parse),
                duration_s: value_f64(&s.duration),
                duration_ns: duration_raw.as_deref().and_then(parse_decimal_ns),
                duration_raw,
                bit_rate: value_u64(&s.bit_rate),
                sample_rate: value_u32(&s.sample_rate),
                channels: value_u32(&s.channels),
                channel_layout: s.channel_layout,
                attached_picture: value_f64(&s.disposition.attached_pic).is_some_and(|v| v != 0.0),
                creation_time: tag_lookup(&tags, "creation_time"),
                timecode: tag_lookup(&tags, "timecode"),
                tags,
            }
        })
        .collect();

    let chapters: Vec<ProbeChapter> = raw
        .chapters
        .into_iter()
        .enumerate()
        .map(|(position, c)| {
            let tags = tags_as_text(c.tags);
            ProbeChapter {
                id: value_i64(&c.id).unwrap_or(position as i64),
                start_s: value_f64(&c.start_time),
                end_s: value_f64(&c.end_time),
                title: tag_lookup(&tags, "title"),
            }
        })
        .collect();

    Ok(ProbeJson {
        format,
        streams,
        chapters,
    })
}

/// `ffprobe` arguments for [`run_ffprobe_json`]. Not [`FfmpegCommand::base`]: that
/// carries ffmpeg-only flags such as `-y` and `-nostats`, which ffprobe rejects.
pub fn ffprobe_json_command(input: &Path) -> FfmpegCommand {
    let mut cmd = FfmpegCommand::new();
    cmd.texts([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_chapters",
    ])
    .path(input);
    cmd
}

/// Run `ffprobe` (a catalog-verified or development binary) on `input` and parse its
/// JSON, bounded by `timeout` (`None` = no deadline; cancellation still applies). A
/// non-zero exit is [`ProbeError::Unreadable`] with the stderr tail.
pub fn run_ffprobe_json(
    ffprobe: &dyn FfmpegBinary,
    input: &Path,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<ProbeJson, ProbeError> {
    let out = run_capture_with(
        ffprobe,
        &ffprobe_json_command(input),
        RunOptions::unbounded(cancel.clone()).with_deadline(timeout),
    )?;
    if !out.status.success() {
        let tail: Vec<&str> = out.stderr.lines().rev().take(3).collect();
        return Err(ProbeError::Unreadable(
            tail.into_iter().rev().collect::<Vec<_>>().join(" | "),
        ));
    }
    parse_ffprobe_json(&String::from_utf8_lossy(&out.stdout))
}

impl crate::VerifiedFfprobe {
    /// [`run_ffprobe_json`] through this catalog-verified ffprobe.
    pub fn ffprobe_json(
        &self,
        input: &Path,
        timeout: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<ProbeJson, ProbeError> {
        run_ffprobe_json(self, input, timeout, cancel)
    }
}

/// One packet from `ffprobe -show_packets`, in the stream's time base.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PacketSample {
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub size: Option<u64>,
    /// ffprobe's flag string, e.g. `K__` for a keyframe.
    pub flags: String,
}

impl PacketSample {
    /// `pts` in seconds for a stream with this `time_base`. The result is an `f64`
    /// (`pts * num / den`), so it is rounded; compare `pts` itself when exactness matters.
    pub fn pts_seconds(&self, time_base: Rational) -> Option<f64> {
        self.pts.map(|ticks| ticks_to_seconds(ticks, time_base))
    }

    /// `dts` in seconds; same `f64` caveat as [`PacketSample::pts_seconds`].
    pub fn dts_seconds(&self, time_base: Rational) -> Option<f64> {
        self.dts.map(|ticks| ticks_to_seconds(ticks, time_base))
    }
}

fn ticks_to_seconds(ticks: i64, time_base: Rational) -> f64 {
    ticks as f64 * time_base.num as f64 / time_base.den as f64
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPackets {
    packets: Vec<RawPacket>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPacket {
    pts: Option<Value>,
    dts: Option<Value>,
    duration: Option<Value>,
    size: Option<Value>,
    flags: Option<String>,
}

/// `ffprobe` arguments for [`probe_packets`]: the first `limit` packets of
/// `stream_index`, as JSON.
pub fn packet_listing_command(input: &Path, stream_index: u32, limit: u32) -> FfmpegCommand {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-v", "error", "-select_streams"])
        .text(stream_index.to_string())
        .texts(["-read_intervals"])
        .text(format!("%+#{limit}"))
        .texts([
            "-print_format",
            "json",
            "-show_packets",
            "-show_entries",
            "packet=pts,dts,duration,size,flags",
        ])
        .path(input);
    cmd
}

/// Parse the JSON from [`packet_listing_command`].
pub fn parse_ffprobe_packets(json: &str) -> Result<Vec<PacketSample>, ProbeError> {
    let raw: RawPackets = serde_json::from_str(json)
        .map_err(|e| ProbeError::Unreadable(format!("ffprobe packet JSON is not valid: {e}")))?;
    Ok(raw
        .packets
        .into_iter()
        .map(|p| PacketSample {
            pts: value_i64(&p.pts),
            dts: value_i64(&p.dts),
            duration: value_i64(&p.duration),
            size: value_u64(&p.size),
            flags: p.flags.unwrap_or_default(),
        })
        .collect())
}

/// The first `limit` packets of one stream, for VFR and first-PTS checks. `limit` must
/// be at least 1. `timeout` of `None` means no deadline; cancellation still applies.
pub fn probe_packets(
    ffprobe: &dyn FfmpegBinary,
    input: &Path,
    stream_index: u32,
    limit: u32,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<Vec<PacketSample>, ProbeError> {
    if limit == 0 {
        return Err(ProbeError::Unreadable(
            "packet limit must be at least 1".into(),
        ));
    }
    let out = run_capture_with(
        ffprobe,
        &packet_listing_command(input, stream_index, limit),
        RunOptions::unbounded(cancel.clone()).with_deadline(timeout),
    )?;
    if !out.status.success() {
        let tail: Vec<&str> = out.stderr.lines().rev().take(3).collect();
        return Err(ProbeError::Unreadable(
            tail.into_iter().rev().collect::<Vec<_>>().join(" | "),
        ));
    }
    parse_ffprobe_packets(&String::from_utf8_lossy(&out.stdout))
}
