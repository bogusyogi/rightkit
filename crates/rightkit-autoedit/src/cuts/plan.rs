//! Cut planning: transcript (+ envelope) -> frame-aligned kept ranges with a
//! reason for every removed range.

use rightkit_timeline::{Fps, SourceId, Ticks};
use rightkit_transcript::{EndSource, Transcript};
use serde::{Deserialize, Serialize};

use crate::cuts::config::{CutConfig, Preset};
use crate::cuts::envelope::Envelope;
use crate::cuts::error::CutError;
use crate::cuts::text::{ends_sentence, find_fillers, find_retakes, normalize, W};

const TICKS_PER_MS: i64 = 705_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    Silence,
    Filler,
    Retake,
    Manual,
    /// Transcript-free footage: dead space or weak-segment selection.
    Visual,
}

/// One kept source range (frame-aligned except possibly a final range that
/// ends at the end of the source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptRange {
    pub source_in: Ticks,
    pub source_out: Ticks,
}

impl KeptRange {
    pub fn start_ms(&self) -> i64 {
        self.source_in.to_units(1000)
    }
    pub fn end_ms(&self) -> i64 {
        self.source_out.to_units(1000)
    }
}

/// A removed source range and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    pub source_start: Ticks,
    pub source_end: Ticks,
    /// Highest-priority reason: manual > retake > filler > silence.
    pub reason: RemovalReason,
    /// Indices into [`CutPlan::decisions`] overlapping this range.
    pub decisions: Vec<usize>,
}

/// One engine decision with its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub kind: RemovalReason,
    /// Source-time extent of the evidence (ms).
    pub start_ms: i64,
    pub end_ms: i64,
    /// Matched/removed text (empty for pure silence).
    pub text: String,
    /// For retakes: the later take that was kept.
    pub kept_text: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutPlan {
    pub preset: Preset,
    pub source: SourceId,
    pub fps: Fps,
    pub source_duration_ms: u64,
    pub kept: Vec<KeptRange>,
    /// Indices (into `Transcript::words`) of the words that survive the edit.
    pub kept_words: Vec<usize>,
    pub removed: Vec<Removal>,
    pub decisions: Vec<Decision>,
}

impl CutPlan {
    /// Total kept duration in ticks.
    pub fn kept_ticks(&self) -> Ticks {
        Ticks(
            self.kept
                .iter()
                .map(|k| k.source_out.0 - k.source_in.0)
                .sum(),
        )
    }

    /// Canonical JSON bytes (deterministic).
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("plan serializes")
    }
}

/// Where an edge may legally sit (ms): never inside a neighbouring word.
#[derive(Debug, Clone, Copy)]
struct Edge {
    ms: i64,
    lo: i64,
    hi: i64,
}

struct Range {
    start: Edge,
    end: Edge,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Manual,
    Retake,
    Filler,
}

