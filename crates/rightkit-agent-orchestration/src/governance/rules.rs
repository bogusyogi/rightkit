//! Individual governance rules. Each rule inspects the context and may veto an action.

use super::{GovernanceContext, GovernanceVerdict};

/// A governance rule: returns `None` to allow, or `Some(reason)` to deny.
pub trait GovernanceRule {
    /// The stable name of this rule, surfaced in deny verdicts.
    fn name(&self) -> &'static str;
    /// Evaluate the rule. `Some(reason)` denies; `None` allows.
    fn check(&self, ctx: &GovernanceContext) -> Option<String>;
}

/// Denies when the budget ceiling has been reached or exceeded.
pub struct BudgetRule;

impl GovernanceRule for BudgetRule {
    fn name(&self) -> &'static str {
        "budget"
    }

    fn check(&self, ctx: &GovernanceContext) -> Option<String> {
        if ctx.budget_ceiling_usd > 0.0 && ctx.budget_used_usd >= ctx.budget_ceiling_usd {
            Some(format!(
                "budget used ${:.2} reached ceiling ${:.2}",
                ctx.budget_used_usd, ctx.budget_ceiling_usd
            ))
        } else {
            None
        }
    }
}

/// Denies when the maximum number of concurrent workers is reached.
pub struct ConcurrencyRule;

impl GovernanceRule for ConcurrencyRule {
    fn name(&self) -> &'static str {
        "concurrency"
    }

    fn check(&self, ctx: &GovernanceContext) -> Option<String> {
        if ctx.active_workers >= ctx.max_workers {
            Some(format!(
                "active workers {} reached max {}",
                ctx.active_workers, ctx.max_workers
            ))
        } else {
            None
        }
    }
}

/// Denies destructive tasks in an untrusted workspace.
pub struct TrustRule;

impl GovernanceRule for TrustRule {
    fn name(&self) -> &'static str {
        "trust"
    }

    fn check(&self, ctx: &GovernanceContext) -> Option<String> {
        if ctx.task_is_destructive && !ctx.workspace_trusted {
            Some("destructive task attempted in an untrusted workspace".into())
        } else {
            None
        }
    }
}

/// Run rules in priority order; the first failing rule wins.
pub fn evaluate_rules(
    rules: &[Box<dyn GovernanceRule>],
    ctx: &GovernanceContext,
) -> GovernanceVerdict {
    for rule in rules {
        if let Some(reason) = rule.check(ctx) {
            return GovernanceVerdict::Deny {
                rule: rule.name().to_string(),
                reason,
            };
        }
    }
    GovernanceVerdict::Allow
}
