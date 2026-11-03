//! ONNX Runtime execution for Parakeet TDT (feature `onnx-tdt`).
//!
//! Written against the model's own tensor contract — three sessions, six input
//! names, five output names — for one model family down one path: a
//! dictation runtime that owns its own decode loop.
//!
//! Deliberately contains NO decode policy. Search, scoring, the emission
//! reward, phrase bias and detokenization live in `crate::parakeet_core`.
//! This module returns logits and states; what to do with them is decided
//! by the caller.
//!
//! ## The contract
//!
//! ```text
//! mel_nemo.onnx        audio [1, samples]                  -> mel [1, feat, frames]
//! encoder-model.onnx   audio_signal [1, feat, frames]      -> outputs [1, enc_dim, T]
//!                      length [1]                             encoded_lengths [1]
//! decoder_joint.onnx   encoder_outputs [1, enc_dim, 1]     -> outputs [.. logits ..]
//!                      targets [1, 1]                         output_states_1
//!                      target_length [1]                      output_states_2
//!                      input_states_1 / input_states_2 [2, 1, 640]
//! ```
//!
//! Joint logits are laid out `[vocab_size token logits .. n duration logits]`.
//! `n_durations` is deliberately not surfaced as a constant: callers derive it
//! as `logits.len() - vocab_size`, so the tensor layout has one source of truth.

#[cfg(target_os = "windows")]
mod environment;
mod model;
mod session;

#[cfg(target_os = "windows")]
pub use environment::init_windows_ort;

pub use model::{
    encoder_pass_age, take_batched_run_stats, EncoderOutput, MelFeatures, OnnxAsr, OnnxAsrConfig,
};
pub use session::{ExecutionProvider, SessionOptions};

/// Errors this crate can produce. Deliberately small: everything here is either
/// a missing or malformed model artifact, or an ORT failure.
#[derive(Debug)]
pub enum Error {
    /// A required file was absent from the model directory.
    MissingArtifact(String),
    /// `config.json` or `vocab.txt` could not be parsed.
    Malformed(String),
    /// ONNX Runtime failed to build a session or execute one.
    Runtime(String),
    /// A tensor came back with a shape the contract above does not allow.
    Shape(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::MissingArtifact(m) => write!(f, "missing model artifact: {m}"),
            Error::Malformed(m) => write!(f, "malformed model metadata: {m}"),
            Error::Runtime(m) => write!(f, "onnx runtime: {m}"),
            Error::Shape(m) => write!(f, "unexpected tensor shape: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
