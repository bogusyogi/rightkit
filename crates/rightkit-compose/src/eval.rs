//! Pure per-frame evaluation. CutRight's native compositor and its typed FFmpeg
//! export both consume the [`FrameDescription`] this module returns, so preview
//! and export agree by construction.

use serde::{Deserialize, Serialize};

use crate::error::CompositionError;
use crate::model::{
    AssetRef, ClickShape, Clip, Composition, Content, CursorSample, CursorSprite, Layer, Pattern,
    Point, Rect, Rgba, Style, Timing, Transform,
};
use crate::retime::{source_time_at_signed, RetimeSegment};
use crate::ticks::{Rational, SignedTicks, Ticks, TICKS_PER_SECOND};
use crate::track::{sample, Easing, Keyframe};
use crate::validate::validate;

/// Default frame schema, emitted by [`evaluate`] & [`evaluate_ms`]. Video
/// source times are unsigned `Ticks`; a negative source is an evaluation error.
pub const FRAME_SCHEMA: &str = "rightkit.frame.v2";

/// Opt-in frame schema, emitted by [`evaluate_v3`] & [`evaluate_ms_v3`]. Video
/// source times are [`SignedTicks`], so negative pre-roll is representable.
pub const FRAME_SCHEMA_V3: &str = "rightkit.frame.v3";

/// Everything a renderer needs to draw one frame. Layers are in stack order,
/// back to front, and include only layers active at `t`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameDescription {
    pub schema: String,
    /// The evaluated timeline time, in ticks.
    pub t: Ticks,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<FrameLayer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameLayer {
    pub id: String,
    /// Index into `Composition::layers`.
    pub stack_index: usize,
    pub opacity: f64,
    pub clip: Option<ClipPx>,
    pub shadow: Option<ShadowPx>,
    pub motion_blur: Option<MotionBlurFrame>,
    pub payload: Payload,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum ClipPx {
    RoundedRect { radius_px: f64 },
    Circle,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ShadowPx {
    pub offset_x_px: f64,
    pub offset_y_px: f64,
    pub blur_px: f64,
    pub color: Rgba,
}

/// Sub-frame time offsets, in signed ticks relative to `t`. A renderer
/// evaluates the composition at each offset and averages the results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionBlurFrame {
    pub offsets_ticks: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Video {
        asset_id: String,
        /// Source time to show, in ticks, after retime and clamping to the asset.
        source: Ticks,
        crop: Rect,
        dest_px: Rect,
    },
    Still {
        asset_id: String,
        crop: Rect,
        dest_px: Rect,
    },
    Webcam {
        asset_id: String,
        source: Ticks,
        dest_px: Rect,
    },
    /// `tip_px` is where the hotspot lands. `visible` is false when the cursor
    /// is hidden, off the followed source, or the followed layer is inactive.
    Cursor {
        asset_id: String,
        visible: bool,
        tip_px: Point,
        sprite_px: Rect,
    },
    Background {
        pattern: Pattern,
    },
    Badge {
        label: String,
        anchor_px: Point,
        style: Style,
    },
    Caption {
        text: String,
        rect_px: Rect,
        style: Style,
    },
    /// Expanding ring. `alpha` is 0 before onset and after completion; the
    /// layer's own opacity is applied separately.
    Ripple {
        center_px: Point,
        radius_px: f64,
        stroke_px: f64,
        alpha: f64,
        style: Style,
    },
    /// Rounded box that grows and fades. `alpha` is 0 outside the animation.
    Highlight {
        rect_px: Rect,
        corner_radius_px: f64,
        alpha: f64,
        style: Style,
    },
}

/// Frame description for the opt-in `rightkit.frame.v3` schema. Same shape as
/// [`FrameDescription`] but layers carry [`PayloadV3`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameDescriptionV3 {
    pub schema: String,
    pub t: Ticks,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<FrameLayerV3>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameLayerV3 {
    pub id: String,
    pub stack_index: usize,
    pub opacity: f64,
    pub clip: Option<ClipPx>,
    pub shadow: Option<ShadowPx>,
    pub motion_blur: Option<MotionBlurFrame>,
    pub payload: PayloadV3,
}

/// v3 payload. Identical to [`Payload`] except `Video::source` is signed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadV3 {
    Video {
        asset_id: String,
        /// Signed source time after retime. Known duration caps only the upper
        /// bound; negative pre-roll is preserved for the caller's decoder policy.
        source: SignedTicks,
        crop: Rect,
        dest_px: Rect,
    },
    Still {
        asset_id: String,
        crop: Rect,
        dest_px: Rect,
    },
    Webcam {
        asset_id: String,
        source: Ticks,
        dest_px: Rect,
    },
    Cursor {
        asset_id: String,
        visible: bool,
        tip_px: Point,
        sprite_px: Rect,
    },
    Background {
        pattern: Pattern,
    },
    Badge {
        label: String,
        anchor_px: Point,
        style: Style,
    },
    Caption {
        text: String,
        rect_px: Rect,
        style: Style,
    },
    Ripple {
        center_px: Point,
        radius_px: f64,
        stroke_px: f64,
        alpha: f64,
        style: Style,
    },
    Highlight {
        rect_px: Rect,
        corner_radius_px: f64,
        alpha: f64,
        style: Style,
    },
}

