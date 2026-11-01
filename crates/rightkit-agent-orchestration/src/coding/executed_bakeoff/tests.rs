use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::*;
use crate::coding::failure::FailureKind;

const SECRET: &[u8] = b"private-corpus-secret-32-bytes!!";

fn task(case_nonce: &str, prompt: &str) -> EvaluationTask {
    EvaluationTask::new(
        TaskIdentity {
            suite_namespace: "private-router-suite-v1",
            private_corpus_secret: SECRET,
            case_nonce,
        },
        prompt,
        VerificationContract {
            required_artifacts: vec![ArtifactRequirement::exact_text(
                "result.txt",
                "correct",
                "incorrect-result",
            )],
        },
    )
    .unwrap()
}

fn record(
    body: &str,
    cost: f64,
    tool_ids: &[&str],
    review_ids: &[&str],
    escalations: u32,
) -> ExecutionRecord {
    ExecutionRecord {
        artifacts: vec![ExecutionArtifact {
            name: "result.txt".to_string(),
            body: body.to_string(),
            produced_by_tool_call_id: tool_ids.first().map(|id| (*id).to_string()),
        }],
        tool_calls: tool_ids
            .iter()
            .map(|id| ToolCallTrace {
                id: (*id).to_string(),
                tool_name: "write_file".to_string(),
            })
            .collect(),
        review_findings: review_ids
            .iter()
            .map(|id| ReviewFinding {
                defect_id: (*id).to_string(),
            })
            .collect(),
        observed_cost_usd: cost,
        escalation_count: escalations,
        observability: ExecutionObservability {
            useful_tool_attribution: true,
            review_findings: true,
            escalations: true,
        },
        execution_failure: None,
    }
}

fn fixture(record: ExecutionRecord, latency: u64) -> RecordedExecution {
    RecordedExecution {
        record,
        recorded_e2e_latency_ms: latency,
    }
}

#[test]
fn task_ids_are_stable_opaque_and_keyed() {
    let identity = TaskIdentity {
        suite_namespace: "private-suite",
        private_corpus_secret: SECRET,
        case_nonce: "private-case-name",
    };
    let first = identity.stable_id().unwrap();
    let second = identity.stable_id().unwrap();
    assert_eq!(first, second);
    assert!(first.as_str().starts_with("r1b_"));
    assert!(!first.as_str().contains("private-case-name"));

    let other_secret = TaskIdentity {
        private_corpus_secret: b"another-private-secret-32-bytes!",
        ..identity
    }
    .stable_id()
    .unwrap();
    assert_ne!(first, other_secret);
}

#[test]
fn weak_secret_is_rejected() {
    let error = TaskIdentity {
        suite_namespace: "suite",
        private_corpus_secret: b"short",
        case_nonce: "case",
    }
    .stable_id()
    .unwrap_err();
    assert!(matches!(error, BakeoffError::WeakCorpusSecret));
}

