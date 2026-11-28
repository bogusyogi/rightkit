use std::fs;

static CORPUS_SCHEMA_VERSION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::host_identity::schema("eval-corpus.v1"));
static CORPUS_REPORT_SCHEMA_VERSION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::host_identity::schema("corpus-bakeoff.v1"));
static CORPUS_LOCK_SCHEMA_VERSION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::host_identity::schema("eval-corpus-lock.v1"));
const MAX_CORPUS_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MEASUREMENT_REPETITIONS: u32 = 10;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusBakeoffRequest {
    manifest_path: String,
    /// Write the profile lock (`<manifest>.lock.json`) before running.
    #[serde(default)]
    freeze: bool,
}

/// Frozen run profile (issue 50). `system_prompt_sha256` must equal the digest of the shipping
/// system prompt, so a benchmark run cannot drift from the product prompt.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusProfile {
    model_id: String,
    context_window: u64,
    effort: String,
    system_prompt_sha256: String,
}

/// CR8A measurement block (issue 68). Every field is read as an `Option` so a missing one is
/// refused by name (`measurement.<field> is required`) instead of by a serde message.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusMeasurement {
    #[serde(default)]
    identity: Option<CorpusIdentity>,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    repetitions: Option<u32>,
    #[serde(default)]
    budgets: Option<CorpusBudgets>,
    #[serde(default)]
    held_out_families: Option<Vec<String>>,
    #[serde(default)]
    verifier_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusIdentity {
    #[serde(default)]
    policy_version: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusBudgets {
    #[serde(default)]
    per_task_usd: Option<f64>,
    #[serde(default)]
    total_usd: Option<f64>,
}

/// The measurement block after every field was checked; its canonical JSON feeds the corpus digest.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckedMeasurement {
    identity: CheckedIdentity,
    seed: u64,
    repetitions: u32,
    budgets: CheckedBudgets,
    held_out_families: Vec<String>,
    verifier_version: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckedIdentity {
    policy_version: String,
    provider_id: String,
    model_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckedBudgets {
    per_task_usd: f64,
    total_usd: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusManifest {
    schema_version: String,
    suite_namespace: String,
    candidate_roles: Vec<String>,
    baseline_role: String,
    tasks: Vec<CorpusTask>,
    #[serde(default)]
    gate: ExecutedBakeoffGateBody,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default)]
    profile: Option<CorpusProfile>,
    #[serde(default)]
    measurement: Option<CorpusMeasurement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusTask {
    case_nonce: String,
    source: CorpusTaskSource,
    verifier: CorpusVerifier,
    #[serde(default)]
    context_tokens: u64,
    #[serde(default = "default_context_class")]
    context_class: ContextClass,
    #[serde(default)]
    context_cohort: Option<String>,
    #[serde(default)]
    candidate_cost_limit_usd: Option<f64>,
    /// Task family (at most 64 bytes); required when the manifest declares `measurement`.
    #[serde(default)]
    family: Option<String>,
    /// Git tree id (40 lowercase hex) of the task workspace directory; required with `measurement`.
    #[serde(default)]
    fixture_tree: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum CorpusTaskSource {
    Private {
        #[serde(rename = "instructionPath")]
        instruction_path: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
    },
    Harbor {
        #[serde(rename = "taskRoot")]
        task_root: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum CorpusVerifier {
    ExactResponse {
        #[serde(rename = "expectedOutputPath")]
        expected_output_path: String,
    },
    Command {
        command: Vec<String>,
        #[serde(default, rename = "requiredArtifacts")]
        required_artifacts: Vec<CorpusRequiredArtifact>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusRequiredArtifact {
    name: String,
    path: String,
    expected_output_path: String,
}

#[derive(Debug, Deserialize)]
struct HarborTaskToml {
    schema_version: String,
    task: HarborTaskIdentity,
}

#[derive(Debug, Deserialize)]
struct HarborTaskIdentity {
    name: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CorpusSourceCounts {
    private: usize,
    harbor: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CorpusBakeoffResponse {
    schema_version: &'static str,
    manifest_sha256: String,
    task_count: usize,
    sources: CorpusSourceCounts,
    report: BakeoffReport,
    decision: BakeoffGateDecision,
    /// Digest of the constant part of the shipping system prompt, read live on every run (issue 50).
    system_prompt_sha256: String,
    /// One entry per executed (task, role) of every repetition (issue 50).
    runs: Vec<CorpusRunArtifact>,
    /// Echo of the validated measurement block (issue 68); absent without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    measurement: Option<serde_json::Value>,
    /// Every repetition's report in order (the first is also `report`); absent without a measurement block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    repetition_reports: Vec<serde_json::Value>,
    /// Additive daemon resource snapshot (`{app}.resource-metrics.v1`
    /// shape) captured when the report is produced. Existing consumers keep
    /// working: this field only adds to the response.
    resource_metrics: serde_json::Value,
}

/// One executed (task, role): `None` token and cost fields mean the provider reported nothing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CorpusRunArtifact {
    case_nonce: String,
    role: String,
    /// `completed`, `aborted` or `deadline`.
    classification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    context_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_path: Option<String>,
}

struct LoadedMeasurement {
    repetitions: u32,
    echo: serde_json::Value,
}

struct LoadedCorpus {
    body: ExecutedBakeoffBody,
    manifest_sha256: String,
    sources: CorpusSourceCounts,
    manifest_path: PathBuf,
    profile: Option<CorpusProfile>,
    measurement: Option<LoadedMeasurement>,
}

#[derive(Debug)]
struct CorpusLoadError {
    status: StatusCode,
    message: String,
}

impl CorpusLoadError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

async fn run_corpus_bakeoff_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<CorpusBakeoffRequest>,
) -> Result<Json<CorpusBakeoffResponse>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|status| corpus_http_error(status, "trusted operator authority required"))?;
    let loaded = load_corpus_manifest(Path::new(&request.manifest_path))
        .map_err(|error| corpus_http_error(error.status, &error.message))?;
    let system_prompt_sha256 = crate::api::shipping_prompt_digest();
    if let Some(profile) = &loaded.profile {
        if profile.system_prompt_sha256 != system_prompt_sha256 {
            return Err(corpus_http_error(
                StatusCode::CONFLICT,
                &format!(
                    "prompt drift: manifest pins {}, the shipping prompt is {system_prompt_sha256}",
                    profile.system_prompt_sha256
                ),
            ));
        }
        enforce_profile_lock(&loaded, profile, request.freeze)
            .map_err(|error| corpus_http_error(error.status, &error.message))?;
    }
    let timeout_seconds = loaded.body.timeout_seconds;
    let task_count = loaded.body.tasks.len();
    let context_by_nonce = loaded
        .body
        .tasks
        .iter()
        .map(|task| {
            (
                task.case_nonce.clone(),
                rightkit_llm::agent_providers::client::capacity::measure_o200k_base(&task.prompt),
            )
        })
        .collect::<BTreeMap<String, u64>>();
    // Snapshot resource state before execution consumes `state`; the snapshot
    // then rides along with the report additively.
    let resource_metrics = crate::api_runtime::resource_metrics_json(&state).await;
    let manifest_path = loaded.manifest_path.clone();
    let repetitions = loaded
        .measurement
        .as_ref()
        .map_or(1, |measurement| measurement.repetitions);
    let measurement_echo = loaded
        .measurement
        .as_ref()
        .map(|measurement| measurement.echo.clone());
    let first_body = loaded.body;
    // One outer timeout covers every repetition. Repetitions run one after the other, each
    // from a freshly loaded body so the first one is the manifest exactly as today.
    let executions = tokio::time::timeout(Duration::from_secs(timeout_seconds), async {
        let mut executions = Vec::with_capacity(repetitions as usize);
        let mut first_body = Some(first_body);
        for repetition in 0..repetitions {
            let mut body = match first_body.take() {
                Some(body) => body,
                None => {
                    load_corpus_manifest(&manifest_path)
                        .map_err(|error| (error.status, error.message))?
                        .body
                }
            };
            if repetition > 0 {
                for task in &mut body.tasks {
                    task.case_nonce = format!("{}#{repetition}", task.case_nonce);
                }
            }
            executions.push(
                run_live_executed_bakeoff(state.clone(), body)
                    .await
                    .map_err(|status| (status, "corpus bakeoff execution failed".to_string()))?,
            );
        }
        Ok::<_, (StatusCode, String)>(executions)
    })
    .await
    .map_err(|_| corpus_http_error(StatusCode::GATEWAY_TIMEOUT, "corpus bakeoff timed out"))?
    .map_err(|(status, message)| corpus_http_error(status, &message))?;
    let mut runs = Vec::new();
    let mut repetition_reports = Vec::new();
    let mut executions = executions.into_iter();
    let first = executions.next().ok_or_else(|| {
        corpus_http_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corpus bakeoff produced no report",
        )
    })?;
    if measurement_echo.is_some() {
        repetition_reports.push(serde_json::to_value(&first.report).unwrap_or(Value::Null));
    }
    runs.extend(corpus_run_artifacts(&first.runs, &context_by_nonce));
    let ExecutedBakeoffResponse {
        report, decision, ..
    } = first;
    for execution in executions {
        repetition_reports.push(serde_json::to_value(&execution.report).unwrap_or(Value::Null));
        runs.extend(corpus_run_artifacts(&execution.runs, &context_by_nonce));
    }
    Ok(Json(CorpusBakeoffResponse {
        schema_version: CORPUS_REPORT_SCHEMA_VERSION.as_str(),
        manifest_sha256: loaded.manifest_sha256,
        task_count,
        sources: loaded.sources,
        report,
        decision,
        system_prompt_sha256,
        runs,
        measurement: measurement_echo,
        repetition_reports,
        resource_metrics,
    }))
}

/// Map the executor's per-run records to the response rows. `deadline` wins over `completed`;
/// a run that neither completed nor hit its limit is `aborted`.
fn corpus_run_artifacts(
    runs: &[ExecutedRunArtifact],
    context_by_nonce: &BTreeMap<String, u64>,
) -> Vec<CorpusRunArtifact> {
    runs.iter()
        .map(|run| {
            let context_tokens = context_by_nonce
                .get(&run.case_nonce)
                .or_else(|| {
                    run.case_nonce
                        .rsplit_once('#')
                        .and_then(|(base, _)| context_by_nonce.get(base))
                })
                .copied()
                .unwrap_or(0);
            CorpusRunArtifact {
                case_nonce: run.case_nonce.clone(),
                role: run.role.clone(),
                classification: if run.deadline {
                    "deadline"
                } else if run.completed {
                    "completed"
                } else {
                    "aborted"
                },
                input_tokens: run.input_tokens,
                cached_input_tokens: run.cached_input_tokens,
                output_tokens: run.output_tokens,
                context_tokens,
                cost_usd: run.observed_cost_usd,
                trace_path: run.trace_path.clone(),
            }
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn corpus_lock_path(manifest_path: &Path) -> PathBuf {
    let mut raw = manifest_path.as_os_str().to_os_string();
    raw.push(".lock.json");
    PathBuf::from(raw)
}

/// `freeze` writes `<manifest>.lock.json` (overwriting); otherwise the lock must exist and match
/// both the manifest digest and the canonical profile digest.
fn enforce_profile_lock(
    loaded: &LoadedCorpus,
    profile: &CorpusProfile,
    freeze: bool,
) -> Result<(), CorpusLoadError> {
    let profile_json = serde_json::to_string(profile)
        .map_err(|_| CorpusLoadError::internal("profile could not be serialised"))?;
    let profile_sha256 = sha256_hex(profile_json.as_bytes());
    let lock_path = corpus_lock_path(&loaded.manifest_path);
    if freeze {
        let lock = json!({
            "schema": CORPUS_LOCK_SCHEMA_VERSION.as_str(),
            "manifestSha256": loaded.manifest_sha256,
            "profileSha256": profile_sha256,
        });
        let bytes = serde_json::to_vec_pretty(&lock)
            .map_err(|_| CorpusLoadError::internal("profile lock could not be serialised"))?;
        return fs::write(&lock_path, bytes)
            .map_err(|_| CorpusLoadError::internal("profile lock could not be written"));
    }
    let raw = fs::read_to_string(&lock_path).map_err(|_| {
        CorpusLoadError::conflict("profile lock missing: run once with freeze:true")
    })?;
    let stored: Value = serde_json::from_str(&raw).map_err(|_| {
        CorpusLoadError::conflict("profile drifted from its lock (lock unreadable)")
    })?;
    let mut differing = Vec::new();
    if stored.get("schema").and_then(Value::as_str) != Some(CORPUS_LOCK_SCHEMA_VERSION.as_str()) {
        differing.push("schema");
    }
    if stored.get("manifestSha256").and_then(Value::as_str) != Some(loaded.manifest_sha256.as_str())
    {
        differing.push("manifest");
    }
    if stored.get("profileSha256").and_then(Value::as_str) != Some(profile_sha256.as_str()) {
        differing.push("profile");
    }
    if differing.is_empty() {
        Ok(())
    } else {
        Err(CorpusLoadError::conflict(format!(
            "profile drifted from its lock ({})",
            differing.join("+")
        )))
    }
}

fn validate_profile(profile: CorpusProfile) -> Result<CorpusProfile, CorpusLoadError> {
    if profile.model_id.trim().is_empty() || profile.model_id.len() > 128 {
        return Err(CorpusLoadError::bad(
            "profile.modelId is required (at most 128 bytes)",
        ));
    }
    if profile.context_window == 0 {
        return Err(CorpusLoadError::bad(
            "profile.contextWindow must be greater than 0",
        ));
    }
    if profile.effort.trim().is_empty() || profile.effort.len() > 64 {
        return Err(CorpusLoadError::bad(
            "profile.effort is required (at most 64 bytes)",
        ));
    }
    let digest = &profile.system_prompt_sha256;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CorpusLoadError::bad(
            "profile.systemPromptSha256 must be 64 lowercase hex characters",
        ));
    }
    Ok(profile)
}

fn measurement_required(field: &str) -> CorpusLoadError {
    CorpusLoadError::bad(format!("measurement.{field} is required"))
}

fn measurement_text(value: Option<String>, field: &str) -> Result<String, CorpusLoadError> {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| measurement_required(field))?;
    if value.len() > 128 {
        return Err(CorpusLoadError::bad(format!(
            "measurement.{field} must be at most 128 bytes"
        )));
    }
    Ok(value)
}

impl CorpusMeasurement {
    /// Check the fields in the order identity.policyVersion, identity.providerId,
    /// identity.modelId, seed, repetitions, budgets, heldOutFamilies, verifierVersion.
    fn validate(self) -> Result<CheckedMeasurement, CorpusLoadError> {
        let identity = self.identity.unwrap_or_default();
        let policy_version = measurement_text(identity.policy_version, "identity.policyVersion")?;
        let provider_id = measurement_text(identity.provider_id, "identity.providerId")?;
        let model_id = measurement_text(identity.model_id, "identity.modelId")?;
        let seed = self.seed.ok_or_else(|| measurement_required("seed"))?;
        let repetitions = self
            .repetitions
            .ok_or_else(|| measurement_required("repetitions"))?;
        if !(1..=MAX_MEASUREMENT_REPETITIONS).contains(&repetitions) {
            return Err(CorpusLoadError::bad(
                "measurement.repetitions must be 1..=10",
            ));
        }
        let budgets = self
            .budgets
            .ok_or_else(|| measurement_required("budgets"))?;
        let per_task_usd = budgets
            .per_task_usd
            .ok_or_else(|| measurement_required("budgets.perTaskUsd"))?;
        let total_usd = budgets
            .total_usd
            .ok_or_else(|| measurement_required("budgets.totalUsd"))?;
        if !(per_task_usd.is_finite()
            && total_usd.is_finite()
            && per_task_usd > 0.0
            && per_task_usd <= total_usd
            && total_usd <= MAX_COST_LIMIT_USD)
        {
            return Err(CorpusLoadError::bad(format!(
                "measurement.budgets must satisfy 0 < perTaskUsd <= totalUsd <= {MAX_COST_LIMIT_USD}"
            )));
        }
        let held_out_families = self
            .held_out_families
            .filter(|families| !families.is_empty())
            .ok_or_else(|| measurement_required("heldOutFamilies"))?;
        if held_out_families
            .iter()
            .any(|family| family.trim().is_empty() || family.len() > 64)
        {
            return Err(CorpusLoadError::bad(
                "measurement.heldOutFamilies entries must be non-empty and at most 64 bytes",
            ));
        }
        let verifier_version = measurement_text(self.verifier_version, "verifierVersion")?;
        Ok(CheckedMeasurement {
            identity: CheckedIdentity {
                policy_version,
                provider_id,
                model_id,
            },
            seed,
            repetitions,
            budgets: CheckedBudgets {
                per_task_usd,
                total_usd,
            },
            held_out_families,
            verifier_version,
        })
    }
}

/// Task-level checks that need only the manifest: label formats always, and with a measurement
/// block family and fixtureTree on every task, a heldout and a development split, and the budgets.
fn check_task_measurement(
    measurement: Option<&CheckedMeasurement>,
    suite_namespace: &str,
    tasks: &[CorpusTask],
) -> Result<(), CorpusLoadError> {
    for task in tasks {
        if let Some(family) = &task.family {
            if family.trim().is_empty() || family.len() > 64 {
                return Err(CorpusLoadError::bad(format!(
                    "task {} family must be non-empty and at most 64 bytes",
                    task.case_nonce
                )));
            }
        }
        if let Some(tree) = &task.fixture_tree {
            if tree.len() != 40
                || !tree
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(CorpusLoadError::bad(format!(
                    "task {} fixtureTree must be 40 lowercase hex characters",
                    task.case_nonce
                )));
            }
        }
    }
    let Some(measurement) = measurement else {
        return Ok(());
    };
    if suite_namespace.len() + format!("#seed={}", measurement.seed).len() > 128 {
        return Err(CorpusLoadError::bad(
            "suiteNamespace is too long to carry the measurement seed",
        ));
    }
    let (mut heldout, mut development) = (0usize, 0usize);
    let mut budget_sum = 0.0f64;
    for task in tasks {
        let Some(family) = task.family.as_deref() else {
            return Err(CorpusLoadError::bad(format!(
                "task {} needs family and fixtureTree",
                task.case_nonce
            )));
        };
        if task.fixture_tree.is_none() {
            return Err(CorpusLoadError::bad(format!(
                "task {} needs family and fixtureTree",
                task.case_nonce
            )));
        }
        if measurement
            .held_out_families
            .iter()
            .any(|held| held == family)
        {
            heldout += 1;
        } else {
            development += 1;
        }
        let limit = match task.candidate_cost_limit_usd {
            Some(limit) if limit > measurement.budgets.per_task_usd => {
                return Err(CorpusLoadError::bad(format!(
                    "task {} candidateCostLimitUsd exceeds measurement.budgets.perTaskUsd",
                    task.case_nonce
                )));
            }
            Some(limit) => limit,
            None => measurement.budgets.per_task_usd,
        };
        budget_sum += limit;
    }
    if heldout == 0 || development == 0 {
        return Err(CorpusLoadError::bad(
            "heldOutFamilies must leave both a heldout and a development task",
        ));
    }
    if budget_sum > measurement.budgets.total_usd + 1e-9 {
        return Err(CorpusLoadError::bad("task budgets exceed budgets.totalUsd"));
    }
    Ok(())
}

fn corpus_http_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message})))
}

