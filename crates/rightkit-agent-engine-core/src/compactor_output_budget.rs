// Output-budget reservation for the compaction summary.
//
// Long-agent sessions must reserve enough of the model's output budget for the
// compacted summary so the engine can always request the full digest without
// truncation. The reservation is `max(model_max_output, MIN_SUMMARY_TOKENS)`,
// matching the language in `DEFAULT_COMPACTION_PROMPT`. Sibling include!
// module: no behavior side-effects; pure helpers.

/// Minimum number of tokens reserved for the compaction summary, regardless
/// of the active model's own `max_output` cap.
pub const MIN_SUMMARY_TOKENS: u32 = 20_000;

/// Env var override exposing the active model's own output cap. When unset,
/// the reservation is `MIN_SUMMARY_TOKENS` (20_000).
pub const MODEL_MAX_OUTPUT_TOKENS_ENV: &str = "RIGHTKIT_MODEL_MAX_OUTPUT";

/// Returns the token reservation for the compaction summary, equal to
/// `max(model_max_output, MIN_SUMMARY_TOKENS)`. When `model_max_output` is
/// `None`, the reservation is `MIN_SUMMARY_TOKENS`.
///
/// This matches the integer-arithmetic form written into
/// `default_compaction.rs`'s `DEFAULT_COMPACTION_PROMPT` (see
/// `default_compaction_tests::default_prompt_requires_analysis_summary_and_large_budget`).
pub fn reserved_summary_tokens(model_max_output: Option<u32>) -> u32 {
    match model_max_output {
        Some(cap) => cap.max(MIN_SUMMARY_TOKENS),
        None => MIN_SUMMARY_TOKENS,
    }
}

/// Reads `RIGHTKIT_MODEL_MAX_OUTPUT` from the environment and returns the
/// effective reservation. Falls back to [`MIN_SUMMARY_TOKENS`] if the env
/// var is unset, malformed, or zero.
pub fn reserved_summary_tokens_from_env() -> u32 {
    let parsed = std::env::var(MODEL_MAX_OUTPUT_TOKENS_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|v| *v > 0);
    reserved_summary_tokens(parsed)
}

#[cfg(test)]
#[path = "compactor_output_budget_tests.rs"]
mod output_budget_tests;

// ---------------------------------------------------------------------------
// Mechanism 9: core safety pass, always last.
// ---------------------------------------------------------------------------

/// Hard ceiling on wire-bound compaction summary text, independent of which
/// strategy produced it (authoritative LLM summary, a reused prefire cache
/// hit, a token-budget digest, or cheap-first pruning).
pub const HARD_SAFETY_CHAR_CEILING: usize = 400_000;

/// Mechanism 9 (core safety pass, always last). Whatever compaction strategy
/// ran, this normalization is applied to its output before it can reach the
/// provider: trims surrounding whitespace and, only if the result still
/// exceeds [`HARD_SAFETY_CHAR_CEILING`], hard-truncates with a marker stating
/// the original size and the cap. Idempotent -- running it twice on its own
/// output is a no-op the second time.
pub fn core_safety_pass(text: &str) -> String {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    if char_count <= HARD_SAFETY_CHAR_CEILING {
        return trimmed.to_string();
    }

    // The marker has to fit *inside* the ceiling, not be appended past it.
    // Truncating to the ceiling and then appending produced output longer than
    // the ceiling, so a second pass saw it as oversized and truncated again --
    // eating real content each time. Since this pass is defined to run last
    // after every strategy, a double application is a normal occurrence, not an
    // edge case.
    let marker = format!(
        "\n...[core safety pass: truncated from {char_count} chars to fit a {HARD_SAFETY_CHAR_CEILING} char cap]...",
    );
    let marker_len = marker.chars().count();
    let keep = HARD_SAFETY_CHAR_CEILING.saturating_sub(marker_len);
    let kept: String = trimmed.chars().take(keep).collect();
    format!("{kept}{marker}")
}

#[cfg(test)]
mod core_safety_pass_tests {
    use super::*;

    #[test]
    fn under_ceiling_is_unchanged_besides_trim() {
        let out = core_safety_pass("  a short summary  ");
        assert_eq!(out, "a short summary");
    }

    #[test]
    fn over_ceiling_is_truncated_with_marker_stating_size_and_cap() {
        let huge = "x".repeat(HARD_SAFETY_CHAR_CEILING + 500);
        let out = core_safety_pass(&huge);
        assert!(out.len() < huge.len());
        assert!(out.contains(&format!(
            "truncated from {}",
            HARD_SAFETY_CHAR_CEILING + 500
        )));
        assert!(out.contains(&HARD_SAFETY_CHAR_CEILING.to_string()));
    }

    #[test]
    fn core_safety_pass_is_idempotent() {
        let huge = "y".repeat(HARD_SAFETY_CHAR_CEILING + 500);
        let once = core_safety_pass(&huge);
        let twice = core_safety_pass(&once);
        assert_eq!(once, twice);
    }

    /// The property that makes idempotence possible, asserted directly: the
    /// marker is accounted for inside the cap rather than appended past it.
    #[test]
    fn truncated_output_fits_within_the_cap_including_its_marker() {
        let huge = "z".repeat(HARD_SAFETY_CHAR_CEILING * 2);
        let out = core_safety_pass(&huge);
        assert!(
            out.chars().count() <= HARD_SAFETY_CHAR_CEILING,
            "output is {} chars, over the {HARD_SAFETY_CHAR_CEILING} cap",
            out.chars().count()
        );
    }
}
