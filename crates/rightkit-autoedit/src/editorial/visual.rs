//! Silent and mixed-footage analysis: motion + audio-energy signals ->
//! reviewable [`EditorialPlan`]. Pure computation over typed signals produced
//! by the engine's FFmpeg/FFprobe boundary (see `autoedit_cmd`); identical
//! signals give identical plans, so tests need no media.

use rightkit_timeline::{Fps, SourceId};
use serde::{Deserialize, Serialize};

use crate::editorial::config::VisualConfig;
use crate::editorial::item::{Draft, Evidence, ItemAction, ItemKind, Tier};
use crate::editorial::plan::{assign_ids, EditorialPlan, SCHEMA};
use crate::editorial::EditorialConfig;

/// One motion sample: frame-difference energy at `ms`, in milli-units
/// (ffmpeg `lavfi.scene_score` * 1000 sampled at a fixed stride).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionSample {
    pub ms: i64,
    pub milli: i32,
}

/// Scene/motion/audio evidence for a source with no usable transcript.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VisualSignals {
    /// Source duration (ms).
    pub duration_ms: i64,
    /// Whether the container carries a video stream.
    pub has_video: bool,
    /// Whether the container carries an audio stream.
    pub has_audio: bool,
    /// Per-frame visual-change samples, source-time sorted.
    pub motion: Vec<MotionSample>,
    /// Hard scene-cut times (ms): motion spikes above the cut threshold.
    pub scene_cuts: Vec<i64>,
    /// Per-10 ms RMS audio envelope (same layout as the speech envelope).
    pub envelope: Vec<f32>,
}

/// Median of a sorted copy; `None` on empty.
fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// RMS at `ms` from the per-10 ms envelope.
fn env_at(envelope: &[f32], ms: i64) -> Option<f32> {
    if ms < 0 {
        return None;
    }
    envelope.get((ms / 10) as usize).copied()
}

/// Whether one instant is "dead": visually still AND (when audio exists) quiet.
fn dead_at(s: &VisualSignals, ms: i64, motion_milli: i32, energy_floor: f32) -> bool {
    let still = if s.has_video && !s.motion.is_empty() {
        // Nearest sample at-or-before ms; missing samples (before the first)
        // count as still only when the first sample is still.
        let m = s
            .motion
            .iter()
            .rev()
            .find(|m| m.ms <= ms)
            .or_else(|| s.motion.first());
        m.is_none_or(|m| m.milli <= motion_milli)
    } else {
        true // no usable motion signal: audio decides alone
    };
    let quiet = if s.has_audio && !s.envelope.is_empty() {
        env_at(&s.envelope, ms).is_none_or(|r| r <= energy_floor)
    } else {
        true
    };
    still && quiet
}

