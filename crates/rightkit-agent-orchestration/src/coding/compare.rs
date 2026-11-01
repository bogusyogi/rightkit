//! Blind model comparison — run several models on the same prompt, hide which
//! is which, let the operator (or an auto-judge) pick the best, then reveal and
//! feed the result back into model selection.
//!
//! Blinding removes brand bias: you judge the *output*, not the logo. The pick
//! is evidence the promotion/taste loop can act on. This module is the pure
//! blinding + tally logic; running the models and persistence live in the daemon.

use serde::{Deserialize, Serialize};

/// A model's raw output for the shared prompt, before blinding.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub model_id: String,
    pub output: String,
}

/// One blinded entry shown to the judge — the model identity is withheld.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlindEntry {
    pub label: String,
    pub output: String,
}

/// The outcome of a judged comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonOutcome {
    pub winner_model: String,
    /// Model ids in presentation order (so callers can see the full field).
    pub field: Vec<String>,
}

/// A blinded comparison: candidates shuffled into labelled entries (A, B, C…)
/// with the label→model mapping kept private until [`reveal`](Self::reveal).
#[derive(Debug, Clone)]
pub struct BlindComparison {
    pub id: String,
    pub prompt: String,
    /// label → model_id, in shuffled order.
    mapping: Vec<(String, String)>,
    entries: Vec<BlindEntry>,
}

impl BlindComparison {
    /// Build a blinded comparison. `seed` makes the shuffle deterministic (tests
    /// pass a fixed seed; production passes a random one).
    pub fn new(
        id: impl Into<String>,
        prompt: impl Into<String>,
        candidates: Vec<Candidate>,
        seed: u64,
    ) -> Self {
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        shuffle(&mut order, seed);
        let mut mapping = Vec::with_capacity(order.len());
        let mut entries = Vec::with_capacity(order.len());
        for (i, &idx) in order.iter().enumerate() {
            let label = label_for(i);
            mapping.push((label.clone(), candidates[idx].model_id.clone()));
            entries.push(BlindEntry {
                label,
                output: candidates[idx].output.clone(),
            });
        }
        Self {
            id: id.into(),
            prompt: prompt.into(),
            mapping,
            entries,
        }
    }

    /// The blinded entries to show the judge (no model identities).
    pub fn blinded(&self) -> &[BlindEntry] {
        &self.entries
    }

    /// The full label→model mapping (for reveal after judging).
    pub fn reveal(&self) -> &[(String, String)] {
        &self.mapping
    }

    /// Resolve a winning label to its model and the full field. Returns `None`
    /// for an unknown label.
    pub fn judge(&self, winner_label: &str) -> Option<ComparisonOutcome> {
        let winner_model = self
            .mapping
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(winner_label))
            .map(|(_, m)| m.clone())?;
        Some(ComparisonOutcome {
            winner_model,
            field: self.mapping.iter().map(|(_, m)| m.clone()).collect(),
        })
    }
}

/// Labels A, B, …, Z, then A1, B1, … for large fields.
fn label_for(i: usize) -> String {
    let letter = (b'A' + (i % 26) as u8) as char;
    if i < 26 {
        letter.to_string()
    } else {
        format!("{letter}{}", i / 26)
    }
}

/// In-place Fisher-Yates using a small deterministic LCG, so no `rand` dep.
fn shuffle(v: &mut [usize], seed: u64) {
    let mut state = seed ^ 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let n = v.len();
    for i in (1..n).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> Vec<Candidate> {
        vec![
            Candidate {
                model_id: "deepseek-v4-pro".into(),
                output: "answer one".into(),
            },
            Candidate {
                model_id: "glm-4.7".into(),
                output: "answer two".into(),
            },
            Candidate {
                model_id: "kimi-k2.6".into(),
                output: "answer three".into(),
            },
        ]
    }

    #[test]
    fn blinded_hides_models_but_keeps_outputs() {
        let c = BlindComparison::new("c1", "prompt", cands(), 42);
        let blinded = c.blinded();
        assert_eq!(blinded.len(), 3);
        // Labels are A/B/C; outputs present; no model id leaks into the entries.
        let labels: Vec<_> = blinded.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["A", "B", "C"]);
        let joined = serde_json::to_string(blinded).unwrap();
        assert!(
            !joined.contains("deepseek") && !joined.contains("glm") && !joined.contains("kimi")
        );
        assert!(joined.contains("answer one") && joined.contains("answer three"));
    }

    #[test]
    fn judge_resolves_label_to_model_and_field() {
        let c = BlindComparison::new("c1", "p", cands(), 7);
        // Whatever model sits at label "A", judging A must return that model.
        let model_at_a = c.reveal()[0].1.clone();
        let outcome = c.judge("a").unwrap();
        assert_eq!(outcome.winner_model, model_at_a);
        assert_eq!(outcome.field.len(), 3);
        assert!(c.judge("Z9").is_none());
    }

    #[test]
    fn shuffle_is_deterministic_per_seed_and_varies_across_seeds() {
        let a = BlindComparison::new("c", "p", cands(), 1);
        let b = BlindComparison::new("c", "p", cands(), 1);
        let d = BlindComparison::new("c", "p", cands(), 2);
        assert_eq!(a.reveal(), b.reveal(), "same seed = same order");
        // Different seeds usually differ; with 3 items and these seeds they do.
        assert_ne!(a.reveal(), d.reveal());
    }
}
