use async_trait::async_trait;
use rightkit_agent_engine_protocol::TranscriptMessage;
use rightkit_agent_orchestration::coding::{
    evaluate_bakeoff, run_executed_bakeoff, to_membrane_comparison, ArtifactDigestVerifier,
    ArtifactRequirement, BakeoffError, BakeoffExecutor, BakeoffGateConfig, BakeoffGateDecision,
    BakeoffReport, Conductor, ConductorPlan, ContextClass, EvaluationTask, EvidencePolicy,
    EvidenceSource, ExecutionArtifact, ExecutionObservability, ExecutionRecord, ExecutionRequest,
    ExecutionTopology, ExecutorDescriptor, FailureKind, MembraneComparisonBindings, ReviewFinding,
    TaskIdentity, ToolCallTrace, VerificationContract, Worker, WorkerOutcome, WorkerResult,
    WorkerTask,
};
use rightkit_agent_tools::coding_tools::SubagentSpawner;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::api::{LiveSession, SessionRuntimeOptions};

const MAX_TASKS: usize = 32;
const MAX_PROMPT_BYTES_PER_TASK: usize = 64 * 1024;
const MAX_TOTAL_PROMPT_BYTES: usize = 512 * 1024;
const MAX_EXPECTED_OUTPUT_BYTES_PER_TASK: usize = 1024 * 1024;
const MAX_TOTAL_EXPECTED_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_EXECUTION_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_SYNTHESIS_DRAFT_BYTES: usize = 256 * 1024;
const MAX_CONTEXT_TOKENS: u64 = 10_000_000;
const MAX_TIMEOUT_SECONDS: u64 = 3_600;
const DEFAULT_TIMEOUT_SECONDS: u64 = 900;
const MAX_COST_LIMIT_USD: f64 = 100.0;
const MAX_VERIFIER_ARGS: usize = 32;
const MAX_VERIFIER_ARG_BYTES: usize = 4 * 1024;
const VERIFIER_TIMEOUT_SECONDS: u64 = 120;
const RESPONSE_ARTIFACT_NAME: &str = "response";
const RESPONSE_DEFECT_ID: &str = "response_mismatch";

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExecutedBakeoffBody {
    suite_namespace: String,
    workspace_root: String,
    candidate_roles: Vec<String>,
    baseline_role: String,
    tasks: Vec<ExecutedBakeoffTaskBody>,
    #[serde(default)]
    gate: ExecutedBakeoffGateBody,
    /// Optional exact host/corpus bindings for ADP-076 projection. No
    /// defaults are synthesized: absent bindings remain a typed refusal.
    #[serde(default)]
    membrane_comparison: Option<MembraneComparisonBindings>,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutedBakeoffTaskBody {
    case_nonce: String,
    prompt: String,
    #[serde(default)]
    expected_output: Option<String>,
    #[serde(default)]
    coding_verifier: Option<ExecutedCodingVerifierBody>,
    #[serde(default)]
    workspace_root: Option<String>,
    #[serde(default)]
    context_tokens: u64,
    #[serde(default = "default_context_class")]
    context_class: ContextClass,
    #[serde(default)]
    context_cohort: Option<String>,
    #[serde(default)]
    candidate_cost_limit_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutedCodingVerifierBody {
    command: Vec<String>,
    #[serde(default)]
    required_artifacts: Vec<ExecutedRequiredArtifactBody>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutedRequiredArtifactBody {
    name: String,
    path: String,
    expected_output: String,
}

#[derive(Debug, Clone)]
struct RuntimeCodingVerifier {
    command: Vec<String>,
    required_artifacts: Vec<(String, String)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutedBakeoffGateBody {
    #[serde(default)]
    minimum_verified_completion_rate: Option<f64>,
    #[serde(default)]
    minimum_completion_rate_delta_vs_baseline: Option<f64>,
    #[serde(default)]
    maximum_escalation_rate: Option<f64>,
    #[serde(default)]
    minimum_tool_call_efficiency: Option<f64>,
    #[serde(default)]
    maximum_review_false_positive_rate: Option<f64>,
    #[serde(default)]
    maximum_long_context_degradation_rate: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExecutedBakeoffResponse {
    pub(crate) report: BakeoffReport,
    pub(crate) decision: BakeoffGateDecision,
    pub(crate) membrane_comparison: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutedBakeoffRuntimeConfig {
    workspace_root: String,
    task_workspace_roots: BTreeMap<String, String>,
    coding_verifiers: BTreeMap<String, RuntimeCodingVerifier>,
    candidate_roles: Vec<String>,
    baseline_role: String,
}

impl ExecutedBakeoffRuntimeConfig {
    fn workspace_root_for(&self, task_id: &str) -> &str {
        self.task_workspace_roots
            .get(task_id)
            .map(String::as_str)
            .unwrap_or(&self.workspace_root)
    }

    fn coding_verifier_for(&self, task_id: &str) -> Option<&RuntimeCodingVerifier> {
        self.coding_verifiers.get(task_id)
    }
}

#[async_trait]
pub(crate) trait ExecutedBakeoffRuntime: Send + Sync {
    async fn execute(
        &self,
        state: &DaemonState,
        topology: ExecutionTopology,
        request: &ExecutionRequest,
        config: &ExecutedBakeoffRuntimeConfig,
    ) -> Result<ExecutionRecord, String>;
}

pub(crate) struct DaemonExecutedBakeoffRuntime;

#[derive(Clone)]
struct RuntimeBakeoffExecutor {
    state: DaemonState,
    runtime: Arc<dyn ExecutedBakeoffRuntime>,
    config: ExecutedBakeoffRuntimeConfig,
    descriptor: ExecutorDescriptor,
}

impl RuntimeBakeoffExecutor {
    fn new(
        state: DaemonState,
        runtime: Arc<dyn ExecutedBakeoffRuntime>,
        config: ExecutedBakeoffRuntimeConfig,
        name: &str,
        topology: ExecutionTopology,
    ) -> Self {
        Self {
            state,
            runtime,
            config,
            descriptor: ExecutorDescriptor {
                name: name.to_string(),
                topology,
                evidence_source: EvidenceSource::LiveExecution,
            },
        }
    }
}

#[async_trait]
impl BakeoffExecutor for RuntimeBakeoffExecutor {
    fn descriptor(&self) -> &ExecutorDescriptor {
        &self.descriptor
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionRecord, BakeoffError> {
        self.runtime
            .execute(&self.state, self.descriptor.topology, request, &self.config)
            .await
            .map_err(|message| BakeoffError::Executor {
                executor: self.descriptor.name.clone(),
                task_id: request.task_id.as_str().to_string(),
                message,
            })
    }
}

async fn run_executed_bakeoff_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<ExecutedBakeoffBody>,
) -> Result<Json<ExecutedBakeoffResponse>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if body.timeout_seconds == 0 || body.timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(StatusCode::BAD_REQUEST);
    }
    let timeout = body.timeout_seconds;
    let run = run_live_executed_bakeoff(state, body);
    match tokio::time::timeout(Duration::from_secs(timeout), run).await {
        Ok(Ok(response)) => Ok(Json(response)),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(StatusCode::GATEWAY_TIMEOUT),
    }
}

pub(crate) async fn run_live_executed_bakeoff(
    state: DaemonState,
    body: ExecutedBakeoffBody,
) -> Result<ExecutedBakeoffResponse, StatusCode> {
    validate_bakeoff_body(&state, &body)?;
    let private_secret = private_corpus_secret(&state.token, &body.suite_namespace);
    let default_limit = lock_recover(&state.settings, "settings").per_task_max_usd;
    let mut tasks = Vec::with_capacity(body.tasks.len());
    let mut task_workspace_roots = BTreeMap::new();
    let mut coding_verifiers = BTreeMap::new();
    for row in &body.tasks {
        let verification = match (&row.expected_output, &row.coding_verifier) {
            (Some(expected), None) => VerificationContract {
                required_artifacts: vec![ArtifactRequirement::exact_text(
                    RESPONSE_ARTIFACT_NAME,
                    expected,
                    RESPONSE_DEFECT_ID,
                )],
            },
            (None, Some(verifier)) => {
                let mut required_artifacts = vec![ArtifactRequirement::exact_text(
                    "verifier-command",
                    "success",
                    "verifier_command_failed",
                )];
                required_artifacts.extend(verifier.required_artifacts.iter().map(|artifact| {
                    ArtifactRequirement::exact_text(
                        &artifact.name,
                        &artifact.expected_output,
                        format!("artifact_mismatch:{}", artifact.name),
                    )
                }));
                VerificationContract { required_artifacts }
            }
            _ => return Err(StatusCode::BAD_REQUEST),
        };
        let mut task = EvaluationTask::new(
            TaskIdentity {
                suite_namespace: &body.suite_namespace,
                private_corpus_secret: &private_secret,
                case_nonce: &row.case_nonce,
            },
            row.prompt.clone(),
            verification,
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;
        let measured_context_tokens =
            rightkit_llm::agent_providers::client::capacity::measure_o200k_base(&row.prompt);
        if row.context_tokens > 0 && row.context_tokens != measured_context_tokens {
            return Err(StatusCode::BAD_REQUEST);
        }
        task.context_tokens = measured_context_tokens;
        task.context_class = row.context_class;
        task.context_cohort = row.context_cohort.clone();
        task.candidate_cost_limit_usd =
            effective_candidate_limit(row.candidate_cost_limit_usd, default_limit);
        if let Some(workspace_root) = row.workspace_root.as_deref() {
            let workspace_root = canonical_bakeoff_workspace(workspace_root)?;
            task_workspace_roots.insert(task.id.as_str().to_string(), workspace_root);
        }
        if let Some(verifier) = &row.coding_verifier {
            coding_verifiers.insert(
                task.id.as_str().to_string(),
                RuntimeCodingVerifier {
                    command: verifier.command.clone(),
                    required_artifacts: verifier
                        .required_artifacts
                        .iter()
                        .map(|artifact| (artifact.name.clone(), artifact.path.clone()))
                        .collect(),
                },
            );
        }
        tasks.push(task);
    }

    let runtime_config = ExecutedBakeoffRuntimeConfig {
        workspace_root: body.workspace_root,
        task_workspace_roots,
        coding_verifiers,
        candidate_roles: body.candidate_roles,
        baseline_role: body.baseline_role,
    };
    let runtime = state.executed_bakeoff_runtime.clone();
    let candidate = RuntimeBakeoffExecutor::new(
        state.clone(),
        runtime.clone(),
        runtime_config.clone(),
        "coderight-routed-orchestration",
        ExecutionTopology::RoutedOrchestrated,
    );
    let baseline = RuntimeBakeoffExecutor::new(
        state,
        runtime,
        runtime_config,
        "coderight-same-cost-single-agent",
        ExecutionTopology::SingleAgent,
    );
    let report = run_executed_bakeoff(&candidate, &baseline, &ArtifactDigestVerifier, &tasks)
        .await
        .map_err(map_bakeoff_error)?;
    let gate = BakeoffGateConfig {
        evidence_policy: EvidencePolicy::LiveOnly,
        require_same_cost_comparison: true,
        minimum_verified_completion_rate: body.gate.minimum_verified_completion_rate,
        minimum_completion_rate_delta_vs_baseline: body
            .gate
            .minimum_completion_rate_delta_vs_baseline,
        maximum_escalation_rate: body.gate.maximum_escalation_rate,
        minimum_tool_call_efficiency: body.gate.minimum_tool_call_efficiency,
        maximum_review_false_positive_rate: body.gate.maximum_review_false_positive_rate,
        maximum_long_context_degradation_rate: body.gate.maximum_long_context_degradation_rate,
    };
    let decision = evaluate_bakeoff(&report, &gate);
    let membrane_comparison = match body.membrane_comparison {
        Some(bindings) => match to_membrane_comparison(&report, bindings, true) {
            Ok(request) => serde_json::to_value(request).unwrap_or_else(|_| serde_json::json!({
                "status": "refused", "reason": "membrane comparison request serialization failed"
            })),
            Err(error) => serde_json::json!({
                "status": "refused", "reason": error.to_string()
            }),
        },
        None => serde_json::json!({
            "status": "unavailable",
            "reason": "ADP-076 host/corpus comparison bindings were not supplied"
        }),
    };
    Ok(ExecutedBakeoffResponse {
        report,
        decision,
        membrane_comparison,
    })
}

fn validate_bakeoff_body(
    state: &DaemonState,
    body: &ExecutedBakeoffBody,
) -> Result<(), StatusCode> {
    if body.suite_namespace.trim().is_empty() || body.suite_namespace.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let workspace = Path::new(body.workspace_root.trim());
    if body.workspace_root.trim().is_empty() || !workspace.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.timeout_seconds == 0 || body.timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.tasks.is_empty() || body.tasks.len() > MAX_TASKS {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.candidate_roles.len() < 2 || body.candidate_roles.len() > 4 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut roles = HashSet::new();
    for role in &body.candidate_roles {
        let role = role.trim();
        if role.is_empty()
            || role.len() > 64
            || !roles.insert(role.to_string())
            || state.agent_registry.get(role).is_none()
        {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let baseline_role = body.baseline_role.trim();
    if baseline_role.is_empty()
        || baseline_role.len() > 64
        || state.agent_registry.get(baseline_role).is_none()
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut nonces = HashSet::new();
    let mut total_prompt = 0usize;
    let mut total_expected = 0usize;
    for task in &body.tasks {
        if task.case_nonce.trim().is_empty()
            || task.case_nonce.len() > 128
            || !nonces.insert(task.case_nonce.trim())
            || task.prompt.trim().is_empty()
            || task.prompt.len() > MAX_PROMPT_BYTES_PER_TASK
            || task
                .expected_output
                .as_ref()
                .is_some_and(|value| value.len() > MAX_EXPECTED_OUTPUT_BYTES_PER_TASK)
            || !valid_task_verifier(task)
            || task.context_tokens > MAX_CONTEXT_TOKENS
            || task
                .context_cohort
                .as_ref()
                .is_some_and(|value| value.len() > 128)
            || !valid_cost_limit(task.candidate_cost_limit_usd)
            || task
                .workspace_root
                .as_deref()
                .is_some_and(|path| canonical_bakeoff_workspace(path).is_err())
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        total_prompt = total_prompt.saturating_add(task.prompt.len());
        total_expected = total_expected.saturating_add(
            task.expected_output.as_ref().map_or(0, String::len)
                + task
                    .coding_verifier
                    .as_ref()
                    .map(|verifier| {
                        verifier
                            .required_artifacts
                            .iter()
                            .map(|artifact| artifact.expected_output.len())
                            .sum::<usize>()
                    })
                    .unwrap_or(0),
        );
    }
    if total_prompt > MAX_TOTAL_PROMPT_BYTES || total_expected > MAX_TOTAL_EXPECTED_OUTPUT_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    for threshold in [
        body.gate.minimum_verified_completion_rate,
        body.gate.maximum_escalation_rate,
        body.gate.minimum_tool_call_efficiency,
        body.gate.maximum_review_false_positive_rate,
        body.gate.maximum_long_context_degradation_rate,
    ] {
        if threshold.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    if body
        .gate
        .minimum_completion_rate_delta_vs_baseline
        .is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn valid_task_verifier(task: &ExecutedBakeoffTaskBody) -> bool {
    match (&task.expected_output, &task.coding_verifier) {
        (Some(_), None) => true,
        (None, Some(verifier)) => {
            !verifier.command.is_empty()
                && verifier.command.len() <= MAX_VERIFIER_ARGS
                && verifier
                    .command
                    .iter()
                    .all(|arg| !arg.is_empty() && arg.len() <= MAX_VERIFIER_ARG_BYTES)
                && verifier.required_artifacts.len() <= 16
                && verifier.required_artifacts.iter().all(|artifact| {
                    !artifact.name.trim().is_empty()
                        && artifact.name.len() <= 128
                        && valid_workspace_relative_path(&artifact.path)
                        && artifact.expected_output.len() <= MAX_EXPECTED_OUTPUT_BYTES_PER_TASK
                })
        }
        _ => false,
    }
}

fn valid_workspace_relative_path(raw: &str) -> bool {
    let path = Path::new(raw.trim());
    !raw.trim().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn canonical_bakeoff_workspace(raw: &str) -> Result<String, StatusCode> {
    let path = Path::new(raw.trim());
    if raw.trim().is_empty() || !path.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().to_string())
        .map_err(|_| StatusCode::BAD_REQUEST)
}

fn valid_cost_limit(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && value > 0.0 && value <= MAX_COST_LIMIT_USD)
}

fn effective_candidate_limit(requested: Option<f64>, operator_limit: f64) -> Option<f64> {
    let operator_limit =
        (operator_limit.is_finite() && operator_limit > 0.0).then_some(operator_limit);
    match (requested, operator_limit) {
        (Some(requested), Some(operator)) => Some(requested.min(operator)),
        (Some(requested), None) => Some(requested),
        (None, operator) => operator,
    }
}

fn private_corpus_secret(token: &str, suite_namespace: &str) -> [u8; 32] {
    let mut digest = rightkit_fs::digest::Sha256Hasher::new();
    digest.update(b"coderight/runtime-executed-bakeoff/corpus-secret/v1\0");
    digest.update(token.as_bytes());
    digest.update(&[0]);
    digest.update(suite_namespace.as_bytes());
    digest.finalize_bytes().into()
}

fn map_bakeoff_error(error: BakeoffError) -> StatusCode {
    match error {
        BakeoffError::WeakCorpusSecret
        | BakeoffError::InvalidTaskIdentity
        | BakeoffError::DuplicateTaskId(_)
        | BakeoffError::EmptyCorpus
        | BakeoffError::BaselineIsNotSingleAgent
        | BakeoffError::InvalidObservedCost { .. } => StatusCode::BAD_REQUEST,
        BakeoffError::Executor { .. } | BakeoffError::Verifier { .. } => StatusCode::BAD_GATEWAY,
        BakeoffError::MissingFixture(_) | BakeoffError::SerializeReport(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

fn default_context_class() -> ContextClass {
    ContextClass::Standard
}

#[derive(Clone)]
struct DaemonSpawnWorker {
    state: DaemonState,
    parent_session_id: String,
    spawn_options: rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
}

#[async_trait]
impl Worker for DaemonSpawnWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        match self
            .state
            .spawn(
                &task.agent,
                &task.payload,
                &self.parent_session_id,
                self.spawn_options.clone(),
            )
            .await
        {
            Ok(output) if output.len() <= MAX_EXECUTION_OUTPUT_BYTES => WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output,
                input_tokens: rightkit_llm::agent_providers::client::capacity::measure_o200k_base(
                    &task.payload,
                ) as u32,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            },
            Ok(_) => failed_worker_result(
                task,
                FailureKind::ContextExhausted,
                "executor output exceeded the bounded evaluation limit",
            ),
            Err(error) => {
                let kind = FailureKind::infer_from_output(&error, false, false);
                failed_worker_result(task, kind, "live agent execution failed")
            }
        }
    }
}

fn failed_worker_result(task: &WorkerTask, kind: FailureKind, message: &str) -> WorkerResult {
    WorkerResult {
        task_id: task.task_id.clone(),
        agent: task.agent.clone(),
        success: false,
        output: message.to_string(),
        input_tokens: 0,
        mop_fired: false,
        failure_kind: Some(kind.clone()),
        agent_version: task.agent_version.clone(),
        rollback_step: None,
        outcome: WorkerOutcome::ApiError {
            error: message.to_string(),
            partial_output: None,
        },
    }
}

#[async_trait]
impl ExecutedBakeoffRuntime for DaemonExecutedBakeoffRuntime {
    async fn execute(
        &self,
        state: &DaemonState,
        topology: ExecutionTopology,
        request: &ExecutionRequest,
        config: &ExecutedBakeoffRuntimeConfig,
    ) -> Result<ExecutionRecord, String> {
        if state.engine_cfg.is_none() {
            return Err("live provider execution is not configured".to_string());
        }
        match topology {
            ExecutionTopology::RoutedOrchestrated => {
                execute_routed_candidate(state, request, config).await
            }
            ExecutionTopology::SingleAgent => execute_single_agent(state, request, config).await,
        }
    }
}

async fn execute_routed_candidate(
    state: &DaemonState,
    request: &ExecutionRequest,
    config: &ExecutedBakeoffRuntimeConfig,
) -> Result<ExecutionRecord, String> {
    let coding_verifier = config
        .coding_verifier_for(request.task_id.as_str())
        .cloned();
    let isolated = BakeoffWorktree::create(
        config.workspace_root_for(request.task_id.as_str()),
        "candidate",
        request,
    )
    .await?;
    let parent_id = create_bakeoff_parent(
        state,
        isolated.path_str(),
        "candidate",
        coding_verifier.is_some(),
        request.cost_limit_usd,
    );
    let _session_guard = BakeoffSessionGuard::new(state.clone(), parent_id.clone());
    // Coding tasks exercise the actual product path: production conductor
    // planning, routing, DAG execution, recovery, and victory gate. The
    // response-only corpus retains the fixed synthesis scaffold so exact-text
    // comparisons remain meaningful and reproducible.
    if candidate_scaffold(coding_verifier.as_ref()) == CandidateScaffold::ProductionConductor {
        let verifier = coding_verifier
            .as_ref()
            .expect("production candidate scaffold requires coding verifier");
        let outcome = state
            .run_conductor(&parent_id, &request.prompt, false)
            .await?;
        let (cost, tool_calls) = collect_live_evidence(state, &parent_id).await?;
        if !outcome.completed {
            return Ok(failed_execution_record(
                cost,
                tool_calls,
                outcome
                    .results
                    .iter()
                    .find_map(|result| result.failure_kind.clone())
                    .unwrap_or(FailureKind::WrongOutput),
            ));
        }
        let response = outcome
            .results
            .last()
            .map(|result| result.output.clone())
            .unwrap_or_default();
        let artifacts = execution_artifacts(isolated.task_path(), response, Some(verifier)).await?;
        return Ok(ExecutionRecord {
            artifacts,
            tool_calls,
            review_findings: Vec::new(),
            observed_cost_usd: cost,
            escalation_count: 0,
            observability: ExecutionObservability::default(),
            execution_failure: request
                .cost_limit_usd
                .filter(|limit| cost > *limit)
                .map(|_| FailureKind::Aborted),
        });
    }
    let per_worker_limit = request
        .cost_limit_usd
        .map(|limit| limit / config.candidate_roles.len() as f64);
    let worker = Arc::new(DaemonSpawnWorker {
        state: state.clone(),
        parent_session_id: parent_id.clone(),
        spawn_options: bakeoff_spawn_options(per_worker_limit, coding_verifier.is_some()),
    });
    let conductor = Conductor::new(worker);
    let draft_roles = &config.candidate_roles[..config.candidate_roles.len() - 1];
    let draft_tasks = draft_roles
        .iter()
        .enumerate()
        .map(|(index, role)| WorkerTask {
            task_id: format!("{}-draft-{index}", request.task_id.as_str()),
            agent: role.clone(),
            payload: request.prompt.clone(),
            agent_version: None,
            conductor_stable_cache_key: None,
        })
        .collect();
    let draft_plan = ConductorPlan::with_conductor_identity(
        draft_tasks,
        true,
        &parent_id,
        rightkit_agent_orchestration::coding::conductor_day_from_iso(&now_iso()),
    );
    let final_role = config
        .candidate_roles
        .last()
        .ok_or_else(|| "candidate roster is empty".to_string())?;
    let final_task = WorkerTask {
        task_id: format!("{}-synthesis", request.task_id.as_str()),
        agent: final_role.clone(),
        payload: request.prompt.clone(),
        agent_version: None,
        conductor_stable_cache_key: None,
    };
    let drafts = conductor.collect(&draft_plan).await;
    if let Some(failed) = drafts.as_slice().iter().find(|result| !result.success) {
        let (cost, tool_calls) = collect_live_evidence(state, &parent_id).await?;
        return Ok(failed_execution_record(
            cost,
            tool_calls,
            failed.failure_kind.clone().unwrap_or(FailureKind::Unknown),
        ));
    }
    let (draft_cost, _) = collect_live_evidence(state, &parent_id).await?;
    if request
        .cost_limit_usd
        .is_some_and(|limit| draft_cost > limit)
    {
        let (_, tool_calls) = collect_live_evidence(state, &parent_id).await?;
        return Ok(failed_execution_record(
            draft_cost,
            tool_calls,
            FailureKind::Aborted,
        ));
    }
    let final_result = conductor
        .execute_synthesis(&draft_plan, &drafts, final_task, MAX_SYNTHESIS_DRAFT_BYTES)
        .await?;

    let (cost, tool_calls) = collect_live_evidence(state, &parent_id).await?;
    if !final_result.success {
        return Ok(failed_execution_record(
            cost,
            tool_calls,
            final_result
                .failure_kind
                .clone()
                .unwrap_or(FailureKind::Unknown),
        ));
    }
    let artifacts = execution_artifacts(
        isolated.task_path(),
        final_result.output,
        coding_verifier.as_ref(),
    )
    .await?;
    let record = ExecutionRecord {
        artifacts,
        tool_calls,
        review_findings: Vec::new(),
        observed_cost_usd: cost,
        escalation_count: 0,
        observability: ExecutionObservability::default(),
        execution_failure: request
            .cost_limit_usd
            .filter(|limit| cost > *limit)
            .map(|_| FailureKind::Aborted),
    };
    isolated.cleanup().await;
    Ok(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateScaffold {
    ProductionConductor,
    FixedResponseSynthesis,
}

fn candidate_scaffold(verifier: Option<&RuntimeCodingVerifier>) -> CandidateScaffold {
    if verifier.is_some() {
        CandidateScaffold::ProductionConductor
    } else {
        CandidateScaffold::FixedResponseSynthesis
    }
}

async fn execute_single_agent(
    state: &DaemonState,
    request: &ExecutionRequest,
    config: &ExecutedBakeoffRuntimeConfig,
) -> Result<ExecutionRecord, String> {
    let coding_verifier = config
        .coding_verifier_for(request.task_id.as_str())
        .cloned();
    let isolated = BakeoffWorktree::create(
        config.workspace_root_for(request.task_id.as_str()),
        "baseline",
        request,
    )
    .await?;
    let parent_id = create_bakeoff_parent(
        state,
        isolated.path_str(),
        "baseline",
        coding_verifier.is_some(),
        request.cost_limit_usd,
    );
    let _session_guard = BakeoffSessionGuard::new(state.clone(), parent_id.clone());
    let conductor = Conductor::new(Arc::new(DaemonSpawnWorker {
        state: state.clone(),
        parent_session_id: parent_id.clone(),
        spawn_options: bakeoff_spawn_options(request.cost_limit_usd, coding_verifier.is_some()),
    }));
    let plan = ConductorPlan::new(
        vec![WorkerTask {
            task_id: format!("{}-baseline", request.task_id.as_str()),
            agent: config.baseline_role.clone(),
            payload: request.prompt.clone(),
            agent_version: None,
            conductor_stable_cache_key: None,
        }],
        false,
    );
    let result = conductor
        .execute(&plan)
        .await
        .into_iter()
        .next()
        .ok_or_else(|| "single-agent baseline did not execute".to_string())?;
    let (cost, tool_calls) = collect_live_evidence(state, &parent_id).await?;
    if !result.success {
        return Ok(failed_execution_record(
            cost,
            tool_calls,
            result.failure_kind.unwrap_or(FailureKind::Unknown),
        ));
    }
    let artifacts = execution_artifacts(
        isolated.task_path(),
        result.output,
        coding_verifier.as_ref(),
    )
    .await?;
    let record = ExecutionRecord {
        artifacts,
        tool_calls,
        review_findings: Vec::new(),
        observed_cost_usd: cost,
        escalation_count: 0,
        observability: ExecutionObservability::default(),
        execution_failure: request
            .cost_limit_usd
            .filter(|limit| cost > *limit)
            .map(|_| FailureKind::Aborted),
    };
    isolated.cleanup().await;
    Ok(record)
}

fn create_bakeoff_parent(
    state: &DaemonState,
    workspace_root: &str,
    strategy: &str,
    mutable: bool,
    budget_ceiling_usd: Option<f64>,
) -> String {
    let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let id = format!("executed-bakeoff-{}-{sequence}", now_millis());
    state.create_session_with_options(
        &id,
        None,
        workspace_root,
        &format!("Executed bakeoff ({strategy})"),
        None,
        SessionRuntimeOptions {
            safe_mode: true,
            permission_mode: Some(if mutable {
                rightkit_agent_policy::PermissionMode::Bypass
            } else {
                rightkit_agent_policy::PermissionMode::Plan
            }),
            allowed_tools: bakeoff_tools(mutable),
            mcp_channel: "executed-bakeoff".to_string(),
            budget_ceiling_usd,
            ..SessionRuntimeOptions::default()
        },
    );
    id
}

fn bakeoff_tools(mutable: bool) -> Vec<String> {
    let mut tools = vec![
        "read_file",
        "list_files",
        "search",
        "git_diff",
        "git_read",
        "lsp",
        "tool_search",
    ];
    if mutable {
        tools.extend([
            "write_file",
            "edit_file",
            "multi_edit",
            "apply_patch",
            "shell",
        ]);
    }
    tools.into_iter().map(str::to_string).collect()
}

fn bakeoff_spawn_options(
    cost_limit_usd: Option<f64>,
    mutable: bool,
) -> rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
    rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
        max_iterations: Some(8),
        permission_mode: Some(if mutable {
            rightkit_agent_policy::PermissionMode::Bypass
        } else {
            rightkit_agent_policy::PermissionMode::Plan
        }),
        allowed_tools: bakeoff_tools(mutable),
        budget_microusd: cost_limit_usd
            .map(|limit| (limit * 1_000_000.0).floor().max(1.0).min(u64::MAX as f64) as u64),
        ..rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default()
    }
}

struct BakeoffWorktree {
    repository_root: PathBuf,
    path: PathBuf,
    task_path: PathBuf,
}

impl BakeoffWorktree {
    async fn create(
        workspace_root: &str,
        strategy: &str,
        request: &ExecutionRequest,
    ) -> Result<Self, String> {
        let (root_status, root_stdout) = super::run_managed_command(super::ManagedCommandSpec {
            program: "git".into(),
            argv: vec!["rev-parse".into(), "--show-toplevel".into()],
            cwd: PathBuf::from(workspace_root),
            env: Vec::new(),
        })
        .await
        .map_err(|error| format!("failed to inspect bakeoff git workspace: {error}"))?;
        if root_status != 0 {
            return Err(format!(
                "executed bakeoff requires a Git workspace for disposable isolation: {}",
                root_stdout.trim()
            ));
        }
        let repository_root = std::fs::canonicalize(root_stdout.trim())
            .map_err(|error| format!("failed to canonicalize bakeoff Git root: {error}"))?;
        let canonical_workspace = std::fs::canonicalize(workspace_root)
            .map_err(|error| format!("failed to canonicalize bakeoff workspace: {error}"))?;
        let workspace_relative = canonical_workspace
            .strip_prefix(&repository_root)
            .map_err(|_| "bakeoff workspace is outside its Git repository".to_string())?
            .to_path_buf();
        let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let task_fragment = request
            .task_id
            .as_str()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .take(16)
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "coderight-executed-bakeoff-{strategy}-{task_fragment}-{}-{sequence}",
            std::process::id()
        ));
        // Own the cleanup guard before awaiting the mutating Git operation. If
        // the outer bakeoff timeout cancels this future midway through `add`,
        // dropping the future still removes any partially-created directory or
        // worktree registration.
        let worktree = Self {
            repository_root,
            task_path: path.join(&workspace_relative),
            path,
        };
        let (status, output) = super::run_managed_command(super::ManagedCommandSpec {
            program: "git".into(),
            argv: vec![
                "worktree".into(),
                "add".into(),
                "--detach".into(),
                worktree.path.display().to_string(),
                "HEAD".into(),
            ],
            cwd: worktree.repository_root.clone(),
            env: Vec::new(),
        })
        .await
        .map_err(|error| format!("failed to create bakeoff worktree: {error}"))?;
        if status != 0 {
            return Err(format!(
                "failed to create bakeoff worktree: {}",
                output.trim()
            ));
        }
        Ok(worktree)
    }

    fn path_str(&self) -> &str {
        self.task_path.to_str().unwrap_or("")
    }

    fn task_path(&self) -> &Path {
        &self.task_path
    }

    async fn cleanup(&self) {
        let _ = super::run_managed_command(super::ManagedCommandSpec {
            program: "git".into(),
            argv: vec![
                "worktree".into(),
                "remove".into(),
                "--force".into(),
                self.path.display().to_string(),
            ],
            cwd: self.repository_root.clone(),
            env: Vec::new(),
        })
        .await;
    }
}

impl Drop for BakeoffWorktree {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

async fn execution_artifacts(
    workspace: &Path,
    response: String,
    verifier: Option<&RuntimeCodingVerifier>,
) -> Result<Vec<ExecutionArtifact>, String> {
    let Some(verifier) = verifier else {
        return Ok(vec![ExecutionArtifact {
            name: RESPONSE_ARTIFACT_NAME.to_string(),
            body: response,
            produced_by_tool_call_id: None,
        }]);
    };
    let (program, args) = verifier
        .command
        .split_first()
        .ok_or_else(|| "coding verifier command is empty".to_string())?;
    let passed = match tokio::time::timeout(
        Duration::from_secs(VERIFIER_TIMEOUT_SECONDS),
        super::run_managed_command(super::ManagedCommandSpec {
            program: program.to_string(),
            argv: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: workspace.to_path_buf(),
            env: Vec::new(),
        }),
    )
    .await
    {
        Ok(Ok((status, _))) => status == 0,
        Ok(Err(_)) => false,
        Err(_) => false,
    };
    let mut artifacts = vec![ExecutionArtifact {
        name: "verifier-command".to_string(),
        body: if passed { "success" } else { "failure" }.to_string(),
        produced_by_tool_call_id: None,
    }];
    let canonical_workspace = std::fs::canonicalize(workspace)
        .map_err(|error| format!("coding workspace disappeared before verification: {error}"))?;
    for (name, relative) in &verifier.required_artifacts {
        let path = canonical_workspace.join(relative);
        let Ok(canonical) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !canonical.starts_with(&canonical_workspace) {
            continue;
        }
        let Ok(body) = read_bounded_verifier_artifact(&canonical) else {
            continue;
        };
        artifacts.push(ExecutionArtifact {
            name: name.clone(),
            body,
            produced_by_tool_call_id: None,
        });
    }
    Ok(artifacts)
}

fn read_bounded_verifier_artifact(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("verifier artifact metadata failed: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_EXPECTED_OUTPUT_BYTES_PER_TASK as u64 {
        return Err("verifier artifact is not a bounded regular file".to_string());
    }
    std::fs::read_to_string(path)
        .map_err(|error| format!("verifier artifact must be UTF-8: {error}"))
}

struct BakeoffSessionGuard {
    state: DaemonState,
    parent_id: String,
}

impl BakeoffSessionGuard {
    fn new(state: DaemonState, parent_id: String) -> Self {
        Self { state, parent_id }
    }
}

impl Drop for BakeoffSessionGuard {
    fn drop(&mut self) {
        let ids = descendant_session_ids(&self.state, &self.parent_id);
        lock_recover(&self.state.deleted_session_ids, "deleted_session_ids")
            .extend(ids.iter().cloned());
        let removed = {
            let mut sessions = lock_recover(&self.state.sessions, "sessions");
            ids.iter()
                .filter_map(|session_id| sessions.remove(session_id))
                .collect::<Vec<_>>()
        };
        for live in removed {
            if let Some(tx) = &live.interrupt_tx {
                let _ = tx.send(true);
            }
            lock_recover(&live.pending_questions, "session.pending_questions").clear();
            lock_recover(&live.pending_plans, "session.pending_plans").clear();
        }
        if let Some(storage) = self.state.storage.clone() {
            let deferred_storage = storage.clone();
            if let Ok(store) = storage.try_lock() {
                for session_id in &ids {
                    let _ = store.delete_session(session_id);
                }
            } else if let Err(error) = std::thread::Builder::new()
                .name("coderight-bakeoff-cleanup".to_string())
                .spawn(move || {
                    // A dedicated thread is independent of the Tokio runtime
                    // that may be cancelling the timed-out bakeoff future.
                    let store = deferred_storage.blocking_lock();
                    for session_id in ids {
                        let _ = store.delete_session(&session_id);
                    }
                })
            {
                tracing::error!(%error, "failed to start durable bakeoff session cleanup");
            }
        }
    }
}

fn descendant_session_ids(state: &DaemonState, parent_id: &str) -> HashSet<String> {
    let sessions = lock_recover(&state.sessions, "sessions");
    let mut ids = HashSet::from([parent_id.to_string()]);
    loop {
        let before = ids.len();
        for session in sessions.values() {
            if session
                .parent_session_id
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
            {
                ids.insert(session.id.clone());
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}

fn failed_execution_record(
    cost: f64,
    tool_calls: Vec<ToolCallTrace>,
    kind: FailureKind,
) -> ExecutionRecord {
    ExecutionRecord {
        artifacts: Vec::new(),
        tool_calls,
        review_findings: Vec::<ReviewFinding>::new(),
        observed_cost_usd: cost,
        escalation_count: 0,
        observability: ExecutionObservability::default(),
        execution_failure: Some(kind),
    }
}

async fn collect_live_evidence(
    state: &DaemonState,
    parent_id: &str,
) -> Result<(f64, Vec<ToolCallTrace>), String> {
    for _ in 0..50 {
        let sessions = descendant_sessions(state, parent_id);
        if !sessions.is_empty() {
            let costs = sessions
                .iter()
                .map(|session| *lock_recover(&session.cost_usd, "session.cost_usd"))
                .collect::<Vec<_>>();
            if costs.iter().all(Option::is_some) {
                let cost = costs.into_iter().flatten().sum::<f64>();
                if !cost.is_finite() || cost < 0.0 {
                    return Err("live execution produced invalid metered cost".to_string());
                }
                return Ok((cost, collect_tool_calls(&sessions)));
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Err("live execution cost is unavailable; refusing synthetic cost evidence".to_string())
}

fn descendant_sessions(state: &DaemonState, parent_id: &str) -> Vec<Arc<LiveSession>> {
    let sessions = lock_recover(&state.sessions, "sessions");
    let mut ids = BTreeSet::from([parent_id.to_string()]);
    loop {
        let before = ids.len();
        for session in sessions.values() {
            if session
                .parent_session_id
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
            {
                ids.insert(session.id.clone());
            }
        }
        if ids.len() == before {
            break;
        }
    }
    sessions
        .values()
        .filter(|session| session.id != parent_id && ids.contains(&session.id))
        .cloned()
        .collect()
}

fn collect_tool_calls(sessions: &[Arc<LiveSession>]) -> Vec<ToolCallTrace> {
    let mut seen = BTreeSet::new();
    let mut calls = Vec::new();
    for session in sessions {
        for message in lock_recover(&session.transcript, "session.transcript").iter() {
            if let TranscriptMessage::Assistant { tool_calls, .. } = message {
                for call in tool_calls {
                    if seen.insert(call.id.clone()) {
                        calls.push(ToolCallTrace {
                            id: call.id.clone(),
                            tool_name: call.name.clone(),
                        });
                    }
                }
            }
        }
    }
    calls
}

pub(crate) fn insert_executed_bakeoff_openapi_components(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "ExecutedRequiredArtifact".to_string(),
        json!({
            "type": "object",
            "required": ["name", "path", "expectedOutput"],
            "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": 128},
                "path": {"type": "string", "minLength": 1},
                "expectedOutput": {"type": "string", "maxLength": MAX_EXPECTED_OUTPUT_BYTES_PER_TASK}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutedCodingVerifier".to_string(),
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "array", "minItems": 1, "maxItems": MAX_VERIFIER_ARGS, "items": {"type": "string", "minLength": 1, "maxLength": MAX_VERIFIER_ARG_BYTES}},
                "requiredArtifacts": {"type": "array", "maxItems": 16, "items": {"$ref": "#/components/schemas/ExecutedRequiredArtifact"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutedBakeoffTask".to_string(),
        json!({
            "type": "object",
            "required": ["caseNonce", "prompt"],
            "oneOf": [
                {"required": ["expectedOutput"], "not": {"required": ["codingVerifier"]}},
                {"required": ["codingVerifier"], "not": {"required": ["expectedOutput"]}}
            ],
            "properties": {
                "caseNonce": {"type": "string", "minLength": 1, "maxLength": 128},
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_PROMPT_BYTES_PER_TASK},
                "expectedOutput": {"type": "string", "maxLength": MAX_EXPECTED_OUTPUT_BYTES_PER_TASK},
                "codingVerifier": {"$ref": "#/components/schemas/ExecutedCodingVerifier"},
                "workspaceRoot": {"type": ["string", "null"], "minLength": 1, "description": "Optional per-task prepared Git workspace. Corpus adapters use this to keep each task isolated."},
                "contextTokens": {"type": "integer", "minimum": 0, "maximum": MAX_CONTEXT_TOKENS, "description": "Optional asserted token count. When non-zero it must match RightKit's measured prompt token count."},
                "contextClass": {"type": "string", "enum": ["standard", "long"]},
                "contextCohort": {"type": ["string", "null"], "maxLength": 128},
                "candidateCostLimitUsd": {"type": ["number", "null"], "exclusiveMinimum": 0, "maximum": MAX_COST_LIMIT_USD}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutedBakeoffGate".to_string(),
        json!({
            "type": "object",
            "properties": {
                "minimumVerifiedCompletionRate": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
                "minimumCompletionRateDeltaVsBaseline": {"type": ["number", "null"], "minimum": -1, "maximum": 1},
                "maximumEscalationRate": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
                "minimumToolCallEfficiency": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
                "maximumReviewFalsePositiveRate": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
                "maximumLongContextDegradationRate": {"type": ["number", "null"], "minimum": 0, "maximum": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutedBakeoffRequest".to_string(),
        json!({
            "type": "object",
            "required": ["suiteNamespace", "workspaceRoot", "candidateRoles", "baselineRole", "tasks"],
            "properties": {
                "suiteNamespace": {"type": "string", "minLength": 1, "maxLength": 128},
                "workspaceRoot": {"type": "string", "minLength": 1},
                "candidateRoles": {"type": "array", "minItems": 2, "maxItems": 4, "uniqueItems": true, "items": {"type": "string", "minLength": 1, "maxLength": 64}},
                "baselineRole": {"type": "string", "minLength": 1, "maxLength": 64},
                "tasks": {"type": "array", "minItems": 1, "maxItems": MAX_TASKS, "items": {"$ref": "#/components/schemas/ExecutedBakeoffTask"}},
                "gate": {"$ref": "#/components/schemas/ExecutedBakeoffGate"},
                "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECONDS, "default": DEFAULT_TIMEOUT_SECONDS}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExecutedBakeoffResponse".to_string(),
        json!({
            "type": "object",
            "required": ["report", "decision"],
            "properties": {
                "report": {
                    "type": "object",
                    "description": "Sanitized coderight.executed-bakeoff.v1 report. It omits prompts, expected digests, and artifact bodies."
                },
                "decision": {
                    "type": "object",
                    "required": ["passed", "failures"],
                    "properties": {
                        "passed": {"type": "boolean"},
                        "failures": {"type": "array", "items": {"type": "string"}}
                    },
                    "additionalProperties": false
                }
            },
            "additionalProperties": false
        }),
    );
}

#[cfg(test)]
mod executed_bakeoff_runtime_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    #[test]
    fn coding_tasks_select_the_production_conductor_scaffold() {
        let verifier = RuntimeCodingVerifier {
            command: vec!["true".into()],
            required_artifacts: Vec::new(),
        };
        assert_eq!(
            candidate_scaffold(Some(&verifier)),
            CandidateScaffold::ProductionConductor
        );
        assert_eq!(
            candidate_scaffold(None),
            CandidateScaffold::FixedResponseSynthesis
        );
    }

    #[tokio::test]
    async fn bakeoff_parent_carries_the_operator_cost_ceiling() {
        let state = DaemonState::new("test-token");
        let dir = tempfile::tempdir().unwrap();
        let id = create_bakeoff_parent(
            &state,
            dir.path().to_str().unwrap(),
            "candidate",
            true,
            Some(0.0123),
        );
        assert_eq!(
            state.get_session(&id).unwrap().budget_ceiling_usd,
            Some(0.0123)
        );
    }

    #[derive(Default)]
    struct MockRuntime {
        calls: Mutex<Vec<(ExecutionTopology, Option<f64>, String)>>,
    }

    #[async_trait]
    impl ExecutedBakeoffRuntime for MockRuntime {
        async fn execute(
            &self,
            _state: &DaemonState,
            topology: ExecutionTopology,
            request: &ExecutionRequest,
            _config: &ExecutedBakeoffRuntimeConfig,
        ) -> Result<ExecutionRecord, String> {
            self.calls.lock().unwrap().push((
                topology,
                request.cost_limit_usd,
                request.task_id.as_str().to_string(),
            ));
            let candidate = topology == ExecutionTopology::RoutedOrchestrated;
            Ok(ExecutionRecord {
                artifacts: vec![ExecutionArtifact {
                    name: RESPONSE_ARTIFACT_NAME.to_string(),
                    body: if candidate {
                        "candidate-fixed".to_string()
                    } else {
                        "baseline-fixed".to_string()
                    },
                    produced_by_tool_call_id: Some(
                        if candidate {
                            "candidate-tool"
                        } else {
                            "baseline-tool"
                        }
                        .to_string(),
                    ),
                }],
                tool_calls: vec![ToolCallTrace {
                    id: if candidate {
                        "candidate-tool"
                    } else {
                        "baseline-tool"
                    }
                    .to_string(),
                    tool_name: "mock-live-tool".to_string(),
                }],
                review_findings: Vec::new(),
                observed_cost_usd: if candidate { 0.02 } else { 0.019 },
                escalation_count: 0,
                observability: ExecutionObservability {
                    useful_tool_attribution: true,
                    review_findings: true,
                    escalations: true,
                },
                execution_failure: None,
            })
        }
    }

    fn valid_body() -> Value {
        json!({
            "suiteNamespace": "runtime-smoke",
            "workspaceRoot": std::env::temp_dir(),
            "candidateRoles": ["coder", "code-reviewer"],
            "baselineRole": "coder",
            "tasks": [
                {"caseNonce": "case-a", "prompt": "private prompt alpha", "expectedOutput": "candidate-fixed", "candidateCostLimitUsd": 0.05},
                {"caseNonce": "case-b", "prompt": "private prompt beta", "expectedOutput": "candidate-fixed", "candidateCostLimitUsd": 0.05}
            ],
            "gate": {"minimumVerifiedCompletionRate": 1.0},
            "timeoutSeconds": 5
        })
    }

    #[test]
    fn production_spawn_profile_is_read_only_and_cost_bounded() {
        let options = bakeoff_spawn_options(Some(0.012345), false);

        assert_eq!(
            options.permission_mode,
            Some(rightkit_agent_policy::PermissionMode::Plan)
        );
        assert_eq!(options.budget_microusd, Some(12_345));
        assert!(options.allowed_tools.contains(&"read_file".to_string()));
        assert!(!options.allowed_tools.contains(&"shell".to_string()));
        assert!(!options.allowed_tools.contains(&"write_file".to_string()));
        assert!(!options.allowed_tools.contains(&"browser_fetch".to_string()));
    }

    #[test]
    fn coding_spawn_profile_is_mutable_only_inside_disposable_evaluation_path() {
        let options = bakeoff_spawn_options(Some(0.01), true);

        assert_eq!(
            options.permission_mode,
            Some(rightkit_agent_policy::PermissionMode::Bypass)
        );
        assert!(options
            .allowed_tools
            .iter()
            .any(|tool| tool == "apply_patch"));
        assert!(options.allowed_tools.iter().any(|tool| tool == "shell"));
    }

    #[tokio::test]
    async fn coding_verifier_runs_out_of_band_and_collects_bounded_artifacts() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("result.txt"), "verified-output").unwrap();
        let command = if cfg!(windows) {
            vec![
                "cmd".into(),
                "/C".into(),
                "if exist result.txt (exit 0) else (exit 1)".into(),
            ]
        } else {
            vec!["sh".into(), "-c".into(), "test -f result.txt".into()]
        };
        let verifier = RuntimeCodingVerifier {
            command,
            required_artifacts: vec![("result".into(), "result.txt".into())],
        };

        let artifacts = execution_artifacts(
            workspace.path(),
            "model response is not the coding verdict".into(),
            Some(&verifier),
        )
        .await
        .unwrap();

        assert_eq!(artifacts[0].name, "verifier-command");
        assert_eq!(artifacts[0].body, "success");
        assert_eq!(artifacts[1].name, "result");
        assert_eq!(artifacts[1].body, "verified-output");
        assert!(artifacts.iter().all(|artifact| {
            !artifact.body.contains("test -f") && !artifact.body.contains("model response")
        }));
    }

    #[tokio::test]
    async fn session_guard_interrupts_and_removes_the_whole_bakeoff_tree() {
        let state = DaemonState::new("");
        state.create_session("bakeoff-parent", None, "/tmp", "parent", None);
        state.create_session(
            "bakeoff-child",
            Some("coder"),
            "/tmp",
            "child",
            Some("bakeoff-parent"),
        );

        drop(BakeoffSessionGuard::new(
            state.clone(),
            "bakeoff-parent".to_string(),
        ));

        assert!(state.get_session("bakeoff-parent").is_none());
        assert!(state.get_session("bakeoff-child").is_none());
        let deleted = lock_recover(&state.deleted_session_ids, "deleted_session_ids");
        assert!(deleted.contains("bakeoff-parent"));
        assert!(deleted.contains("bakeoff-child"));
    }

    #[tokio::test]
    async fn session_guard_finishes_storage_cleanup_when_writer_is_contended() {
        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage.clone());
        state.create_session("bakeoff-persisted", None, "/tmp", "parent", None);

        for _ in 0..200 {
            if storage
                .lock()
                .await
                .get_session("bakeoff-persisted")
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            storage
                .lock()
                .await
                .get_session("bakeoff-persisted")
                .unwrap()
                .is_some(),
            "session row must exist before cleanup contention is introduced"
        );

        let held_writer = storage.lock().await;
        drop(BakeoffSessionGuard::new(
            state,
            "bakeoff-persisted".to_string(),
        ));
        drop(held_writer);

        for _ in 0..200 {
            if storage
                .lock()
                .await
                .get_session("bakeoff-persisted")
                .unwrap()
                .is_none()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("contended bakeoff session cleanup did not finish within 1s");
    }

    #[tokio::test]
    async fn production_workspace_is_a_disposable_detached_worktree() {
        let root = std::env::temp_dir().join(format!(
            "coderight-bakeoff-isolation-test-{}-{}",
            std::process::id(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let output = rightkit_child_process::command_std("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"]);
        git(&["config", "user.email", "bakeoff-test@coderight.invalid"]);
        git(&["config", "user.name", "RightKit Bakeoff Test"]);
        // A developer with the Windows default `core.autocrlf=true` would get
        // "pristine\r\n" back out of the worktree checkout. Pin the repo-local
        // setting so the byte-exact assertion below stays a content check
        // rather than a line-ending check.
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "core.eol", "lf"]);
        let selected = root.join("task-subdir");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::write(selected.join("fixture.txt"), "pristine\n").unwrap();
        git(&["add", "task-subdir/fixture.txt"]);
        git(&["commit", "-m", "fixture"]);
        let task = EvaluationTask::new(
            TaskIdentity {
                suite_namespace: "isolation-test",
                private_corpus_secret: &[3_u8; 32],
                case_nonce: "case-a",
            },
            "inspect fixture",
            VerificationContract::default(),
        )
        .unwrap();
        let request = ExecutionRequest {
            task_id: task.id,
            prompt: task.prompt,
            context_tokens: 0,
            cost_limit_usd: Some(0.01),
        };

        let isolated = BakeoffWorktree::create(selected.to_str().unwrap(), "candidate", &request)
            .await
            .unwrap();
        let worktree_root = isolated.path.clone();
        let isolated_path = isolated.task_path().to_path_buf();
        assert_ne!(isolated_path, root);
        assert_eq!(isolated_path.file_name().unwrap(), "task-subdir");
        assert_eq!(
            std::fs::read_to_string(isolated_path.join("fixture.txt")).unwrap(),
            "pristine\n"
        );
        drop(isolated);
        assert!(!worktree_root.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn authenticated_route_invokes_live_executors_transfers_same_cost_and_hides_corpus() {
        let runtime = Arc::new(MockRuntime::default());
        let state = DaemonState::new("secret-token").with_executed_bakeoff_runtime(runtime.clone());
        let app = crate::api::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/runtime/executed-bakeoff")
                    .header("authorization", "Bearer secret-token")
                    .header("content-type", "application/json")
                    .body(Body::from(valid_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["report"]["contains_live_execution_evidence"], true);
        assert_eq!(
            response["report"]["candidate"]["executor"]["evidence_source"],
            "live_execution"
        );
        assert_eq!(
            response["report"]["same_cost_single_agent_baseline"]["executor"]["topology"],
            "single_agent"
        );
        assert_eq!(
            response["report"]["comparison_receipt"]["evaluator_id"],
            "artifact_digest_verifier"
        );
        assert_eq!(
            response["report"]["comparison_receipt"]["evaluator_version"],
            "1"
        );
        assert_eq!(
            response["report"]["candidate"]["execution_receipt"],
            response["report"]["comparison_receipt"]["candidate_execution_receipt"]
        );
        assert_eq!(
            response["report"]["same_cost_single_agent_baseline"]["execution_receipt"],
            response["report"]["comparison_receipt"]["baseline_execution_receipt"]
        );
        assert!(!response["report"]["comparison_receipt"]["receipt_id"]
            .as_str()
            .unwrap_or_default()
            .is_empty());
        assert_eq!(response["decision"]["passed"], true);
        let serialized = response.to_string();
        for secret in [
            "private prompt alpha",
            "private prompt beta",
            "candidate-fixed",
            "baseline-fixed",
            "response_mismatch",
        ] {
            assert!(!serialized.contains(secret), "response leaked {secret}");
        }

        let calls = runtime.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        for pair in calls.chunks_exact(2) {
            assert_eq!(pair[0].0, ExecutionTopology::RoutedOrchestrated);
            assert_eq!(pair[1].0, ExecutionTopology::SingleAgent);
            assert_eq!(pair[1].1, Some(0.02));
            assert_eq!(pair[0].2, pair[1].2);
        }
    }

    #[tokio::test]
    async fn route_requires_bearer_auth_before_executor_invocation() {
        let runtime = Arc::new(MockRuntime::default());
        let state = DaemonState::new("secret-token").with_executed_bakeoff_runtime(runtime.clone());
        let status = crate::api::build_router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/runtime/executed-bakeoff")
                    .header("content-type", "application/json")
                    .body(Body::from(valid_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(runtime.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn route_rejects_lower_trust_before_executor_invocation() {
        let runtime = Arc::new(MockRuntime::default());
        let state = DaemonState::new("").with_executed_bakeoff_runtime(runtime.clone());
        let status = crate::api::build_router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/runtime/executed-bakeoff")
                    .header("content-type", "application/json")
                    .header("x-coderight-source-trust", "remoteAgentUntrusted")
                    .body(Body::from(valid_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(runtime.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn executed_bakeoff_openapi_contract_is_registered() {
        assert_eq!(
            benchmark_openapi_request_schema("post", "/runtime/executed-bakeoff"),
            Some(benchmark_schema_ref("ExecutedBakeoffRequest"))
        );
        assert_eq!(
            benchmark_openapi_response_schema("post", "/runtime/executed-bakeoff"),
            Some(benchmark_schema_ref("ExecutedBakeoffResponse"))
        );
        let components = benchmark_openapi_components();
        assert!(components.contains_key("ExecutedBakeoffRequest"));
        assert!(components.contains_key("ExecutedBakeoffResponse"));
    }

    #[tokio::test]
    async fn production_runtime_fails_closed_without_a_live_provider() {
        let state = DaemonState::new("");
        let task = EvaluationTask::new(
            TaskIdentity {
                suite_namespace: "fail-closed",
                private_corpus_secret: &[7_u8; 32],
                case_nonce: "no-provider",
            },
            "do not execute",
            VerificationContract::default(),
        )
        .unwrap();
        let error = DaemonExecutedBakeoffRuntime
            .execute(
                &state,
                ExecutionTopology::SingleAgent,
                &ExecutionRequest {
                    task_id: task.id,
                    prompt: task.prompt,
                    context_tokens: 0,
                    cost_limit_usd: Some(0.01),
                },
                &ExecutedBakeoffRuntimeConfig {
                    workspace_root: std::env::temp_dir().to_string_lossy().into_owned(),
                    task_workspace_roots: BTreeMap::new(),
                    coding_verifiers: BTreeMap::new(),
                    candidate_roles: vec!["coder".to_string(), "code-reviewer".to_string()],
                    baseline_role: "coder".to_string(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, "live provider execution is not configured");
    }
}
