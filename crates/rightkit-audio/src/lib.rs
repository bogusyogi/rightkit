//! Shared audio for Right Suite apps.
//!
//! * Typed FFmpeg filter graphs ([`filter`], [`chain`], [`ducking`], [`room_tone`],
//!   [`bus`], [`preset`]) — donor CutRight `cutright-audio`.
//! * Measurement and offline renders through the shared `rightkit-ffmpeg` runner
//!   ([`measure`], [`profile`] loudness gate).
//! * WAV I/O ([`wav`]) — dedupes ScrapeRight's reader.
//! * Live capture ([`capture`], feature `capture`) — donor HeardRight
//!   `heardright_capture`; Windows endpoint/power lifecycle in [`lifecycle`].
//!
//! Graph builders are pure and never spawn processes; everything that runs FFmpeg goes
//! through [`rightkit_ffmpeg`] so cancellation, timeout and process-tree ownership are
//! uniform.

pub mod bus;
#[cfg(feature = "capture")]
pub mod capture;
pub mod chain;
pub mod ducking;
pub mod error;
pub mod filter;
#[cfg(feature = "capture")]
pub mod lifecycle;
pub mod measure;
pub mod preset;
pub mod profile;
pub mod room_tone;
pub mod wav;

pub use bus::{dialogue_bus_fragment, GraphFragment};
pub use chain::{Compressor, DeEsser, Denoise, DialogueChain, Expander, HighPass, Limiter};
pub use ducking::{sidechain_split_fragment, Ducking};
pub use error::AudioError;
pub use filter::Filter;
pub use measure::{AStats, AudioTools, LevelMeasurement};
pub use preset::{Preset, RoomTonePolicy};
pub use profile::{AudioGate, LoudnessGateResult};
pub use room_tone::{
    find_room_tone, room_tone_fill_fragment, GapSpan, RoomToneParams, RoomToneSample,
};
