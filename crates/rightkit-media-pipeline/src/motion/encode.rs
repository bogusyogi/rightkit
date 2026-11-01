//! The host side of Motion's ffmpeg use: which ffmpeg runs (the staged LGPL build, trust class), the
//! shared `h264_encoder_unavailable` error shape, and the encoder JSON the Motion view reads. The
//! render and encode driver, encoder pick, quality maps, alpha encoders and PNG frame conversion are
//! `rightkit-motion` (feature `encode`); nothing here builds an ffmpeg command or converts a frame.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rightkit_ffmpeg::{EncoderInventory, FfmpegBinary};
use rightkit_media_gen::host::ffmpeg::FfmpegHandle;
use rightkit_media_gen::host::h264;
use rightkit_motion::{EncoderKind, RenderError};
use serde::Serialize;

/// The encoder as the Motion view and the run manifest show it. `h264` is the view's "plays in the
/// in-app player" flag; the owner's `Encoder` carries the same fact as `kind`.
#[derive(Debug, Clone, Serialize)]
pub struct EncoderView {
    pub name: &'static str,
    pub codec: &'static str,
    pub h264: bool,
    /// True only when MPEG-4 was requested with `allow_mpeg4_fallback`.
    pub fallback: bool,
}

impl From<&rightkit_motion::Encoder> for EncoderView {
    fn from(e: &rightkit_motion::Encoder) -> Self {
        Self {
            name: e.name,
            codec: e.codec,
            h264: e.kind == EncoderKind::H264,
            fallback: e.fallback,
        }
    }
}

/// The alpha output named by a request: `prores` or `vp9` (and their aliases).
pub fn parse_alpha(name: &str) -> Option<EncoderKind> {
    match name.to_ascii_lowercase().as_str() {
        "prores" | "prores4444" | "prores_4444" | "mov" => Some(EncoderKind::ProRes4444),
        "vp9" | "webm" => Some(EncoderKind::Vp9Alpha),
        _ => None,
    }
}

/// The pixel format an output of `kind` is encoded in (reported in the result).
pub fn pixel_format(kind: Option<EncoderKind>) -> &'static str {
    match kind {
        Some(EncoderKind::ProRes4444) => "yuva444p10le",
        Some(EncoderKind::Vp9Alpha) => "yuva420p",
        _ => "yuv420p",
    }
}

/// The owner's handle for the located ffmpeg (an `FfmpegBinary`: the encoder probe and the runner take
/// it, and it keeps the verified/development trust class).
pub fn ffmpeg_bin() -> Result<FfmpegHandle, String> {
    rightkit_media_gen::host::ffmpeg::resolve_ffmpeg()
        .map_err(|e| format!("ffmpeg_missing: no verified ffmpeg (bundled; RIGHTKIT_FFMPEG or PATH in development builds): {e}"))
}

/// The located ffmpeg's path, for messages and status only; spawns take [`ffmpeg_bin`].
pub fn ffmpeg_path() -> Result<PathBuf, String> {
    ffmpeg_bin().map(|h| h.ffmpeg_path().to_path_buf())
}

/// `verified` or `development` (the owner's trust class of the located ffmpeg); `None` when none resolved.
pub fn ffmpeg_trust() -> Option<&'static str> {
    ffmpeg_bin()
        .ok()
        .map(|h| rightkit_media_gen::host::ffmpeg::trust(&h))
}

/// The video encoders of the located ffmpeg, cached once a probe succeeds. A failed probe is reported
/// as itself, never as a missing encoder.
pub fn inventory() -> Result<&'static EncoderInventory, String> {
    static CACHE: OnceLock<EncoderInventory> = OnceLock::new();
    if let Some(found) = CACHE.get() {
        return Ok(found);
    }
    let bin = ffmpeg_bin()?;
    let probed = rightkit_motion::probe_inventory(&bin, Duration::from_secs(30)).map_err(|e| {
        format!(
            "{e} (cannot list encoders of {})",
            bin.ffmpeg_path().display()
        )
    })?;
    Ok(CACHE.get_or_init(|| probed))
}

