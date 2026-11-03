//! Native adapter between the immutable evaluated timeline snapshot and the native wgpu
//! compositor (feature `native`).
//!
//! Contract:
//! - Input is an immutable [`ComposeSnapshot`]: project + revision + timeline
//!   hash identity, the compiled render plan, output spec, per-clip visuals.
//! - [`mapper::compose_frame`] produces `FrameDescriptor`s plus the texture
//!   requirements a transport must satisfy. It maps the timeline semantics the
//!   model actually has, evaluates keyframed opacity, transform and crop and
//!   cross-fade-in through `rightkit-compose`, and *rejects* the rest (masks
//!   without a mask source, unknown effect shaders, cross-fades from a
//!   non-plain clip, and crops outside the source frame, which decode refuses).
//!   Timeline keys reach this path through `ComposeSnapshot::from_timeline`.
//! - [`audio::audio_descriptor`] produces the sample-exact mix contract;
//!   audio mixing itself stays with the engine's audio path.
//! - [`identity`], [`cache`], [`scheduler`] are the bounded frame-request
//!   infrastructure (rewritten): stale revisions,
//!   cancellation and project switches are identity mismatches, never served
//!   frames.
//! - [`transport::TextureProvider`] is the decoder/transport seam. No decoder
//!   ships in this crate.
//! - [`executor::NativeCompositor`] runs frames offscreen for proof; embedded
//!   playback inside a desktop shell is a separate integration.

pub mod audio;
mod base_eval;
pub mod blockon;
pub mod cache;
pub mod clock;
pub mod decode;
pub mod executor;
pub mod identity;
pub mod mapper;
pub mod pcm;
pub mod scheduler;
pub mod snapshot;
pub mod transport;

pub use audio::{audio_descriptor, AttenuationWindow, AudioMixDescriptor, AudioSegment};
pub use cache::{FrameCache, FrameResource};
pub use clock::{AvSync, LoopRange, SyncAction, TimelineClock};
pub use decode::{BusPixels, DecodeBus, DecodeError, FfmpegDecoder, PixelLru, SourceSpec};
pub use executor::{texture_from_rgba, ExecuteError, NativeCompositor};
pub use identity::{FrameDeadline, FrameKey, FramePriority, FrameRequest, PreviewQuality};
pub use mapper::{compose_frame, ComposeError, ComposedFrame, TextureRequirement};
pub use pcm::{decode_and_mix, decode_pcm, mix, AudioError, AudioStreamer, Pcm};
pub use scheduler::{FrameScheduler, SchedulerError, SchedulerStats};
pub use snapshot::{
    visuals_from_timeline, ClipVisual, ComposeSnapshot, CropRect, EffectPassSpec, LayerTransform,
    OutputSpec, SnapshotError, SnapshotIdentity,
};
pub use transport::{decode_size, TextureProvider};
