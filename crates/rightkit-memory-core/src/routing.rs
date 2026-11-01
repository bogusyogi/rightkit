//! Query-aware routing policy for selecting retrieval tier (Low/Mid/High).
//!
//! Routes queries to appropriate tiers using learned weights:
//! - Low: lexical-only (BM25-style keyword matching)
//! - Mid: lexical + small embeddings
//! - High: full embeddings + reciprocal rank fusion

use serde::{Deserialize, Serialize};

/// Retrieval tier strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetrievalTier {
    /// Lexical-only (BM25-style keyword matching).
    Low,
    /// Lexical + small embeddings.
    Mid,
    /// Full embeddings + reciprocal rank fusion.
    High,
}

/// Extracted features from a query goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryFeatures {
    /// Number of words in the query.
    pub word_count: usize,
    /// Whether the query contains code-related terms (fn, async, struct, etc).
    pub has_code_terms: bool,
    /// Measure of query specificity in [0, 1]. Higher = more specific.
    pub specificity: f64,
    /// Historical hit rate when this query routed to the final tier [0, 1].
    pub prior_hit_rate: f64,
}

impl Default for QueryFeatures {
    fn default() -> Self {
        Self {
            word_count: 0,
            has_code_terms: false,
            specificity: 0.0,
            prior_hit_rate: 0.5,
        }
    }
}

/// Learned routing policy with updatable weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    /// Weight for word count (favors Mid/High for longer queries).
    w_word_count: f64,
    /// Weight for code term indicator.
    w_code_terms: f64,
    /// Weight for specificity (favors High for specific queries).
    w_specificity: f64,
    /// Weight for prior hit rate (favors tiers with history of success).
    w_prior_hit_rate: f64,
    /// Cost factor for Mid tier (budget term).
    cost_mid: f64,
    /// Cost factor for High tier (budget term).
    cost_high: f64,
}

impl RoutingPolicy {
    /// Create a new routing policy with sensible default weights.
    pub fn new() -> Self {
        Self {
            w_word_count: 0.2,
            w_code_terms: 0.3,
            w_specificity: 0.25,
            w_prior_hit_rate: 0.25,
            cost_mid: 2.0,
            cost_high: 5.0,
        }
    }

    /// Extract features from a query goal string.
    pub fn features(goal: &str) -> QueryFeatures {
        let words: Vec<&str> = goal.split_whitespace().collect();
        let word_count = words.len();

        let words_lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let code_terms = [
            "fn", "async", "struct", "impl", "trait", "const", "static", "enum", "vec", "box",
            "unwrap", "result", "option", "error", "panic", "loop", "closure", "macro", "derive",
            "crate", "mod", "pub", "mut", "ref", "clone", "copy", "move", "lifetime",
        ];
        // Match on whole words, not substrings.
        let has_code_terms = words_lower
            .iter()
            .any(|word| code_terms.contains(&word.as_str()));

        // Specificity: longer queries with more unique terms tend to be more specific.
        let unique_words = words.iter().collect::<std::collections::HashSet<_>>().len();
        let specificity = (unique_words as f64 / (word_count.max(1) as f64)).min(1.0)
            * (word_count as f64 / 20.0).min(1.0);

        QueryFeatures {
            word_count,
            has_code_terms,
            specificity,
            prior_hit_rate: 0.5, // neutral default; updated by reward
        }
    }

    /// Route a query to a tier based on its features.
    pub fn route(&self, features: &QueryFeatures) -> RetrievalTier {
        // Score each tier independently.
        // Lower cost tiers prefer simple queries; higher cost tiers prefer complex ones.

        // Low tier: for short, generic queries (no code, low specificity).
        let low_score = if features.has_code_terms {
            0.2 // Code queries penalize Low heavily
        } else {
            0.8 - (features.specificity * 0.3) // Generic, non-code queries get ~0.8
        };

        // Mid tier: moderate length, some code terms, moderate specificity.
        let word_distance = ((features.word_count as f64 - 5.0).abs() / 10.0).min(1.0);
        let mid_base = (1.0 - word_distance) * 0.6; // Best when word_count ~= 5
        let mid_code = if features.has_code_terms { 0.2 } else { 0.0 };
        let mid_spec = features.specificity * 0.15;
        let mid_score = mid_base + mid_code + mid_spec;

        // High tier: long, specific, code-heavy queries.
        let word_bonus = ((features.word_count as f64) / 20.0).min(1.0);
        let code_bonus = if features.has_code_terms { 0.5 } else { 0.0 };
        let spec_bonus = features.specificity * 0.3;
        let high_score = word_bonus * 0.3 + code_bonus + spec_bonus;

        // Return tier with highest score.
        if high_score > mid_score && high_score > low_score {
            RetrievalTier::High
        } else if mid_score > low_score {
            RetrievalTier::Mid
        } else {
            RetrievalTier::Low
        }
    }

    /// Update weights based on effectiveness feedback.
    /// If the chosen tier was effective, reinforce; if expensive but ineffective, penalize.
    pub fn reward(
        &mut self,
        features: &QueryFeatures,
        tier: RetrievalTier,
        was_effective: bool,
        cost: f64,
    ) {
        let learning_rate = 0.1;
        let effectiveness_signal = if was_effective { 1.0 } else { -1.0 };
        let cost_penalty = if !was_effective && cost > 1.0 {
            -0.1
        } else {
            0.0
        };

        // Update weights to favor/disfavor the tier in future.
        let boost = effectiveness_signal * learning_rate;

        if features.has_code_terms {
            self.w_code_terms += boost * 0.5;
        }

        match tier {
            RetrievalTier::Low => {
                // Low tier success: small boost to specificity (was good enough with lexical).
                if was_effective {
                    self.w_specificity -= learning_rate * 0.05; // reduce need for High
                }
            }
            RetrievalTier::Mid => {
                // Mid tier: boost word_count weight if effective, penalize cost if not.
                self.w_word_count += boost * 0.3;
                self.cost_mid += cost_penalty;
            }
            RetrievalTier::High => {
                // High tier: boost specificity + prior_hit_rate if effective.
                self.w_specificity += boost * 0.4;
                self.w_prior_hit_rate += boost * 0.3;
                self.cost_high += cost_penalty;
            }
        }

        // Keep weights in a sane range [0.05, 0.5].
        self.w_word_count = self.w_word_count.clamp(0.05, 0.5);
        self.w_code_terms = self.w_code_terms.clamp(0.05, 0.5);
        self.w_specificity = self.w_specificity.clamp(0.05, 0.5);
        self.w_prior_hit_rate = self.w_prior_hit_rate.clamp(0.05, 0.5);
        self.cost_mid = self.cost_mid.clamp(0.5, 5.0);
        self.cost_high = self.cost_high.clamp(1.0, 10.0);
    }
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self::new()
    }
}