impl From<Payload> for PayloadV3 {
    fn from(payload: Payload) -> Self {
        match payload {
            Payload::Video {
                asset_id,
                source,
                crop,
                dest_px,
            } => PayloadV3::Video {
                asset_id,
                source: source.into(),
                crop,
                dest_px,
            },
            Payload::Still {
                asset_id,
                crop,
                dest_px,
            } => PayloadV3::Still {
                asset_id,
                crop,
                dest_px,
            },
            Payload::Webcam {
                asset_id,
                source,
                dest_px,
            } => PayloadV3::Webcam {
                asset_id,
                source,
                dest_px,
            },
            Payload::Cursor {
                asset_id,
                visible,
                tip_px,
                sprite_px,
            } => PayloadV3::Cursor {
                asset_id,
                visible,
                tip_px,
                sprite_px,
            },
            Payload::Background { pattern } => PayloadV3::Background { pattern },
            Payload::Badge {
                label,
                anchor_px,
                style,
            } => PayloadV3::Badge {
                label,
                anchor_px,
                style,
            },
            Payload::Caption {
                text,
                rect_px,
                style,
            } => PayloadV3::Caption {
                text,
                rect_px,
                style,
            },
            Payload::Ripple {
                center_px,
                radius_px,
                stroke_px,
                alpha,
                style,
            } => PayloadV3::Ripple {
                center_px,
                radius_px,
                stroke_px,
                alpha,
                style,
            },
            Payload::Highlight {
                rect_px,
                corner_radius_px,
                alpha,
                style,
            } => PayloadV3::Highlight {
                rect_px,
                corner_radius_px,
                alpha,
                style,
            },
        }
    }
}

/// Evaluates the composition at a millisecond time. A thin wrapper over
/// [`evaluate`] that converts exactly, 705,600 ticks per millisecond. A value
/// past the composition's duration returns `OutOfRange`.
pub fn evaluate_ms(
    composition: &Composition,
    t_ms: u64,
) -> Result<FrameDescription, CompositionError> {
    evaluate(composition, Ticks::from_ms(t_ms))
}

/// Evaluates the composition at a millisecond time into the opt-in v3 frame.
pub fn evaluate_ms_v3(
    composition: &Composition,
    t_ms: u64,
) -> Result<FrameDescriptionV3, CompositionError> {
    evaluate_v3(composition, Ticks::from_ms(t_ms))
}

