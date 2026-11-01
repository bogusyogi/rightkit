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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn temp_json_file(content: &str) -> String {
        let dir = std::env::temp_dir();
        let path = dir.join("test_instincts.json");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path.to_string_lossy().to_string()
    }

    fn instinct(id: &str, rule: &str) -> Instinct {
        Instinct {
            id: id.to_string(),
            rule: rule.to_string(),
            observed_at_sessions: vec![],
            created_at: "2026-06-01".to_string(),
            deprecated: false,
        }
    }

    #[test]
    fn store_add_and_all() {
        let mut store = InstinctStore::new();
        assert!(store.is_empty());
        store.add(instinct("i1", "rule1"));
        store.add(instinct("i2", "rule2"));
        assert_eq!(store.len(), 2);
        assert_eq!(store.all().len(), 2);
    }

    #[test]
    fn store_active_filters_deprecated() {
        let mut store = InstinctStore::new();
        let inst1 = instinct("i1", "rule1");
        let mut inst2 = instinct("i2", "rule2");
        inst2.deprecated = true;
        store.add(inst1);
        store.add(inst2);
        assert_eq!(store.all().len(), 2);
        assert_eq!(store.active().len(), 1);
        assert_eq!(store.active()[0].id, "i1");
    }

    #[test]
    fn store_deprecate() {
        let mut store = InstinctStore::new();
        store.add(instinct("i1", "rule1"));
        store.deprecate("i1").unwrap();
        assert_eq!(store.active().len(), 0);
        assert_eq!(store.all().len(), 1);
        assert!(store.all()[0].deprecated);
    }

    #[test]
    fn store_deprecate_nonexistent() {
        let mut store = InstinctStore::new();
        let result = store.deprecate("ghost");
        assert!(result.is_err());
    }

    #[test]
    fn store_merge_with_seed() {
        let mut store = InstinctStore::new();
        store.add(instinct("i1", "existing"));
        let seeds = vec![
            instinct("i1", "should_not_overwrite"),
            instinct("i2", "seed"),
        ];
        store.merge_with_seed(seeds);
        assert_eq!(store.len(), 2);
        assert_eq!(store.all()[0].rule, "existing"); // i1 unchanged
        assert_eq!(store.all()[1].rule, "seed");
    }

    #[test]
    fn load_from_json() {
        let content = r#"{
  "instincts": [
    {
      "id": "i1",
      "rule": "avoid nil",
      "created_at": "2026-06-01",
      "deprecated": false,
      "observed_at_sessions": ["s1", "s2"]
    },
    {
      "id": "i2",
      "rule": "use cache",
      "created_at": "2026-06-02",
      "deprecated": true,
      "observed_at_sessions": ["s3"]
    }
  ]
}"#;
        let path = temp_json_file(content);
        let store = InstinctStore::load_from_json(&path).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.all()[0].id, "i1");
        assert!(store.all()[1].deprecated);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn to_json_roundtrip() {
        let mut store = InstinctStore::new();
        let mut inst = instinct("i1", "test rule");
        inst.observed_at_sessions = vec!["s1".to_string(), "s2".to_string()];
        store.add(inst);
        let json = store.to_json().unwrap();
        assert!(json.contains("i1"));
        assert!(json.contains("test rule"));
        assert!(json.contains("s1"));
    }

    #[test]
    fn preference_eval_gate_filters_deprecated() {
        let inst1 = instinct("i1", "active");
        let mut inst2 = instinct("i2", "deprecated");
        inst2.deprecated = true;
        let instincts = vec![&inst1, &inst2];
        let filtered = PreferenceEvalGate::filter(instincts);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "i1");
    }

    #[test]
    fn preference_eval_gate_sorts_by_recency() {
        let mut inst1 = instinct("i1", "old");
        inst1.created_at = "2026-06-01".to_string();
        let mut inst2 = instinct("i2", "new");
        inst2.created_at = "2026-06-05".to_string();
        let instincts = vec![&inst1, &inst2];
        let filtered = PreferenceEvalGate::filter(instincts);
        assert_eq!(filtered[0].id, "i2"); // Newer first
        assert_eq!(filtered[1].id, "i1");
    }

    #[test]
    fn preference_eval_gate_empty() {
        let instincts: Vec<_> = vec![];
        let filtered = PreferenceEvalGate::filter(instincts);
        assert_eq!(filtered.len(), 0);
    }
}
