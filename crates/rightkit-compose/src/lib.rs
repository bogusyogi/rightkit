//! `rightkit-compose`: the evaluated composition model (RK9).
//!
//! A [`Composition`] is a durable, serialisable document of layers in stack
//! order: video and stills, cursor sprites with hotspots that follow a source
//! layer, procedural backgrounds, rounded and circular clipping, shadows,
//! temporal blur, canvas-space badges and captions, and an independently timed
//! webcam. Transforms, crops and opacity are keyframed with easing.
//!
//! [`evaluate`] is pure: it maps `(composition, t_ms)` to a [`FrameDescription`]
//! with no renderer attached. CutRight's native compositor and its typed FFmpeg
//! export both consume that description, so preview and export agree.
//!
//! No rendering, codec or I/O lives here.

pub mod error;
pub mod eval;
pub mod model;
pub mod track;
pub mod validate;

pub use error::CompositionError;
pub use eval::{
    evaluate, ClipPx, FrameDescription, FrameLayer, MotionBlurFrame, Payload, ShadowPx,
    FRAME_SCHEMA,
};
pub use model::{
    AssetRef, Canvas, ClickShape, Clip, Composition, Content, CursorSample, CursorSprite, Layer,
    MotionBlur, Pattern, Point, Rect, Rgba, Shadow, Stop, Style, Timing, Transform, SCHEMA,
};
pub use track::{sample, Easing, Keyframe, Lerp, Track};
pub use validate::validate;
