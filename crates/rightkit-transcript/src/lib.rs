//! `rightkit-transcript`: the shared word-timed transcript contract.
//!
//! Pipeline: producer (ASR, TTS manifest, SRT/VTT file) -> [`Transcript`] ->
//! [`build_cues`] ([`CaptionConfig`]) -> [`to_srt`] / [`to_vtt`] / timing JSON.
//!
//! - [`contract`]: `rightkit.transcript.v1` (reads `cutright.transcript.v1`).
//! - [`words`]: [`TimedWord`] normalisation and source -> output time mapping.
//! - [`segment`]: deterministic cue segmentation with named profiles.
//! - [`export`]: SRT, WebVTT, plain text, and the shared timing JSON.
//! - [`parse`]: SRT/WebVTT/timing-JSON import (cues -> interpolated words).
//! - [`align`]: cue-edge vs word-boundary check used by qualifiers.
//!
//! All times are integer milliseconds. No audio, no models, no I/O here:
//! engines hand in `(text, start, end)` and apps write the strings out.

pub mod align;
pub mod contract;
pub mod export;
pub mod parse;
pub mod segment;
pub mod words;

pub use align::{check_alignment, AlignmentIssue, ALIGNMENT_TOLERANCE_MS};
pub use contract::{
    EndSource, ModelRef, Span, Transcript, TranscriptError, Word, LEGACY_SCHEMA, SCHEMA,
};
pub use export::{to_plain_text, to_srt, to_timing_json, to_vtt, Newline};
pub use parse::{parse_srt, parse_timing_json, parse_vtt, words_from_cues, ParseError, ParsedCue};
pub use segment::{build_cues, normalize_words, CaptionConfig, Cue, Profile};
pub use words::{words_from_seconds, words_from_transcript, TimedWord, MAP_DURATION_TOLERANCE_MS};
