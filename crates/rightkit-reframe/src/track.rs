//! Temporal visual perception and reframing (REV2 plan §15.5 / Phase 7).
//!
//! The old `reframe_plan` sampled exactly one Vision face box at each
//! timeline segment's *midpoint* and used it, unmodified, as the crop center
//! for that whole segment. A subject who moves during the segment drifts out
//! of frame, nobody follows whoever is actually talking, on-screen text can
//! get cropped out, and a hard cut mid-segment (if the source itself has one)
//! gets no visual response at all.
//!
//! This module is the pure, deterministic core that replaces the single
//! midpoint box with a *temporal track*: many samples per segment, each
//! carrying evidence from up to five modalities (faces, active-speaker
//! evidence, body/hands, OCR boxes, saliency), explicit shot-boundary and
//! gap flags, and confidence — fused into one target per sample, then
//! smoothed with a bounded-acceleration integrator so the crop never snaps
//! or jitters, and finally nudged by a safe-zone cost function so on-screen
//! text is not cropped out where that's achievable within the same
//! acceleration bound.
//!
//! Everything in this module is pure (no I/O, no process spawns, no clock
//! reads) so it is exhaustively unit-testable: `reframe.rs` is the only
//! caller, and it owns sampling frames, invoking the Vision worker, loading
//! transcripts/manual anchors, and writing the resulting artifacts.

use serde::{Deserialize, Serialize};

/// Which detector produced a [`ModalityObservation`]. Ordered roughly by
/// authority: active-speaker evidence (who is actually talking, from
/// word-level transcript timing) outranks a raw face box, which outranks a
/// body/hands box, which outranks generic saliency. OCR is deliberately not
/// a modality here — on-screen text is a *constraint* the safe-zone cost
/// function respects, never a thing the crop chases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Face,
    ActiveSpeaker,
    BodyHands,
    Saliency,
}

impl Modality {
    /// Base authority weight before per-observation confidence/size scaling.
    /// Active-speaker evidence dominates a merely-present face (the
    /// "alternating speakers" and "multi-subject handoff" fixtures below);
    /// body/hands and saliency are present mainly to avoid losing the
    /// subject entirely when no face is visible, so they carry much less
    /// weight than face and active-speaker evidence and cannot yank the crop
    /// on their own (the "gesture crossing frame" fixture).
    fn base_weight(self) -> f64 {
        match self {
            Modality::ActiveSpeaker => 3.0,
            Modality::Face => 2.0,
            Modality::BodyHands => 0.6,
            Modality::Saliency => 0.3,
        }
    }
}

/// One detector's evidence at one sampled instant, in normalized `[0, 1]`
/// source-frame coordinates (top-left origin, matching the rest of this
/// pipeline's convention).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModalityObservation {
    pub modality: Modality,
    pub center_x: f64,
    pub center_y: f64,
    /// Detector confidence in `[0, 1]`.
    pub confidence: f64,
    /// Relative size of the detected region (e.g. bounding-box area as a
    /// fraction of frame area). Used only as a tiebreaker among same-modality
    /// observations (bigger/closer subject wins); never compared across
    /// modalities.
    #[serde(default = "default_extent")]
    pub extent: f64,
}

fn default_extent() -> f64 {
    1.0
}

/// A normalized on-screen-text bounding box that the safe-zone cost function
/// tries not to crop out. Never drives the fused target itself.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OcrBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// One sampled instant on the OUTPUT timeline. `reframe.rs` builds one of
/// these per sample per segment; everything downstream in this module is a
/// pure function of a `Vec<TrackSample>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackSample {
    pub output_ms: i64,
    pub source_id: String,
    /// Index returned by the detector for this source frame. It is provenance
    /// only: timing authority stays with Rust's output timeline.
    #[serde(default)]
    pub source_frame_index: Option<u64>,
    /// Continuous Vision sequence identity. A source/track discontinuity
    /// changes this id & resets native sequence state upstream.
    #[serde(default)]
    pub sequence_id: Option<String>,
    /// True at the first sample of a new edited segment (a hard cut the
    /// timeline already knows about) — see [`build_temporal_track`] for why
    /// this resets the smoother instead of being bound by it.
    #[serde(default)]
    pub shot_boundary: bool,
    #[serde(default)]
    pub observations: Vec<ModalityObservation>,
    #[serde(default)]
    pub ocr_boxes: Vec<OcrBox>,
    /// A human-provided override for this exact sample. When present it wins
    /// outright — full confidence, not a gap, and no detector's evidence is
    /// consulted at all. This is the "manual anchors where confidence is
    /// low" requirement: the human overrides, the tracker does not guess.
    #[serde(default)]
    pub manual_anchor: Option<(f64, f64)>,
}

