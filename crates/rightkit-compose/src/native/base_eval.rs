//! Base-layer evaluation through `rightkit-compose`.
//!
//! [`crate::native::mapper::compose_frame`] used to locate the timed clip for a frame,
//! build the layer list and evaluate title fades and clip opacity itself. This
//! module adapts the host's compiled [`RenderPlan`] into a
//! [`crate::Composition`] and lets `crate::evaluate_v3`
//! decide which video / hold / title layers are active at the frame, their
//! stack order, the source time and the opacity. `compose_frame` then maps the
//! result back into the unchanged `FrameDescriptor` structure.
//!
//! Keyframed opacity, transform and crop are rk tracks on the clip layer, and a
//! cross-fade in is an rk FramePlay/SignedHold layer for outgoing picture plus
//! opacity keys on incoming clip. What stays the host's: the static quad
//! geometry (letterbox fit and the explicit pixel transform with rotation and
//! flip), blend modes, effects, masks, validation of unsupported combinations,
//! texture identity, decoder and cache.
//!
//! # Timing, source time & opacity
//!
//! RightKit's `Ticks::from_frames_rounded` maps output frames to half-down
//! rounded ticks. Layer timing is `[ft(start), ft(end))`.
//! `RetimeSegment::FramePlay` reads signed source time using relative frame
//! rounding, including non-whole-tick rates & opening-card offsets.
//! `SignedHold` preserves negative source times for held pictures.
//!
//! `Opacity::new` validates static clip opacity before building its track;
//! invalid values return `ComposeError::InvalidVisual`. Keyed opacity remains
//! validated by RightKit. `FrameFade::opacity_track` owns cross-fade & title
//! ramps, including overlapping fades & clipped titles. It computes in `f32`
//! before widening; incoming clip opacity scales those keys in `f64`.

use crate as rk;
use rightkit_timeline::plan::VideoSegment;
use rightkit_timeline::{CardSlot, Ticks};

use crate::native::mapper::{is_plain_visual, ComposeError};
use crate::native::snapshot::{ClipVisual, ComposeSnapshot};

/// What a rk layer stands for, indexed by rk `stack_index`.
#[derive(Debug)]
pub(crate) enum BaseKind {
    Clip {
        clip_id: String,
        input: usize,
        /// True when the clip has keyframed transform: its rk destination is
        /// used as the quad.
        keyed_transform: bool,
    },
    Hold {
        input: usize,
    },
    Title {
        overlay: usize,
    },
}

#[derive(Debug)]
pub(crate) struct BaseComposition {
    pub composition: rk::Composition,
    pub kinds: Vec<BaseKind>,
}

/// One active base layer, in stack order (video first, then titles).
#[derive(Debug)]
pub(crate) enum ActiveBase {
    Clip {
        clip_id: String,
        input: usize,
        source_time: Ticks,
        opacity: f32,
        /// Sampled source window, `Rect::FULL` when uncropped.
        crop: rk::Rect,
        /// Evaluated destination in output pixels, for keyframed transforms.
        dest: Option<rk::Rect>,
    },
    Hold {
        input: usize,
        source_time: Ticks,
        opacity: f32,
    },
    Title {
        overlay: usize,
        opacity: f32,
    },
}

/// Output instant of frame `frame` as an unsigned rk tick count.
fn tk(snapshot: &ComposeSnapshot, frame: i64) -> u64 {
    let fps = rk::Rational::new(snapshot.plan.fps.num, snapshot.plan.fps.den);
    rk::Ticks::from_frames_rounded(frame.max(0) as u64, fps)
        .expect("validated plan frame rate & representable output time")
        .0
}

fn asset(id: String, width: u32, height: u32) -> rk::AssetRef {
    rk::AssetRef {
        id,
        blake3: String::new(),
        // rk requires positive pixel dimensions; audio-only inputs carry 0x0.
        // The geometry is the host's, so the value is not used.
        width: width.max(1),
        height: height.max(1),
        duration: None,
    }
}

