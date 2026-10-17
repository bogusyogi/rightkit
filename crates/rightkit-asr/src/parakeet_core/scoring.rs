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

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    fn reference_log_softmax(logits: &[f32]) -> Vec<f32> {
        if logits.is_empty() {
            return Vec::new();
        }
        let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let sum_exp: f32 = logits.iter().map(|&x| (x - max).exp()).sum();
        let log_z = max + sum_exp.ln();
        logits.iter().map(|&x| x - log_z).collect()
    }

    #[test]
    fn top_k_matches_stable_sort_on_random_vectors() {
        let mut rng = Lcg(0x5eed);
        for round in 0..2000 {
            let n = (rng.next() % 40) as usize;
            // Small value alphabet in even rounds forces many ties.
            let levels = if round % 2 == 0 { 4 } else { 1000 };
            let values: Vec<f32> = (0..n)
                .map(|_| -((rng.next() % levels) as f32) * 0.5)
                .collect();
            for k in [0usize, 1, 2, 3, 5, 16, 17, 40, 100] {
                assert_eq!(
                    top_k_indices(&values, k),
                    top_k_indices_sorted(&values, k),
                    "n={n} k={k} values={values:?}"
                );
            }
        }
    }

    #[test]
    fn top_k_matches_legacy_with_nan_in_any_position() {
        let n = f32::NAN;
        let cases: [&[f32]; 9] = [
            &[0.0, n, 1.0],
            &[n, 0.0, 1.0],
            &[1.0, 0.0, n],
            &[n, 1.0, n, 2.0, 0.5],
            &[1.0, n, n, 3.0, n, 2.0],
            &[n, n, n],
            &[n],
            &[2.0, 1.0, n, 3.0],
            &[n, 5.0, 5.0, n, 1.0],
        ];
        for values in cases {
            for k in [1usize, 2, 3] {
                assert_eq!(
                    top_k_indices(values, k),
                    top_k_indices_sorted(values, k),
                    "k={k} values={values:?}"
                );
            }
        }
        assert_eq!(top_k_indices(&[0.0, n, 1.0], 1), vec![0]);
    }

    #[test]
    fn top_k_edge_cases() {
        assert!(top_k_indices(&[], 2).is_empty());
        assert!(top_k_indices(&[1.0, 2.0], 0).is_empty());
        assert_eq!(top_k_indices(&[3.0, 3.0, 3.0], 1), vec![0]);
        assert_eq!(top_k_indices(&[3.0, 3.0, 3.0], 2), vec![0, 1]);
        assert_eq!(top_k_indices(&[1.0, 5.0, 5.0, 2.0], 5), vec![1, 2, 3, 0]);
    }

    #[test]
    fn top_k_matches_on_vocab_sized_input() {
        let mut rng = Lcg(7);
        let values: Vec<f32> = (0..8198).map(|_| -((rng.next() % 50) as f32)).collect();
        for k in [1usize, 2, 4] {
            assert_eq!(top_k_indices(&values, k), top_k_indices_sorted(&values, k));
        }
    }

    #[test]
    fn log_prob_at_matches_log_softmax() {
        let mut rng = Lcg(99);
        for _ in 0..200 {
            let n = 1 + (rng.next() % 64) as usize;
            let logits: Vec<f32> = (0..n)
                .map(|_| (rng.next() % 4000) as f32 * 0.01 - 20.0)
                .collect();
            let full = reference_log_softmax(&logits);
            assert_eq!(full, log_softmax(&logits));
            for i in 0..n {
                assert!((log_prob_at(&logits, i) - full[i]).abs() <= 1e-6);
            }
        }
        assert!(logsumexp(&[]).is_infinite());
    }
}
