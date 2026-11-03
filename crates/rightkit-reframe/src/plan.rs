//! The reframe plan: smoothed crop keyframes in source pixels, review anchors, and the
//! approval gate (locked invariant: no vertical delivery without an approved plan and
//! approved anchors).

use crate::analyze::Evidence;
use crate::detect::DetectorInfo;
use crate::error::{ReframeError, Result};
use crate::track::{
    build_temporal_track, collapse_segment_anchor, SmoothingParams, TrackPoint, TrackSample,
};
use serde::{Deserialize, Serialize};

pub const PLAN_SCHEMA: &str = "cutright.reframe.plan.v1";

/// A human override: the crop centre for an output-time range (normalized source coords).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ManualAnchor {
    pub start_ms: u64,
    pub end_ms: u64,
    pub center_x: f64,
    pub center_y: f64,
}

#[derive(Debug, Clone)]
pub struct PlanParams {
    pub output_width: u32,
    pub output_height: u32,
    /// 1.0 = the largest 9:16 window that fits; >1 crops tighter (and then follows y too).
    pub zoom: f64,
    pub smoothing: SmoothingParams,
    /// Below this share of confidently-tracked samples the plan falls back to a centre crop.
    pub min_tracked_fraction: f64,
    /// Below this mean confidence over tracked samples likewise.
    pub min_mean_confidence: f64,
    /// Anchors are split at cuts and at most this long.
    pub anchor_window_ms: u64,
    pub manual_anchors: Vec<ManualAnchor>,
    /// Target aspect as `w:h` for the crop window (independent of `output_*` dims).
    pub aspect_w: u32,
    pub aspect_h: u32,
}

