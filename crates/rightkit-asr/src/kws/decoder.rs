//! Keyword modified beam search, ported from Sherpa-ONNX 1.13.4
//! `transducer-keyword-decoder.cc` and `hypothesis.{h,cc}` for one stream, with the
//! HeardRight active-path expiry built in (formerly
//! `0001-expire-active-keyword-hypotheses.patch`).
//!
//! Floating-point order follows the C++: scores are `f32` per frame and `f64` across
//! frames, hypothesis merges use a double log-add, and the joiner runs once per frame
//! on every live hypothesis (its int8 graph quantizes the whole batch together, so the
//! batch must match Sherpa's for identical logits).

use super::context_graph::{ContextGraph, ROOT};
use super::model::{EncoderOut, Zipformer2Transducer};
use crate::error::Result;

/// Decoder settings fixed at spotter creation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecoderOptions {
    pub max_active_paths: usize,
    pub num_trailing_blanks: i32,
    pub unk_id: Option<i64>,
    pub active_path_ttl_frames: Option<i32>,
    pub softmax_f64: bool,
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
/// log-adds the score and keeps the first hypothesis's other fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Hypotheses(Vec<Hypothesis>);

impl Hypotheses {
    pub(crate) fn single(h: Hypothesis) -> Self {
        Self(vec![h])
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }

