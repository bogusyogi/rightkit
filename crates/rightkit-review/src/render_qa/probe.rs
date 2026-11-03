//! Probes on a final render, run with the editing ffmpeg/ffprobe, and the pure
//! parsers of their output.

use std::io::Read;
use std::path::{Path, PathBuf};

use rightkit_ffmpeg::FfmpegCommand;
use rightkit_video_project::probe::spawn_ffprobe;
use rightkit_video_project::worker::{spawn_ffmpeg, wait_with_output, StdioMode, StdioPolicy};
use serde_json::Value;

use crate::render_qa::error::QaError;

/// The editing ffmpeg and ffprobe (paths come from the engine bundle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl QaTools {
    pub fn new(ffmpeg: PathBuf, ffprobe: PathBuf) -> Result<Self, QaError> {
        for (name, p) in [("ffmpeg", &ffmpeg), ("ffprobe", &ffprobe)] {
            if !p.is_file() {
                return Err(QaError::ToolMissing(format!(
                    "{name} not found at {}",
                    p.display()
                )));
            }
        }
        Ok(Self { ffmpeg, ffprobe })
    }

    /// First line of `ffmpeg -version`.
    pub fn identity(&self) -> String {
        let mut cmd = FfmpegCommand::new();
        cmd.text("-version");
        spawn_ffmpeg(
            &self.ffmpeg,
            &cmd,
            StdioPolicy {
                stdin: StdioMode::Null,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Piped,
            },
        )
        .and_then(wait_with_output)
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "ffmpeg (version unknown)".into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoStream {
    pub codec: String,
    pub pix_fmt: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub codec: String,
    pub channels: u32,
    pub sample_rate: Option<u32>,
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub format_duration_s: Option<f64>,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn ratio(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    let (n, d) = s.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d != 0.0 && n != 0.0).then_some(n / d)
}

/// Parses `ffprobe -show_streams -show_format -of json` output.
pub fn parse_ffprobe(json: &str) -> Result<Probe, QaError> {
    let doc: Value = serde_json::from_str(json).map_err(|e| QaError::Probe(e.to_string()))?;
    let mut probe = Probe {
        format_duration_s: num(&doc["format"]["duration"]),
        video: None,
        audio: None,
    };
    for s in doc["streams"].as_array().cloned().unwrap_or_default() {
        let codec = s["codec_name"].as_str().unwrap_or("unknown").to_string();
        match s["codec_type"].as_str() {
            Some("video") if probe.video.is_none() => {
                // Attached pictures (cover art) are not the video stream.
                if s["disposition"]["attached_pic"].as_i64() == Some(1) {
                    continue;
                }
                probe.video = Some(VideoStream {
                    codec,
                    pix_fmt: s["pix_fmt"].as_str().map(str::to_string),
                    width: s["width"].as_u64().unwrap_or(0) as u32,
                    height: s["height"].as_u64().unwrap_or(0) as u32,
                    fps: ratio(&s["avg_frame_rate"]).or_else(|| ratio(&s["r_frame_rate"])),
                    start_s: num(&s["start_time"]),
                    duration_s: num(&s["duration"]),
                });
            }
            Some("audio") if probe.audio.is_none() => {
                probe.audio = Some(AudioStream {
                    codec,
                    channels: s["channels"].as_u64().unwrap_or(0) as u32,
                    sample_rate: num(&s["sample_rate"]).map(|v| v as u32),
                    start_s: num(&s["start_time"]),
                    duration_s: num(&s["duration"]),
                });
            }
            _ => {}
        }
    }
    Ok(probe)
}

pub fn probe(tools: &QaTools, file: &Path) -> Result<Probe, QaError> {
    let out = spawn_ffprobe(
        &tools.ffprobe,
        [
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ]
        .into_iter()
        .map(std::ffi::OsStr::new)
        .chain(std::iter::once(file.as_os_str())),
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|e| QaError::io("starting ffprobe", e))?;
    if !out.status.success() {
        return Err(QaError::Probe(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    parse_ffprobe(&String::from_utf8_lossy(&out.stdout))
}

/// A time interval in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Interval {
    pub fn len_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }

    /// True when `self` lies inside any of `ranges` (with `slack_ms` of tolerance on both edges).
    pub fn within(&self, ranges: &[(i64, i64)], slack_ms: i64) -> bool {
        ranges
            .iter()
            .any(|&(a, b)| self.start_ms >= a - slack_ms && self.end_ms <= b + slack_ms)
    }
}

fn secs_to_ms(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

/// Value after `key` up to whitespace, e.g. `black_start:1.5`.
fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(key)? + key.len();
    line[i..].split_whitespace().next()
}

/// `blackdetect` log lines: `black_start:0 black_end:1.2 black_duration:1.2`.
/// A black that never ends (a `black_start` with no `black_end`) is closed at `duration_ms`.
pub fn parse_blackdetect(stderr: &str, duration_ms: i64) -> Vec<Interval> {
    stderr
        .lines()
        .filter(|l| l.contains("black_start:"))
        .filter_map(|l| {
            let start_ms = secs_to_ms(after(l, "black_start:")?.parse::<f64>().ok()?);
            let end_ms = match after(l, "black_end:") {
                Some(v) => secs_to_ms(v.parse::<f64>().ok()?),
                None => duration_ms.max(start_ms),
            };
            Some(Interval { start_ms, end_ms })
        })
        .collect()
}

/// `freezedetect` log lines (`freeze_start` / `freeze_duration` / `freeze_end`).
/// A freeze that never ends is closed at `duration_ms`.
pub fn parse_freezedetect(stderr: &str, duration_ms: i64) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut open: Option<i64> = None;
    for l in stderr.lines() {
        if let Some(v) = after(l, "lavfi.freezedetect.freeze_start:") {
            if let Ok(s) = v.parse::<f64>() {
                open = Some(secs_to_ms(s));
            }
        } else if let Some(v) = after(l, "lavfi.freezedetect.freeze_end:") {
            if let (Some(start), Ok(e)) = (open.take(), v.parse::<f64>()) {
                out.push(Interval {
                    start_ms: start,
                    end_ms: secs_to_ms(e),
                });
            }
        }
    }
    if let Some(start) = open {
        out.push(Interval {
            start_ms: start,
            end_ms: duration_ms.max(start),
        });
    }
    merge_abutting(out)
}

/// One still picture can be reported as several back-to-back freezes (the detector
/// restarts where the encoder changes the frame slightly, e.g. at a keyframe). Intervals
/// closer than one frame are one freeze, so its length is judged whole.
fn merge_abutting(intervals: Vec<Interval>) -> Vec<Interval> {
    const MAX_GAP_MS: i64 = 42;
    let mut out: Vec<Interval> = Vec::with_capacity(intervals.len());
    for next in intervals {
        match out.last_mut() {
            Some(last) if next.start_ms - last.end_ms <= MAX_GAP_MS => {
                last.end_ms = last.end_ms.max(next.end_ms);
            }
            _ => out.push(next),
        }
    }
    out
}

/// `silencedetect` log lines. A silence that never ends is closed at `duration_ms`.
pub fn parse_silencedetect(stderr: &str, duration_ms: i64) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut open: Option<i64> = None;
    for l in stderr.lines() {
        if let Some(v) = after(l, "silence_start:") {
            if let Ok(s) = v.parse::<f64>() {
                open = Some(secs_to_ms(s.max(0.0)));
            }
        } else if let Some(v) = after(l, "silence_end:") {
            if let (Some(start), Ok(e)) = (open.take(), v.parse::<f64>()) {
                out.push(Interval {
                    start_ms: start,
                    end_ms: secs_to_ms(e),
                });
            }
        }
    }
    if let Some(start) = open {
        out.push(Interval {
            start_ms: start,
            end_ms: duration_ms.max(start),
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ebur128 {
    /// `None` when the audio is entirely below the gate (reported as `-inf`).
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
}

fn labeled(text: &str, label: &str) -> Option<Option<f64>> {
    let line = text.lines().find(|l| l.trim_start().starts_with(label))?;
    let v = line
        .trim_start()
        .strip_prefix(label)?
        .split_whitespace()
        .next()?;
    Some(v.parse::<f64>().ok().filter(|x| x.is_finite()))
}

/// The `Summary:` block of `ebur128=peak=true`.
pub fn parse_ebur128(stderr: &str) -> Option<Ebur128> {
    let summary = &stderr[stderr.rfind("Summary:")?..];
    Some(Ebur128 {
        integrated_lufs: labeled(summary, "I:")?,
        true_peak_dbtp: labeled(summary, "Peak:").flatten(),
    })
}

/// Counts runs of full-scale samples per channel over interleaved little-endian f32 PCM.
#[derive(Debug, Clone)]
pub struct ClipCounter {
    channels: usize,
    threshold: f32,
    min_run: u32,
    runs: Vec<u32>,
    index: usize,
    pub clipped_runs: u64,
    pub clipped_samples: u64,
    pub peak: f32,
    pub samples: u64,
    carry: Vec<u8>,
}

impl ClipCounter {
    pub fn new(channels: u32, threshold: f64, min_run: u32) -> Self {
        let channels = channels.max(1) as usize;
        Self {
            channels,
            threshold: threshold as f32,
            min_run: min_run.max(1),
            runs: vec![0; channels],
            index: 0,
            clipped_runs: 0,
            clipped_samples: 0,
            peak: 0.0,
            samples: 0,
            carry: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(bytes);
        let whole = buf.len() / 4 * 4;
        for c in buf[..whole].as_chunks::<4>().0 {
            let v = f32::from_le_bytes(*c).abs();
            self.samples += 1;
            if v > self.peak {
                self.peak = v;
            }
            let ch = self.index;
            self.index = (self.index + 1) % self.channels;
            if v >= self.threshold {
                self.runs[ch] += 1;
                if self.runs[ch] == self.min_run {
                    self.clipped_runs += 1;
                    self.clipped_samples += self.min_run as u64;
                } else if self.runs[ch] > self.min_run {
                    self.clipped_samples += 1;
                }
            } else {
                self.runs[ch] = 0;
            }
        }
        self.carry = buf[whole..].to_vec();
    }
}

// RIGHTKIT GAP: the typed worker boundary stays. rightkit-qa's media and loudness calls
// (tracked: bogusyogi/rightkit#355)
// (media.rs:330 `ffmpeg_to_file`, loudness.rs:253) spawn through its own process runner
// (process.rs:190 `run_owned`) with its own tracker, so they are not used for these passes.
fn run_info(tools: &QaTools, args: &[String]) -> Result<String, QaError> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostats", "-nostdin", "-v", "info"])
        .texts(args.iter().cloned());
    let out = spawn_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|e| QaError::io("starting ffmpeg", e))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(QaError::ToolFailed {
            tool: "ffmpeg",
            message: stderr.lines().last().unwrap_or("").to_string(),
        });
    }
    Ok(stderr)
}

fn input(file: &Path, pre: &[&str], post: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = pre.iter().map(|s| s.to_string()).collect();
    a.push("-i".into());
    a.push(file.display().to_string());
    a.extend(post.iter().map(|s| s.to_string()));
    a
}

/// Full decode of every stream; `true` only for a clean exit and empty error log.
pub fn decode_clean(tools: &QaTools, file: &Path) -> Result<(bool, String), QaError> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostdin", "-v", "error"])
        .input(file);
    cmd.texts(["-map", "0:V?", "-map", "0:a?", "-f", "null", "-"]);
    let out = spawn_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|e| QaError::io("starting ffmpeg", e))?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Ok((out.status.success() && stderr.is_empty(), stderr))
}

