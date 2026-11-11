use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use async_trait::async_trait;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::{
    AggregateMetrics, BakeoffError, BakeoffReport, ComparisonMetrics, ComparisonReceipt,
    ContextClass, EvaluationTask, EvidenceSource, ExecutionRecord, ExecutionRequest,
    ExecutionTopology, ExecutorDescriptor, LongContextMetrics, RunScore, StableTaskId,
    StrategyReport, TaskComparison, VerificationContract, VerificationVerdict,
};
use crate::failure::FailureKind;

const SAME_COST_EPSILON_USD: f64 = 1e-9;

#[async_trait]
pub trait BakeoffExecutor: Send + Sync {
    fn descriptor(&self) -> &ExecutorDescriptor;

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionRecord, BakeoffError>;

    /// Recorded executors provide captured latency because replay wall time is
    /// not execution latency. Live executors use harness wall-clock time.
    fn recorded_latency_ms(&self, _task_id: &StableTaskId) -> Option<u64> {
        None
    }
}

#[async_trait]
pub trait Verifier: Send + Sync {
    /// Stable evaluator identity when the verifier supplies one.  The
    /// default keeps older/custom verifiers source-compatible while making
    /// the missing H6 identity explicit in emitted telemetry.
    fn evaluator_id(&self) -> Option<&str> {
        None
    }

    fn evaluator_version(&self) -> Option<&str> {
        None
    }

    async fn verify(
        &self,
        task_id: &StableTaskId,
        contract: &VerificationContract,
        execution: &ExecutionRecord,
    ) -> Result<VerificationVerdict, BakeoffError>;
}