/// Plans the cuts for `transcript`. `envelope` is per-10 ms RMS from source time 0.
pub fn plan_cuts(
    transcript: &Transcript,
    envelope: Option<&[f32]>,
    cfg: &CutConfig,
    fps: Fps,
) -> Result<CutPlan, CutError> {
    transcript.validate()?;
    let source = SourceId::new(transcript.source_blake3.clone())
        .map_err(|_| CutError::SourceId(transcript.source_blake3.clone()))?;
    let env = envelope.and_then(Envelope::new);
    let duration = transcript.duration_ms as i64;
    let words = refine_words(transcript, env.as_ref(), cfg);
    let n = words.len();
    let mut marks: Vec<Option<Mark>> = vec![None; n];
    let mut decisions: Vec<Decision> = Vec::new();
    let join = |a: usize, b: usize| -> String {
        words[a..b]
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };

    // 1. Manual removals (word-granular).
    for m in &cfg.manual_removals {
        let (ms, me) = (m.start_ms as i64, m.end_ms as i64);
        let hit: Vec<usize> = (0..n)
            .filter(|&i| {
                let mid = (words[i].start + words[i].end) / 2;
                mid >= ms && mid < me
            })
            .collect();
        if let (Some(&a), Some(&b)) = (hit.first(), hit.last()) {
            for &i in &hit {
                marks[i] = Some(Mark::Manual);
            }
            decisions.push(Decision {
                kind: RemovalReason::Manual,
                start_ms: words[a].start,
                end_ms: words[b].end,
                text: join(a, b + 1),
                kept_text: None,
                detail: format!("manual removal {ms}..{me} ms snapped to word edges"),
            });
        }
    }

    // 2. Fillers (configured, case/punct-insensitive, multi-word).
    let mut filler_marked = vec![false; n];
    if cfg.remove_fillers {
        let removed: Vec<bool> = marks.iter().map(Option::is_some).collect();
        for h in find_fillers(&words, &removed, cfg) {
            filler_marked[h.first..h.end].fill(true);
            let side = |p: i64| {
                if p == i64::MAX {
                    "edge".to_string()
                } else {
                    format!("{p} ms")
                }
            };
            decisions.push(Decision {
                kind: RemovalReason::Filler,
                start_ms: words[h.first].start,
                end_ms: words[h.end - 1].end,
                text: join(h.first, h.end),
                kept_text: None,
                detail: if h.context {
                    format!(
                        "isolated filler (pause before {}, after {})",
                        side(h.pause_before),
                        side(h.pause_after)
                    )
                } else {
                    "filler".to_string()
                },
            });
        }
    }

    // 3. Retakes: ignore fillers/manual when comparing; keep the LAST take.
    let eligible: Vec<bool> = (0..n)
        .map(|i| marks[i].is_none() && !filler_marked[i])
        .collect();
    for h in find_retakes(&words, &eligible, &cfg.retake) {
        for i in h.from..h.to {
            if marks[i] != Some(Mark::Manual) {
                marks[i] = Some(Mark::Retake);
                filler_marked[i] = false;
            }
        }
        decisions.push(Decision {
            kind: RemovalReason::Retake,
            start_ms: words[h.from].start,
            end_ms: words[h.to - 1].end,
            text: join(h.from, h.to),
            kept_text: Some(join(h.kept_from, h.kept_end)),
            detail: format!(
                "earlier attempt repeats the later take (similarity {:.2}); last take kept",
                h.similarity
            ),
        });
    }
    // Fillers not swallowed by a retake or manual removal keep their own mark.
    for i in 0..n {
        if marks[i].is_none() && filler_marked[i] {
            marks[i] = Some(Mark::Filler);
        }
    }
    // Drop filler decisions whose words were taken over by a retake/manual removal.
    decisions.retain(|d| {
        d.kind != RemovalReason::Filler
            || (0..n).any(|i| {
                marks[i] == Some(Mark::Filler)
                    && words[i].start >= d.start_ms
                    && words[i].end <= d.end_ms
            })
    });

    let kept_idx: Vec<usize> = (0..n).filter(|&i| marks[i].is_none()).collect();
    let (Some(&first), Some(&last)) = (kept_idx.first(), kept_idx.last()) else {
        return Err(CutError::NothingKept);
    };

    // 4. Gaps between kept words -> cut points.
    let mut ranges: Vec<Range> = Vec::new();
    let prev_end = |i: usize| if i == 0 { 0 } else { words[i - 1].end };
    let next_start = |i: usize| words.get(i + 1).map_or(duration, |w| w.start);

    // Leading edge.
    let lead = leading_start(&words, first, cfg, duration, &mut decisions);
    let mut cur_start = Edge {
        ms: lead,
        lo: prev_end(first),
        hi: words[first].start,
    };
    let env_snap = |ms: i64, lo: i64, hi: i64| -> i64 {
        match &env {
            Some(e) if hi > lo => {
                let w = cfg.snap_window_ms;
                e.quietest((ms - w).max(lo), (ms + w).min(hi), ms)
                    .unwrap_or(ms)
            }
            _ => ms,
        }
    };

    for pair in kept_idx.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (wa, wb) = (&words[a], &words[b]);
        let removed_between = b > a + 1;
        let p = if ends_sentence(&wa.text) {
            cfg.sentence_pause_ms
        } else {
            cfg.max_pause_ms
        };
        let (s_pre, s_post, silence_only);
        if removed_between {
            s_pre = (words[a + 1].start - wa.end).max(0);
            s_post = (wb.start - words[b - 1].end).max(0);
            silence_only = false;
        } else {
            let sil = (wb.start - wa.end).max(0);
            s_pre = sil / 2;
            s_post = sil - sil / 2;
            silence_only = true;
        }
        let sil = s_pre + s_post;
        let retained = if cfg.remove_silence { sil.min(p) } else { sil };
        if silence_only && (!cfg.remove_silence || sil - retained < cfg.min_cut_ms) {
            continue;
        }
        let share = if sil == 0 { 0 } else { retained * s_pre / sil };
        let keep_after = s_pre.min(share.max(cfg.release_pad_ms));
        let keep_before = s_post.min((retained - share).max(cfg.attack_pad_ms));
        let (mut c0, mut c1) = (wa.end + keep_after, wb.start - keep_before);
        if c1 <= c0 {
            continue;
        }
        if silence_only {
            decisions.push(Decision {
                kind: RemovalReason::Silence,
                start_ms: c0,
                end_ms: c1,
                text: String::new(),
                kept_text: None,
                detail: format!("pause {sil} ms shortened to {} ms", sil - (c1 - c0)),
            });
        } else if cfg.remove_silence && sil > retained {
            decisions.push(Decision {
                kind: RemovalReason::Silence,
                start_ms: wa.end + keep_after,
                end_ms: wb.start - keep_before,
                text: String::new(),
                kept_text: None,
                detail: format!("surrounding pause {sil} ms shortened to {retained} ms"),
            });
        }
        // Snap both edges to the quietest envelope frames, never across words.
        let mid = (c0 + c1) / 2;
        let e_lo = wa.end + cfg.release_pad_ms.min(s_pre);
        let e_hi = if silence_only {
            mid
        } else {
            words[a + 1].start
        };
        let s_lo = if silence_only { mid } else { words[b - 1].end };
        let s_hi = wb.start - cfg.attack_pad_ms.min(s_post);
        let (e0, s0) = (c0, c1);
        c0 = env_snap(c0, e_lo.min(c0), e_hi.max(c0));
        c1 = env_snap(c1, s_lo.min(c1), s_hi.max(c1));
        if c1 <= c0 {
            c0 = e0;
            c1 = s0;
        }
        ranges.push(Range {
            start: cur_start,
            end: Edge {
                ms: c0,
                lo: wa.end,
                hi: next_start(a),
            },
        });
        cur_start = Edge {
            ms: c1,
            lo: prev_end(b),
            hi: wb.start,
        };
    }

    // Trailing edge.
    let tail = trailing_end(&words, last, cfg, duration, &mut decisions);
    ranges.push(Range {
        start: cur_start,
        end: Edge {
            ms: tail,
            lo: words[last].end,
            hi: next_start(last),
        },
    });

    // 5. Frame-align (never leaving a neighbouring word's span), merge touching ranges.
    let dur_ticks = Ticks(duration * TICKS_PER_MS);
    let mut kept: Vec<KeptRange> = Vec::new();
    for r in &ranges {
        let s = align_start(&r.start, fps);
        let e = align_end(&r.end, fps, dur_ticks);
        if e <= s {
            continue;
        }
        match kept.last_mut() {
            Some(prev) if s <= prev.source_out => prev.source_out = prev.source_out.max(e),
            _ => kept.push(KeptRange {
                source_in: s,
                source_out: e,
            }),
        }
    }
    if kept.is_empty() {
        return Err(CutError::NothingKept);
    }

    decisions.sort_by(|a, b| {
        (a.start_ms, a.end_ms, a.kind, &a.text).cmp(&(b.start_ms, b.end_ms, b.kind, &b.text))
    });
    let removed = removals(&kept, dur_ticks, &decisions);
    Ok(CutPlan {
        preset: cfg.preset,
        source,
        fps,
        source_duration_ms: transcript.duration_ms,
        kept,
        kept_words: kept_idx,
        removed,
        decisions,
    })
}

