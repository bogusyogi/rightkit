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
