//! Per-verifier & aggregate budgets (PL-007).
//!
//! The canon row requires bounded verification: per-verifier budgets and an
//! aggregate budget, with a cap on how many verifier units may start and how
//! many findings one verifier may emit. Everything is wall-clock bounded so a
//! wedged language server or a hung typechecker can delay a turn by at most
//! the aggregate budget, and every unit that does not finish in time is
//! reported honestly as `Unavailable` — never as clean.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyBudgets {
    /// Wall-clock bound for the whole fan-out. Units still running when this
    /// elapses are cancelled and recorded `Unavailable` (aggregate-budget
    /// reason).
    pub aggregate: Duration,
    /// Wall-clock bound for one verifier unit. Applied on top of the
    /// aggregate; a unit may finish earlier but never later than this.
    pub per_verifier: Duration,
    /// Wall-clock bound for one file inside a multi-file unit (the LSP
    /// diagnostics op per file). Mirrors the post-edit diagnostics seam's
    /// per-file timeout.
    pub per_verifier_file: Duration,
    /// Maximum number of verifier units that may start. Overflow units are
    /// not started; one synthetic budget outcome records the shortfall.
    pub max_verifiers: usize,
    /// Maximum findings one verifier unit may contribute. A unit that hits
    /// the cap is marked truncated rather than silently cut.
    pub max_findings_per_verifier: usize,
}

impl Default for VerifyBudgets {
    fn default() -> Self {
        Self {
            aggregate: Duration::from_secs(30),
            per_verifier: Duration::from_secs(15),
            // Mirrors the 8s per-file bound the post-edit diagnostics seam
            // uses for the same `lsp diagnostics` op.
            per_verifier_file: Duration::from_secs(8),
            max_verifiers: 8,
            max_findings_per_verifier: 64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_per_file_stays_under_per_verifier() {
        let budgets = VerifyBudgets::default();
        assert!(budgets.per_verifier_file <= budgets.per_verifier);
        assert!(budgets.per_verifier <= budgets.aggregate);
        assert!(budgets.max_verifiers > 0);
        assert!(budgets.max_findings_per_verifier > 0);
    }
}