#[tokio::test]
async fn recorded_bakeoff_reports_every_required_metric_without_live_claims() {
    let mut standard = task("case-standard", "PRIVATE STANDARD PROMPT");
    standard.context_class = ContextClass::Standard;
    standard.context_cohort = Some("cohort-a".to_string());
    let mut long = task("case-long", "PRIVATE LONG PROMPT");
    long.context_class = ContextClass::Long;
    long.context_cohort = Some("cohort-a".to_string());
    long.context_tokens = 180_000;
    let tasks = vec![standard, long];

    let candidate = RecordedFixtureExecutor::new(
        "routed-candidate",
        ExecutionTopology::RoutedOrchestrated,
        [
            (
                tasks[0].id.clone(),
                fixture(record("correct", 0.02, &["tc-1"], &["ghost"], 0), 120),
            ),
            (
                tasks[1].id.clone(),
                fixture(
                    record("wrong", 0.02, &["tc-2", "tc-3"], &["incorrect-result"], 1),
                    480,
                ),
            ),
        ],
    );
    let baseline = RecordedFixtureExecutor::new(
        "same-cost-single-agent",
        ExecutionTopology::SingleAgent,
        tasks.iter().map(|task| {
            (
                task.id.clone(),
                fixture(record("correct", 0.02, &["base-tool"], &[], 0), 200),
            )
        }),
    );

    let report = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &tasks)
        .await
        .unwrap();

    let metrics = &report.candidate.metrics;
    assert_eq!(metrics.tasks, 2);
    assert_eq!(metrics.verified_completions, 1);
    assert_eq!(metrics.verified_completion_rate, 0.5);
    assert_eq!(metrics.failures_by_type.get("wrong_output"), Some(&1));
    assert!((metrics.total_cost_usd - 0.04).abs() < 1e-12);
    assert_eq!(metrics.average_e2e_latency_ms, 300.0);
    assert_eq!(metrics.escalation_rate, Some(0.5));
    assert_eq!(metrics.tool_call_efficiency, Some(1.0 / 3.0));
    assert_eq!(metrics.review_false_positive_rate, Some(0.5));
    assert_eq!(metrics.long_context.paired_cohorts, 1);
    assert_eq!(metrics.long_context.degradation_rate, Some(1.0));
    assert!(report.comparison.same_cost_comparable);
    assert_eq!(report.comparison.baseline_budget_violations, 0);
    assert!(!report.contains_live_execution_evidence);
    assert_eq!(
        report.same_cost_single_agent_baseline.executor.topology,
        ExecutionTopology::SingleAgent
    );
    assert_eq!(
        report.comparison_receipt.evaluator_id,
        "artifact_digest_verifier"
    );
    assert_eq!(report.comparison_receipt.evaluator_version, "1");
    assert_eq!(
        report.candidate.execution_receipt,
        report.comparison_receipt.candidate_execution_receipt
    );
    assert_eq!(
        report.same_cost_single_agent_baseline.execution_receipt,
        report.comparison_receipt.baseline_execution_receipt
    );
    assert_ne!(
        report.per_task[0].candidate.execution_receipt,
        report.per_task[0].baseline.execution_receipt
    );
    let outcomes = rightkit_logs::agent_telemetry::evaluation_outcomes();
    let matched: Vec<_> = outcomes
        .iter()
        .filter(|outcome| outcome.task_id.as_deref() == Some(report.per_task[0].task_id.as_str()))
        .collect();
    assert!(matched.len() >= 2);
    assert!(matched.iter().all(|outcome| {
        outcome.experiment_id.as_deref() == Some(report.comparison_receipt.experiment_id.as_str())
    }));
    let candidate_h6 = matched
        .iter()
        .find(|outcome| {
            outcome.execution_receipt.receipt_id.as_deref()
                == Some(report.per_task[0].candidate.execution_receipt.as_str())
        })
        .unwrap();
    let baseline_h6 = matched
        .iter()
        .find(|outcome| {
            outcome.execution_receipt.receipt_id.as_deref()
                == Some(report.per_task[0].baseline.execution_receipt.as_str())
        })
        .unwrap();
    assert_eq!(
        candidate_h6.baseline_ref.as_deref(),
        Some(report.per_task[0].baseline.execution_receipt.as_str())
    );
    assert_eq!(
        baseline_h6.baseline_ref.as_deref(),
        Some(report.per_task[0].baseline.execution_receipt.as_str())
    );
    assert!(candidate_h6
        .unavailable_fields
        .iter()
        .all(|field| field.field != "experiment_id" && field.field != "baseline_ref"));
    assert!(baseline_h6
        .unavailable_fields
        .iter()
        .all(|field| field.field != "experiment_id" && field.field != "baseline_ref"));
    assert!(candidate_h6
        .execution_receipt
        .evidence_ids
        .contains(&report.comparison_receipt.receipt_id));
    assert!(baseline_h6
        .execution_receipt
        .evidence_ids
        .contains(&report.comparison_receipt.receipt_id));

    let json = report.to_json_pretty().unwrap();
    assert!(json.contains("coderight.executed-bakeoff.v1"));
    assert!(json.contains("recorded_fixture"));
    assert!(!json.contains("PRIVATE STANDARD PROMPT"));
    assert!(!json.contains("PRIVATE LONG PROMPT"));
    assert!(!json.contains("correct\""));
    assert!(!json.contains("private-corpus-secret"));

    let offline_gate = evaluate_bakeoff(
        &report,
        &BakeoffGateConfig {
            evidence_policy: EvidencePolicy::LiveOrRecorded,
            require_same_cost_comparison: true,
            minimum_verified_completion_rate: Some(0.5),
            minimum_completion_rate_delta_vs_baseline: Some(-0.5),
            maximum_escalation_rate: Some(0.5),
            minimum_tool_call_efficiency: Some(0.3),
            maximum_review_false_positive_rate: Some(0.5),
            maximum_long_context_degradation_rate: Some(1.0),
        },
    );
    assert!(offline_gate.passed, "{:?}", offline_gate.failures);

    let release_gate = evaluate_bakeoff(
        &report,
        &BakeoffGateConfig {
            evidence_policy: EvidencePolicy::LiveOnly,
            require_same_cost_comparison: true,
            minimum_verified_completion_rate: None,
            minimum_completion_rate_delta_vs_baseline: None,
            maximum_escalation_rate: None,
            minimum_tool_call_efficiency: None,
            maximum_review_false_positive_rate: None,
            maximum_long_context_degradation_rate: None,
        },
    );
    assert!(!release_gate.passed);
    assert!(release_gate.failures[0].contains("live execution"));
}

