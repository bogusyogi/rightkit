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

#[cfg(test)]
mod tests {
    use super::*;

    // Unit test: the split is a detector artefact no fixture reproduces on demand.
    #[test]
    fn back_to_back_freezes_are_one_freeze() {
        let log = "lavfi.freezedetect.freeze_start: 2.967\nlavfi.freezedetect.freeze_end: 4.0\n\
                   lavfi.freezedetect.freeze_start: 4.0\nlavfi.freezedetect.freeze_end: 7.0\n\
                   lavfi.freezedetect.freeze_start: 9.0\nlavfi.freezedetect.freeze_end: 9.5\n";
        let found = parse_freezedetect(log, 10_000);
        let spans: Vec<(i64, i64)> = found.iter().map(|i| (i.start_ms, i.end_ms)).collect();
        assert_eq!(spans, vec![(2967, 7000), (9000, 9500)]);
    }

    #[test]
    fn parses_detector_logs() {
        let log = "[blackdetect @ 0x1] black_start:1.5 black_end:3.25 black_duration:1.75\n\
                   [Parsed_freezedetect_1 @ 0x2] lavfi.freezedetect.freeze_start: 4\n\
                   [Parsed_freezedetect_1 @ 0x2] lavfi.freezedetect.freeze_duration: 2\n\
                   [Parsed_freezedetect_1 @ 0x2] lavfi.freezedetect.freeze_end: 6\n\
                   [Parsed_freezedetect_1 @ 0x2] lavfi.freezedetect.freeze_start: 9\n";
        assert_eq!(
            parse_blackdetect(log, 10_000),
            vec![Interval {
                start_ms: 1500,
                end_ms: 3250
            }]
        );
        let f = parse_freezedetect(log, 10_000);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].start_ms, f[0].end_ms), (4000, 6000));
        assert_eq!((f[1].start_ms, f[1].end_ms), (9000, 10_000));
        let s = parse_silencedetect(
            "[silencedetect @ 0x3] silence_start: 2.5\n[silencedetect @ 0x3] silence_end: 4.5 | silence_duration: 2\n[silencedetect @ 0x3] silence_start: 8\n",
            9_000,
        );
        assert_eq!(s.len(), 2);
        assert_eq!((s[1].start_ms, s[1].end_ms), (8000, 9000));
    }

    #[test]
    fn parses_ebur128_summary() {
        let log = "garbage\n  Summary:\n\n  Integrated loudness:\n    I:         -14.2 LUFS\n    Threshold: -24.3 LUFS\n\n  Loudness range:\n    LRA:         3.1 LU\n\n  True peak:\n    Peak:       -1.4 dBFS\n";
        let e = parse_ebur128(log).unwrap();
        assert_eq!(e.integrated_lufs, Some(-14.2));
        assert_eq!(e.true_peak_dbtp, Some(-1.4));
        let silent = "  Summary:\n  Integrated loudness:\n    I:         -inf LUFS\n  True peak:\n    Peak:       -inf dBFS\n";
        let e = parse_ebur128(silent).unwrap();
        assert_eq!(e.integrated_lufs, None);
        assert_eq!(e.true_peak_dbtp, None);
        assert!(parse_ebur128("nothing here").is_none());
    }

    #[test]
    fn clip_counter_counts_runs_per_channel_across_chunks() {
        let mut c = ClipCounter::new(2, 0.999, 3);
        let mut bytes = Vec::new();
        // L: 1,1,1,1,0.2 ; R: 0.1 throughout -> one run of 4 on L.
        for l in [1.0f32, 1.0, 1.0, 1.0, 0.2] {
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&0.1f32.to_le_bytes());
        }
        // Split mid-sample to exercise the carry.
        c.feed(&bytes[..7]);
        c.feed(&bytes[7..]);
        assert_eq!(c.clipped_runs, 1);
        assert_eq!(c.clipped_samples, 4);
        assert_eq!(c.samples, 10);
        assert!((c.peak - 1.0).abs() < 1e-6);
        // Two isolated full-scale samples are not clipping.
        let mut c = ClipCounter::new(1, 0.999, 3);
        let mut b = Vec::new();
        for v in [1.0f32, 0.0, 1.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        c.feed(&b);
        assert_eq!(c.clipped_runs, 0);
    }

    #[test]
    fn parses_ffprobe_json_and_skips_cover_art() {
        let json = r#"{"streams":[
          {"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1},"width":10,"height":10},
          {"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"30000/1001","start_time":"0.000000","duration":"3.003000","disposition":{"attached_pic":0}},
          {"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000","start_time":"0.000000","duration":"3.010000"}],
          "format":{"duration":"3.010000"}}"#;
        let p = parse_ffprobe(json).unwrap();
        let v = p.video.unwrap();
        assert_eq!((v.codec.as_str(), v.width, v.height), ("h264", 1920, 1080));
        assert!((v.fps.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(p.audio.unwrap().sample_rate, Some(48000));
        assert_eq!(p.format_duration_s, Some(3.01));
    }

    #[test]
    fn interval_within_allowed_ranges() {
        let i = Interval {
            start_ms: 0,
            end_ms: 400,
        };
        assert!(i.within(&[(0, 500)], 0));
        assert!(i.within(&[(50, 450)], 60));
        assert!(!i.within(&[(1000, 2000)], 100));
    }

    // Unit test: a detector line cut off by end of stream is malformed-input territory no e2e render produces.
    #[test]
    fn unterminated_black_freeze_and_silence_close_at_duration() {
        assert_eq!(
            parse_blackdetect("[blackdetect @ 0x1] black_start:7\n", 10_000),
            vec![Interval {
                start_ms: 7000,
                end_ms: 10_000
            }]
        );
        assert_eq!(
            parse_freezedetect("lavfi.freezedetect.freeze_start: 2\n", 10_000),
            vec![Interval {
                start_ms: 2000,
                end_ms: 10_000
            }]
        );
        assert_eq!(
            parse_silencedetect("[silencedetect @ 0x3] silence_start: 8\n", 10_000),
            vec![Interval {
                start_ms: 8000,
                end_ms: 10_000
            }]
        );
    }
}
