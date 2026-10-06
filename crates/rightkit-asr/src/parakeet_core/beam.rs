//! [PHASE-2 CORE PRIMITIVES — NOT A BLIND `decode_window.rs` REPLACEMENT]
//!
//! The recurrent CoreML/ORT calls remain in platform adapters during extraction.
//! These types/utilities centralize hypothesis representation and deterministic
//! pruning. Claude should port the *current local* loop mechanically and prove
//! parity before deleting platform implementations.

use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct BeamHypothesis<S> {
    pub tokens: Vec<usize>,
    pub score: f32,
    pub t: usize,
    pub last_token: usize,
    pub state: S,
    /// Monotonic insertion order supplied by the caller. This makes ties explicit
    /// instead of depending on unstable sort behavior.
    pub ordinal: u64,
}

fn score_order<S>(a: &BeamHypothesis<S>, b: &BeamHypothesis<S>) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.ordinal.cmp(&b.ordinal))
}

/// Extraction-compatible global pruning primitive. This still compares hypotheses
/// at different `t`; it does NOT claim to solve the under-generation architecture.
pub fn prune_global_stable<S>(
    mut hypotheses: Vec<BeamHypothesis<S>>,
    width: usize,
) -> Vec<BeamHypothesis<S>> {
    hypotheses.sort_by(score_order);
    hypotheses.truncate(width.max(1).min(hypotheses.len()));
    hypotheses
}

/// Experimental/principled direction: prune independently within equal encoder
/// time. Kept separate so turning it on is an explicit behavior change with its own
/// WER/S-D-I/silence evaluation, not an accidental consequence of refactoring.
pub fn prune_time_synchronous_stable<S>(
    hypotheses: Vec<BeamHypothesis<S>>,
    width_per_t: usize,
) -> Vec<BeamHypothesis<S>> {
    let mut by_t: BTreeMap<usize, Vec<BeamHypothesis<S>>> = BTreeMap::new();
    for h in hypotheses {
        by_t.entry(h.t).or_default().push(h);
    }

    let mut out = Vec::new();
    for (_, mut group) in by_t {
        group.sort_by(score_order);
        group.truncate(width_per_t.max(1).min(group.len()));
        out.extend(group);
    }
    out
}