/// Evaluates the composition at timeline time `t` into the default `v2` frame.
/// Validation runs first, so an invalid composition is reported rather than
/// drawn. A video layer whose retimed source is negative is an `Invalid` error;
/// use [`evaluate_v3`] to keep negative pre-roll.
pub fn evaluate(composition: &Composition, t: Ticks) -> Result<FrameDescription, CompositionError> {
    let (width, height, layers) = evaluate_with(
        composition,
        t,
        payload_at,
        |id, stack_index, opacity, clip, shadow, motion_blur, payload| FrameLayer {
            id,
            stack_index,
            opacity,
            clip,
            shadow,
            motion_blur,
            payload,
        },
    )?;
    Ok(FrameDescription {
        schema: FRAME_SCHEMA.to_string(),
        t,
        width,
        height,
        layers,
    })
}

/// Evaluates at `t` into the opt-in `rightkit.frame.v3` frame, whose video
/// source is [`SignedTicks`]. Same validation & layer selection as [`evaluate`].
pub fn evaluate_v3(
    composition: &Composition,
    t: Ticks,
) -> Result<FrameDescriptionV3, CompositionError> {
    let (width, height, layers) = evaluate_with(
        composition,
        t,
        payload_at_v3,
        |id, stack_index, opacity, clip, shadow, motion_blur, payload| FrameLayerV3 {
            id,
            stack_index,
            opacity,
            clip,
            shadow,
            motion_blur,
            payload,
        },
    )?;
    Ok(FrameDescriptionV3 {
        schema: FRAME_SCHEMA_V3.to_string(),
        t,
        width,
        height,
        layers,
    })
}

type Assemble<P, L> =
    fn(String, usize, f64, Option<ClipPx>, Option<ShadowPx>, Option<MotionBlurFrame>, P) -> L;

/// Builds one layer payload for the layer visible at `t` with the given (x, y) placement.
type PayloadFor<P> = fn(&Composition, &Layer, Ticks, (f64, f64)) -> Result<P, CompositionError>;

fn evaluate_with<P, L>(
    composition: &Composition,
    t: Ticks,
    payload_for: PayloadFor<P>,
    assemble: Assemble<P, L>,
) -> Result<(u32, u32, Vec<L>), CompositionError> {
    validate(composition)?;
    if t >= composition.duration {
        return Err(CompositionError::OutOfRange {
            t,
            duration: composition.duration,
        });
    }
    let canvas = composition.canvas;
    let (width, height) = (f64::from(canvas.width), f64::from(canvas.height));
    let min_side = width.min(height);

    let mut layers = Vec::new();
    for (stack_index, layer) in composition.layers.iter().enumerate() {
        if !is_active(layer.timing, t) {
            continue;
        }
        let local = Ticks(t.0 - layer.timing.start.0);
        let opacity = sample(&layer.opacity, local).unwrap_or(1.0);
        let payload = payload_for(composition, layer, t, (width, height))?;
        let clip = layer.clip.map(|c| match c {
            Clip::RoundedRect { radius } => ClipPx::RoundedRect {
                radius_px: radius * min_side,
            },
            Clip::Circle => ClipPx::Circle,
        });
        let shadow = layer.shadow.map(|s| ShadowPx {
            offset_x_px: s.offset_x * min_side,
            offset_y_px: s.offset_y * min_side,
            blur_px: s.blur * min_side,
            color: s.color,
        });
        let motion_blur = layer.motion_blur.map(|m| MotionBlurFrame {
            offsets_ticks: blur_offsets(m.samples, m.shutter_degrees, composition.fps),
        });
        layers.push(assemble(
            layer.id.clone(),
            stack_index,
            opacity,
            clip,
            shadow,
            motion_blur,
            payload,
        ));
    }
    Ok((canvas.width, canvas.height, layers))
}

fn is_active(timing: Timing, t: Ticks) -> bool {
    t >= timing.start && t < timing.end
}