/// Fitted placement of a source in canvas fractions (letterbox, as
/// `mapper::fit_quad`). Unknown geometry fills the canvas.
fn fit_fraction(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> rk::Rect {
    if src_w == 0 || src_h == 0 {
        return rk::Rect::FULL;
    }
    let scale = (f64::from(out_w) / f64::from(src_w)).min(f64::from(out_h) / f64::from(src_h));
    let width = f64::from(src_w) * scale / f64::from(out_w);
    let height = f64::from(src_h) * scale / f64::from(out_h);
    rk::Rect {
        x: (1.0 - width) / 2.0,
        y: (1.0 - height) / 2.0,
        width,
        height,
    }
}

/// Checks every clip's cross-fade and keyed visuals before rk sees them, so
/// an unsupported combination is rejected explicitly and never approximated.
pub(crate) fn check_visual_transitions(snapshot: &ComposeSnapshot) -> Result<(), ComposeError> {
    let plan = &snapshot.plan;
    // What precedes the clip being checked: nothing or black (a fade from
    // black is valid), a plain clip (its held last frame is the outgoing
    // picture), or anything else (rejected).
    let mut prev_plain = true;
    for segment in &plan.video {
        if segment.frames() <= 0 {
            continue;
        }
        match segment {
            VideoSegment::Clip { clip_id, .. } => {
                let visual = snapshot.visuals.get(clip_id).cloned().unwrap_or_default();
                let frames = segment.frames();
                let n = visual.cross_fade_in_frames;
                if n > 1 {
                    if n > frames {
                        return Err(ComposeError::InvalidVisual {
                            clip_id: clip_id.clone(),
                            reason: format!("cross-fade of {n} frames exceeds the clip's {frames}"),
                        });
                    }
                    if !prev_plain {
                        return Err(ComposeError::UnsupportedVisual {
                            clip_id: clip_id.clone(),
                            reason: "cross-fade needs a plain clip, black or the start directly before it".to_string(),
                        });
                    }
                    if !visual.opacity_keys.is_empty() {
                        return Err(ComposeError::UnsupportedVisual {
                            clip_id: clip_id.clone(),
                            reason: "cross-fade and keyframed opacity cannot be combined"
                                .to_string(),
                        });
                    }
                }
                prev_plain = is_plain_visual(&visual);
            }
            VideoSegment::Hold { .. } => prev_plain = false,
            VideoSegment::Black { .. } => prev_plain = true,
        }
    }
    Ok(())
}

/// Keyframed crop for a clip: the keyed track, else the static crop as one key.
fn crop_track(visual: &ClipVisual) -> Vec<rk::Keyframe<rk::Rect>> {
    if !visual.crop_keys.is_empty() {
        return visual.crop_keys.clone();
    }
    match visual.crop {
        Some(c) if !c.is_full() => vec![rk::Keyframe {
            at: rk::Ticks(0),
            value: rk::Rect {
                x: f64::from(c.x),
                y: f64::from(c.y),
                width: f64::from(c.width),
                height: f64::from(c.height),
            },
            easing: rk::Easing::Linear,
        }],
        _ => Vec::new(),
    }
}

/// Adapts the plan's video segments and titles into a rk composition.
pub(crate) fn build_composition(
    snapshot: &ComposeSnapshot,
) -> Result<BaseComposition, ComposeError> {
    let plan = &snapshot.plan;
    let (width, height) = snapshot.output.effective_size();
    let total = plan.total_frames;
    let fps = rk::Rational::new(plan.fps.num, plan.fps.den);
    let mut composition = rk::Composition::new(
        rk::Canvas { width, height },
        fps,
        rk::Ticks(tk(snapshot, total)),
    );
    let mut kinds: Vec<BaseKind> = Vec::new();
    let input_asset = |input: usize| {
        let (w, h) = plan
            .inputs
            .get(input)
            .map_or((0, 0), |i| (i.width, i.height));
        asset(format!("in{input}"), w, h)
    };

    // Video segments, contiguous from frame 0. `last` is the most recent clip's
    // last source frame, which a Hold repeats.
    // Programme video starts after the opening cards: `plan.video` covers the
    // programme only, while every timing here is an output frame.
    let mut cursor = snapshot
        .plan
        .cards
        .iter()
        .filter(|c| c.slot == CardSlot::Opening)
        .map(|c| c.frames)
        .sum::<i64>();
    let mut last: Option<(usize, i128)> = None;
    // The clip immediately before the current segment, with its last source
    // frame and exclusive source end. Only a directly preceding clip can be a
    // cross-fade's outgoing picture.
    let mut prev_clip_end: Option<(usize, i128, i128)> = None;
    for segment in &plan.video {
        let frames = segment.frames();
        if frames <= 0 {
            continue;
        }
        let start = cursor;
        cursor += frames;
        if start < total {
            let timing = rk::Timing {
                start: rk::Ticks(tk(snapshot, start)),
                end: rk::Ticks(tk(snapshot, cursor.min(total))),
            };
            let span = timing.end.0 - timing.start.0;
            match segment {
                VideoSegment::Clip {
                    clip_id,
                    input,
                    source_in,
                    ..
                } => {
                    let visual = snapshot.visuals.get(clip_id).cloned().unwrap_or_default();
                    let visual_opacity = visual.opacity;
                    let fade_frames = visual.cross_fade_in_frames;
                    let validated_opacity = || {
                        rk::Opacity::new(f64::from(visual_opacity)).map_err(|error| {
                            ComposeError::InvalidVisual {
                                clip_id: clip_id.clone(),
                                reason: error.to_string(),
                            }
                        })
                    };
                    let opacity = if fade_frames > 1 {
                        // Cross-fade in from the previous clip's handle or held frame.
                        let clip_opacity = validated_opacity()?.as_f64();
                        let fade_start = tk(snapshot, start);
                        let n = fade_frames;
                        let fade_end = tk(snapshot, (start + n).min(total));
                        // A fade from black has no outgoing layer.
                        if let Some((prev_input, held_source, end_source)) = prev_clip_end {
                            let has_handle = i128::from(plan.inputs[prev_input].duration.0)
                                >= end_source + i128::from(tk(snapshot, n));
                            let prev_source = if has_handle { end_source } else { held_source };
                            let fade_timing = rk::Timing {
                                start: rk::Ticks(fade_start),
                                end: rk::Ticks(fade_end),
                            };
                            composition.layers.push(rk::Layer {
                                id: format!("b{}", composition.layers.len()),
                                timing: fade_timing,
                                transform: Vec::new(),
                                opacity: Vec::new(),
                                clip: None,
                                shadow: None,
                                motion_blur: None,
                                content: rk::Content::Video {
                                    asset: input_asset(prev_input),
                                    placement: rk::Rect::FULL,
                                    crop: Vec::new(),
                                    retime: vec![if has_handle {
                                        rk::RetimeSegment::FramePlay {
                                            source_in: rk::SignedTicks(prev_source),
                                            fps,
                                            start_frame: start,
                                            frames: n,
                                        }
                                    } else {
                                        rk::RetimeSegment::SignedHold {
                                            source_at: rk::SignedTicks(prev_source),
                                            duration: rk::Ticks(fade_end - fade_start),
                                        }
                                    }],
                                },
                            });
                            kinds.push(BaseKind::Hold { input: prev_input });
                        }
                        let mut ramp = rk::FrameFade {
                            start_frame: start,
                            frames,
                            fade_in_frames: n,
                            fade_out_frames: 0,
                        }
                        .opacity_track(fps, start, cursor.min(total))
                        .map_err(|error| ComposeError::Transport(format!("cross-fade: {error}")))?;
                        for key in &mut ramp {
                            key.value *= clip_opacity;
                        }
                        ramp
                    } else if !visual.opacity_keys.is_empty() {
                        visual.opacity_keys.clone()
                    } else {
                        validated_opacity()?.constant_track()
                    };
                    let keyed_transform = !visual.transform_keys.is_empty();
                    // Taken here, not before the cross-fade layer above is pushed,
                    // so the two layers never share an id.
                    composition.layers.push(rk::Layer {
                        id: format!("b{}", composition.layers.len()),
                        timing,
                        transform: visual.transform_keys.clone(),
                        opacity,
                        clip: None,
                        shadow: None,
                        motion_blur: None,
                        content: rk::Content::Video {
                            asset: input_asset(*input),
                            placement: fit_fraction(
                                plan.inputs.get(*input).map_or(0, |i| i.width),
                                plan.inputs.get(*input).map_or(0, |i| i.height),
                                width,
                                height,
                            ),
                            crop: crop_track(&visual),
                            retime: vec![rk::RetimeSegment::FramePlay {
                                source_in: rk::SignedTicks(i128::from(source_in.0)),
                                fps,
                                start_frame: start,
                                frames,
                            }],
                        },
                    });
                    kinds.push(BaseKind::Clip {
                        clip_id: clip_id.clone(),
                        input: *input,
                        keyed_transform,
                    });
                }
                VideoSegment::Hold { .. } => {
                    // A Hold with no preceding clip is black: no layer.
                    if let Some((input, source)) = last {
                        composition.layers.push(rk::Layer {
                            id: format!("b{}", composition.layers.len()),
                            timing,
                            transform: Vec::new(),
                            opacity: Vec::new(),
                            clip: None,
                            shadow: None,
                            motion_blur: None,
                            content: rk::Content::Video {
                                asset: input_asset(input),
                                placement: rk::Rect::FULL,
                                crop: Vec::new(),
                                retime: vec![rk::RetimeSegment::SignedHold {
                                    source_at: rk::SignedTicks(source),
                                    duration: rk::Ticks(span),
                                }],
                            },
                        });
                        kinds.push(BaseKind::Hold { input });
                    }
                }
                VideoSegment::Black { .. } => {}
            }
        }
        prev_clip_end = None;
        if let VideoSegment::Clip {
            input, source_in, ..
        } = segment
        {
            let end = i128::from(source_in.0) + i128::from(tk(snapshot, frames - 1));
            last = Some((*input, end));
            prev_clip_end = Some((
                *input,
                end,
                i128::from(source_in.0) + i128::from(tk(snapshot, frames)),
            ));
        }
    }

    // Titles, drawn after (above) the video in plan order.
    for (index, overlay) in plan.overlays.iter().enumerate() {
        let visible_start = overlay.start_frame.max(0);
        let visible_end = overlay
            .start_frame
            .saturating_add(overlay.frames)
            .min(total);
        if overlay.frames <= 0 || visible_start >= visible_end {
            continue;
        }
        let layer_start = tk(snapshot, visible_start);
        let opacity = rk::FrameFade {
            start_frame: overlay.start_frame,
            frames: overlay.frames,
            fade_in_frames: overlay.fade_in_frames,
            fade_out_frames: overlay.fade_out_frames,
        }
        .opacity_track(fps, visible_start, visible_end)
        .map_err(|error| ComposeError::Transport(format!("title fade: {error}")))?;
        composition.layers.push(rk::Layer {
            id: format!("b{}", composition.layers.len()),
            timing: rk::Timing {
                start: rk::Ticks(layer_start),
                end: rk::Ticks(tk(snapshot, visible_end)),
            },
            transform: Vec::new(),
            opacity,
            clip: None,
            shadow: None,
            motion_blur: None,
            content: rk::Content::Still {
                // A title is a full-frame raster, drawn full canvas.
                asset: asset(format!("title{index}"), width, height),
                placement: rk::Rect::FULL,
                crop: Vec::new(),
            },
        });
        kinds.push(BaseKind::Title { overlay: index });
    }

    Ok(BaseComposition { composition, kinds })
}

/// Evaluates the base layers active at `frame_index` through rk. The caller
/// has already range-checked the frame.
pub(crate) fn evaluate_base_layers(
    snapshot: &ComposeSnapshot,
    frame_index: i64,
) -> Result<Vec<ActiveBase>, ComposeError> {
    check_visual_transitions(snapshot)?;
    let built = build_composition(snapshot)?;
    let description = rk::evaluate_v3(&built.composition, rk::Ticks(tk(snapshot, frame_index)))
        .map_err(|e| ComposeError::Transport(format!("base-layer evaluation: {e}")))?;
    let mismatch = |id: &str| {
        ComposeError::Transport(format!(
            "base-layer evaluation: layer '{id}': evaluated payload does not match the adapted layer kind"
        ))
    };
    let mut active = Vec::with_capacity(description.layers.len());
    for layer in &description.layers {
        let Some(kind) = built.kinds.get(layer.stack_index) else {
            return Err(mismatch(&layer.id));
        };
        match (kind, &layer.payload) {
            (
                BaseKind::Clip {
                    clip_id,
                    input,
                    keyed_transform,
                },
                rk::PayloadV3::Video {
                    source,
                    crop,
                    dest_px,
                    ..
                },
            ) => {
                let source_time = Ticks(i64::try_from(source.0).map_err(|_| {
                    ComposeError::Transport("video source time exceeds RightKit i64 clock".into())
                })?);
                active.push(ActiveBase::Clip {
                    clip_id: clip_id.clone(),
                    input: *input,
                    source_time,
                    opacity: layer.opacity as f32,
                    crop: *crop,
                    dest: keyed_transform.then_some(*dest_px),
                });
            }
            (
                BaseKind::Hold { input },
                rk::PayloadV3::Video {
                    source: rk_source, ..
                },
            ) => {
                active.push(ActiveBase::Hold {
                    input: *input,
                    source_time: Ticks(i64::try_from(rk_source.0).map_err(|_| {
                        ComposeError::Transport(
                            "video source time exceeds RightKit i64 clock".into(),
                        )
                    })?),
                    opacity: layer.opacity as f32,
                });
            }
            (BaseKind::Title { overlay }, rk::PayloadV3::Still { .. }) => {
                active.push(ActiveBase::Title {
                    overlay: *overlay,
                    opacity: layer.opacity as f32,
                });
            }
            _ => return Err(mismatch(&layer.id)),
        }
    }
    Ok(active)
}
