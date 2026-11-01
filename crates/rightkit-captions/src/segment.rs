//! CutRight preset policy around RightKit cue segmentation: per-preset limits, and the font
//! that supplies measured widths. Timing, phrase grouping, splitting, merging, line breaking
//! and overlap normalisation are `rightkit-transcript`'s.

use rightkit_timeline::Preset;

use crate::font::FontFace;
use crate::words::TimedWord;

pub use rightkit_transcript::{normalize_words, CaptionConfig, Cue, Profile};

/// Segmentation limits for a delivery preset: CutRight's choice of RightKit profile.
pub fn config_for_preset(preset: Preset) -> CaptionConfig {
    CaptionConfig::for_profile(match preset {
        Preset::YouTube => Profile::Standard,
        Preset::TikTok => Profile::Vertical,
        Preset::Reels | Preset::Square => Profile::Compact,
    })
}

/// Font and usable text width for deterministic pixel-based line breaking. RightKit takes the
/// width as a callback; this is the face and size behind it.
#[derive(Debug, Clone, Copy)]
pub struct LineMetrics<'a> {
    pub face: &'a FontFace,
    pub font_px: u32,
    pub max_width_px: u64,
    /// Measure the rendered case while keeping transcript text unchanged.
    pub uppercase: bool,
}

impl LineMetrics<'_> {
    fn width(&self, text: &str) -> Result<u64, String> {
        self.face
            .measure(text)
            .map(|m| m.width_px(self.font_px))
            .map_err(|e| e.to_string())
    }

    /// Runs `f` with RightKit's metrics view over this font. RightKit applies `uppercase`
    /// itself before measuring, so the callback measures the text it is given.
    fn with_shared<R>(&self, f: impl FnOnce(&rightkit_transcript::LineMetrics<'_>) -> R) -> R {
        let measure = |text: &str| self.width(text);
        f(&rightkit_transcript::LineMetrics {
            measure_width: &measure,
            max_width_px: self.max_width_px,
            uppercase: self.uppercase,
        })
    }

    /// Wraps `text` to `max_chars` and the measured width (RightKit's `wrap_lines`).
    pub(crate) fn wrap(&self, text: &str, max_chars: usize) -> Result<Vec<String>, String> {
        self.with_shared(|shared| rightkit_transcript::wrap_lines(text, max_chars, Some(shared)))
    }
}

/// Build cues using RightKit's timing, phrase grouping, splitting & merging.
pub fn build_cues(words: &[TimedWord], cfg: &CaptionConfig) -> Vec<Cue> {
    rightkit_transcript::build_cues(words, cfg)
}

/// Builds cues through RightKit; with a font, measured widths drive splitting, merging and
/// line balancing (a cue never silently exceeds the safe width).
pub fn build_cues_with_metrics(
    words: &[TimedWord],
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<Vec<Cue>, String> {
    match metrics {
        Some(metrics) => metrics.with_shared(|shared| {
            rightkit_transcript::build_cues_with_metrics(words, cfg, Some(shared))
        }),
        None => rightkit_transcript::build_cues_with_metrics(words, cfg, None),
    }
}