/// Runs of dead time `[start_ms, end_ms)` sampled at `stride_ms`.
fn dead_runs(s: &VisualSignals, cfg: &VisualConfig) -> Vec<(i64, i64)> {
    let stride = cfg.sample_stride_ms.max(10);
    let floor = energy_floor(s, cfg);
    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut cur: Option<i64> = None;
    let mut ms = 0;
    while ms < s.duration_ms {
        if dead_at(s, ms, cfg.motion_dead_milli, floor) {
            cur = cur.or(Some(ms));
        } else if let Some(start) = cur.take() {
            runs.push((start, ms));
        }
        ms += stride;
    }
    if let Some(start) = cur {
        runs.push((start, s.duration_ms));
    }
    // Merge runs separated by less than twice the pad: the surviving sliver
    // would be dead space anyway.
    let join = 2 * cfg.dead_pad_ms;
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in runs {
        match merged.last_mut() {
            Some(l) if a - l.1 <= join => l.1 = b,
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Quiet-envelope floor: `energy_dead_ratio` of the median level, with an
/// absolute floor so silent-but-hiss-free tracks still read as quiet.
fn energy_floor(s: &VisualSignals, cfg: &VisualConfig) -> f32 {
    let med = median(s.envelope.iter().map(|&r| r as f64).collect()).unwrap_or(0.0);
    (med as f32 * cfg.energy_dead_ratio).max(cfg.energy_dead_floor)
}

/// Scene-delimited segments with a score in `[0,1]`-ish space.
fn segments(s: &VisualSignals, cfg: &VisualConfig) -> Vec<(i64, i64, f64)> {
    let mut bounds: Vec<i64> = vec![0];
    bounds.extend(
        s.scene_cuts
            .iter()
            .copied()
            .filter(|&t| t > 0 && t < s.duration_ms),
    );
    bounds.push(s.duration_ms);
    bounds.sort_unstable();
    bounds.dedup();
    let motion_med = median(s.motion.iter().map(|m| m.milli as f64).collect()).unwrap_or(0.0);
    let env_floor = energy_floor(s, cfg) as f64;
    let env_med = median(s.envelope.iter().map(|&r| r as f64).collect()).unwrap_or(0.0);
    let mut out = Vec::new();
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a < cfg.min_segment_ms {
            continue;
        }
        let mo: Vec<f64> = s
            .motion
            .iter()
            .filter(|m| m.ms >= a && m.ms < b)
            .map(|m| m.milli as f64)
            .collect();
        let en: Vec<f64> = ((a / 10)..(b / 10))
            .filter_map(|i| s.envelope.get(i as usize))
            .map(|&r| r as f64)
            .collect();
        // Score = motion level relative to median (clamped 0..1) blended with
        // energy level above the dead floor (clamped 0..1). Missing signals
        // contribute neutrally (0.5).
        let motion_score = match (s.has_video, mo.is_empty(), motion_med > 0.0) {
            (false, _, _) | (_, true, _) => 0.5,
            (_, _, false) => 0.0,
            _ => (median(mo).unwrap_or(0.0) / (2.0 * motion_med)).clamp(0.0, 1.0),
        };
        let energy_score = match (s.has_audio, en.is_empty(), env_med > env_floor) {
            (false, _, _) | (_, true, _) => 0.5,
            (_, _, false) => 0.0,
            _ => ((median(en).unwrap_or(0.0) - env_floor) / (env_med - env_floor).max(1e-6))
                .clamp(0.0, 1.0),
        };
        out.push((a, b, 0.5 * motion_score + 0.5 * energy_score));
    }
    out
}

/// Analyses purely visual/audio signals into a reviewable plan for a source
/// with no transcript. Dead space (still + quiet) becomes Auto removals;
/// weak scene segments become Suggest removals for best-segment selection.
pub fn analyze_visual(
    source: &SourceId,
    signals: &VisualSignals,
    cfg: &EditorialConfig,
    fps: Fps,
) -> Result<EditorialPlan, crate::editorial::EditorialError> {
    if signals.duration_ms <= 0 {
        return Err(crate::cuts::CutError::NothingKept.into());
    }
    let mut drafts: Vec<Draft> = Vec::new();
    let floor = energy_floor(signals, &cfg.visual);
    for (a, b) in dead_runs(signals, &cfg.visual) {
        // Keep `dead_pad_ms` of the dead run as handles at both edges; interior
        // or edge-adjacent runs keep one handle only.
        let (mut s0, mut e0) = (a, b);
        if s0 > 0 {
            s0 += cfg.visual.dead_pad_ms;
        }
        if e0 < signals.duration_ms {
            e0 -= cfg.visual.dead_pad_ms;
        }
        if e0 - s0 < cfg.visual.min_dead_ms {
            continue;
        }
        let span = if a <= 0 {
            "leading dead space"
        } else if b >= signals.duration_ms {
            "trailing dead space"
        } else {
            "dead space"
        };
        let mut signals_map = std::collections::BTreeMap::new();
        signals_map.insert("removed_ms".to_string(), (e0 - s0) as f64);
        drafts.push(Draft {
            kind: ItemKind::DeadSpace,
            tier: Tier::Auto,
            confidence: 0.9,
            start_ms: s0,
            end_ms: e0,
            reason: format!("{span}: still picture and audio below the dead floor ({e0}-{s0} ms)",),
            evidence: Evidence {
                notes: vec![format!(
                    "motion <= {} milli, envelope <= {floor:.4} RMS",
                    cfg.visual.motion_dead_milli
                )],
                signals: signals_map,
                ..Evidence::default()
            },
            action: ItemAction::RemoveRange {
                start_ms: s0,
                end_ms: e0,
            },
            take: None,
        });
    }

    // Best-segment selection: weak scene segments are suggested removals.
    let segs = segments(signals, &cfg.visual);
    let med_score = median(segs.iter().map(|s| s.2).collect()).unwrap_or(0.0);
    if med_score > 0.0 {
        let mut weak: Vec<(i64, i64, f64)> = segs
            .into_iter()
            .filter(|(_, _, sc)| *sc < med_score * cfg.visual.weak_ratio as f64)
            .collect();
        weak.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        let budget = (signals.duration_ms as f64 * cfg.visual.max_weak_fraction as f64) as i64;
        let mut spent = 0i64;
        for (a, b, sc) in weak {
            if spent + (b - a) > budget {
                break;
            }
            spent += b - a;
            let mut signals_map = std::collections::BTreeMap::new();
            signals_map.insert("score".to_string(), (sc * 1000.0).round() / 1000.0);
            signals_map.insert(
                "median_score".to_string(),
                (med_score * 1000.0).round() / 1000.0,
            );
            drafts.push(Draft {
                kind: ItemKind::WeakSegment,
                tier: Tier::Suggest,
                confidence: 0.6,
                start_ms: a,
                end_ms: b,
                reason: format!(
                    "low-energy segment (score {sc:.2} vs median {med_score:.2}); candidate for best-segment selection"
                ),
                evidence: Evidence {
                    notes: vec!["scene segment scored by motion + audio energy".to_string()],
                    signals: signals_map,
                    ..Evidence::default()
                },
                action: ItemAction::RemoveRange {
                    start_ms: a,
                    end_ms: b,
                },
                take: None,
            });
        }
    }

    let mut items: Vec<crate::editorial::item::EditorialItem> =
        drafts.into_iter().map(Draft::into_item).collect();
    items.sort_by(|a, b| {
        (a.start_ms, a.end_ms, a.kind, &a.reason).cmp(&(b.start_ms, b.end_ms, b.kind, &b.reason))
    });
    assign_ids(&mut items);
    let mut plan = EditorialPlan {
        retake_escalations: Vec::new(),
        schema: SCHEMA.to_string(),
        id: String::new(),
        source_blake3: source.as_str().to_string(),
        fps,
        source_duration_ms: signals.duration_ms as u64,
        config: cfg.clone(),
        items,
        beats: Vec::new(),
        take_groups: Vec::new(),
    };
    let hash = blake3::hash(&plan.to_json());
    plan.id = format!("ed-{}", &hash.to_hex()[..8]);
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editorial::item::ItemKind;

    fn cfg() -> EditorialConfig {
        EditorialConfig::natural()
    }
    fn fps() -> Fps {
        Fps::new(30, 1).unwrap()
    }
    fn sid() -> SourceId {
        SourceId::new("ab".repeat(32)).unwrap()
    }

    fn sig(duration_ms: i64, motion: &[(i64, i32)], env: &[f32]) -> VisualSignals {
        VisualSignals {
            duration_ms,
            has_video: true,
            has_audio: true,
            motion: motion
                .iter()
                .map(|&(ms, milli)| MotionSample { ms, milli })
                .collect(),
            scene_cuts: Vec::new(),
            envelope: env.to_vec(),
        }
    }

    #[test]
    fn weak_segments_are_suggested_not_auto() {
        // Two lively segments, one flat+quiet segment between scene cuts.
        let mut motion = Vec::new();
        let mut env = vec![0.001f32; 1000];
        for i in 0..40 {
            let ms = i * 250;
            let flat = (4000..6000).contains(&ms);
            motion.push(MotionSample {
                ms,
                milli: if flat { 20 } else { 400 },
            });
            if !flat {
                for e in &mut env[(ms / 10) as usize..((ms + 250) / 10) as usize] {
                    *e = 0.1;
                }
            }
        }
        let mut s = sig(10_000, &[], &env);
        s.motion = motion;
        s.scene_cuts = vec![4000, 6000];
        let plan = analyze_visual(&sid(), &s, &cfg(), fps()).unwrap();
        let weak: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::WeakSegment)
            .collect();
        assert_eq!(weak.len(), 1, "{:?}", plan.items);
        assert_eq!(weak[0].tier, Tier::Suggest);
        assert!(!weak[0].default_accepted());
    }

    #[test]
    fn missing_signals_do_not_panic() {
        let s = sig(5000, &[], &[]); // no motion, no envelope
        let mut s = s;
        s.has_video = false;
        s.has_audio = false;
        let plan = analyze_visual(&sid(), &s, &cfg(), fps()).unwrap();
        assert!(plan.items.is_empty() || plan.items.iter().all(|i| i.end_ms > i.start_ms));
    }
}
