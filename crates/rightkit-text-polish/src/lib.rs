//! Transcript polish for dictation apps.
//!
//! - [`text_pipeline`]: deterministic local (L0) cleanup of a finished transcript (inline edits,
//!   repetition collapse, spoken formatting, numbers, dates, units, casing, a proper-noun
//!   lexicon, optional host-supplied brand homophone repair, visual chunking) and parsing of
//!   spoken control tails and AI-transform commands.
//! - [`vocabulary`]: user vocabulary terms and replacement rules, applied after polish.
//! - [`vocab_learn`]: learns vocabulary suggestions from the user's own corrections.
//!
//! Spoken standalone-command recognition lives in `rightkit-voice-commands`; this crate uses it
//! only to keep visual chunking off command-shaped text.

pub mod text_pipeline;
pub mod vocab_learn;
pub mod vocabulary;
