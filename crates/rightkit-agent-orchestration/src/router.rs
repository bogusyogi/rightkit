//! The agent router — matches tasks to the best-fitting registered agent.

use crate::registry::{AgentDescriptor, AgentRegistry};
use crate::review_panel::{council_for_category, jury_for_category, ReviewPanel};
use rightkit_agent_engine_protocol::TaskNode;
use rightkit_llm::agent_providers::model_metadata::{ModelCapability, ModelTier};
use serde::{Deserialize, Serialize};

/// Versioned, data-backed model/tool/policy assignment for one runtime role.
/// Model identifiers are resolved against the caller's live catalog; this
/// module does not maintain a second compiled roster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolePackV1 {
    pub schema_version: u32,
    pub role: String,
    pub primary: String,
    pub cheap_candidate: String,
    pub fallbacks: Vec<String>,
    pub verifier: String,
    pub reasoning: String,
    pub context_budget: u64,
    pub toolset: Vec<String>,
    pub permissions: Vec<String>,
    pub isolation: String,
    pub max_turns: u32,
    pub max_cost_usd: f64,
    pub retry_limit: u8,
    pub escalate_on: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RolePackError {
    #[error("role pack field `{0}` is missing or invalid")]
    InvalidField(&'static str),
    #[error("role pack references unknown model `{0}`")]
    UnknownModel(String),
    #[error("role pack model `{model}` is unqualified: {reason}")]
    UnqualifiedModel { model: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolePackVerifierReceipt {
    pub model_id: String,
    pub independent: bool,
    pub degradation_reason: Option<String>,
}

impl RolePackV1 {
    /// Parse the deliberately small TOML surface used by role-pack assets.
    /// Unknown keys are ignored for forward compatibility; every v1 required
    /// key is still checked below.
    pub fn parse_toml(raw: &str) -> Result<Self, RolePackError> {
        let values = role_pack_values(raw);
        Ok(Self {
            schema_version: parse_u32(&values, "schema_version")?,
            role: parse_string(&values, "role")?,
            primary: parse_string(&values, "primary")?,
            cheap_candidate: parse_string(&values, "cheap_candidate")?,
            fallbacks: parse_array(&values, "fallbacks")?,
            verifier: parse_string(&values, "verifier")?,
            reasoning: parse_string(&values, "reasoning")?,
            context_budget: parse_u64(&values, "context_budget")?,
            toolset: parse_array(&values, "toolset")?,
            permissions: parse_array(&values, "permissions")?,
            isolation: parse_string(&values, "isolation")?,
            max_turns: parse_u32(&values, "max_turns")?,
            max_cost_usd: values
                .get("max_cost_usd")
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or(RolePackError::InvalidField("max_cost_usd"))?,
            retry_limit: parse_u32(&values, "retry_limit")?
                .try_into()
                .map_err(|_| RolePackError::InvalidField("retry_limit"))?,
            escalate_on: parse_array(&values, "escalate_on")?,
        })
    }

    pub fn validate(&self, catalog: &[ModelCapability]) -> Result<(), RolePackError> {
        if self.schema_version != 1 {
            return Err(RolePackError::InvalidField("schema_version"));
        }
        if self.role.trim().is_empty()
            || self.reasoning.trim().is_empty()
            || self.toolset.is_empty()
            || self.permissions.is_empty()
            || self.isolation.trim().is_empty()
            || self.context_budget == 0
            || self.max_turns == 0
            || self.escalate_on.is_empty()
        {
            return Err(RolePackError::InvalidField("required v1 assignment"));
        }
        let mut assigned = vec![self.primary.as_str(), self.cheap_candidate.as_str()];
        assigned.extend(self.fallbacks.iter().map(String::as_str));
        assigned.push(self.verifier.as_str());
        for slug in assigned {
            let capability = catalog
                .iter()
                .find(|model| model.slug == slug)
                .ok_or_else(|| RolePackError::UnknownModel(slug.to_string()))?;
            if capability.context_window_tokens < self.context_budget {
                return Err(RolePackError::UnqualifiedModel {
                    model: slug.to_string(),
                    reason: format!(
                        "context window {} is below role budget {}",
                        capability.context_window_tokens, self.context_budget
                    ),
                });
            }
        }
        let cheap = catalog
            .iter()
            .find(|model| model.slug == self.cheap_candidate)
            .expect("assigned models checked above");
        if cheap.tier != ModelTier::Cheap {
            return Err(RolePackError::UnqualifiedModel {
                model: cheap.slug.clone(),
                reason: "cheap_candidate is not catalogued as cheap".into(),
            });
        }
        Ok(())
    }

    pub fn verifier_receipt(&self) -> RolePackVerifierReceipt {
        let producer_family = provider_family(&self.primary);
        let verifier_family = provider_family(&self.verifier);
        let independent = self.primary != self.verifier && producer_family != verifier_family;
        RolePackVerifierReceipt {
            model_id: self.verifier.clone(),
            independent,
            degradation_reason: (!independent)
                .then(|| "role-pack verifier is not from a different provider family".to_string()),
        }
    }
}

pub fn builtin_role_pack(role: &str) -> Result<RolePackV1, RolePackError> {
    let raw = match role {
        "orchestrator" => include_str!("../../../fixtures/role-packs/orchestrator.v1.toml"),
        "coder" => include_str!("../../../fixtures/role-packs/coder.v1.toml"),
        "verifier" => include_str!("../../../fixtures/role-packs/verifier.v1.toml"),
        _ => return Err(RolePackError::InvalidField("role")),
    };
    RolePackV1::parse_toml(raw)
}

fn provider_family(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(provider, _)| provider)
}

fn role_pack_values(raw: &str) -> std::collections::HashMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn parse_string(
    values: &std::collections::HashMap<String, String>,
    key: &'static str,
) -> Result<String, RolePackError> {
    let value = values
        .get(key)
        .and_then(|value| value.strip_prefix('"')?.strip_suffix('"'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(RolePackError::InvalidField(key))?;
    Ok(value.to_string())
}

fn parse_array(
    values: &std::collections::HashMap<String, String>,
    key: &'static str,
) -> Result<Vec<String>, RolePackError> {
    let value = values
        .get(key)
        .and_then(|value| value.strip_prefix('[')?.strip_suffix(']'))
        .ok_or(RolePackError::InvalidField(key))?;
    let items = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            item.strip_prefix('"')
                .and_then(|item| item.strip_suffix('"'))
                .map(str::to_string)
                .ok_or(RolePackError::InvalidField(key))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if items.is_empty() {
        return Err(RolePackError::InvalidField(key));
    }
    Ok(items)
}

fn parse_u32(
    values: &std::collections::HashMap<String, String>,
    key: &'static str,
) -> Result<u32, RolePackError> {
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or(RolePackError::InvalidField(key))
}

fn parse_u64(
    values: &std::collections::HashMap<String, String>,
    key: &'static str,
) -> Result<u64, RolePackError> {
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or(RolePackError::InvalidField(key))
}

/// Traceable routing output for audit logs and UI explanations.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    pub task_id: String,
    pub agent_name: String,
    pub role: String,
    pub model_id: String,
    pub matched_by: String,
    pub escalation_path: Vec<String>,
    pub jury_panel: Option<ReviewPanel>,
    pub council_panel: Option<ReviewPanel>,
    pub rationale: String,
}

/// Routes tasks to agents based on role then capability.
#[derive(Debug, Default)]
pub struct AgentRouter;

impl AgentRouter {
    pub fn new() -> Self {
        Self
    }

    /// Choose the best agent for a single task.
    ///
    /// Preference order:
    /// 1. An agent whose `role` equals the task's `assigned_role`.
    /// 2. A duration/category rule that maps the task to a live role.
    /// 3. An agent whose `capabilities` contain the task's `category`.
    /// 4. None.
    pub fn route<'a>(
        &self,
        registry: &'a AgentRegistry,
        task: &TaskNode,
    ) -> Option<&'a AgentDescriptor> {
        self.select(registry, task).map(|(agent, _, _)| agent)
    }

    pub fn route_with_trace(
        &self,
        registry: &AgentRegistry,
        task: &TaskNode,
    ) -> Option<RoutingDecision> {
        let (agent, matched_by, rationale) = self.select(registry, task)?;

        Some(RoutingDecision {
            task_id: task.id.clone(),
            agent_name: agent.name.clone(),
            role: agent.role.clone(),
            model_id: agent.model_id.clone(),
            matched_by,
            escalation_path: agent.escalation_path.clone(),
            jury_panel: jury_for_category(&task.category),
            council_panel: council_for_category(&task.category),
            rationale,
        })
    }

    fn select<'a>(
        &self,
        registry: &'a AgentRegistry,
        task: &TaskNode,
    ) -> Option<(&'a AgentDescriptor, String, String)> {
        if !task.assigned_role.is_empty() {
            if let Some(a) = registry.all().iter().find(|a| a.role == task.assigned_role) {
                return Some((
                    a,
                    "assigned_role".into(),
                    format!(
                        "{} selected because task {} explicitly assigned role '{}'.",
                        a.name, task.id, task.assigned_role
                    ),
                ));
            }
        }
        if let Some(role) = category_duration_role(task) {
            if let Some(a) = registry.all().iter().find(|a| a.role == role) {
                return Some((
                    a,
                    "category_duration".into(),
                    format!(
                        "{} selected for category '{}' and duration '{}'; mapped to role '{}'.",
                        a.name,
                        task.category,
                        display_duration(task),
                        role
                    ),
                ));
            }
        }
        if !task.category.is_empty() {
            if let Some(a) = registry
                .all()
                .iter()
                .find(|a| a.capabilities.iter().any(|c| c == &task.category))
            {
                return Some((
                    a,
                    "capability".into(),
                    format!(
                        "{} selected because it advertises capability '{}'.",
                        a.name, task.category
                    ),
                ));
            }
        }
        None
    }
}