fn load_corpus_manifest(path: &Path) -> Result<LoadedCorpus, CorpusLoadError> {
    let manifest_path = fs::canonicalize(path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest does not exist"))?;
    let metadata = fs::metadata(&manifest_path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest metadata is unavailable"))?;
    if !metadata.is_file() {
        return Err(CorpusLoadError::bad("corpus manifest must be a file"));
    }
    if metadata.len() > MAX_CORPUS_MANIFEST_BYTES {
        return Err(CorpusLoadError::too_large("corpus manifest exceeds 1 MiB"));
    }
    let raw_bytes = fs::read(&manifest_path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest is unreadable"))?;
    if raw_bytes.len() as u64 > MAX_CORPUS_MANIFEST_BYTES {
        return Err(CorpusLoadError::too_large("corpus manifest exceeds 1 MiB"));
    }
    let raw = String::from_utf8(raw_bytes)
        .map_err(|_| CorpusLoadError::bad("corpus manifest must be UTF-8 JSON"))?;
    let manifest: CorpusManifest = serde_json::from_str(&raw)
        .map_err(|error| CorpusLoadError::bad(format!("invalid corpus manifest: {error}")))?;
    if manifest.schema_version != CORPUS_SCHEMA_VERSION.as_str() {
        return Err(CorpusLoadError::bad(format!(
            "unsupported corpus schemaVersion {}; expected {}",
            manifest.schema_version, *CORPUS_SCHEMA_VERSION
        )));
    }
    if manifest.tasks.is_empty() || manifest.tasks.len() > MAX_TASKS {
        return Err(CorpusLoadError::bad(format!(
            "corpus must contain 1..={MAX_TASKS} tasks"
        )));
    }
    let profile = manifest.profile.map(validate_profile).transpose()?;
    let measurement = manifest
        .measurement
        .map(CorpusMeasurement::validate)
        .transpose()?;
    check_task_measurement(
        measurement.as_ref(),
        &manifest.suite_namespace,
        &manifest.tasks,
    )?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| CorpusLoadError::bad("corpus manifest has no parent directory"))?;
    let mut tasks = Vec::with_capacity(manifest.tasks.len());
    let mut sources = CorpusSourceCounts::default();
    let mut corpus_digest = rightkit_fs::digest::Sha256Hasher::new();
    digest_framed(&mut corpus_digest, b"manifest", raw.as_bytes());
    if let Some(measurement) = &measurement {
        let canonical = serde_json::to_string(measurement)
            .map_err(|_| CorpusLoadError::internal("measurement could not be serialised"))?;
        digest_framed(&mut corpus_digest, b"measurement", canonical.as_bytes());
    }
    let mut task_rows = Vec::new();
    let (mut heldout_count, mut development_count) = (0usize, 0usize);
    for task in manifest.tasks {
        let (instruction_path, workspace_root) = match task.source {
            CorpusTaskSource::Private {
                instruction_path,
                workspace_root,
            } => {
                sources.private += 1;
                (
                    resolve_corpus_file(root, &instruction_path, "private instruction")?,
                    resolve_corpus_dir(root, &workspace_root, "private workspace")?,
                )
            }
            CorpusTaskSource::Harbor {
                task_root,
                workspace_root,
            } => {
                sources.harbor += 1;
                let task_root = resolve_corpus_dir(root, &task_root, "Harbor task root")?;
                validate_harbor_task(&task_root)?;
                (
                    resolve_corpus_file(&task_root, "instruction.md", "Harbor instruction")?,
                    resolve_corpus_dir(root, &workspace_root, "Harbor workspace")?,
                )
            }
        };
        let (git_root, git_identity, tree_id) = clean_workspace_git_identity(&workspace_root)?;
        if let Some(expected_tree) = task.fixture_tree.as_deref() {
            if expected_tree != tree_id {
                return Err(CorpusLoadError::bad(format!(
                    "task {} fixtureTree does not match the workspace (expected {expected_tree}, found {})",
                    task.case_nonce,
                    if tree_id.is_empty() { "unavailable" } else { tree_id.as_str() }
                )));
            }
        }
        let prompt = read_bounded_utf8(
            &instruction_path,
            MAX_PROMPT_BYTES_PER_TASK as u64,
            "instruction",
        )?;
        let (expected_output, coding_verifier, expected_bytes) = match task.verifier {
            CorpusVerifier::ExactResponse {
                expected_output_path,
            } => {
                let expected_path =
                    resolve_corpus_file(root, &expected_output_path, "isolated verifier input")?;
                require_verifier_outside_git(&expected_path, &git_root)?;
                let expected = read_bounded_utf8(
                    &expected_path,
                    MAX_EXPECTED_OUTPUT_BYTES_PER_TASK as u64,
                    "verifier input",
                )?;
                (Some(expected.clone()), None, vec![expected])
            }
            CorpusVerifier::Command {
                command,
                required_artifacts,
            } => {
                let mut expected_bytes = Vec::with_capacity(required_artifacts.len());
                let mut artifacts = Vec::with_capacity(required_artifacts.len());
                for artifact in required_artifacts {
                    let expected_path = resolve_corpus_file(
                        root,
                        &artifact.expected_output_path,
                        "isolated verifier artifact input",
                    )?;
                    require_verifier_outside_git(&expected_path, &git_root)?;
                    let expected = read_bounded_utf8(
                        &expected_path,
                        MAX_EXPECTED_OUTPUT_BYTES_PER_TASK as u64,
                        "verifier artifact input",
                    )?;
                    expected_bytes.push(expected.clone());
                    artifacts.push(ExecutedRequiredArtifactBody {
                        name: artifact.name,
                        path: artifact.path,
                        expected_output: expected,
                    });
                }
                (
                    None,
                    Some(ExecutedCodingVerifierBody {
                        command,
                        required_artifacts: artifacts,
                    }),
                    expected_bytes,
                )
            }
        };
        digest_framed(&mut corpus_digest, b"case", task.case_nonce.as_bytes());
        digest_framed(&mut corpus_digest, b"instruction", prompt.as_bytes());
        digest_framed(&mut corpus_digest, b"git", git_identity.as_bytes());
        for expected in &expected_bytes {
            digest_framed(&mut corpus_digest, b"expected", expected.as_bytes());
        }
        let mut candidate_cost_limit_usd = task.candidate_cost_limit_usd;
        if let Some(measurement) = &measurement {
            let family = task.family.clone().unwrap_or_default();
            let fixture_tree = task.fixture_tree.clone().unwrap_or_default();
            digest_framed(&mut corpus_digest, b"family", family.as_bytes());
            digest_framed(&mut corpus_digest, b"fixtureTree", fixture_tree.as_bytes());
            let heldout = measurement.held_out_families.contains(&family);
            if heldout {
                heldout_count += 1;
            } else {
                development_count += 1;
            }
            task_rows.push(json!({
                "caseNonce": task.case_nonce,
                "family": family,
                "split": if heldout { "heldout" } else { "development" },
                "fixtureTree": fixture_tree,
            }));
            candidate_cost_limit_usd =
                Some(candidate_cost_limit_usd.unwrap_or(measurement.budgets.per_task_usd));
        }
        tasks.push(ExecutedBakeoffTaskBody {
            case_nonce: task.case_nonce,
            prompt,
            expected_output,
            coding_verifier,
            workspace_root: Some(workspace_root.to_string_lossy().to_string()),
            context_tokens: task.context_tokens,
            context_class: task.context_class,
            context_cohort: task.context_cohort,
            candidate_cost_limit_usd,
        });
    }
    let workspace_root = tasks
        .first()
        .and_then(|task| task.workspace_root.clone())
        .ok_or_else(|| CorpusLoadError::bad("corpus has no workspace"))?;
    // The seed enters the private-corpus secret through the suite namespace.
    let suite_namespace = match &measurement {
        Some(measurement) => format!("{}#seed={}", manifest.suite_namespace, measurement.seed),
        None => manifest.suite_namespace,
    };
    let measurement = measurement.map(|measurement| LoadedMeasurement {
        repetitions: measurement.repetitions,
        echo: json!({
            "identity": measurement.identity,
            "seed": measurement.seed,
            "repetitions": measurement.repetitions,
            "budgets": measurement.budgets,
            "verifierVersion": measurement.verifier_version,
            "splits": {"development": development_count, "heldout": heldout_count},
            "tasks": task_rows,
        }),
    });
    Ok(LoadedCorpus {
        body: ExecutedBakeoffBody {
            membrane_comparison: None,
            suite_namespace,
            workspace_root,
            candidate_roles: manifest.candidate_roles,
            baseline_role: manifest.baseline_role,
            tasks,
            gate: manifest.gate,
            timeout_seconds: manifest.timeout_seconds,
        },
        manifest_sha256: corpus_digest.finalize(),
        sources,
        manifest_path,
        profile,
        measurement,
    })
}

fn require_verifier_outside_git(path: &Path, git_root: &Path) -> Result<(), CorpusLoadError> {
    if path.starts_with(git_root) {
        return Err(CorpusLoadError::bad(
            "isolated verifier input must be outside the agent workspace Git repository",
        ));
    }
    Ok(())
}

/// Returns the Git root, the HEAD/subdirectory identity string and the Git tree id of the
/// workspace directory (empty when Git cannot resolve one).
fn clean_workspace_git_identity(
    workspace: &Path,
) -> Result<(PathBuf, String, String), CorpusLoadError> {
    let git = |args: &[&str]| {
        let spec = super::ManagedCommandSpec {
            program: "git".into(),
            argv: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: workspace.to_path_buf(),
            env: Vec::new(),
        };
        let result = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .map_err(|_| "runtime unavailable".to_string())
                .and_then(|runtime| runtime.block_on(super::run_managed_command(spec)))
        })
        .join()
        .map_err(|_| "managed Git worker panicked".to_string())
        .and_then(|result| result);
        result
            .map_err(|_| CorpusLoadError::bad("task workspace Git command is unavailable"))
            .and_then(|(status, stdout)| {
                (status == 0).then_some(stdout).ok_or_else(|| {
                    CorpusLoadError::bad("task workspace must belong to a Git repository")
                })
            })
    };
    let root_output = git(&["rev-parse", "--show-toplevel"])?;
    let git_root = fs::canonicalize(root_output.trim())
        .map_err(|_| CorpusLoadError::bad("task workspace Git root is unavailable"))?;
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(CorpusLoadError::bad(
            "task workspace Git repository must be clean so the evaluated snapshot is reproducible",
        ));
    }
    let head_output = git(&["rev-parse", "HEAD"])?;
    let head = head_output.trim().to_string();
    if head.len() != 40 || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CorpusLoadError::bad(
            "task workspace Git HEAD is unavailable",
        ));
    }
    let relative = workspace
        .strip_prefix(&git_root)
        .map_err(|_| CorpusLoadError::bad("task workspace is outside its Git repository"))?;
    let subdir = relative.to_string_lossy().replace('\\', "/");
    let tree_spec = if subdir.is_empty() || subdir == "." {
        "HEAD^{tree}".to_string()
    } else {
        format!("HEAD:{subdir}")
    };
    let tree_id = git(&["rev-parse", tree_spec.as_str()])
        .map(|output| output.trim().to_string())
        .unwrap_or_default();
    Ok((
        git_root,
        format!("head={head};subdir={}", relative.to_string_lossy()),
        tree_id,
    ))
}