/// One video pass: black and frozen intervals.
// RIGHTKIT GAP: blackdetect and freezedetect interval output has no shared equivalent.
// (tracked: bogusyogi/rightkit#355)
// rightkit-review's frame rules need per-frame samples, and rightkit-qa has no black or freeze
// detector. The thresholds (`pix_th`, noise dB) are host policy.
pub fn detect_video(
    tools: &QaTools,
    file: &Path,
    black_min_ms: i64,
    black_pix_th: f64,
    freeze_min_ms: i64,
    freeze_noise_db: f64,
    duration_ms: i64,
) -> Result<(Vec<Interval>, Vec<Interval>), QaError> {
    let vf = format!(
        "blackdetect=d={:.3}:pic_th=0.98:pix_th={black_pix_th:.3},freezedetect=n={freeze_noise_db}dB:d={:.3}",
        black_min_ms as f64 / 1000.0,
        freeze_min_ms as f64 / 1000.0
    );
    let stderr = run_info(
        tools,
        &input(file, &[], &["-an", "-vf", &vf, "-f", "null", "-"]),
    )?;
    Ok((
        parse_blackdetect(&stderr, duration_ms),
        parse_freezedetect(&stderr, duration_ms),
    ))
}

/// One audio pass: silence intervals and EBU R128 loudness / true peak.
// RIGHTKIT GAP: silencedetect has no shared rule. The ebur128 pass runs the filter that
// (tracked: bogusyogi/rightkit#355)
// rightkit-qa::loudness::ebur128_ffmpeg (loudness.rs:253) also runs, but that wrapper returns an
// error where this returns None (no summary), and it reports one peak for both sample and true
// peak (loudness.rs:310-311). Kept here.
pub fn detect_audio(
    tools: &QaTools,
    file: &Path,
    silence_noise_db: f64,
    silence_min_ms: i64,
    duration_ms: i64,
) -> Result<(Vec<Interval>, Option<Ebur128>), QaError> {
    let af = format!(
        "silencedetect=noise={silence_noise_db}dB:d={:.3},ebur128=peak=true:framelog=quiet",
        silence_min_ms as f64 / 1000.0
    );
    let stderr = run_info(
        tools,
        &input(file, &[], &["-vn", "-af", &af, "-f", "null", "-"]),
    )?;
    Ok((
        parse_silencedetect(&stderr, duration_ms),
        parse_ebur128(&stderr),
    ))
}