/// The encoder for an opaque render. `allow_mpeg4` is the request's `allow_mpeg4_fallback`.
pub fn pick_encoder(allow_mpeg4: bool) -> Result<rightkit_motion::Encoder, String> {
    let path = ffmpeg_path()?;
    let found = inventory()?;
    rightkit_motion::pick_encoder(found, std::env::consts::OS, allow_mpeg4, &mut |_| true)
        .map_err(|e| render_error(e, Some(path.as_path())))
}

/// The alpha encoder for `kind`, or the owner's `alpha_encoder_unavailable`.
pub fn pick_alpha_encoder(kind: EncoderKind) -> Result<rightkit_motion::Encoder, String> {
    rightkit_motion::pick_alpha_encoder(inventory()?, kind).map_err(|e| render_error(e, None))
}

/// Listing rows for the shared `h264_encoder_unavailable` details (`h264::details` takes listing text).
fn listing_rows(names: &[String]) -> String {
    names.iter().map(|n| format!(" V....D {n}\n")).collect()
}

/// `h264_encoder_unavailable: <message>; details=<json>`. Motion reports errors as strings, so
/// the typed code leads and the structured details follow as JSON.
pub fn unavailable_error(
    ffmpeg: Option<&Path>,
    listing: &str,
    ffmpeg_message: Option<&str>,
) -> String {
    format!(
        "{}: {}; details={}",
        h264::CODE,
        h264::unavailable_message(),
        h264::details(ffmpeg, listing, ffmpeg_message)
    )
}

/// A `rightkit-motion` error as Motion's `code: detail` string. The H.264 and MPEG-4 refusal keeps
/// The shared `h264_encoder_unavailable` shape (with its JSON details); everything else is the
/// owner's own message, whose prefix is the stable code.
pub fn render_error(error: RenderError, ffmpeg: Option<&Path>) -> String {
    match error {
        RenderError::EncoderUnavailable {
            kind: EncoderKind::H264 | EncoderKind::Mpeg4,
            listed,
            ffmpeg_message,
        } => {
            let path = ffmpeg.map(Path::to_path_buf).or_else(|| ffmpeg_path().ok());
            let listing = if listed.is_empty() {
                inventory()
                    .map(|i| listing_rows(i.names()))
                    .unwrap_or_default()
            } else {
                listing_rows(&listed)
            };
            unavailable_error(path.as_deref(), &listing, ffmpeg_message.as_deref())
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_format_names_parse() {
        assert_eq!(parse_alpha("ProRes"), Some(EncoderKind::ProRes4444));
        assert_eq!(parse_alpha("webm"), Some(EncoderKind::Vp9Alpha));
        assert_eq!(parse_alpha("gif"), None);
    }

    #[test]
    fn the_unavailable_error_leads_with_the_typed_code() {
        let e = unavailable_error(None, "", Some("init failed"));
        assert!(e.starts_with("h264_encoder_unavailable: "), "{e}");
    }

    #[test]
    fn an_h264_refusal_keeps_the_shared_code_and_details() {
        let e = render_error(
            RenderError::EncoderUnavailable {
                kind: EncoderKind::H264,
                listed: vec!["mpeg4".into()],
                ffmpeg_message: Some("init failed".into()),
            },
            None,
        );
        assert!(
            e.starts_with("h264_encoder_unavailable: ") && e.contains("details="),
            "{e}"
        );
        let alpha = render_error(
            RenderError::EncoderUnavailable {
                kind: EncoderKind::ProRes4444,
                listed: vec![],
                ffmpeg_message: None,
            },
            None,
        );
        assert!(alpha.starts_with("alpha_encoder_unavailable"), "{alpha}");
    }

    #[test]
    fn the_view_flags_only_h264() {
        assert!(EncoderView::from(&rightkit_motion::Encoder::mpeg4_fallback()).fallback);
        assert!(!EncoderView::from(&rightkit_motion::Encoder::mpeg4_fallback()).h264);
        assert_eq!(pixel_format(Some(EncoderKind::Vp9Alpha)), "yuva420p");
        assert_eq!(pixel_format(None), "yuv420p");
    }
}
