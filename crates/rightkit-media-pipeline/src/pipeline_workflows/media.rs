//! Native media helpers for pipeline steps: FFmpeg/ffprobe discovery and
//! probing, whisper.cpp CLI discovery. All operations are subprocess calls on
//! resolved binaries — no Node/Python. FFmpeg/ffprobe discovery is
//! `crate::ffmpeg` (rightkit-ffmpeg: dev env override, bundled externalBin, dev PATH);
//! whisper-cli keeps its own bundled -> `RIGHTKIT_WHISPER` -> PATH order.

use std::path::{Path, PathBuf};

use rightkit_media_gen::host::ffmpeg::{trust, FfmpegBinary, FfmpegHandle, FfprobeHandle};
use rightkit_media_gen::host::h264;
use serde_json::{json, Value};

use crate::{GenError, GenResult};

pub struct Tools {
    /// The owner's handles (verified or development); spawns take `.ffmpeg_path()` at the call site.
    pub ffmpeg: Option<FfmpegHandle>,
    pub ffprobe: Option<FfprobeHandle>,
    /// The owner's verification error when `ffmpeg` / `ffprobe` did not resolve (no fallback exists).
    pub ffmpeg_error: Option<String>,
    pub ffprobe_error: Option<String>,
    pub whisper: Option<PathBuf>,
}

fn from_env(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn bundled(name: &str) -> Option<PathBuf> {
    // Installed: externalBin lands beside the app executable. Dev checkout:
    // src-tauri/binaries/<name>-<target triple>(.exe).
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let direct = exe_dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let stem = name.trim_end_matches(".exe");
    let triple = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let suffixed = format!("{stem}-{triple}{}", if cfg!(windows) { ".exe" } else { "" });
    for ancestor in exe_dir.ancestors() {
        let cand = ancestor.join("src-tauri").join("binaries").join(&suffixed);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Resolve the tool set once per process.
pub fn tools() -> &'static Tools {
    static TOOLS: std::sync::OnceLock<Tools> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        let ffmpeg = crate::ffmpeg::resolve_ffmpeg();
        let ffprobe = crate::ffmpeg::resolve_ffprobe();
        Tools {
            ffmpeg_error: ffmpeg.as_ref().err().map(ToString::to_string),
            ffprobe_error: ffprobe.as_ref().err().map(ToString::to_string),
            ffmpeg: ffmpeg.ok(),
            ffprobe: ffprobe.ok(),
            // whisper-cli ships as a Tauri externalBin (built by
            // scripts/build-whisper-for-tauri.mjs): bundled first, then the
            // RIGHTKIT_WHISPER dev override, then PATH.
            whisper: bundled("whisper-cli")
                .or_else(|| from_env("RIGHTKIT_WHISPER"))
                .or_else(|| on_path("whisper-cli"))
                .or_else(|| on_path("whisper-cpp"))
                .or_else(|| on_path("whisper"))
                .or_else(|| on_path("main")),
        }
    })
}

/// Run a binary with args; capture stdout+stderr. `ffmpeg_missing`-style codes.
pub fn run(bin: &Path, args: &[String], what: &str) -> GenResult<(bool, String, String)> {
    let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(1800))
        .stdout_cap(256 * 1024 * 1024)
        .stderr_cap(256 * 1024 * 1024);
    let out = rightkit_child_process::run(bin, args, &options).map_err(|e| {
        GenError::new(
            format!("{what}_missing"),
            format!("cannot run {}: {e}", bin.display()),
        )
    })?;
    if out.timed_out {
        return Err(GenError::new(
            format!("{what}_missing"),
            format!(
                "cannot run {}: child exceeded its time limit and was terminated",
                bin.display()
            ),
        ));
    }
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout.bytes).into_owned(),
        String::from_utf8_lossy(&out.stderr.bytes).into_owned(),
    ))
}