/// Refines approximate (`NextTokenStart`) word ends with VAD and the envelope.
fn refine_words(t: &Transcript, env: Option<&Envelope>, cfg: &CutConfig) -> Vec<W> {
    t.words
        .iter()
        .map(|w| {
            let start = w.start_ms as i64;
            let mut end = w.end_ms as i64;
            if w.end_source == EndSource::NextTokenStart {
                let floor = start + cfg.min_word_ms;
                if let Some(s) = t
                    .vad
                    .iter()
                    .find(|s| (s.start_ms as i64) < end && (s.end_ms as i64) > start)
                {
                    if (s.end_ms as i64) < end {
                        end = (s.end_ms as i64).max(floor.min(end));
                    }
                }
                if let Some(le) = env.and_then(|e| e.last_loud_end(start, end)) {
                    if le > start {
                        end = le.max(floor.min(end)).min(end);
                    }
                }
            }
            W {
                start,
                end,
                text: w.text.clone(),
                norm: normalize(&w.text),
            }
        })
        .collect()
}

fn leading_start(
    words: &[W],
    first: usize,
    cfg: &CutConfig,
    _duration: i64,
    decisions: &mut Vec<Decision>,
) -> i64 {
    if !cfg.remove_silence && first == 0 {
        return 0;
    }
    let f = &words[first];
    let (floor, avail) = if first == 0 {
        (0, f.start)
    } else {
        (words[first - 1].end, f.start - words[first - 1].end)
    };
    let keep = avail.min(cfg.lead_in_ms);
    let start = f.start - keep;
    if first == 0 {
        if start <= 0 || start < cfg.min_cut_ms {
            return 0;
        }
        decisions.push(Decision {
            kind: RemovalReason::Silence,
            start_ms: 0,
            end_ms: start,
            text: String::new(),
            kept_text: None,
            detail: format!("leading silence {} ms trimmed to {keep} ms", f.start),
        });
    }
    start.max(floor)
}

