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
