//! Snapshot -> `FrameDescriptor` mapper.
//!
//! Produces the compositor's frame descriptor plus the list of textures the
//! frame-transport layer must supply. The mapper is pure: it never touches the
//! GPU, which keeps timing/mapping tests deterministic and host-independent.

use thiserror::Error;

use crate as rk;
use rightkit_compositor::{
    BlendMode, CanvasClearDescriptor, EffectPassDescriptor, FrameDescriptor, FrameItemDescriptor,
    LayerDescriptor, QuadTransformDescriptor,
};
use rightkit_timeline::{SourceId, Ticks};

use crate::native::base_eval::{self, ActiveBase};
use crate::native::snapshot::{ClipVisual, ComposeSnapshot, LayerTransform};

/// Effect shaders the adapted compositor actually builds pipelines for.
/// Anything else is rejected — never silently approximated.
pub const SUPPORTED_EFFECT_SHADERS: &[&str] = &["gaussian-blur"];

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error("frame {0} is outside the plan's {1} frames")]
    OutOfRange(i64, i64),
    #[error("clip '{clip_id}': {reason}")]
    UnsupportedVisual { clip_id: String, reason: String },
    #[error(
        "clip '{clip_id}': effect shader '{shader}' is not supported (supported: gaussian-blur)"
    )]
    UnsupportedEffect { clip_id: String, shader: String },
    #[error(
        "clip '{clip_id}': mask '{mask}' declared but no mask source exists in the evaluated model"
    )]
    UnsupportedMask { clip_id: String, mask: String },
    #[error("clip '{clip_id}': blend mode '{mode}' is not a recognized compositor blend mode")]
    UnsupportedBlendMode { clip_id: String, mode: String },
    #[error("clip '{clip_id}': transitions are not part of the evaluated model (contiguous ripple only)")]
    UnsupportedTransition { clip_id: String },
    #[error("clip '{clip_id}': invalid visual: {reason}")]
    InvalidVisual { clip_id: String, reason: String },
    #[error("overlay '{0}' references a source the plan does not know")]
    UnknownSource(String),
    #[error("texture request failed: {0}")]
    Transport(String),
}

/// What the frame-transport layer must materialize for a texture id.
/// Decoder/cache ownership stays outside this crate; this is the contract.
#[derive(Debug, Clone, PartialEq)]
pub enum TextureRequirement {
    /// A decoded source frame. `source_time` is the exact tick to decode;
    /// `quality_scale` is the decode-size scale for preview tiers.
    SourceFrame {
        texture_id: String,
        input: usize,
        source: SourceId,
        source_time: Ticks,
        quality_scale: f32,
        /// Source window to sample, as fractions of the source frame.
        /// `Rect::FULL` means the whole frame.
        crop: rk::Rect,
    },
    /// A title raster at full output dimensions (alpha-premultiplied RGBA),
    /// produced by the title rasterizer, not by this adapter.
    RasterizedTitle {
        texture_id: String,
        overlay_id: String,
    },
}

impl TextureRequirement {
    pub fn texture_id(&self) -> &str {
        match self {
            Self::SourceFrame { texture_id, .. } | Self::RasterizedTitle { texture_id, .. } => {
                texture_id
            }
        }
    }
}

/// One output frame's compositor input plus its transport requirements.
#[derive(Debug)]
pub struct ComposedFrame {
    /// Output frame index under the plan's fps.
    pub frame_index: i64,
    /// Output-time position of the frame's start, in ticks (shared rounding).
    pub start: Ticks,
    pub descriptor: FrameDescriptor,
    pub requirements: Vec<TextureRequirement>,
}

fn source_texture_id(input: usize, source_time: Ticks, scale: f32, crop: rk::Rect) -> String {
    // Identity: which source frame, at which decode quality, through which
    // crop window. Two clips of the same source at the same source time share
    // one texture (dedup is correct: the texture is content, not placement).
    if crop == rk::Rect::FULL {
        format!("src:{input}:t{}:q{:.3}", source_time.0, scale)
    } else {
        format!(
            "src:{input}:t{}:q{:.3}:c{:.6},{:.6},{:.6},{:.6}",
            source_time.0, scale, crop.x, crop.y, crop.width, crop.height
        )
    }
}

fn title_texture_id(overlay_id: &str) -> String {
    format!("title:{overlay_id}")
}

fn parse_blend_mode(clip_id: &str, mode: &str) -> Result<BlendMode, ComposeError> {
    let unsupported = || ComposeError::UnsupportedBlendMode {
        clip_id: clip_id.to_string(),
        mode: mode.to_string(),
    };
    // The donor serde repr is kebab-case.
    serde_plain_name(mode).ok_or_else(unsupported)
}

