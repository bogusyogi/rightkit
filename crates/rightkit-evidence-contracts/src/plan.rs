use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    canonical_digest,
    id::{NodeId, PlanId, ProviderId},
    receipt::ExecutorBindingOutcome,
    require_version, ContractError,
};

/// The semantic interpretation required by an executor. The wire spelling
/// matches the `executorRequirement` object used by dispatch packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionSemanticRequirement {
    Forbidden,
    Conditional,
    Required,
}

/// One independently checkable completion condition for an executor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCompletionCheck {
    pub kind: String,
    pub id: String,
}

/// Bounded escalation policy for an executor requirement.
///
/// Outcomes deliberately reuse the receipt vocabulary so a host can carry a
/// plan requirement into an `ExecutorBindingReceiptV1` without translating
/// outcome names. In particular, `Denied` is never an escalation trigger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionEscalationPolicy {
    pub permitted_on: Vec<ExecutorBindingOutcome>,
    pub forbidden_on: Vec<ExecutorBindingOutcome>,
}

/// Portable, host-neutral executor requirements for one materialized node.
///
/// This is the LEG-MR-4 requirement shape. The canonical value lives on the
/// corresponding `PlanNode`; `Plan::executor_requirements` is only a
/// compatibility projection for consumers that still read the Option B map.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionRequirementV1 {
    pub semantic_requirement: ExecutionSemanticRequirement,
    pub capabilities: Vec<String>,
    /// Concrete operations permitted for this work unit. Operations refine
    /// broad effect classes so an executor never guesses intent from a
    /// capability or effect name.
    #[serde(default = "default_execution_operations")]
    pub operations: Vec<String>,
    pub effects: Vec<String>,
    pub authority_ceiling: Vec<String>,
    pub completion: Vec<ExecutionCompletionCheck>,
    pub escalation: ExecutionEscalationPolicy,
}

fn default_execution_operations() -> Vec<String> {
    vec!["*".into()]
}

