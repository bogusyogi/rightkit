//! Native WhisperKit-format Whisper on CoreML (macOS; ANE/GPU/CPU).
//!
//! Runs the precompiled `AudioEncoder.mlmodelc` + `TextDecoder.mlmodelc`
//! in-process. The mel front-end is computed in Rust (canonical openai/whisper
//! log-mel), not by the WhisperKit MelSpectrogram stage.
//!
//! Donor: HeardRight `heardright-inference/src/whisper_coreml/engine.rs`, a
//! port of WhisperKit's `TextDecoder.decodeText` greedy KV-cache loop. Per step:
//! force the prompt token while inside the prompt, else feed the previously
//! sampled token; predict; apply the logits filters; argmax; write the
//! `[1,5120,1,1]` KV update into `cache[:,:,:,i]`; advance the two masks.
pub(crate) mod bridge;
pub use bridge::ComputeUnits;
// Whisper CoreML decoder: needs the `whisper-tokenizers` feature (tokenizer.json).
#[cfg(feature = "whisper-tokenizers")]
mod decoder;
#[cfg(feature = "whisper-tokenizers")]
pub use decoder::WhisperCoreMl;
