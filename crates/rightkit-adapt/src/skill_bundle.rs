//! Synthesized skill bundles ready for human approval.
//!
//! A `SkillBundle` is the output of the CoEvoSkills synthesis loop:
//! a packaged, verified skill candidate that awaits operator review.

use serde::{Deserialize, Serialize};

/// A synthesized skill candidate ready for human approval.
/// Output of the CoEvoSkills synthesis loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillBundle {
    /// Unique identifier for this bundle.
    pub id: String,
    /// Human-readable name for the skill.
    pub name: String,
    /// Description of what the skill does.
    pub description: String,
    /// The synthesized skill body (SKILL.md compatible content).
    pub skill_body: String,
    /// Number of synthesis+verify cycles completed.
    pub cycles: u32,
    /// Verification score from the last cycle (0.0-1.0).
    pub verification_score: f64,
    /// The source proposals that were synthesized.
    pub source_proposal_ids: Vec<String>,
    /// Whether the bundle has been approved by a human operator.
    pub approved: bool,
}

impl SkillBundle {
    /// Create a new skill bundle with the given name and description.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: format!("skill-bundle-{}", uuid_like()),
            name: name.into(),
            description: description.into(),
            skill_body: String::new(),
            cycles: 0,
            verification_score: 0.0,
            source_proposal_ids: Vec::new(),
            approved: false,
        }
    }
}

/// Simple deterministic ID generation without uuid dep.
/// Uses current time in microseconds as hex.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("{ts:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_bundle_with_unique_id() {
        let b1 = SkillBundle::new("test", "a test skill");
        let b2 = SkillBundle::new("test", "a test skill");
        assert_ne!(b1.id, b2.id);
        assert!(b1.id.starts_with("skill-bundle-"));
    }

    #[test]
    fn initializes_with_default_values() {
        let bundle = SkillBundle::new("my-skill", "does things");
        assert_eq!(bundle.name, "my-skill");
        assert_eq!(bundle.description, "does things");
        assert_eq!(bundle.skill_body, "");
        assert_eq!(bundle.cycles, 0);
        assert_eq!(bundle.verification_score, 0.0);
        assert!(bundle.source_proposal_ids.is_empty());
        assert!(!bundle.approved);
    }

    #[test]
    fn roundtrip_serde() {
        let mut bundle = SkillBundle::new("test-skill", "test description");
        bundle.cycles = 3;
        bundle.verification_score = 0.85;
        bundle.source_proposal_ids = vec!["prop-1".to_string(), "prop-2".to_string()];
        bundle.approved = false;

        let json = serde_json::to_string(&bundle).unwrap();
        let back: SkillBundle = serde_json::from_str(&json).unwrap();

        assert_eq!(back.id, bundle.id);
        assert_eq!(back.name, bundle.name);
        assert_eq!(back.description, bundle.description);
        assert_eq!(back.cycles, 3);
        assert!((back.verification_score - 0.85).abs() < 1e-9);
        assert_eq!(back.source_proposal_ids, vec!["prop-1", "prop-2"]);
    }
}
