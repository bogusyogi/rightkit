//! [DROP-IN CORE]
//! Pure score arithmetic shared by platform decoder loops.

use std::cmp::Ordering;

pub fn log_softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max = logits
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let sum_exp: f32 = logits.iter().map(|&x| (x - max).exp()).sum();
    let log_z = max + sum_exp.ln();
    logits.iter().map(|&x| x - log_z).collect()
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

/// Stable top-k by descending score. Equal values retain lower input index first.
pub fn top_k_indices(values: &[f32], k: usize) -> Vec<usize> {
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
