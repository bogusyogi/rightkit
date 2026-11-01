//! rightkit-ffmpeg: resolve the catalog-verified FFmpeg and ffprobe the way a shipped app does (bundled beside the
//! engine executable, verified against the approved `decode` row: content digest + LGPL build probe), then run them:
//! render a real WAV, probe it, measure loudness, and prove the runner's timeout kills a runaway child.
//!
//! Needs: ffmpeg and ffprobe staged for this host and byte-identical to the approved `decode` catalog rows of
//! `crates/rightkit-ffmpeg/catalog/artifacts.json`. The crate looks (first hit wins) at, in the engine executable's
//! directory: `ffmpeg[.exe]`, `runtime/ffmpeg[.exe]`, `resources/runtime/ffmpeg[.exe]` (macOS also
//! `../Resources/runtime/...`), then `src-tauri/binaries/ffmpeg-<target-triple>[.exe]` in the directory and each
//! ancestor. The build is Release mode: an unverified binary is refused, never used.
use std::path::Path;
use std::time::{Duration, Instant};

use rightkit_ffmpeg::loudnorm::{measure_loudness, LoudnessTarget};
use rightkit_ffmpeg::probe::{probe_media, run_ffprobe_json};
use rightkit_ffmpeg::{
    resolve_ffmpeg_handle_with, resolve_ffprobe_handle_with, run_checked, run_capture, BuildMode, CancellationToken,
    FfmpegBinary, FfmpegCommand, FfmpegHandle, FfprobeHandle, ResolveOptions, RunError, Tool, ToolHandle,
    UnverifiedRequest,
};
use serde_json::{json, Value};

const VARIANT: &str = "decode";
const T: Duration = Duration::from_secs(60);

fn where_to_stage(tool: &str) -> String {
    format!(
        "needs a catalog-verified {tool} ({VARIANT} variant, approved row for this host) beside the engine executable \
         ({tool}[.exe], runtime/{tool}[.exe] or resources/runtime/{tool}[.exe]) or at src-tauri/binaries/{tool}-<target-triple>[.exe] \
         in the executable's directory or an ancestor; stage it from the rightkit-ffmpeg catalog archive via the gate's sidecar sources"
    )
}

fn exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("current_exe: {e}"))
}

/// The bundled, catalog-verified ffmpeg (shared with the audio, playback and asr probes). Release mode: a binary
/// that is not the approved build is an error, never a development fallback.
pub fn ffmpeg_handle() -> Result<FfmpegHandle, String> {
    let handle = resolve_ffmpeg_handle_with(
        &exe()?,
        VARIANT,
        &UnverifiedRequest::new(Tool::Ffmpeg),
        BuildMode::Release,
        &ResolveOptions::runtime_layout(),
        T,
    )
    .map_err(|e| format!("{e}; {}", where_to_stage("ffmpeg")))?
    .handle;
    if !handle.is_verified() {
        return Err("ffmpeg resolved but is not catalog-verified".into());
    }
    Ok(handle)
}

pub fn ffprobe_handle() -> Result<FfprobeHandle, String> {
    let handle = resolve_ffprobe_handle_with(
        &exe()?,
        VARIANT,
        &UnverifiedRequest::new(Tool::Ffprobe),
        BuildMode::Release,
        &ResolveOptions::runtime_layout(),
        T,
    )
    .map_err(|e| format!("{e}; {}", where_to_stage("ffprobe")))?
    .handle;
    if !handle.is_verified() {
        return Err("ffprobe resolved but is not catalog-verified".into());
    }
    Ok(handle)
}

fn artifact_id_ffmpeg(h: &FfmpegHandle) -> String {
    match h {
        ToolHandle::Verified(v) => v.artifact_id().to_string(),
        ToolHandle::Development(_) => "development".into(),
    }
}

fn artifact_id_ffprobe(h: &FfprobeHandle) -> String {
    match h {
        ToolHandle::Verified(v) => v.artifact_id().to_string(),
        ToolHandle::Development(_) => "development".into(),
    }
}

