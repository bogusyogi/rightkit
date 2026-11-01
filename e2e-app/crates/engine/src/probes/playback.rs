//! rightkit-playback: remux a real WAV into a playback-safe AAC .m4a through the catalog-verified FFmpeg, check the
//! derivative with ffprobe, prove the content-fingerprinted cache reuse, source immutability and cancellation, and
//! exercise the revocable source registry and the request-id registry.
//!
//! Needs: the catalog-verified ffmpeg and ffprobe resolved by `probes::ffmpeg` (see its header for where the crate
//! looks). The audio path uses FFmpeg's native `aac` encoder; the video remux path (stream copy) is not exercised
//! because the `decode` build has no H.264 encoder to make a fixture with.
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg::probe::run_ffprobe_json;
use rightkit_ffmpeg::{CancellationToken, FfmpegBinary};
use rightkit_playback::{
    compatibility_copy, MediaKind, PlaybackError, PlaybackRegistry, PlaybackSource, SourceRegistry,
};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let ffmpeg = super::ffmpeg::ffmpeg_handle()?;
    let ffprobe = super::ffmpeg::ffprobe_handle()?;
    let dir = ctx.dir("playback")?;
    let source = dir.join("source tone.wav");
    super::ffmpeg::render_sine(&ffmpeg, &source, 440, 2)?;
    let before = std::fs::read(&source).map_err(|e| format!("read source: {e}"))?;
    let cache = dir.join("cache");
    let token = CancellationToken::new();
    let bin = ffmpeg.ffmpeg_path();

    // The source registry hands the UI an id, never a path; a revoked id stops resolving.
    let mut sources = SourceRegistry::new();
    let id = sources.register(&source).map_err(|e| format!("register: {e}"))?;
    if sources.register(&source).map_err(|e| format!("re-register: {e}"))? != id {
        return Err("registering the same unchanged file gave a different id".into());
    }
    let resolved = sources.resolve(&id).map_err(|e| format!("resolve: {e}"))?.to_path_buf();
    if resolved != source.canonicalize().map_err(|e| format!("canonicalize: {e}"))? {
        return Err("resolve returned a different path".into());
    }
    if sources.resolve_ref("/definitely/not/registered.wav") != Err(PlaybackError::InvalidRequest) {
        return Err("an unregistered path must be InvalidRequest".into());
    }
    if sources.resolve_or_register(&source) != Err(PlaybackError::NotAllowed) && !sources.has_authoriser() {
        // The file is already explicitly registered, so it keeps its id; with no hook a new path is refused.
    }
    let other = dir.join("other.wav");
    std::fs::write(&other, b"RIFFxxxx").map_err(|e| format!("write other: {e}"))?;
    if sources.resolve_or_register(&other) != Err(PlaybackError::NotAllowed) {
        return Err("a path outside any authoriser must be NotAllowed".into());
    }

    // Remux through the real FFmpeg.
    let src = PlaybackSource::new(&resolved).with_kind(MediaKind::Audio);
    let derivative = compatibility_copy(bin, &src, &cache, &token).map_err(|e| format!("compatibility_copy: {e}"))?;
    if derivative.extension().and_then(|e| e.to_str()) != Some("m4a") || !derivative.starts_with(&cache) {
        return Err(format!("unexpected derivative {}", derivative.display()));
    }
    let bytes = derivative.metadata().map_err(|e| format!("stat derivative: {e}"))?.len();
    if bytes < 2_000 {
        return Err(format!("derivative is only {bytes} bytes"));
    }
    if std::fs::read(&source).map_err(|e| format!("re-read source: {e}"))? != before {
        return Err("the source file was modified".into());
    }
    let pj = run_ffprobe_json(&ffprobe, &derivative, Some(Duration::from_secs(30)), &CancellationToken::new())
        .map_err(|e| format!("ffprobe derivative: {e}"))?;
    let audio = pj.first_audio().ok_or("derivative has no audio stream")?;
    if audio.codec_name.as_deref() != Some("aac") {
        return Err(format!("derivative codec is {:?}, expected aac", audio.codec_name));
    }
    let dur = pj.format.duration_s.ok_or("derivative has no duration")?;
    if (dur - 2.0).abs() > 0.2 {
        return Err(format!("derivative lasts {dur}s, expected about 2s"));
    }

    // Second call reuses the cached derivative (same path, file untouched).
    let stamp = derivative.metadata().and_then(|m| m.modified()).map_err(|e| format!("mtime: {e}"))?;
    let again = compatibility_copy(bin, &src, &cache, &token).map_err(|e| format!("second copy: {e}"))?;
    let stamp2 = again.metadata().and_then(|m| m.modified()).map_err(|e| format!("mtime 2: {e}"))?;
    if again != derivative || stamp != stamp2 {
        return Err("the second call did not reuse the cached derivative".into());
    }

    // A cancelled token stops before spawn; a missing source is Missing.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    if compatibility_copy(bin, &src, &dir.join("cache2"), &cancelled) != Err(PlaybackError::Cancelled) {
        return Err("a cancelled token must give Cancelled".into());
    }
    let missing = PlaybackSource::new(dir.join("nope.wav")).with_kind(MediaKind::Audio);
    if compatibility_copy(bin, &missing, &cache, &token) != Err(PlaybackError::Missing) {
        return Err("a missing source must give Missing".into());
    }

    // Request-id registry: claim, busy on duplicate, release on drop, early cancel is remembered.
    let reg = Arc::new(PlaybackRegistry::new());
    let first = reg.begin("playback-a").map_err(|e| format!("begin: {e}"))?;
    if reg.begin("playback-a").err() != Some(PlaybackError::Busy) {
        return Err("a duplicate request id must be Busy".into());
    }
    reg.cancel("playback-a");
    if !first.token().is_cancelled() {
        return Err("cancel did not reach the active token".into());
    }
    drop(first);
    reg.begin("playback-a").map_err(|e| format!("id not released on drop: {e}"))?;
    reg.cancel("playback-early");
    if reg.begin("playback-early").err() != Some(PlaybackError::Cancelled) {
        return Err("a cancel before begin must be remembered".into());
    }
    if reg.begin("bad-id").err() != Some(PlaybackError::InvalidRequest) {
        return Err("an id without the playback- prefix must be InvalidRequest".into());
    }

    // Revoking a source makes its id unusable; the file stays.
    if !sources.revoke(&id) || sources.resolve(&id) != Err(PlaybackError::InvalidRequest) || !source.is_file() {
        return Err("revoke must drop the id and keep the file".into());
    }

    Ok(json!({
        "derivative": derivative.file_name().map(|n| n.to_string_lossy().into_owned()),
        "derivative_bytes": bytes,
        "duration_s": dur,
        "codec": audio.codec_name,
        "cache_reused": true,
    }))
}