impl TrackSample {
    /// A sample carries no usable evidence when there is no manual override
    /// and every detector came back empty. This is an explicit condition
    /// (checked directly, not inferred from a fused confidence of `0.0`) so
    /// a genuinely low-confidence-but-present detection is never confused
    /// with "nothing was detected here at all".
    fn is_gap(&self) -> bool {
        self.manual_anchor.is_none() && self.observations.is_empty()
    }
}

/// One point on the final, smoothed temporal track — what gets collapsed
/// into per-segment anchors and, in full, written to the
/// `reframe-track.json` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackPoint {
    pub output_ms: i64,
    pub center_x: f64,
    pub center_y: f64,
    /// Confidence of the evidence actually driving this point. `0.0` during
    /// a gap: the position is held (not recentered), but the artifact still
    /// records honestly that nothing was seen here.
    pub confidence: f64,
    pub gap: bool,
    pub shot_boundary: bool,
    /// Which modality (or `"manual"` / `"gap_hold"`) produced this point,
    /// for provenance and human review.
    pub source: String,
}

/// Bounded-acceleration smoothing parameters, in normalized-position units
/// per second / per second squared. Defaults are deliberately conservative:
/// a full-frame traverse (`1.0` normalized units) takes at least ~1.1s at
/// max velocity, and reaching max velocity from rest takes ~0.3s — fast
/// enough to keep up with a walking subject, far short of a whip-pan.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SmoothingParams {
    pub max_velocity: f64,
    pub max_acceleration: f64,
    /// Spring stiffness/damping driving the desired acceleration toward the
    /// fused target before the acceleration clamp is applied.
    pub spring_stiffness: f64,
    pub spring_damping: f64,
    /// Per-gap-sample velocity decay (`0..1`); holds position rather than
    /// coasting indefinitely on stale velocity.
    pub gap_velocity_damping: f64,
    /// Dead zone (normalized distance): while the fused target stays this close to the
    /// current crop centre the crop holds still, so small head wobble never moves the frame.
    pub dead_zone: f64,
}

impl Default for SmoothingParams {
    fn default() -> Self {
        Self {
            max_velocity: 0.9,
            max_acceleration: 3.0,
            spring_stiffness: 8.0,
            spring_damping: 5.0,
            gap_velocity_damping: 0.6,
            dead_zone: 0.02,
        }
    }
}

/// Fusion result for one sample, before smoothing.
struct FusedTarget {
    center: Option<(f64, f64)>,
    confidence: f64,
    source: &'static str,
}

