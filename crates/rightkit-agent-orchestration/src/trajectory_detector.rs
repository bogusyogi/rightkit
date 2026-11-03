//! Siamese-style sequence anomaly detector — a lightweight, embedding-free online
//! detector for trajectory anomalies using bigram transition probabilities.
//!
//! The detector learns normal action bigram patterns from observed sequences and
//! flags sequences that have low probability under the learned model, suggesting
//! looping, repetition, or deviation from normal behavior.

use std::collections::HashMap;

/// A lightweight sequence-aware anomaly scorer using bigram statistics.
/// This is "Siamese-style" in that it models pairwise action transitions
/// (no actual neural net needed, just transition probability learning).
pub struct SequenceAnomalyDetector {
    /// Window size for recent-actions context (e.g., last 10 actions).
    window: usize,
    /// Transition counts: (prev_action, next_action) -> count.
    bigrams: HashMap<(String, String), usize>,
    /// Unigram counts for normalization.
    unigrams: HashMap<String, usize>,
    /// Total transitions observed for probability estimation.
    total_transitions: usize,
}

impl SequenceAnomalyDetector {
    /// Create a new detector with a given window size.
    /// Window size determines how many recent actions are considered for scoring.
    pub fn new(window: usize) -> Self {
        Self {
            window: window.max(1),
            bigrams: HashMap::new(),
            unigrams: HashMap::new(),
            total_transitions: 0,
        }
    }

    /// Observe a single action, updating the bigram statistics.
    /// This learns the normal transition patterns.
    pub fn observe(&mut self, action: &str) {
        *self.unigrams.entry(action.to_string()).or_insert(0) += 1;
    }

    /// Record a transition from one action to the next.
    /// Called when we observe action A followed by action B.
    fn record_transition(&mut self, from: &str, to: &str) {
        let key = (from.to_string(), to.to_string());
        *self.bigrams.entry(key).or_insert(0) += 1;
        self.total_transitions += 1;
    }

    /// Compute the anomaly score for a sequence of recent actions.
    /// Returns 0.0 for perfectly normal sequences, approaching 1.0 for highly anomalous ones.
    /// Uses negative log-likelihood of the sequence under the learned bigram model.
    pub fn anomaly_score(&self, recent_actions: &[String]) -> f64 {
        let start = recent_actions.len().saturating_sub(self.window);
        let recent_actions = &recent_actions[start..];
        if recent_actions.len() < 2 {
            return 0.0; // Need at least 2 actions to compute bigram score
        }

        if self.total_transitions == 0 {
            return 0.0; // No training data yet
        }

        let mut score = 0.0;
        let mut valid_bigrams = 0;

        for window_slice in recent_actions.windows(2) {
            let from = &window_slice[0];
            let to = &window_slice[1];

            let key = (from.clone(), to.clone());
            let count = self.bigrams.get(&key).copied().unwrap_or(0);

            let from_count = self.unigrams.get(from).copied().unwrap_or(1);
            let transition_prob = if from_count > 0 {
                count as f64 / from_count as f64
            } else {
                0.001 // Unseen transition
            };

            if transition_prob > 0.0 {
                score += -(transition_prob.ln());
                valid_bigrams += 1;
            } else {
                // Unseen transition is maximally anomalous
                score += 10.0;
                valid_bigrams += 1;
            }
        }

        if valid_bigrams == 0 {
            return 0.0;
        }

        let avg_score = score / valid_bigrams as f64;
        (avg_score / 10.0).min(1.0)
    }

    /// Check if a sequence is anomalous based on a threshold.
    /// Returns true if the anomaly score exceeds the threshold.
    pub fn is_anomalous(&self, recent: &[String], threshold: f64) -> bool {
        self.anomaly_score(recent) > threshold
    }

    /// Train the detector with a known-good sequence (e.g., a successful agent run).
    pub fn train_sequence(&mut self, actions: &[String]) {
        for action in actions {
            self.observe(action);
        }

        // Record all transitions in the sequence
        for window_slice in actions.windows(2) {
            self.record_transition(&window_slice[0], &window_slice[1]);
        }
    }

    /// Return the current bigram count for a transition.
    pub fn bigram_count(&self, from: &str, to: &str) -> usize {
        self.bigrams
            .get(&(from.to_string(), to.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Return the current unigram count for an action.
    pub fn unigram_count(&self, action: &str) -> usize {
        self.unigrams.get(action).copied().unwrap_or(0)
    }
}
