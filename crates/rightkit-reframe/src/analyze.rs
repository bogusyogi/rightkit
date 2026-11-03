//! Evidence gathering: sample frames, detect + track faces, score the active speaker, fall
//! back to motion saliency, and emit the per-sample evidence the tracker fuses.

use crate::detect::{DetectorInfo, FaceDetector};
use crate::error::{ReframeError, Result};
use crate::frame::GrayFrame;
use crate::media::{analysis_size, blake3_file, probe, sample_frames, SampleSpan, Tools};
use crate::track::{Modality, ModalityObservation, TrackSample};
use crate::tracker::{FaceTracker, SpeakerSelector};
use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

pub const EVIDENCE_SCHEMA: &str = "cutright.reframe.evidence.v1";

#[derive(Debug, Clone)]
pub struct AnalyzeOptions {
    /// Target spacing between samples (widened to respect `max_samples`).
    pub interval_ms: u64,
    pub max_samples: usize,
    /// Longest edge of the frames handed to the detector.
    pub max_edge: u32,
    /// Spans of source time to sample; `None` = the whole file. Each span's first sample is a
    /// shot boundary (hard cut). Use this to analyse an edit's keep-ranges on the output
    /// timeline.
    pub spans: Option<Vec<SampleSpan>>,
    /// Speech spans in *source* ms (from the transcript VAD). `None` = unknown, so speech is
    /// assumed throughout and mouth motion alone picks the speaker; `Some(empty)` = no speech.
    pub speech: Option<Vec<(u64, u64)>>,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            interval_ms: 250,
            max_samples: 3000,
            max_edge: 640,
            spans: None,
            speech: None,
        }
    }
}

