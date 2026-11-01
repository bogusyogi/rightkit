//! ADP-076 bridge: turn an executed the host app bakeoff into Membrane's
//! externally-evaluated comparison request.  This module never executes,
//! admits, or promotes a candidate; it only projects verified host evidence.

use super::types::BakeoffReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneComparisonBindings {
    pub comparison_id: String,
    pub target: String,
    pub target_version: u64,
    pub scope: String,
    pub allowed_change_sha256: String,
    pub development_dataset_sha256: String,
    pub test_dataset_sha256: String,
    pub host_configuration_sha256: String,
    pub evaluator_sha256: String,
    pub limits: MembraneComparisonLimits,
    pub usage: MembraneComparisonUsage,
    /// Case strata/digests are private corpus metadata and therefore must be
    /// supplied by the host; they cannot be inferred from aggregate scores.
    pub cases: Vec<MembraneCaseBinding>,
    /// Development-selected candidate identity for the held-out projection.
    /// The host supplies this after its development decision; the adapter
    /// never guesses a winner.
    pub frozen_candidate_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneCaseBinding {
    pub task_id: String,
    pub case_sha256: String,
    pub stratum: String,
    pub frozen_test: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneComparisonLimits {
    pub candidates: usize,
    pub cases: usize,
    pub evaluator_calls: u64,
    pub proposal_iterations: u64,
    pub cost_microunits: u64,
    pub elapsed_ms: u64,
    pub concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneComparisonUsage {
    pub evaluator_calls: u64,
    pub proposal_iterations: u64,
    pub cost_microunits: u64,
    pub elapsed_ms: u64,
    pub concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneCandidateCase {
    pub candidate_sha256: String,
    pub case_id: String,
    pub case_sha256: String,
    pub stratum: String,
    pub receipt_id: String,
    pub correct: bool,
    pub adherent: bool,
    pub recurred: bool,
    pub false_block: bool,
    pub authority_violation: bool,
    pub latency_ms: u64,
    pub cost_microunits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembraneCandidateComparisonV1 {
    pub schema_version: u32,
    pub comparison_id: String,
    pub target: String,
    pub target_version: u64,
    pub scope: String,
    pub allowed_change_sha256: String,
    pub baseline_sha256: String,
    pub candidates: Vec<String>,
    pub development_dataset_sha256: String,
    pub test_dataset_sha256: String,
    pub evaluator_sha256: String,
    pub host_configuration_sha256: String,
    pub limits: MembraneComparisonLimits,
    pub usage: MembraneComparisonUsage,
    pub cancelled: bool,
    pub development: Vec<MembraneCandidateCase>,
    pub frozen_test: Vec<MembraneCandidateCase>,
}

#[derive(Debug, thiserror::Error)]
pub enum MembraneAdapterError {
    #[error("bakeoff has no per-task execution evidence")]
    EmptyReport,
    #[error("missing private case binding for task {0}")]
    MissingCase(String),
    #[error("duplicate private case binding for task {0}")]
    DuplicateCase(String),
    #[error("bakeoff report contains no live execution evidence")]
    NoLiveEvidence,
    #[error("frozen-test bindings require the development-selected candidate identity")]
    MissingFrozenCandidate,
}

/// Project a real executed bakeoff into the canonical Membrane request shape.
/// `live_only` is useful for release qualification: recorded fixtures are
/// rejected before they can be mistaken for host-run evidence.
pub fn to_membrane_comparison(
    report: &BakeoffReport,
    bindings: MembraneComparisonBindings,
    live_only: bool,
) -> Result<MembraneCandidateComparisonV1, MembraneAdapterError> {
    if report.per_task.is_empty() {
        return Err(MembraneAdapterError::EmptyReport);
    }
    if live_only && !report.contains_live_execution_evidence {
        return Err(MembraneAdapterError::NoLiveEvidence);
    }
    let mut by_task = std::collections::BTreeMap::new();
    for binding in bindings.cases {
        let task_id = binding.task_id.clone();
        if by_task.insert(task_id.clone(), binding).is_some() {
            return Err(MembraneAdapterError::DuplicateCase(task_id));
        }
    }
    let baseline = identity_digest(&report.same_cost_single_agent_baseline.executor);
    let candidate = identity_digest(&report.candidate.executor);
    let mut development = Vec::with_capacity(report.per_task.len() * 2);
    let mut frozen_test = Vec::with_capacity(report.per_task.len() * 2);
    for row in &report.per_task {
        let task_id = row.task_id.as_str().to_string();
        let binding = by_task
            .remove(&task_id)
            .ok_or_else(|| MembraneAdapterError::MissingCase(task_id.clone()))?;
        let target = if binding.frozen_test {
            &mut frozen_test
        } else {
            &mut development
        };
        target.push(score(&baseline, &binding, &task_id, &row.baseline));
        if !binding.frozen_test
            || bindings.frozen_candidate_sha256.as_deref() == Some(candidate.as_str())
        {
            target.push(score(&candidate, &binding, &task_id, &row.candidate));
        } else if bindings.frozen_candidate_sha256.is_none() {
            return Err(MembraneAdapterError::MissingFrozenCandidate);
        }
    }
    Ok(MembraneCandidateComparisonV1 {
        schema_version: 1,
        comparison_id: bindings.comparison_id,
        target: bindings.target,
        target_version: bindings.target_version,
        scope: bindings.scope,
        allowed_change_sha256: bindings.allowed_change_sha256,
        baseline_sha256: baseline,
        candidates: vec![candidate],
        development_dataset_sha256: bindings.development_dataset_sha256,
        test_dataset_sha256: bindings.test_dataset_sha256,
        evaluator_sha256: bindings.evaluator_sha256,
        host_configuration_sha256: bindings.host_configuration_sha256,
        limits: bindings.limits,
        usage: bindings.usage,
        cancelled: false,
        development,
        frozen_test,
    })
}

fn score(
    candidate_sha256: &str,
    binding: &MembraneCaseBinding,
    task_id: &str,
    score: &super::types::RunScore,
) -> MembraneCandidateCase {
    MembraneCandidateCase {
        candidate_sha256: candidate_sha256.into(),
        case_id: task_id.into(),
        case_sha256: binding.case_sha256.clone(),
        stratum: binding.stratum.clone(),
        receipt_id: score.execution_receipt.clone(),
        correct: score.verified_completion,
        adherent: score.verified_completion,
        recurred: false,
        false_block: false,
        authority_violation: false,
        latency_ms: score.e2e_latency_ms,
        cost_microunits: (score.observed_cost_usd.max(0.0) * 1_000_000.0) as u64,
    }
}

fn identity_digest(executor: &super::types::ExecutorDescriptor) -> String {
    let raw = serde_json::to_vec(executor).expect("executor descriptor serializes");
    rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&raw))
}
