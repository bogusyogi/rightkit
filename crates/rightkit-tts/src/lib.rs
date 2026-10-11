//! Shared text-to-speech runtime.
//!
//! Feature boundaries (see `Cargo.toml`):
//! - `system`: no-model OS voice backend (`say` on macOS, SAPI on Windows) with a
//!   word-timed manifest that can be passed to `rightkit-transcript`.
//! - `client`: QTS worker client (spawn, chunking, model manifest verification).
//! - `engine`: the vendored Qwen3-TTS worker (`worker`, binary `rightkit-tts-worker`); implies `client`.
//!
//! `wav` and the sample-rate constants are always available.

pub mod wav;

#[cfg(feature = "client")]
pub mod chunk;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod mode_location;
#[cfg(feature = "client")]
pub mod models;
#[cfg(feature = "client")]
pub mod protocol;
#[cfg(feature = "system")]
pub mod system;
#[cfg(feature = "client")]
pub mod worker_args;

#[cfg(feature = "client")]
pub use client::{QtsClient, QtsError, SynthOptions, SynthResult};
#[cfg(feature = "client")]
pub use mode_location::ModeLocation;
#[cfg(feature = "client")]
pub use protocol::{Mode, PRESET_SPEAKERS};
#[cfg(feature = "system")]
pub use system::{
    chunk_sentences, SentenceChunk, SystemTts, SystemTtsError, SystemTtsManifest, SystemTtsOptions,
    SystemTtsResult, SystemTtsSegment,
};

#[cfg(feature = "engine")]
pub mod worker;

pub const QTS_SAMPLE_RATE_HZ: u32 = 24_000;
pub const SAMPLE_RATE_HZ: u32 = QTS_SAMPLE_RATE_HZ;