/// v3 payload: video keeps its signed source, everything else is shared.
fn payload_at_v3(
    composition: &Composition,
    layer: &Layer,
    t: Ticks,
    canvas: (f64, f64),
) -> Result<PayloadV3, CompositionError> {
    let Content::Video {
        asset,
        placement,
        crop,
        retime,
    } = &layer.content
    else {
        return payload_at(composition, layer, t, canvas).map(PayloadV3::from);
    };
    let (width, height) = canvas;
    let local = Ticks(t.0 - layer.timing.start.0);
    let (dest, crop) = media_geometry(layer, *placement, crop, local);
    Ok(PayloadV3::Video {
        asset_id: asset.id.clone(),
        source: video_source(retime, local, asset),
        crop,
        dest_px: to_px(dest, width, height),
    })
}

fn payload_at(
    composition: &Composition,
    layer: &Layer,
    t: Ticks,
    canvas: (f64, f64),
) -> Result<Payload, CompositionError> {
    let (width, height) = canvas;
    let local = Ticks(t.0 - layer.timing.start.0);
    Ok(match &layer.content {
        Content::Video {
            asset,
            placement,
            crop,
            retime,
        } => {
            let (dest, crop) = media_geometry(layer, *placement, crop, local);
            let source = Ticks::try_from(video_source(retime, local, asset)).map_err(|_| {
                CompositionError::Invalid {
                    layer: layer.id.clone(),
                    reason: "negative video source time; evaluate with evaluate_v3".to_string(),
                }
            })?;
            Payload::Video {
                asset_id: asset.id.clone(),
                source,
                crop,
                dest_px: to_px(dest, width, height),
            }
        }
        Content::Still {
            asset,
            placement,
            crop,
        } => {
            let (dest, crop) = media_geometry(layer, *placement, crop, local);
            Payload::Still {
                asset_id: asset.id.clone(),
                crop,
                dest_px: to_px(dest, width, height),
            }
        }
        Content::Webcam {
            asset,
            placement,
            source_start,
        } => {
            let dest = place(*placement, layer_transform(layer, local));
            Payload::Webcam {
                asset_id: asset.id.clone(),
                source: clamp_to_asset(source_start.saturating_add(local), asset),
                dest_px: to_px(dest, width, height),
            }
        }
        Content::Cursor {
            follows,
            scale,
            sprite,
            samples,
        } => {
            let spec = CursorSpec {
                follows,
                scale: *scale,
                sprite,
                samples,
            };
            cursor_payload(composition, &spec, local, t, canvas)
        }
        Content::Background { pattern } => Payload::Background {
            pattern: pattern.clone(),
        },
        Content::Badge {
            label,
            anchor,
            style,
        } => Payload::Badge {
            label: label.clone(),
            anchor_px: Point {
                x: anchor.x * width,
                y: anchor.y * height,
            },
            style: *style,
        },
        Content::Caption { text, rect, style } => Payload::Caption {
            text: text.clone(),
            rect_px: to_px(*rect, width, height),
            style: *style,
        },
        Content::ClickEffect {
            anchor,
            effect,
            style,
            onset,
            duration,
            easing,
        } => click_payload(
            *anchor,
            *effect,
            *style,
            (*onset, *duration, *easing),
            local,
            (width, height),
        ),
    })
}

/// Animation progress of a click effect at a layer-local time. Returns
/// `None` before onset; `Some((eased, linear))` while running and `None`
/// once the animation has completed.
fn click_progress(
    onset: Ticks,
    duration: Ticks,
    easing: Easing,
    local: Ticks,
) -> Option<(f64, f64)> {
    if local < onset {
        return None;
    }
    let elapsed = Ticks(local.0 - onset.0);
    if elapsed >= duration {
        return None;
    }
    let linear = elapsed.0 as f64 / duration.0 as f64;
    Some((easing.apply_over(linear, duration), linear))
}

