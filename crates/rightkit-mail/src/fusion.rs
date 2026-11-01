//! Reciprocal rank fusion of the keyword ranking and the vector ranking.
//!
//! A message scores `1 / (k + rank)` for each list it appears in (rank starts
//! at 1) and the scores add, so a message both lists like beats one only a
//! single list likes, and no score scale has to be reconciled between FTS
//! relevance and vector distance. Pure: no database, no embedder.

use std::cmp::Ordering;
use std::collections::HashMap;

/// The usual RRF constant.
pub const RRF_K: f64 = 60.0;

/// One ranked message: its row id plus the fields the tie-break needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rowid: i64,
    pub internal_date: i64,
    pub message_id: String,
}

/// Fuse two best-first lists into one best-first list of message rowids.
/// Equal scores break by newest message, then by message id, then rowid, so
/// the order never depends on input order or hash iteration.
pub fn fuse(keyword: &[Hit], vector: &[Hit], k: f64) -> Vec<i64> {
    let mut scored: HashMap<i64, (f64, &Hit)> = HashMap::new();
    for list in [keyword, vector] {
        let mut seen = std::collections::HashSet::new();
        for (i, hit) in list.iter().enumerate() {
            if !seen.insert(hit.rowid) {
                continue; // a repeated row keeps its best rank only
            }
            let gain = 1.0 / (k + (i + 1) as f64);
            scored.entry(hit.rowid).or_insert((0.0, hit)).0 += gain;
        }
    }
    let mut out: Vec<(f64, &Hit)> = scored.into_values().collect();
    out.sort_by(|(sa, a), (sb, b)| {
        sb.partial_cmp(sa)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.internal_date.cmp(&a.internal_date))
            .then_with(|| a.message_id.cmp(&b.message_id))
            .then_with(|| a.rowid.cmp(&b.rowid))
    });
    out.into_iter().map(|(_, h)| h.rowid).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(rowid: i64, date: i64) -> Hit {
        Hit {
            rowid,
            internal_date: date,
            message_id: format!("m{rowid}"),
        }
    }

    fn hits(rows: &[(i64, i64)]) -> Vec<Hit> {
        rows.iter().map(|&(r, d)| hit(r, d)).collect()
    }

    #[test]
    fn keyword_only_when_no_vectors_match() {
        let kw = hits(&[(3, 10), (1, 30), (2, 20)]);
        assert_eq!(fuse(&kw, &[], RRF_K), vec![3, 1, 2]);
    }

    #[test]
    fn vector_only_when_the_keyword_side_is_empty() {
        let vec = hits(&[(5, 1), (4, 2)]);
        assert_eq!(fuse(&[], &vec, RRF_K), vec![5, 4]);
        assert!(fuse(&[], &[], RRF_K).is_empty());
    }

    #[test]
    fn a_row_in_both_lists_beats_single_list_leaders() {
        // 1 leads keywords, 9 leads vectors; 2 is second in both.
        let kw = hits(&[(1, 5), (2, 5), (3, 5)]);
        let vec = hits(&[(9, 5), (2, 5), (8, 5)]);
        // 2: 2/62; 1 and 9: 1/61 each; 3 and 8: 1/63 each.
        assert_eq!(fuse(&kw, &vec, RRF_K), vec![2, 1, 9, 3, 8]);
    }

    #[test]
    fn ties_break_by_newest_then_message_id() {
        // 1 and 2 swap places across the lists, so they tie exactly.
        let kw = hits(&[(1, 10), (2, 20)]);
        let vec = hits(&[(2, 20), (1, 10)]);
        assert_eq!(fuse(&kw, &vec, RRF_K), vec![2, 1], "newest first");

        let mut a = hit(7, 50);
        a.message_id = "b".into();
        let mut b = hit(8, 50);
        b.message_id = "a".into();
        let kw = vec![a.clone(), b.clone()];
        let vc = vec![b, a];
        assert_eq!(fuse(&kw, &vc, RRF_K), vec![8, 7], "equal dates: id order");
    }

    #[test]
    fn order_is_stable_across_input_order_and_repeats() {
        let kw = hits(&[(1, 10), (2, 20), (3, 30), (2, 20)]);
        let vec = hits(&[(3, 30), (1, 10), (4, 40)]);
        let first = fuse(&kw, &vec, RRF_K);
        for _ in 0..20 {
            assert_eq!(fuse(&kw, &vec, RRF_K), first);
        }
        assert_eq!(first.len(), 4, "each row appears once");
    }
}
