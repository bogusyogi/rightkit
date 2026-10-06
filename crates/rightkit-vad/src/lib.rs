//! # rightkit-vad
//!
//! Product-neutral voice activity detection. Owns the Silero scorer
//! (caller-supplied ONNX model path; nothing is bundled or downloaded), the
//! frame protocol, and the span/chunking policy. Owns no dictation, push-to-talk
//! or command policy: callers decide what a span means.
//!
//! Layers, each usable alone:
//! - [`FrameScorer`]: any per-frame speech-probability source ([`Silero`] or a test double).
//! - [`scores_to_spans`]: Silero `get_speech_timestamps`-shaped hysteresis
//!   (`threshold` start, `neg_threshold` end, `min_speech`, `min_silence`,
//!   `pad`, merge).
//! - [`chunk_spans`]: split/merge spans into decode-sized chunks
//!   (`max_chunk_ms`, `min_chunk_ms`, `merge_gap_ms`).
//! - [`FrameStream`]: lossless streaming carry. Capture callbacks of any size
//!   in, whole 512-sample frames out; the remainder is retained, never
//!   dropped, because Silero is recurrent. A scorer failure yields `None`
//!   ("unknown"), never silence.

mod silero;
mod spans;
mod stream;

pub use silero::{Silero, SileroOptions};
pub use spans::{
    chunk_spans, detect, energy_scores, scores_to_spans, ChunkConfig, Span, VadConfig,
};
pub use stream::{FrameStream, FrameVerdicts};

/// Silero v5 16 kHz frame size (32 ms).
pub const FRAME_SAMPLES: usize = 512;
/// Frame duration in milliseconds.
pub const FRAME_MS: u64 = 32;
/// Required sample rate.
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub enum Error {
    Model(String),
    Input(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Model(m) => write!(f, "vad model: {m}"),
            Error::Input(m) => write!(f, "vad input: {m}"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Per-frame speech probability source. Stateful (recurrent) scorers must
/// honour `reset`.
pub trait FrameScorer {
    fn reset(&mut self);
    /// Probability in `[0,1]` for one 512-sample 16 kHz mono frame.
    fn score(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<f32>;
}