/// Execute the candidate first, then give the single-agent baseline that
/// candidate's observed per-task spend as its hard comparison budget.
pub async fn run_executed_bakeoff(
    candidate: &dyn BakeoffExecutor,
    baseline: &dyn BakeoffExecutor,
    verifier: &dyn Verifier,
    tasks: &[EvaluationTask],
) -> Result<BakeoffReport, BakeoffError> {
    validate_inputs(baseline, tasks)?;

    let experiment_id = digest_fields(
        [
            "coderight.executed-bakeoff-experiment.v1",
            candidate.descriptor().name.as_str(),
            topology_tag(candidate.descriptor().topology),
            evidence_source_tag(candidate.descriptor().evidence_source),
            baseline.descriptor().name.as_str(),
            topology_tag(baseline.descriptor().topology),
            evidence_source_tag(baseline.descriptor().evidence_source),
            verifier.evaluator_id().unwrap_or("unknown"),
            verifier.evaluator_version().unwrap_or("unknown"),
        ]
        .into_iter()
        .chain(tasks.iter().map(|task| task.id.as_str())),
    );

    let mut per_task = Vec::with_capacity(tasks.len());
    let mut cohorts = HashMap::new();
    let mut pending_outcomes = Vec::with_capacity(tasks.len());

    for task in tasks {
        let candidate_request = ExecutionRequest {
            task_id: task.id.clone(),
            prompt: task.prompt.clone(),
            context_tokens: task.context_tokens,
            cost_limit_usd: task.candidate_cost_limit_usd,
        };
        let (candidate_score, candidate_outcome) = execute_and_score(
            candidate,
            verifier,
            task,
            &candidate_request,
            &experiment_id,
            "candidate",
        )
        .await?;

        let baseline_limit = candidate_score.observed_cost_usd;
        let baseline_request = ExecutionRequest {
            task_id: task.id.clone(),
            prompt: task.prompt.clone(),
            context_tokens: task.context_tokens,
            cost_limit_usd: Some(baseline_limit),
        };
        let (baseline_score, baseline_outcome) = execute_and_score(
            baseline,
            verifier,
            task,
            &baseline_request,
            &experiment_id,
            "baseline",
        )
        .await?;
        let within_limit =
            baseline_score.observed_cost_usd <= baseline_limit + SAME_COST_EPSILON_USD;
        pending_outcomes.push((
            candidate_outcome,
            baseline_outcome,
            baseline_score.execution_receipt.clone(),
        ));

        if let Some(cohort) = &task.context_cohort {
            cohorts.insert(task.id.clone(), (cohort.clone(), task.context_class));
        }
        per_task.push(TaskComparison {
            task_id: task.id.clone(),
            context_class: task.context_class,
            candidate: candidate_score,
            baseline: baseline_score,
            baseline_cost_limit_usd: baseline_limit,
            baseline_within_same_cost_limit: within_limit,
        });
    }

    let candidate_metrics = aggregate(&per_task, true, &cohorts);
    let baseline_metrics = aggregate(&per_task, false, &cohorts);
    let budget_violations = per_task
        .iter()
        .filter(|row| !row.baseline_within_same_cost_limit)
        .count();
    let contains_live = matches!(
        candidate.descriptor().evidence_source,
        EvidenceSource::LiveExecution
    ) || matches!(
        baseline.descriptor().evidence_source,
        EvidenceSource::LiveExecution
    );

    let candidate_execution_receipt = digest_fields(per_task.iter().flat_map(|row| {
        [
            row.task_id.as_str(),
            row.candidate.execution_receipt.as_str(),
        ]
    }));
    let baseline_execution_receipt = digest_fields(per_task.iter().flat_map(|row| {
        [
            row.task_id.as_str(),
            row.baseline.execution_receipt.as_str(),
        ]
    }));
    let mut report = BakeoffReport {
        schema_version: BakeoffReport::schema_version(),
        candidate: StrategyReport {
            executor: candidate.descriptor().clone(),
            metrics: candidate_metrics.clone(),
            execution_receipt: candidate_execution_receipt.clone(),
        },
        same_cost_single_agent_baseline: StrategyReport {
            executor: baseline.descriptor().clone(),
            metrics: baseline_metrics.clone(),
            execution_receipt: baseline_execution_receipt.clone(),
        },
        comparison: ComparisonMetrics {
            completion_rate_delta: candidate_metrics.verified_completion_rate
                - baseline_metrics.verified_completion_rate,
            total_cost_delta_usd: candidate_metrics.total_cost_usd
                - baseline_metrics.total_cost_usd,
            same_cost_comparable: budget_violations == 0,
            baseline_budget_violations: budget_violations,
        },
        per_task,
        contains_live_execution_evidence: contains_live,
        comparison_receipt: ComparisonReceipt {
            schema_version: "coderight.executed-bakeoff-receipt.v1".to_string(),
            experiment_id: experiment_id.clone(),
            candidate_execution_receipt,
            baseline_execution_receipt,
            evaluator_id: verifier.evaluator_id().unwrap_or("unknown").to_string(),
            evaluator_version: verifier
                .evaluator_version()
                .unwrap_or("unknown")
                .to_string(),
            report_digest: String::new(),
            receipt_id: String::new(),
        },
    };
    let report_digest = report_digest(&report);
    let receipt_id = digest_fields([
        "coderight.executed-bakeoff-comparison-receipt.v1",
        report.comparison_receipt.experiment_id.as_str(),
        report
            .comparison_receipt
            .candidate_execution_receipt
            .as_str(),
        report
            .comparison_receipt
            .baseline_execution_receipt
            .as_str(),
        report.comparison_receipt.evaluator_id.as_str(),
        report.comparison_receipt.evaluator_version.as_str(),
        report_digest.as_str(),
    ]);
    report.comparison_receipt.report_digest = report_digest;
    report.comparison_receipt.receipt_id = receipt_id;
    for (mut candidate_outcome, mut baseline_outcome, baseline_receipt) in pending_outcomes {
        let comparison_receipt_id = report.comparison_receipt.receipt_id.clone();
        candidate_outcome.bind_comparison_identity(
            experiment_id.clone(),
            Some(baseline_receipt.clone()),
            candidate_outcome
                .execution_receipt
                .receipt_id
                .clone()
                .unwrap_or_default(),
            comparison_receipt_id.clone(),
        );
        baseline_outcome.bind_comparison_identity(
            experiment_id.clone(),
            Some(baseline_receipt),
            baseline_outcome
                .execution_receipt
                .receipt_id
                .clone()
                .unwrap_or_default(),
            comparison_receipt_id,
        );
        rightkit_agent_telemetry::record_evaluation_outcome(candidate_outcome);
        rightkit_agent_telemetry::record_evaluation_outcome(baseline_outcome);
    }
    Ok(report)
}

