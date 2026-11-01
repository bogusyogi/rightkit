//! Burn-in: cues -> `drawtext` filters, same approach as timeline titles.
//!
//! Text goes through content-addressed `textfile=` with `expansion=none`
//! (`caption-<blake3 16 hex>.txt`), cues show via `enable='between(n,a,b)'`
//! frame ranges using the timeline's single rounding rule
//! ([`Fps::ticks_to_frame`]), and layout is integer permille of the output size.
//! The caller writes every returned [`TextFile`] before running FFmpeg and
//! appends the filters to the composed video chain (after titles).

use rightkit_timeline::title::{escape_filter_path, ADVANCE_PERMILLE};
use rightkit_timeline::{Fps, Preset, TextFile, Ticks, TimelineError, TitleAssets};

use crate::font::FontFace;
use crate::segment::{config_for_preset, Cue, LineMetrics};

/// Caption box layout for a preset (permille of output width/height).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptionLayout {
    /// Font size, permille of output height.
    pub font: i64,
    /// Widest the box may be, permille of output width.
    pub max_width: i64,
    /// Box bottom edge, permille of output height.
    pub anchor: i64,
    /// Box opacity in percent.
    pub box_percent: i64,
}

impl CaptionLayout {
    pub fn for_preset(preset: Preset) -> Self {
        match preset {
            // Title-safe: box bottom at 90% of a 1080p frame.
            Preset::YouTube => Self {
                font: 40,
                max_width: 800,
                anchor: 900,
                box_percent: 60,
            },
            // Reels/Shorts overlay their own UI over the bottom ~22% and the
            // top ~12%: the box bottom stays at 76%, above that zone.
            // TikTok: bottom ~25% and the right action rail (x > 83%): narrower box,
            // bottom at 72%.
            Preset::TikTok => Self {
                font: 26,
                max_width: 660,
                anchor: 720,
                box_percent: 60,
            },
            Preset::Reels => Self {
                font: 26,
                max_width: 800,
                anchor: 760,
                box_percent: 60,
            },
            // Square 1:1 (R1): box bottom at 88%, width 80%.
            Preset::Square => Self {
                font: 30,
                max_width: 800,
                anchor: 880,
                box_percent: 60,
            },
        }
    }

    pub fn font_px(&self, h: i64) -> i64 {
        (h * self.font + 500) / 1000
    }

    pub fn pad(&self, h: i64) -> i64 {
        self.font_px(h) * 40 / 100
    }

    /// Longest line (in characters) that fits the box, for the monospace font.
    pub fn max_chars(&self, w: i64, h: i64) -> usize {
        let avail = w * self.max_width / 1000 - 2 * self.pad(h);
        (avail * 1000 / (self.font_px(h).max(1) * ADVANCE_PERMILLE)).max(1) as usize
    }
}

/// `drawtext` filters (cue order, none overlapping in frames) and the text files
/// they read. Cues starting at or after `total_frames` are skipped; a cue that
/// overlaps the next after frame rounding ends where the next one starts.
pub fn caption_filters(
    cues: &[Cue],
    preset: Preset,
    fps: Fps,
    total_frames: i64,
    assets: &TitleAssets<'_>,
) -> Result<(Vec<String>, Vec<TextFile>), TimelineError> {
    caption_filters_with_font(cues, preset, fps, total_frames, assets, None)
}

/// Burn-in wrapping with a known font; no face retains character-count layout.
pub fn caption_filters_with_font(
    cues: &[Cue],
    preset: Preset,
    fps: Fps,
    total_frames: i64,
    assets: &TitleAssets<'_>,
    face: Option<&FontFace>,
) -> Result<(Vec<String>, Vec<TextFile>), TimelineError> {
    let h = preset.height() as i64;
    let lay = CaptionLayout::for_preset(preset);
    let (font_px, pad) = (lay.font_px(h), lay.pad(h));
    let metrics = face.map(|face| LineMetrics {
        face,
        font_px: font_px as u32,
        max_width_px: (preset.width() as i64 * lay.max_width / 1000 - 2 * pad).max(0) as u64,
        uppercase: false,
    });
    let max_chars = config_for_preset(preset).max_chars_per_line;
    let font = escape_filter_path(assets.font)?;
    let y = format!("{}-text_h", h * lay.anchor / 1000 - pad);

    let frame = |ms: u64| fps.ticks_to_frame(Ticks::from_millis(ms as i64));
    let mut filters = Vec::new();
    let mut files: Vec<TextFile> = Vec::new();
    // Frame windows first; an overlapping earlier cue is cut back to where the
    // next one starts (dropping the later cue lost whole sentences when one
    // cue ran long), so cues never overlap and none is lost.
    let mut ordered: Vec<&Cue> = cues.iter().collect();
    ordered.sort_by_key(|c| (c.start_ms, c.end_ms));
    let mut spans: Vec<(i64, i64, String)> = Vec::new();
    for c in ordered {
        let content = if let Some(metrics) = metrics.as_ref() {
            let mut lines = Vec::new();
            for line in &c.lines {
                lines.extend(
                    metrics
                        .wrap(line, max_chars)
                        .map_err(TimelineError::Unsupported)?,
                );
            }
            lines.join("\n")
        } else {
            c.lines.join("\n")
        };
        if content.trim().is_empty() {
            continue;
        }
        let start = frame(c.start_ms);
        if start >= total_frames {
            continue;
        }
        let end = frame(c.end_ms).max(start + 1).min(total_frames); // exclusive
        if let Some(prev) = spans.last_mut() {
            prev.1 = prev.1.min(start);
        }
        spans.push((start, end, content));
    }
    spans.retain(|(s, e, _)| e > s);
    for (start, end, content) in spans {
        let hash = blake3::hash(content.as_bytes()).to_hex();
        let path = assets.text_dir.join(format!("caption-{}.txt", &hash[..16]));
        filters.push(format!(
            "drawtext=fontfile={font}:textfile={}:expansion=none:fontsize={font_px}:fontcolor=white:text_align=center:box=1:boxcolor=black@0.{:02}:boxborderw={pad}:x=(w-text_w)/2:y={y}:enable='between(n,{start},{})'",
            escape_filter_path(&path)?,
            lay.box_percent,
            end - 1,
        ));
        if !files.iter().any(|t| t.path == path) {
            files.push(TextFile { path, content });
        }
    }
    Ok((filters, files))
}
