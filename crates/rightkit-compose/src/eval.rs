//! Pure per-frame evaluation. CutRight's native compositor and its typed FFmpeg
//! export both consume the [`FrameDescription`] this module returns, so preview
//! and export agree by construction.

use serde::{Deserialize, Serialize};

use crate::error::CompositionError;
use crate::model::{
    AssetRef, ClickShape, Clip, Composition, Content, CursorSample, CursorSprite, Layer, Pattern,
    Point, Rect, Rgba, Style, Timing, Transform,
};
use crate::track::{sample, Easing, Keyframe};
use crate::validate::validate;

pub const FRAME_SCHEMA: &str = "rightkit.frame.v1";

/// Everything a renderer needs to draw one frame. Layers are in stack order,
/// back to front, and include only layers active at `t_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameDescription {
    pub schema: String,
    pub t_ms: u64,
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

/// Sub-frame time offsets, in milliseconds relative to `t_ms`. A renderer
/// evaluates the composition at each offset and averages the results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionBlurFrame {
    pub offsets_ms: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Video {
        asset_id: String,
        source_ms: f64,
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
        source_ms: f64,
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

/// Evaluates the composition at `t_ms`. Validation runs first, so an invalid
/// composition is reported rather than drawn.
pub fn evaluate(
    composition: &Composition,
    t_ms: u64,
) -> Result<FrameDescription, CompositionError> {
    validate(composition)?;
    if t_ms >= composition.duration_ms {
        return Err(CompositionError::OutOfRange {
            t_ms,
            duration_ms: composition.duration_ms,
        });
    }
    let canvas = composition.canvas;
    let (width, height) = (f64::from(canvas.width), f64::from(canvas.height));
    let min_side = width.min(height);

    let mut layers = Vec::new();
    for (stack_index, layer) in composition.layers.iter().enumerate() {
        if !is_active(layer.timing, t_ms) {
            continue;
        }
        let local = t_ms - layer.timing.start_ms;
        let opacity = sample(&layer.opacity, local).unwrap_or(1.0);
        let payload = payload_at(composition, layer, t_ms, (width, height));
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
            offsets_ms: blur_offsets(
                m.samples,
                m.shutter_degrees,
                canvas.frame_rate_num,
                canvas.frame_rate_den,
            ),
        });
        layers.push(FrameLayer {
            id: layer.id.clone(),
            stack_index,
            opacity,
            clip,
            shadow,
            motion_blur,
            payload,
        });
    }

    Ok(FrameDescription {
        schema: FRAME_SCHEMA.to_string(),
        t_ms,
        width: canvas.width,
        height: canvas.height,
        layers,
    })
}

fn is_active(timing: Timing, t_ms: u64) -> bool {
    t_ms >= timing.start_ms && t_ms < timing.end_ms
}

fn payload_at(composition: &Composition, layer: &Layer, t_ms: u64, canvas: (f64, f64)) -> Payload {
    let (width, height) = canvas;
    let local = t_ms - layer.timing.start_ms;
    match &layer.content {
        Content::Video {
            asset,
            placement,
            crop,
            source_start_ms,
            speed,
        } => {
            let (dest, crop) = media_geometry(layer, *placement, crop, local);
            Payload::Video {
                asset_id: asset.id.clone(),
                source_ms: source_time(*source_start_ms, *speed, local, asset),
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
            source_start_ms,
        } => {
            let dest = place(*placement, layer_transform(layer, local));
            Payload::Webcam {
                asset_id: asset.id.clone(),
                source_ms: source_time(*source_start_ms, 1.0, local, asset),
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
            cursor_payload(composition, &spec, local, t_ms, canvas)
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
            onset_ms,
            duration_ms,
            easing,
        } => click_payload(
            *anchor,
            *effect,
            *style,
            (*onset_ms, *duration_ms, *easing),
            local,
            (width, height),
        ),
    }
}

/// Animation progress of a click effect at a layer-local time. Returns
/// `None` before onset; `Some((eased, linear))` while running and `None`
/// once the animation has completed.
fn click_progress(
    onset_ms: u64,
    duration_ms: u64,
    easing: Easing,
    local: u64,
) -> Option<(f64, f64)> {
    if local < onset_ms {
        return None;
    }
    let elapsed = local - onset_ms;
    if elapsed >= duration_ms {
        return None;
    }
    let linear = elapsed as f64 / duration_ms as f64;
    Some((easing.apply_over(linear, duration_ms as f64), linear))
}

fn click_payload(
    anchor: Point,
    effect: ClickShape,
    style: Style,
    (onset_ms, duration_ms, easing): (u64, u64, Easing),
    local: u64,
    canvas: (f64, f64),
) -> Payload {
    let (width, height) = canvas;
    let min_side = width.min(height);
    let centre = Point {
        x: anchor.x * width,
        y: anchor.y * height,
    };
    let progress = click_progress(onset_ms, duration_ms, easing, local);
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

fn layer_transform(layer: &Layer, local: u64) -> Transform {
    sample(&layer.transform, local).unwrap_or(Transform::IDENTITY)
}

/// Destination (canvas fractions) and visible crop (source fractions) of a media layer.
fn media_geometry(
    layer: &Layer,
    placement: Rect,
    crop: &[Keyframe<Rect>],
    local: u64,
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

fn source_time(source_start_ms: u64, speed: f64, local: u64, asset: &AssetRef) -> f64 {
    let raw = source_start_ms as f64 + speed * local as f64;
    match asset.duration_ms {
        Some(duration) => raw.clamp(0.0, duration as f64),
        None => raw.max(0.0),
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
/// (canvas fractions) and crop (source fractions) at `t_ms`. `None` when the
/// layer is inactive at `t_ms`.
fn follow_geometry<'a>(
    composition: &'a Composition,
    follows: &str,
    t_ms: u64,
) -> Option<(&'a AssetRef, Rect, Rect)> {
    let target = composition.layers.iter().find(|l| l.id == follows)?;
    if !is_active(target.timing, t_ms) {
        return None;
    }
    let local = t_ms - target.timing.start_ms;
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
fn cursor_state(samples: &[CursorSample], local: u64) -> Option<(f64, f64, bool)> {
    let first = samples.first()?;
    if local <= first.at_ms {
        return Some((first.x, first.y, first.visible));
    }
    let last = samples.last()?;
    if local >= last.at_ms {
        return Some((last.x, last.y, last.visible));
    }
    let index = samples.partition_point(|s| s.at_ms <= local) - 1;
    let a = &samples[index];
    let b = &samples[index + 1];
    let u = (local - a.at_ms) as f64 / (b.at_ms - a.at_ms) as f64;
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
    local: u64,
    t_ms: u64,
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
    let Some((source, dest, crop)) = follow_geometry(composition, spec.follows, t_ms) else {
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

/// Sub-frame offsets for a shutter of `shutter_degrees` sampled `samples`
/// times, centred on the frame time.
fn blur_offsets(
    samples: u32,
    shutter_degrees: f64,
    frame_rate_num: u32,
    frame_rate_den: u32,
) -> Vec<f64> {
    let frame_ms = 1000.0 * f64::from(frame_rate_den) / f64::from(frame_rate_num);
    let count = f64::from(samples);
    let shutter = shutter_degrees / 360.0;
    (0..samples)
        .map(|k| ((f64::from(k) + 0.5) / count - 0.5) * shutter * frame_ms)
        .collect()
}