fn trailing_end(
    words: &[W],
    last: usize,
    cfg: &CutConfig,
    duration: i64,
    decisions: &mut Vec<Decision>,
) -> i64 {
    let l = &words[last];
    if !cfg.remove_silence && last + 1 == words.len() {
        return duration;
    }
    let (ceil, avail) = match words.get(last + 1) {
        None => (duration, duration - l.end),
        Some(n) => (n.start, n.start - l.end),
    };
    let keep = avail.min(cfg.tail_out_ms);
    let end = l.end + keep;
    if last + 1 == words.len() {
        if duration - end < cfg.min_cut_ms {
            return duration;
        }
        decisions.push(Decision {
            kind: RemovalReason::Silence,
            start_ms: end,
            end_ms: duration,
            text: String::new(),
            kept_text: None,
            detail: format!("trailing silence {avail} ms trimmed to {keep} ms"),
        });
    }
    end.min(ceil)
}

fn frame_ticks(fps: Fps, f: i64) -> Ticks {
    fps.frame_to_ticks(f)
}

/// Nearest frame boundary to `target` inside `[lo, hi]`, if one exists.
fn nearest_frame(fps: Fps, target: Ticks, lo: Ticks, hi: Ticks) -> Option<Ticks> {
    let target = Ticks(target.0.clamp(lo.0, hi.0));
    let f0 = fps.ticks_to_frame(target);
    let mut best: Option<(i64, Ticks)> = None;
    for f in (f0 - 2).max(0)..=f0 + 2 {
        let t = frame_ticks(fps, f);
        if t >= lo && t <= hi {
            let d = (t.0 - target.0).abs();
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, t));
            }
        }
    }
    best.map(|b| b.1)
}

fn align_start(e: &Edge, fps: Fps) -> Ticks {
    let (t, lo, hi) = (
        Ticks(e.ms * TICKS_PER_MS),
        Ticks(e.lo * TICKS_PER_MS),
        Ticks(e.hi * TICKS_PER_MS),
    );
    if let Some(f) = nearest_frame(fps, t, lo.min(hi), hi) {
        return f;
    }
    // No frame inside the legal range: the frame at/before the next word keeps it whole.
    let mut f = fps.ticks_to_frame(hi);
    while f > 0 && frame_ticks(fps, f) > hi {
        f -= 1;
    }
    frame_ticks(fps, f)
}

fn align_end(e: &Edge, fps: Fps, duration: Ticks) -> Ticks {
    let (t, lo, hi) = (
        Ticks(e.ms * TICKS_PER_MS),
        Ticks(e.lo * TICKS_PER_MS),
        Ticks(e.hi * TICKS_PER_MS),
    );
    if t >= duration {
        // Final range reaches the end of the source: use the exact end.
        return duration;
    }
    let aligned = nearest_frame(fps, t, lo, hi.max(lo)).unwrap_or_else(|| {
        // No frame inside the legal range: the frame at/after the word end keeps it whole.
        let mut f = fps.ticks_to_frame(lo);
        while frame_ticks(fps, f) < lo {
            f += 1;
        }
        frame_ticks(fps, f)
    });
    aligned.min(duration)
}

fn removals(kept: &[KeptRange], duration: Ticks, decisions: &[Decision]) -> Vec<Removal> {
    let mut gaps: Vec<(Ticks, Ticks)> = Vec::new();
    let mut cursor = Ticks::ZERO;
    for k in kept {
        if k.source_in > cursor {
            gaps.push((cursor, k.source_in));
        }
        cursor = k.source_out;
    }
    if duration > cursor {
        gaps.push((cursor, duration));
    }
    gaps.into_iter()
        .map(|(s, e)| {
            let ids: Vec<usize> = decisions
                .iter()
                .enumerate()
                .filter(|(_, d)| d.start_ms * TICKS_PER_MS < e.0 && d.end_ms * TICKS_PER_MS > s.0)
                .map(|(i, _)| i)
                .collect();
            let reason = ids
                .iter()
                .map(|&i| decisions[i].kind)
                .max()
                .unwrap_or(RemovalReason::Silence);
            Removal {
                source_start: s,
                source_end: e,
                reason,
                decisions: ids,
            }
        })
        .collect()
}