fn click_payload(
    anchor: Point,
    effect: ClickShape,
    style: Style,
    (onset, duration, easing): (Ticks, Ticks, Easing),
    local: Ticks,
    canvas: (f64, f64),
) -> Payload {
    let (width, height) = canvas;
    let min_side = width.min(height);
    let centre = Point {
        x: anchor.x * width,
        y: anchor.y * height,
    };
    let progress = click_progress(onset, duration, easing, local);
    match effect {
        ClickShape::Ripple {
            max_radius,
            stroke_width,
        } => {
            let (radius_px, alpha) = match progress {
                Some((eased, linear)) => (max_radius * min_side * eased, 1.0 - linear),
                None => (0.0, 0.0),
            };
            Payload::Ripple {
                center_px: centre,
                radius_px,
                stroke_px: stroke_width * min_side,
                alpha,
                style,
            }
        }
        ClickShape::Highlight {
            half_width,
            half_height,
            grow,
            corner_radius,
        } => {
            let (scale, alpha) = match progress {
                Some((eased, linear)) => (1.0 + grow * eased, 1.0 - linear),
                None => (1.0, 0.0),
            };
            let (hw, hh) = (half_width * scale * width, half_height * scale * height);
            Payload::Highlight {
                rect_px: Rect {
                    x: centre.x - hw,
                    y: centre.y - hh,
                    width: 2.0 * hw,
                    height: 2.0 * hh,
                },
                corner_radius_px: corner_radius * min_side,
                alpha,
                style,
            }
        }
    }
}

fn layer_transform(layer: &Layer, local: Ticks) -> Transform {
    sample(&layer.transform, local).unwrap_or(Transform::IDENTITY)
}

/// Destination (canvas fractions) and visible crop (source fractions) of a media layer.
fn media_geometry(
    layer: &Layer,
    placement: Rect,
    crop: &[Keyframe<Rect>],
    local: Ticks,
) -> (Rect, Rect) {
    let dest = place(placement, layer_transform(layer, local));
    let crop = sample(crop, local).unwrap_or(Rect::FULL);
    (dest, crop)
}

/// Applies a transform about the placement centre. Scale is uniform.
fn place(placement: Rect, transform: Transform) -> Rect {
    let width = placement.width * transform.scale;
    let height = placement.height * transform.scale;
    let cx = placement.x + placement.width / 2.0;
    let cy = placement.y + placement.height / 2.0;
    Rect {
        x: cx - width / 2.0 + transform.dx,
        y: cy - height / 2.0 + transform.dy,
        width,
        height,
    }
}

/// Source time of a video layer at layer-local `local`. An empty retime is
/// identity. Otherwise the retime maps the time, and validation guarantees the
/// segments cover the layer.
fn video_source(retime: &[RetimeSegment], local: Ticks, asset: &AssetRef) -> SignedTicks {
    let raw = if retime.is_empty() {
        local.into()
    } else {
        source_time_at_signed(retime, local).unwrap_or_default()
    };
    match asset.duration {
        Some(duration) => raw.min(duration.into()),
        None => raw,
    }
}

/// Clamps a source time to the asset's known duration, if it has one.
fn clamp_to_asset(raw: Ticks, asset: &AssetRef) -> Ticks {
    match asset.duration {
        Some(duration) => raw.min(duration),
        None => raw,
    }
}

fn to_px(r: Rect, width: f64, height: f64) -> Rect {
    Rect {
        x: r.x * width,
        y: r.y * height,
        width: r.width * width,
        height: r.height * height,
    }
}

/// Finds the followed Video or Still layer and returns its asset, destination
/// (canvas fractions) and crop (source fractions) at `t`. `None` when the
/// layer is inactive at `t`.
fn follow_geometry<'a>(
    composition: &'a Composition,
    follows: &str,
    t: Ticks,
) -> Option<(&'a AssetRef, Rect, Rect)> {
    let target = composition.layers.iter().find(|l| l.id == follows)?;
    if !is_active(target.timing, t) {
        return None;
    }
    let local = Ticks(t.0 - target.timing.start.0);
    match &target.content {
        Content::Video {
            asset,
            placement,
            crop,
            ..
        }
        | Content::Still {
            asset,
            placement,
            crop,
        } => {
            let (dest, crop) = media_geometry(target, *placement, crop, local);
            Some((asset, dest, crop))
        }
        _ => None,
    }
}

