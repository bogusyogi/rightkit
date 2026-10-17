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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_in_several_lists_outrank_a_single_top_hit() {
        let lexical = ["a", "b", "c"];
        let semantic = ["b", "d", "a"];
        let fused = reciprocal_rank_fusion(&[&lexical[..], &semantic[..]], RRF_K);
        let order: Vec<&str> = fused.iter().map(|(key, _)| *key).collect();
        assert_eq!(order, vec!["b", "a", "d", "c"]);
        let b = 1.0 / 62.0 + 1.0 / 61.0;
        assert!((fused[0].1 - b).abs() < 1e-12);
    }

    #[test]
    fn ties_keep_first_appearance_and_repeats_count_once() {
        let first = ["x", "y", "x"];
        let second = ["y", "x"];
        let fused = reciprocal_rank_fusion(&[&first[..], &second[..]], 0.0);
        let order: Vec<&str> = fused.iter().map(|(key, _)| *key).collect();
        assert_eq!(order, vec!["x", "y"]);
        // k = 0: x = 1/1 (first list) + 1/2 (second list); y = 1/2 + 1/1.
        assert!((fused[0].1 - 1.5).abs() < 1e-12);
        assert!((fused[1].1 - 1.5).abs() < 1e-12);
    }

    #[test]
    fn empty_input_and_invalid_k_are_empty() {
        let none: [&[u32]; 0] = [];
        assert!(reciprocal_rank_fusion(&none, RRF_K).is_empty());
        let one = [1u32, 2];
        assert!(reciprocal_rank_fusion(&[&one[..]], -1.0).is_empty());
        assert!(reciprocal_rank_fusion(&[&one[..]], f64::NAN).is_empty());
        assert_eq!(reciprocal_rank_fusion(&[&one[..]], RRF_K).len(), 2);
    }
}
