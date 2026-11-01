//! Guardrail replay harness — runs the exact acceptance filter chain the
//! production app-polish lane applies to LLM output, without calling any LLM.
//! Mirrors the inner `normalize` closure of `app_polish_outcome` in
//! `l3_cleanup/lanes.rs` verbatim (same fns, same order, same
//! 1_200 extra-chars budget). Used offline to check whether generated corpus
//! (input_text, output_text) targets would survive the shipped guardrails.
//!
//! Visibility note: `normalize_output_with_extra`, `scrub_dashes`, and
//! `no_unsupported_numeric_value` are defined in `l3_cleanup/acceptance.rs`
//! and re-exported as `pub(crate)` through `crate::l3_cleanup` (see
//! `src/l3_cleanup.rs`). They were private to the merged include! module
//! (unreachable from a sibling module even within the same crate), so this
//! change bumped their visibility to `pub(crate)` — a visibility-only edit,
//! no behavior change — so this harness calls the real implementations
//! instead of duplicating their logic.

use crate::l3_cleanup::{
    no_unsupported_numeric_value, normalize_output_with_extra, scrub_dashes, strip_field_text_echo,
};

/// One row's outcome from replaying the production guardrail chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardrailOutcome {
    Pass { accepted_text: String },
    Fail { reason: &'static str },
}

/// Replays the exact filter chain `app_polish_outcome` applies to LLM output
/// before accepting it into the app-polish lane:
///
/// ```text
/// normalize_output_with_extra(input, output, 1_200)
///   -> scrub_dashes
///   -> strip_field_text_echo
///   -> no_unsupported_numeric_value (reject filter)
/// ```
///
/// A row fails with the reason of the first failing step; a row that
/// survives all steps passes and returns the post-normalization accepted
/// text (what would actually have been typed into the user's app).
pub fn replay_guardrails(input: &str, output: &str, field_text: Option<&str>) -> GuardrailOutcome {
    let Some(normalized) = normalize_output_with_extra(input, output, 1_200) else {
        return GuardrailOutcome::Fail {
            reason: "normalize_output_with_extra_rejected",
        };
    };
    let scrubbed = scrub_dashes(&normalized);
    let deechoed = field_text
        .map(|field| strip_field_text_echo(field, input, &scrubbed))
        .unwrap_or(scrubbed);
    if !no_unsupported_numeric_value(input, &deechoed) {
        return GuardrailOutcome::Fail {
            reason: "no_unsupported_numeric_value_rejected",
        };
    }
    GuardrailOutcome::Pass {
        accepted_text: deechoed,
    }
}
