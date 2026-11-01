//! L3 code skeletonizer — extract signatures/decls from source files.
//!
//! v1 supports a small set of common languages (Rust/Python/JS/TS). Unsupported
//! extensions pass through unchanged (parity with the workspace `skel.py`
//! fallback).

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetSkeleton {
    pub text: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub budget_tokens: usize,
    pub level: &'static str,
    pub budget_met: bool,
    /// Present only when full source bytes were durably published to a
    /// resolvable recovery handle. Pure skeletonization does not publish.
    pub recovery_marker: Option<crate::pull::compress::RecoveryMarkerV1>,
}

pub fn skeletonize_with_spans(
    path: &Path,
    src: &str,
) -> (String, Vec<super::fidelity::SpanMapping>) {
    super::ast::render(path, src)
}
pub fn skeletonize(path: &Path, src: &str) -> String {
    skeletonize_with_spans(path, src).0
}
pub fn skeletonize_to_budget(path: &Path, src: &str, budget_tokens: usize) -> BudgetSkeleton {
    let input_tokens = super::compress::estimate_tokens(src);
    let candidate = skeletonize(path, src);
    let candidate_tokens = super::compress::estimate_tokens(&candidate);
    let (text, level) = if candidate_tokens <= budget_tokens {
        (candidate, "signature")
    } else {
        // A path alone is not a retained original. Keep exact source and make
        // the unsatisfied budget observable to the final delivery owner.
        (src.to_owned(), "exact-over-budget")
    };
    let output_tokens = super::compress::estimate_tokens(&text);
    BudgetSkeleton {
        text,
        input_tokens,
        output_tokens,
        budget_tokens,
        level,
        budget_met: output_tokens <= budget_tokens,
        recovery_marker: None,
    }
}
