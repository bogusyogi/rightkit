//! Input colour-space detection (port of `video-media/src/color.rs`: `ColorSpaceKind`,
//! `SourceColorTags`, `detect_color_space`, `probe_source_color_tags`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use rightkit_ffmpeg::{probe::parse_ffprobe_json, FfmpegCommand};

use crate::{run_media_capture, ColorError};

/// The classified input colour space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpaceKind {
    /// SDR Rec.709: the working and delivery space.
    Rec709,
    /// HLG (`arib-std-b67`) HDR.
    Rec2020Hlg,
    /// PQ (`smpte2084`) HDR.
    Rec2020Pq,
    /// Apple Log (iPhone 15 Pro+). FFmpeg has no transfer enum for it: clips are tagged
    /// `bt2020` primaries with no explicit transfer, so this is a heuristic; callers with
    /// better knowledge (user choice, container metadata) pass the kind explicitly.
    AppleLog,
    /// Tags present but not recognised: treated as SDR pass-through, never guessed at.
    Unknown,
}

impl ColorSpaceKind {
    /// Whether converting this kind to SDR needs FFmpeg's `zscale` filter (PQ and HLG only;
    /// Apple Log goes through `scale` + `lut3d`).
    pub fn needs_zscale(self) -> bool {
        matches!(self, ColorSpaceKind::Rec2020Pq | ColorSpaceKind::Rec2020Hlg)
    }
}

/// Raw colour tags as ffprobe reports them (`unknown` and empty are normalised to `None`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceColorTags {
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub color_matrix: Option<String>,
    /// `tv` / `pc` when tagged.
    pub color_range: Option<String>,
}

/// Classify tags. Pure; unit-tested against fixture tag combinations.
pub fn detect_color_space(tags: &SourceColorTags) -> ColorSpaceKind {
    match tags.color_transfer.as_deref() {
        Some("smpte2084") => ColorSpaceKind::Rec2020Pq,
        Some("arib-std-b67") => ColorSpaceKind::Rec2020Hlg,
        Some("bt709") | Some("smpte170m") | Some("bt470bg") | Some("iec61966-2-1") => {
            ColorSpaceKind::Rec709
        }
        None if tags.color_primaries.as_deref() == Some("bt2020") => ColorSpaceKind::AppleLog,
        _ => ColorSpaceKind::Unknown,
    }
}

/// Parse `ffprobe -select_streams v:0 -show_entries stream=... -of json` output.
pub fn parse_ffprobe_color_json(json: &str) -> Result<SourceColorTags, ColorError> {
    let probe = parse_ffprobe_json(json).map_err(|e| ColorError::Parse(e.to_string()))?;
    let Some(stream) = probe.streams.into_iter().next() else {
        return Ok(SourceColorTags::default());
    };
    let norm = |tag: Option<String>| tag.filter(|s| !s.is_empty() && s != "unknown");
    Ok(SourceColorTags {
        color_transfer: norm(stream.color_transfer),
        color_primaries: norm(stream.color_primaries),
        color_matrix: norm(stream.color_space),
        color_range: norm(stream.color_range),
    })
}

/// Probe the first video stream's colour tags with ffprobe.
pub fn probe_color_tags(ffprobe: &Path, media: &Path) -> Result<SourceColorTags, ColorError> {
    if !media.is_file() {
        return Err(ColorError::Process(format!(
            "input does not exist: {}",
            media.display()
        )));
    }
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .text("stream=color_transfer,color_primaries,color_space,color_range")
        .texts(["-print_format", "json"])
        .path(media);
    let out = run_media_capture(ffprobe, &cmd)?;
    if !out.status.success() {
        return Err(ColorError::Process(out.stderr.trim().to_string()));
    }
    parse_ffprobe_color_json(&String::from_utf8_lossy(&out.stdout))
}
