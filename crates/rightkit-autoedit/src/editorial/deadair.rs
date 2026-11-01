//! Dead air beyond word gaps, measured on the audio envelope (per-10 ms RMS
//! from source time 0): breaths inside pauses, quiet trailing-off speech, and
//! speech away from the microphone. Without an envelope only over-long pauses
//! are reported. Plain pauses stay with the cut engine's silence decisions.

use rightkit_transcript::Transcript;

use crate::editorial::config::EditorialConfig;
use crate::editorial::item::{word_draft, Draft, Evidence, ItemAction, ItemKind, Tier};
use crate::editorial::util::{round2, Wd};

const FRAME_MS: i64 = 10;

struct Env<'a>(&'a [f32]);

impl Env<'_> {
    fn frame(&self, ms: i64) -> usize {
        (ms.max(0) / FRAME_MS) as usize
    }

    fn mean(&self, a: i64, b: i64) -> Option<f32> {
        let (fa, fb) = (self.frame(a), self.frame(b).min(self.0.len()));
        if fb <= fa {
            return None;
        }
        Some(self.0[fa..fb].iter().sum::<f32>() / (fb - fa) as f32)
    }
}

fn percentile(mut v: Vec<f32>, p: usize) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[(v.len() - 1) * p / 100]
}

pub(crate) fn detect(
    transcript: &Transcript,
    words: &[Wd],
    envelope: Option<&[f32]>,
    cfg: &EditorialConfig,
) -> Vec<Draft> {
    let dc = &cfg.dead_air;
    let mut out = Vec::new();
    if !dc.enabled || words.is_empty() {
        return out;
    }
    let duration = transcript.duration_ms as i64;
    // Gaps: head, between words, tail.
    let mut gaps: Vec<(i64, i64)> = Vec::new();
    gaps.push((0, words[0].start));
    for p in words.windows(2) {
        gaps.push((p[0].end, p[1].start));
    }
    gaps.push((words[words.len() - 1].end, duration));

    // Over-long pauses the preset would otherwise keep.
    if !cfg.cuts.remove_silence {
        let keep = (cfg.cuts.max_pause_ms / 2).max(cfg.cuts.release_pad_ms);
        for &(a, b) in &gaps {
            if b - a >= dc.long_pause_ms && b - a > 2 * keep + cfg.cuts.min_cut_ms {
                let (s, e) = (a + keep, b - keep);
                out.push(range_draft(
                    ItemKind::LongPause,
                    s,
                    e,
                    Tier::Auto,
                    0.9,
                    format!("pause of {} ms with silence removal off", b - a),
                    &[("gap_ms", (b - a) as f64)],
                ));
            }
        }
    }

    let Some(env) = envelope.filter(|e| !e.is_empty()).map(Env) else {
        return out;
    };
    // Calibrate: speech level over words, noise floor over everything else.
    let levels: Vec<Option<f32>> = words
        .iter()
        .map(|w| {
            if w.end - w.start >= 80 {
                env.mean(w.start, w.end)
            } else {
                None
            }
        })
        .collect();
    let lv: Vec<f32> = levels.iter().flatten().copied().collect();
    if lv.is_empty() {
        return out;
    }
    let speech = percentile(lv, 50);
    let mut near_word = vec![false; env.0.len()];
    for w in words {
        let (fa, fb) = (env.frame(w.start - 30), env.frame(w.end + 30 + FRAME_MS));
        for slot in near_word.iter_mut().take(fb.min(env.0.len())).skip(fa) {
            *slot = true;
        }
    }
    let quiet_frames: Vec<f32> = env
        .0
        .iter()
        .zip(&near_word)
        .filter(|(v, near)| !**near && v.is_finite())
        .map(|(v, _)| *v)
        .collect();
    let floor = if quiet_frames.is_empty() {
        percentile(env.0.iter().copied().filter(|v| v.is_finite()).collect(), 5)
    } else {
        percentile(quiet_frames, 10)
    };
    if !(speech.is_finite() && floor.is_finite()) || speech <= floor * 1.5 + 1e-6 {
        return out;
    }
    let span = speech - floor;

    // Breaths: runs of mid-level energy inside a gap.
    let (lo, hi) = (floor + dc.breath_lo * span, floor + dc.breath_hi * span);
    let guard = 60;
    for &(a, b) in &gaps {
        let (ga, gb) = (a + guard, b - guard);
        if gb - ga < dc.breath_min_ms {
            continue;
        }
        let in_band = |f: usize| env.0.get(f).is_some_and(|v| *v >= lo && *v <= hi);
        let (fa, fb) = (env.frame(ga), env.frame(gb).min(env.0.len()));
        let mut f = fa;
        while f < fb {
            if !in_band(f) {
                f += 1;
                continue;
            }
            // Run with up to two out-of-band frames bridged.
            let start = f;
            let mut end = f + 1;
            let mut miss = 0;
            let mut g = f + 1;
            while g < fb && miss <= 2 {
                if in_band(g) {
                    end = g + 1;
                    miss = 0;
                } else {
                    miss += 1;
                }
                g += 1;
            }
            f = end;
            let len = (end - start) as i64 * FRAME_MS;
            if len < dc.breath_min_ms {
                continue;
            }
            let pad = 20;
            let s = (start as i64 * FRAME_MS - pad).max(a + cfg.cuts.release_pad_ms);
            let e = (end as i64 * FRAME_MS + pad).min(b - cfg.cuts.attack_pad_ms);
            if e - s < cfg.cuts.min_cut_ms {
                continue;
            }
            let level = env
                .mean(start as i64 * FRAME_MS, end as i64 * FRAME_MS)
                .unwrap_or(0.0);
            let tier = if len >= dc.breath_auto_ms {
                Tier::Auto
            } else {
                Tier::Suggest
            };
            out.push(range_draft(
                ItemKind::Breath,
                s,
                e,
                tier,
                0.5 + 0.4 * (len as f32 / 800.0).min(1.0),
                format!("breath of {len} ms between words"),
                &[
                    ("breath_ms", len as f64),
                    ("level_ratio", round2(((level - floor) / span) as f64)),
                ],
            ));
        }
    }

    // Quiet word runs: low energy (trailing off) vs off-mic.
    let ratio: Vec<Option<f32>> = (0..words.len())
        .map(|i| {
            env.mean(words[i].start, words[i].end)
                .map(|m| ((m - floor) / span).max(0.0))
        })
        .collect();
    let mut i = 0;
    while i < words.len() {
        let quiet = |k: usize| ratio[k].is_some_and(|r| r < dc.quiet_ratio);
        if !quiet(i) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < words.len()
            && quiet(j)
            && words[j].start - words[j - 1].end <= dc.quiet_join_gap_ms
        {
            j += 1;
        }
        let dur = words[j - 1].end - words[i].start;
        if j - i >= dc.quiet_min_words && dur >= dc.quiet_min_ms {
            let rs: Vec<f32> = (i..j).filter_map(|k| ratio[k]).collect();
            let mean = rs.iter().sum::<f32>() / rs.len() as f32;
            let confs: Vec<f32> = (i..j).filter_map(|k| words[k].conf).collect();
            let conf_mean =
                (!confs.is_empty()).then(|| confs.iter().sum::<f32>() / confs.len() as f32);
            let off_mic = mean < dc.off_mic_ratio || conf_mean.is_some_and(|c| c < 0.4);
            let kind = if off_mic {
                ItemKind::OffMic
            } else {
                ItemKind::LowEnergy
            };
            let mut d = word_draft(
                words,
                kind,
                i,
                j,
                Tier::Suggest,
                0.45 + 0.45 * (1.0 - (mean / dc.quiet_ratio).min(1.0)),
                if off_mic {
                    format!(
                        "{} words at {:.0}% of speaking level: away from the mic",
                        j - i,
                        mean * 100.0
                    )
                } else {
                    format!(
                        "{} quiet words at {:.0}% of speaking level",
                        j - i,
                        mean * 100.0
                    )
                },
            );
            d.evidence
                .signals
                .insert("level_ratio".into(), round2(mean as f64));
            d.evidence.signals.insert("duration_ms".into(), dur as f64);
            if let Some(c) = conf_mean {
                d.evidence
                    .signals
                    .insert("mean_confidence".into(), round2(c as f64));
            }
            out.push(d);
        }
        i = j;
    }
    out
}

fn range_draft(
    kind: ItemKind,
    start_ms: i64,
    end_ms: i64,
    tier: Tier,
    confidence: f32,
    reason: String,
    signals: &[(&str, f64)],
) -> Draft {
    Draft {
        kind,
        tier,
        confidence,
        start_ms,
        end_ms,
        reason,
        evidence: Evidence {
            signals: signals.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Evidence::default()
        },
        action: ItemAction::RemoveRange { start_ms, end_ms },
        take: None,
    }
}
