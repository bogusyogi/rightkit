//! [DROP-IN CORE]
//! First-party result and intermediate types. These replace public dependence on
//! `parakeet_rs::transducer::{TimedToken, TranscriptionResult}`.

// One result type for the whole crate: the HeardRight core types are
// field-identical to the crate's `decoder` types, so they are the same type.
pub use crate::decoder::{TimedToken, TranscriptionResult};

/// Runtime-neutral token/piece emitted by a platform decoder loop.
///
/// `piece` is the raw vocabulary piece (for example `▁like`, `1`, or `.`).
/// `start`/`end` are already converted to seconds by the platform adapter so the
/// shared detokenizer never needs to know encoder stride, hop size, or sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPiece {
    pub token_id: usize,
    pub piece: String,
    pub start: f32,
    pub end: f32,
}