fn serde_plain_name(mode: &str) -> Option<BlendMode> {
    Some(match mode {
        "normal" => BlendMode::Normal,
        "darken" => BlendMode::Darken,
        "multiply" => BlendMode::Multiply,
        "color-burn" => BlendMode::ColorBurn,
        "lighten" => BlendMode::Lighten,
        "screen" => BlendMode::Screen,
        "plus-lighter" => BlendMode::PlusLighter,
        "color-dodge" => BlendMode::ColorDodge,
        "overlay" => BlendMode::Overlay,
        "soft-light" => BlendMode::SoftLight,
        "hard-light" => BlendMode::HardLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

/// True when a clip's visuals are the identity: a held frame of such a clip
/// is the clip's own frame, so a cross-fade may start from it.
pub(crate) fn is_plain_visual(visual: &ClipVisual) -> bool {
    visual.opacity == 1.0
        && visual.transform.is_none()
        && visual.transform_keys.is_empty()
        && !matches!(visual.crop, Some(c) if !c.is_full())
        && visual.crop_keys.is_empty()
        && visual.opacity_keys.is_empty()
        && visual.blend_mode.is_none()
        && visual.mask.is_none()
        && visual.effect_groups.is_empty()
}

/// Returns the clip's visual parameters (defaults when absent) plus its
/// validated blend mode, after rejecting every unsupported feature.
fn validate_visuals<'a>(
    clip_id: &str,
    snapshot: &'a ComposeSnapshot,
) -> Result<(&'a crate::native::snapshot::ClipVisual, BlendMode), ComposeError> {
    static DEFAULT: std::sync::OnceLock<crate::native::snapshot::ClipVisual> =
        std::sync::OnceLock::new();
    let visual = snapshot
        .visuals
        .get(clip_id)
        .unwrap_or_else(|| DEFAULT.get_or_init(Default::default));
    if visual.transform.is_some() && !visual.transform_keys.is_empty() {
        return Err(ComposeError::UnsupportedVisual {
            clip_id: clip_id.to_string(),
            reason: "static transform (rotation/flip) and keyframed transform cannot be combined"
                .to_string(),
        });
    }
    if visual.cross_fade_in_frames < 0 {
        return Err(ComposeError::InvalidVisual {
            clip_id: clip_id.to_string(),
            reason: "cross-fade frames must not be negative".to_string(),
        });
    }
    if visual.cross_fade_in_frames > 1 && !visual.opacity_keys.is_empty() {
        return Err(ComposeError::UnsupportedVisual {
            clip_id: clip_id.to_string(),
            reason: "cross-fade and keyframed opacity cannot be combined".to_string(),
        });
    }
    if let Some(mask) = &visual.mask {
        return Err(ComposeError::UnsupportedMask {
            clip_id: clip_id.to_string(),
            mask: mask.clone(),
        });
    }
    let blend_mode = match &visual.blend_mode {
        Some(mode) => parse_blend_mode(clip_id, mode)?,
        None => BlendMode::Normal,
    };
    for group in &visual.effect_groups {
        for pass in group {
            if !SUPPORTED_EFFECT_SHADERS.contains(&pass.shader.as_str()) {
                return Err(ComposeError::UnsupportedEffect {
                    clip_id: clip_id.to_string(),
                    shader: pass.shader.clone(),
                });
            }
        }
    }
    Ok((visual, blend_mode))
}

fn quad(transform: Option<LayerTransform>, width: u32, height: u32) -> QuadTransformDescriptor {
    match transform {
        Some(t) => QuadTransformDescriptor {
            center_x: t.center[0],
            center_y: t.center[1],
            width: t.size[0],
            height: t.size[1],
            rotation_degrees: t.rotation_degrees,
            flip_x: t.flip_x,
            flip_y: t.flip_y,
        },
        // Default: fill the output frame.
        None => QuadTransformDescriptor {
            center_x: width as f32 / 2.0,
            center_y: height as f32 / 2.0,
            width: width as f32,
            height: height as f32,
            rotation_degrees: 0.0,
            flip_x: false,
            flip_y: false,
        },
    }
}

/// Aspect-preserving letterbox fit of a `src_w`x`src_h` frame into the output.
/// Sources with unknown geometry (audio-only inputs carry 0x0) fill the frame.
fn fit_quad(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> QuadTransformDescriptor {
    if src_w == 0 || src_h == 0 {
        return quad(None, out_w, out_h);
    }
    let scale = (out_w as f32 / src_w as f32).min(out_h as f32 / src_h as f32);
    QuadTransformDescriptor {
        center_x: out_w as f32 / 2.0,
        center_y: out_h as f32 / 2.0,
        width: src_w as f32 * scale,
        height: src_h as f32 * scale,
        rotation_degrees: 0.0,
        flip_x: false,
        flip_y: false,
    }
}

/// Title opacity at frame `f` given in/out fades, all in output frames.
pub fn overlay_opacity(
    item_start: i64,
    item_frames: i64,
    fade_in: i64,
    fade_out: i64,
    f: i64,
) -> f32 {
    let into = f - item_start;
    let remaining = item_frames - into - 1;
    let mut opacity = 1.0f32;
    if fade_in > 0 {
        opacity = opacity.min((into + 1) as f32 / fade_in as f32);
    }
    if fade_out > 0 {
        opacity = opacity.min((remaining + 1) as f32 / fade_out as f32);
    }
    opacity.clamp(0.0, 1.0)
}

/// Maps output frame `frame_index` of `snapshot` to a frame descriptor.
///
/// Supported semantics today (matching the evaluated model): one video track
/// of contiguous source clips, hold/black tails, titles as supplied raster
/// textures with fades, per-clip opacity/transform/flip, keyframed opacity,
/// transform and crop, a cross-fade in from a plain clip, supported effect
/// shaders, duck/gain audio (returned via [`crate::native::audio`], not here).
///
/// Which clip, hold or title is active, its stack order, source time, opacity,
/// keyed transform and crop come from `crate::evaluate` (see
/// `crate::native::base_eval`). Static geometry (letterbox fit, pixel transform,
/// rotation, flip), blend, effects, masks, rejection of unsupported
/// combinations and texture identity stay here.
pub fn compose_frame(
    snapshot: &ComposeSnapshot,
    frame_index: i64,
) -> Result<ComposedFrame, ComposeError> {
    let plan = &snapshot.plan;
    if frame_index < 0 || frame_index >= plan.total_frames {
        return Err(ComposeError::OutOfRange(frame_index, plan.total_frames));
    }
    let (width, height) = snapshot.output.effective_size();
    let scale = snapshot.output.scale;
    let mut items = Vec::new();
    let mut requirements = Vec::new();

    if let Some((card, fill)) = plan.cards.iter().find_map(|card| {
        (frame_index >= card.start_frame && frame_index < card.start_frame + card.frames)
            .then_some(card)
            .and_then(|card| card.fill.as_ref().map(|fill| (card, fill)))
    }) {
        let input = &plan.inputs[fill.input];
        let elapsed = plan.fps.frame_to_ticks(frame_index - card.start_frame);
        let mut source_time = Ticks(fill.source_in.0 + elapsed.0);
        if input.duration.0 > 0 {
            let last = (plan.fps.ticks_to_frame(input.duration) - 1).max(0);
            source_time = source_time.min(plan.fps.frame_to_ticks(last));
        }
        let texture_id = source_texture_id(fill.input, source_time, scale, rk::Rect::FULL);
        requirements.push(TextureRequirement::SourceFrame {
            texture_id: texture_id.clone(),
            input: fill.input,
            source: input.source.clone(),
            source_time,
            quality_scale: scale,
            crop: rk::Rect::FULL,
        });
        // Match push_card's even slot dimensions and aspect-preserving fit.
        let even = |v: i64| ((v.max(2) / 2) * 2) as u32;
        let rw = even(i64::from(width) * fill.rect.width / 1000);
        let rh = even(i64::from(height) * fill.rect.height / 1000);
        let mut transform = fit_quad(input.width, input.height, rw, rh);
        transform.width = (transform.width / 2.0).floor().max(1.0) * 2.0;
        transform.height = (transform.height / 2.0).floor().max(1.0) * 2.0;
        transform.center_x += (i64::from(width) * fill.rect.x / 1000) as f32;
        transform.center_y += (i64::from(height) * fill.rect.y / 1000) as f32;
        items.push(FrameItemDescriptor::Layer(LayerDescriptor {
            texture_id,
            transform,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            effect_pass_groups: Vec::new(),
            mask: None,
        }));
    }

    let active = base_eval::evaluate_base_layers(snapshot, frame_index)?;
    for layer in active {
        match layer {
            ActiveBase::Clip {
                clip_id,
                input,
                source_time,
                opacity,
                crop,
                dest,
            } => {
                let (visual, blend_mode) = validate_visuals(&clip_id, snapshot)?;
                let texture_id = source_texture_id(input, source_time, scale, crop);
                requirements.push(TextureRequirement::SourceFrame {
                    texture_id: texture_id.clone(),
                    input,
                    source: plan.inputs[input].source.clone(),
                    source_time,
                    quality_scale: scale,
                    crop,
                });
                let effect_pass_groups = visual
                    .effect_groups
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|p| EffectPassDescriptor {
                                shader: p.shader.clone(),
                                uniforms: p.uniforms.clone(),
                            })
                            .collect()
                    })
                    .collect();
                let transform = if let Some(d) = dest {
                    // Keyframed transform, already evaluated by rk in pixels.
                    QuadTransformDescriptor {
                        center_x: d.x as f32 + d.width as f32 / 2.0,
                        center_y: d.y as f32 + d.height as f32 / 2.0,
                        width: d.width as f32,
                        height: d.height as f32,
                        rotation_degrees: 0.0,
                        flip_x: false,
                        flip_y: false,
                    }
                } else if visual.transform.is_some() {
                    quad(visual.transform, width, height)
                } else {
                    fit_quad(
                        plan.inputs[input].width,
                        plan.inputs[input].height,
                        width,
                        height,
                    )
                };
                items.push(FrameItemDescriptor::Layer(LayerDescriptor {
                    texture_id,
                    transform,
                    opacity,
                    blend_mode,
                    effect_pass_groups,
                    mask: None,
                }));
            }
            ActiveBase::Hold {
                input,
                source_time,
                opacity,
            } => {
                let texture_id = source_texture_id(input, source_time, scale, rk::Rect::FULL);
                requirements.push(TextureRequirement::SourceFrame {
                    texture_id: texture_id.clone(),
                    input,
                    source: plan.inputs[input].source.clone(),
                    source_time,
                    quality_scale: scale,
                    crop: rk::Rect::FULL,
                });
                items.push(FrameItemDescriptor::Layer(LayerDescriptor {
                    texture_id,
                    transform: fit_quad(
                        plan.inputs[input].width,
                        plan.inputs[input].height,
                        width,
                        height,
                    ),
                    opacity,
                    blend_mode: BlendMode::Normal,
                    effect_pass_groups: Vec::new(),
                    mask: None,
                }));
            }
            ActiveBase::Title { overlay, opacity } => {
                let overlay = &plan.overlays[overlay];
                let texture_id = title_texture_id(&overlay.id);
                requirements.push(TextureRequirement::RasterizedTitle {
                    texture_id: texture_id.clone(),
                    overlay_id: overlay.id.clone(),
                });
                items.push(FrameItemDescriptor::Layer(LayerDescriptor {
                    texture_id,
                    transform: quad(None, width, height),
                    opacity,
                    blend_mode: BlendMode::Normal,
                    effect_pass_groups: Vec::new(),
                    mask: None,
                }));
            }
        }
    }

    Ok(ComposedFrame {
        frame_index,
        start: plan.fps.frame_to_ticks(frame_index),
        descriptor: FrameDescriptor {
            width,
            height,
            clear: CanvasClearDescriptor {
                color: clear_colour(snapshot, frame_index)?,
            },
            items,
        },
        requirements,
    })
}

