/// Greedy CTC decoder on a flat logits slice.
///
/// `logits`: flat `[T * vocab_size]` in row-major order (T outermost).
/// `vocab[0]` = blank token '\0'; `vocab[i]` for i>0 = real char.
pub(crate) fn ctc_greedy_decode(
    logits: &[f32],
    t_len: usize,
    vocab_size: usize,
    vocab: &[char],
) -> String {
    let mut result = String::new();
    let mut prev = usize::MAX;

    for t in 0..t_len {
        let row = &logits[t * vocab_size..(t + 1) * vocab_size];
        let (idx, _) = row
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, &f32::NEG_INFINITY));

        if idx != prev && idx != 0 {
            if let Some(&c) = vocab.get(idx) {
                result.push(c);
            }
        }
        prev = idx;
    }
    result
}
