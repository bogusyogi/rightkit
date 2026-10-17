//! `rightkit-compose`: the evaluated composition model (RK9).
//!
//! Time is integer [`Ticks`] on the CutRight clock, 705,600,000 per second.
//! Video speed is a [`RetimeSegment`] list, and easings include the ScreenRight
//! camera curve.
//!
//! A [`Composition`] is a durable, serialisable document of layers in stack
//! order: video and stills, cursor sprites with hotspots that follow a source
//! layer, procedural backgrounds, rounded and circular clipping, shadows,
//! temporal blur, canvas-space badges and captions, and an independently timed
//! webcam. Transforms, crops and opacity are keyframed with easing.
//!
//! [`evaluate`] is pure: it maps `(composition, t)` to a [`FrameDescription`]
//! with no renderer attached. CutRight's native compositor and its typed FFmpeg
//! export both consume that description, so preview and export agree.
//!
//! No rendering, codec or I/O lives here.

pub mod error;
pub mod eval;
mod legacy;
pub mod model;
pub mod opacity;
pub mod retime;
pub mod ticks;
pub mod track;
pub mod validate;

pub use error::CompositionError;
pub use eval::{
    evaluate, evaluate_ms, evaluate_ms_v3, evaluate_v3, ClipPx, FrameDescription,
    FrameDescriptionV3, FrameLayer, FrameLayerV3, MotionBlurFrame, Payload, PayloadV3, ShadowPx,
    FRAME_SCHEMA, FRAME_SCHEMA_V3,
};
pub use model::{
    AssetRef, Canvas, ClickShape, Clip, Composition, Content, CursorSample, CursorSprite, Layer,
    MotionBlur, Pattern, Point, Rect, Rgba, Shadow, Stop, Style, Timing, Transform,
    LEGACY_SCHEMA_V1, SCHEMA,
};
pub use opacity::{FrameFade, FrameFadeError, Opacity, OpacityError};
pub use retime::{output_span, source_time_at, source_time_at_signed, RetimeSegment};
pub use ticks::{Rational, SignedTicks, Ticks, TICKS_PER_MS, TICKS_PER_SECOND};
pub use track::{sample, Easing, Keyframe, Lerp, Track, CAMERA_BOUNCE_MAX};
pub use validate::validate;