#[tokio::test]
async fn comparison_receipt_changes_when_public_execution_evidence_changes() {
    let task = task("receipt-sensitivity", "private receipt prompt");
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(
            task.id.clone(),
            fixture(record("correct", 0.01, &[], &[], 0), 10),
        )],
    );
    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        [(
            task.id.clone(),
            fixture(record("correct", 0.01, &[], &[], 0), 10),
        )],
    );
    let first = run_executed_bakeoff(
        &candidate,
        &baseline,
        &ArtifactDigestVerifier,
        std::slice::from_ref(&task),
    )
    .await
    .unwrap();
    let changed_candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(
            task.id.clone(),
            fixture(record("correct", 0.02, &[], &[], 0), 10),
        )],
    );
    let second = run_executed_bakeoff(
        &changed_candidate,
        &baseline,
        &ArtifactDigestVerifier,
        &[task],
    )
    .await
    .unwrap();
    assert_ne!(
        first.comparison_receipt.receipt_id,
        second.comparison_receipt.receipt_id
    );
    assert_ne!(
        first.comparison_receipt.report_digest,
        second.comparison_receipt.report_digest
    );
}

#[tokio::test]
async fn execution_receipt_binds_all_public_execution_evidence_dimensions() {
    let task = task("receipt-public-fields", "private receipt prompt");
    let baseline_record = record("correct", 0.01, &[], &[], 0);
    let mut artifact_changed = baseline_record.clone();
    artifact_changed.artifacts[0].body = "different".to_string();
    let mut tool_changed = baseline_record.clone();
    tool_changed.tool_calls.push(ToolCallTrace {
        id: "tool-b".to_string(),
        tool_name: "read_file".to_string(),
    });
    let mut failure_changed = baseline_record.clone();
    failure_changed.execution_failure = Some(FailureKind::ProviderError);
    let mut review_changed = baseline_record.clone();
    review_changed.review_findings.push(ReviewFinding {
        defect_id: "review-change".to_string(),
    });
    let mut escalation_changed = baseline_record.clone();
    escalation_changed.escalation_count = 1;

    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        [(task.id.clone(), fixture(baseline_record.clone(), 10))],
    );
    let variants = [
        artifact_changed,
        tool_changed,
        failure_changed,
        review_changed,
        escalation_changed,
    ];
    let mut receipts = Vec::with_capacity(variants.len());
    for variant in variants {
        let candidate = RecordedFixtureExecutor::new(
            "candidate",
            ExecutionTopology::RoutedOrchestrated,
            [(task.id.clone(), fixture(variant, 10))],
        );
        let report = run_executed_bakeoff(
            &candidate,
            &baseline,
            &ArtifactDigestVerifier,
            std::slice::from_ref(&task),
        )
        .await
        .unwrap();
        receipts.push(report.per_task[0].candidate.execution_receipt.clone());
    }
    for (index, receipt) in receipts.iter().enumerate() {
        assert!(
            receipts[..index].iter().all(|previous| previous != receipt),
            "public evidence variant {index} collided with earlier receipt"
        );
    }
}

