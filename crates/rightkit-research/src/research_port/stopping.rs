//! Port of `research-core/stopping.py`: deterministic adaptive stopping for
//! Research acquisition, evaluated from persisted coverage, evidence gain,
//! and hook-metered budget.

use std::collections::BTreeSet;

/// Mirrors the keyword-argument payload accepted by `stopping.decide(...)`.
#[derive(Debug, Clone, Default)]
pub struct StoppingInput {
    pub required_questions: Vec<String>,
    pub answered_questions: Vec<String>,
    pub consecutive_no_gain: i64,
    pub external_requests_used: i64,
    pub external_requests_budget: i64,
    pub blocking_gaps: Vec<String>,
}

/// Mirrors the dict returned by `stopping.decide(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppingDecision {
    pub action: &'static str,
    pub reason: &'static str,
    pub complete: bool,
    pub unanswered: Vec<String>,
    pub blocking_gaps: Vec<String>,
}

/// Production entry point — same decision order as the Python:
/// budget exhaustion, then full coverage, then stagnation, else continue.
pub fn decide(input: &StoppingInput) -> StoppingDecision {
    let required: BTreeSet<&str> = input
        .required_questions
        .iter()
        .map(String::as_str)
        .collect();
    let answered: BTreeSet<&str> = input
        .answered_questions
        .iter()
        .map(String::as_str)
        .collect();
    let unanswered: Vec<String> = required
        .difference(&answered)
        .map(|s| s.to_string())
        .collect();
    let gaps: Vec<String> = input
        .blocking_gaps
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let budget_exhausted = input.external_requests_used >= input.external_requests_budget;

    if budget_exhausted {
        return StoppingDecision {
            action: "stop",
            reason: "budget-exhausted",
            complete: unanswered.is_empty() && gaps.is_empty(),
            unanswered,
            blocking_gaps: gaps,
        };
    }
    if unanswered.is_empty() && gaps.is_empty() {
        return StoppingDecision {
            action: "stop",
            reason: "coverage-complete",
            complete: true,
            unanswered: vec![],
            blocking_gaps: vec![],
        };
    }
    if input.consecutive_no_gain >= 2 {
        return StoppingDecision {
            action: "stop",
            reason: "no-material-gain",
            complete: false,
            unanswered,
            blocking_gaps: gaps,
        };
    }
    StoppingDecision {
        action: "continue",
        reason: "targeted-gap-search",
        complete: false,
        unanswered,
        blocking_gaps: gaps,
    }
}
