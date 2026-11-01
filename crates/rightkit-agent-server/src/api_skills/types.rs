#[derive(Clone)]
struct SkillItem {
    slug: String,
    name: String,
    description: String,
    trigger: String,
    enabled: bool,
    lifecycle: SkillLifecycle,
    version: Option<String>,
    author: Option<String>,
    compatibility: Vec<String>,
    platforms: Vec<String>,
    required_environment_variables: Vec<String>,
    metadata: rightkit_agent_host_traits::skill_host::manifest::SkillPackageMetadata,
    category: SkillCategory,
    priority: i32,
    disable_model_invocation: bool,
    tags: Vec<String>,
    path_affinity: Vec<String>,
    effects: SkillEffects,
    path: PathBuf,
    directory: PathBuf,
    kind: String,
    valid: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallSkillRequest {
    slug: String,
    content: String,
    name: Option<String>,
    description: Option<String>,
    trigger: Option<String>,
    enabled: Option<bool>,
    category: Option<SkillCategory>,
    priority: Option<i32>,
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillProvenanceReviewRequest {
    source_url: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillProvenanceFinding {
    severity: &'static str,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillProvenanceReviewResponse {
    ok: bool,
    source_url: String,
    normalized_source_url: Option<String>,
    content_hash: String,
    manifest_name: Option<String>,
    manifest_description: Option<String>,
    recommended_lifecycle: String,
    findings: Vec<SkillProvenanceFinding>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSkillRequest {
    content: String,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillLifecycleRequest {
    reason: Option<String>,
    operator_override: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillAuditEntry {
    slug: String,
    action: String,
    lifecycle: SkillLifecycle,
    actor: String,
    reason: Option<String>,
    operator_override: bool,
    timestamp_unix_secs: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillGovernanceStore {
    proposals: Vec<SkillProposalRecord>,
    versions: Vec<SkillVersionRecord>,
    benchmark_verdicts: Vec<SkillBenchmarkVerdict>,
    audit_log: Vec<SkillGovernanceAuditEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillProposalRecord {
    id: String,
    slug: String,
    title: String,
    summary: String,
    proposed_content: String,
    content_hash: String,
    status: SkillProposalStatus,
    source_session_id: Option<String>,
    source_task_id: Option<String>,
    created_at_unix_secs: u64,
    updated_at_unix_secs: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum SkillProposalStatus {
    Proposed,
    Quarantined,
    Benchmarked,
    Active,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillVersionRecord {
    id: String,
    slug: String,
    version: u32,
    content_hash: String,
    parent_hash: Option<String>,
    proposal_id: Option<String>,
    change_reason: String,
    created_at_unix_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillBenchmarkVerdict {
    id: String,
    slug: String,
    proposal_id: Option<String>,
    benchmark_id: String,
    gate_mode: rightkit_agent_host_traits::skill_host::SkillGateMode,
    score_continuous: f64,
    passed: bool,
    verdict_hash: String,
    created_at_unix_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillGovernanceAuditEntry {
    id: String,
    target_type: String,
    target_id: String,
    slug: Option<String>,
    action: String,
    reason: Option<String>,
    actor: String,
    operator_override: bool,
    timestamp_unix_secs: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSkillProposalRequest {
    id: Option<String>,
    slug: String,
    title: Option<String>,
    summary: String,
    content: String,
    source_session_id: Option<String>,
    source_task_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillProposalQuarantineRequest {
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillBenchmarkRequest {
    slug: Option<String>,
    benchmark_id: String,
    gate_mode: Option<rightkit_agent_host_traits::skill_host::SkillGateMode>,
    score_continuous: Option<f64>,
    passed: bool,
    verdict: Option<serde_json::Value>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillProposalBenchmarkRunRequest {
    /// `role` uses `/runtime/benchmark`; `terminalBench` uses `/runtime/terminal-bench/run`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    repeats: Option<u32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    benchmark_id: Option<String>,
    #[serde(default)]
    gate_mode: Option<rightkit_agent_host_traits::skill_host::SkillGateMode>,
    #[serde(default)]
    threshold: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}