fn category_duration_role(task: &TaskNode) -> Option<&'static str> {
    let category = task.category.trim().to_ascii_lowercase();
    let long = matches!(
        task.estimated_duration.trim().to_ascii_lowercase().as_str(),
        "long" | "very-long" | "very_long" | "verylong"
    );

    match category.as_str() {
        "orchestration" | "routing" => Some("conductor"),
        "planning" | "decomposition" => Some("planner"),
        "architecture" | "api-design" | "schema" | "system-design" | "large-context"
        | "repo-wide" => Some("architect"),
        "code"
        | "coding"
        | "implementation"
        | "rust"
        | "typescript"
        | "python"
        | "refactoring"
        | "complex-implementation" => {
            if long {
                Some("engineer")
            } else {
                Some("coder")
            }
        }
        "ui" | "frontend" | "styling" | "components" | "static-site" | "react" | "css"
        | "ui-components" | "accessibility" => Some("ui-coder"),
        "product" | "requirements" | "ux" | "scope" | "claims" | "acceptance-criteria" => {
            Some("product-analyst")
        }
        "product-signoff" | "claim-approval" | "launch-gate" | "positioning" | "release-scope" => {
            Some("product-lead")
        }
        "research" | "topic-research" | "ux-research" | "user-research" | "market-research"
        | "domain-research" | "developer-research" | "synthesis" | "citations" => {
            Some("researcher")
        }
        "audit-kimi" | "repo-audit-kimi" | "audit-stack-kimi" | "evidence-audit-kimi" => {
            Some("auditor-kimi")
        }
        "audit" | "evidence-audit" | "claim-audit" | "claim-verification" | "readiness-audit"
        | "doc-drift" | "dead-code" | "schema-drift" | "security-smell" | "code-review"
        | "quality" | "pr-analysis" | "best-practices" | "security" | "threat-modeling" => {
            Some("auditor")
        }
        "review"
        | "plan-review"
        | "architecture-review"
        | "review-plan"
        | "plan-jury"
        | "adversarial-plan"
        | "failure-modes" => Some("challenger"),
        "visual-qa"
        | "visual-review"
        | "screenshot"
        | "ui-review"
        | "design-review"
        | "product-design-audit"
        | "ux-audit" => Some("lens"),
        "content" | "copywriting" | "seo" | "marketing" => Some("writer"),
        "watchdog" | "nudge" => Some("watchdog"),
        "completion" | "verification" | "arbiter" => Some("signoff"),
        _ => None,
    }
}

fn display_duration(task: &TaskNode) -> &str {
    if task.estimated_duration.trim().is_empty() {
        "unspecified"
    } else {
        task.estimated_duration.as_str()
    }
}

#[cfg(test)]
#[cfg(test)]
#[path = "router_tests.rs"]
mod router_tests;