fn digest_framed(digest: &mut rightkit_fs::digest::Sha256Hasher, label: &[u8], bytes: &[u8]) {
    digest.update(&(label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update(&(bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn validate_harbor_task(task_root: &Path) -> Result<(), CorpusLoadError> {
    let task_toml = resolve_corpus_file(task_root, "task.toml", "Harbor task.toml")?;
    let raw = read_bounded_utf8(&task_toml, MAX_CORPUS_MANIFEST_BYTES, "Harbor task.toml")?;
    let metadata: HarborTaskToml = toml::from_str(&raw)
        .map_err(|error| CorpusLoadError::bad(format!("invalid Harbor task.toml: {error}")))?;
    if !metadata.schema_version.starts_with("1.") || metadata.task.name.trim().is_empty() {
        return Err(CorpusLoadError::bad(
            "Harbor task.toml requires schema_version 1.x and a non-empty task.name",
        ));
    }
    Ok(())
}

fn resolve_corpus_file(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let path = resolve_corpus_member(root, raw, label)?;
    if !path.is_file() {
        return Err(CorpusLoadError::bad(format!("{label} must be a file")));
    }
    Ok(path)
}

fn resolve_corpus_dir(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let path = resolve_corpus_member(root, raw, label)?;
    if !path.is_dir() {
        return Err(CorpusLoadError::bad(format!("{label} must be a directory")));
    }
    Ok(path)
}

fn resolve_corpus_member(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let relative = Path::new(raw.trim());
    if raw.trim().is_empty() || relative.is_absolute() {
        return Err(CorpusLoadError::bad(format!(
            "{label} must be a non-empty relative path"
        )));
    }
    let root =
        fs::canonicalize(root).map_err(|_| CorpusLoadError::bad("corpus root is unavailable"))?;
    let resolved = fs::canonicalize(root.join(relative))
        .map_err(|_| CorpusLoadError::bad(format!("{label} does not exist")))?;
    if !resolved.starts_with(&root) {
        return Err(CorpusLoadError::bad(format!(
            "{label} escapes the corpus root"
        )));
    }
    Ok(resolved)
}

fn read_bounded_utf8(path: &Path, max: u64, label: &str) -> Result<String, CorpusLoadError> {
    let metadata = fs::metadata(path)
        .map_err(|_| CorpusLoadError::bad(format!("{label} metadata is unavailable")))?;
    if metadata.len() > max {
        return Err(CorpusLoadError::too_large(format!(
            "{label} exceeds {max} bytes"
        )));
    }
    let bytes =
        fs::read(path).map_err(|_| CorpusLoadError::bad(format!("{label} is unreadable")))?;
    if bytes.len() as u64 > max {
        return Err(CorpusLoadError::too_large(format!(
            "{label} exceeds {max} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| CorpusLoadError::bad(format!("{label} must be UTF-8")))
}