impl Default for PlanParams {
    fn default() -> Self {
        Self {
            output_width: 1080,
            output_height: 1920,
            zoom: 1.0,
            smoothing: SmoothingParams::default(),
            min_tracked_fraction: 0.5,
            min_mean_confidence: 0.35,
            anchor_window_ms: 4000,
            manual_anchors: Vec::new(),
            aspect_w: 9,
            aspect_h: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    Tracked,
    /// Tracking confidence was too low; the whole plan is a fixed centre crop.
    CentreFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fallback {
    pub reason: String,
    pub tracked_fraction: f64,
    pub mean_confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStats {
    pub samples: usize,
    pub face_samples: usize,
    pub active_speaker_samples: usize,
    pub saliency_samples: usize,
    pub manual_samples: usize,
    pub gap_samples: usize,
    pub tracked_fraction: f64,
    pub mean_confidence: f64,
}

/// One smoothed crop position: top-left of the crop window in source pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub t_ms: u64,
    pub x: u32,
    pub y: u32,
    pub center_x: f64,
    pub center_y: f64,
    pub confidence: f64,
    /// `face` | `active_speaker` | `saliency` | `manual` | `gap_hold` | `centre_fallback` | `fused`
    pub source: String,
    pub gap: bool,
    pub shot_boundary: bool,
}

/// A reviewable summary of the path over one stretch (a shot, or at most `anchor_window_ms`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub start_ms: u64,
    pub end_ms: u64,
    pub center_x: f64,
    pub center_y: f64,
    pub confidence: f64,
    pub source: String,
    pub gap: bool,
    pub manual: bool,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub approver: String,
    /// RFC 3339, UTC.
    pub approved_at: String,
    /// `plan_hash()` at the moment of approval; any later edit invalidates it.
    pub plan_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReframePlan {
    pub schema: String,
    pub source_blake3: String,
    pub source_width: u32,
    pub source_height: u32,
    /// End of the planned timeline (last keyframe + one sample interval).
    pub duration_ms: u64,
    pub target_aspect: String,
    pub output_width: u32,
    pub output_height: u32,
    pub crop_width: u32,
    pub crop_height: u32,
    pub interval_ms: u64,
    pub detector: DetectorInfo,
    pub mode: PlanMode,
    pub fallback: Option<Fallback>,
    pub stats: PlanStats,
    pub keyframes: Vec<Keyframe>,
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub approval: Option<Approval>,
}

fn q4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn even(v: f64) -> u32 {
    (((v / 2.0).round() as i64).max(0) * 2) as u32
}

/// Largest `aw`:`ah` window (divided by `zoom`) inside a `sw x sh` frame, even-sized.
pub fn crop_size(sw: u32, sh: u32, zoom: f64, aw: u32, ah: u32) -> (u32, u32) {
    let zoom = zoom.max(1.0);
    let aw = aw.max(1) as f64;
    let ah = ah.max(1) as f64;
    let (cw, ch) = if (sw as f64) * ah >= (sh as f64) * aw {
        let ch = sh as f64 / zoom;
        (ch * aw / ah, ch)
    } else {
        let cw = sw as f64 / zoom;
        (cw, cw * ah / aw)
    };
    (even(cw).clamp(2, sw & !1), even(ch).clamp(2, sh & !1))
}

fn crop_origin(center: f64, size: u32, frame: u32) -> u32 {
    let max = (frame - size) as f64;
    even((center * frame as f64 - size as f64 / 2.0).clamp(0.0, max)).min(max as u32)
}

fn source_class(p: &TrackPoint) -> &str {
    p.source.as_str()
}

impl ReframePlan {
    /// Hash of everything a reviewer approves: geometry, keyframes and anchor positions —
    /// not the approval record or the per-anchor approved flags.
    pub fn plan_hash(&self) -> String {
        let mut c = self.clone();
        c.approval = None;
        for a in &mut c.anchors {
            a.approved = false;
        }
        let bytes = serde_json::to_vec(&c).expect("plan serialises");
        rightkit_fs::digest::blake3_bytes(&bytes)
    }

    pub fn approve_anchor(&mut self, index: usize) -> Result<()> {
        self.anchors
            .get_mut(index)
            .ok_or_else(|| ReframeError::Invalid(format!("no anchor {index}")))?
            .approved = true;
        Ok(())
    }

    /// Record approval. Fails unless every anchor has been approved first.
    pub fn approve(
        &mut self,
        approver: &str,
        approved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        if approver.trim().is_empty() {
            return Err(ReframeError::NotApproved("approver is empty".into()));
        }
        if let Some(i) = self.anchors.iter().position(|a| !a.approved) {
            return Err(ReframeError::NotApproved(format!(
                "anchor {i} is not approved"
            )));
        }
        self.approval = Some(Approval {
            approver: approver.trim().to_string(),
            approved_at: approved_at.to_rfc3339(),
            plan_hash: self.plan_hash(),
        });
        Ok(())
    }

    /// Approve every anchor and the plan in one step (the reviewer accepted it as shown).
    pub fn approve_all(
        &mut self,
        approver: &str,
        approved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        for a in &mut self.anchors {
            a.approved = true;
        }
        self.approve(approver, approved_at)
    }

    /// The render gate. Passes only for a recorded approval whose hash still matches the
    /// plan, with every anchor approved.
    pub fn require_approved(&self) -> Result<()> {
        let Some(ap) = &self.approval else {
            return Err(ReframeError::NotApproved("no approval recorded".into()));
        };
        if ap.approver.trim().is_empty() {
            return Err(ReframeError::NotApproved("approver is empty".into()));
        }
        if self.anchors.is_empty() {
            return Err(ReframeError::NotApproved("plan has no anchors".into()));
        }
        if let Some(i) = self.anchors.iter().position(|a| !a.approved) {
            return Err(ReframeError::NotApproved(format!(
                "anchor {i} is not approved"
            )));
        }
        if ap.plan_hash != self.plan_hash() {
            return Err(ReframeError::NotApproved(
                "plan changed after it was approved (hash mismatch)".into(),
            ));
        }
        Ok(())
    }

    /// The smoothed path as `(start_ms, center_x_milli, center_y_milli)` for the timeline's
    /// face-centre fit.
    pub fn crop_anchors_milli(&self) -> Vec<(i64, i32, i32)> {
        let milli = |v: f64| (v.clamp(0.0, 1.0) * 1000.0).round() as i32;
        self.keyframes
            .iter()
            .map(|k| (k.t_ms as i64, milli(k.center_x), milli(k.center_y)))
            .collect()
    }

    /// The same path as the timeline's typed face track.
    pub fn to_face_track(&self) -> rightkit_timeline::FaceTrack {
        rightkit_timeline::FaceTrack {
            anchors: self
                .crop_anchors_milli()
                .into_iter()
                .map(|(t, x, y)| rightkit_timeline::CropAnchor {
                    start_ms: t.max(0),
                    center_x_milli: x,
                    center_y_milli: y,
                })
                .collect(),
        }
    }
}

/// Clamp a normalized centre so the crop window stays inside the frame.
fn clamp_centre(c: f64, half: f64) -> f64 {
    if half >= 0.5 {
        0.5
    } else {
        c.clamp(half, 1.0 - half)
    }
}

/// Build an (unapproved) plan from detection evidence.
pub fn plan_from_evidence(ev: &Evidence, params: &PlanParams) -> Result<ReframePlan> {
    if ev.samples.is_empty() {
        return Err(ReframeError::Invalid("evidence has no samples".into()));
    }
    let (cw, ch) = crop_size(
        ev.source_width,
        ev.source_height,
        params.zoom,
        params.aspect_w,
        params.aspect_h,
    );
    let half_w = cw as f64 / ev.source_width as f64 / 2.0;
    let half_h = ch as f64 / ev.source_height as f64 / 2.0;

    // Keep every target reachable (inside the valid centre range) and apply manual anchors.
    let mut samples: Vec<TrackSample> = ev.samples.clone();
    for s in &mut samples {
        for o in &mut s.observations {
            o.center_x = clamp_centre(o.center_x, half_w);
            o.center_y = clamp_centre(o.center_y, half_h);
        }
        let t = s.output_ms.max(0) as u64;
        s.manual_anchor = params
            .manual_anchors
            .iter()
            .find(|m| t >= m.start_ms && t < m.end_ms)
            .map(|m| {
                (
                    clamp_centre(m.center_x, half_w),
                    clamp_centre(m.center_y, half_h),
                )
            });
    }

    let track = build_temporal_track(&samples, &params.smoothing, half_w, half_h);

    let n = track.len();
    let count = |f: &dyn Fn(&TrackPoint) -> bool| track.iter().filter(|p| f(p)).count();
    let manual = count(&|p| source_class(p) == "manual");
    let saliency = count(&|p| source_class(p) == "saliency");
    let gap = count(&|p| p.gap);
    let speaker = count(&|p| source_class(p) == "active_speaker");
    let face = count(&|p| source_class(p) == "face");
    let tracked: Vec<&TrackPoint> = track
        .iter()
        .filter(|p| !p.gap && source_class(p) != "saliency")
        .collect();
    let tracked_fraction = tracked.len() as f64 / n as f64;
    let mean_confidence = if tracked.is_empty() {
        0.0
    } else {
        tracked.iter().map(|p| p.confidence).sum::<f64>() / tracked.len() as f64
    };

    let fallback_reason = if tracked_fraction < params.min_tracked_fraction {
        Some(format!(
            "subject tracked in only {:.0}% of samples (needs {:.0}%)",
            tracked_fraction * 100.0,
            params.min_tracked_fraction * 100.0
        ))
    } else if mean_confidence < params.min_mean_confidence {
        Some(format!(
            "mean tracking confidence {:.2} is below {:.2}",
            mean_confidence, params.min_mean_confidence
        ))
    } else {
        None
    };

    let stats = PlanStats {
        samples: n,
        face_samples: face,
        active_speaker_samples: speaker,
        saliency_samples: saliency,
        manual_samples: manual,
        gap_samples: gap,
        tracked_fraction: q4(tracked_fraction),
        mean_confidence: q4(mean_confidence),
    };

    let first_ms = track.first().map_or(0, |p| p.output_ms.max(0) as u64);
    let last_ms = track.last().map_or(0, |p| p.output_ms.max(0) as u64);
    let duration_ms = last_ms + ev.interval_ms;

    let (mode, fallback, keyframes, anchors) = if let Some(reason) = fallback_reason {
        let kf = Keyframe {
            t_ms: first_ms,
            x: crop_origin(0.5, cw, ev.source_width),
            y: crop_origin(0.5, ch, ev.source_height),
            center_x: 0.5,
            center_y: 0.5,
            confidence: 0.0,
            source: "centre_fallback".into(),
            gap: false,
            shot_boundary: true,
        };
        let anchor = Anchor {
            start_ms: first_ms,
            end_ms: duration_ms,
            center_x: 0.5,
            center_y: 0.5,
            confidence: 0.0,
            source: "centre_fallback".into(),
            gap: false,
            manual: false,
            approved: false,
        };
        (
            PlanMode::CentreFallback,
            Some(Fallback {
                reason,
                tracked_fraction: q4(tracked_fraction),
                mean_confidence: q4(mean_confidence),
            }),
            vec![kf],
            vec![anchor],
        )
    } else {
        let keyframes: Vec<Keyframe> = track
            .iter()
            .map(|p| Keyframe {
                t_ms: p.output_ms.max(0) as u64,
                x: crop_origin(p.center_x, cw, ev.source_width),
                y: crop_origin(p.center_y, ch, ev.source_height),
                center_x: q4(p.center_x),
                center_y: q4(p.center_y),
                confidence: q4(p.confidence),
                source: p.source.clone(),
                gap: p.gap,
                shot_boundary: p.shot_boundary,
            })
            .collect();
        // Anchor windows: split at cuts and every `anchor_window_ms`.
        let mut bounds: Vec<u64> = vec![first_ms];
        for p in &track {
            let t = p.output_ms.max(0) as u64;
            if (p.shot_boundary && t > *bounds.last().unwrap())
                || t >= bounds.last().unwrap() + params.anchor_window_ms
            {
                bounds.push(t);
            }
        }
        bounds.push(duration_ms);
        let anchors = bounds
            .windows(2)
            .filter(|w| w[1] > w[0])
            .map(|w| {
                let a = collapse_segment_anchor(&track, w[0] as i64, w[1] as i64);
                Anchor {
                    start_ms: w[0],
                    end_ms: w[1],
                    center_x: q4(a.center_x),
                    center_y: q4(a.center_y),
                    confidence: q4(a.confidence),
                    manual: a.source == "manual",
                    source: a.source,
                    gap: a.gap,
                    approved: false,
                }
            })
            .collect();
        (PlanMode::Tracked, None, keyframes, anchors)
    };

    Ok(ReframePlan {
        schema: PLAN_SCHEMA.into(),
        source_blake3: ev.source_blake3.clone(),
        source_width: ev.source_width,
        source_height: ev.source_height,
        duration_ms,
        target_aspect: format!("{}:{}", params.aspect_w, params.aspect_h),
        output_width: params.output_width,
        output_height: params.output_height,
        crop_width: cw,
        crop_height: ch,
        interval_ms: ev.interval_ms,
        detector: ev.detector.clone(),
        mode,
        fallback,
        stats,
        keyframes,
        anchors,
        approval: None,
    })
}