fn tool_version(bin: &Path, args: &[&str]) -> String {
    run(
        bin,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "tool",
    )
    .ok()
    .and_then(|(_, so, se)| {
        so.lines()
            .next()
            .map(String::from)
            .or_else(|| se.lines().next().map(String::from))
    })
    .unwrap_or_default()
    .chars()
    .take(120)
    .collect()
}

/// `studio.pipelines.media_status`: resolved tools + capability flags.
pub fn media_status() -> Value {
    let t = tools();
    let present = |p: &Option<PathBuf>| p.as_ref().map(|x| x.to_string_lossy().into_owned());
    json!({
        "ffmpeg": {
            "path": t.ffmpeg.as_ref().map(|h| h.ffmpeg_path().to_string_lossy().into_owned()),
            "trust": t.ffmpeg.as_ref().map(trust),
            "error": t.ffmpeg_error,
            "version": t.ffmpeg.as_ref().map(|h| tool_version(h.ffmpeg_path(), &["-version"])),
        },
        "ffprobe": {
            "path": t.ffprobe.as_ref().map(|h| h.ffmpeg_path().to_string_lossy().into_owned()),
            "trust": t.ffprobe.as_ref().map(trust),
            "error": t.ffprobe_error,
            "version": t.ffprobe.as_ref().map(|h| tool_version(h.ffmpeg_path(), &["-version"])),
        },
        "whisper": {
            "path": present(&t.whisper),
            "version": t.whisper.as_ref().map(|p| tool_version(p, &["--help"])),
        },
        "capabilities": {
            "compose": t.ffmpeg.is_some(),
            "probe": t.ffprobe.is_some() || t.ffmpeg.is_some(),
            "loudness": t.ffmpeg.is_some(),
            "transcribe": t.whisper.is_some(),
        },
    })
}

/// Capabilities of the resolved ffmpeg binary, probed once per binary path.
/// The bundled build is LGPL-only (`--disable-gpl --disable-nonfree`): it has
/// `h264_videotoolbox` and png/webp/mjpeg decoders but no `subtitles`/
/// `drawtext` filters (no libass), so burn-in falls back to a `mov_text`
/// soft track there while richer builds (e.g. a dev ffmpeg on PATH with
/// libass) can burn. Same contract as
/// `src-tauri/src/motion/encode.rs::pick_encoder`.
pub struct FfCaps {
    pub encoders: String,
    pub decoders: String,
    pub filters: String,
}

pub fn ffmpeg_caps(ffmpeg: &impl FfmpegBinary) -> FfCaps {
    let grab = |args: &[&str]| -> String {
        run(
            ffmpeg.ffmpeg_path(),
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "ffmpeg",
        )
        .map(|(_, so, _)| so)
        .unwrap_or_default()
    };
    FfCaps {
        encoders: grab(&["-hide_banner", "-encoders"]),
        decoders: grab(&["-hide_banner", "-decoders"]),
        filters: grab(&["-hide_banner", "-filters"]),
    }
}

impl FfCaps {
    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name))
    }
    pub fn has_filter(&self, name: &str) -> bool {
        self.filters
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name))
    }
    /// Video encoder + whether it is real H.264, for readiness checks: the shared rule
    /// (`rightkit_media_gen::host::h264`) with MPEG-4 counted as usable. Compose itself decides
    /// through the same rule and requires an explicit opt-in for MPEG-4 output.
    pub fn pick_video_encoder(&self) -> GenResult<(&'static str, bool)> {
        match h264::decide(&self.encoders, None, true) {
            h264::Pick::H264(name) => Ok((name, true)),
            h264::Pick::Mpeg4Fallback => Ok(("mpeg4", false)),
            h264::Pick::Unavailable => Err(GenError::new(h264::CODE, h264::unavailable_message())),
        }
    }
    /// Whether burned-in subtitles are possible (libass `subtitles` filter).
    pub fn can_burn_subtitles(&self) -> bool {
        self.has_filter("subtitles")
    }
    /// Whether a soft mov_text subtitle track can be muxed into MP4.
    pub fn can_mux_subtitles(&self) -> bool {
        self.has_encoder("mov_text") && (self.has_decoder("srt") || self.has_decoder("subrip"))
    }
    pub fn has_decoder(&self, name: &str) -> bool {
        self.decoders
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name))
    }
}

