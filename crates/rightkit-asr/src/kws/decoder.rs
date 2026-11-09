//! Keyword modified beam search, ported from Sherpa-ONNX 1.13.4
//! `transducer-keyword-decoder.cc` and `hypothesis.{h,cc}` for one stream, with the
//! HeardRight active-path expiry built in (formerly
//! `0001-expire-active-keyword-hypotheses.patch`).
//!
//! Floating-point order follows the C++: scores are `f32` per frame and `f64` across
//! frames, hypothesis merges use a double log-add, and the joiner runs once per frame
//! on every live hypothesis (its int8 graph quantizes the whole batch together, so the
//! batch must match Sherpa's for identical logits). [`JoinerBatching::PerHypothesis`],
//! [`ThresholdTokens::MatchedKeyword`] and [`MergeTimestamps::Likelier`] depart from
//! Sherpa on request. The decoder, unlike the joiner, computes each row independently of
//! its batch (fp32), so `decoder_cache` reuses rows across frames without changing a bit.

use super::context_graph::{ContextGraph, ROOT};
use super::math::Math;
use super::model::{EncoderOut, Zipformer2Transducer};
use super::trace::{
    keyword_first_frame, FiringTrace, StepTrace, TraceExtension, TraceMatch, TraceMerge,
    TraceGapDrop, TracePath, TraceState, TraceTop, MAX_STEP_EXTENSIONS,
};
use super::{JoinerBatching, MergeTimestamps, ThresholdTokens};
use crate::error::Result;

/// Decoder settings fixed at spotter creation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecoderOptions {
    pub max_active_paths: usize,
    pub num_trailing_blanks: i32,
    pub unk_id: Option<i64>,
    pub active_path_ttl_frames: Option<i32>,
    pub softmax_f64: bool,
    pub threshold_tokens: ThresholdTokens,
    pub joiner_batching: JoinerBatching,
    pub merge_timestamps: MergeTimestamps,
    pub math: Math,
    /// Serve decoder rows from the model's per-context cache
    /// ([`Zipformer2Transducer::run_decoder_cached`]).
    pub decoder_cache: bool,
    /// Stale-token gap guard: drop a path whose newest keyword token is more than this many
    /// encoder frames after the previous one. `None` (the default) is Sherpa's behaviour.
    pub stale_gap_frames: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hypothesis {
    pub ys: Vec<i64>,
    pub timestamps: Vec<i32>,
    pub ys_probs: Vec<f32>,
    pub log_prob: f64,
    pub context_state: usize,
    pub num_trailing_blanks: i32,
}

impl Hypothesis {
    fn blank(context_size: usize) -> Self {
        Self {
            ys: blanks(context_size),
            timestamps: Vec::new(),
            ys_probs: Vec::new(),
            log_prob: 0.0,
            context_state: ROOT,
            num_trailing_blanks: 0,
        }
    }
}

fn blanks(context_size: usize) -> Vec<i64> {
    let mut ys = vec![-1; context_size];
    if let Some(last) = ys.last_mut() {
        *last = 0;
    }
    ys
}

/// Sherpa's `Hypotheses`: hypotheses keyed by token sequence. Adding an existing key
/// log-adds the score and keeps the first hypothesis's other fields
/// ([`MergeTimestamps::First`]), or the likelier one's ([`MergeTimestamps::Likelier`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Hypotheses(Vec<Hypothesis>);

impl Hypotheses {
    pub(crate) fn single(h: Hypothesis) -> Self {
        Self(vec![h])
    }

    /// `observe`, when given (trace only: the sink and the history cap), receives one
    /// record per merge; it never feeds back into the result.
    fn add(
        &mut self,
        h: Hypothesis,
        math: Math,
        merge: MergeTimestamps,
        observe: Option<(&mut Vec<TraceMerge>, usize)>,
    ) {
        match self.0.iter_mut().find(|e| e.ys == h.ys) {
            Some(existing) => {
                let log_prob = log_add(existing.log_prob, h.log_prob, math);
                let keep_incoming =
                    merge == MergeTimestamps::Likelier && h.log_prob > existing.log_prob;
                if let Some((events, cap)) = observe {
                    events.push(TraceMerge::observe(existing, &h, keep_incoming, log_prob, cap));
                }
                if keep_incoming {
                    *existing = h;
                }
                existing.log_prob = log_prob;
            }
            None => self.0.push(h),
        }
    }

