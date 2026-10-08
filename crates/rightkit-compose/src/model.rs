//! The serialisable composition document.
//!
//! Geometry is normalised so one document renders at preview and export
//! resolution without edits: canvas positions and sizes are fractions of the
//! canvas (0..=1), source crops are fractions of the source frame, and clip,
//! shadow and blur sizes are fractions of the shorter canvas side. Times are
//! integer milliseconds on the composition timeline; keyframes are local to
//! their layer.

use serde::{Deserialize, Serialize};

use crate::track::Track;

/// Schema identifier written into every composition document.
pub const SCHEMA: &str = "rightkit.composition.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub schema: String,
    pub canvas: Canvas,
    pub duration_ms: u64,
    /// Back to front: a later layer paints over every earlier one.
    pub layers: Vec<Layer>,
}

impl Composition {
    /// An empty composition at the current schema version.
    pub fn new(canvas: Canvas, duration_ms: u64) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            canvas,
            duration_ms,
            layers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
}

/// A normalised rectangle (fractions of the canvas, or of the source for crops).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
}

/// A normalised point, or a pixel offset where a field says so.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// Visible span of a layer on the composition timeline (`end_ms` exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Keyframable transform applied about a layer's placement centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Offset in canvas fractions.
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        dx: 0.0,
        dy: 0.0,
        scale: 1.0,
    };
}

/// Content-addressed media reference. Dimensions are source pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: String,
    pub blake3: String,
    pub width: u32,
    pub height: u32,
    /// Known media duration; evaluation clamps source times to it. Stills omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub timing: Timing,
    /// Media layers only (Video, Still, Webcam).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transform: Track<Transform>,
    /// Every layer kind. Missing track means fully opaque.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Track<f64>,
    /// Media layers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<Clip>,
    /// Media layers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    /// Media and cursor layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<MotionBlur>,
    pub content: Content,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    /// Full-motion screen or camera footage. `crop` is keyframed; a missing
    /// crop track shows the whole source.
    Video {
        asset: AssetRef,
        placement: Rect,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        crop: Track<Rect>,
        source_start_ms: u64,
        /// Source seconds per timeline second. Negative values reverse.
        #[serde(default = "one")]
        speed: f64,
    },
    /// A still image with optional keyframed crop (Ken Burns).
    Still {
        asset: AssetRef,
        placement: Rect,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        crop: Track<Rect>,
    },
    /// Independently timed camera layer. It has its own timing and source offset.
    Webcam {
        asset: AssetRef,
        placement: Rect,
        source_start_ms: u64,
    },
    /// A cursor drawn in the source space of an earlier Video or Still layer,
    /// so it moves with that layer's transform and crop.
    Cursor {
        follows: String,
        /// 1.0 draws the sprite at source pixel density.
        scale: f64,
        sprite: CursorSprite,
        /// Layer-local source-normalised positions, sorted by `at_ms`.
        samples: Vec<CursorSample>,
    },
    /// Full-canvas procedural background.
    Background { pattern: Pattern },
    /// Canvas-space label anchored at a normalised point.
    Badge {
        label: String,
        anchor: Point,
        style: Style,
    },
    /// Canvas-space caption in a normalised box.
    Caption {
        text: String,
        rect: Rect,
        style: Style,
    },
}

fn one() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Clip {
    /// `radius` is a fraction of the shorter canvas side.
    RoundedRect {
        radius: f64,
    },
    Circle,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    /// Fractions of the shorter canvas side.
    pub offset_x: f64,
    pub offset_y: f64,
    pub blur: f64,
    pub color: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MotionBlur {
    /// Shutter opening in degrees, 0 < x <= 360.
    pub shutter_degrees: f64,
    /// Sub-frame samples, 1..=64.
    pub samples: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pattern {
    Solid { color: Rgba },
    LinearGradient { angle_deg: f64, stops: Vec<Stop> },
    RadialGradient { center: Point, stops: Vec<Stop> },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    /// Position along the gradient, 0..=1, non-decreasing across stops.
    pub at: f64,
    pub color: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub foreground: Rgba,
    pub background: Rgba,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorSprite {
    pub asset: AssetRef,
    /// Hotspot in sprite pixels, measured from the sprite's top-left corner.
    pub hotspot: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CursorSample {
    /// Layer-local time in milliseconds.
    pub at_ms: u64,
    /// Source-normalised position; values outside 0..=1 are off the source.
    pub x: f64,
    pub y: f64,
    /// Idle hide: the visibility of this sample holds until the next one.
    #[serde(default = "yes")]
    pub visible: bool,
}
