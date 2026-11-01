//! Review panels for verdict-bearing juries and advisory councils.
//!
//! The split mirrors the local review skills:
//! - `/review` is an independent plan/architecture jury that can block or approve.
//! - `/review-self` is an internal plan preflight that refines work but does
//!   not replace the independent jury.
//!
//! These panels review plans and architecture artifacts. They may inspect code
//! as evidence when a plan references it, but they are not code-review panels.
//! Repo/code/product-state audits route to `auditor`; screenshot/product-flow
//! evidence routes to `visual-review`.
mod catalog;

use std::sync::Arc;

use crate::coding::conductor::{Worker, WorkerResult, WorkerTask};
use crate::coding::jury::{JurorVerdict, JuryDecision, JurySystem};

const DELIBERATION_BALLOT_TEMPLATE: &str =
    include_str!("../../data/coding/prompts/jury-deliberation.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Jury,
    Council,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAuthority {
    Verdict,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelMember {
    pub id: String,
    pub agent_role: Option<String>,
    pub model_id: Option<String>,
    pub mandate: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanel {
    pub id: String,
    pub kind: PanelKind,
    pub authority: PanelAuthority,
    pub quorum: usize,
    pub purpose: String,
    pub members: Vec<PanelMember>,
}

impl ReviewPanel {
    pub fn member_ids(&self) -> Vec<String> {
        self.members.iter().map(|m| m.id.clone()).collect()
    }

    pub fn model_ids(&self) -> Vec<String> {
        self.members
            .iter()
            .filter_map(|m| m.model_id.clone())
            .collect()
    }

    pub fn can_decide(&self) -> bool {
        self.authority == PanelAuthority::Verdict
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelMemberResult {
    pub member_id: String,
    pub agent_role: Option<String>,
    pub approve: Option<bool>,
    pub confidence: f64,
    pub reasoning: String,
    pub output: String,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelRunResult {
    pub panel: ReviewPanel,
    pub member_results: Vec<PanelMemberResult>,
    pub decision: Option<JuryDecision>,
    pub ballot_rounds: usize,
    pub output: String,
    pub success: bool,
}

impl PanelRunResult {
    pub fn approval_count(&self) -> usize {
        self.member_results
            .iter()
            .filter(|r| r.approve == Some(true))
            .count()
    }

    pub fn rejection_count(&self) -> usize {
        self.member_results
            .iter()
            .filter(|r| r.approve == Some(false))
            .count()
    }
}

pub use catalog::{council_for_category, jury_for_category};
pub async fn execute_panel(
    panel: &ReviewPanel,
    task_id: &str,
    artifact: &str,
    worker: Arc<dyn Worker>,
) -> PanelRunResult {
    let first_ballot_results = run_panel_ballot(panel, task_id, worker.clone(), |member| {
        panel_member_prompt(panel, member, artifact)
    })
    .await;
    let mut member_results = first_ballot_results.clone();
    let mut ballot_rounds = usize::from(panel.can_decide());

    let decision = if panel.can_decide() {
        let jury = JurySystem::new(panel.quorum);
        let first_verdicts = verdicts_from_results(&first_ballot_results);
        if let Some(brief) = jury.deliberation_brief(&first_verdicts) {
            let second_task_id = format!("{task_id}:deliberation");
            let second_ballot_results =
                run_panel_ballot(panel, &second_task_id, worker.clone(), |member| {
                    panel_deliberation_prompt(panel, member, artifact, &brief)
                })
                .await;
            let second_verdicts = verdicts_from_results(&second_ballot_results);
            member_results = second_ballot_results;
            ballot_rounds = 2;
            Some(jury.decide(&second_verdicts))
        } else {
            Some(jury.decide(&first_verdicts))
        }
    } else {
        None
    };

    let success = match &decision {
        Some(JuryDecision::Approved { .. }) => true,
        Some(JuryDecision::Rejected { .. } | JuryDecision::NoConsensus) => false,
        None => member_results.iter().all(|r| r.success),
    };

    let output = summarize_panel(panel, &member_results, &decision, ballot_rounds);

    PanelRunResult {
        panel: panel.clone(),
        member_results,
        decision,
        ballot_rounds,
        output,
        success,
    }
}

async fn run_panel_ballot<F>(
    panel: &ReviewPanel,
    task_id: &str,
    worker: Arc<dyn Worker>,
    payload_for: F,
) -> Vec<PanelMemberResult>
where
    F: Fn(&PanelMember) -> String,
{
    let mut handles = Vec::new();
    for member in panel.members.iter().filter(|m| m.agent_role.is_some()) {
        let member = member.clone();
        let worker = worker.clone();
        let task = WorkerTask {
            task_id: format!("{task_id}:{}", member.id),
            agent: member.agent_role.clone().unwrap(),
            payload: payload_for(&member),
            agent_version: None,
            conductor_stable_cache_key: None,
        };
        handles.push(tokio::spawn(async move {
            let result = worker.run(&task).await;
            panel_result_from_worker(&member, result)
        }));
    }
    drop(payload_for);

    let mut member_results = Vec::new();
    for handle in handles {
        if let Ok(result) = handle.await {
            member_results.push(result);
        }
    }
    member_results
}

fn verdicts_from_results(results: &[PanelMemberResult]) -> Vec<JurorVerdict> {
    results
        .iter()
        .filter_map(|r| {
            r.approve.map(|approve| {
                JurorVerdict::new(
                    r.member_id.clone(),
                    approve,
                    r.confidence,
                    r.reasoning.clone(),
                )
            })
        })
        .collect()
}

fn panel_member_prompt(panel: &ReviewPanel, member: &PanelMember, artifact: &str) -> String {
    let authority = match panel.authority {
        PanelAuthority::Verdict => {
            "You are a verdict-bearing juror. Begin with exactly 'VERDICT: APPROVE' or 'VERDICT: REJECT', then 'CONFIDENCE: <0.0-1.0>'."
        }
        PanelAuthority::Advisory => {
            "You are an advisory council lens. Do not approve shipment; list improvements and risks."
        }
    };
    format!(
        "Panel: {}\nPurpose: {}\nMember: {}\nMandate: {}\n{}\n\nArtifact to review:\n{}",
        panel.id, panel.purpose, member.id, member.mandate, authority, artifact
    )
}

fn panel_deliberation_prompt(
    panel: &ReviewPanel,
    member: &PanelMember,
    artifact: &str,
    deliberation_brief: &str,
) -> String {
    format!(
        "{}\n\nPanel: {}\nPurpose: {}\nMember: {}\nMandate: {}\n\n{}\nArtifact to review:\n{}",
        DELIBERATION_BALLOT_TEMPLATE.trim(),
        panel.id,
        panel.purpose,
        member.id,
        member.mandate,
        deliberation_brief,
        artifact
    )
}

fn panel_result_from_worker(member: &PanelMember, result: WorkerResult) -> PanelMemberResult {
    let (approve, confidence, reasoning) = parse_panel_verdict(&result.output);
    PanelMemberResult {
        member_id: member.id.clone(),
        agent_role: member.agent_role.clone(),
        approve,
        confidence,
        reasoning,
        output: result.output,
        success: result.success,
    }
}

fn parse_panel_verdict(text: &str) -> (Option<bool>, f64, String) {
    let lower = text.to_ascii_lowercase();
    let approve = if lower.contains("verdict: approve") || lower.contains("verdict: pass") {
        Some(true)
    } else if lower.contains("verdict: reject") || lower.contains("verdict: fail") {
        Some(false)
    } else {
        None
    };

    let confidence = text
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            let (_, rhs) = lower.split_once("confidence:")?;
            rhs.split_whitespace().next().and_then(|raw| {
                raw.trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
                    .parse::<f64>()
                    .ok()
            })
        })
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    let reasoning = panel_reasoning(text);

    (approve, confidence, reasoning)
}

fn panel_reasoning(text: &str) -> String {
    let reasoning = text
        .lines()
        .filter(|line| {
            let lower = line.trim_start().to_ascii_lowercase();
            !lower.starts_with("verdict:") && !lower.starts_with("confidence:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if reasoning.is_empty() {
        text.trim().to_string()
    } else {
        reasoning
    }
}

fn summarize_panel(
    panel: &ReviewPanel,
    member_results: &[PanelMemberResult],
    decision: &Option<JuryDecision>,
    ballot_rounds: usize,
) -> String {
    let mut out = format!(
        "Panel: {}\nAuthority: {:?}\nBallot rounds: {}\nDecision: {:?}\n",
        panel.id, panel.authority, ballot_rounds, decision
    );
    for result in member_results {
        out.push_str(&format!(
            "\n## {}\nAgent: {}\nApprove: {:?}\nConfidence: {:.2}\nSuccess: {}\n{}",
            result.member_id,
            result.agent_role.as_deref().unwrap_or("inline-council"),
            result.approve,
            result.confidence,
            result.success,
            result.output
        ));
    }
    out
}

#[cfg(test)]
#[cfg(test)]
#[path = "review_panel_tests.rs"]
mod review_panel_tests;
