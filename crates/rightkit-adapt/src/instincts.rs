//! Instinct store: manages approved taste/memory as evolving event-driven signals.
//!
//! An [`Instinct`] is a rule or preference observed during sessions, with:
//! - A unique identifier
//! - A rule (pattern/behavior to adopt or avoid)
//! - Timestamps and sessions where it was observed
//! - Deprecation flag for taste evolution
//!
//! The store loads from `~/<state-dir>/instincts.json`, merges with seed instincts,
//! and exports to JSON for persistence.

use serde::{Deserialize, Serialize};

/// An approved instinct: a taste/memory rule with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instinct {
    /// Unique identifier, e.g. "instinct-0".
    pub id: String,
    /// The rule/pattern itself (human-readable).
    pub rule: String,
    /// Sessions where this instinct was observed.
    pub observed_at_sessions: Vec<String>,
    /// When the instinct was first approved.
    pub created_at: String,
    /// Whether this instinct has been superseded or marked outdated.
    pub deprecated: bool,
}

/// In-memory collection of instincts loaded from file and merged with seeds.
#[derive(Debug, Clone, Default)]
pub struct InstinctStore {
    instincts: Vec<Instinct>,
}

impl InstinctStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            instincts: Vec::new(),
        }
    }

    /// Add an instinct to the store.
    pub fn add(&mut self, instinct: Instinct) {
        self.instincts.push(instinct);
    }

    /// Load instincts from a JSON file.
    pub fn load_from_json(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let data: JsonData = serde_json::from_str(&content)?;
        Ok(Self {
            instincts: data.instincts.unwrap_or_default(),
        })
    }

    /// Merge in a set of seed instincts (for defaults/bootstrapping).
    /// Instincts already in the store are not overwritten.
    pub fn merge_with_seed(&mut self, seeds: Vec<Instinct>) {
        let existing_ids: std::collections::HashSet<_> =
            self.instincts.iter().map(|i| i.id.clone()).collect();
        for seed in seeds {
            if !existing_ids.contains(&seed.id) {
                self.instincts.push(seed);
            }
        }
    }

    /// Return non-deprecated instincts.
    pub fn active(&self) -> Vec<&Instinct> {
        self.instincts.iter().filter(|i| !i.deprecated).collect()
    }

    /// Return all instincts.
    pub fn all(&self) -> Vec<&Instinct> {
        self.instincts.iter().collect()
    }

    /// Mark an instinct as deprecated by id.
    pub fn deprecate(&mut self, id: &str) -> Result<(), String> {
        match self.instincts.iter_mut().find(|i| i.id == id) {
            Some(instinct) => {
                instinct.deprecated = true;
                Ok(())
            }
            None => Err(format!("instinct not found: {}", id)),
        }
    }

    /// Export the store to JSON format (as a string).
    pub fn to_json(&self) -> Result<String, Box<dyn std::error::Error>> {
        let data = JsonData {
            instincts: Some(self.instincts.clone()),
        };
        Ok(serde_json::to_string_pretty(&data)?)
    }

    /// Number of instincts in the store.
    pub fn len(&self) -> usize {
        self.instincts.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.instincts.is_empty()
    }
}

/// Preference evaluation gate: filters and ranks instincts by recency and active status.
#[derive(Debug, Clone)]
pub struct PreferenceEvalGate;

impl PreferenceEvalGate {
    /// Filter instincts, preferring fresh taste over stale.
    /// Returns non-deprecated instincts ranked by recency (assuming `created_at`
    /// is parseable as a timestamp-like string; here we sort lexicographically).
    pub fn filter(instincts: Vec<&Instinct>) -> Vec<&Instinct> {
        let mut filtered: Vec<_> = instincts.into_iter().filter(|i| !i.deprecated).collect();
        filtered.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // Newer first
        filtered
    }
}

/// Internal JSON structure for serialization/deserialization.
#[derive(Debug, Serialize, Deserialize)]
struct JsonData {
    #[serde(default)]
    instincts: Option<Vec<Instinct>>,
}
