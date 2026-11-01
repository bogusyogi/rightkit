//! Spec-governed development artifacts and coverage analysis.
//!
//! The daemon owns persistence and HTTP routing. This crate is intentionally
//! pure data plus deterministic analysis so normal chat-first sessions stay
//! unaffected unless a caller explicitly creates or binds a feature spec.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeatureSpec {
    pub id: String,
    pub workspace_root: String,
    pub branch: Option<String>,
    pub title: String,
    pub status: SpecStatus,
    pub source_prompt_hash: String,
    #[serde(default)]
    pub user_stories: Vec<UserStory>,
    #[serde(default)]
    pub functional_requirements: Vec<Requirement>,
    #[serde(default)]
    pub success_criteria: Vec<SuccessCriterion>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub edge_cases: Vec<String>,
    #[serde(default)]
    pub constitution_sources: Vec<ConstitutionSource>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SpecStatus {
    Draft,
    Planned,
    Implementing,
    Done,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConstitutionSource {
    pub scope: String,
    pub path: String,
    pub body_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConstitutionDecisionVerdict {
    Compliant,
    Violation,
    Overridden,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConstitutionDecision {
    pub id: String,
    pub spec_id: String,
    pub source_path: String,
    pub source_sha256: String,
    pub rule: String,
    pub verdict: ConstitutionDecisionVerdict,
    pub rationale: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UserStory {
    pub id: String,
    pub priority: u8,
    pub title: String,
    pub narrative: String,
    pub independent_test: String,
    #[serde(default)]
    pub acceptance_scenarios: Vec<AcceptanceScenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceScenario {
    pub id: String,
    pub given: String,
    pub when: String,
    pub then: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    pub id: String,
    pub text: String,
    pub source_story_id: Option<String>,
    pub needs_clarification: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SuccessCriterion {
    pub id: String,
    pub text: String,
    pub buildable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeatureTask {
    pub id: String,
    pub spec_id: String,
    pub story_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub success_criterion_ids: Vec<String>,
    pub foundation_phase: Option<String>,
    pub description: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub parallelizable: bool,
    pub status: TaskStatus,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TraceLink {
    pub id: String,
    pub spec_id: String,
    pub from_kind: TraceKind,
    pub from_id: String,
    pub to_kind: TraceKind,
    pub to_id: String,
    pub confidence: f32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpecEvidence {
    pub id: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub evidence_id: Option<String>,
    pub path: String,
    pub kind: String,
    pub content_hash: String,
    pub shippable: bool,
    pub producer: String,
    pub provenance: String,
    pub verdict_passed: Option<bool>,
    pub verdict_summary: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TraceKind {
    Story,
    Requirement,
    SuccessCriterion,
    Task,
    Evidence,
    Constitution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisStatus {
    Pass,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpecAnalysisRun {
    pub id: String,
    pub spec_id: String,
    pub status: AnalysisStatus,
    #[serde(default)]
    pub findings: Vec<SpecFinding>,
    pub coverage: SpecCoverage,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpecFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub kind: String,
    pub message: String,
    pub source_kind: TraceKind,
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum FindingSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpecCoverage {
    pub requirement_count: usize,
    pub requirements_with_tasks: usize,
    pub success_criteria_count: usize,
    pub buildable_criteria_with_evidence: usize,
    pub task_count: usize,
    pub tasks_with_mapping: usize,
    pub independently_testable_stories: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpecTrace {
    pub spec_id: String,
    #[serde(default)]
    pub links: Vec<TraceLink>,
    #[serde(default)]
    pub evidence: Vec<SpecEvidence>,
    pub latest_analysis: Option<SpecAnalysisRun>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecStore {
    #[serde(default)]
    specs: BTreeMap<String, FeatureSpec>,
    #[serde(default)]
    tasks: BTreeMap<String, Vec<FeatureTask>>,
    #[serde(default)]
    analysis_runs: BTreeMap<String, Vec<SpecAnalysisRun>>,
    #[serde(default)]
    constitution_decisions: BTreeMap<String, Vec<ConstitutionDecision>>,
}

impl SpecStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_spec(&mut self, spec: FeatureSpec) {
        self.specs.insert(spec.id.clone(), spec);
    }

    pub fn list_specs(&self, workspace_root: Option<&str>) -> Vec<FeatureSpec> {
        self.specs
            .values()
            .filter(|spec| {
                workspace_root
                    .map(|root| spec.workspace_root == root)
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    pub fn spec(&self, id: &str) -> Option<FeatureSpec> {
        self.specs.get(id).cloned()
    }

    pub fn spec_mut(&mut self, id: &str) -> Option<&mut FeatureSpec> {
        self.specs.get_mut(id)
    }

    pub fn add_tasks(&mut self, spec_id: &str, mut tasks: Vec<FeatureTask>) {
        self.tasks
            .entry(spec_id.to_string())
            .or_default()
            .append(&mut tasks);
    }

    pub fn tasks(&self, spec_id: &str) -> Vec<FeatureTask> {
        self.tasks.get(spec_id).cloned().unwrap_or_default()
    }

    pub fn record_analysis(&mut self, run: SpecAnalysisRun) {
        self.analysis_runs
            .entry(run.spec_id.clone())
            .or_default()
            .push(run);
    }

    pub fn latest_analysis(&self, spec_id: &str) -> Option<SpecAnalysisRun> {
        self.analysis_runs
            .get(spec_id)
            .and_then(|runs| runs.last())
            .cloned()
    }

    pub fn add_constitution_decision(&mut self, decision: ConstitutionDecision) {
        let rows = self
            .constitution_decisions
            .entry(decision.spec_id.clone())
            .or_default();
        if let Some(existing) = rows.iter_mut().find(|row| row.id == decision.id) {
            *existing = decision;
        } else {
            rows.push(decision);
        }
    }

    pub fn constitution_decisions(&self, spec_id: &str) -> Vec<ConstitutionDecision> {
        self.constitution_decisions
            .get(spec_id)
            .cloned()
            .unwrap_or_default()
    }
}

pub fn analyze_spec(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    run_id: impl Into<String>,
    created_at: impl Into<String>,
) -> SpecAnalysisRun {
    analyze_spec_with_constitution_decisions(spec, tasks, &[], run_id, created_at)
}

pub fn analyze_spec_with_constitution_decisions(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    constitution_decisions: &[ConstitutionDecision],
    run_id: impl Into<String>,
    created_at: impl Into<String>,
) -> SpecAnalysisRun {
    let requirement_ids = spec
        .functional_requirements
        .iter()
        .map(|req| req.id.as_str())
        .collect::<BTreeSet<_>>();
    let story_ids = spec
        .user_stories
        .iter()
        .map(|story| story.id.as_str())
        .collect::<BTreeSet<_>>();
    let criterion_ids = spec
        .success_criteria
        .iter()
        .map(|criterion| criterion.id.as_str())
        .collect::<BTreeSet<_>>();

    let requirements_with_tasks = spec
        .functional_requirements
        .iter()
        .filter(|req| {
            tasks
                .iter()
                .any(|task| task.requirement_ids.iter().any(|id| id == &req.id))
        })
        .count();

    let buildable_criteria_with_evidence = spec
        .success_criteria
        .iter()
        .filter(|criterion| criterion.buildable)
        .filter(|criterion| {
            tasks.iter().any(|task| {
                task.success_criterion_ids
                    .iter()
                    .any(|id| id == &criterion.id)
                    && !task.evidence_ids.is_empty()
            })
        })
        .count();

    let tasks_with_mapping = tasks.iter().filter(|task| task_has_mapping(task)).count();

    let independently_testable_stories = spec
        .user_stories
        .iter()
        .filter(|story| !is_placeholder_text(&story.independent_test))
        .count();

    let coverage = SpecCoverage {
        requirement_count: spec.functional_requirements.len(),
        requirements_with_tasks,
        success_criteria_count: spec.success_criteria.len(),
        buildable_criteria_with_evidence,
        task_count: tasks.len(),
        tasks_with_mapping,
        independently_testable_stories,
    };

    let mut findings = Vec::new();

    for req in &spec.functional_requirements {
        if !tasks
            .iter()
            .any(|task| task.requirement_ids.iter().any(|id| id == &req.id))
        {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Critical,
                "requirement_without_task",
                format!("Requirement {} has no mapped task.", req.id),
                TraceKind::Requirement,
                req.id.clone(),
            ));
        }
        if req.needs_clarification || is_placeholder_text(&req.text) {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Warning,
                "requirement_needs_clarification",
                format!("Requirement {} still needs clarification.", req.id),
                TraceKind::Requirement,
                req.id.clone(),
            ));
        }
        if let Some(story_id) = &req.source_story_id {
            if !story_ids.contains(story_id.as_str()) {
                findings.push(finding(
                    findings.len(),
                    FindingSeverity::Warning,
                    "unknown_story_reference",
                    format!(
                        "Requirement {} references missing story {}.",
                        req.id, story_id
                    ),
                    TraceKind::Requirement,
                    req.id.clone(),
                ));
            }
        }
    }

    for criterion in spec
        .success_criteria
        .iter()
        .filter(|criterion| criterion.buildable)
    {
        let has_evidence = tasks.iter().any(|task| {
            task.success_criterion_ids
                .iter()
                .any(|id| id == &criterion.id)
                && !task.evidence_ids.is_empty()
        });
        if !has_evidence {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Warning,
                "success_criterion_without_evidence",
                format!(
                    "Buildable success criterion {} has no evidence-bearing task.",
                    criterion.id
                ),
                TraceKind::SuccessCriterion,
                criterion.id.clone(),
            ));
        }
    }

    for story in &spec.user_stories {
        if is_placeholder_text(&story.independent_test) {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Warning,
                "story_not_independently_testable",
                format!("User story {} lacks an independent test.", story.id),
                TraceKind::Story,
                story.id.clone(),
            ));
        }
    }

    for task in tasks {
        if !task_has_mapping(task) {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Critical,
                "task_without_mapping",
                format!(
                    "Task {} is not mapped to a story, requirement, success criterion, or foundation phase.",
                    task.id
                ),
                TraceKind::Task,
                task.id.clone(),
            ));
        }
        for req_id in &task.requirement_ids {
            if !requirement_ids.contains(req_id.as_str()) {
                findings.push(finding(
                    findings.len(),
                    FindingSeverity::Warning,
                    "unknown_requirement_reference",
                    format!(
                        "Task {} references missing requirement {}.",
                        task.id, req_id
                    ),
                    TraceKind::Task,
                    task.id.clone(),
                ));
            }
        }
        for criterion_id in &task.success_criterion_ids {
            if !criterion_ids.contains(criterion_id.as_str()) {
                findings.push(finding(
                    findings.len(),
                    FindingSeverity::Warning,
                    "unknown_success_criterion_reference",
                    format!(
                        "Task {} references missing success criterion {}.",
                        task.id, criterion_id
                    ),
                    TraceKind::Task,
                    task.id.clone(),
                ));
            }
        }
        if task
            .paths
            .iter()
            .any(|path| path.starts_with("..") || path.contains("/../") || path.contains("\\..\\"))
        {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Warning,
                "task_path_outside_structure",
                format!(
                    "Task {} references a path outside the declared work tree.",
                    task.id
                ),
                TraceKind::Task,
                task.id.clone(),
            ));
        }
    }

    for decision in constitution_decisions {
        let source_matches = spec.constitution_sources.iter().any(|source| {
            source.path == decision.source_path && source.body_sha256 == decision.source_sha256
        });
        if !source_matches {
            findings.push(finding(
                findings.len(),
                FindingSeverity::Warning,
                "constitution_decision_source_mismatch",
                format!(
                    "Constitution decision {} references a source that is not attached to this spec.",
                    decision.id
                ),
                TraceKind::Constitution,
                decision.id.clone(),
            ));
        }
        match decision.verdict {
            ConstitutionDecisionVerdict::Violation => findings.push(finding(
                findings.len(),
                FindingSeverity::Critical,
                "constitution_violation",
                format!(
                    "Constitution decision {} records an unresolved violation: {}.",
                    decision.id, decision.rule
                ),
                TraceKind::Constitution,
                decision.id.clone(),
            )),
            ConstitutionDecisionVerdict::Overridden => {
                if decision.evidence_ids.is_empty() || decision.rationale.trim().is_empty() {
                    findings.push(finding(
                        findings.len(),
                        FindingSeverity::Critical,
                        "constitution_override_without_evidence",
                        format!(
                            "Constitution override {} must include rationale and evidence ids.",
                            decision.id
                        ),
                        TraceKind::Constitution,
                        decision.id.clone(),
                    ));
                }
            }
            ConstitutionDecisionVerdict::Compliant => {}
        }
    }

    let status = if findings
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Critical)
    {
        AnalysisStatus::Block
    } else if findings
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Warning)
    {
        AnalysisStatus::Warn
    } else {
        AnalysisStatus::Pass
    };

    SpecAnalysisRun {
        id: run_id.into(),
        spec_id: spec.id.clone(),
        status,
        findings,
        coverage,
        created_at: created_at.into(),
    }
}

pub fn build_trace(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    latest_analysis: Option<SpecAnalysisRun>,
    created_at: impl AsRef<str>,
) -> SpecTrace {
    build_trace_with_evidence(spec, tasks, latest_analysis, created_at, Vec::new())
}

pub fn build_trace_with_evidence(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    latest_analysis: Option<SpecAnalysisRun>,
    created_at: impl AsRef<str>,
    evidence: Vec<SpecEvidence>,
) -> SpecTrace {
    let created_at = created_at.as_ref();
    let mut links = Vec::new();

    for requirement in &spec.functional_requirements {
        if let Some(story_id) = &requirement.source_story_id {
            links.push(trace_link(
                &spec.id,
                TraceKind::Story,
                story_id,
                TraceKind::Requirement,
                &requirement.id,
                links.len(),
                created_at,
            ));
        }
    }

    for task in tasks {
        if let Some(story_id) = &task.story_id {
            links.push(trace_link(
                &spec.id,
                TraceKind::Story,
                story_id,
                TraceKind::Task,
                &task.id,
                links.len(),
                created_at,
            ));
        }
        for req_id in &task.requirement_ids {
            links.push(trace_link(
                &spec.id,
                TraceKind::Requirement,
                req_id,
                TraceKind::Task,
                &task.id,
                links.len(),
                created_at,
            ));
        }
        for criterion_id in &task.success_criterion_ids {
            links.push(trace_link(
                &spec.id,
                TraceKind::SuccessCriterion,
                criterion_id,
                TraceKind::Task,
                &task.id,
                links.len(),
                created_at,
            ));
        }
        for evidence_id in &task.evidence_ids {
            links.push(trace_link(
                &spec.id,
                TraceKind::Task,
                &task.id,
                TraceKind::Evidence,
                evidence_id,
                links.len(),
                created_at,
            ));
        }
    }

    for item in &evidence {
        if let Some(task_id) = &item.task_id {
            links.push(trace_link(
                &spec.id,
                TraceKind::Task,
                task_id,
                TraceKind::Evidence,
                &item.id,
                links.len(),
                created_at,
            ));
        }
    }

    for source in &spec.constitution_sources {
        links.push(trace_link(
            &spec.id,
            TraceKind::Constitution,
            &source.path,
            TraceKind::Story,
            &spec.id,
            links.len(),
            created_at,
        ));
    }

    SpecTrace {
        spec_id: spec.id.clone(),
        links,
        evidence,
        latest_analysis,
    }
}

pub fn is_placeholder_text(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.is_empty()
        || matches!(
            normalized.as_str(),
            "todo" | "tbd" | "placeholder" | "n/a" | "na" | "unknown" | "fix later"
        )
        || normalized.contains("lorem ipsum")
}

pub fn spec_has_placeholder_heavy_content(spec: &FeatureSpec) -> bool {
    let mut fields = vec![spec.title.as_str()];
    fields.extend(
        spec.functional_requirements
            .iter()
            .map(|req| req.text.as_str()),
    );
    fields.extend(
        spec.success_criteria
            .iter()
            .map(|criterion| criterion.text.as_str()),
    );
    fields.extend(spec.user_stories.iter().flat_map(|story| {
        [
            story.title.as_str(),
            story.narrative.as_str(),
            story.independent_test.as_str(),
        ]
    }));

    let checked = fields.len().max(1);
    let placeholders = fields
        .iter()
        .filter(|field| is_placeholder_text(field))
        .count();
    placeholders > 0 && placeholders * 2 >= checked
}

fn task_has_mapping(task: &FeatureTask) -> bool {
    task.story_id
        .as_ref()
        .is_some_and(|id| !id.trim().is_empty())
        || !task.requirement_ids.is_empty()
        || !task.success_criterion_ids.is_empty()
        || task
            .foundation_phase
            .as_ref()
            .is_some_and(|phase| !phase.trim().is_empty())
}

fn finding(
    index: usize,
    severity: FindingSeverity,
    kind: impl Into<String>,
    message: impl Into<String>,
    source_kind: TraceKind,
    source_id: impl Into<String>,
) -> SpecFinding {
    SpecFinding {
        id: format!("F{:03}", index + 1),
        severity,
        kind: kind.into(),
        message: message.into(),
        source_kind,
        source_id: source_id.into(),
    }
}

fn trace_link(
    spec_id: &str,
    from_kind: TraceKind,
    from_id: &str,
    to_kind: TraceKind,
    to_id: &str,
    index: usize,
    created_at: &str,
) -> TraceLink {
    TraceLink {
        id: format!("TL{:03}", index + 1),
        spec_id: spec_id.to_string(),
        from_kind,
        from_id: from_id.to_string(),
        to_kind,
        to_id: to_id.to_string(),
        confidence: 1.0,
        created_at: created_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_blocks_unmapped_requirement_and_task() {
        let spec = sample_spec();
        let tasks = vec![FeatureTask {
            id: "T001".to_string(),
            spec_id: spec.id.clone(),
            story_id: None,
            requirement_ids: Vec::new(),
            success_criterion_ids: Vec::new(),
            foundation_phase: None,
            description: "Implement the thing".to_string(),
            paths: Vec::new(),
            parallelizable: false,
            status: TaskStatus::Todo,
            evidence_ids: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        }];

        let run = analyze_spec(&spec, &tasks, "run-1", "2026-06-08T00:00:00Z");

        assert_eq!(run.status, AnalysisStatus::Block);
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "requirement_without_task"));
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "task_without_mapping"));
    }

    #[test]
    fn trace_links_requirement_to_task_to_evidence() {
        let spec = sample_spec();
        let tasks = vec![FeatureTask {
            id: "T001".to_string(),
            spec_id: spec.id.clone(),
            story_id: Some("US1".to_string()),
            requirement_ids: vec!["FR-001".to_string()],
            success_criterion_ids: vec!["SC-001".to_string()],
            foundation_phase: None,
            description: "Implement and test auth".to_string(),
            paths: vec!["engine/src/auth.rs".to_string()],
            parallelizable: true,
            status: TaskStatus::Done,
            evidence_ids: vec!["test:cargo-test-auth".to_string()],
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        }];

        let trace = build_trace(&spec, &tasks, None, "2026-06-08T00:00:00Z");

        assert!(trace.links.iter().any(|link| {
            link.from_kind == TraceKind::Requirement
                && link.from_id == "FR-001"
                && link.to_kind == TraceKind::Task
                && link.to_id == "T001"
        }));
        assert!(trace.links.iter().any(|link| {
            link.from_kind == TraceKind::Task
                && link.from_id == "T001"
                && link.to_kind == TraceKind::Evidence
                && link.to_id == "test:cargo-test-auth"
        }));
    }

    #[test]
    fn placeholder_heavy_specs_are_detected() {
        let mut spec = sample_spec();
        spec.title = "TBD".to_string();
        spec.functional_requirements[0].text = "TODO".to_string();
        spec.success_criteria[0].text = "placeholder".to_string();

        assert!(spec_has_placeholder_heavy_content(&spec));
    }

    #[test]
    fn constitution_violation_blocks_until_evidence_backed_override() {
        let mut spec = sample_spec();
        spec.constitution_sources.push(ConstitutionSource {
            scope: "workspace".to_string(),
            path: "D:/work/.rightkit/constitution.md".to_string(),
            body_sha256: "sha256:abc".to_string(),
        });
        let tasks = vec![mapped_task(&spec)];
        let violation = ConstitutionDecision {
            id: "cd-1".to_string(),
            spec_id: spec.id.clone(),
            source_path: "D:/work/.rightkit/constitution.md".to_string(),
            source_sha256: "sha256:abc".to_string(),
            rule: "All user-facing changes require tests.".to_string(),
            verdict: ConstitutionDecisionVerdict::Violation,
            rationale: "No test evidence was supplied.".to_string(),
            evidence_ids: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        };

        let blocked = analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            std::slice::from_ref(&violation),
            "run-violation",
            "2026-06-08T00:00:00Z",
        );

        assert_eq!(blocked.status, AnalysisStatus::Block);
        assert!(blocked
            .findings
            .iter()
            .any(|finding| finding.kind == "constitution_violation"));

        let mut override_decision = violation;
        override_decision.verdict = ConstitutionDecisionVerdict::Overridden;
        override_decision.rationale = "Risk accepted after evidence review.".to_string();
        override_decision.evidence_ids = vec!["EV-001".to_string()];
        let passed = analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &[override_decision],
            "run-override",
            "2026-06-08T00:00:00Z",
        );

        assert_ne!(passed.status, AnalysisStatus::Block);
        assert!(!passed
            .findings
            .iter()
            .any(|finding| finding.kind == "constitution_violation"));
    }

    fn sample_spec() -> FeatureSpec {
        FeatureSpec {
            id: "spec-1".to_string(),
            workspace_root: "D:/work".to_string(),
            branch: None,
            title: "Add auth".to_string(),
            status: SpecStatus::Draft,
            source_prompt_hash: "hash".to_string(),
            user_stories: vec![UserStory {
                id: "US1".to_string(),
                priority: 1,
                title: "Sign in".to_string(),
                narrative: "As a user I can sign in.".to_string(),
                independent_test: "Submit credentials and see a session.".to_string(),
                acceptance_scenarios: Vec::new(),
            }],
            functional_requirements: vec![Requirement {
                id: "FR-001".to_string(),
                text: "Users can authenticate with stored credentials.".to_string(),
                source_story_id: Some("US1".to_string()),
                needs_clarification: false,
            }],
            success_criteria: vec![SuccessCriterion {
                id: "SC-001".to_string(),
                text: "Auth tests pass.".to_string(),
                buildable: true,
            }],
            assumptions: Vec::new(),
            edge_cases: Vec::new(),
            constitution_sources: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        }
    }

    fn mapped_task(spec: &FeatureSpec) -> FeatureTask {
        FeatureTask {
            id: "T001".to_string(),
            spec_id: spec.id.clone(),
            story_id: Some("US1".to_string()),
            requirement_ids: vec!["FR-001".to_string()],
            success_criterion_ids: vec!["SC-001".to_string()],
            foundation_phase: None,
            description: "Implement and test auth".to_string(),
            paths: Vec::new(),
            parallelizable: true,
            status: TaskStatus::Done,
            evidence_ids: vec!["EV-001".to_string()],
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        }
    }
}