/// `studio.pipelines.probe`: streams/format via ffprobe (JSON) or `ffmpeg -i`
/// (stderr parse) when ffprobe is absent.
pub fn probe(path: &Path) -> GenResult<Value> {
    let t = tools();
    if let Some(ffprobe) = &t.ffprobe {
        let args = [
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([path.to_string_lossy().into_owned()])
        .collect::<Vec<_>>();
        let (ok, stdout, stderr) = run(ffprobe.ffmpeg_path(), &args, "ffprobe")?;
        if !ok {
            return Err(GenError::new(
                "probe_failed",
                format!(
                    "ffprobe failed on {}: {}",
                    path.display(),
                    stderr.trim().chars().take(400).collect::<String>()
                ),
            ));
        }
        let parsed: Value = serde_json::from_str(&stdout)
            .map_err(|e| GenError::new("probe_failed", format!("ffprobe JSON unreadable: {e}")))?;
        return Ok(summarize_probe(&parsed));
    }
    if let Some(ffmpeg) = &t.ffmpeg {
        let args = ["-hide_banner", "-i"]
            .iter()
            .map(|s| s.to_string())
            .chain([path.to_string_lossy().into_owned()])
            .collect::<Vec<_>>();
        let (_, _, stderr) = run(ffmpeg.ffmpeg_path(), &args, "ffmpeg")?;
        // ffmpeg -i exits nonzero; parse the stderr header.
        return Ok(parse_ffmpeg_header(&stderr, path));
    }
    Err(GenError::new(
        "tool_missing",
        format!(
            "no verified ffprobe or ffmpeg on this machine (ffprobe: {}; ffmpeg: {})",
            t.ffprobe_error.as_deref().unwrap_or("unresolved"),
            t.ffmpeg_error.as_deref().unwrap_or("unresolved")
        ),
    ))
}

fn summarize_probe(v: &Value) -> Value {
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video");
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    json!({
        "duration_s": v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()),
        "size_bytes": v["format"]["size"].as_str().and_then(|s| s.parse::<u64>().ok()),
        "video": video.map(|s| json!({
            "codec": s["codec_name"], "width": s["width"], "height": s["height"],
            "fps": s["avg_frame_rate"],
        })),
        "audio": audio.map(|s| json!({
            "codec": s["codec_name"], "sample_rate": s["sample_rate"], "channels": s["channels"],
        })),
        "streams": streams.len(),
    })
}

fn parse_ffmpeg_header(stderr: &str, path: &Path) -> Value {
    let mut duration: Option<f64> = None;
    let mut width: Option<i64> = None;
    let mut height: Option<i64> = None;
    let mut audio = false;
    for line in stderr.lines() {
        if let Some(i) = line.find("Duration:") {
            let t = line[i + 9..].trim_start();
            if let Some(end) = t.find(',') {
                duration = parse_ts(&t[..end]);
            }
        }
        if line.contains("Video:") {
            for part in line.split(',') {
                let p = part.trim();
                if let Some((w, h)) = p.split_once('x') {
                    if let (Ok(wi), Ok(hi)) = (w.trim().parse::<i64>(), h.trim().parse::<i64>()) {
                        if wi > 0 && hi > 0 && wi < 20000 {
                            width = Some(wi);
                            height = Some(hi);
                        }
                    }
                }
            }
        }
        if line.contains("Audio:") {
            audio = true;
        }
    }
    json!({
        "duration_s": duration,
        "video": width.map(|w| json!({ "width": w, "height": height })),
        "audio": audio.then_some(json!({})),
        "probe": "ffmpeg -i fallback",
        "path": path.to_string_lossy(),
    })
}

