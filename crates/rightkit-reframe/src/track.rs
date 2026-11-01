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

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f64, y: f64, confidence: f64) -> ModalityObservation {
        ModalityObservation {
            modality: Modality::Face,
            center_x: x,
            center_y: y,
            confidence,
            extent: 1.0,
        }
    }

    fn speaker(x: f64, y: f64, confidence: f64) -> ModalityObservation {
        ModalityObservation {
            modality: Modality::ActiveSpeaker,
            center_x: x,
            center_y: y,
            confidence,
            extent: 1.0,
        }
    }

    fn body(x: f64, y: f64, confidence: f64) -> ModalityObservation {
        ModalityObservation {
            modality: Modality::BodyHands,
            center_x: x,
            center_y: y,
            confidence,
            extent: 1.0,
        }
    }

    fn sample(ms: i64, observations: Vec<ModalityObservation>) -> TrackSample {
        TrackSample {
            output_ms: ms,
            source_id: "cam-a".into(),
            source_frame_index: None,
            sequence_id: None,
            shot_boundary: false,
            observations,
            ocr_boxes: Vec::new(),
            manual_anchor: None,
        }
    }

    /// Fixture 1 — one moving subject: a single face drifting left to right
    /// across a 2s segment, then holding still for another second. The track
    /// must follow it (end up near the final, now-stationary target) without
    /// ever taking a single step larger than the bounded acceleration/
    /// velocity allow — a bounded-acceleration follower necessarily trails a
    /// moving target somewhat while it is still moving, so the "settles near
    /// the target" assertion is made once the subject has stopped and the
    /// spring has had time to catch up, not mid-motion.
    #[test]
    fn fixture_one_moving_subject_tracks_without_overshoot() {
        let params = SmoothingParams::default();
        let mut samples = Vec::new();
        for step in 0..=20 {
            let t = step as f64 / 20.0;
            let x = 0.15 + 0.7 * t; // 0.15 -> 0.85
            samples.push(sample(step * 100, vec![face(x, 0.5, 0.9)]));
        }
        for step in 21..=30 {
            samples.push(sample(step * 100, vec![face(0.85, 0.5, 0.9)]));
        }
        let track = build_temporal_track(&samples, &params, 0.2, 0.2);
        let last = track.last().unwrap();
        assert!(
            (last.center_x - 0.85).abs() < 0.1,
            "track should end near the final target, got {}",
            last.center_x
        );
        let max_step = track
            .windows(2)
            .map(|pair| (pair[1].center_x - pair[0].center_x).abs())
            .fold(0.0_f64, f64::max);
        let dt = 0.1;
        let allowed = params.max_velocity * dt * 1.05; // small float-slop allowance
        assert!(
            max_step <= allowed,
            "step {max_step} exceeded velocity-bounded allowance {allowed}"
        );
    }

    /// Fixture 2 — alternating speakers: two faces are visible throughout,
    /// but active-speaker evidence flips from the left one to the right one
    /// partway through. The fused target (before smoothing) must follow the
    /// active speaker, not a blend of both faces or whichever is bigger.
    #[test]
    fn fixture_alternating_speakers_follows_active_speaker_not_blend() {
        let early = sample(
            0,
            vec![
                face(0.2, 0.5, 0.95),
                face(0.8, 0.5, 0.9),
                speaker(0.2, 0.5, 0.9),
            ],
        );
        let late = sample(
            1_000,
            vec![
                face(0.2, 0.5, 0.95),
                face(0.8, 0.5, 0.9),
                speaker(0.8, 0.5, 0.85),
            ],
        );
        let early_fused = fuse_sample(&early);
        let late_fused = fuse_sample(&late);
        let (ex, _) = early_fused.center.expect("early fused center");
        let (lx, _) = late_fused.center.expect("late fused center");
        assert!(
            (ex - 0.2).abs() < 0.05,
            "should lock to left speaker, got {ex}"
        );
        assert!(
            (lx - 0.8).abs() < 0.05,
            "should lock to right speaker, got {lx}"
        );
        assert_eq!(early_fused.source, "active_speaker");
        assert_eq!(late_fused.source, "active_speaker");
    }

    /// Fixture 3 — a gesture crossing frame: a hand/body detection sweeps
    /// quickly across the frame while a face stays put nearby. Body/hands
    /// carries much less authority than face, so the smoothed track must
    /// stay close to the steady face rather than being yanked along with the
    /// gesture.
    #[test]
    fn fixture_gesture_crossing_frame_does_not_yank_the_crop() {
        let params = SmoothingParams::default();
        let mut samples = Vec::new();
        for step in 0..=10 {
            let t = step as f64 / 10.0;
            let hand_x = 0.1 + 0.8 * t; // sweeps almost edge to edge
            samples.push(sample(
                step * 100,
                vec![face(0.5, 0.5, 0.95), body(hand_x, 0.5, 0.8)],
            ));
        }
        let track = build_temporal_track(&samples, &params, 0.2, 0.2);
        for point in &track {
            assert!(
                (point.center_x - 0.5).abs() < 0.15,
                "gesture pulled the crop too far from the steady face: {}",
                point.center_x
            );
        }
    }

    /// Fixture 4 — a no-face interval: several consecutive samples have no
    /// detections at all. The track must hold the last known confident
    /// position (never recenter to the frame midpoint) and must record the
    /// interval as an explicit gap with zero confidence, not silently as
    /// "centered, fully confident".
    #[test]
    fn fixture_no_face_interval_holds_position_and_records_gap() {
        let params = SmoothingParams::default();
        let mut samples = vec![
            sample(0, vec![face(0.75, 0.4, 0.9)]),
            sample(100, vec![face(0.75, 0.4, 0.9)]),
        ];
        for step in 2..8 {
            samples.push(sample(step * 100, Vec::new()));
        }
        let track = build_temporal_track(&samples, &params, 0.2, 0.2);
        let last_confident = track[1].center_x;
        for point in &track[2..] {
            assert!(point.gap, "expected an explicit gap flag");
            assert_eq!(point.confidence, 0.0);
            assert!(
                (point.center_x - last_confident).abs() < 0.05,
                "gap should hold position near {last_confident}, got {}",
                point.center_x
            );
            assert!(
                (point.center_x - 0.5).abs() > 0.05,
                "gap must not recenter to the frame midpoint"
            );
        }
    }

    /// Fixture 5 — OCR-heavy screen capture: a face sits far to one side, and
    /// an on-screen text box would be clipped by a crop centered purely on
    /// that face. The safe-zone nudge must move the achievable crop toward
    /// including the text rather than ignoring it.
    #[test]
    fn fixture_ocr_heavy_screen_nudges_toward_text_box() {
        let stepped = (0.85, 0.5);
        let current = (0.85, 0.5);
        let target = (0.85, 0.5);
        let ocr_boxes = vec![OcrBox {
            x0: 0.55,
            y0: 0.45,
            x1: 0.75,
            y1: 0.55,
        }];
        let baseline_cost = safe_zone_cost(stepped, target, &ocr_boxes, 0.2, 0.2);
        let nudged = nudge_for_safe_zone(stepped, current, target, &ocr_boxes, 0.2, 0.2, 0.05);
        let nudged_cost = safe_zone_cost(nudged, target, &ocr_boxes, 0.2, 0.2);
        assert!(
            nudged_cost <= baseline_cost,
            "nudge should not make OCR clipping worse: {nudged_cost} vs {baseline_cost}"
        );
        assert!(
            nudged.0 < stepped.0,
            "nudge should move left toward the text box, got {}",
            nudged.0
        );
    }

    /// Fixture 6 — a rapid cut: a shot boundary lands mid-track with a
    /// target on the opposite side of the frame from where the previous shot
    /// left off. The new shot's first point must land exactly on its own
    /// target with zero smoothing drag — unlike ordinary motion, a cut has
    /// no continuity with what came before.
    #[test]
    fn fixture_rapid_cut_snaps_instantly_at_the_shot_boundary() {
        let params = SmoothingParams::default();
        let mut before = sample(0, vec![face(0.15, 0.5, 0.9)]);
        before.shot_boundary = true;
        let mut settle = sample(200, vec![face(0.15, 0.5, 0.9)]);
        settle.shot_boundary = false;
        let mut after = sample(300, vec![face(0.9, 0.5, 0.9)]);
        after.shot_boundary = true;
        let samples = vec![before, settle, after];
        let track = build_temporal_track(&samples, &params, 0.2, 0.2);
        let cut_point = &track[2];
        assert!(cut_point.shot_boundary);
        assert!(
            (cut_point.center_x - 0.9).abs() < 1e-9,
            "shot boundary must land exactly on its own target, got {}",
            cut_point.center_x
        );
    }

    /// Fixture 7 — multi-subject handoff: two subjects are both on screen for
    /// the whole segment (no shot boundary), and active-speaker evidence
    /// hands off from one to the other mid-segment. Unlike the rapid-cut
    /// fixture, this transition must be smooth (bounded by acceleration),
    /// not an instant jump, because there was no cut.
    #[test]
    fn fixture_multi_subject_handoff_transitions_smoothly_without_a_cut() {
        let params = SmoothingParams::default();
        let mut samples = Vec::new();
        for step in 0..5 {
            samples.push(sample(
                step * 100,
                vec![
                    face(0.2, 0.5, 0.9),
                    face(0.8, 0.5, 0.9),
                    speaker(0.2, 0.5, 0.9),
                ],
            ));
        }
        for step in 5..15 {
            samples.push(sample(
                step * 100,
                vec![
                    face(0.2, 0.5, 0.9),
                    face(0.8, 0.5, 0.9),
                    speaker(0.8, 0.5, 0.9),
                ],
            ));
        }
        let track = build_temporal_track(&samples, &params, 0.2, 0.2);
        assert!(!track.iter().any(|point| point.shot_boundary));
        let max_step = track
            .windows(2)
            .map(|pair| (pair[1].center_x - pair[0].center_x).abs())
            .fold(0.0_f64, f64::max);
        let dt = 0.1;
        let allowed = params.max_velocity * dt * 1.05;
        assert!(
            max_step <= allowed,
            "handoff without a cut must stay within the bounded step, got {max_step}"
        );
        assert!(
            (track.last().unwrap().center_x - 0.8).abs() < 0.1,
            "should have handed off to the new speaker by the end"
        );
    }

    /// Manual anchors override everything: even with strong, unambiguous
    /// face evidence pulling elsewhere, a manual anchor on a sample wins
    /// outright with full confidence and is never blended with detector
    /// evidence.
    #[test]
    fn manual_anchor_overrides_detector_evidence_outright() {
        let mut overridden = sample(0, vec![face(0.1, 0.1, 0.99)]);
        overridden.manual_anchor = Some((0.7, 0.3));
        let fused = fuse_sample(&overridden);
        assert_eq!(fused.center, Some((0.7, 0.3)));
        assert_eq!(fused.confidence, 1.0);
        assert_eq!(fused.source, "manual");
    }

    /// Determinism: identical input samples and parameters must produce a
    /// byte-identical track — required by the REV2 plan ("same source plus
    /// same sample rate ⇒ same track").
    #[test]
    fn identical_samples_and_params_produce_identical_tracks() {
        let params = SmoothingParams::default();
        let samples = vec![
            sample(0, vec![face(0.3, 0.4, 0.9)]),
            sample(100, vec![face(0.4, 0.4, 0.9)]),
            sample(200, Vec::new()),
            sample(300, vec![body(0.6, 0.5, 0.7)]),
        ];
        let first = build_temporal_track(&samples, &params, 0.2, 0.2);
        let second = build_temporal_track(&samples, &params, 0.2, 0.2);
        assert_eq!(first, second);
    }

    /// `collapse_segment_anchor` produces exactly the shape `reframe.rs`
    /// needs for the per-segment `anchors[]` array `final_render.rs`
    /// requires: one weighted-centroid point per segment window.
    #[test]
    fn collapse_segment_anchor_weights_by_confidence_within_window() {
        let track = vec![
            TrackPoint {
                output_ms: 0,
                center_x: 0.2,
                center_y: 0.5,
                confidence: 0.9,
                gap: false,
                shot_boundary: false,
                source: "face".to_string(),
            },
            TrackPoint {
                output_ms: 500,
                center_x: 0.8,
                center_y: 0.5,
                confidence: 0.1,
                gap: false,
                shot_boundary: false,
                source: "face".to_string(),
            },
            TrackPoint {
                output_ms: 1_500,
                center_x: 0.5,
                center_y: 0.5,
                confidence: 0.9,
                gap: false,
                shot_boundary: false,
                source: "face".to_string(),
            },
        ];
        let anchor = collapse_segment_anchor(&track, 0, 1_000);
        assert!(
            anchor.center_x < 0.5,
            "should weight toward the higher-confidence 0.2 point, got {}",
            anchor.center_x
        );
        assert!(!anchor.gap);
    }

    /// Loads each of the seven acceptance fixtures from disk
    /// (`fixtures/reframe/track/v1/valid/*.json`) and asserts they at least
    /// parse into `Vec<TrackSample>` and produce a track with the same
    /// length — the on-disk fixtures are the durable, reviewable artifact
    /// the REV2 plan asks for; the behavior they exercise is proven in
    /// detail by the fixture-specific tests above using the same shapes.
    #[test]
    fn on_disk_acceptance_fixtures_parse_and_build_a_full_length_track() {
        let names = [
            "one_moving_subject",
            "alternating_speakers",
            "gesture_crossing_frame",
            "no_face_interval",
            "ocr_heavy_screen_capture",
            "rapid_cut",
            "multi_subject_handoff",
        ];
        let params = SmoothingParams::default();
        for name in names {
            let path = format!(
                "{}/tests/fixtures/reframe/track/v1/valid/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read fixture {path}: {error}"));
            let samples: Vec<TrackSample> = serde_json::from_str(&raw)
                .unwrap_or_else(|error| panic!("parse fixture {path}: {error}"));
            assert!(!samples.is_empty(), "{name} fixture must not be empty");
            let track = build_temporal_track(&samples, &params, 0.2, 0.2);
            assert_eq!(
                track.len(),
                samples.len(),
                "{name}: track must have one point per sample"
            );
        }
    }
}
