//! Deterministic compiler for the workflow control plane.
//!
//! WP-01-T02 acceptance:
//! - rejects unknown fields, duplicate IDs, cycles, unreachable nodes,
//!   unknown references, illegal joins / maps / loops, missing bounds,
//!   unsupported runtimes, and forbidden effects;
//! - produces canonical JSON, stable content hash, compiled plan,
//!   diagnostics, and effective capability ceilings.
//!

use thiserror::Error;

use crate::control_plane::domain::{
    CapabilitySet, CompiledPlan, ContentHash, Diagnostic, DiagnosticSeverity, NodeSpec,
    WorkflowDefinition, WorkflowId,
};
// Re-export so callers (and the crate prelude) can refer to the outcome
// type via `crate::control_plane::compiler::CompilerOutcome` without reaching into
// `crate::control_plane::domain` directly.
pub use crate::control_plane::domain::CompilerOutcome;

/// Typed failures the compiler can emit. Replaces free-form "compile
/// error" strings throughout the workflow crate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompilerError {
    #[error("duplicate node id `{0}` in workflow `{1}`")]
    DuplicateNodeId(String, WorkflowId),
    #[error("unknown edge target `{0}` from node `{1}`")]
    UnknownEdgeTarget(String, String),
    #[error("cycle detected: {0}")]
    CycleDetected(String),
    #[error("unreachable nodes: {0:?}")]
    UnreachableNodes(Vec<String>),
    #[error("missing entry node for workflow `{0}`")]
    MissingEntryNode(WorkflowId),
    #[error("missing bounds on node `{0}`: {1}")]
    MissingBounds(String, String),
    #[error("invalid retry policy on node `{0}`: {1}")]
    InvalidRetryPolicy(String, String),
    #[error("unsupported runtime `{0}` for node `{1}`")]
    UnsupportedRuntime(String, String),
    #[error("forbidden effect `{0}` declared by node `{1}`")]
    ForbiddenEffect(String, String),
    #[error("unknown field `{0}` in workflow spec")]
    UnknownField(String),
    #[error("invalid expression on node `{0}`: {1}")]
    InvalidExpression(String, String),
    #[error("internal compiler invariant violated: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSource {
    pub definition: WorkflowDefinition,
    pub nodes: Vec<NodeSpec>,
    /// Immutable admission authority copied from authoring.  This is kept
    /// beside the compiled graph so publication and execution cannot fall
    /// back to mutable schedule/session state.
    #[serde(default)]
    pub spec: SourceAuthority,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthority {
    pub inputs: serde_yaml::Value,
    pub budgets: serde_yaml::Value,
    pub workspace: WorkspaceAuthority,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAuthority {
    pub root: String,
    pub trust: String,
}

pub fn parse_json(source: &str) -> Result<WorkflowSource, CompilerError> {
    serde_json::from_str(source).map_err(|e| CompilerError::UnknownField(e.to_string()))
}

pub fn parse_yaml(source: &str) -> Result<WorkflowSource, CompilerError> {
    serde_yaml::from_str(source).map_err(|e| CompilerError::UnknownField(e.to_string()))
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum AuthoringWorkflowSpec {
    Legacy(LegacyAuthoring),
    V1Alpha(EnvelopeAuthoring),
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyAuthoring {
    pub kind: String,
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub trigger: serde_yaml::Value,
    pub inputs: serde_yaml::Value,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub nodes: Vec<AuthoringNode>,
    #[serde(default)]
    pub edges: Vec<AuthoringEdge>,
    pub budgets: serde_yaml::Value,
    #[serde(default)]
    pub workspace: WorkspaceAuthority,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeAuthoring {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: AuthoringMetadata,
    pub spec: EnvelopeSpec,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringMetadata {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeSpec {
    pub enabled: bool,
    pub inputs: serde_yaml::Value,
    pub budgets: serde_yaml::Value,
    pub capabilities: serde_yaml::Value,
    pub triggers: Vec<serde_yaml::Value>,
    pub nodes: Vec<AuthoringNode>,
    pub completion: serde_yaml::Value,
    #[serde(default)]
    pub workspace: WorkspaceAuthority,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub when: Option<String>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringNode {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub attempt: Option<serde_yaml::Value>,
    #[serde(default)]
    pub payload: Option<serde_yaml::Value>,
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub bounds: Option<serde_yaml::Value>,
    #[serde(default)]
    pub retry: Option<serde_yaml::Value>,
    #[serde(default)]
    pub inputs: Option<serde_yaml::Value>,
    #[serde(default, rename = "outputSchema")]
    pub output_schema: Option<String>,
    #[serde(default)]
    pub claims: Vec<serde_yaml::Value>,
    #[serde(default)]
    pub completion: Option<String>,
    #[serde(default)]
    pub config: Option<serde_yaml::Value>,
    #[serde(default)]
    pub effect: Option<serde_yaml::Value>,
    #[serde(default)]
    pub effects: Vec<serde_yaml::Value>,
    #[serde(default)]
    pub test: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
}

pub fn parse_authoring_yaml(source: &str) -> Result<WorkflowSource, CompilerError> {
    let spec: AuthoringWorkflowSpec =
        serde_yaml::from_str(source).map_err(|e| CompilerError::UnknownField(e.to_string()))?;
    translate_authoring(spec)
}

pub fn parse_authoring_json(source: &str) -> Result<WorkflowSource, CompilerError> {
    let spec: AuthoringWorkflowSpec =
        serde_json::from_str(source).map_err(|e| CompilerError::UnknownField(e.to_string()))?;
    translate_authoring(spec)
}

/// Validate immutable admission authority before a draft may be published.
/// Empty workspace identity is rejected rather than inferred from a caller.
pub fn validate_source_authority(source: &WorkflowSource) -> Result<(), CompilerError> {
    if !source.spec.inputs.is_mapping() {
        return Err(CompilerError::Internal(
            "spec.inputs must be an object".into(),
        ));
    }
    if !source.spec.budgets.is_mapping() {
        return Err(CompilerError::Internal(
            "spec.budgets must be an object".into(),
        ));
    }
    if source.spec.workspace.root.trim().is_empty() {
        return Err(CompilerError::Internal(
            "spec.workspace.root is required".into(),
        ));
    }
    if source.spec.workspace.trust.trim().is_empty() {
        return Err(CompilerError::Internal(
            "spec.workspace.trust is required".into(),
        ));
    }
    Ok(())
}

fn translate_authoring(spec: AuthoringWorkflowSpec) -> Result<WorkflowSource, CompilerError> {
    let (id, name, description, authoring_nodes, global_edges, authority) = match spec {
        AuthoringWorkflowSpec::Legacy(v) => (
            v.id,
            v.name,
            v.description,
            v.nodes,
            v.edges,
            SourceAuthority {
                inputs: v.inputs,
                budgets: v.budgets,
                workspace: v.workspace,
            },
        ),
        AuthoringWorkflowSpec::V1Alpha(v) => {
            if v.spec.enabled {
                return Err(CompilerError::ForbiddenEffect(
                    "enabled reference workflow".into(),
                    v.metadata.id,
                ));
            }
            (
                v.metadata.id,
                v.metadata.name,
                v.metadata.description,
                v.spec.nodes,
                Vec::new(),
                SourceAuthority {
                    inputs: v.spec.inputs,
                    budgets: v.spec.budgets,
                    workspace: v.spec.workspace,
                },
            )
        }
    };
    let mut nodes = Vec::new();
    for raw in &authoring_nodes {
        let kind = parse_node_kind(&raw.kind)?;
        let mut edges = global_edges
            .iter()
            .filter(|e| e.from == raw.id && e.to != "end")
            .map(|e| crate::control_plane::domain::EdgeSpec {
                target: e.to.clone(),
                when: e.when.clone(),
            })
            .collect::<Vec<_>>();
        for dependent in authoring_nodes
            .iter()
            .filter(|n| n.depends_on.iter().any(|d| d == &raw.id))
        {
            edges.push(crate::control_plane::domain::EdgeSpec {
                target: dependent.id.clone(),
                when: dependent.when.clone(),
            });
        }
        let max_attempts = yaml_u64(raw.retry.as_ref().or(raw.attempt.as_ref()), "maxAttempts")
            .or_else(|| yaml_u64(raw.attempt.as_ref(), "max_attempts"))
            .unwrap_or(1) as u32;
        let config = authoring_config(raw, kind)?;
        nodes.push(NodeSpec {
            id: raw.id.clone(),
            kind,
            label: raw.label.clone(),
            capabilities: CapabilitySet::from_iter(
                raw.capabilities
                    .iter()
                    .cloned()
                    .map(crate::control_plane::domain::Capability::new),
            ),
            edges,
            retry: crate::control_plane::domain::RetryPolicy {
                max_attempts,
                backoff: crate::control_plane::domain::BackoffStrategy::Fixed { delay_ms: 0 },
                retryable: Vec::new(),
            },
            config,
        });
    }
    Ok(WorkflowSource {
        definition: WorkflowDefinition {
            id: crate::control_plane::domain::workflow_definition_id(
                id.trim_start_matches("wfd_")
                    .trim_start_matches("wfd-")
                    .to_string(),
            ),
            name,
            description,
            owner_scope: "reference".into(),
            revision: crate::control_plane::domain::Revision::ZERO,
            created_at: String::new(),
            updated_at: String::new(),
        },
        nodes,
        spec: authority,
    })
}

fn parse_node_kind(raw: &str) -> Result<crate::control_plane::domain::NodeKind, CompilerError> {
    use crate::control_plane::domain::NodeKind::*;
    Ok(match raw.to_ascii_lowercase().as_str() {
        "agent" => Agent,
        "recipe" => Recipe,
        "transform" => Transform,
        "condition" => Condition,
        "join" => Join,
        "map" => Map,
        "loop" => Loop,
        "wait" | "waittimer" => WaitTimer,
        "waitevent" => WaitEvent,
        "approval" => Approval,
        "effect" => Effect,
        "subworkflow" => Subworkflow,
        other => {
            return Err(CompilerError::UnsupportedRuntime(
                other.into(),
                "authoring".into(),
            ));
        }
    })
}
fn yaml_u64(value: Option<&serde_yaml::Value>, key: &str) -> Option<u64> {
    value?
        .as_mapping()?
        .get(serde_yaml::Value::String(key.into()))?
        .as_u64()
}
fn yaml_str(value: Option<&serde_yaml::Value>, key: &str) -> Option<String> {
    value?
        .as_mapping()?
        .get(serde_yaml::Value::String(key.into()))?
        .as_str()
        .map(str::to_string)
}
fn yaml_json(value: Option<&serde_yaml::Value>) -> Result<String, CompilerError> {
    match value {
        Some(value) => {
            serde_json::to_string(value).map_err(|error| CompilerError::Internal(error.to_string()))
        }
        None => Ok("null".into()),
    }
}
fn normalize_event_filter(source: &str) -> String {
    let mut output = String::with_capacity(source.len() + 16);
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while index < source.len() {
        let ch = source[index..].chars().next().expect("character boundary");
        if let Some(expected) = quote {
            output.push(ch);
            index += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == expected {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if source[index..].starts_with("event.")
            && output
                .chars()
                .last()
                .is_none_or(|prior| !prior.is_ascii_alphanumeric() && prior != '_' && prior != '.')
        {
            output.push_str("inputs.event.");
            index += "event.".len();
            continue;
        }
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}
fn authored_effect(raw: &AuthoringNode) -> Option<&serde_yaml::Value> {
    raw.effect
        .as_ref()
        .or_else(|| raw.effects.first())
        .or(raw.payload.as_ref())
}
fn normalized_effect_adapter(raw: &AuthoringNode) -> String {
    let operation = yaml_str(authored_effect(raw), "operation")
        .or_else(|| yaml_str(authored_effect(raw), "kind"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if operation.contains("comment") {
        "DraftComment".into()
    } else if operation.contains("review") || operation.contains("notify") {
        "DraftReview".into()
    } else {
        "DraftDocument".into()
    }
}
fn authoring_config(
    raw: &AuthoringNode,
    kind: crate::control_plane::domain::NodeKind,
) -> Result<crate::control_plane::domain::NodeConfig, CompilerError> {
    use crate::control_plane::domain::{NodeConfig as C, NodeKind as K};
    Ok(match kind {
        K::Map => C::Map {
            max_items: yaml_u64(raw.bounds.as_ref(), "maxItems")
                .or_else(|| yaml_u64(raw.config.as_ref(), "maxItems"))
                .unwrap_or(0) as u32,
        },
        K::Loop => C::Loop {
            max_iterations: yaml_u64(raw.config.as_ref(), "maxIterations").unwrap_or(0) as u32,
            per_iteration_budget: yaml_u64(raw.config.as_ref(), "perIterationBudget").unwrap_or(0),
            progress_fingerprint: yaml_str(raw.config.as_ref(), "progressFingerprint")
                .unwrap_or_default(),
            no_progress_limit: yaml_u64(raw.config.as_ref(), "noProgressLimit").unwrap_or(0) as u32,
        },
        K::WaitTimer | K::WaitEvent => {
            let filter_source = yaml_str(raw.config.as_ref(), "filter")
                .or_else(|| yaml_str(raw.config.as_ref(), "filterExpression"))
                .or_else(|| yaml_str(raw.payload.as_ref(), "filter"));
            let filter_expression = filter_source
                .as_deref()
                .map(normalize_event_filter)
                .map(|source| {
                    if source.len() > 2_048 {
                        return Err(CompilerError::InvalidExpression(
                            raw.id.clone(),
                            "filter exceeds 2048 bytes".into(),
                        ));
                    }
                    let expression =
                        crate::control_plane::expression::parse(&source).map_err(|error| {
                            CompilerError::InvalidExpression(raw.id.clone(), error.to_string())
                        })?;
                    if !crate::control_plane::expression::is_bounded(&expression, 64, 16) {
                        return Err(CompilerError::InvalidExpression(
                            raw.id.clone(),
                            "filter exceeds 64 nodes or depth 16".into(),
                        ));
                    }
                    Ok(expression)
                })
                .transpose()?;
            let filter_hash = yaml_str(raw.config.as_ref(), "filterHash")
                .or_else(|| yaml_str(raw.payload.as_ref(), "filterHash"))
                .or_else(|| {
                    filter_expression.as_ref().map(|value| {
                        crate::control_plane::domain::ContentHash::sha256(
                            crate::control_plane::expression::canonical_text(value).as_bytes(),
                        )
                        .to_string()
                    })
                });
            C::Wait {
                max_matches: yaml_u64(raw.config.as_ref(), "maxMatches").unwrap_or(1) as u32,
                timeout_ms: yaml_u64(raw.bounds.as_ref(), "maxRuntimeSeconds").unwrap_or(60) * 1000,
                topic: yaml_str(raw.config.as_ref(), "topic")
                    .or_else(|| yaml_str(raw.payload.as_ref(), "topic")),
                correlation_key: yaml_str(raw.config.as_ref(), "correlationKey")
                    .or_else(|| yaml_str(raw.config.as_ref(), "correlation"))
                    .or_else(|| yaml_str(raw.payload.as_ref(), "correlationKey")),
                filter_expression,
                filter_hash,
            }
        }
        K::Approval => C::Approval {
            role: raw
                .role
                .clone()
                .or_else(|| yaml_str(raw.config.as_ref(), "role"))
                .unwrap_or_else(|| "operator".into()),
            expires_after_ms: yaml_u64(raw.config.as_ref(), "expiresAfterMs")
                .or_else(|| yaml_u64(raw.config.as_ref(), "expiresSeconds").map(|v| v * 1000))
                .unwrap_or(86_400_000),
        },
        K::Effect => C::Effect {
            adapter: normalized_effect_adapter(raw),
            operation: yaml_str(authored_effect(raw), "operation")
                .or_else(|| yaml_str(authored_effect(raw), "kind"))
                .unwrap_or_else(|| "draft".into()),
            draft_only: true,
            max_attempts: yaml_u64(authored_effect(raw), "maxAttempts").unwrap_or(1) as u32,
        },
        K::Condition => C::Condition {
            predicate: raw
                .test
                .clone()
                .or_else(|| raw.when.clone())
                .or_else(|| yaml_str(raw.payload.as_ref(), "predicate"))
                .unwrap_or_else(|| "true".into()),
        },
        K::Join => C::Join {
            mode: match yaml_str(raw.config.as_ref(), "mode").as_deref() {
                Some("allTerminal") | None => "all".into(),
                Some("anyTerminal") => "any".into(),
                Some(mode) => mode.into(),
            },
        },
        _ => C::Execution {
            runtime: raw.runtime.clone().unwrap_or_else(|| {
                if kind == K::Recipe {
                    "recipe".into()
                } else if kind == K::Transform {
                    "deterministic".into()
                } else {
                    "native".into()
                }
            }),
            instruction: yaml_str(raw.payload.as_ref(), "spec").unwrap_or_else(|| {
                serde_json::json!({
                    "label": raw.label,
                    "role": raw.role,
                    "inputs": raw.inputs,
                    "config": raw.config,
                    "completion": raw.completion,
                })
                .to_string()
            }),
            runtime_target: if kind == K::Subworkflow {
                yaml_str(raw.payload.as_ref().or(raw.config.as_ref()), "workflow").and_then(
                    |workflow| {
                        yaml_str(raw.payload.as_ref().or(raw.config.as_ref()), "versionPin")
                            .or_else(|| {
                                yaml_str(raw.payload.as_ref().or(raw.config.as_ref()), "version")
                            })
                            .map(|version| format!("{workflow}@{version}"))
                    },
                )
            } else {
                raw.role
                    .clone()
                    .or_else(|| yaml_str(raw.payload.as_ref(), "role"))
            },
            recipe_ref: yaml_str(raw.payload.as_ref(), "recipe")
                .or_else(|| yaml_str(raw.config.as_ref(), "recipe")),
            inputs_json: yaml_json(raw.inputs.as_ref().or_else(|| {
                raw.payload.as_ref().and_then(|payload| {
                    payload
                        .as_mapping()
                        .and_then(|mapping| mapping.get(serde_yaml::Value::String("inputs".into())))
                })
            }))?,
            transform_operation: yaml_str(raw.payload.as_ref(), "transform")
                .or_else(|| yaml_str(raw.config.as_ref(), "operation"))
                .or_else(|| (kind == K::Transform).then(|| raw.id.clone())),
            template: yaml_str(raw.payload.as_ref(), "template")
                .or_else(|| yaml_str(raw.config.as_ref(), "template")),
            max_runtime_ms: yaml_u64(raw.bounds.as_ref(), "maxRuntimeSeconds").unwrap_or(60) * 1000,
            max_output_bytes: yaml_u64(raw.bounds.as_ref(), "maxOutputBytes").unwrap_or(1_048_576),
            output_schema: raw
                .output_schema
                .clone()
                .or_else(|| yaml_str(raw.payload.as_ref(), "output_schema"))
                .unwrap_or_else(|| "schema://reference/opaque".into()),
        },
    })
}

/// Compile `definition` into an immutable workflow version. The function
/// is pure: same definition + same `now_iso` ⇒ same canonical JSON, same
/// content hash, same compiled plan, same diagnostics.
///
pub fn compile(
    definition: &WorkflowDefinition,
    nodes: &[NodeSpec],
    now_iso: &str,
) -> CompilerOutcome {
    let mut diagnostics = Vec::new();
    let mut errors = 0u32;

    // 1. Duplicate node ids.
    let mut seen = std::collections::BTreeSet::new();
    for n in nodes {
        if !seen.insert(n.id.clone()) {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "duplicate_node_id",
                CompilerError::DuplicateNodeId(n.id.clone(), definition.id.clone()).to_string(),
                Some(n.id.clone()),
            ));
            errors += 1;
        }
    }

    // 2. Edge targets resolve.
    for n in nodes {
        for edge in &n.edges {
            if !nodes.iter().any(|m| m.id == edge.target) {
                diagnostics.push(diagnostic(
                    DiagnosticSeverity::Error,
                    "unknown_edge_target",
                    CompilerError::UnknownEdgeTarget(edge.target.clone(), n.id.clone()).to_string(),
                    Some(n.id.clone()),
                ));
                errors += 1;
            }
        }
    }

    // 2b. Every node must carry finite retry/backoff bounds. These checks
    // keep malformed or model-authored specs from creating unbounded work.
    for n in nodes {
        if n.retry.max_attempts == 0 {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "missing_bounds",
                CompilerError::MissingBounds(n.id.clone(), "retry.max_attempts".into()).to_string(),
                Some(n.id.clone()),
            ));
            errors += 1;
        }
        match n.retry.backoff {
            crate::control_plane::domain::BackoffStrategy::Fixed { .. } => {}
            crate::control_plane::domain::BackoffStrategy::Exponential {
                base_ms,
                max_ms,
                jitter,
            } => {
                if base_ms > max_ms || jitter.is_some_and(|j| !(0.0..=1.0).contains(&j)) {
                    diagnostics.push(diagnostic(
                        DiagnosticSeverity::Error,
                        "invalid_retry_policy",
                        CompilerError::InvalidRetryPolicy(
                            n.id.clone(),
                            "backoff bounds/jitter".into(),
                        )
                        .to_string(),
                        Some(n.id.clone()),
                    ));
                    errors += 1;
                }
            }
        }
        if matches!(n.kind, crate::control_plane::domain::NodeKind::Effect)
            && n.capabilities.as_slice().iter().any(|c| {
                matches!(
                    c.as_str(),
                    "effect.publish"
                        | "effect.merge"
                        | "effect.deploy"
                        | "effect.delete"
                        | "effect.repository_security"
                )
            })
        {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "forbidden_effect",
                CompilerError::ForbiddenEffect("automatic external mutation".into(), n.id.clone())
                    .to_string(),
                Some(n.id.clone()),
            ));
            errors += 1;
        }
        use crate::control_plane::domain::{NodeConfig, NodeKind};
        let config_ok = matches!(
            (&n.kind, &n.config),
            (NodeKind::Map, NodeConfig::Map { max_items }) if *max_items > 0
        ) || matches!(
            (&n.kind, &n.config),
            (NodeKind::Loop, NodeConfig::Loop { max_iterations, per_iteration_budget, progress_fingerprint, no_progress_limit })
                if *max_iterations > 0 && *per_iteration_budget > 0 && !progress_fingerprint.is_empty() && *no_progress_limit > 0
        ) || matches!(
            (&n.kind, &n.config),
            (NodeKind::WaitTimer, NodeConfig::Wait { max_matches, timeout_ms, .. }) if *max_matches > 0 && *timeout_ms > 0
        ) || matches!(
            (&n.kind, &n.config),
            (NodeKind::WaitEvent, NodeConfig::Wait { max_matches, timeout_ms, topic: Some(topic), correlation_key: Some(correlation), filter_expression, filter_hash })
                if *max_matches > 0 && *timeout_ms > 0 && !topic.is_empty() && !correlation.is_empty()
                    && filter_expression.as_ref().is_some_and(|value| crate::control_plane::expression::is_bounded(value, 64, 16))
                    && filter_hash.as_ref().is_some_and(|hash| filter_expression.as_ref().is_some_and(|value| *hash == crate::control_plane::domain::ContentHash::sha256(crate::control_plane::expression::canonical_text(value).as_bytes()).to_string()))
        ) || matches!(
            (&n.kind, &n.config),
            (NodeKind::Approval, NodeConfig::Approval { role, expires_after_ms }) if !role.is_empty() && *expires_after_ms > 0
        ) || matches!(
            (&n.kind, &n.config),
            (NodeKind::Effect, NodeConfig::Effect { adapter, operation, draft_only: true, max_attempts }) if crate::control_plane::effect::is_v1_allowed(adapter) && !operation.is_empty() && *max_attempts > 0
        ) || matches!((&n.kind, &n.config), (NodeKind::Condition, NodeConfig::Condition { predicate }) if !predicate.is_empty())
            || matches!((&n.kind, &n.config), (NodeKind::Join, NodeConfig::Join { mode }) if matches!(mode.as_str(), "all" | "any"))
            || matches!((&n.kind, &n.config), (kind @ (NodeKind::Agent | NodeKind::Recipe | NodeKind::Transform | NodeKind::Subworkflow), NodeConfig::Execution { runtime, instruction, runtime_target, recipe_ref, transform_operation, max_runtime_ms, max_output_bytes, output_schema, .. }) if matches!(runtime.as_str(), "native" | "codex" | "claude" | "conductor" | "recipe" | "deterministic" | "a2a") && (runtime != "a2a" || (matches!(kind, NodeKind::Agent | NodeKind::Recipe) && runtime_target.as_ref().is_some_and(|value| !value.trim().is_empty()))) && *max_runtime_ms > 0 && *max_output_bytes > 0 && !output_schema.is_empty() && match kind { NodeKind::Recipe => recipe_ref.as_ref().is_some_and(|value| !value.is_empty()), NodeKind::Transform => transform_operation.as_ref().is_some_and(|value| !value.is_empty()), NodeKind::Subworkflow => runtime_target.as_ref().is_some_and(|value| value.contains('@')), _ => !instruction.is_empty() });
        if !config_ok {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "invalid_node_config",
                CompilerError::MissingBounds(n.id.clone(), "typed node config".into()).to_string(),
                Some(n.id.clone()),
            ));
            errors += 1;
        }
    }

    // 3. Cycle detection (DFS).
    if let Some(cycle) = detect_cycle(nodes) {
        diagnostics.push(diagnostic(
            DiagnosticSeverity::Error,
            "cycle_detected",
            CompilerError::CycleDetected(cycle).to_string(),
            None,
        ));
        errors += 1;
    }

    // 4. Entry node presence. The compiled plan's `entry_node` is the
    // first node in declared order for V1; visual authoring arrives later.
    let entry_node = match nodes.first() {
        Some(n) => n.id.clone(),
        None => {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "missing_entry_node",
                CompilerError::MissingEntryNode(definition.id.clone()).to_string(),
                None,
            ));
            errors += 1;
            String::new()
        }
    };

    // 5. Unreachable nodes from the entry node (BFS).
    if !nodes.is_empty() {
        let unreachable = unreachable_nodes(nodes, &entry_node);
        if !unreachable.is_empty() {
            diagnostics.push(diagnostic(
                DiagnosticSeverity::Error,
                "unreachable_nodes",
                CompilerError::UnreachableNodes(unreachable.clone()).to_string(),
                None,
            ));
            errors += 1;
        }
    }

    // 6. Workflow capability summary: deterministic union of node requests.
    let effective_capabilities = CapabilitySet::from_iter(
        nodes
            .iter()
            .flat_map(|n| n.capabilities.as_slice().to_vec()),
    );

    if errors > 0 {
        return CompilerOutcome::Failed { diagnostics };
    }

    let mut canonical_nodes = nodes.to_vec();
    canonical_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let compiled_plan = CompiledPlan {
        entry_node,
        nodes: canonical_nodes,
        effective_capabilities,
    };
    let canonical_json = serde_json::to_string(&compiled_plan)
        .expect("CompiledPlan serialization is infallible (no nested non-string keys)");
    let content_hash = ContentHash::sha256(canonical_json.as_bytes());

    let version_id = crate::control_plane::domain::workflow_version_id({
        let mut h = rightkit_fs::digest::Sha256Hasher::new();
        h.update(definition.id.stable().as_str().as_bytes());
        h.update(content_hash.as_str().as_bytes());
        rightkit_fs::digest::hex(&h.finalize_bytes()[..11])
    });

    let _ = now_iso; // reserved for WP-01 deterministic timestamp embedding.

    CompilerOutcome::Compiled {
        version_id,
        content_hash,
        plan: compiled_plan,
    }
}

fn diagnostic(
    severity: DiagnosticSeverity,
    code: &str,
    message: String,
    node_id: Option<String>,
) -> Diagnostic {
    Diagnostic {
        severity,
        code: code.to_string(),
        message,
        node_id,
    }
}

fn detect_cycle(nodes: &[NodeSpec]) -> Option<String> {
    let mut state: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    // 0 = unvisited, 1 = on stack, 2 = done.
    let keys: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    for key in keys {
        if state.get(&key).copied().unwrap_or(0) != 0 {
            continue;
        }
        if let Some(cycle) = dfs(nodes, &mut state, &key) {
            return Some(cycle);
        }
    }
    None
}

fn dfs(
    nodes: &[NodeSpec],
    state: &mut std::collections::HashMap<String, u8>,
    node_id: &str,
) -> Option<String> {
    state.insert(node_id.to_string(), 1);
    if let Some(n) = nodes.iter().find(|n| n.id == node_id) {
        for edge in &n.edges {
            match state.get(&edge.target).copied().unwrap_or(0) {
                1 => return Some(format!("{} -> {}", node_id, edge.target)),
                0 => {
                    if let Some(cycle) = dfs(nodes, state, &edge.target) {
                        return Some(cycle);
                    }
                }
                _ => {}
            }
        }
    }
    state.insert(node_id.to_string(), 2);
    None
}

fn unreachable_nodes(nodes: &[NodeSpec], entry: &str) -> Vec<String> {
    let mut reachable = std::collections::BTreeSet::new();
    let mut stack = vec![entry.to_string()];
    while let Some(cur) = stack.pop() {
        if !reachable.insert(cur.clone()) {
            continue;
        }
        if let Some(n) = nodes.iter().find(|n| n.id == cur) {
            for edge in &n.edges {
                stack.push(edge.target.clone());
            }
        }
    }
    nodes
        .iter()
        .filter(|n| !reachable.contains(&n.id))
        .map(|n| n.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::{
        BackoffStrategy, Capability, CapabilitySet, EdgeSpec, FailureClass, NodeKind, NodeSpec,
        RetryPolicy, WorkflowDefinition,
    };

    fn def() -> WorkflowDefinition {
        WorkflowDefinition {
            id: crate::control_plane::domain::workflow_id("alpha"),
            name: "alpha".into(),
            description: "test".into(),
            owner_scope: "owner".into(),
            revision: crate::control_plane::domain::Revision::ZERO,
            created_at: "2026-08-06T00:00:00Z".into(),
            updated_at: "2026-08-06T00:00:00Z".into(),
        }
    }

    fn node(id: &str, edges: &[&str]) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Transform,
            label: None,
            capabilities: CapabilitySet::from_iter([Capability::new("fs.read")]),
            edges: edges
                .iter()
                .map(|t| EdgeSpec {
                    target: (*t).into(),
                    when: None,
                })
                .collect(),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffStrategy::Fixed { delay_ms: 100 },
                retryable: vec![FailureClass::TransientNetwork],
            },
            config: crate::control_plane::domain::NodeConfig::Execution {
                runtime: "deterministic".into(),
                instruction: "transform input".into(),
                runtime_target: None,
                recipe_ref: None,
                inputs_json: "{}".into(),
                transform_operation: Some("identity".into()),
                template: None,
                max_runtime_ms: 1_000,
                max_output_bytes: 1_024,
                output_schema: "schema://test".into(),
            },
        }
    }

    #[test]
    fn compile_succeeds_for_linear_chain() {
        let n1 = node("n1", &["n2"]);
        let n2 = node("n2", &["n3"]);
        let n3 = node("n3", &[]);
        let outcome = compile(&def(), &[n1, n2, n3], "2026-08-06T00:00:00Z");
        match outcome {
            CompilerOutcome::Compiled {
                content_hash, plan, ..
            } => {
                assert_eq!(plan.entry_node, "n1");
                assert_eq!(plan.nodes.len(), 3);
                assert!(content_hash.as_str().starts_with("sha256:"));
            }
            CompilerOutcome::Failed { diagnostics } => panic!(
                "expected compile success, got diagnostics: {:?}",
                diagnostics
            ),
        }
    }

    #[test]
    fn duplicate_node_ids_fail() {
        let n1 = node("n1", &["n2"]);
        let n2 = node("n1", &[]); // duplicate id
        let outcome = compile(&def(), &[n1, n2], "2026-08-06T00:00:00Z");
        assert!(matches!(outcome, CompilerOutcome::Failed { .. }));
    }

    #[test]
    fn unknown_edge_target_fails() {
        let n1 = node("n1", &["n_missing"]);
        let outcome = compile(&def(), &[n1], "2026-08-06T00:00:00Z");
        assert!(matches!(outcome, CompilerOutcome::Failed { .. }));
    }

    #[test]
    fn cycle_fails() {
        let n1 = node("n1", &["n2"]);
        let n2 = node("n2", &["n1"]);
        let outcome = compile(&def(), &[n1, n2], "2026-08-06T00:00:00Z");
        assert!(matches!(outcome, CompilerOutcome::Failed { .. }));
    }

    #[test]
    fn unreachable_node_fails() {
        let n1 = node("n1", &[]);
        let n2 = node("n2", &[]);
        let outcome = compile(&def(), &[n1, n2], "2026-08-06T00:00:00Z");
        assert!(matches!(outcome, CompilerOutcome::Failed { .. }));
    }

    #[test]
    fn canonical_hash_is_stable() {
        let n1 = node("n1", &["n2"]);
        let n2 = node("n2", &[]);
        let a = compile(&def(), &[n1.clone(), n2.clone()], "2026-08-06T00:00:00Z");
        let b = compile(&def(), &[n1, n2], "2026-08-06T00:00:00Z");
        let (
            CompilerOutcome::Compiled {
                content_hash: ha, ..
            },
            CompilerOutcome::Compiled {
                content_hash: hb, ..
            },
        ) = (a, b)
        else {
            panic!("both compiles should succeed");
        };
        assert_eq!(ha, hb);
    }

    #[test]
    fn a2a_runtime_is_admitted_only_for_bounded_agent_or_recipe_nodes() {
        let mut agent = node("agent", &[]);
        agent.kind = NodeKind::Agent;
        if let crate::control_plane::domain::NodeConfig::Execution { runtime, .. } =
            &mut agent.config
        {
            *runtime = "a2a".into();
        }
        if let crate::control_plane::domain::NodeConfig::Execution { runtime_target, .. } =
            &mut agent.config
        {
            *runtime_target = Some("executor-1".into());
        }
        assert!(matches!(
            compile(&def(), &[agent.clone()], "2026-08-06T00:00:00Z"),
            CompilerOutcome::Compiled { .. }
        ));

        if let crate::control_plane::domain::NodeConfig::Execution { runtime_target, .. } =
            &mut agent.config
        {
            *runtime_target = None;
        }
        assert!(matches!(
            compile(&def(), &[agent.clone()], "2026-08-06T00:00:00Z"),
            CompilerOutcome::Failed { .. }
        ));

        agent.kind = NodeKind::Transform;
        assert!(matches!(
            compile(&def(), &[agent], "2026-08-06T00:00:00Z"),
            CompilerOutcome::Failed { .. }
        ));
    }

    #[test]
    fn event_wait_requires_explicit_match_routing() {
        let mut wait = node("wait", &[]);
        wait.kind = crate::control_plane::domain::NodeKind::WaitEvent;
        wait.config = crate::control_plane::domain::NodeConfig::Wait {
            max_matches: 1,
            timeout_ms: 1_000,
            topic: None,
            correlation_key: None,
            filter_expression: None,
            filter_hash: None,
        };
        assert!(matches!(
            compile(&def(), &[wait.clone()], "2026-08-06T00:00:00Z"),
            CompilerOutcome::Failed { .. }
        ));
        wait.config = crate::control_plane::domain::NodeConfig::Wait {
            max_matches: 1,
            timeout_ms: 1_000,
            topic: Some("issue.updated".into()),
            correlation_key: Some("issue:42".into()),
            filter_expression: Some(
                crate::control_plane::expression::parse("inputs.event.state == \"ready\"").unwrap(),
            ),
            filter_hash: Some(
                crate::control_plane::domain::ContentHash::sha256(
                    b"(inputs.event.state == \"ready\")",
                )
                .to_string(),
            ),
        };
        assert!(matches!(
            compile(&def(), &[wait], "2026-08-06T00:00:00Z"),
            CompilerOutcome::Compiled { .. }
        ));
    }
}