fn parse_ts(ts: &str) -> Option<f64> {
    let mut it = ts.trim().splitn(3, ':');
    let h: f64 = it.next()?.parse().ok()?;
    let m: f64 = it.next()?.parse().ok()?;
    let s: f64 = it.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Full-decode check: decode every packet, fail on any error line.
pub fn decode_check(ffmpeg: &impl FfmpegBinary, path: &Path) -> GenResult<bool> {
    let args = ["-v", "error", "-xerror", "-i"]
        .iter()
        .map(|s| s.to_string())
        .chain([
            path.to_string_lossy().into_owned(),
            "-f".into(),
            "null".into(),
            "-".into(),
        ])
        .collect::<Vec<_>>();
    let (ok, _, stderr) = run(ffmpeg.ffmpeg_path(), &args, "ffmpeg")?;
    Ok(ok && stderr.trim().is_empty())
}

/// EBU R128 integrated loudness via ffmpeg's `ebur128` filter.
pub fn loudness(ffmpeg: &impl FfmpegBinary, path: &Path) -> GenResult<Value> {
    let args = ["-hide_banner", "-nostats", "-i"]
        .iter()
        .map(|s| s.to_string())
        .chain([
            path.to_string_lossy().into_owned(),
            "-filter_complex".into(),
            "ebur128=peak=true".into(),
            "-f".into(),
            "null".into(),
            "-".into(),
        ])
        .collect::<Vec<_>>();
    let (_, _, stderr) = run(ffmpeg.ffmpeg_path(), &args, "ffmpeg")?;
    let mut integrated: Option<f64> = None;
    let mut lra: Option<f64> = None;
    let mut true_peak: Option<f64> = None;
    // The summary block is the last "I:"/"LRA:"/"Peak:" cluster in stderr.
    for line in stderr.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("I:").and_then(|r| {
            r.trim()
                .strip_suffix("LUFS")
                .map(str::trim)
                .or(Some(r.trim()))
        }) {
            if let Ok(x) = v.trim_end_matches(" LUFS").trim().parse::<f64>() {
                integrated = Some(x);
            }
        } else if l.starts_with("LRA:") {
            if let Ok(x) = l[4..].trim().trim_end_matches(" LU").trim().parse::<f64>() {
                lra = Some(x);
            }
        } else if l.starts_with("Peak:") || l.starts_with("True peak:") {
            let tail = l.split(':').last().unwrap_or("");
            if let Ok(x) = tail.trim().trim_end_matches(" dBFS").trim().parse::<f64>() {
                true_peak = Some(x);
            }
        }
    }
    match integrated {
        Some(i) => Ok(json!({ "integrated_lufs": i, "lra_lu": lra, "true_peak_dbfs": true_peak })),
        None => Err(GenError::new(
            "loudness_failed",
            "ebur128 produced no integrated loudness (no audio stream?)",
        )),
    }
}

/// Locate a whisper model: explicit path, `RIGHTKIT_WHISPER_MODEL`, then the largest
/// of the store-installed model and any `*.bin` in `<data_dir>/models/whisper`.
pub fn whisper_model(data_dir: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit.filter(|s| !s.is_empty()) {
        let pb = PathBuf::from(p);
        return pb.is_file().then_some(pb);
    }
    if let Some(p) = std::env::var_os("RIGHTKIT_WHISPER_MODEL") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    // The largest model wins, whether the store installed it or it was placed in the folder by hand.
    let dir = data_dir.join("models").join("whisper");
    let flat = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("bin"));
    // `max_by_key` keeps the last of equal sizes, so the store copy wins a tie with its flat original.
    flat.chain(super::asr::installed_whisper_model(data_dir))
        .max_by_key(|p| p.metadata().map(|m| m.len()).unwrap_or(0))
}
