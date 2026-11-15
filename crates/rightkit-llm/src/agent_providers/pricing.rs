//! Model pricing and cost estimation. Prices are USD per 1M tokens, sourced
//! from the loadable [`crate::agent_providers::model_metadata`] table (bundled
//! `assets/model-metadata.toml`, overridable via `<config-dir>/model-metadata.toml`).
//! Unknown models fall back to a conservative default so a missing entry never
//! silently reports zero cost.
//!
//! The cost arithmetic is `crate::PriceTable` (one row per lookup, built
//! from the the host app catalog row that `capability_of` resolved). This module
//! only owns the the host app policy around it: which catalog row applies (the
//! provider/model slug identity incl. the one native alias), the conservative
//! default for an unpriced model, and the per-1k / task-footprint helpers.
//!
//! RIGHTKIT-SERIOUS: the model catalog (compaction/reasoning/tier) is not in RightKit; see `model_metadata.rs` (report P-5)

use crate::{ModelPrice, PriceTable, Usage};

use crate::agent_providers::model_metadata::{capability_of, ModelCapability};

/// Conservative fallback when a model isn't in the table.
const DEFAULT_PROMPT_PER_M: f64 = 1.0;
const DEFAULT_COMPLETION_PER_M: f64 = 3.0;

/// Fallback context window when a model isn't in the table.
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 32_768;

fn price_of(model: &str) -> (f64, f64) {
    capability_of(model)
        .map(|m| (m.prompt_per_m, m.completion_per_m))
        .unwrap_or((DEFAULT_PROMPT_PER_M, DEFAULT_COMPLETION_PER_M))
}

/// Prompt-cache price multipliers over the input price, `(read, write)`, where they differ from the
/// table default (read 0.1x, write 1.25x). The write is the 5-minute rate (1.25x); the engine adds the
/// 1-hour premium (2x in all) only on requests that asked for an hour
/// (the engine's one-hour cache write factor). Opus 5.5 and Sonnet 5.5 read at 0.05x,
/// Fable and Mythos 5.1 and Fable 5.2 at 0.025x (Anthropic's pricing table, 2026-11-03; Fable 5.2 on 2026-11-09). Ids routed
/// through a gateway carry a vendor prefix (`anthropic/claude-...`) and keep the default.
fn cache_multipliers(slug: &str) -> (Option<f64>, Option<f64>) {
    if !slug.starts_with("claude-") {
        return (None, None);
    }
    let read = if slug.contains("sonnet-5-5") || slug.contains("opus-5-5") {
        Some(0.05)
    } else if slug.contains("fable-5-1")
        || slug.contains("mythos-5-1")
        || slug.contains("fable-5-2")
    {
        Some(0.025)
    } else {
        None
    };
    (read, Some(1.25))
}

/// USD cost of `usage` at the given per-1M prices, computed by rightkit-llm's
/// `PriceTable`. `None` only when the row is rejected as invalid (negative or
/// non-finite price in a user override file).
fn table_cost(slug: &str, prompt_per_m: f64, completion_per_m: f64, usage: &Usage) -> Option<f64> {
    let table = PriceTable::from_rows(vec![ModelPrice {
        slug: slug.to_string(),
        input_per_m: prompt_per_m,
        output_per_m: completion_per_m,
        context_window: None,
        cache_read_multiplier: cache_multipliers(slug).0,
        cache_write_multiplier: cache_multipliers(slug).1,
    }])
    .ok()?;
    table.cost(slug, usage)
}

fn row_cost(row: &ModelCapability, usage: &Usage) -> Option<f64> {
    table_cost(&row.slug, row.prompt_per_m, row.completion_per_m, usage)
}

/// True when `model` has a curated price entry in the table. Used to
/// distinguish "priced at the conservative default" from "genuinely unknown,
/// report null cost" for per-session cost attribution.
pub fn is_priced_model(model: &str) -> bool {
    capability_of(model).is_some()
}

/// Prompt and completion price per 1k tokens for a model (used to seed the
/// engine's `UsageAccount`).
pub fn price_per_1k(model: &str) -> (f64, f64) {
    let (p, c) = price_of(model);
    (p / 1000.0, c / 1000.0)
}

/// Cache read and write price per 1k tokens for a model, or `None` when the model is unpriced or is not
/// a native Claude id (gateway ids keep unknown cache pricing, so cost stays "unknown" rather than guessed).
/// Writes are priced at the 5-minute rate; the engine adds the 1-hour premium per request that asked
/// for an hour.
pub fn cache_price_per_1k(model: &str) -> Option<(f64, f64)> {
    // The metadata table only holds vendor-prefixed slugs (`anthropic/claude-...`), so the native
    // check is on the id the caller routes with, not on the matched row's slug.
    capability_of(model)?;
    if !model.starts_with("claude-") {
        return None;
    }
    let (prompt_per_m, _) = price_of(model);
    let (read_multiplier, write_multiplier) = cache_multipliers(model);
    Some((
        prompt_per_m / 1000.0 * read_multiplier.unwrap_or(0.1),
        prompt_per_m / 1000.0 * write_multiplier.unwrap_or(1.25),
    ))
}

/// The long-prompt tier of a model: `(threshold in input tokens, prompt multiplier, completion
/// multiplier)` over the base rates, or `None` when the model is billed at one rate. A request whose
/// input (uncached plus cache read and write) exceeds the threshold is billed at the multiples.
pub fn long_context_tier(model: &str) -> Option<(u64, f64, f64)> {
    let row = capability_of(model)?;
    let threshold = row.long_context_threshold_tokens?;
    let prompt = row.long_prompt_per_m? / row.prompt_per_m;
    let completion = row.long_completion_per_m? / row.completion_per_m;
    (prompt.is_finite() && completion.is_finite() && prompt > 0.0 && completion > 0.0)
        .then_some((threshold, prompt, completion))
}

/// Estimate the USD cost of a turn.
pub fn estimate_cost(model: &str, prompt_tokens: u32, completion_tokens: u32) -> f64 {
    let usage = Usage {
        input_tokens: u64::from(prompt_tokens),
        output_tokens: u64::from(completion_tokens),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    capability_of(model)
        .and_then(|row| row_cost(row, &usage))
        .or_else(|| {
            table_cost(
                "default",
                DEFAULT_PROMPT_PER_M,
                DEFAULT_COMPLETION_PER_M,
                &usage,
            )
        })
        .unwrap_or(0.0)
}

/// Context window size in tokens for a model slug. Falls back to 32k if unknown.
pub fn context_window_tokens(model: &str) -> u64 {
    capability_of(model)
        .map(|m| m.context_window_tokens)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

/// Compaction-relevant model capability. Unknown models are deliberately
/// `None`: the caller must surface unknown capacity and disable automatic
/// lossy context decisions instead of trusting the generic 32k compatibility
/// fallback returned by [`context_window_tokens`].
pub fn compaction_capability(
    model: &str,
) -> Option<crate::agent_providers::model_metadata::ModelCapability> {
    capability_of(model).cloned()
}

/// Typical token footprint of a single agent task, for cost preflight when
/// actual usage isn't known yet.
pub const TASK_PROMPT_TOKENS: u32 = 6_000;
pub const TASK_COMPLETION_TOKENS: u32 = 2_000;

/// Projected cost of one task executed by `model`.
pub fn projected_task_cost(model: &str) -> f64 {
    estimate_cost(model, TASK_PROMPT_TOKENS, TASK_COMPLETION_TOKENS)
}
