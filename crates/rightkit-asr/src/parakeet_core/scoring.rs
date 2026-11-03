//! [DROP-IN CORE]
//! Pure score arithmetic shared by platform decoder loops.

use std::cmp::Ordering;

pub fn log_softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let log_z = logsumexp(logits);
    logits.iter().map(|&x| x - log_z).collect()
}

/// `ln(sum(exp(logits)))`, computed with the same max-shift arithmetic as
/// [`log_softmax`] so `logits[i] - logsumexp(logits)` is bit-identical to
/// `log_softmax(logits)[i]`. Returns `f32::NEG_INFINITY` for empty input.
pub fn logsumexp(logits: &[f32]) -> f32 {
    if logits.is_empty() {
        return f32::NEG_INFINITY;
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum_exp: f32 = logits.iter().map(|&x| (x - max).exp()).sum();
    max + sum_exp.ln()
}

/// Log-probability of `logits[idx]` without allocating the full log-softmax vector.
/// Bit-identical to `log_softmax(logits)[idx]`. Panics if `idx` is out of range.
pub fn log_prob_at(logits: &[f32], idx: usize) -> f32 {
    logits[idx] - logsumexp(logits)
}

/// Blank transition score. Kept separate so a future principled search change can
/// alter normalization/pruning without conflating it with the emission mitigation.
pub fn score_blank(parent_score: f32, blank_logp: f32, duration_logp: f32) -> f32 {
    parent_score + blank_logp + duration_logp
}

/// Nonblank transition score. `emit_reward` is the calibrated insertion bonus from
/// c0239763 when used by macOS. `bias_bonus` remains supplied by the platform's
/// existing context-bias implementation during extraction.
pub fn score_emission(
    parent_score: f32,
    token_logp: f32,
    duration_logp: f32,
    emit_reward: f32,
    bias_bonus: f32,
) -> f32 {
    parent_score + token_logp + duration_logp + emit_reward + bias_bonus
}

/// Largest `k` served by the single-pass buffer; larger requests use the full sort.
const TOP_K_BUFFER_MAX: usize = 16;

/// Stable top-k by descending score. Equal values retain lower input index first.
///
/// O(n * k) single pass with a sorted buffer of at most `k` indices (k <= 16);
/// larger `k` falls back to the full stable sort. Candidates are visited in ascending
/// index order, so a later index displaces an earlier one only on a strictly greater
/// value; ties therefore keep the lowest index, matching the stable sort. Inputs
/// containing NaN are routed to the legacy sort, whose comparator treats NaN as Equal.
pub fn top_k_indices(values: &[f32], k: usize) -> Vec<usize> {
    let k = k.min(values.len());
    if k == 0 {
        return Vec::new();
    }
    // NaN makes the reference comparator non-transitive, so only the legacy sort
    // reproduces its output; the fast paths below assume NaN-free input.
    if k > TOP_K_BUFFER_MAX || values.iter().any(|v| v.is_nan()) {
        return top_k_indices_sorted(values, k);
    }
    if k == 1 {
        let mut best = 0usize;
        for (i, &v) in values.iter().enumerate().skip(1) {
            if v > values[best] {
                best = i;
            }
        }
        return vec![best];
    }
    let mut buf: Vec<usize> = Vec::with_capacity(k);
    for (i, &v) in values.iter().enumerate() {
        if buf.len() == k {
            if v <= values[buf[k - 1]] {
                continue;
            }
            buf.pop();
        }
        let mut pos = buf.len();
        while pos > 0 && v > values[buf[pos - 1]] {
            pos -= 1;
        }
        buf.insert(pos, i);
    }
    buf
}

/// Reference implementation: full stable sort, then truncate.
fn top_k_indices_sorted(values: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| {
        values[b]
            .partial_cmp(&values[a])
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });
    idx.truncate(k.min(idx.len()));
    idx
}
