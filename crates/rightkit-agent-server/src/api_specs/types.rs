#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSpecsQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSpecRequest {
    workspace_root: String,
    title: Option<String>,
    description: String,
    branch: Option<String>,
    #[serde(default)]
    user_stories: Vec<UserStoryInput>,
    #[serde(default)]
    functional_requirements: Vec<RequirementInput>,
    #[serde(default)]
    success_criteria: Vec<SuccessCriterionInput>,
    #[serde(default)]
    assumptions: Vec<String>,
    #[serde(default)]
    edge_cases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchSpecRequest {
    title: Option<String>,
    status: Option<SpecStatus>,
    #[serde(default)]
    assumptions: Option<Vec<String>>,
    #[serde(default)]
    edge_cases: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStoryInput {
    id: Option<String>,
    priority: Option<u8>,
    title: String,
    narrative: String,
    independent_test: String,
    #[serde(default)]
    acceptance_scenarios: Vec<AcceptanceScenario>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequirementInput {
    id: Option<String>,
    text: String,
    source_story_id: Option<String>,
    #[serde(default)]
    needs_clarification: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuccessCriterionInput {
    id: Option<String>,
    text: String,
    #[serde(default = "default_true")]
    buildable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddTasksRequest {
    #[serde(default)]
    tasks: Vec<TaskInput>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PlanSpecRequest {
    constraints: Option<String>,
    #[serde(default = "default_true")]
    include_verification: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ChecklistSpecRequest {
    focus: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ImplementSpecRequest {
    title: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConstitutionDecisionRequest {
    id: Option<String>,
    source_path: Option<String>,
    source_sha256: Option<String>,
    rule: String,
    verdict: ConstitutionDecisionVerdict,
    rationale: String,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskInput {
    id: Option<String>,
    story_id: Option<String>,
    #[serde(default)]
    requirement_ids: Vec<String>,
    #[serde(default)]
    success_criterion_ids: Vec<String>,
    foundation_phase: Option<String>,
    description: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    parallelizable: bool,
    status: Option<TaskStatus>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecEnvelope {
    spec: FeatureSpec,
    tasks: Vec<FeatureTask>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecClarification {
    id: String,
    source_kind: rightkit_evidence::specs::TraceKind,
    source_id: String,
    question: String,
    required: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecClarifyResponse {
    spec_id: String,
    questions: Vec<SpecClarification>,
    blocking: bool,
    next_action: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecPlanResponse {
    spec: FeatureSpec,
    tasks: Vec<FeatureTask>,
    created_task_count: usize,
    skipped_existing_count: usize,
    analysis: rightkit_evidence::specs::SpecAnalysisRun,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecChecklistItem {
    id: String,
    label: String,
    status: String,
    source_kind: rightkit_evidence::specs::TraceKind,
    source_id: String,
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecChecklistResponse {
    spec_id: String,
    focus: Option<String>,
    ready: bool,
    blocker_count: usize,
    warning_count: usize,
    items: Vec<SpecChecklistItem>,
    analysis: rightkit_evidence::specs::SpecAnalysisRun,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecImplementResponse {
    spec_id: String,
    session_id: Option<String>,
    accepted: bool,
    dry_run: bool,
    prompt: String,
    analysis: rightkit_evidence::specs::SpecAnalysisRun,
}