/// Streams the decoded audio as interleaved f32 and counts clipped runs (no temp files).
// RIGHTKIT GAP: no shared clipped-run counter exists in rightkit-review or rightkit-qa.
// (tracked: bogusyogi/rightkit#355)
pub fn scan_clipping(
    tools: &QaTools,
    file: &Path,
    channels: u32,
    threshold: f64,
    min_run: u32,
) -> Result<ClipCounter, QaError> {
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-hide_banner", "-nostdin", "-v", "error"])
        .input(file);
    cmd.texts(["-vn", "-f", "f32le", "-acodec", "pcm_f32le", "-"]);
    // stdin stays inherited, as before: `Command::spawn` defaults to inherit.
    let mut child = spawn_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        StdioPolicy {
            stdin: StdioMode::Inherit,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .map_err(|e| QaError::io("starting ffmpeg", e))?;
    let mut stdout = child.take_stdout().expect("piped stdout");
    // Stderr drains on its own thread: stdout is read to EOF below, and an unread stderr
    // pipe could fill and stall ffmpeg before stdout ends.
    let mut stderr_pipe = child.take_stderr().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut bytes);
        bytes
    });
    let mut counter = ClipCounter::new(channels, threshold, min_run);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = stdout
            .read(&mut buf)
            .map_err(|e| QaError::io("reading decoded audio", e))?;
        if n == 0 {
            break;
        }
        counter.feed(&buf[..n]);
    }
    let status = child
        .wait()
        .map_err(|e| QaError::io("waiting for ffmpeg", e))?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(QaError::ToolFailed {
            tool: "ffmpeg",
            message: String::from_utf8_lossy(&stderr).trim().to_string(),
        });
    }
    Ok(counter)
}
