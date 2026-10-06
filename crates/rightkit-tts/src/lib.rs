//! Shared text-to-speech runtime.
//!
//! `system` provides a no-model OS voice backend (`say` on macOS and SAPI on
//! Windows). `engine` adds the vendored Qwen3-TTS QTS worker. Both return PCM
//! WAV plus a word-timed manifest that can be passed to `rightkit-transcript`.

pub mod chunk;
pub mod client;
pub mod models;
pub mod protocol;
pub mod system;
pub mod wav;

pub use client::{QtsError, QtsClient, SynthOptions, SynthResult};
pub use system::{chunk_sentences, SentenceChunk, SystemTts, SystemTtsError, SystemTtsManifest, SystemTtsOptions, SystemTtsResult, SystemTtsSegment};

#[cfg(feature = "engine")]
pub mod worker;

pub const QTS_SAMPLE_RATE_HZ: u32 = 24_000;
pub const SAMPLE_RATE_HZ: u32 = QTS_SAMPLE_RATE_HZ;