/// Fuse one sample's modality observations into a single target, or `None`
/// if this is a genuine gap. Manual anchors short-circuit everything else.
///
/// Active-speaker evidence is special-cased rather than blended in with the
/// weighted average of every observation: when active-speaker evidence is
/// present, the nearest face observation (if any is close enough to
/// plausibly be the same person) supplies the precise center, and every
/// *other* face is discarded for this sample. Averaging every visible face
/// together regardless of who is speaking is exactly the "biggest/loudest
/// box wins" behavior this phase replaces — the point of active-speaker
/// evidence is to pick *which* subject, not to blend all of them.
fn fuse_sample(sample: &TrackSample) -> FusedTarget {
    if let Some(anchor) = sample.manual_anchor {
        return FusedTarget {
            center: Some(anchor),
            confidence: 1.0,
            source: "manual",
        };
    }
    if sample.is_gap() {
        return FusedTarget {
            center: None,
            confidence: 0.0,
            source: "gap_hold",
        };
    }

    let active_speaker = sample
        .observations
        .iter()
        .filter(|observation| observation.modality == Modality::ActiveSpeaker)
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence));

    if let Some(speaker) = active_speaker {
        const MAX_MATCH_DISTANCE: f64 = 0.35;
        let matched_face = sample
            .observations
            .iter()
            .filter(|observation| observation.modality == Modality::Face)
            .map(|face| {
                let dx = face.center_x - speaker.center_x;
                let dy = face.center_y - speaker.center_y;
                (face, (dx * dx + dy * dy).sqrt())
            })
            .filter(|(_, distance)| *distance <= MAX_MATCH_DISTANCE)
            .min_by(|(_, a), (_, b)| a.total_cmp(b));

        return match matched_face {
            Some((face, _)) => FusedTarget {
                center: Some((face.center_x, face.center_y)),
                confidence: speaker.confidence.max(face.confidence),
                source: "active_speaker",
            },
            None => FusedTarget {
                center: Some((speaker.center_x, speaker.center_y)),
                confidence: speaker.confidence,
                source: "active_speaker",
            },
        };
    }

    // No active-speaker evidence this sample: blend the remaining
    // modalities by authority weight, with face/body observations also
    // weighted by their own detected extent so a large, close subject
    // dominates a small, distant one within the same modality.
    let mut weighted_x = 0.0;
    let mut weighted_y = 0.0;
    let mut weighted_confidence = 0.0;
    let mut total_weight = 0.0;
    let mut dominant: Option<(&ModalityObservation, f64)> = None;
    for observation in &sample.observations {
        if observation.modality == Modality::ActiveSpeaker {
            continue; // handled above; unreachable here but kept explicit.
        }
        let weight = observation.modality.base_weight()
            * observation.confidence.clamp(0.0, 1.0)
            * observation.extent.max(0.01);
        weighted_x += observation.center_x * weight;
        weighted_y += observation.center_y * weight;
        weighted_confidence += observation.confidence * weight;
        total_weight += weight;
        if dominant.is_none_or(|(_, best)| weight > best) {
            dominant = Some((observation, weight));
        }
    }
    if total_weight <= 0.0 {
        return FusedTarget {
            center: None,
            confidence: 0.0,
            source: "gap_hold",
        };
    }
    let source = match dominant.map(|(observation, _)| observation.modality) {
        Some(Modality::Face) => "face",
        Some(Modality::BodyHands) => "body_hands",
        Some(Modality::Saliency) => "saliency",
        Some(Modality::ActiveSpeaker) | None => "fused",
    };
    FusedTarget {
        center: Some((weighted_x / total_weight, weighted_y / total_weight)),
        confidence: (weighted_confidence / total_weight).clamp(0.0, 1.0),
        source,
    }
}

/// Cost of placing a crop window (given as half-extents in normalized units)
/// centered at `candidate` versus the fused `target`, accounting for OCR
/// boxes that must not be clipped. Lower is better. Exposed for tests and for
/// any future caller that wants to score a manual override before saving it.
pub fn safe_zone_cost(
    candidate: (f64, f64),
    target: (f64, f64),
    ocr_boxes: &[OcrBox],
    crop_half_w: f64,
    crop_half_h: f64,
) -> f64 {
    let dx = candidate.0 - target.0;
    let dy = candidate.1 - target.1;
    let distance_cost = (dx * dx + dy * dy).sqrt();

    let (left, right) = (candidate.0 - crop_half_w, candidate.0 + crop_half_w);
    let (top, bottom) = (candidate.1 - crop_half_h, candidate.1 + crop_half_h);
    let clip_cost: f64 = ocr_boxes
        .iter()
        .map(|ocr| {
            let clipped_left = (left - ocr.x0).max(0.0);
            let clipped_right = (ocr.x1 - right).max(0.0);
            let clipped_top = (top - ocr.y0).max(0.0);
            let clipped_bottom = (ocr.y1 - bottom).max(0.0);
            clipped_left + clipped_right + clipped_top + clipped_bottom
        })
        .sum();

    let edge_margin = 0.02;
    let edge_cost = (edge_margin - left.max(0.0)).max(0.0)
        + (edge_margin - (1.0 - right).max(0.0)).max(0.0)
        + (edge_margin - top.max(0.0)).max(0.0)
        + (edge_margin - (1.0 - bottom).max(0.0)).max(0.0);

    // OCR clipping dominates the score (text must not be cropped out where
    // avoidable); a small edge-safety term discourages hugging the source
    // boundary; raw distance from the fused target is the tiebreaker.
    clip_cost * 10.0 + edge_cost * 4.0 + distance_cost
}