    fn add(&mut self, h: Hypothesis) {
        match self.0.iter_mut().find(|e| e.ys == h.ys) {
            Some(existing) => existing.log_prob = log_add(existing.log_prob, h.log_prob),
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
fn log_add(x: f64, y: f64) -> f64 {
    let (big, diff) = if x < y { (y, x - y) } else { (x, y - x) };
    if diff >= MIN_LOG_DIFF {
        big + diff.exp().ln_1p()
    } else {
        big
    }
}

/// `LogSoftmax<float>` over one row. Production Sherpa builds (libc++, MSVC) resolve its
/// `exp`/`log` to the float functions; the GCC build resolves them to the double ones.
fn log_softmax(row: &mut [f32], f64_math: bool) {
    let mut m = row[0];
    for &x in &row[1..] {
        if m < x {
            m = x;
        }
    }
    let mut sum = 0.0f32;
    let offset = if f64_math {
        for &x in row.iter() {
            sum = (sum as f64 + ((x - m) as f64).exp()) as f32;
        }
        (m as f64 + (sum as f64).ln()) as f32
    } else {
        for &x in row.iter() {
            sum += (x - m).exp();
        }
        m + sum.ln()
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
        }
    }
}

/// A path whose active keyword context began more than `ttl` frames ago.
fn expired(h: &Hypothesis, graph: &ContextGraph, current_frame: i32, ttl: i32) -> bool {
    let level = graph.state(h.context_state).level;
    if level == 0 || h.timestamps.len() < level {
        return false;
    }
    current_frame - h.timestamps[h.timestamps.len() - level] > ttl
}

/// Decode one encoder chunk, updating `result` in place.
pub(crate) fn decode_chunk(
    model: &mut Zipformer2Transducer,
    graph: &ContextGraph,
    options: &DecoderOptions,
    encoder_out: &EncoderOut,
    result: &mut DecoderResult,
) -> Result<()> {
    let vocab = model.vocab_size();
    let context_size = model.context_size();
    let mut cur = std::mem::take(&mut result.hyps);

    for t in 0..encoder_out.frames {
        if let Some(ttl) = options.active_path_ttl_frames {
            let current_frame = result.frame_offset + t as i32;
            cur.0.retain(|h| !expired(h, graph, current_frame, ttl));
            if cur.0.is_empty() {
                cur.add(Hypothesis::blank(context_size));
            }
        }
        let prev = std::mem::take(&mut cur.0);
        let rows = prev.len();

        let mut contexts = Vec::with_capacity(rows * context_size);
        for h in &prev {
            contexts.extend_from_slice(&h.ys[h.ys.len() - context_size..]);
        }
        let (_, decoder_out) = model.run_decoder(contexts)?;
        let frame = encoder_out.frame(t);
        let mut encoder_rows = Vec::with_capacity(rows * frame.len());
        for _ in 0..rows {
            encoder_rows.extend_from_slice(frame);
        }
        let mut scores = model.run_joiner(&encoder_rows, &decoder_out, rows)?;
        for row in scores.chunks_mut(vocab) {
            log_softmax(row, options.softmax_f64);
        }
        let logprobs = scores.clone();
        for (row, h) in scores.chunks_mut(vocab).zip(&prev) {
            for x in row.iter_mut() {
                *x = (*x as f64 + h.log_prob) as f32;
            }
        }

        let mut hyps = Hypotheses::default();
        for k in topk_index(&scores, options.max_active_paths) {
            let hyp_index = k / vocab;
            let token = (k % vocab) as i64;
            let mut h = prev[hyp_index].clone();
            let mut context_score = 0.0f32;
            if token != 0 && Some(token) != options.unk_id {
                h.ys.push(token);
                h.timestamps.push(t as i32 + result.frame_offset);
                let lp = logprobs[hyp_index * vocab + token as usize];
                h.ys_probs.push(if options.softmax_f64 {
                    (lp as f64).exp() as f32
                } else {
                    lp.exp()
                });
                h.num_trailing_blanks = 0;
                let (score, state) = graph.forward_one_step(h.context_state, token as i32);
                context_score = score;
                h.context_state = state;
                if graph.state(state).token == -1 {
                    h.ys = blanks(context_size);
                    h.timestamps.clear();
                    h.ys_probs.clear();
                }
            } else {
                h.num_trailing_blanks += 1;
            }
            h.log_prob = (scores[k] + context_score) as f64;
            hyps.add(h);
        }

        let best = hyps.most_probable();
        if let Some(end) = graph.matched(best.context_state) {
            let end = graph.state(end);
            // Upstream averages the FIRST `level` token probabilities of the path.
            let mut ys_prob = 0.0f32;
            for p in best.ys_probs.iter().take(end.level) {
                ys_prob += *p;
            }
            ys_prob /= end.level as f32;
            if best.num_trailing_blanks > options.num_trailing_blanks
                && ys_prob >= end.ac_threshold
            {
                result.tokens = best.ys[best.ys.len().saturating_sub(end.level)..].to_vec();
                result.timestamps =
                    best.timestamps[best.timestamps.len().saturating_sub(end.level)..].to_vec();
                result.keyword = end.phrase.clone();
                hyps = Hypotheses::single(Hypothesis::blank(context_size));
            }
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

    fn hyp(ys: &[i64], timestamps: &[i32], state: usize, log_prob: f64) -> Hypothesis {
        Hypothesis {
            ys: ys.to_vec(),
            timestamps: timestamps.to_vec(),
            ys_probs: vec![0.9; timestamps.len()],
            log_prob,
            context_state: state,
            num_trailing_blanks: 0,
        }
    }

    #[test]
    fn topk_orders_by_value_then_index() {
        assert_eq!(topk_index(&[0.1, 0.5, 0.5, -1.0, 0.7], 3), vec![4, 1, 2]);
        assert_eq!(topk_index(&[0.1, 0.2], 8), vec![1, 0]);
    }

    #[test]
    fn duplicate_paths_log_add_and_keep_the_first() {
        let mut hs = Hypotheses::default();
        hs.add(hyp(&[-1, 0, 5], &[3], 1, -1.0));
        hs.add(hyp(&[-1, 0, 5], &[7], 2, -1.0));
        assert_eq!(hs.len(), 1);
        assert_eq!(hs.0[0].timestamps, vec![3]);
        assert!((hs.0[0].log_prob - (-1.0 + 2f64.ln())).abs() < 1e-12);
    }

    #[test]
    fn log_softmax_rows_sum_to_one() {
        for f64_math in [false, true] {
            let mut row = vec![1.0f32, 2.0, 3.0, -4.0];
            log_softmax(&mut row, f64_math);
            let total: f64 = row.iter().map(|x| (*x as f64).exp()).sum();
            assert!((total - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn expiry_counts_from_the_first_token_of_the_active_keyword() {
        let ids = vec![vec![5, 6, 7]];
        let graph = ContextGraph::new(&ids, 1.0, 0.25, &[], &["K".into()], &[]);
        let (_, a) = graph.forward_one_step(ROOT, 5);
        let (_, b) = graph.forward_one_step(a, 6);
        // Two tokens into the keyword; the first was emitted at frame 10.
        let h = hyp(&[-1, 0, 9, 5, 6], &[2, 10, 30], b, 0.0);
        assert!(!expired(&h, &graph, 48, 38));
        assert!(expired(&h, &graph, 49, 38));
        // At the root nothing expires.
        assert!(!expired(&hyp(&[-1, 0], &[], ROOT, 0.0), &graph, 1_000, 38));
    }
}