    /// First hypothesis with the highest score (`std::max_element`).
    fn most_probable(&self) -> &Hypothesis {
        let mut best = &self.0[0];
        for h in &self.0[1..] {
            if best.log_prob < h.log_prob {
                best = h;
            }
        }
        best
    }
}

/// `log(DBL_EPSILON)`, Sherpa's `SHERPA_ONNX_MIN_LOG_DIFF_DOUBLE`.
const MIN_LOG_DIFF: f64 = -36.043_653_389_117_15;

/// `LogAdd<double>`.
fn log_add(x: f64, y: f64, math: Math) -> f64 {
    let (big, diff) = if x < y { (y, x - y) } else { (x, y - x) };
    if diff >= MIN_LOG_DIFF {
        big + math.ln_1p(math.exp(diff))
    } else {
        big
    }
}

/// `LogSoftmax<float>` over one row. Production Sherpa builds (libc++, MSVC) resolve its
/// `exp`/`log` to the float functions; the GCC build resolves them to the double ones.
pub(crate) fn log_softmax(row: &mut [f32], f64_math: bool, math: Math) {
    let mut m = row[0];
    for &x in &row[1..] {
        if m < x {
            m = x;
        }
    }
    let mut sum = 0.0f32;
    let offset = if f64_math {
        for &x in row.iter() {
            sum = (sum as f64 + math.exp((x - m) as f64)) as f32;
        }
        (m as f64 + math.ln(sum as f64)) as f32
    } else {
        for &x in row.iter() {
            sum += math.expf(x - m);
        }
        m + math.lnf(sum)
    };
    for x in row.iter_mut() {
        *x -= offset;
    }
}

/// `TopkIndex`: indices of the `k` largest values, largest first. Exact ties keep the
/// lower index first (the C++ `partial_sort` leaves tie order unspecified).
fn topk_index(values: &[f32], k: usize) -> Vec<usize> {
    let k = k.min(values.len());
    let mut idx: Vec<usize> = (0..values.len()).collect();
    let order = |a: &usize, b: &usize| {
        values[*b]
            .partial_cmp(&values[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(b))
    };
    if k < idx.len() && k > 0 {
        idx.select_nth_unstable_by(k - 1, order);
        idx.truncate(k);
    }
    idx.sort_by(order);
    idx.truncate(k);
    idx
}

/// What the beam knew when the reported keyword fired; Sherpa keeps none of it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Firing {
    /// The `context_size` tokens before the keyword's first token: the decoder context
    /// that token was emitted in.
    pub context: Vec<i64>,
    /// Probability of each keyword token when it was emitted.
    pub token_probs: Vec<f32>,
    /// The mean probability that passed `threshold` (per [`ThresholdTokens`]).
    pub score: f32,
    pub threshold: f32,
    /// Encoder frame (since reset) the keyword fired on.
    pub frame: i32,
    /// Paths the stale-token gap guard dropped since the last reset, up to this step.
    pub gap_drops: u64,
    /// Of those, the ones dropped in the chunk this keyword fired in, up to this step.
    pub gap_drops_chunk: u64,
    /// Onset estimates, filled in by the spotter when onset estimation is on.
    pub onset: Option<super::onset::KeywordOnset>,
}

/// Per-stream decoder state (`TransducerKeywordResult`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecoderResult {
    /// Encoder frames decoded since the last reset.
    pub frame_offset: i32,
    pub tokens: Vec<i64>,
    pub keyword: String,
    pub num_trailing_blanks: i32,
    pub timestamps: Vec<i32>,
    pub hyps: Hypotheses,
    /// The firing `tokens`/`timestamps` describe; `None` before the first.
    pub firing: Option<Firing>,
    /// Keywords fired since the last reset.
    pub firings: u64,
    /// Paths dropped by the stale-token gap guard since the last reset.
    pub gap_drops: u64,
}

impl DecoderResult {
    pub(crate) fn new(context_size: usize) -> Self {
        Self {
            frame_offset: 0,
            tokens: Vec::new(),
            keyword: String::new(),
            num_trailing_blanks: 0,
            timestamps: Vec::new(),
            hyps: Hypotheses::single(Hypothesis::blank(context_size)),
            firing: None,
            firings: 0,
            gap_drops: 0,
        }
    }

    pub(crate) fn empty() -> Self {
        Self {
            frame_offset: 0,
            tokens: Vec::new(),
            keyword: String::new(),
            num_trailing_blanks: 0,
            timestamps: Vec::new(),
            hyps: Hypotheses::default(),
            firing: None,
            firings: 0,
            gap_drops: 0,
        }
    }
}