/// Canvas clear colour at `frame_index`. A card frame is its card's solid colour
/// (its title is an overlay item, already in `items`); every other frame is black.
fn clear_colour(snapshot: &ComposeSnapshot, frame_index: i64) -> Result<[f32; 4], ComposeError> {
    let Some(card) = snapshot
        .plan
        .cards
        .iter()
        .find(|c| frame_index >= c.start_frame && frame_index < c.start_frame + c.frames)
    else {
        return Ok([0.0, 0.0, 0.0, 1.0]);
    };
    let invalid = || ComposeError::InvalidVisual {
        clip_id: card.id.clone(),
        reason: "card colour must be #RRGGBB".to_string(),
    };
    let hex = card
        .colour
        .strip_prefix('#')
        .filter(|h| h.len() == 6 && h.is_ascii())
        .ok_or_else(invalid)?;
    let channel = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16)
            .map(|v| f32::from(v) / 255.0)
            .map_err(|_| invalid())
    };
    Ok([channel(0)?, channel(2)?, channel(4)?, 1.0])
}

/// Pre-switch local evaluation, kept as the parity reference for
/// `parity_tests`. It is the exact body `compose_frame` had before the base
/// layers moved to `rightkit-compose`.
#[cfg(test)]
pub(crate) fn reference_compose_frame(
    snapshot: &ComposeSnapshot,
    frame_index: i64,
) -> Result<ComposedFrame, ComposeError> {
    use rightkit_timeline::plan::VideoSegment;
    let plan = &snapshot.plan;
    if frame_index < 0 || frame_index >= plan.total_frames {
        return Err(ComposeError::OutOfRange(frame_index, plan.total_frames));
    }
    let (width, height) = snapshot.output.effective_size();
    let scale = snapshot.output.scale;
    let mut items = Vec::new();
    let mut requirements = Vec::new();

    // Locate the active video segment.
    let mut cursor = 0i64;
    // Texture id of the most recent clip's last source frame, for Hold.
    let mut last: Option<(usize, SourceId, Ticks)> = None;
    for segment in &plan.video {
        let frames = segment.frames();
        if frame_index >= cursor + frames {
            if let VideoSegment::Clip {
                input, source_in, ..
            } = segment
            {
                last = Some((
                    *input,
                    plan.inputs[*input].source.clone(),
                    Ticks(source_in.0 + plan.fps.frame_to_ticks(frames - 1).0),
                ));
            }
            cursor += frames;
            continue;
        }
        match segment {
            VideoSegment::Clip {
                clip_id,
                input,
                source_in,
                ..
            } => {
                let (visual, blend_mode) = validate_visuals(clip_id, snapshot)?;
                let offset_ticks = plan.fps.frame_to_ticks(frame_index - cursor);
                let source_time = Ticks(source_in.0 + offset_ticks.0);
                let texture_id = source_texture_id(*input, source_time, scale, rk::Rect::FULL);
                requirements.push(TextureRequirement::SourceFrame {
                    texture_id: texture_id.clone(),
                    input: *input,
                    source: plan.inputs[*input].source.clone(),
                    source_time,
                    quality_scale: scale,
                    crop: rk::Rect::FULL,
                });
                let effect_pass_groups = visual
                    .effect_groups
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|p| EffectPassDescriptor {
                                shader: p.shader.clone(),
                                uniforms: p.uniforms.clone(),
                            })
                            .collect()
                    })
                    .collect();
                let transform = if visual.transform.is_some() {
                    quad(visual.transform, width, height)
                } else {
                    fit_quad(
                        plan.inputs[*input].width,
                        plan.inputs[*input].height,
                        width,
                        height,
                    )
                };
                items.push(FrameItemDescriptor::Layer(LayerDescriptor {
                    texture_id,
                    transform,
                    opacity: visual.opacity,
                    blend_mode,
                    effect_pass_groups,
                    mask: None,
                }));
            }
            VideoSegment::Hold { .. } => {
                if let Some((input, source, source_time)) = &last {
                    let texture_id = source_texture_id(*input, *source_time, scale, rk::Rect::FULL);
                    requirements.push(TextureRequirement::SourceFrame {
                        texture_id: texture_id.clone(),
                        input: *input,
                        source: source.clone(),
                        source_time: *source_time,
                        quality_scale: scale,
                        crop: rk::Rect::FULL,
                    });
                    items.push(FrameItemDescriptor::Layer(LayerDescriptor {
                        texture_id,
                        transform: fit_quad(
                            plan.inputs[*input].width,
                            plan.inputs[*input].height,
                            width,
                            height,
                        ),
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        effect_pass_groups: Vec::new(),
                        mask: None,
                    }));
                }
                // Hold with no preceding clip behaves as black.
            }
            VideoSegment::Black { .. } => {}
        }
        break;
    }

    // Overlays: titles are supplied as full-frame rasterized alpha textures;
    // fades become layer opacity.
    for overlay in &plan.overlays {
        let end = overlay.start_frame + overlay.frames;
        if frame_index < overlay.start_frame || frame_index >= end {
            continue;
        }
        let opacity = overlay_opacity(
            overlay.start_frame,
            overlay.frames,
            overlay.fade_in_frames,
            overlay.fade_out_frames,
            frame_index,
        );
        let texture_id = title_texture_id(&overlay.id);
        requirements.push(TextureRequirement::RasterizedTitle {
            texture_id: texture_id.clone(),
            overlay_id: overlay.id.clone(),
        });
        items.push(FrameItemDescriptor::Layer(LayerDescriptor {
            texture_id,
            transform: quad(None, width, height),
            opacity,
            blend_mode: BlendMode::Normal,
            effect_pass_groups: Vec::new(),
            mask: None,
        }));
    }

    Ok(ComposedFrame {
        frame_index,
        start: plan.fps.frame_to_ticks(frame_index),
        descriptor: FrameDescriptor {
            width,
            height,
            clear: CanvasClearDescriptor {
                color: [0.0, 0.0, 0.0, 1.0],
            },
            items,
        },
        requirements,
    })
}
