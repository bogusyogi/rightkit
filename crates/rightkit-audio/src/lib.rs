//! Reusable audio primitives.
//!
//! * Typed FFmpeg filter graphs ([`filter`], [`chain`], [`ducking`], [`room_tone`],
//!   [`bus`], [`preset`]).
//! * Measurement and offline renders through the shared `rightkit-ffmpeg` runner
//!   ([`measure`], [`profile`] loudness gate).
//! * Pure-Rust [`mic_align`] (NCC mic-to-camera lag) and [`room_fill`] (equal-power looped
//!   room-tone fill).
//! * WAV I/O ([`wav`]).
//! * Live capture ([`capture`], feature `capture`) and Windows endpoint/power lifecycle ([`lifecycle`]).
//! * Audio output (`playback`, feature `playback`): lock-free ring into a cpal output
//!   stream with a latency-adjusted playback clock.
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
pub mod mic_align;
#[cfg(feature = "playback")]
pub mod playback;
pub mod preset;
pub mod profile;
pub mod room_fill;
pub mod room_tone;
#[cfg(feature = "capture")]
pub mod setup;
pub mod wav;

pub use bus::{dialogue_bus_fragment, GraphFragment};
pub use chain::{Compressor, DeEsser, Denoise, DialogueChain, Expander, HighPass, Limiter};
pub use ducking::{sidechain_split_fragment, Ducking};
pub use error::AudioError;
pub use filter::Filter;
pub use measure::{AStats, AudioTools, LevelMeasurement};
pub use mic_align::{align_mic, AlignParams, MicAlignment};
pub use preset::{Preset, RoomTonePolicy};
pub use profile::{AudioGate, LoudnessGateResult};
pub use room_fill::{
    encode_mono_f32_wav, rms, room_tone_bed, room_tone_fill, FillParams, ToneFill,
};
pub use room_tone::{
    find_room_tone, room_tone_fill_fragment, GapSpan, RoomToneParams, RoomToneSample,
};