#[tokio::test]
async fn unobserved_optional_metrics_are_null_instead_of_fabricated_zero() {
    let task = task("unobserved", "prompt");
    let mut unobserved = record("correct", 0.01, &["tool-a"], &[], 0);
    unobserved.observability = ExecutionObservability::default();
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(task.id.clone(), fixture(unobserved.clone(), 10))],
    );
    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        [(task.id.clone(), fixture(unobserved, 10))],
    );

    let report = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &[task])
        .await
        .unwrap();

    assert_eq!(report.candidate.metrics.tool_call_efficiency, None);
    assert_eq!(report.candidate.metrics.review_false_positive_rate, None);
    assert_eq!(report.candidate.metrics.escalation_rate, None);
}

struct CapturingBaseline {
    descriptor: ExecutorDescriptor,
    requests: Mutex<Vec<ExecutionRequest>>,
    records: BTreeMap<StableTaskId, ExecutionRecord>,
}

#[async_trait]
impl BakeoffExecutor for CapturingBaseline {
    fn descriptor(&self) -> &ExecutorDescriptor {
        &self.descriptor
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionRecord, BakeoffError> {
        self.requests.lock().unwrap().push(request.clone());
        self.records
            .get(&request.task_id)
            .cloned()
            .ok_or_else(|| BakeoffError::MissingFixture(request.task_id.as_str().to_string()))
    }

    fn recorded_latency_ms(&self, task_id: &StableTaskId) -> Option<u64> {
        self.records.get(task_id).map(|_| 10)
    }
}

#[tokio::test]
async fn baseline_receives_candidate_observed_spend_as_per_task_limit() {
    let task = task("budget-case", "do the private task");
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(
            task.id.clone(),
            fixture(record("correct", 0.037, &[], &[], 0), 100),
        )],
    );
    let baseline = CapturingBaseline {
        descriptor: ExecutorDescriptor {
            name: "live-baseline".to_string(),
            topology: ExecutionTopology::SingleAgent,
            evidence_source: EvidenceSource::LiveExecution,
        },
        requests: Mutex::new(Vec::new()),
        records: [(task.id.clone(), record("correct", 0.038, &[], &[], 0))]
            .into_iter()
            .collect(),
    };

    let report = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &[task])
        .await
        .unwrap();
    let requests = baseline.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].cost_limit_usd, Some(0.037));
    assert!(!report.comparison.same_cost_comparable);
    assert_eq!(report.comparison.baseline_budget_violations, 1);
    assert!(report.contains_live_execution_evidence);
    let live_only = evaluate_bakeoff(
        &report,
        &BakeoffGateConfig {
            evidence_policy: EvidencePolicy::LiveOnly,
            require_same_cost_comparison: false,
            minimum_verified_completion_rate: None,
            minimum_completion_rate_delta_vs_baseline: None,
            maximum_escalation_rate: None,
            minimum_tool_call_efficiency: None,
            maximum_review_false_positive_rate: None,
            maximum_long_context_degradation_rate: None,
        },
    );
    assert!(
        !live_only.passed,
        "mixed recorded/live evidence is not a live bakeoff"
    );
}

