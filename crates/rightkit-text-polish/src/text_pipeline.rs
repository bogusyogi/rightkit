//! Deterministic transcript polish and spoken-command parsing.
//!
//! Pure text in, text out: no models, no network. The entry points are
//! [`deterministic_polish`] (and its brand-aware and tail variants),
//! [`parse_control_command`] and [`parse_ai_transform_command`].

mod control;
mod inline;
mod normalize;
mod number_words;
mod polish;

pub use control::{
    has_trailing_control_wake, is_bare_summarize_trigger, looks_like_wake_word,
    parse_ai_transform_command, parse_control_command, parse_selected_text_ai_transform_command,
    parse_timed_ai_transform_command, scrub_command_tokens, strip_fired_control_tail,
    strip_kws_onset_cut_residue, AiTransformCommand, AiTransformIntent, ControlCommand,
    ControlIntent, TimedToken,
};
pub use inline::{
    aggressive_speech_cleanup, apply_inline_edits, apply_inline_formatting, expand_snippets,
    tighten_domain_spacing,
};
pub use polish::{
    apply_brand_homophones, apply_replacements, deterministic_polish, deterministic_polish_tail,
    deterministic_polish_tail_with_brand, deterministic_polish_with_brand,
    restore_vocabulary_casing, visual_chunk_transcript, BrandHomophones, HomophoneGuard,
};
