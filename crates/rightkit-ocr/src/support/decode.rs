/// Greedy CTC decoder on a flat logits slice.
///
/// `logits`: flat `[T * vocab_size]` in row-major order (T outermost).
/// `vocab[0]` = blank token '\0'; `vocab[i]` for i>0 = real char.
#[cfg(test)]
pub(crate) fn ctc_greedy_decode(
    logits: &[f32],
    t_len: usize,
    vocab_size: usize,
    vocab: &[char],
) -> String {
    ctc_greedy_decode_scored(logits, t_len, vocab_size, vocab).0
}

/// As [`ctc_greedy_decode`], also returning the mean per-character score (the
/// winning class value of each emitted step) when the recognizer emits
/// probabilities in 0..=1 (softmax output). `None` for raw-logit models.
pub(crate) fn ctc_greedy_decode_scored(
    logits: &[f32],
    t_len: usize,
    vocab_size: usize,
    vocab: &[char],
) -> (String, Option<f32>) {
    let mut result = String::new();
    let (mut score_sum, mut score_n, mut is_prob) = (0.0f32, 0u32, true);
    let mut prev = usize::MAX;

    for t in 0..t_len {
        let row = &logits[t * vocab_size..(t + 1) * vocab_size];
        let (idx, best) = row
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, &f32::NEG_INFINITY));

        if idx != prev && idx != 0 {
            if let Some(&c) = vocab.get(idx) {
                result.push(c);
                if !(0.0..=1.0).contains(best) {
                    is_prob = false;
                }
                score_sum += *best;
                score_n += 1;
            }
        }
        prev = idx;
    }
    let conf = (is_prob && score_n > 0).then(|| score_sum / score_n as f32);
    (result, conf)
}
