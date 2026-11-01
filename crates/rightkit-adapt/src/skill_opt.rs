//! SkillOpt-style optimization runs.
//!
//! This is the deterministic bookkeeping layer for closed skill improvement:
//! record the optimization target, rejected edit buffer, and benchmark gates,
//! then package a candidate skill bundle for human review.

use crate::eval_gate::{EvalEvidence, EvalGate, GateOutcome};
use crate::meta_skill::{
    initial_skillopt_meta_skill, refine_skillopt_meta_skill, SkillOptMetaSkill,
};
use crate::skill_bundle::SkillBundle;
use serde::{Deserialize, Serialize};

const SKILLOPT_PHASES: &[&str] = &[
    "rollout",
    "reflect",
    "aggregate",
    "select",
    "update",
    "evaluate",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationTarget {
    pub skill_id: String,
    pub objective: String,
    pub optimizer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RejectedEdit {
    pub artifact_id: String,
    pub reason: String,
    pub patch_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkGateKind {
    Hard,
    Soft,
    Mixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkGate {
    pub name: String,
    pub kind: BenchmarkGateKind,
    pub threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationCandidate {
    pub id: String,
    pub epoch: u32,
    pub operation: String,
    pub summary: String,
    pub proposed_body: String,
    pub proposed_hash: String,
    pub baseline_score: f64,
    pub validation_score: f64,
    /// The exact held-out evidence used for this decision. Keeping the
    /// structured input with the candidate makes the promotion decision
    /// auditable after restart; the rejection reason is only a summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_evidence: Option<EvalEvidence>,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationCandidateProposal {
    pub id: String,
    pub run_id: String,
    pub epoch: u32,
    pub operation: String,
    pub summary: String,
    pub proposed_body: String,
    pub proposed_hash: String,
    pub source: String,
    pub meta_skill_hash: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationProposalInput {
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationProposalResult {
    pub proposed: bool,
    pub proposal: SkillOptimizationCandidateProposal,
    pub run: SkillOptimizationRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationEpochInput {
    pub proposed_body: String,
    pub validation_score: f64,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub gate_mode: Option<BenchmarkGateKind>,
    #[serde(default)]
    pub threshold: Option<f64>,
    /// FM-091: full executed-bakeoff evidence (train/heldout/safety/cost/
    /// latency/regression). When present the run's own `EvalGate` — the same
    /// gate every other learning proposal is promoted through
    /// (`proposals.rs::ProposalBuilder::build`, `api_workflows/routes.rs`) —
    /// must also `Proceed` before a candidate can be accepted. Missing
    /// evidence is a hard rejection: a run-local score or caller-supplied
    /// threshold is not proof that a held-out bakeoff was executed.
    #[serde(default)]
    pub eval_evidence: Option<EvalEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationEpochResult {
    pub accepted: bool,
    pub candidate: SkillOptimizationCandidate,
    pub run: SkillOptimizationRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkillOptimizationStatus {
    Pending,
    Running,
    Paused,
    Accepted,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationHistoryEntry {
    pub event: String,
    pub epoch: u32,
    pub message: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationRun {
    pub id: String,
    pub name: String,
    pub target: SkillOptimizationTarget,
    pub rejected_edits: Vec<RejectedEdit>,
    pub gates: Vec<BenchmarkGate>,
    pub status: SkillOptimizationStatus,
    pub current_epoch: u32,
    pub history: Vec<SkillOptimizationHistoryEntry>,
    #[serde(default)]
    pub candidates: Vec<SkillOptimizationCandidate>,
    #[serde(default)]
    pub pending_candidate_proposals: Vec<SkillOptimizationCandidateProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_checkpoint: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_version_hash: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub skill_bundle: SkillBundle,
    #[serde(default)]
    pub meta_skill: SkillOptMetaSkill,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillOptimizationStore {
    #[serde(default)]
    runs: Vec<SkillOptimizationRun>,
}

impl SkillOptimizationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn runs(&self) -> &[SkillOptimizationRun] {
        &self.runs
    }

    pub fn run(&self, id: &str) -> Option<&SkillOptimizationRun> {
        self.runs.iter().find(|run| run.id == id)
    }

    pub fn add_run(
        &mut self,
        id: String,
        name: String,
        target: SkillOptimizationTarget,
        rejected_edits: Vec<RejectedEdit>,
        gates: Vec<BenchmarkGate>,
        created_at: String,
    ) -> SkillOptimizationRun {
        let skill_bundle = build_skill_bundle(&name, &target, &rejected_edits, &gates);
        let meta_skill =
            initial_skillopt_meta_skill(&target.skill_id, &target.objective, created_at.clone());
        let best_version_hash = content_hash(&skill_bundle.skill_body);
        let checkpoint = serde_json::json!({
            "runId": id,
            "skillId": target.skill_id,
            "currentEpoch": 0,
            "status": "pending",
            "acceptedCandidateCount": 0,
            "rejectedEditCount": rejected_edits.len(),
            "gateCount": gates.len(),
            "bestVersionHash": best_version_hash,
            "metaSkillHash": meta_skill.content_hash.clone(),
        });
        let run = SkillOptimizationRun {
            id,
            name,
            target,
            rejected_edits,
            gates,
            status: SkillOptimizationStatus::Pending,
            current_epoch: 0,
            history: vec![SkillOptimizationHistoryEntry {
                event: "created".to_string(),
                epoch: 0,
                message: "training run created".to_string(),
                created_at: created_at.clone(),
                checkpoint: Some(checkpoint.clone()),
            }],
            candidates: Vec::new(),
            pending_candidate_proposals: Vec::new(),
            resume_checkpoint: Some(checkpoint),
            best_version_hash: Some(best_version_hash),
            created_at: created_at.clone(),
            updated_at: created_at,
            completed_at: None,
            skill_bundle,
            meta_skill,
        };
        self.runs.push(run.clone());
        run
    }

    pub fn resume_run(
        &mut self,
        id: &str,
        checkpoint: Option<serde_json::Value>,
        message: String,
        updated_at: String,
    ) -> Option<SkillOptimizationRun> {
        let run = self.runs.iter_mut().find(|run| run.id == id)?;
        run.current_epoch = run.current_epoch.saturating_add(1);
        run.status = SkillOptimizationStatus::Running;
        let checkpoint = checkpoint.unwrap_or_else(|| {
            serde_json::json!({
                "runId": run.id,
                "skillId": run.target.skill_id,
                "currentEpoch": run.current_epoch,
                "status": "running",
                "rejectedEditCount": run.rejected_edits.len(),
                "gateCount": run.gates.len(),
            })
        });
        run.resume_checkpoint = Some(checkpoint.clone());
        run.updated_at = updated_at.clone();
        run.history.push(SkillOptimizationHistoryEntry {
            event: "resumed".to_string(),
            epoch: run.current_epoch,
            message,
            created_at: updated_at,
            checkpoint: Some(checkpoint),
        });
        Some(run.clone())
    }

    pub fn propose_candidate(
        &mut self,
        id: &str,
        input: SkillOptimizationProposalInput,
        proposed_at: String,
    ) -> Option<SkillOptimizationProposalResult> {
        let run = self.runs.iter_mut().find(|run| run.id == id)?;
        let epoch = run.current_epoch.saturating_add(1);
        let operation = input
            .operation
            .as_deref()
            .map(str::trim)
            .filter(|operation| !operation.is_empty())
            .unwrap_or_else(|| default_candidate_operation(run))
            .chars()
            .take(80)
            .collect::<String>();
        let summary = input
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .unwrap_or_else(|| default_candidate_summary(run))
            .chars()
            .take(500)
            .collect::<String>();
        let instructions = input
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty());
        let proposed_body = build_candidate_proposal_body(run, &operation, &summary, instructions);
        let proposed_hash = content_hash(&proposed_body);
        let proposal_id = format!(
            "{}-proposal-{}",
            run.id,
            run.pending_candidate_proposals.len().saturating_add(1)
        );
        let proposal = SkillOptimizationCandidateProposal {
            id: proposal_id.clone(),
            run_id: run.id.clone(),
            epoch,
            operation,
            summary,
            proposed_body,
            proposed_hash,
            source: "skillopt-meta-skill".to_string(),
            meta_skill_hash: run.meta_skill.content_hash.clone(),
            created_at: proposed_at.clone(),
        };
        run.pending_candidate_proposals.push(proposal.clone());
        run.updated_at = proposed_at.clone();
        let checkpoint = serde_json::json!({
            "runId": run.id,
            "skillId": run.target.skill_id,
            "currentEpoch": run.current_epoch,
            "status": run.status,
            "pendingCandidateProposalCount": run.pending_candidate_proposals.len(),
            "lastCandidateProposalId": proposal.id.clone(),
            "lastCandidateProposalHash": proposal.proposed_hash.clone(),
            "metaSkillHash": run.meta_skill.content_hash.clone(),
            "metaSkillVersion": run.meta_skill.version,
        });
        run.resume_checkpoint = Some(checkpoint.clone());
        run.history.push(SkillOptimizationHistoryEntry {
            event: "candidateProposed".to_string(),
            epoch,
            message: "candidate proposal generated from the current meta-skill and run evidence"
                .to_string(),
            created_at: proposed_at,
            checkpoint: Some(checkpoint),
        });

        Some(SkillOptimizationProposalResult {
            proposed: true,
            proposal,
            run: run.clone(),
        })
    }

    pub fn execute_epoch(
        &mut self,
        id: &str,
        input: SkillOptimizationEpochInput,
        executed_at: String,
    ) -> Option<SkillOptimizationEpochResult> {
        let run = self.runs.iter_mut().find(|run| run.id == id)?;
        let epoch = run.current_epoch.saturating_add(1);
        let operation = input
            .operation
            .as_deref()
            .map(str::trim)
            .filter(|operation| !operation.is_empty())
            .unwrap_or("replace")
            .chars()
            .take(80)
            .collect::<String>();
        let summary = input
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .unwrap_or("candidate skill document update")
            .chars()
            .take(500)
            .collect::<String>();
        let proposed_body = input.proposed_body;
        let proposed_hash = content_hash(&proposed_body);
        let baseline_score = run.skill_bundle.verification_score;
        // FM-091 executed bakeoff gate: promotion requires a live held-out
        // validation score (finite, not NaN/inf) that strictly improves the
        // baseline *and* satisfies all configured benchmark gates. A missing
        // bakeoff result (non-finite score) is a hard reject even if gates are
        // empty — the score must come from an executed harness run.
        if !input.validation_score.is_finite() || !baseline_score.is_finite() {
            let candidate_id = format!("{}-epoch-{}", run.id, epoch);
            let candidate = SkillOptimizationCandidate {
                id: candidate_id.clone(),
                epoch,
                operation,
                summary,
                proposed_body,
                proposed_hash,
                baseline_score: if baseline_score.is_finite() {
                    baseline_score
                } else {
                    0.0
                },
                validation_score: if input.validation_score.is_finite() {
                    input.validation_score
                } else {
                    0.0
                },
                eval_evidence: input.eval_evidence,
                accepted: false,
                rejection_reason: Some(
                    "validation or baseline score is not finite (bakeoff not executed)".to_string(),
                ),
                created_at: executed_at.clone(),
            };
            run.rejected_edits.push(RejectedEdit {
                artifact_id: candidate.id.clone(),
                reason: "non-finite bakeoff score".to_string(),
                patch_preview: candidate.proposed_body.chars().take(280).collect(),
            });
            run.current_epoch = epoch;
            run.updated_at = executed_at.clone();
            run.candidates.push(candidate.clone());
            run.status = SkillOptimizationStatus::Rejected;
            return Some(SkillOptimizationEpochResult {
                accepted: false,
                candidate,
                run: run.clone(),
            });
        }
        let gate_thresholds = selected_gate_thresholds(run, input.gate_mode.as_ref());
        let explicit_threshold = input.threshold.filter(|threshold| threshold.is_finite());
        let gate_passed = executed_bakeoff_gate_passed(
            input.validation_score,
            &gate_thresholds,
            explicit_threshold,
        );
        let improves = input.validation_score > baseline_score;
        // FM-091: promotion must clear the shared `EvalGate` (held-out
        // correctness, safety, cost, latency, regression & sample-size
        // checks) — the same gate every other proposal kind promotes
        // through. Missing evidence is represented as a block rather than
        // falling back to the run-local score. A `HoldForReview` outcome is
        // treated as not-yet-accepted here: `execute_epoch` has no separate
        // review-hold state, so the candidate stays rejected/buffered until
        // re-run with conclusive evidence.
        let eval_gate_outcome = match input.eval_evidence.as_ref() {
            Some(evidence) => Some(EvalGate::new().evaluate(evidence)),
            None => Some(GateOutcome::Block {
                reason: "executed bakeoff evidence is missing".to_string(),
            }),
        };
        let eval_gate_passed = !matches!(
            eval_gate_outcome,
            Some(GateOutcome::Block { .. }) | Some(GateOutcome::HoldForReview { .. })
        );
        let accepted = improves && gate_passed && eval_gate_passed;
        let rejection_reason = if accepted {
            None
        } else if !improves {
            Some(format!(
                "validation score {:.4} did not strictly improve baseline {:.4}",
                input.validation_score, baseline_score
            ))
        } else if !eval_gate_passed {
            match eval_gate_outcome {
                Some(GateOutcome::Block { reason }) => Some(format!(
                    "executed bakeoff eval gate blocked promotion: {reason}"
                )),
                Some(GateOutcome::HoldForReview { reason }) => Some(format!(
                    "executed bakeoff eval gate held for review: {reason}"
                )),
                Some(GateOutcome::Proceed { .. }) => None,
                None => Some("executed bakeoff evidence is missing".to_string()),
            }
        } else {
            Some(format!(
                "validation score {:.4} did not satisfy configured benchmark gate thresholds",
                input.validation_score
            ))
        };
        let candidate_id = format!("{}-epoch-{}", run.id, epoch);
        let candidate = SkillOptimizationCandidate {
            id: candidate_id.clone(),
            epoch,
            operation,
            summary,
            proposed_body,
            proposed_hash,
            baseline_score,
            validation_score: input.validation_score,
            eval_evidence: input.eval_evidence,
            accepted,
            rejection_reason: rejection_reason.clone(),
            created_at: executed_at.clone(),
        };

        for phase in SKILLOPT_PHASES {
            let message = match *phase {
                "rollout" => "candidate skill body collected for held-out validation",
                "reflect" => "candidate compared against the current best skill body",
                "aggregate" => "benchmark gate evidence aggregated for selection",
                "select" => {
                    if accepted {
                        "candidate selected because it strictly improved the validation score and passed gates"
                    } else {
                        "candidate rejected by strict-improvement or benchmark-gate selection"
                    }
                }
                "update" => {
                    if accepted {
                        "best skill body updated to accepted candidate"
                    } else {
                        "best skill body left unchanged and candidate retained in rejected-edit buffer"
                    }
                }
                "evaluate" => "run checkpoint refreshed after candidate evaluation",
                _ => "SkillOpt phase recorded",
            };
            run.history.push(SkillOptimizationHistoryEntry {
                event: (*phase).to_string(),
                epoch,
                message: message.to_string(),
                created_at: executed_at.clone(),
                checkpoint: Some(serde_json::json!({
                    "candidateId": candidate_id.clone(),
                    "candidateHash": candidate.proposed_hash.clone(),
                    "baselineScore": baseline_score,
                    "validationScore": candidate.validation_score,
                    "accepted": accepted,
                })),
            });
        }

        if accepted {
            run.skill_bundle.skill_body = candidate.proposed_body.clone();
            run.skill_bundle.verification_score = candidate.validation_score;
            run.skill_bundle.cycles = run.skill_bundle.cycles.saturating_add(1);
            run.best_version_hash = Some(candidate.proposed_hash.clone());
            run.status = SkillOptimizationStatus::Accepted;
        } else {
            run.rejected_edits.push(RejectedEdit {
                artifact_id: candidate.id.clone(),
                reason: rejection_reason
                    .clone()
                    .unwrap_or_else(|| "candidate rejected".to_string()),
                patch_preview: candidate.proposed_body.chars().take(280).collect(),
            });
            run.status = SkillOptimizationStatus::Rejected;
        }

        run.current_epoch = epoch;
        run.updated_at = executed_at.clone();
        run.candidates.push(candidate.clone());
        let previous_meta_skill = run.meta_skill.clone();
        run.meta_skill = refine_skillopt_meta_skill(&previous_meta_skill, run, executed_at.clone());
        let accepted_candidate_count = run
            .candidates
            .iter()
            .filter(|candidate| candidate.accepted)
            .count();
        let checkpoint = serde_json::json!({
            "runId": run.id,
            "skillId": run.target.skill_id,
            "currentEpoch": run.current_epoch,
            "status": if accepted { "accepted" } else { "rejected" },
            "lastCandidateId": candidate.id.clone(),
            "lastCandidateHash": candidate.proposed_hash.clone(),
            "lastValidationScore": candidate.validation_score,
            "acceptedCandidateCount": accepted_candidate_count,
            "candidateCount": run.candidates.len(),
            "rejectedEditCount": run.rejected_edits.len(),
            "gateCount": run.gates.len(),
            "bestVersionHash": run.best_version_hash.clone(),
            "metaSkillHash": run.meta_skill.content_hash.clone(),
            "metaSkillVersion": run.meta_skill.version,
        });
        run.resume_checkpoint = Some(checkpoint.clone());
        run.history.push(SkillOptimizationHistoryEntry {
            event: "metaSkillRefined".to_string(),
            epoch,
            message: "edit-proposal meta-skill refined from epoch evidence".to_string(),
            created_at: executed_at.clone(),
            checkpoint: Some(serde_json::json!({
                "metaSkillHash": run.meta_skill.content_hash.clone(),
                "metaSkillVersion": run.meta_skill.version,
                "acceptedPatternCount": run.meta_skill.accepted_pattern_count,
                "rejectedPatternCount": run.meta_skill.rejected_pattern_count,
            })),
        });
        run.history.push(SkillOptimizationHistoryEntry {
            event: "checkpoint".to_string(),
            epoch,
            message: "epoch checkpoint persisted".to_string(),
            created_at: executed_at,
            checkpoint: Some(checkpoint),
        });

        Some(SkillOptimizationEpochResult {
            accepted,
            candidate,
            run: run.clone(),
        })
    }

    pub fn cancel_run(
        &mut self,
        id: &str,
        reason: String,
        completed_at: String,
    ) -> Option<SkillOptimizationRun> {
        let run = self.runs.iter_mut().find(|run| run.id == id)?;
        run.status = SkillOptimizationStatus::Cancelled;
        run.updated_at = completed_at.clone();
        run.completed_at = Some(completed_at.clone());
        run.history.push(SkillOptimizationHistoryEntry {
            event: "cancelled".to_string(),
            epoch: run.current_epoch,
            message: reason,
            created_at: completed_at,
            checkpoint: run.resume_checkpoint.clone(),
        });
        Some(run.clone())
    }
}

/// FM-091 executed bakeoff gate predicate: every required threshold (explicit
/// plus configured gates filtered by `gate_mode`) must be satisfied by the
/// held-out validation score. This is the harness-bound promotion check — a
/// candidate without an executed bakeoff score never reaches this call site
/// because the caller already rejected non-finite scores.
fn executed_bakeoff_gate_passed(
    validation_score: f64,
    gate_thresholds: &[f64],
    explicit_threshold: Option<f64>,
) -> bool {
    explicit_threshold
        .into_iter()
        .chain(gate_thresholds.iter().copied())
        .all(|threshold| validation_score >= threshold)
}

fn selected_gate_thresholds(
    run: &SkillOptimizationRun,
    gate_mode: Option<&BenchmarkGateKind>,
) -> Vec<f64> {
    run.gates
        .iter()
        .filter(|gate| gate.threshold.is_finite())
        .filter(|gate| match gate_mode {
            Some(kind) => &gate.kind == kind,
            None => true,
        })
        .map(|gate| gate.threshold)
        .collect()
}

fn default_candidate_operation(run: &SkillOptimizationRun) -> &'static str {
    if run.rejected_edits.is_empty() {
        "add"
    } else {
        "replace"
    }
}

fn default_candidate_summary(run: &SkillOptimizationRun) -> &str {
    run.meta_skill
        .guidance
        .last()
        .map(String::as_str)
        .unwrap_or(run.target.objective.as_str())
}

fn build_candidate_proposal_body(
    run: &SkillOptimizationRun,
    operation: &str,
    summary: &str,
    instructions: Option<&str>,
) -> String {
    let base_body = strip_previous_candidate_proposal(&run.skill_bundle.skill_body);
    let guidance = if run.meta_skill.guidance.is_empty() {
        format!("- Preserve the target objective: {}", run.target.objective)
    } else {
        run.meta_skill
            .guidance
            .iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|guidance| format!("- {guidance}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rejected_guardrails = if run.rejected_edits.is_empty() {
        "- No rejected edits recorded yet; keep the change narrow and benchmarkable.".to_string()
    } else {
        run.rejected_edits
            .iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|edit| {
                format!(
                    "- avoid {} because {}; preview: {}",
                    edit.artifact_id, edit.reason, edit.patch_preview
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let gates = if run.gates.is_empty() {
        "- no explicit gates configured".to_string()
    } else {
        run.gates
            .iter()
            .map(|gate| format!("- {} ({:?}) >= {:.4}", gate.name, gate.kind, gate.threshold))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let extra = instructions
        .map(|instructions| format!("\n### Operator instructions\n{instructions}\n"))
        .unwrap_or_default();

    format!(
        "{base_body}\n\n## SkillOpt Candidate Proposal\n- target skill: {}\n- objective: {}\n- optimizer: {}\n- target model: {}\n- operation: {}\n- summary: {}\n- baseline score: {:.4}\n- best version: {}\n- meta-skill: v{} {}\n\n### Edit guidance\n{}\n\n### Rejected-edit guardrails\n{}\n\n### Benchmark gates\n{}\n{}",
        run.target.skill_id,
        run.target.objective,
        run.target.optimizer,
        run.target
            .target_model
            .as_deref()
            .unwrap_or("session/default"),
        operation,
        summary,
        run.skill_bundle.verification_score,
        run.best_version_hash.as_deref().unwrap_or("none"),
        run.meta_skill.version,
        run.meta_skill.content_hash,
        guidance,
        rejected_guardrails,
        gates,
        extra
    )
}

fn strip_previous_candidate_proposal(body: &str) -> String {
    body.split("\n## SkillOpt Candidate Proposal")
        .next()
        .unwrap_or(body)
        .trim_end()
        .to_string()
}

fn build_skill_bundle(
    name: &str,
    target: &SkillOptimizationTarget,
    rejected_edits: &[RejectedEdit],
    gates: &[BenchmarkGate],
) -> SkillBundle {
    let mut bundle = SkillBundle::new(
        sanitize_skill_name(name),
        format!("Optimizes {}: {}", target.skill_id, target.objective),
    );
    let optimizer_line = format!(
        "{}\n- target model: {}",
        target.optimizer,
        target.target_model.as_deref().unwrap_or("session/default")
    );
    bundle.skill_body = format!(
        "# Skill Optimization: {}\n\n## Target\n- skill: {}\n- objective: {}\n- optimizer: {}\n\n## Rejected edit buffer\n{}\n\n## Benchmark gates\n{}\n",
        name,
        target.skill_id,
        target.objective,
        optimizer_line,
        rejected_edits
            .iter()
            .map(|edit| format!(
                "- {}: {}\n  preview: {}",
                edit.artifact_id, edit.reason, edit.patch_preview
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        gates
            .iter()
            .map(|gate| format!("- {} ({:?}) >= {:.2}", gate.name, gate.kind, gate.threshold))
            .collect::<Vec<_>>()
            .join("\n")
    );
    bundle.verification_score = if gates
        .iter()
        .any(|gate| matches!(gate.kind, BenchmarkGateKind::Hard))
    {
        0.75
    } else {
        0.6
    };
    bundle.cycles = 1;
    bundle
}

fn sanitize_skill_name(name: &str) -> String {
    let slug = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "skill-optimization".to_string()
    } else {
        slug
    }
}

fn content_hash(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("skillopt:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_records_target_rejected_edits_and_mixed_gates() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: Some("kimi-k2".to_string()),
            },
            vec![RejectedEdit {
                artifact_id: "diff-1".to_string(),
                reason: "unsafe shell".to_string(),
                patch_preview: "rm -rf target".to_string(),
            }],
            vec![
                BenchmarkGate {
                    name: "unit".to_string(),
                    kind: BenchmarkGateKind::Hard,
                    threshold: 1.0,
                },
                BenchmarkGate {
                    name: "quality".to_string(),
                    kind: BenchmarkGateKind::Mixed,
                    threshold: 0.8,
                },
            ],
            "2026-06-09T00:00:00Z".to_string(),
        );

        assert_eq!(run.status, SkillOptimizationStatus::Pending);
        assert_eq!(run.target.skill_id, "review");
        assert_eq!(run.target.target_model.as_deref(), Some("kimi-k2"));
        assert_eq!(run.rejected_edits.len(), 1);
        assert_eq!(run.current_epoch, 0);
        assert_eq!(run.history[0].event, "created");
        assert!(run.resume_checkpoint.is_some());
        assert!(run
            .best_version_hash
            .as_deref()
            .unwrap()
            .starts_with("skillopt:"));
        assert!(run.skill_bundle.skill_body.contains("Rejected edit buffer"));
        assert_eq!(store.runs().len(), 1);
    }

    #[test]
    fn run_can_resume_and_cancel_with_checkpoint_history() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            Vec::new(),
            "2026-06-09T00:00:00Z".to_string(),
        );

        let resumed = store
            .resume_run(
                &run.id,
                Some(serde_json::json!({ "nextCandidate": "skill_v0002.md" })),
                "resume after benchmark refresh".to_string(),
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();
        assert_eq!(resumed.status, SkillOptimizationStatus::Running);
        assert_eq!(resumed.current_epoch, 1);
        assert_eq!(resumed.history[1].event, "resumed");
        assert_eq!(
            resumed.resume_checkpoint.unwrap()["nextCandidate"],
            "skill_v0002.md"
        );

        let cancelled = store
            .cancel_run(
                &run.id,
                "operator stopped stale run".to_string(),
                "2026-06-09T00:02:00Z".to_string(),
            )
            .unwrap();
        assert_eq!(cancelled.status, SkillOptimizationStatus::Cancelled);
        assert_eq!(
            cancelled.completed_at.as_deref(),
            Some("2026-06-09T00:02:00Z")
        );
        assert_eq!(cancelled.history[2].event, "cancelled");
    }

    #[test]
    fn epoch_execution_accepts_strictly_better_gated_candidate() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            vec![BenchmarkGate {
                name: "held-out".to_string(),
                kind: BenchmarkGateKind::Hard,
                threshold: 0.9,
            }],
            "2026-06-09T00:00:00Z".to_string(),
        );
        let initial_hash = run.best_version_hash.clone();

        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# Improved review skill\n\nAlways cite line evidence."
                        .to_string(),
                    validation_score: 0.95,
                    operation: Some("replace".to_string()),
                    summary: Some("Require line evidence".to_string()),
                    gate_mode: Some(BenchmarkGateKind::Hard),
                    threshold: None,
                    eval_evidence: Some(passing_eval_evidence()),
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(result.accepted);
        assert_eq!(result.run.status, SkillOptimizationStatus::Accepted);
        assert_eq!(result.run.current_epoch, 1);
        assert_ne!(result.run.best_version_hash, initial_hash);
        assert_eq!(result.run.candidates.len(), 1);
        assert_eq!(result.run.skill_bundle.verification_score, 0.95);
        assert!(result
            .run
            .history
            .iter()
            .any(|entry| entry.event == "rollout" && entry.epoch == 1));
        assert!(result
            .run
            .history
            .iter()
            .any(|entry| entry.event == "evaluate" && entry.epoch == 1));
        assert_eq!(
            result.run.resume_checkpoint.unwrap()["acceptedCandidateCount"],
            1
        );
    }

    #[test]
    fn epoch_execution_rejects_missing_executed_bakeoff_evidence() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-missing-evidence".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            Vec::new(),
            "2026-06-09T00:00:00Z".to_string(),
        );

        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# Higher local score".to_string(),
                    validation_score: 0.95,
                    operation: Some("replace".to_string()),
                    summary: Some("No held-out receipt".to_string()),
                    gate_mode: None,
                    threshold: None,
                    eval_evidence: None,
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(!result.accepted);
        assert_eq!(result.run.status, SkillOptimizationStatus::Rejected);
        assert_eq!(
            result.candidate.rejection_reason.as_deref(),
            Some("executed bakeoff eval gate blocked promotion: executed bakeoff evidence is missing")
        );
    }

    #[test]
    fn epoch_execution_rejects_regression_and_buffers_edit() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            Vec::new(),
            "2026-06-09T00:00:00Z".to_string(),
        );
        let initial_body = run.skill_bundle.skill_body.clone();

        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# Worse review skill".to_string(),
                    validation_score: 0.4,
                    operation: Some("delete".to_string()),
                    summary: Some("Remove safeguards".to_string()),
                    gate_mode: None,
                    threshold: None,
                    eval_evidence: None,
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(!result.accepted);
        assert_eq!(result.run.status, SkillOptimizationStatus::Rejected);
        assert_eq!(result.run.skill_bundle.skill_body, initial_body);
        assert_eq!(result.run.rejected_edits.len(), 1);
        assert_eq!(result.run.rejected_edits[0].artifact_id, "run-1-epoch-1");
        assert!(result.run.candidates[0].rejection_reason.is_some());
        assert_eq!(result.run.resume_checkpoint.unwrap()["candidateCount"], 1);
    }

    fn passing_eval_evidence() -> EvalEvidence {
        EvalEvidence {
            train_pass_rate_pct: 95.0,
            heldout_pass_rate_pct: 95.0,
            safety_pass_rate_pct: 100.0,
            cost_ratio_to_baseline: 1.0,
            latency_ratio_to_baseline: 1.0,
            pass_rate_pct: 95.0,
            regression_count: 0,
            new_untested_capability_requested: false,
            total_runs: 10,
            expected_run_count_min: 10,
        }
    }

    #[test]
    fn epoch_execution_accepts_when_eval_gate_evidence_proceeds() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            Vec::new(),
            "2026-06-09T00:00:00Z".to_string(),
        );

        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# Improved review skill".to_string(),
                    validation_score: 0.95,
                    operation: Some("replace".to_string()),
                    summary: Some("Require line evidence".to_string()),
                    gate_mode: None,
                    threshold: None,
                    eval_evidence: Some(passing_eval_evidence()),
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(result.accepted);
        assert_eq!(result.run.status, SkillOptimizationStatus::Accepted);
        assert_eq!(
            result.candidate.eval_evidence,
            Some(passing_eval_evidence())
        );
    }

    #[test]
    fn epoch_execution_rejects_when_eval_gate_evidence_blocks_despite_strict_improvement() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            Vec::new(),
            "2026-06-09T00:00:00Z".to_string(),
        );
        // A regression in held-out evidence must block promotion even though
        // validation_score strictly improves the run-local baseline and no
        // run-local benchmark gates are configured — the shared EvalGate is
        // the deciding authority once evidence is supplied.
        let mut evidence = passing_eval_evidence();
        evidence.regression_count = 1;

        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# Regressed but higher score".to_string(),
                    validation_score: 0.95,
                    operation: Some("replace".to_string()),
                    summary: Some("Looks better, regresses".to_string()),
                    gate_mode: None,
                    threshold: None,
                    eval_evidence: Some(evidence),
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(!result.accepted);
        assert_eq!(result.run.status, SkillOptimizationStatus::Rejected);
        assert!(result
            .candidate
            .rejection_reason
            .as_deref()
            .unwrap()
            .contains("executed bakeoff eval gate blocked"));
    }

    #[test]
    fn epoch_execution_rejects_non_finite_bakeoff_score() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: None,
            },
            Vec::new(),
            vec![BenchmarkGate {
                name: "held-out".to_string(),
                kind: BenchmarkGateKind::Hard,
                threshold: 0.9,
            }],
            "2026-06-09T00:00:00Z".to_string(),
        );
        let result = store
            .execute_epoch(
                &run.id,
                SkillOptimizationEpochInput {
                    proposed_body: "# NaN bakeoff".to_string(),
                    validation_score: f64::NAN,
                    operation: Some("replace".to_string()),
                    summary: Some("non-finite".to_string()),
                    gate_mode: None,
                    threshold: None,
                    eval_evidence: None,
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();
        assert!(!result.accepted);
        assert!(result
            .candidate
            .rejection_reason
            .as_deref()
            .unwrap()
            .contains("not finite"));
        assert!(result.candidate.validation_score.is_finite());
        serde_json::to_value(&result.candidate).expect("rejected candidate remains serializable");
    }

    #[test]
    fn candidate_proposal_uses_meta_skill_and_rejected_edit_evidence() {
        let mut store = SkillOptimizationStore::new();
        let run = store.add_run(
            "run-1".to_string(),
            "Review Tightening".to_string(),
            SkillOptimizationTarget {
                skill_id: "review".to_string(),
                objective: "Catch unsafe shell suggestions".to_string(),
                optimizer: "skillopt-local".to_string(),
                target_model: Some("kimi-k2".to_string()),
            },
            vec![RejectedEdit {
                artifact_id: "candidate-0".to_string(),
                reason: "removed line evidence".to_string(),
                patch_preview: "Delete file/line requirement".to_string(),
            }],
            vec![BenchmarkGate {
                name: "held-out".to_string(),
                kind: BenchmarkGateKind::Hard,
                threshold: 0.9,
            }],
            "2026-06-09T00:00:00Z".to_string(),
        );

        let proposal = store
            .propose_candidate(
                &run.id,
                SkillOptimizationProposalInput {
                    operation: None,
                    summary: Some("Restore evidence safeguards".to_string()),
                    instructions: Some("Keep changes easy to benchmark.".to_string()),
                },
                "2026-06-09T00:01:00Z".to_string(),
            )
            .unwrap();

        assert!(proposal.proposed);
        assert_eq!(proposal.proposal.run_id, "run-1");
        assert_eq!(proposal.proposal.operation, "replace");
        assert_eq!(
            proposal.proposal.meta_skill_hash,
            run.meta_skill.content_hash
        );
        assert!(proposal
            .proposal
            .proposed_body
            .contains("Restore evidence safeguards"));
        assert!(proposal
            .proposal
            .proposed_body
            .contains("removed line evidence"));
        assert_eq!(proposal.run.pending_candidate_proposals.len(), 1);
        assert_eq!(
            proposal.run.history.last().unwrap().event,
            "candidateProposed"
        );
        assert_eq!(
            proposal.run.resume_checkpoint.unwrap()["pendingCandidateProposalCount"],
            1
        );
    }
}
