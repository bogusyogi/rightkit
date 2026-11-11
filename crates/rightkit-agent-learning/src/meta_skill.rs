//! Meta-skill refinement for SkillOpt-style training runs.
//!
//! The meta-skill is the trainable guidance document for proposing better
//! future skill edits. It is updated from accepted/rejected candidate evidence
//! at epoch boundaries while remaining pure and deterministic.

use crate::skill_opt::SkillOptimizationRun;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptMetaSkill {
    pub version: u32,
    pub body: String,
    pub content_hash: String,
    pub refined_from_epoch: u32,
    pub accepted_pattern_count: usize,
    pub rejected_pattern_count: usize,
    pub guidance: Vec<String>,
    pub updated_at: String,
}

impl Default for SkillOptMetaSkill {
    fn default() -> Self {
        initial_skillopt_meta_skill(
            "unknown",
            "preserve and improve skill behavior",
            "1970-01-01T00:00:00Z".to_string(),
        )
    }
}

pub fn initial_skillopt_meta_skill(
    skill_id: &str,
    objective: &str,
    created_at: String,
) -> SkillOptMetaSkill {
    let guidance = vec![
        "Propose bounded add/delete/replace edits only.".to_string(),
        "Preserve the existing skill's trigger, scope, and safety constraints unless the objective explicitly changes them.".to_string(),
        "Prefer edits that add verifiable behavior and remove ambiguity.".to_string(),
        "Reject changes that reduce held-out validation score or bypass configured gates.".to_string(),
    ];
    let body = render_meta_skill_body(1, skill_id, objective, &guidance, &[], &[]);
    SkillOptMetaSkill {
        version: 1,
        content_hash: meta_skill_hash(&body),
        body,
        refined_from_epoch: 0,
        accepted_pattern_count: 0,
        rejected_pattern_count: 0,
        guidance,
        updated_at: created_at,
    }
}

pub fn refine_skillopt_meta_skill(
    previous: &SkillOptMetaSkill,
    run: &SkillOptimizationRun,
    updated_at: String,
) -> SkillOptMetaSkill {
    let accepted = run
        .candidates
        .iter()
        .filter(|candidate| candidate.accepted)
        .map(|candidate| {
            format!(
                "Repeat `{}` edits when they improve validation from {:.2} to {:.2}: {}",
                candidate.operation,
                candidate.baseline_score,
                candidate.validation_score,
                candidate.summary
            )
        })
        .collect::<Vec<_>>();
    let rejected = run
        .candidates
        .iter()
        .filter(|candidate| !candidate.accepted)
        .map(|candidate| {
            format!(
                "Avoid `{}` edits rejected at {:.2}: {}",
                candidate.operation,
                candidate.validation_score,
                candidate
                    .rejection_reason
                    .as_deref()
                    .unwrap_or("candidate failed the selection gate")
            )
        })
        .collect::<Vec<_>>();

    let mut guidance = previous.guidance.clone();
    if !accepted.is_empty() {
        push_unique(
            &mut guidance,
            "Prefer proposal patterns that have already produced strict validation gains.",
        );
    }
    if !rejected.is_empty() {
        push_unique(
            &mut guidance,
            "Before proposing a broad rewrite, explain why it will not repeat rejected edit patterns.",
        );
    }
    if run
        .gates
        .iter()
        .any(|gate| gate.threshold >= run.skill_bundle.verification_score)
    {
        push_unique(
            &mut guidance,
            "When gates are near or above the current score, make smaller edits that isolate one behavior at a time.",
        );
    }

    let version = previous.version.saturating_add(1);
    let body = render_meta_skill_body(
        version,
        &run.target.skill_id,
        &run.target.objective,
        &guidance,
        &accepted,
        &rejected,
    );
    SkillOptMetaSkill {
        version,
        content_hash: meta_skill_hash(&body),
        body,
        refined_from_epoch: run.current_epoch,
        accepted_pattern_count: accepted.len(),
        rejected_pattern_count: rejected.len(),
        guidance,
        updated_at,
    }
}

fn push_unique(guidance: &mut Vec<String>, value: &str) {
    if !guidance.iter().any(|existing| existing == value) {
        guidance.push(value.to_string());
    }
}

fn render_meta_skill_body(
    version: u32,
    skill_id: &str,
    objective: &str,
    guidance: &[String],
    accepted: &[String],
    rejected: &[String],
) -> String {
    format!(
        "# SkillOpt Meta-Skill v{version}\n\n## Target\n- skill: {skill_id}\n- objective: {objective}\n\n## Proposal guidance\n{}\n\n## Accepted patterns\n{}\n\n## Rejected patterns\n{}\n",
        guidance
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        list_or_none(accepted),
        list_or_none(rejected),
    )
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "- none yet".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn meta_skill_hash(content: &str) -> String {
    let mut hash = 0x9e3779b185ebca87_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.rotate_left(5).wrapping_mul(0x517cc1b727220a95);
    }
    format!("metaskill:{hash:016x}")
}
