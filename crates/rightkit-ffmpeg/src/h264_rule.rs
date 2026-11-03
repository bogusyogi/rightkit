//! The one H.264 encoder rule for compose, Motion and lip-sync (donor: GenRight `genright-protocol/src/h264.rs`).
//!
//! The bundled ffmpeg is LGPL-only. It carries the platform hardware H.264 encoder
//! (VideoToolbox on macOS, Media Foundation on Windows) and no software H.264 encoder, by
//! RightKit licence policy. Every caller applies the same rule:
//!
//! 1. Use the platform hardware encoder when the located ffmpeg lists it. The software names
//!    below are used only when a development ffmpeg lists them; the bundled build never does.
//! 2. When no H.264 encoder is listed, or the encoder refuses to start (an encoder
//!    initialisation error in ffmpeg's own output), fail with `h264_encoder_unavailable` and
//!    its details. MPEG-4 is never produced silently.
//! 3. MPEG-4 is produced only when the caller passed `allow_mpeg4_fallback: true`. The result
//!    then reports `video_codec: "mpeg4"` and `fallback: true`.
//! 4. Every other failure, including a failed `-encoders` probe, passes through unchanged.
//!    It is never retried with another encoder and never reported as an encoder problem.

use std::path::Path;

use serde_json::{json, Value};

/// Typed error code for "no usable H.264 encoder".
pub const CODE: &str = "h264_encoder_unavailable";

/// Note attached to every result that used MPEG-4 because the caller asked for it.
pub const MPEG4_NOTE: &str = "MPEG-4 Part 2 video was produced because allow_mpeg4_fallback was set; some players and platforms will not accept MPEG-4 video";

/// Outcome of the one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Use this H.264 encoder.
    H264(&'static str),
    /// No H.264 encoder, and the caller asked for MPEG-4.
    Mpeg4Fallback,
    /// No H.264 encoder, and no MPEG-4 was requested (or none is listed).
    Unavailable,
}

/// The platform hardware H.264 encoder that the bundled build is expected to carry.
pub fn platform_encoder() -> &'static str {
    if cfg!(windows) {
        "h264_mf"
    } else if cfg!(target_os = "macos") {
        "h264_videotoolbox"
    } else {
        "h264_vaapi"
    }
}

/// Video encoder names in an `ffmpeg -encoders` listing, in listing order
/// (`crate::encoders::video_encoder_names`).
pub fn video_encoders(listing: &str) -> Vec<String> {
    crate::encoders::video_encoder_names(listing)
}

/// The single decision, on `crate::pick_h264`. `exclude` removes an H.264 encoder that
/// already refused to start.
pub fn decide(listing: &str, exclude: Option<&str>, allow_mpeg4: bool) -> Pick {
    let inventory = crate::EncoderInventory::parse(listing);
    let os = std::env::consts::OS;
    if let Some(name) = crate::pick_h264(&inventory, os, &mut |n| Some(n) != exclude) {
        return Pick::H264(name);
    }
    if allow_mpeg4 && inventory.has("mpeg4") {
        return Pick::Mpeg4Fallback;
    }
    Pick::Unavailable
}

/// Whether ffmpeg's own output shows that an encoder would not start. Only these messages
/// classify a failure as the encoder's; any other failure is passed through unchanged.
pub fn is_init_failure(ffmpeg_output: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "error while opening encoder",
        "unknown encoder",
        "encoder not found",
        "could not open encoder",
        "cannot open encoder",
        "vtcompressionsession",
    ];
    let text = ffmpeg_output.to_ascii_lowercase();
    MARKERS.iter().any(|marker| text.contains(marker))
}

/// The video encoders a located ffmpeg lists (`crate::EncoderInventory::probe`), as
/// listing rows (` V....D <name>`) in ffmpeg's own order. A failure here is a probe failure,
/// never an encoder finding.
pub fn probe_listing(ffmpeg: &impl crate::FfmpegBinary) -> Result<String, String> {
    let inventory = crate::EncoderInventory::probe(ffmpeg, std::time::Duration::from_secs(30))
        .map_err(|e| {
            format!(
                "cannot list encoders of {}: {e}",
                ffmpeg.ffmpeg_path().display()
            )
        })?;
    Ok(inventory
        .names()
        .iter()
        .map(|n| {
            format!(
                " V....D {n}
"
            )
        })
        .collect())
}

/// The last `max` characters of ffmpeg output, trimmed.
pub fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max {
        trimmed.to_string()
    } else {
        trimmed.chars().skip(count - max).collect()
    }
}

/// Human message for `h264_encoder_unavailable`; identical for every caller.
pub fn unavailable_message() -> String {
    format!(
        "no usable H.264 encoder ({}) in the resolved ffmpeg; set allow_mpeg4_fallback to accept MPEG-4 video",
        platform_encoder()
    )
}

/// Structured details for `h264_encoder_unavailable`; the same keys from every caller.
pub fn details(ffmpeg: Option<&Path>, listing: &str, ffmpeg_message: Option<&str>) -> Value {
    json!({
        "platform_encoder": platform_encoder(),
        "ffmpeg_path": ffmpeg.map(|p| p.to_string_lossy().into_owned()),
        "available_video_encoders": video_encoders(listing),
        "ffmpeg_message": ffmpeg_message.map(|m| tail(m, 900)),
    })
}
