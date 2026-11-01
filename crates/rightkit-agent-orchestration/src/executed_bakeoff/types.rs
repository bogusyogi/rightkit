use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::failure::FailureKind;

const REPORT_SCHEMA_VERSION: &str = "coderight.executed-bakeoff.v1";
const TASK_ID_DOMAIN: &[u8] = b"rightkit-agent/r1b/task-id/v1";

/// Opaque, non-sequential task identifier safe to include in reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StableTaskId(String);

impl StableTaskId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Private inputs used to derive a stable task ID. The secret stays in the
/// corpus loader and is never stored in a task or report.
#[derive(Debug, Clone)]
pub struct TaskIdentity<'a> {
    pub suite_namespace: &'a str,
    pub private_corpus_secret: &'a [u8],
    pub case_nonce: &'a str,
}

impl TaskIdentity<'_> {
    pub fn stable_id(&self) -> Result<StableTaskId, BakeoffError> {
        if self.private_corpus_secret.len() < 16 {
            return Err(BakeoffError::WeakCorpusSecret);
        }
        if self.suite_namespace.trim().is_empty() || self.case_nonce.trim().is_empty() {
            return Err(BakeoffError::InvalidTaskIdentity);
        }

        let mut message = Vec::new();
        message.extend_from_slice(TASK_ID_DOMAIN);
        append_framed(&mut message, self.suite_namespace.as_bytes());
        append_framed(&mut message, self.case_nonce.as_bytes());
        let digest = rightkit_crypto::hmac_sha256(self.private_corpus_secret, &message);
        Ok(StableTaskId(format!(
            "r1b_{}",
            rightkit_crypto::hex_encode(&digest[..16])
        )))
    }
}