impl ExecutionRequirementV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        for (path, values) in [
            ("capabilities", &self.capabilities),
            ("operations", &self.operations),
            ("effects", &self.effects),
            ("authority_ceiling", &self.authority_ceiling),
        ] {
            if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
                return Err(ContractError::InvalidContract {
                    path: path.into(),
                    reason: "must contain non-empty values".into(),
                });
            }
            let unique: BTreeSet<_> = values.iter().collect();
            if unique.len() != values.len() {
                return Err(ContractError::InvalidContract {
                    path: path.into(),
                    reason: "must not contain duplicate values".into(),
                });
            }
        }
        if self.completion.is_empty()
            || self
                .completion
                .iter()
                .any(|check| check.kind.trim().is_empty() || check.id.trim().is_empty())
        {
            return Err(ContractError::InvalidContract {
                path: "completion".into(),
                reason: "must contain checks with non-empty kind and id".into(),
            });
        }
        let completion_ids: BTreeSet<_> = self.completion.iter().map(|check| &check.id).collect();
        if completion_ids.len() != self.completion.len() {
            return Err(ContractError::InvalidContract {
                path: "completion".into(),
                reason: "check ids must be unique".into(),
            });
        }
        for (path, outcomes) in [
            ("escalation.permitted_on", &self.escalation.permitted_on),
            ("escalation.forbidden_on", &self.escalation.forbidden_on),
        ] {
            if outcomes
                .iter()
                .enumerate()
                .any(|(index, outcome)| outcomes[..index].contains(outcome))
            {
                return Err(ContractError::InvalidContract {
                    path: path.into(),
                    reason: "must not contain duplicate outcomes".into(),
                });
            }
        }
        if self
            .escalation
            .permitted_on
            .iter()
            .any(|outcome| self.escalation.forbidden_on.contains(outcome))
        {
            return Err(ContractError::InvalidContract {
                path: "escalation".into(),
                reason: "permitted and forbidden outcomes must not overlap".into(),
            });
        }
        if self
            .escalation
            .permitted_on
            .contains(&ExecutorBindingOutcome::Denied)
        {
            return Err(ContractError::InvalidContract {
                path: "escalation.permitted_on".into(),
                reason: "denied may never permit escalation".into(),
            });
        }
        if self.semantic_requirement == ExecutionSemanticRequirement::Forbidden
            && !self.escalation.permitted_on.is_empty()
        {
            return Err(ContractError::InvalidContract {
                path: "escalation.permitted_on".into(),
                reason: "forbidden semantic requirement cannot escalate".into(),
            });
        }
        if self.semantic_requirement == ExecutionSemanticRequirement::Conditional
            && self.escalation.permitted_on.is_empty()
        {
            return Err(ContractError::InvalidContract {
                path: "escalation.permitted_on".into(),
                reason: "conditional semantic requirement needs escalation outcomes".into(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeKind {
    Provider,
    Gate,
    Report,
    External,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanNode {
    pub id: NodeId,
    pub kind: PlanNodeKind,
    pub provider: Option<ProviderId>,
    pub depends_on: Vec<NodeId>,
    pub configuration: BTreeMap<String, serde_json::Value>,
    /// The single canonical executor requirement for this node, when one is
    /// declared. `Plan::executor_requirements` is derived from this field.
    #[serde(default)]
    pub executor_requirement: Option<ExecutionRequirementV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanSerde {
    #[serde(deserialize_with = "crate::deserialize_schema_version_1")]
    schema_version: u32,
    id: PlanId,
    nodes: Vec<PlanNode>,
    providers: Vec<ProviderId>,
    resources: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    executor_requirements: BTreeMap<NodeId, ExecutionRequirementV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub schema_version: u32,
    pub id: PlanId,
    pub nodes: Vec<PlanNode>,
    pub providers: Vec<ProviderId>,
    pub resources: BTreeMap<String, serde_json::Value>,
    /// Compatibility projection for Option B consumers. Exactly one value is
    /// canonical: the requirement on the matching `PlanNode`. On
    /// deserialization, a map entry populates that node only when its node
    /// field is absent; the node field wins on conflict. Serialization always
    /// projects node fields back into this map, so the map is never a source
    /// of truth.
    pub executor_requirements: BTreeMap<NodeId, ExecutionRequirementV1>,
}

fn projected_executor_requirements(nodes: &[PlanNode]) -> BTreeMap<NodeId, ExecutionRequirementV1> {
    nodes
        .iter()
        .filter_map(|node| {
            node.executor_requirement
                .as_ref()
                .map(|requirement| (node.id.clone(), requirement.clone()))
        })
        .collect()
}

impl Serialize for Plan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        PlanSerde {
            schema_version: self.schema_version,
            id: self.id.clone(),
            nodes: self.nodes.clone(),
            providers: self.providers.clone(),
            resources: self.resources.clone(),
            executor_requirements: projected_executor_requirements(&self.nodes),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Plan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut serialized = PlanSerde::deserialize(deserializer)?;
        let compatibility_requirements = std::mem::take(&mut serialized.executor_requirements);
        for node in &mut serialized.nodes {
            if node.executor_requirement.is_none() {
                node.executor_requirement = compatibility_requirements.get(&node.id).cloned();
            }
        }
        let executor_requirements = projected_executor_requirements(&serialized.nodes);
        Ok(Self {
            schema_version: serialized.schema_version,
            id: serialized.id,
            nodes: serialized.nodes,
            providers: serialized.providers,
            resources: serialized.resources,
            executor_requirements,
        })
    }
}

impl Plan {
    pub fn new(
        schema_version: u32,
        id: PlanId,
        nodes: Vec<PlanNode>,
        providers: Vec<ProviderId>,
    ) -> Result<Self, ContractError> {
        let executor_requirements = projected_executor_requirements(&nodes);
        let plan = Self {
            schema_version,
            id,
            nodes,
            providers,
            resources: BTreeMap::new(),
            executor_requirements,
        };
        plan.validate()?;
        Ok(plan)
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        require_version(self.schema_version, 1)?;
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.clone()) {
                return Err(ContractError::InvalidContract {
                    path: "nodes.id".into(),
                    reason: "duplicate node id".into(),
                });
            }
        }
        let provider_ids: BTreeSet<_> = self.providers.iter().collect();
        if provider_ids.len() != self.providers.len() {
            return Err(ContractError::InvalidContract {
                path: "providers".into(),
                reason: "duplicate provider id".into(),
            });
        }
        for node in &self.nodes {
            if node.depends_on.iter().any(|dep| !ids.contains(dep)) {
                return Err(ContractError::InvalidContract {
                    path: "nodes.depends_on".into(),
                    reason: "unknown dependency".into(),
                });
            }
        }
        for node in &self.nodes {
            if let Some(requirement) = &node.executor_requirement {
                requirement.validate().map_err(|error| match error {
                    ContractError::InvalidContract { path, reason } => {
                        ContractError::InvalidContract {
                            path: format!("nodes.executor_requirement.{}.{path}", node.id),
                            reason,
                        }
                    }
                    other => other,
                })?;
            }
        }
        // The public map remains available to old callers, but it is only a
        // compatibility projection. Check its keys for dangling references
        // without consulting its values as plan semantics.
        for node_id in self.executor_requirements.keys() {
            if !ids.contains(node_id) {
                return Err(ContractError::InvalidContract {
                    path: "executor_requirements".into(),
                    reason: "requirement references unknown node id".into(),
                });
            }
        }
        self.ordered_nodes().map(|_| ())
    }
    pub fn ordered_nodes(&self) -> Result<Vec<PlanNode>, ContractError> {
        let mut by_id = BTreeMap::new();
        for node in &self.nodes {
            by_id.insert(node.id.clone(), node.clone());
        }
        let mut remaining: BTreeSet<NodeId> = by_id.keys().cloned().collect();
        let mut done = BTreeSet::new();
        let mut output = Vec::with_capacity(self.nodes.len());
        while let Some(id) = remaining
            .iter()
            .find(|id| by_id[*id].depends_on.iter().all(|dep| done.contains(dep)))
            .cloned()
        {
            remaining.remove(&id);
            done.insert(id.clone());
            output.push(by_id[&id].clone());
        }
        if !remaining.is_empty() {
            return Err(ContractError::InvalidContract {
                path: "nodes".into(),
                reason: "cycle detected".into(),
            });
        }
        Ok(output)
    }
    pub fn digest(&self) -> Result<String, crate::canonical::CanonicalError> {
        canonical_digest(self)
    }
}
