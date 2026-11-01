//! Captions for CutRight (media plan M2-T3).
//!
//! Pipeline: output-time [`TimedWord`]s -> [`build_cues`] ([`CaptionConfig`] per
//! preset) -> [`to_srt`] / [`to_vtt`] exports and [`caption_filters`] burn-in.
//!
//! - [`words`]: the input type and the transcript + time-map helper
//!   ([`words_from_transcript`]) that drops words inside removed ranges.
//! - [`segment`]: phrase grouping, splitting, merging and line breaking.
//! - [`export`]: SRT and WebVTT text.
//! - [`burn`]: `drawtext` filters + text files (the editing FFmpeg has no libass).
//! - [`style`], [`safezone`], [`styled`]: JSON style presets (outline/shadow/box,
//!   karaoke, cards, emphasis), per-platform safe zones, and their `drawtext` burn-in.
//! - [`align`]: [`check_alignment`], the word-boundary check the qualifier runs.

pub mod align;
pub mod burn;
pub mod export;
pub use rightkit_typeset::font;
pub mod ids;
pub mod placement;
pub mod safezone;
pub mod segment;
pub mod style;
pub mod styled;
pub mod words;

pub use align::{check_alignment, AlignmentIssue, ALIGNMENT_TOLERANCE_MS};
pub use burn::{caption_filters, CaptionLayout};
pub use export::{to_srt, to_vtt, Newline};
pub use font::{FontError, FontFace, Measurement};
pub use ids::{cue_id, cue_ids, cue_source_spans, disambiguate_ids};
pub use safezone::{Exclusion, Rect, SafeZone, SafeZoneCatalog};
pub use segment::{build_cues, config_for_preset, CaptionConfig, Cue};
pub use style::{CaptionStyle, Color, Orientation, StyleCatalog};
pub use styled::{
    build_styled_cues, cue_rect, max_box_width, place_box, style_config, styled_caption_filters,
    CaptionTarget, Emphasis, Geometry,
};
pub use words::{words_from_transcript, TimedWord};
