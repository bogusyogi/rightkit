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
