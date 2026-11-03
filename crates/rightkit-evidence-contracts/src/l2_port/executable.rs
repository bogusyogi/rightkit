//! Rust port of `src/packages/contracts/executable.mjs` (L2 packet, EC-C).
//!
//! The JS validator composes draft 2020-12 structural validation (via
//! `lib/qualification/schema-validator.mjs`) with executability rules JSON
//! Schema cannot express. This port keeps the two concerns separate the
//! same way:
//!   - Structural shape is enforced by `serde(deny_unknown_fields)` plus
//!     required fields on `ExecutionContract` and its nested types, which is
//!     the Rust-native equivalent of the draft 2020-12 structural pass for
//!     this schema (`execution-contract-v1.schema.json`). A JSON payload
//!     that fails to deserialize is the port's structural-error signal.
//!   - `run_executability_checks` reproduces every rule the JS
//!     `runExecutabilityChecks` enforces beyond structural shape:
//!       * openQuestions must be empty
//!       * EXACT artifacts must carry non-null string content
//!       * BOUNDED artifacts must carry non-empty locked[] and freedom[]
//!       * shared-mutable-resource dependency edges must carry a resourceKey
//!       * contract-wide ID uniqueness across R-/D-/I-/AC-/NG-/tasks
//!
//! Error `code`/`path`/`message` values and sort order match the JS
//! implementation so golden fixtures translate directly.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IdStatement {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub alternatives_considered: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub verifies_decisions: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactUnit {
    pub id: String,
    pub path: String,
    pub latitude: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub locked: Option<Vec<String>>,
    #[serde(default)]
    pub freedom: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Artifacts {
    pub exact: Vec<ArtifactUnit>,
    pub bounded: Vec<ArtifactUnit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub reason: String,
    #[serde(default)]
    pub resource_key: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OpenQuestion {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub blocks_artifacts: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scope {
    #[serde(rename = "own")]
    pub own: Vec<String>,
    pub read: Vec<String>,
    pub forbidden: Vec<String>,
}

/// Minimal structural port of `execution-contract-v1.schema.json`'s required
/// shape. Fields not needed by executability checks are still modeled (as
/// `serde_json::Value` where a nested shape isn't checked here) so
/// `deny_unknown_fields` gives the same "unknown property" structural
/// rejection as the JSON Schema's `additionalProperties: false`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionContract {
    pub schema_version: u32,
    pub kind: String,
    pub contract_id: String,
    pub version: u32,
    pub source_revision: String,
    #[serde(default)]
    pub advisory_profile: Option<serde_json::Value>,
    pub objective: String,
    pub current_state: String,
    pub desired_state: String,
    pub requirements: Vec<IdStatement>,
    pub decisions: Vec<IdStatement>,
    pub invariants: Vec<IdStatement>,
    pub non_goals: Vec<IdStatement>,
    pub scope: Scope,
    pub artifacts: Artifacts,
    pub tasks: Vec<String>,
    pub dependencies: Vec<DependencyEdge>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub declared_checks: Vec<String>,
    pub evidence_requirements: Vec<String>,
    pub authorized_effect_classes: Vec<String>,
    pub repair_latitude: Vec<String>,
    pub stop_conditions: Vec<String>,
    pub escalation_conditions: Vec<String>,
    pub rollback: Vec<String>,
    pub open_questions: Vec<OpenQuestion>,
    #[serde(default)]
    pub budget: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutableContractError {
    pub code: String,
    pub path: String,
    pub message: String,
}

fn err(code: &str, path: impl Into<String>, message: impl Into<String>) -> ExecutableContractError {
    ExecutableContractError {
        code: code.into(),
        path: path.into(),
        message: message.into(),
    }
}

struct IdEntry {
    id: String,
    r#where: &'static str,
}

fn collect_ids(contract: &ExecutionContract) -> Vec<IdEntry> {
    let mut ids = Vec::new();
    let buckets: [(&'static str, &[IdStatement]); 4] = [
        ("requirements", &contract.requirements),
        ("decisions", &contract.decisions),
        ("invariants", &contract.invariants),
        ("nonGoals", &contract.non_goals),
    ];
    for (label, list) in buckets {
        for entry in list {
            ids.push(IdEntry {
                id: entry.id.clone(),
                r#where: label,
            });
        }
    }
    for entry in &contract.acceptance_criteria {
        ids.push(IdEntry {
            id: entry.id.clone(),
            r#where: "acceptanceCriteria",
        });
    }
    for entry in &contract.open_questions {
        ids.push(IdEntry {
            id: entry.id.clone(),
            r#where: "openQuestions",
        });
    }
    for task_id in &contract.tasks {
        ids.push(IdEntry {
            id: task_id.clone(),
            r#where: "tasks",
        });
    }
    ids
}

/// Port of `runExecutabilityChecks`.
pub fn run_executability_checks(contract: &ExecutionContract) -> Vec<ExecutableContractError> {
    let mut errors = Vec::new();

    if !contract.open_questions.is_empty() {
        errors.push(err(
            "EXEC_OPEN_QUESTIONS_NONEMPTY",
            "$.openQuestions",
            "execution contract has open questions",
        ));
    }

    for (latitude, list) in [
        ("exact", &contract.artifacts.exact),
        ("bounded", &contract.artifacts.bounded),
    ] {
        for (index, unit) in list.iter().enumerate() {
            let unit_path = format!("$.artifacts.{latitude}[{index}]");
            let id_label = if unit.id.is_empty() {
                unit_path.clone()
            } else {
                unit.id.clone()
            };
            if latitude == "exact" {
                match &unit.content {
                    None => errors.push(err(
                        "EXEC_EXACT_CONTENT_NULL",
                        format!("{unit_path}.content"),
                        format!("EXACT artifact {id_label} requires content"),
                    )),
                    Some(serde_json::Value::Null) => errors.push(err(
                        "EXEC_EXACT_CONTENT_NULL",
                        format!("{unit_path}.content"),
                        format!("EXACT artifact {id_label} requires content"),
                    )),
                    Some(serde_json::Value::String(_)) => {}
                    Some(other) => {
                        let ty = match other {
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "boolean",
                            serde_json::Value::Array(_) => "object",
                            serde_json::Value::Object(_) => "object",
                            _ => "unknown",
                        };
                        errors.push(err(
                            "EXEC_EXACT_CONTENT_TYPE",
                            format!("{unit_path}.content"),
                            format!("EXACT artifact {id_label} content must be string; got {ty}"),
                        ));
                    }
                }
            } else {
                let locked_empty = unit.locked.as_ref().map(|l| l.is_empty()).unwrap_or(true);
                if locked_empty {
                    errors.push(err(
                        "EXEC_BOUNDED_LOCKED_EMPTY",
                        format!("{unit_path}.locked"),
                        format!("BOUNDED artifact {id_label} requires locked and freedom"),
                    ));
                }
                let freedom_empty = unit.freedom.as_ref().map(|f| f.is_empty()).unwrap_or(true);
                if freedom_empty {
                    errors.push(err(
                        "EXEC_BOUNDED_FREEDOM_EMPTY",
                        format!("{unit_path}.freedom"),
                        format!("BOUNDED artifact {id_label} requires locked and freedom"),
                    ));
                }
            }
        }
    }

    for (index, edge) in contract.dependencies.iter().enumerate() {
        if edge.reason == "shared-mutable-resource" {
            let missing = match &edge.resource_key {
                None => true,
                Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::String(s)) => s.is_empty(),
                Some(_) => false,
            };
            if missing {
                errors.push(err(
                    "EXEC_RESOURCE_KEY_MISSING",
                    format!("$.dependencies[{index}].resourceKey"),
                    "shared mutable dependency requires resourceKey",
                ));
            }
        }
    }

    let ids = collect_ids(contract);
    let mut seen: std::collections::HashMap<&str, &'static str> = std::collections::HashMap::new();
    for entry in &ids {
        if let Some(first_where) = seen.get(entry.id.as_str()) {
            errors.push(err(
                "EXEC_ID_NOT_UNIQUE",
                format!("$.{first_where}[*].id"),
                format!(
                    "execution contract ids must be unique: {} appears in {} and {}",
                    entry.id, first_where, entry.r#where
                ),
            ));
        } else {
            seen.insert(entry.id.as_str(), entry.r#where);
        }
    }

    errors
}

fn sort_errors(mut errors: Vec<ExecutableContractError>) -> Vec<ExecutableContractError> {
    let mut seen = std::collections::HashSet::new();
    errors.retain(|e| seen.insert((e.code.clone(), e.path.clone(), e.message.clone())));
    errors.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.message.cmp(&b.message))
    });
    errors
}

/// Port of `collectExecutableContractErrors`. Structural (schema) failures
/// are surfaced by the caller's `serde_json::from_value::<ExecutionContract>`
/// step before this is reached; this function assumes structural validity
/// and only runs the executability rules, matching the JS function's
/// behavior once `validateSchema` has produced zero structural issues.
pub fn collect_executable_contract_errors(
    contract: &ExecutionContract,
) -> Vec<ExecutableContractError> {
    sort_errors(run_executability_checks(contract))
}

/// Port of `validateExecutableContract`: returns the contract on success, or
/// the first sorted error's message as `Err`, matching the JS
/// throw-first-sorted-failure behavior.
pub fn validate_executable_contract(
    contract: ExecutionContract,
) -> Result<ExecutionContract, String> {
    let errors = collect_executable_contract_errors(&contract);
    if let Some(first) = errors.first() {
        return Err(first.message.clone());
    }
    Ok(contract)
}