#[derive(Serialize)]
struct ReportDigestMaterial<'a> {
    schema_version: &'a str,
    candidate: &'a StrategyReport,
    baseline: &'a StrategyReport,
    comparison: &'a ComparisonMetrics,
    per_task: &'a [TaskComparison],
    contains_live_execution_evidence: bool,
}

fn report_digest(report: &BakeoffReport) -> String {
    let material = ReportDigestMaterial {
        schema_version: &report.schema_version,
        candidate: &report.candidate,
        baseline: &report.same_cost_single_agent_baseline,
        comparison: &report.comparison,
        per_task: &report.per_task,
        contains_live_execution_evidence: report.contains_live_execution_evidence,
    };
    let bytes = serde_json::to_vec(&material).expect("report digest material is serializable");
    let serialized = String::from_utf8_lossy(&bytes);
    digest_fields(["coderight.executed-bakeoff-report.v1", serialized.as_ref()])
}

fn digest_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = rightkit_fs::digest::Sha256Hasher::new();
    for field in fields {
        digest.update(&(field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize_bytes() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn topology_tag(topology: ExecutionTopology) -> &'static str {
    match topology {
        ExecutionTopology::RoutedOrchestrated => "routed_orchestrated",
        ExecutionTopology::SingleAgent => "single_agent",
    }
}

fn evidence_source_tag(source: EvidenceSource) -> &'static str {
    match source {
        EvidenceSource::LiveExecution => "live_execution",
        EvidenceSource::RecordedFixture => "recorded_fixture",
    }
}

fn validate_inputs(
    baseline: &dyn BakeoffExecutor,
    tasks: &[EvaluationTask],
) -> Result<(), BakeoffError> {
    if baseline.descriptor().topology != ExecutionTopology::SingleAgent {
        return Err(BakeoffError::BaselineIsNotSingleAgent);
    }
    if tasks.is_empty() {
        return Err(BakeoffError::EmptyCorpus);
    }
    let mut ids = BTreeSet::new();
    for task in tasks {
        if !ids.insert(task.id.clone()) {
            return Err(BakeoffError::DuplicateTaskId(task.id.as_str().to_string()));
        }
    }
    Ok(())
}

async fn execute_and_score(
    executor: &dyn BakeoffExecutor,
    verifier: &dyn Verifier,
    task: &EvaluationTask,
    request: &ExecutionRequest,
    experiment_id: &str,
    role: &str,
) -> Result<
    (
        RunScore,
        rightkit_agent_telemetry::EvaluationOutcomeV1,
    ),
    BakeoffError,
> {
    let started = Instant::now();
    let record = executor
        .execute(request)
        .await
        .map_err(|error| BakeoffError::Executor {
            executor: executor.descriptor().name.clone(),
            task_id: task.id.as_str().to_string(),
            message: error.to_string(),
        })?;
    if !record.observed_cost_usd.is_finite() || record.observed_cost_usd < 0.0 {
        return Err(BakeoffError::InvalidObservedCost {
            executor: executor.descriptor().name.clone(),
            task_id: task.id.as_str().to_string(),
        });
    }
    let wall_latency = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let latency = match executor.descriptor().evidence_source {
        EvidenceSource::LiveExecution => wall_latency,
        EvidenceSource::RecordedFixture => {
            executor
                .recorded_latency_ms(&task.id)
                .ok_or_else(|| BakeoffError::Executor {
                    executor: executor.descriptor().name.clone(),
                    task_id: task.id.as_str().to_string(),
                    message: "recorded fixture did not provide captured latency".to_string(),
                })?
        }
    };

    let verdict = verifier
        .verify(&task.id, &task.verification, &record)
        .await
        .map_err(|error| BakeoffError::Verifier {
            task_id: task.id.as_str().to_string(),
            message: error.to_string(),
        })?;
    let verified_completion = record.execution_failure.is_none() && verdict.passed;
    let receipt_fields = public_execution_receipt_fields(
        experiment_id,
        role,
        task,
        executor,
        &record,
        &verdict,
        latency,
        verified_completion,
    );
    let execution_receipt = digest_fields(receipt_fields.iter().map(String::as_str));
    let failure_type = if verified_completion {
        None
    } else {
        record
            .execution_failure
            .clone()
            .or(verdict.failure_kind.clone())
            .or(Some(FailureKind::WrongOutput))
    };

    let useful_ids: BTreeSet<&str> = verdict
        .useful_tool_call_ids
        .iter()
        .map(String::as_str)
        .collect();
    let useful_tool_calls = record.observability.useful_tool_attribution.then(|| {
        record
            .tool_calls
            .iter()
            .filter(|call| useful_ids.contains(call.id.as_str()))
            .count()
    });
    let tool_calls = record.tool_calls.len();
    let confirmed_defects: BTreeSet<&str> = verdict
        .confirmed_defect_ids
        .iter()
        .map(String::as_str)
        .collect();
    let review_false_positives = record.observability.review_findings.then(|| {
        record
            .review_findings
            .iter()
            .filter(|finding| !confirmed_defects.contains(finding.defect_id.as_str()))
            .count()
    });
    let review_findings = record
        .observability
        .review_findings
        .then_some(record.review_findings.len());

    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);
    let outcome_id = format!(
        "{}:{}:{}",
        task.id.as_str(),
        executor.descriptor().name,
        timestamp_unix_ms
    );
    let mut outcome = rightkit_agent_telemetry::EvaluationOutcomeV1::verified_completion(
        outcome_id,
        verifier.evaluator_id().map(str::to_string),
        verifier.evaluator_version().map(str::to_string),
        task.id.as_str().to_string(),
        task.id.as_str().to_string(),
        verified_completion,
        record.observed_cost_usd,
        latency,
        tool_calls as u64,
        match executor.descriptor().evidence_source {
            EvidenceSource::LiveExecution => "live_execution",
            EvidenceSource::RecordedFixture => "recorded_fixture",
        },
        rightkit_agent_telemetry::ProvenanceV1::from_evidence(
            "executed_bakeoff",
            vec![
                task.id.as_str().to_string(),
                executor.descriptor().name.clone(),
            ],
        ),
        timestamp_unix_ms,
    );
    let score = RunScore {
        verified_completion,
        failure_type,
        observed_cost_usd: record.observed_cost_usd,
        e2e_latency_ms: latency,
        escalation_count: record
            .observability
            .escalations
            .then_some(record.escalation_count),
        tool_calls,
        useful_tool_calls: useful_tool_calls.unwrap_or(0),
        tool_call_efficiency: useful_tool_calls.and_then(|useful| ratio(useful, tool_calls)),
        review_findings,
        review_false_positives,
        review_false_positive_rate: review_false_positives
            .zip(review_findings)
            .and_then(|(false_positives, findings)| ratio(false_positives, findings)),
        evidence_source: executor.descriptor().evidence_source,
        execution_receipt,
    };
    outcome.execution_receipt.receipt_id = Some(score.execution_receipt.clone());
    outcome.execution_receipt.receipt_unavailable_reason = None;
    Ok((score, outcome))
}