/// Search the small set of positions reachable from `current` within one
/// step's acceleration/velocity bounds (the step already computed by the
/// smoother) for the one that minimizes [`safe_zone_cost`] against `target`
/// and `ocr_boxes`. Returns `stepped` unchanged if no reachable nudge is
/// cheaper — the smoother's own bounded motion is left untouched whenever
/// nothing needs to move for OCR safety.
fn nudge_for_safe_zone(
    stepped: (f64, f64),
    current: (f64, f64),
    target: (f64, f64),
    ocr_boxes: &[OcrBox],
    crop_half_w: f64,
    crop_half_h: f64,
    max_step: f64,
) -> (f64, f64) {
    if ocr_boxes.is_empty() {
        return stepped;
    }
    let mut best = stepped;
    let mut best_cost = safe_zone_cost(stepped, target, ocr_boxes, crop_half_w, crop_half_h);
    const CANDIDATE_ANGLES: usize = 12;
    for step_index in 0..CANDIDATE_ANGLES {
        let angle = (step_index as f64 / CANDIDATE_ANGLES as f64) * std::f64::consts::TAU;
        let candidate = (
            (current.0 + max_step * angle.cos()).clamp(0.0, 1.0),
            (current.1 + max_step * angle.sin()).clamp(0.0, 1.0),
        );
        let cost = safe_zone_cost(candidate, target, ocr_boxes, crop_half_w, crop_half_h);
        if cost < best_cost {
            best = candidate;
            best_cost = cost;
        }
    }
    best
}

/// Build the smoothed temporal track from raw samples.
///
/// `samples` must be sorted by `output_ms` (the caller controls sample
/// order; this function does not re-sort, so a caller bug surfaces as a
/// visibly wrong track rather than being silently masked). `dt` between
/// consecutive samples is derived from their `output_ms` delta, so an
/// irregular sample spacing (e.g. denser sampling near a segment boundary)
/// is handled correctly.
///
/// Shot boundaries reset the smoother's velocity to zero and let position
/// jump straight to that sample's fused target with no acceleration bound —
/// a hard cut has no continuity with the previous shot to smooth against, so
/// treating it like ordinary motion would mean the reframed crop visibly
/// drags the old shot's position into the new one for a moment. Every other
/// sample is bounded by `params`.
pub fn build_temporal_track(
    samples: &[TrackSample],
    params: &SmoothingParams,
    crop_half_w: f64,
    crop_half_h: f64,
) -> Vec<TrackPoint> {
    let mut points = Vec::with_capacity(samples.len());
    let mut position: Option<(f64, f64)> = None;
    let mut velocity = (0.0, 0.0);
    let mut previous_ms: Option<i64> = None;

    for sample in samples {
        let fused = fuse_sample(sample);
        let dt = previous_ms
            .map(|previous| ((sample.output_ms - previous).max(0) as f64) / 1_000.0)
            .unwrap_or(0.0)
            .max(1.0 / 240.0); // avoid division blow-ups on duplicate timestamps
        previous_ms = Some(sample.output_ms);

        let gap = fused.center.is_none();
        let point = if sample.shot_boundary {
            velocity = (0.0, 0.0);
            let landing = fused.center.or(position).unwrap_or((0.5, 0.5));
            position = Some(landing);
            TrackPoint {
                output_ms: sample.output_ms,
                center_x: landing.0,
                center_y: landing.1,
                confidence: fused.confidence,
                gap,
                shot_boundary: true,
                source: fused.source.to_string(),
            }
        } else if let Some(target) = fused.center {
            let current = position.unwrap_or(target);
            let dead = ((target.0 - current.0).powi(2) + (target.1 - current.1).powi(2)).sqrt()
                < params.dead_zone;
            let drive = if dead { current } else { target };
            let (stepped, new_velocity) = step_toward_target(current, velocity, drive, dt, params);
            velocity = new_velocity;
            let max_step = params.max_velocity * dt;
            let nudged = nudge_for_safe_zone(
                stepped,
                current,
                target,
                &sample.ocr_boxes,
                crop_half_w,
                crop_half_h,
                max_step,
            );
            position = Some(nudged);
            TrackPoint {
                output_ms: sample.output_ms,
                center_x: nudged.0,
                center_y: nudged.1,
                confidence: fused.confidence,
                gap: false,
                shot_boundary: false,
                source: fused.source.to_string(),
            }
        } else {
            // Explicit gap: hold the last known position (never recenter to
            // 0.5/0.5) and decay velocity so a long gap settles rather than
            // coasting on stale motion.
            velocity = (
                velocity.0 * params.gap_velocity_damping,
                velocity.1 * params.gap_velocity_damping,
            );
            let held = position.unwrap_or((0.5, 0.5));
            TrackPoint {
                output_ms: sample.output_ms,
                center_x: held.0,
                center_y: held.1,
                confidence: 0.0,
                gap: true,
                shot_boundary: false,
                source: "gap_hold".to_string(),
            }
        };
        points.push(point);
    }
    points
}

