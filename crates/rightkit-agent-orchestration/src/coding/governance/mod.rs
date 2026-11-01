//! The orchestration governance layer — allows or denies orchestration actions.

pub mod rules;

use rules::{BudgetRule, ConcurrencyRule, GovernanceRule, TrustRule};

/// The inputs a governance decision is made against.
#[derive(Debug, Clone)]
pub struct GovernanceContext {
    pub budget_used_usd: f64,
    pub budget_ceiling_usd: f64,
    pub active_workers: usize,
    pub max_workers: usize,
    pub task_is_destructive: bool,
    pub workspace_trusted: bool,
}

/// The outcome of a governance evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceVerdict {
    Allow,
    Deny { rule: String, reason: String },
}

/// Decision result of per-dispatch gating (budget + trust, consulted per task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    DenyBudget,
    DenyTrust,
}

impl GovernanceVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, GovernanceVerdict::Allow)
    }
}

/// Applies an ordered set of governance rules. First failing rule wins.
pub struct OrchestrationGovernance {
    rules: Vec<Box<dyn GovernanceRule>>,
}

impl Default for OrchestrationGovernance {
    fn default() -> Self {
        Self {
            rules: vec![
                Box::new(BudgetRule),
                Box::new(ConcurrencyRule),
                Box::new(TrustRule),
            ],
        }
    }
}

impl OrchestrationGovernance {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn evaluate(&self, ctx: &GovernanceContext) -> GovernanceVerdict {
        rules::evaluate_rules(&self.rules, ctx)
    }

    /// Per-dispatch gating: consults budget + trust for a single task before it
    /// is dispatched (in addition to the pre-run projected-cost check). All
    /// values are explicit so this can be called per worker dispatch without a
    /// full `GovernanceContext`. `task_cost_estimate`/budget are USD; trust is 0-255.
    pub fn gate_dispatch(
        &self,
        spent_usd: f64,
        budget_usd: f64,
        task_cost_estimate: f64,
        agent_trust_level: u8,
        required_trust: u8,
    ) -> GateDecision {
        if budget_usd > 0.0 && spent_usd + task_cost_estimate >= budget_usd {
            return GateDecision::DenyBudget;
        }
        if agent_trust_level < required_trust {
            return GateDecision::DenyTrust;
        }
        GateDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> GovernanceContext {
        GovernanceContext {
            budget_used_usd: 1.0,
            budget_ceiling_usd: 100.0,
            active_workers: 1,
            max_workers: 4,
            task_is_destructive: false,
            workspace_trusted: true,
        }
    }

    #[test]
    fn allow_when_all_ok() {
        let g = OrchestrationGovernance::default();
        assert_eq!(g.evaluate(&ctx()), GovernanceVerdict::Allow);
    }

    #[test]
    fn deny_over_budget() {
        let g = OrchestrationGovernance::default();
        let mut c = ctx();
        c.budget_used_usd = 100.0;
        match g.evaluate(&c) {
            GovernanceVerdict::Deny { rule, .. } => assert_eq!(rule, "budget"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn deny_too_many_workers() {
        let g = OrchestrationGovernance::default();
        let mut c = ctx();
        c.active_workers = 4;
        match g.evaluate(&c) {
            GovernanceVerdict::Deny { rule, .. } => assert_eq!(rule, "concurrency"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn deny_destructive_untrusted() {
        let g = OrchestrationGovernance::default();
        let mut c = ctx();
        c.task_is_destructive = true;
        c.workspace_trusted = false;
        match g.evaluate(&c) {
            GovernanceVerdict::Deny { rule, .. } => assert_eq!(rule, "trust"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn allow_destructive_when_trusted() {
        let g = OrchestrationGovernance::default();
        let mut c = ctx();
        c.task_is_destructive = true;
        c.workspace_trusted = true;
        assert_eq!(g.evaluate(&c), GovernanceVerdict::Allow);
    }

    #[test]
    fn first_failing_rule_wins() {
        let g = OrchestrationGovernance::default();
        // Both budget and concurrency would fail; budget is checked first.
        let mut c = ctx();
        c.budget_used_usd = 200.0;
        c.active_workers = 10;
        match g.evaluate(&c) {
            GovernanceVerdict::Deny { rule, .. } => assert_eq!(rule, "budget"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn budget_enforcement() {
        // Required by spec: budget_used >= ceiling -> Deny naming the budget rule.
        let g = OrchestrationGovernance::default();
        let c = GovernanceContext {
            budget_used_usd: 50.0,
            budget_ceiling_usd: 50.0,
            active_workers: 0,
            max_workers: 4,
            task_is_destructive: false,
            workspace_trusted: true,
        };
        match g.evaluate(&c) {
            GovernanceVerdict::Deny { rule, reason } => {
                assert_eq!(rule, "budget");
                assert!(!reason.is_empty());
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn gate_dispatch_allows_under_budget_and_trust() {
        let g = OrchestrationGovernance::default();
        assert_eq!(
            g.gate_dispatch(10.0, 100.0, 5.0, 100, 50),
            GateDecision::Allow
        );
    }

    #[test]
    fn gate_dispatch_denies_over_budget() {
        let g = OrchestrationGovernance::default();
        assert_eq!(
            g.gate_dispatch(95.0, 100.0, 10.0, 100, 50),
            GateDecision::DenyBudget
        );
    }

    #[test]
    fn gate_dispatch_denies_low_trust() {
        let g = OrchestrationGovernance::default();
        assert_eq!(
            g.gate_dispatch(10.0, 100.0, 5.0, 30, 50),
            GateDecision::DenyTrust
        );
    }
}
