//! Immutable evaluated snapshot: the only input the compositor adapter reads.
//!
//! A snapshot is produced once per accepted revision by the engine, then shared
//! by preview and export. It is immutable: cache identity, cancellation and
//! stale-frame suppression all key off [`SnapshotIdentity`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate as rk;
use rightkit_compositor::EffectUniformValueDescriptor;
use rightkit_timeline::{ClipVisualKeys, Fps, RenderPlan, Timeline, TimelineError};

use std::collections::HashMap;

/// Identity that makes a snapshot — and every frame derived from it — unique.
/// Project switches, new revisions and relinked sources all change this tuple,
/// which is what makes stale-frame suppression precise instead of heuristic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotIdentity {
    /// Stable project id (the project document's BLAKE3 namespace).
    pub project: String,
    /// Revision sequence in the timeline store.
    pub revision: u64,
    /// BLAKE3 (hex) of the canonical timeline JSON at this revision.
    pub timeline_blake3: String,
}

/// Output frame geometry and quality tier shared by preview and export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    /// Resolution scale relative to full quality (`1.0` = full). Preview may
    /// render below 1.0; export identity requires `1.0` at delivery dimensions.
    pub scale: f32,
}

impl OutputSpec {
    /// Delivery-dimension spec used for export-compatible frame identity.
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            scale: 1.0,
        }
    }

    /// Effective render dimensions after the quality scale, rounded up so a
    /// scaled preview never collapses to zero.
    pub fn effective_size(&self) -> (u32, u32) {
        (
            ((self.width as f32 * self.scale).ceil() as u32).max(1),
            ((self.height as f32 * self.scale).ceil() as u32).max(1),
        )
    }
}

/// Normalized source crop rectangle (0.0–1.0 of the source frame).
///
/// The crop is evaluated into the source texture requirement and the FFmpeg
/// decode path applies it (an FFmpeg `crop` on even pixels). A window that is
/// empty, non-finite or outside the source frame is refused with
/// `DecodeError::InvalidCrop` rather than sampling the full frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl CropRect {
    pub const FULL: CropRect = CropRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    pub fn is_full(&self) -> bool {
        *self == Self::FULL
    }
}

/// Placement of a layer in output pixels, mirroring the donor's quad transform.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerTransform {
    /// Center of the quad in output pixels.
    pub center: [f32; 2],
    /// Rendered size in output pixels.
    pub size: [f32; 2],
    pub rotation_degrees: f32,
    pub flip_x: bool,
    pub flip_y: bool,
}

/// One effect pass declared on a clip. `shader` names a pipeline the
/// compositor actually built (today only `gaussian-blur`); anything else is
/// rejected by the mapper instead of being silently dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectPassSpec {
    pub shader: String,
    #[serde(default)]
    pub uniforms: HashMap<String, EffectUniformValueDescriptor>,
}

/// Optional per-clip visual parameters. Absent entry = defaults: opacity 1,
/// full-frame fit, no effects, no mask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipVisual {
    #[serde(default = "one")]
    pub opacity: f32,
    /// Override quad transform; `None` means full output frame.
    #[serde(default)]
    pub transform: Option<LayerTransform>,
    #[serde(default)]
    pub crop: Option<CropRect>,
    /// Blend mode other than `normal`; anything set is checked for support.
    #[serde(default)]
    pub blend_mode: Option<String>,
    /// Effect pass groups, applied in order. Shader names are validated.
    #[serde(default)]
    pub effect_groups: Vec<Vec<EffectPassSpec>>,
    /// Mask texture requirement. Masks need a supplied alpha texture; until a
    /// mask source exists in the timeline model, any value is rejected.
    #[serde(default)]
    pub mask: Option<String>,
    /// Keyframed opacity in `0..=1`, keyed at clip-local ticks. Empty means
    /// the static `opacity` applies. Cannot be combined with `cross_fade_in_frames`.
    #[serde(default)]
    pub opacity_keys: Vec<rk::Keyframe<f64>>,
    /// Keyframed transform about the fitted placement centre, keyed at
    /// clip-local ticks. Empty means no keyframed transform. Cannot be combined
    /// with the static `transform` (which carries rotation and flip).
    #[serde(default)]
    pub transform_keys: Vec<rk::Keyframe<rk::Transform>>,
    /// Keyframed source crop, keyed at clip-local ticks. Empty means the
    /// static `crop` applies. The crop selects a source window that is drawn
    /// into the clip's fitted placement.
    #[serde(default)]
    pub crop_keys: Vec<rk::Keyframe<rk::Rect>>,
    /// Cross-fade in over this many output frames, starting at the clip's
    /// first frame, from the previous clip's held last frame. `0` or `1` is a
    /// cut. The previous clip must be plain (see `mapper::is_plain_visual`).
    #[serde(default)]
    pub cross_fade_in_frames: i64,
}