#[tokio::test]
async fn receipt_identity_binds_topology_and_evidence_source_even_with_same_metrics() {
    let task = task("descriptor-receipt", "private descriptor prompt");
    let fixture = fixture(record("correct", 0.01, &[], &[], 0), 10);
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(task.id.clone(), fixture.clone())],
    );
    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        [(task.id.clone(), fixture.clone())],
    );
    let first = run_executed_bakeoff(
        &candidate,
        &baseline,
        &ArtifactDigestVerifier,
        std::slice::from_ref(&task),
    )
    .await
    .unwrap();
    let topology_changed = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::SingleAgent,
        [(task.id.clone(), fixture.clone())],
    );
    let second = run_executed_bakeoff(
        &topology_changed,
        &baseline,
        &ArtifactDigestVerifier,
        std::slice::from_ref(&task),
    )
    .await
    .unwrap();
    assert_eq!(
        first.candidate.metrics.verified_completion_rate,
        second.candidate.metrics.verified_completion_rate
    );
    assert_eq!(
        first.candidate.metrics.total_cost_usd,
        second.candidate.metrics.total_cost_usd
    );
    assert_ne!(
        first.comparison_receipt.experiment_id,
        second.comparison_receipt.experiment_id
    );

    let source_changed = CapturingBaseline {
        descriptor: ExecutorDescriptor {
            name: "baseline".to_string(),
            topology: ExecutionTopology::SingleAgent,
            evidence_source: EvidenceSource::LiveExecution,
        },
        requests: Mutex::new(Vec::new()),
        records: [(task.id.clone(), fixture.record.clone())]
            .into_iter()
            .collect(),
    };
    let third = run_executed_bakeoff(
        &candidate,
        &source_changed,
        &ArtifactDigestVerifier,
        &[task],
    )
    .await
    .unwrap();
    assert_eq!(
        first
            .same_cost_single_agent_baseline
            .metrics
            .verified_completion_rate,
        third
            .same_cost_single_agent_baseline
            .metrics
            .verified_completion_rate
    );
    assert_eq!(
        first.same_cost_single_agent_baseline.metrics.total_cost_usd,
        third.same_cost_single_agent_baseline.metrics.total_cost_usd
    );
    assert_ne!(
        first.comparison_receipt.experiment_id,
        third.comparison_receipt.experiment_id
    );
}

#[tokio::test]
async fn execution_failure_is_typed_even_when_artifacts_happen_to_match() {
    let task = task("failure-case", "private failure task");
    let mut failed = record("correct", 0.01, &[], &[], 0);
    failed.execution_failure = Some(FailureKind::ProviderError);
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        [(task.id.clone(), fixture(failed, 50))],
    );
    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        [(
            task.id.clone(),
            fixture(record("correct", 0.01, &[], &[], 0), 40),
        )],
    );

    let report = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &[task])
        .await
        .unwrap();
    assert!(!report.per_task[0].candidate.verified_completion);
    assert_eq!(
        report.per_task[0].candidate.failure_type,
        Some(FailureKind::ProviderError)
    );
}

#[tokio::test]
async fn non_single_agent_baseline_is_rejected() {
    let task = task("topology-case", "private task");
    let fixtures = [(
        task.id.clone(),
        fixture(record("correct", 0.01, &[], &[], 0), 10),
    )];
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        fixtures.clone(),
    );
    let invalid_baseline = RecordedFixtureExecutor::new(
        "not-a-baseline",
        ExecutionTopology::RoutedOrchestrated,
        fixtures,
    );
    let error = run_executed_bakeoff(
        &candidate,
        &invalid_baseline,
        &ArtifactDigestVerifier,
        &[task],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, BakeoffError::BaselineIsNotSingleAgent));
}

#[tokio::test]
async fn duplicate_task_ids_are_rejected_before_execution() {
    let task = task("duplicate-case", "private task");
    let fixtures = [(
        task.id.clone(),
        fixture(record("correct", 0.01, &[], &[], 0), 10),
    )];
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        fixtures.clone(),
    );
    let baseline =
        RecordedFixtureExecutor::new("baseline", ExecutionTopology::SingleAgent, fixtures);
    let error = run_executed_bakeoff(
        &candidate,
        &baseline,
        &ArtifactDigestVerifier,
        &[task.clone(), task],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, BakeoffError::DuplicateTaskId(_)));
}

#[tokio::test]
async fn empty_corpus_cannot_produce_a_passing_report() {
    let candidate = RecordedFixtureExecutor::new(
        "candidate",
        ExecutionTopology::RoutedOrchestrated,
        std::iter::empty::<(StableTaskId, RecordedExecution)>(),
    );
    let baseline = RecordedFixtureExecutor::new(
        "baseline",
        ExecutionTopology::SingleAgent,
        std::iter::empty::<(StableTaskId, RecordedExecution)>(),
    );
    let error = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &[])
        .await
        .unwrap_err();
    assert!(matches!(error, BakeoffError::EmptyCorpus));
}