/// The mean token probability a keyword's threshold is compared against.
/// [`ThresholdTokens::Upstream`] averages the FIRST `level` probabilities of the path
/// (Sherpa's behaviour, which reads the wrong tokens when a fail transition left earlier
/// tokens on the path); [`ThresholdTokens::MatchedKeyword`] averages the keyword's own last
/// `level`. Both divide by `level`, in the same float order.
fn keyword_score(ys_probs: &[f32], level: usize, mode: ThresholdTokens) -> f32 {
    let probs = match mode {
        ThresholdTokens::Upstream => &ys_probs[..level.min(ys_probs.len())],
        ThresholdTokens::MatchedKeyword => &ys_probs[ys_probs.len().saturating_sub(level)..],
    };
    let mut sum = 0.0f32;
    for p in probs {
        sum += *p;
    }
    sum / level as f32
}

/// The stale-token gap guard's test, on plain values: `Some(gap)` when a path now at keyword
/// depth `level` (at least 2) has a newest consecutive keyword-token pair more than `limit`
/// frames apart (strictly greater; `limit` frames apart is kept). Only the newest pair is
/// checked, because every earlier pair was checked when its token was emitted. `None` for
/// the guard off, a path not on a keyword (`level` under 2), or one with too few timestamps.
fn stale_gap_exceeds(timestamps: &[i32], level: usize, limit: Option<i32>) -> Option<i32> {
    let limit = limit?;
    if level < 2 || timestamps.len() < level {
        return None;
    }
    let n = timestamps.len();
    let gap = timestamps[n - 1] - timestamps[n - 2];
    (gap > limit).then_some(gap)
}

/// A path whose active keyword context began more than `ttl` frames ago.
fn expired(h: &Hypothesis, graph: &ContextGraph, current_frame: i32, ttl: i32) -> bool {
    let level = graph.state(h.context_state).level;
    if level == 0 || h.timestamps.len() < level {
        return false;
    }
    current_frame - h.timestamps[h.timestamps.len() - level] > ttl
}

