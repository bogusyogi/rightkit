//! Reciprocal-rank fusion (RRF) of ranked lists. Pure, no model and no feature gate.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// The damping constant of the original RRF paper (Cormack, Clarke and Büttcher, 2009).
pub const RRF_K: f64 = 60.0;

/// Fuses ranked lists, best first, by reciprocal rank. An item scores `sum(1 / (k + rank))`
/// over every list that holds it, with rank starting at 1, so an item near the top of several
/// lists beats one at the top of only one. Returns each item once, best score first. Items with
/// equal scores keep the order in which they first appear across the lists.
///
/// An item may appear at most once per list; a repeat in one list counts at its first rank.
/// A `k` that is not finite or is negative returns an empty result. Use [`RRF_K`] for the
/// conventional value.
pub fn reciprocal_rank_fusion<K: Eq + Hash + Clone>(rankings: &[&[K]], k: f64) -> Vec<(K, f64)> {
    if !k.is_finite() || k < 0.0 {
        return Vec::new();
    }
    let mut slots: HashMap<K, usize> = HashMap::new();
    let mut fused: Vec<(K, f64)> = Vec::new();
    for ranking in rankings {
        let mut seen: HashSet<&K> = HashSet::with_capacity(ranking.len());
        for (position, item) in ranking.iter().enumerate() {
            if !seen.insert(item) {
                continue;
            }
            let contribution = 1.0 / (k + position as f64 + 1.0);
            match slots.get(item) {
                Some(&slot) => fused[slot].1 += contribution,
                None => {
                    slots.insert(item.clone(), fused.len());
                    fused.push((item.clone(), contribution));
                }
            }
        }
    }
    // Stable sort: equal scores keep first-appearance order.
    fused.sort_by(|left, right| right.1.total_cmp(&left.1));
    fused
}
