use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::model::{
    context::PolicyContext,
    effect::{ApprovalRequirement, ContractVersion, EffectClass, EnforcementLevel, TrustLevel},
    path::{CanonicalPath, PathOperation},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleDecision {
    Allow,
    Deny,
}

/// A predicate constrains a rule; an absent clause constrains nothing. Every
/// field therefore defaults, so a rule that matches on effect class alone is
/// written as `{"effect_class": "FILE_DELETE"}` rather than five empty sets.
/// Requiring them made the shipped pack unparseable, which took the MCP
/// server down at startup with `missing field `operations``.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RulePredicate {
    pub effect_class: Option<EffectClass>,
    pub operations: BTreeSet<PathOperation>,
    pub repositories: BTreeSet<String>,
    pub worktrees: BTreeSet<String>,
    pub path_prefixes: BTreeSet<String>,
    pub required_tags: BTreeSet<String>,
    pub contract: Option<ContractVersion>,
    pub trust: Option<TrustLevel>,
    pub approval: Option<ApprovalRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub schema_version: u32,
    pub id: String,
    pub effect_class: EffectClass,
    pub rule: RuleDecision,
    pub predicate: RulePredicate,
    pub approval_required: bool,
    pub trust_minimum: TrustLevel,
    pub required_enforcement: EnforcementLevel,
    pub receipt_required: bool,
    pub exception_capable: bool,
    pub note: Option<String>,
}

impl PolicyRule {
    pub fn matches(&self, ctx: &PolicyContext) -> bool {
        let p = &self.predicate;
        if self.effect_class != ctx.effect_class
            || p.effect_class
                .is_some_and(|effect| effect != ctx.effect_class)
        {
            return false;
        }
        if !p.operations.is_empty() && !p.operations.contains(&ctx.operation) {
            return false;
        }
        if !p.repositories.is_empty() && !p.repositories.contains(&ctx.repository) {
            return false;
        }
        if !p.worktrees.is_empty() && !p.worktrees.contains(&ctx.worktree) {
            return false;
        }
        if !p.required_tags.is_subset(&ctx.tags) {
            return false;
        }
        if let Some(contract) = &p.contract {
            if contract != &ctx.contract {
                return false;
            }
        }
        if let Some(trust) = p.trust {
            if !ctx.trust.satisfies(trust) {
                return false;
            }
        }
        if let Some(approval) = p.approval {
            if !ctx.approval_satisfies(approval) {
                return false;
            }
        }
        if !p.path_prefixes.is_empty() {
            let Some(path) = &ctx.path else {
                return false;
            };
            // Prefixes are projected into the request's canonical identity. This
            // keeps containment bound to root, repository/worktree scope, and
            // symlink resolution instead of treating a string prefix as scope.
            let matches = p
                .path_prefixes
                .iter()
                .filter_map(|prefix| {
                    CanonicalPath::from_relative(
                        path.root_identity.clone(),
                        path.scope.clone(),
                        prefix,
                        path.symlink.clone(),
                    )
                    .ok()
                })
                .any(|prefix| prefix.contains(path));
            if !matches {
                return false;
            }
        }
        true
    }
}