/// One semi-implicit-Euler step of a critically-damped spring toward
/// `target`, with acceleration and velocity both clamped to `params`. This
/// is the actual "bounded acceleration" mechanism: the spring supplies a
/// *desired* acceleration, which is then hard-clamped before being
/// integrated, so no single step can exceed `max_acceleration` regardless of
/// how far away the target is.
fn step_toward_target(
    current: (f64, f64),
    velocity: (f64, f64),
    target: (f64, f64),
    dt: f64,
    params: &SmoothingParams,
) -> ((f64, f64), (f64, f64)) {
    let desired_ax =
        params.spring_stiffness * (target.0 - current.0) - params.spring_damping * velocity.0;
    let desired_ay =
        params.spring_stiffness * (target.1 - current.1) - params.spring_damping * velocity.1;
    let accel_magnitude = (desired_ax * desired_ax + desired_ay * desired_ay).sqrt();
    let (ax, ay) = if accel_magnitude > params.max_acceleration && accel_magnitude > 0.0 {
        let scale = params.max_acceleration / accel_magnitude;
        (desired_ax * scale, desired_ay * scale)
    } else {
        (desired_ax, desired_ay)
    };

    let mut new_vx = velocity.0 + ax * dt;
    let mut new_vy = velocity.1 + ay * dt;
    let speed = (new_vx * new_vx + new_vy * new_vy).sqrt();
    if speed > params.max_velocity && speed > 0.0 {
        let scale = params.max_velocity / speed;
        new_vx *= scale;
        new_vy *= scale;
    }

    let new_x = (current.0 + new_vx * dt).clamp(0.0, 1.0);
    let new_y = (current.1 + new_vy * dt).clamp(0.0, 1.0);
    ((new_x, new_y), (new_vx, new_vy))
}

/// One segment's collapsed anchor: the confidence-weighted centroid of every
/// smoothed track point that falls within `[output_start_ms, output_end_ms)`,
/// or (if none do — a degenerate zero-length or unsampled segment) the
/// nearest point by time. This is what `reframe.rs` writes into the
/// per-segment `anchors[]` array that `final_render.rs` consumes; the full
/// per-sample resolution lives only in the separate `reframe-track.json`
/// artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentAnchor {
    pub center_x: f64,
    pub center_y: f64,
    pub confidence: f64,
    pub gap: bool,
    pub source: String,
}

pub fn collapse_segment_anchor(
    track: &[TrackPoint],
    output_start_ms: i64,
    output_end_ms: i64,
) -> SegmentAnchor {
    let in_range: Vec<&TrackPoint> = track
        .iter()
        .filter(|point| point.output_ms >= output_start_ms && point.output_ms < output_end_ms)
        .collect();
    if in_range.is_empty() {
        let nearest = track
            .iter()
            .min_by_key(|point| (point.output_ms - (output_start_ms + output_end_ms) / 2).abs());
        return match nearest {
            Some(point) => SegmentAnchor {
                center_x: point.center_x,
                center_y: point.center_y,
                confidence: point.confidence,
                gap: point.gap,
                source: point.source.clone(),
            },
            None => SegmentAnchor {
                center_x: 0.5,
                center_y: 0.5,
                confidence: 0.0,
                gap: true,
                source: "gap_hold".to_string(),
            },
        };
    }
    let weight_of = |point: &TrackPoint| point.confidence.max(0.05);
    let total_weight: f64 = in_range.iter().map(|point| weight_of(point)).sum();
    let center_x = in_range
        .iter()
        .map(|point| point.center_x * weight_of(point))
        .sum::<f64>()
        / total_weight;
    let center_y = in_range
        .iter()
        .map(|point| point.center_y * weight_of(point))
        .sum::<f64>()
        / total_weight;
    let confidence =
        in_range.iter().map(|point| point.confidence).sum::<f64>() / in_range.len() as f64;
    let gap = in_range.iter().all(|point| point.gap);
    // Provenance source of the most-confident contributing point, so a human
    // reviewing the anchor sees what actually drove it.
    let source = in_range
        .iter()
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
        .map(|point| point.source.clone())
        .unwrap_or_else(|| "gap_hold".to_string());
    SegmentAnchor {
        center_x,
        center_y,
        confidence,
        gap,
        source,
    }
}