fn public_execution_receipt_fields(
    experiment_id: &str,
    role: &str,
    task: &EvaluationTask,
    executor: &dyn BakeoffExecutor,
    record: &ExecutionRecord,
    verdict: &VerificationVerdict,
    latency: u64,
    verified_completion: bool,
) -> Vec<String> {
    let descriptor = executor.descriptor();
    let mut fields: Vec<String> = vec![
        "coderight.executed-bakeoff-execution-receipt.v1",
        experiment_id,
        role,
        task.id.as_str(),
        &descriptor.name,
        topology_tag(descriptor.topology),
        evidence_source_tag(descriptor.evidence_source),
        &record.observed_cost_usd.to_bits().to_string(),
        &latency.to_string(),
        &verified_completion.to_string(),
        record
            .execution_failure
            .as_ref()
            .map(failure_name)
            .unwrap_or("none"),
        verdict
            .failure_kind
            .as_ref()
            .map(failure_name)
            .unwrap_or("none"),
        &record.observability.useful_tool_attribution.to_string(),
        &record.observability.review_findings.to_string(),
        &record.observability.escalations.to_string(),
        &record.escalation_count.to_string(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    fields.push(format!("artifact_count:{}", record.artifacts.len()));
    for artifact in &record.artifacts {
        fields.extend([
            "artifact".to_string(),
            artifact.name.clone(),
            sha256_hex(artifact.body.as_bytes()),
            artifact
                .produced_by_tool_call_id
                .clone()
                .unwrap_or_default(),
        ]);
    }
    fields.push(format!("tool_call_count:{}", record.tool_calls.len()));
    for call in &record.tool_calls {
        fields.extend([
            "tool_call".to_string(),
            call.id.clone(),
            call.tool_name.clone(),
        ]);
    }
    fields.push(format!(
        "review_finding_count:{}",
        record.review_findings.len()
    ));
    for finding in &record.review_findings {
        fields.extend(["review_finding".to_string(), finding.defect_id.clone()]);
    }
    fields.extend([
        "confirmed_defect_count".to_string(),
        verdict.confirmed_defect_ids.len().to_string(),
    ]);
    fields.extend(
        verdict
            .confirmed_defect_ids
            .iter()
            .flat_map(|id| ["confirmed_defect".to_string(), id.clone()]),
    );
    fields.extend([
        "useful_tool_id_count".to_string(),
        verdict.useful_tool_call_ids.len().to_string(),
    ]);
    fields.extend(
        verdict
            .useful_tool_call_ids
            .iter()
            .flat_map(|id| ["useful_tool_call".to_string(), id.clone()]),
    );
    fields
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in rightkit_crypto::sha256(bytes) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn aggregate(
    rows: &[TaskComparison],
    candidate: bool,
    cohorts: &HashMap<StableTaskId, (String, ContextClass)>,
) -> AggregateMetrics {
    let scores: Vec<&RunScore> = rows
        .iter()
        .map(|row| {
            if candidate {
                &row.candidate
            } else {
                &row.baseline
            }
        })
        .collect();
    let completed = scores
        .iter()
        .filter(|score| score.verified_completion)
        .count();
    let mut failures = BTreeMap::new();
    for score in &scores {
        if let Some(kind) = &score.failure_type {
            *failures.entry(failure_name(kind).to_string()).or_insert(0) += 1;
        }
    }
    let total_tool_calls: usize = scores.iter().map(|score| score.tool_calls).sum();
    let useful_tool_calls: usize = scores.iter().map(|score| score.useful_tool_calls).sum();
    let tool_attribution_available = scores
        .iter()
        .filter(|score| score.tool_calls > 0)
        .all(|score| score.tool_call_efficiency.is_some());
    let total_reviews = scores
        .iter()
        .map(|score| score.review_findings)
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum::<usize>());
    let false_reviews = scores
        .iter()
        .map(|score| score.review_false_positives)
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum::<usize>());
    let observed_escalations = scores
        .iter()
        .map(|score| score.escalation_count)
        .collect::<Option<Vec<_>>>();

    AggregateMetrics {
        tasks: scores.len(),
        verified_completions: completed,
        verified_completion_rate: ratio_or_zero(completed, scores.len()),
        failures_by_type: failures,
        total_cost_usd: scores.iter().map(|score| score.observed_cost_usd).sum(),
        average_e2e_latency_ms: if scores.is_empty() {
            0.0
        } else {
            scores
                .iter()
                .map(|score| score.e2e_latency_ms as f64)
                .sum::<f64>()
                / scores.len() as f64
        },
        escalation_rate: observed_escalations.as_ref().map(|values| {
            ratio_or_zero(
                values.iter().filter(|count| **count > 0).count(),
                scores.len(),
            )
        }),
        tool_call_efficiency: tool_attribution_available
            .then(|| ratio(useful_tool_calls, total_tool_calls))
            .flatten(),
        review_false_positive_rate: false_reviews
            .zip(total_reviews)
            .and_then(|(false_reviews, total_reviews)| ratio(false_reviews, total_reviews)),
        long_context: long_context_metrics(rows, candidate, cohorts),
    }
}

fn long_context_metrics(
    rows: &[TaskComparison],
    candidate: bool,
    cohorts: &HashMap<StableTaskId, (String, ContextClass)>,
) -> LongContextMetrics {
    let mut paired: BTreeMap<&str, [Option<bool>; 2]> = BTreeMap::new();
    for row in rows {
        let Some((cohort, class)) = cohorts.get(&row.task_id) else {
            continue;
        };
        let score = if candidate {
            &row.candidate
        } else {
            &row.baseline
        };
        let pair = paired.entry(cohort).or_insert([None, None]);
        match class {
            ContextClass::Standard => pair[0] = Some(score.verified_completion),
            ContextClass::Long => pair[1] = Some(score.verified_completion),
        }
    }

    let complete_pairs: Vec<[bool; 2]> = paired
        .values()
        .filter_map(|pair| Some([pair[0]?, pair[1]?]))
        .collect();
    if complete_pairs.is_empty() {
        return LongContextMetrics::default();
    }
    let standard_rate = ratio_or_zero(
        complete_pairs.iter().filter(|pair| pair[0]).count(),
        complete_pairs.len(),
    );
    let long_rate = ratio_or_zero(
        complete_pairs.iter().filter(|pair| pair[1]).count(),
        complete_pairs.len(),
    );
    LongContextMetrics {
        paired_cohorts: complete_pairs.len(),
        standard_verified_completion_rate: Some(standard_rate),
        long_verified_completion_rate: Some(long_rate),
        degradation_rate: Some(standard_rate - long_rate),
    }
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn ratio_or_zero(numerator: usize, denominator: usize) -> f64 {
    ratio(numerator, denominator).unwrap_or(0.0)
}

fn failure_name(kind: &FailureKind) -> &'static str {
    match kind {
        FailureKind::Timeout => "timeout",
        FailureKind::MopLoop => "mop_loop",
        FailureKind::WrongOutput => "wrong_output",
        FailureKind::ToolError => "tool_error",
        FailureKind::ContextExhausted => "context_exhausted",
        FailureKind::MaxRetriesExhausted => "max_retries_exhausted",
        FailureKind::Aborted => "aborted",
        FailureKind::DestructiveAction => "destructive_action",
        FailureKind::ScopeViolation => "scope_violation",
        FailureKind::ContentPolicy => "content_policy",
        FailureKind::ProviderError => "provider_error",
        FailureKind::WorkerPanic => "worker_panic",
        FailureKind::Unknown => "unknown",
    }
}