impl ClipVisual {
    /// Adapts a timeline clip's durable keys into the compose visual.
    ///
    /// Units convert here and nowhere else: opacity permille becomes `0..=1`,
    /// transform offsets become canvas fractions, scale permille becomes a
    /// multiplier, and crop permille becomes a normalized source rect. Key times
    /// are timeline ticks, the same clip-local tick rate rightkit-compose
    /// evaluates. Keys were validated when the timeline accepted them.
    pub fn from_keys(keys: &ClipVisualKeys) -> Self {
        let at = |t: rightkit_timeline::Ticks| rk::Ticks(t.0.max(0) as u64);
        Self {
            opacity_keys: keys
                .opacity
                .iter()
                .map(|k| rk::Keyframe {
                    at: at(k.at),
                    value: k.value as f64 / 1000.0,
                    easing: rk::Easing::Linear,
                })
                .collect(),
            transform_keys: keys
                .transform
                .iter()
                .map(|k| rk::Keyframe {
                    at: at(k.at),
                    value: rk::Transform {
                        dx: k.value.dx as f64 / 1000.0,
                        dy: k.value.dy as f64 / 1000.0,
                        scale: k.value.scale as f64 / 1000.0,
                    },
                    easing: rk::Easing::Linear,
                })
                .collect(),
            crop_keys: keys
                .crop
                .iter()
                .map(|k| rk::Keyframe {
                    at: at(k.at),
                    value: rk::Rect {
                        x: k.value.x as f64 / 1000.0,
                        y: k.value.y as f64 / 1000.0,
                        width: k.value.width as f64 / 1000.0,
                        height: k.value.height as f64 / 1000.0,
                    },
                    easing: rk::Easing::Linear,
                })
                .collect(),
            cross_fade_in_frames: keys.cross_fade_in_frames,
            ..Self::default()
        }
    }
}

fn one() -> f32 {
    1.0
}

/// Per-clip compose visuals for every keyed clip in a timeline's corrections.
pub fn visuals_from_timeline(timeline: &Timeline) -> BTreeMap<String, ClipVisual> {
    timeline
        .corrections
        .visuals
        .iter()
        .map(|(clip_id, keys)| (clip_id.clone(), ClipVisual::from_keys(keys)))
        .collect()
}

impl Default for ClipVisual {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            transform: None,
            crop: None,
            blend_mode: None,
            effect_groups: Vec::new(),
            mask: None,
            opacity_keys: Vec::new(),
            transform_keys: Vec::new(),
            crop_keys: Vec::new(),
            cross_fade_in_frames: 0,
        }
    }
}

/// The immutable evaluated input to every compose call.
#[derive(Debug, Clone)]
pub struct ComposeSnapshot {
    pub identity: SnapshotIdentity,
    /// Compiled render plan — the evaluated source spans, placements and audio
    /// layers produced by `rightkit_timeline::compile` under the shared
    /// frame-rounding rule.
    pub plan: RenderPlan,
    pub output: OutputSpec,
    /// Per-clip visual parameters keyed by clip id. Defaults when absent.
    pub visuals: BTreeMap<String, ClipVisual>,
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("timeline failed validation/compile: {0}")]
    Timeline(#[from] TimelineError),
    #[error("output dimensions must be non-zero")]
    EmptyOutput,
}

impl ComposeSnapshot {
    /// Evaluates a timeline revision into a snapshot. `revision` and
    /// `timeline_blake3` come from the store (`head_hash`), keeping cache
    /// identity honest without re-deriving the hash here.
    pub fn evaluate(
        project: impl Into<String>,
        revision: u64,
        timeline_blake3: impl Into<String>,
        timeline: &Timeline,
        output: OutputSpec,
        visuals: BTreeMap<String, ClipVisual>,
    ) -> Result<Self, SnapshotError> {
        if output.width == 0 || output.height == 0 {
            return Err(SnapshotError::EmptyOutput);
        }
        let plan = rightkit_timeline::compile(timeline)?;
        Ok(Self {
            identity: SnapshotIdentity {
                project: project.into(),
                revision,
                timeline_blake3: timeline_blake3.into(),
            },
            plan,
            output,
            visuals,
        })
    }

    /// Evaluates a timeline revision with its own keyed visuals adapted in, so
    /// callers cannot silently drop `timeline.corrections.visuals`.
    pub fn from_timeline(
        project: impl Into<String>,
        revision: u64,
        timeline_blake3: impl Into<String>,
        timeline: &Timeline,
        output: OutputSpec,
    ) -> Result<Self, SnapshotError> {
        Self::evaluate(
            project,
            revision,
            timeline_blake3,
            timeline,
            output,
            visuals_from_timeline(timeline),
        )
    }

    pub fn fps(&self) -> Fps {
        self.plan.fps
    }
}
