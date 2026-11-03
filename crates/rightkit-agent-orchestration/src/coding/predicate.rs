//! Evaluation of task completion predicates (DoneWhen).
//!
//! A task's `done_when_predicates` are weighted boolean conditions. A task is
//! "done" when the satisfied weight fraction crosses a threshold. Predicates are
//! evaluated against the worker's output + a small evaluation context.

use rightkit_agent_engine_protocol::{CompletionPredicateResult, DoneWhenPredicate};

/// Context available to predicate evaluation.
pub struct PredicateContext<'a> {
    /// The worker's output text.
    pub output: &'a str,
    /// Whether the worker reported success.
    pub success: bool,
    /// Optional shell exit code if the task ran a command.
    pub exit_code: Option<i32>,
}

/// Evaluates DoneWhen predicates against a context.
pub struct PredicateEvaluator;

impl PredicateEvaluator {
    /// Evaluate a single predicate string against the context.
    /// Supported predicate forms (heuristic, extensible):
    ///   - "tests pass" / "tests_pass" → success && output mentions passing tests, no "FAILED"
    ///   - "exit 0" / "exit_zero" → exit_code == Some(0) or (success when no code)
    ///   - "contains:<substr>" → output contains substr (case-insensitive)
    ///   - "no_errors" → output has no "error"/"panic"/"failed"
    ///   - "compiles" → output mentions "Finished"/"compiled" and not "error["
    ///   - anything else → satisfied = success (fallback: trust worker success)
    pub fn evaluate_one(predicate: &str, ctx: &PredicateContext) -> bool {
        let p = predicate.trim().to_lowercase();
        let out = ctx.output.to_lowercase();
        if let Some(needle) = p.strip_prefix("contains:") {
            return out.contains(needle.trim());
        }
        match p.as_str() {
            "tests pass" | "tests_pass" | "tests passing" => {
                ctx.success && !out.contains("failed") && !out.contains("error[")
            }
            "exit 0" | "exit_zero" | "exit code 0" => {
                ctx.exit_code.map(|c| c == 0).unwrap_or(ctx.success)
            }
            "no_errors" | "no errors" => {
                !out.contains("error") && !out.contains("panic") && !out.contains("failed")
            }
            "compiles" | "builds" => {
                (out.contains("finished") || out.contains("compiled")) && !out.contains("error[")
            }
            "has_citations" | "cites_evidence" => has_citation(ctx.output),
            _ => ctx.success,
        }
    }

    /// Evaluate all predicates, returning per-predicate results.
    pub fn evaluate_all(
        predicates: &[DoneWhenPredicate],
        ctx: &PredicateContext,
    ) -> Vec<CompletionPredicateResult> {
        predicates
            .iter()
            .map(|dp| CompletionPredicateResult {
                predicate: dp.predicate.clone(),
                weight: dp.weight,
                satisfied: Self::evaluate_one(&dp.predicate, ctx),
            })
            .collect()
    }

    /// Returns the satisfied-weight fraction (0.0-1.0). If no predicates, returns
    /// 1.0 when the worker succeeded (free-text done_when → trust success), else 0.0.
    pub fn satisfied_fraction(
        results: &[CompletionPredicateResult],
        fallback_success: bool,
    ) -> f64 {
        if results.is_empty() {
            return if fallback_success { 1.0 } else { 0.0 };
        }
        let total: f64 = results.iter().map(|r| r.weight).sum();
        if total <= 0.0 {
            return if fallback_success { 1.0 } else { 0.0 };
        }
        let satisfied: f64 = results
            .iter()
            .filter(|r| r.satisfied)
            .map(|r| r.weight)
            .sum();
        satisfied / total
    }

    /// Whether the task should be considered Done given a completion threshold (default 1.0 = all).
    pub fn is_done(
        results: &[CompletionPredicateResult],
        fallback_success: bool,
        threshold: f64,
    ) -> bool {
        Self::satisfied_fraction(results, fallback_success) >= threshold
    }
}

/// True if the output contains at least one evidence citation: a URL, or a
/// `path.ext:line` / `path/seg:line` file:line marker.
fn has_citation(output: &str) -> bool {
    if output.contains("http://") || output.contains("https://") {
        return true;
    }
    output
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '`')
        .any(|tok| {
            tok.rsplit_once(':').is_some_and(|(path, rest)| {
                !path.is_empty()
                    && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && (path.contains('.') || path.contains('/') || path.contains('\\'))
            })
        })
}