/// Interpolated cursor state at a layer-local time: position and visibility.
/// Visibility holds from the earlier sample until the next one.
fn cursor_state(samples: &[CursorSample], local: Ticks) -> Option<(f64, f64, bool)> {
    let first = samples.first()?;
    if local <= first.at {
        return Some((first.x, first.y, first.visible));
    }
    let last = samples.last()?;
    if local >= last.at {
        return Some((last.x, last.y, last.visible));
    }
    let index = samples.partition_point(|s| s.at <= local) - 1;
    let a = &samples[index];
    let b = &samples[index + 1];
    let u = (local.0 - a.at.0) as f64 / (b.at.0 - a.at.0) as f64;
    Some((a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u, a.visible))
}

/// The parts of a cursor layer needed to place its sprite.
struct CursorSpec<'a> {
    follows: &'a str,
    scale: f64,
    sprite: &'a CursorSprite,
    samples: &'a [CursorSample],
}

fn cursor_payload(
    composition: &Composition,
    spec: &CursorSpec,
    local: Ticks,
    t: Ticks,
    canvas: (f64, f64),
) -> Payload {
    let (width, height) = canvas;
    let hidden = Payload::Cursor {
        asset_id: spec.sprite.asset.id.clone(),
        visible: false,
        tip_px: Point { x: 0.0, y: 0.0 },
        sprite_px: Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        },
    };
    let Some((x, y, sample_visible)) = cursor_state(spec.samples, local) else {
        return hidden;
    };
    let Some((source, dest, crop)) = follow_geometry(composition, spec.follows, t) else {
        return hidden;
    };

    // Source-normalised point to a fraction of the visible crop.
    let u = (x - crop.x) / crop.width;
    let v = (y - crop.y) / crop.height;
    let inside = (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v);

    // Canvas fractions per source pixel, so the sprite keeps source density.
    let per_px_x = dest.width / (crop.width * f64::from(source.width));
    let per_px_y = dest.height / (crop.height * f64::from(source.height));
    let tip = Point {
        x: dest.x + u * dest.width,
        y: dest.y + v * dest.height,
    };
    let scale = spec.scale;
    let hot = Point {
        x: spec.sprite.hotspot.x * scale * per_px_x,
        y: spec.sprite.hotspot.y * scale * per_px_y,
    };
    let sprite_norm = Rect {
        x: tip.x - hot.x,
        y: tip.y - hot.y,
        width: f64::from(spec.sprite.asset.width) * scale * per_px_x,
        height: f64::from(spec.sprite.asset.height) * scale * per_px_y,
    };

    Payload::Cursor {
        asset_id: spec.sprite.asset.id.clone(),
        visible: sample_visible && inside,
        tip_px: Point {
            x: tip.x * width,
            y: tip.y * height,
        },
        sprite_px: to_px(sprite_norm, width, height),
    }
}

/// Sub-frame offsets in ticks for a shutter of `shutter_degrees` sampled
/// `samples` times, centred on the frame time. The frame duration is
/// `TICKS_PER_SECOND * den / num`, exact for NTSC rates, and each offset is
/// rounded to the nearest tick.
fn blur_offsets(samples: u32, shutter_degrees: f64, fps: Rational) -> Vec<i64> {
    let frame_ticks = TICKS_PER_SECOND as f64 * f64::from(fps.den) / f64::from(fps.num);
    let count = f64::from(samples);
    let shutter = shutter_degrees / 360.0;
    (0..samples)
        .map(|k| {
            let u = (f64::from(k) + 0.5) / count - 0.5;
            (u * shutter * frame_ticks).round() as i64
        })
        .collect()
}