/// Decode one encoder chunk, updating `result` in place. `trace`, when present, only
/// observes: it is never read to decide anything.
pub(crate) fn decode_chunk(
    model: &mut Zipformer2Transducer,
    graph: &ContextGraph,
    options: &DecoderOptions,
    encoder_out: &EncoderOut,
    result: &mut DecoderResult,
    mut trace: Option<&mut TraceState>,
) -> Result<()> {
    let vocab = model.vocab_size();
    let context_size = model.context_size();
    let mut cur = std::mem::take(&mut result.hyps);
    // Trace only: the per-record history cap.
    let history = trace.as_ref().map_or(0, |tr| tr.config.history());
    // Paths the gap guard dropped in this chunk (the guard is off when `stale_gap_frames` is
    // `None`; then this stays 0).
    let mut chunk_gap_drops = 0u64;

    for t in 0..encoder_out.frames {
        // Trace only: whether this step is recorded, and what the TTL pass dropped.
        let rec = match trace.as_mut() {
            Some(tr) => tr.begin_step(result.frame_offset + t as i32),
            None => false,
        };
        let firings_before = result.firings;
        let mut expired_paths: Vec<TracePath> = Vec::new();
        let mut refilled_blank = false;
        if let Some(ttl) = options.active_path_ttl_frames {
            let current_frame = result.frame_offset + t as i32;
            if rec {
                for h in &cur.0 {
                    if expired(h, graph, current_frame, ttl) {
                        expired_paths.push(TracePath::of(graph, h));
                    }
                }
            }
            cur.0.retain(|h| !expired(h, graph, current_frame, ttl));
            if cur.0.is_empty() {
                refilled_blank = true;
                cur.add(
                    Hypothesis::blank(context_size),
                    options.math,
                    options.merge_timestamps,
                    None,
                );
            }
        }
        let prev = std::mem::take(&mut cur.0);
        let rows = prev.len();

        let mut contexts = Vec::with_capacity(rows * context_size);
        for h in &prev {
            contexts.extend_from_slice(&h.ys[h.ys.len() - context_size..]);
        }
        let (decoder_dim, decoder_out) = if options.decoder_cache {
            model.run_decoder_cached(&contexts)?
        } else {
            model.run_decoder(contexts)?
        };
        let frame = encoder_out.frame(t);
        let mut scores = match options.joiner_batching {
            JoinerBatching::Beam => {
                let mut encoder_rows = Vec::with_capacity(rows * frame.len());
                for _ in 0..rows {
                    encoder_rows.extend_from_slice(frame);
                }
                model.run_joiner(&encoder_rows, &decoder_out, rows)?
            }
            JoinerBatching::PerHypothesis => {
                let mut all = Vec::with_capacity(rows * vocab);
                for row in decoder_out.chunks_exact(decoder_dim) {
                    all.extend(model.run_joiner(frame, row, 1)?);
                }
                all
            }
        };
        for row in scores.chunks_mut(vocab) {
            log_softmax(row, options.softmax_f64, options.math);
        }
        let logprobs = scores.clone();
        for (row, h) in scores.chunks_mut(vocab).zip(&prev) {
            for x in row.iter_mut() {
                *x = (*x as f64 + h.log_prob) as f32;
            }
        }

        let mut hyps = Hypotheses::default();
        // Trace only: parents with a child in the beam, and the merges on this step.
        let mut survived: Vec<bool> = if rec { vec![false; rows] } else { Vec::new() };
        let mut merges: Vec<TraceMerge> = Vec::new();
        let selected_k = topk_index(&scores, options.max_active_paths);
        for &k in &selected_k {
            let hyp_index = k / vocab;
            if rec {
                survived[hyp_index] = true;
            }
            let token = (k % vocab) as i64;
            let mut h = prev[hyp_index].clone();
            let mut context_score = 0.0f32;
            if token != 0 && Some(token) != options.unk_id {
                h.ys.push(token);
                h.timestamps.push(t as i32 + result.frame_offset);
                let lp = logprobs[hyp_index * vocab + token as usize];
                h.ys_probs.push(if options.softmax_f64 {
                    options.math.exp(lp as f64) as f32
                } else {
                    options.math.expf(lp)
                });
                h.num_trailing_blanks = 0;
                let (score, state) = graph.forward_one_step(h.context_state, token as i32);
                context_score = score;
                h.context_state = state;
                if graph.state(state).token == -1 {
                    h.ys = blanks(context_size);
                    h.timestamps.clear();
                    h.ys_probs.clear();
                } else if options.stale_gap_frames.is_some() {
                    // Pure filter: a path whose newest keyword token came too long after
                    // the previous one never enters the new beam (no merge, no log-add, no
                    // back-fill), as in the offline port.
                    let level = graph.state(state).level;
                    if let Some(gap) =
                        stale_gap_exceeds(&h.timestamps, level, options.stale_gap_frames)
                    {
                        chunk_gap_drops += 1;
                        result.gap_drops += 1;
                        if let Some(tr) = trace.as_mut() {
                            tr.push_gap_drop(TraceGapDrop::capture(
                                &h,
                                level,
                                result.frame_offset + t as i32,
                                gap,
                                history,
                            ));
                        }
                        continue;
                    }
                }
            } else {
                h.num_trailing_blanks += 1;
            }
            h.log_prob = (scores[k] + context_score) as f64;
            hyps.add(
                h,
                options.math,
                options.merge_timestamps,
                if rec { Some((&mut merges, history)) } else { None },
            );
        }

        if options.stale_gap_frames.is_some() && hyps.0.is_empty() {
            // Every candidate was dropped by the guard: the beam restarts from blank.
            hyps = Hypotheses::single(Hypothesis::blank(context_size));
        }
        let best = hyps.most_probable();
        // Trace only: the beam as the step leaves it, before a firing resets it.
        let step_head = if rec {
            Some((hyps.0.len(), TraceTop::of(graph, best, history)))
        } else {
            None
        };
        let mut step_match: Option<TraceMatch> = None;
        if let Some(end) = graph.matched(best.context_state) {
            let end = graph.state(end);
            // Upstream averages the FIRST `level` token probabilities of the path.
            let ys_prob = keyword_score(&best.ys_probs, end.level, options.threshold_tokens);
            if rec {
                step_match = Some(TraceMatch {
                    score: ys_prob,
                    threshold: end.ac_threshold,
                    trailing_blanks_ok: best.num_trailing_blanks > options.num_trailing_blanks,
                });
            }
            if best.num_trailing_blanks > options.num_trailing_blanks
                && ys_prob >= end.ac_threshold
            {
                let first = best.ys.len().saturating_sub(end.level);
                result.tokens = best.ys[first..].to_vec();
                result.timestamps =
                    best.timestamps[best.timestamps.len().saturating_sub(end.level)..].to_vec();
                result.keyword = end.phrase.clone();
                result.firing = Some(Firing {
                    context: best.ys[first.saturating_sub(context_size)..first].to_vec(),
                    token_probs: best.ys_probs[best.ys_probs.len().saturating_sub(end.level)..]
                        .to_vec(),
                    score: ys_prob,
                    threshold: end.ac_threshold,
                    frame: t as i32 + result.frame_offset,
                    gap_drops: result.gap_drops,
                    gap_drops_chunk: chunk_gap_drops,
                    onset: None,
                });
                result.firings += 1;
                if let (Some(tr), Some(firing)) = (trace.as_mut(), result.firing.as_ref()) {
                    if tr.config.firing {
                        tr.push_firing(FiringTrace::capture(
                            graph,
                            best,
                            &hyps.0,
                            firing,
                            &end.phrase,
                            result.firings,
                            history,
                        ));
                    }
                }
                hyps = Hypotheses::single(Hypothesis::blank(context_size));
            }
        }
        if let (Some((hypotheses_out, top)), Some(tr)) = (step_head, trace.as_mut()) {
            let mut pruned = Vec::new();
            for (h, kept) in prev.iter().zip(&survived) {
                if !*kept && graph.state(h.context_state).level > 0 {
                    pruned.push(TracePath::of(graph, h));
                }
            }
            for merge in merges.iter_mut() {
                merge.level = graph
                    .state(merge.context_state)
                    .level
                    .max(graph.state(merge.incoming_context_state).level);
            }
            merges.retain(|merge| merge.level > 0);
            // Why an extension of a keyword path lost: its score against the beam cutoff
            // and against the parent's best selected child. Bounded; read-only.
            let cutoff_score = selected_k.last().map(|k| scores[*k]);
            let mut extensions: Vec<TraceExtension> = Vec::new();
            let mut extensions_omitted = 0usize;
            for keyword_parents in [true, false] {
                for (parent, h) in prev.iter().enumerate() {
                    let level = graph.state(h.context_state).level;
                    if (level > 0) != keyword_parents {
                        continue;
                    }
                    for token in graph.extension_tokens(h.context_state) {
                        if token <= 0 || token as usize >= vocab {
                            continue;
                        }
                        if extensions.len() >= MAX_STEP_EXTENSIONS {
                            extensions_omitted += 1;
                            continue;
                        }
                        let index = parent * vocab + token as usize;
                        let score = scores[index];
                        let sibling = selected_k
                            .iter()
                            .filter(|k| **k / vocab == parent && **k != index)
                            .max_by(|a, b| {
                                scores[**a]
                                    .partial_cmp(&scores[**b])
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                        extensions.push(TraceExtension {
                            parent_context_state: h.context_state,
                            parent_level: level,
                            parent_first_token_frame: keyword_first_frame(h, level),
                            token: i64::from(token),
                            token_prob: if options.softmax_f64 {
                                options.math.exp(logprobs[index] as f64) as f32
                            } else {
                                options.math.expf(logprobs[index])
                            },
                            score,
                            rank: scores.iter().filter(|other| **other > score).count(),
                            selected: selected_k.contains(&index),
                            cutoff_score,
                            sibling_token: sibling.map(|k| (*k % vocab) as i64),
                            sibling_score: sibling.map(|k| scores[*k]),
                        });
                    }
                }
            }
            tr.push_step(StepTrace {
                frame: result.frame_offset + t as i32,
                clock: Default::default(),
                hypotheses_in: rows,
                hypotheses_out,
                refilled_blank,
                top: Some(top),
                matched: step_match,
                fired: result.firings != firings_before,
                expired: expired_paths,
                pruned,
                merges,
                extensions,
                extensions_omitted,
            });
        }
        cur = hyps;
    }

    result.num_trailing_blanks = cur.most_probable().num_trailing_blanks;
    result.hyps = cur;
    result.frame_offset += encoder_out.frames as i32;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hyp(ys: &[i64], timestamps: &[i32], probs: &[f32], log_prob: f64) -> Hypothesis {
        Hypothesis {
            ys: ys.to_vec(),
            timestamps: timestamps.to_vec(),
            ys_probs: probs.to_vec(),
            log_prob,
            context_state: 3,
            num_trailing_blanks: 0,
        }
    }

    /// `Hypotheses::add` with and without a trace observer: identical beams on both `Math`
    /// paths, both merge policies, higher, lower and equal scores, and the observer sees
    /// exactly what the policy decided without changing it.
    #[test]
    fn add_is_identical_with_and_without_the_observer() {
        for portable in [false, true] {
            let math = Math::new(portable);
            for merge in [MergeTimestamps::First, MergeTimestamps::Likelier] {
                for (existing_lp, incoming_lp) in [(-1.0, -2.0), (-2.0, -1.0), (-1.5, -1.5)] {
                    let existing = hyp(&[-1, 0, 5], &[4, 6], &[0.5, 0.25], existing_lp);
                    let incoming = hyp(&[-1, 0, 5], &[2, 7], &[0.125, 0.75], incoming_lp);
                    let start = Hypotheses::single(existing.clone());

                    let mut plain = start.clone();
                    plain.add(incoming.clone(), math, merge, None);

                    let mut observed = start.clone();
                    let mut events = Vec::new();
                    observed.add(incoming.clone(), math, merge, Some((&mut events, 1)));

                    assert_eq!(plain, observed, "{portable} {merge:?} {existing_lp} {incoming_lp}");
                    assert_eq!(plain.0.len(), 1);
                    let merged_lp = log_add(existing_lp, incoming_lp, math);
                    assert_eq!(plain.0[0].log_prob, merged_lp);
                    assert_eq!(plain.0[0].ys, existing.ys);
                    // Only a strictly higher incoming score under Likelier replaces fields;
                    // ties keep the existing path.
                    let incoming_kept =
                        merge == MergeTimestamps::Likelier && incoming_lp > existing_lp;
                    let kept = if incoming_kept { &incoming } else { &existing };
                    assert_eq!(plain.0[0].timestamps, kept.timestamps);
                    assert_eq!(plain.0[0].ys_probs, kept.ys_probs);

                    assert_eq!(events.len(), 1);
                    let event = &events[0];
                    assert_eq!(event.incoming_kept, incoming_kept);
                    assert_eq!(event.merged_log_prob, merged_lp);
                    assert_eq!(event.existing.log_prob, existing_lp);
                    assert_eq!(event.incoming.log_prob, incoming_lp);
                    // The history cap keeps the last entry and records the original length.
                    assert_eq!(event.existing.timestamps, vec![6]);
                    assert_eq!(event.existing.timestamps_len, 2);
                    assert!(event.existing.truncated);
                }
            }
        }
    }

    /// The gap guard's filter on a synthetic beam: a pair exactly G apart is kept, G + 1 is
    /// dropped, paths that are not on a keyword of depth two or more are untouched, and with
    /// the guard off every path is kept.
    #[test]
    fn stale_gap_guard_filters_only_keyword_paths_over_the_limit() {
        const G: i32 = 8;
        // (level, timestamps): the path's keyword depth and its token frames.
        let beam: [(usize, Vec<i32>); 6] = [
            (3, vec![10, 14, 22]),     // newest gap == G
            (3, vec![10, 14, 23]),     // newest gap == G + 1
            (3, vec![10, 40, 44]),     // an old large gap was checked when it was emitted
            (1, vec![50]),             // first token: no consecutive pair
            (0, vec![]),               // not on a keyword
            (2, vec![5, 5 + G + 100]), // depth two, far apart
        ];
        let kept = |limit: Option<i32>| -> Vec<bool> {
            beam.iter()
                .map(|(level, ts)| stale_gap_exceeds(ts, *level, limit).is_none())
                .collect()
        };
        assert_eq!(kept(Some(G)), [true, false, true, true, true, false]);
        assert_eq!(stale_gap_exceeds(&[10, 14, 23], 3, Some(G)), Some(9));
        assert_eq!(stale_gap_exceeds(&[10, 14, 22], 3, Some(G)), None);
        // Guard off: nothing is ever dropped, whatever the gaps.
        assert!(kept(None).iter().all(|keep| *keep));
        // Fewer timestamps than the depth (tokens left by a fail transition): untouched.
        assert_eq!(stale_gap_exceeds(&[50], 2, Some(G)), None);
    }

    #[test]
    fn add_of_a_new_key_pushes_and_records_no_merge() {
        let math = Math::new(false);
        let first = hyp(&[-1, 0, 5], &[4], &[0.5], -1.0);
        let other = hyp(&[-1, 0, 6], &[5], &[0.4], -1.2);
        let mut plain = Hypotheses::single(first.clone());
        plain.add(other.clone(), math, MergeTimestamps::Likelier, None);
        let mut observed = Hypotheses::single(first);
        let mut events = Vec::new();
        observed.add(other, math, MergeTimestamps::Likelier, Some((&mut events, 4)));
        assert_eq!(plain, observed);
        assert_eq!(plain.0.len(), 2);
        assert!(events.is_empty());
    }
}