/// Speech spans (source ms) from a `cutright.transcript.v1`: the VAD spans, or, when the
/// transcript carries none, words merged across gaps shorter than 300 ms.
pub fn speech_spans(t: &Transcript) -> Vec<(u64, u64)> {
    if !t.vad.is_empty() {
        return t.vad.iter().map(|s| (s.start_ms, s.end_ms)).collect();
    }
    let mut out: Vec<(u64, u64)> = Vec::new();
    for w in &t.words {
        match out.last_mut() {
            Some(last) if w.start_ms <= last.1 + 300 => last.1 = last.1.max(w.end_ms),
            _ => out.push((w.start_ms, w.end_ms)),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub frames: usize,
    pub detect_ms_total: f64,
    pub detect_ms_mean: f64,
    pub wall_ms: f64,
}

/// Everything the planner needs; re-planning (new smoothing, manual anchors) never needs
/// another detection pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub schema: String,
    pub source_blake3: String,
    pub source_width: u32,
    pub source_height: u32,
    pub source_duration_ms: u64,
    pub interval_ms: u64,
    pub spans: Vec<SampleSpan>,
    pub detector: DetectorInfo,
    pub timing: Timing,
    pub samples: Vec<TrackSample>,
}

/// Interval after widening so the total sample count stays within `max_samples`.
pub fn effective_interval(spans: &[SampleSpan], interval_ms: u64, max_samples: usize) -> u64 {
    let total: u64 = spans
        .iter()
        .map(|s| s.source_end_ms.saturating_sub(s.source_start_ms))
        .sum();
    // each span can add one rounding sample
    let budget = (max_samples.saturating_sub(spans.len())).max(1) as u64;
    interval_ms.max(total.div_ceil(budget)).max(50)
}

/// Motion-saliency fallback for frames with no face: centroid of inter-frame luma change.
/// Returns `(cx, cy, confidence, extent)`.
fn motion_saliency(now: &GrayFrame, prev: &GrayFrame) -> Option<(f64, f64, f64, f64)> {
    if now.width != prev.width || now.height != prev.height {
        return None;
    }
    const STEP: usize = 4;
    const THRESHOLD: i32 = 18;
    let (mut sx, mut sy, mut n, mut cells) = (0.0f64, 0.0f64, 0u64, 0u64);
    let mut y = 0;
    while y < now.height {
        let mut x = 0;
        while x < now.width {
            cells += 1;
            let i = y * now.width + x;
            if (now.data[i] as i32 - prev.data[i] as i32).abs() >= THRESHOLD {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
            x += STEP;
        }
        y += STEP;
    }
    let mass = n as f64 / cells.max(1) as f64;
    if n == 0 || mass < 0.002 {
        return None;
    }
    Some((
        sx / n as f64 / now.width as f64,
        sy / n as f64 / now.height as f64,
        (mass * 8.0).clamp(0.05, 0.5),
        mass,
    ))
}

pub fn analyze(
    tools: &Tools,
    media: &Path,
    detector: &mut dyn FaceDetector,
    detector_info: DetectorInfo,
    opts: &AnalyzeOptions,
) -> Result<Evidence> {
    let wall = Instant::now();
    let info = probe(tools, media)?;
    let source_blake3 = blake3_file(media)?;
    let spans = match &opts.spans {
        Some(s) if !s.is_empty() => s.clone(),
        Some(_) => return Err(ReframeError::Invalid("analysis spans are empty".into())),
        None => vec![SampleSpan {
            source_start_ms: 0,
            source_end_ms: info.duration_ms,
            output_start_ms: 0,
        }],
    };
    let interval = effective_interval(&spans, opts.interval_ms, opts.max_samples);
    let size = analysis_size(info.width, info.height, opts.max_edge);

    let mut tracker = FaceTracker::default();
    let mut selector = SpeakerSelector::default();
    let mut prev_gray: Option<GrayFrame> = None;
    let mut prev_ms: Option<u64> = None;
    let mut samples: Vec<TrackSample> = Vec::new();
    let mut detect_total = 0.0f64;
    let mut frames = 0usize;

    sample_frames(tools, media, &spans, interval, size, |sf| {
        if sf.shot_boundary {
            tracker.reset();
            selector.reset();
            prev_gray = None;
            prev_ms = None;
        }
        let gray = sf.frame.to_gray();
        let t0 = Instant::now();
        let faces = detector.detect(&sf.frame)?;
        detect_total += t0.elapsed().as_secs_f64() * 1000.0;
        frames += 1;

        let dt = prev_ms.map_or(interval as f32 / 1000.0, |p| {
            sf.source_ms.saturating_sub(p) as f32 / 1000.0
        });
        let ids = tracker.update(&faces, dt);
        tracker.observe_mouths(&ids, &faces, &gray, prev_gray.as_ref());

        let speaking = match &opts.speech {
            None => true,
            Some(spans) => spans
                .iter()
                .any(|(s, e)| *s <= sf.source_ms && sf.source_ms < *e),
        };

        let mut observations: Vec<ModalityObservation> = Vec::new();
        for f in &faces {
            let (cx, cy) = f.center();
            observations.push(ModalityObservation {
                modality: Modality::Face,
                center_x: cx as f64,
                center_y: cy as f64,
                confidence: f.score as f64,
                extent: f.area() as f64,
            });
        }
        if speaking && !faces.is_empty() {
            let candidates: Vec<(u32, f32, f32)> = ids
                .iter()
                .zip(&faces)
                .map(|(id, f)| (*id, tracker.energy(*id), f.area()))
                .collect();
            if let Some((id, dominance)) = selector.pick(&candidates) {
                if let Some(idx) = ids.iter().position(|i| *i == id) {
                    let f = &faces[idx];
                    let (cx, cy) = f.center();
                    observations.push(ModalityObservation {
                        modality: Modality::ActiveSpeaker,
                        center_x: cx as f64,
                        center_y: cy as f64,
                        confidence: (0.5 + 0.5 * dominance as f64).max(f.score as f64 * 0.8),
                        extent: f.area() as f64,
                    });
                }
            }
        } else if faces.is_empty() {
            selector.reset();
            if let Some(prev) = prev_gray.as_ref() {
                if let Some((cx, cy, conf, extent)) = motion_saliency(&gray, prev) {
                    observations.push(ModalityObservation {
                        modality: Modality::Saliency,
                        center_x: cx,
                        center_y: cy,
                        confidence: conf,
                        extent,
                    });
                }
            }
        }

        samples.push(TrackSample {
            output_ms: sf.output_ms as i64,
            source_id: "source".into(),
            source_frame_index: Some(sf.index as u64),
            sequence_id: None,
            shot_boundary: sf.shot_boundary,
            observations,
            ocr_boxes: Vec::new(),
            manual_anchor: None,
        });
        prev_gray = Some(gray);
        prev_ms = Some(sf.source_ms);
        Ok(())
    })?;

    Ok(Evidence {
        schema: EVIDENCE_SCHEMA.into(),
        source_blake3,
        source_width: info.width,
        source_height: info.height,
        source_duration_ms: info.duration_ms,
        interval_ms: interval,
        spans,
        detector: detector_info,
        timing: Timing {
            frames,
            detect_ms_total: detect_total,
            detect_ms_mean: if frames > 0 {
                detect_total / frames as f64
            } else {
                0.0
            },
            wall_ms: wall.elapsed().as_secs_f64() * 1000.0,
        },
        samples,
    })
}

/// Convenience: fetch/verify the YuNet model in the shared store, load it on the best
/// execution provider, and analyse.
pub fn analyze_with_yunet(
    tools: &Tools,
    media: &Path,
    models_root: &Path,
    app_id: &str,
    opts: &AnalyzeOptions,
) -> Result<Evidence> {
    let model = crate::models::ensure_yunet(models_root, app_id)?;
    let mut det = crate::detect::YuNet::load(&model)?;
    let info = det.info();
    analyze(tools, media, &mut det, info, opts)
}