fn append_framed(message: &mut Vec<u8>, bytes: &[u8]) {
    message.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    message.extend_from_slice(bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextClass {
    Standard,
    Long,
}

#[derive(Debug, Clone)]
pub struct EvaluationTask {
    pub id: StableTaskId,
    pub prompt: String,
    pub context_tokens: u64,
    pub context_class: ContextClass,
    /// Private pairing key for equivalent standard/long-context cases.
    pub context_cohort: Option<String>,
    pub candidate_cost_limit_usd: Option<f64>,
    pub verification: VerificationContract,
}

impl EvaluationTask {
    pub fn new(
        identity: TaskIdentity<'_>,
        prompt: impl Into<String>,
        verification: VerificationContract,
    ) -> Result<Self, BakeoffError> {
        Ok(Self {
            id: identity.stable_id()?,
            prompt: prompt.into(),
            context_tokens: 0,
            context_class: ContextClass::Standard,
            context_cohort: None,
            candidate_cost_limit_usd: None,
            verification,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRequirement {
    pub artifact_name: String,
    pub sha256_hex: String,
    /// Defect proven present when this requirement is not satisfied.
    pub defect_id: String,
}

impl ArtifactRequirement {
    pub fn exact_text(
        artifact_name: impl Into<String>,
        expected: &str,
        defect_id: impl Into<String>,
    ) -> Self {
        Self {
            artifact_name: artifact_name.into(),
            sha256_hex: rightkit_crypto::hex_encode(&rightkit_crypto::sha256(expected.as_bytes())),
            defect_id: defect_id.into(),
        }
    }
}

/// Acceptance data visible only to the verifier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerificationContract {
    pub required_artifacts: Vec<ArtifactRequirement>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRequest {
    pub task_id: StableTaskId,
    pub prompt: String,
    pub context_tokens: u64,
    /// Candidate limit from the corpus, or the candidate's observed spend for
    /// the same-cost single-agent baseline.
    pub cost_limit_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionArtifact {
    pub name: String,
    pub body: String,
    pub produced_by_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallTrace {
    pub id: String,
    pub tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub defect_id: String,
}

/// Raw executor output. There is intentionally no `success` field: only the
/// isolated verifier decides whether artifacts satisfy the task.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRecord {
    pub artifacts: Vec<ExecutionArtifact>,
    pub tool_calls: Vec<ToolCallTrace>,
    pub review_findings: Vec<ReviewFinding>,
    pub observed_cost_usd: f64,
    pub escalation_count: u32,
    /// Which optional quality signals were actually observed by the executor.
    /// Empty vectors and zero counters are not evidence that the rate was zero.
    pub observability: ExecutionObservability,
    pub execution_failure: Option<FailureKind>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionObservability {
    pub useful_tool_attribution: bool,
    pub review_findings: bool,
    pub escalations: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTopology {
    RoutedOrchestrated,
    SingleAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    LiveExecution,
    RecordedFixture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorDescriptor {
    pub name: String,
    pub topology: ExecutionTopology,
    pub evidence_source: EvidenceSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationVerdict {
    pub passed: bool,
    pub failure_kind: Option<FailureKind>,
    pub confirmed_defect_ids: Vec<String>,
    pub useful_tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunScore {
    pub verified_completion: bool,
    pub failure_type: Option<FailureKind>,
    pub observed_cost_usd: f64,
    pub e2e_latency_ms: u64,
    pub escalation_count: Option<u32>,
    pub tool_calls: usize,
    pub useful_tool_calls: usize,
    pub tool_call_efficiency: Option<f64>,
    pub review_findings: Option<usize>,
    pub review_false_positives: Option<usize>,
    pub review_false_positive_rate: Option<f64>,
    pub evidence_source: EvidenceSource,
    /// Digest of immutable public execution evidence for this task/role.
    pub execution_receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComparison {
    pub task_id: StableTaskId,
    pub context_class: ContextClass,
    pub candidate: RunScore,
    pub baseline: RunScore,
    pub baseline_cost_limit_usd: f64,
    pub baseline_within_same_cost_limit: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LongContextMetrics {
    pub paired_cohorts: usize,
    pub standard_verified_completion_rate: Option<f64>,
    pub long_verified_completion_rate: Option<f64>,
    /// Standard minus long completion rate. Positive means degradation.
    pub degradation_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub tasks: usize,
    pub verified_completions: usize,
    pub verified_completion_rate: f64,
    pub failures_by_type: BTreeMap<String, usize>,
    pub total_cost_usd: f64,
    pub average_e2e_latency_ms: f64,
    pub escalation_rate: Option<f64>,
    pub tool_call_efficiency: Option<f64>,
    pub review_false_positive_rate: Option<f64>,
    pub long_context: LongContextMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyReport {
    pub executor: ExecutorDescriptor,
    pub metrics: AggregateMetrics,
    pub execution_receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonMetrics {
    pub completion_rate_delta: f64,
    pub total_cost_delta_usd: f64,
    pub same_cost_comparable: bool,
    pub baseline_budget_violations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReceipt {
    pub schema_version: String,
    pub experiment_id: String,
    pub candidate_execution_receipt: String,
    pub baseline_execution_receipt: String,
    pub evaluator_id: String,
    pub evaluator_version: String,
    pub report_digest: String,
    pub receipt_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BakeoffReport {
    pub schema_version: String,
    pub candidate: StrategyReport,
    pub same_cost_single_agent_baseline: StrategyReport,
    pub comparison: ComparisonMetrics,
    pub per_task: Vec<TaskComparison>,
    pub contains_live_execution_evidence: bool,
    pub comparison_receipt: ComparisonReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePolicy {
    /// Suitable for deterministic CI and fixture regression runs.
    LiveOrRecorded,
    /// Suitable for release claims that require actual execution.
    LiveOnly,
}

/// Caller-selected promotion thresholds. No benchmark thresholds are baked
/// into the harness: release owners must choose and review them explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakeoffGateConfig {
    pub evidence_policy: EvidencePolicy,
    pub require_same_cost_comparison: bool,
    pub minimum_verified_completion_rate: Option<f64>,
    pub minimum_completion_rate_delta_vs_baseline: Option<f64>,
    pub maximum_escalation_rate: Option<f64>,
    pub minimum_tool_call_efficiency: Option<f64>,
    pub maximum_review_false_positive_rate: Option<f64>,
    pub maximum_long_context_degradation_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BakeoffGateDecision {
    pub passed: bool,
    pub failures: Vec<String>,
}

impl BakeoffReport {
    pub(crate) fn schema_version() -> String {
        REPORT_SCHEMA_VERSION.to_string()
    }

    pub fn to_json_pretty(&self) -> Result<String, BakeoffError> {
        serde_json::to_string_pretty(self).map_err(BakeoffError::SerializeReport)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BakeoffError {
    #[error("private corpus secret must contain at least 16 bytes")]
    WeakCorpusSecret,
    #[error("suite namespace and case nonce must be non-empty")]
    InvalidTaskIdentity,
    #[error("evaluation corpus contains duplicate task id {0}")]
    DuplicateTaskId(String),
    #[error("evaluation corpus must contain at least one executed task")]
    EmptyCorpus,
    #[error("same-cost baseline must declare single_agent topology")]
    BaselineIsNotSingleAgent,
    #[error("executor {executor} failed on {task_id}: {message}")]
    Executor {
        executor: String,
        task_id: String,
        message: String,
    },
    #[error("verifier failed on {task_id}: {message}")]
    Verifier { task_id: String, message: String },
    #[error("executor {executor} returned an invalid observed cost for {task_id}")]
    InvalidObservedCost { executor: String, task_id: String },
    #[error("recorded fixture missing for task {0}")]
    MissingFixture(String),
    #[error("could not serialize bakeoff report: {0}")]
    SerializeReport(serde_json::Error),
}