/// Render `seconds` of a mono 48 kHz 16-bit sine with the real ffmpeg.
pub fn render_sine(bin: &dyn FfmpegBinary, out: &Path, hz: u32, seconds: u32) -> Result<(), String> {
    let mut cmd = FfmpegCommand::base("error");
    cmd.texts([
        "-f".to_string(),
        "lavfi".into(),
        "-i".into(),
        format!("sine=frequency={hz}:sample_rate=48000:duration={seconds}"),
        "-ac".into(),
        "1".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
    ]);
    cmd.path(out);
    run_checked(bin, &cmd, T, &CancellationToken::new()).map_err(|e| format!("render sine: {e}"))?;
    Ok(())
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let ffmpeg = ffmpeg_handle()?;
    let ffprobe = ffprobe_handle()?;
    let dir = ctx.dir("ffmpeg")?;
    let wav = dir.join("tone.wav");
    render_sine(&ffmpeg, &wav, 440, 2)?;
    let size = std::fs::metadata(&wav).map_err(|e| format!("stat {}: {e}", wav.display()))?.len();
    // 2 s * 48000 * 2 bytes plus the header.
    if !(190_000..=200_000).contains(&size) {
        return Err(format!("rendered WAV is {size} bytes, expected about 192044"));
    }

    // ffprobe JSON: exact stream facts.
    let cancel = CancellationToken::new();
    let pj = run_ffprobe_json(&ffprobe, &wav, Some(T), &cancel).map_err(|e| format!("ffprobe: {e}"))?;
    let audio = pj.first_audio().ok_or("ffprobe found no audio stream")?;
    if audio.codec_name.as_deref() != Some("pcm_s16le") || audio.sample_rate != Some(48_000) || audio.channels != Some(1) {
        return Err(format!(
            "ffprobe stream mismatch: codec {:?} rate {:?} channels {:?}",
            audio.codec_name, audio.sample_rate, audio.channels
        ));
    }
    let dur = pj.format.duration_s.ok_or("ffprobe gave no duration")?;
    if (dur - 2.0).abs() > 0.05 {
        return Err(format!("ffprobe duration {dur}s, expected 2.0s"));
    }

    // ffmpeg -i parse agrees.
    let mp = probe_media(&ffmpeg, &wav, &cancel).map_err(|e| format!("probe_media: {e}"))?;
    let ms = mp.duration_ms.ok_or("probe_media gave no duration")?;
    if ms.abs_diff(2000) > 60 {
        return Err(format!("probe_media duration {ms} ms, expected 2000"));
    }

    // Loudness: a full-scale-ish 440 Hz sine measures a finite, plausible level.
    let m = measure_loudness(&ffmpeg, &wav, &LoudnessTarget::default(), T, &cancel).map_err(|e| format!("loudnorm: {e}"))?;
    if !m.input_i.is_finite() || !(-30.0..=0.0).contains(&m.input_i) {
        return Err(format!("integrated loudness {} LUFS is not plausible for a sine", m.input_i));
    }

    // The runner owns the child: a runaway render hits its timeout and is killed, not left running.
    let mut long = FfmpegCommand::base("error");
    long.texts(["-f", "lavfi", "-i", "sine=frequency=220:sample_rate=48000:duration=3600", "-f", "null", "-"]);
    let started = Instant::now();
    match run_capture(&ffmpeg, &long, Duration::from_secs(2), &CancellationToken::new()) {
        Err(RunError::TimedOut(_)) => {}
        other => return Err(format!("a 1 hour render under a 2 s timeout must be TimedOut, got {:?}", other.map(|c| c.status))),
    }
    let waited = started.elapsed();
    if waited > Duration::from_secs(15) {
        return Err(format!("the timed-out child took {waited:?} to stop"));
    }

    // Cancellation before start is refused without spawning.
    let token = CancellationToken::new();
    token.cancel();
    match run_capture(&ffmpeg, &long, T, &token) {
        Err(RunError::Cancelled) => {}
        other => return Err(format!("a cancelled token must give Cancelled, got {:?}", other.map(|c| c.status))),
    }

    Ok(json!({
        "ffmpeg_artifact": artifact_id_ffmpeg(&ffmpeg),
        "ffmpeg_path": ffmpeg.ffmpeg_path().display().to_string(),
        "ffprobe_artifact": artifact_id_ffprobe(&ffprobe),
        "wav_bytes": size,
        "duration_s": dur,
        "integrated_lufs": m.input_i,
        "timeout_stop_ms": waited.as_millis() as u64,
    }))
}
