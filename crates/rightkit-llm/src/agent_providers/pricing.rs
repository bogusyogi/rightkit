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

/// USD cost of `usage` at the given per-1M prices, computed by rightkit-llm's
/// `PriceTable`. `None` only when the row is rejected as invalid (negative or
/// non-finite price in a user override file).
fn table_cost(slug: &str, prompt_per_m: f64, completion_per_m: f64, usage: &Usage) -> Option<f64> {
    let table = PriceTable::from_rows(vec![ModelPrice {
        slug: slug.to_string(),
        input_per_m: prompt_per_m,
        output_per_m: completion_per_m,
        context_window: None,
        cache_read_multiplier: None,
        cache_write_multiplier: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_priced_from_table() {
        // deepseek-v4-pro: 0.435 + 0.87 per 1M tokens (see model-metadata.toml).
        let c = estimate_cost("deepseek/deepseek-v4-pro", 1_000_000, 1_000_000);
        assert!((c - 1.305).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn unknown_model_uses_default() {
        let c = estimate_cost("acme/mystery", 1_000_000, 0);
        assert!((c - DEFAULT_PROMPT_PER_M).abs() < 1e-9);
    }

    #[test]
    fn is_priced_model_distinguishes_known_from_default_fallback() {
        assert!(is_priced_model("deepseek/deepseek-v4-pro"));
        assert!(!is_priced_model("acme/mystery"));
    }

    #[test]
    fn expensive_model_costs_more_than_cheap() {
        let pricey = projected_task_cost("qwen/qwen3.7-max");
        let cheap = projected_task_cost("deepseek/deepseek-v4-flash");
        assert!(pricey > cheap);
    }

    #[test]
    fn price_per_1k_scales_from_per_m() {
        let (p, _) = price_per_1k("deepseek/deepseek-v4-pro");
        assert!((p - 0.000435).abs() < 1e-12);
    }

    #[test]
    fn grok_build_uses_curated_openrouter_pricing_and_window() {
        let c = estimate_cost("x-ai/grok-build-0.1", 1_000_000, 1_000_000);
        assert!((c - 3.0).abs() < 1e-9);
        assert_eq!(context_window_tokens("x-ai/grok-build-0.1"), 256_000);
    }

    #[test]
    fn final_roster_models_have_curated_prices() {
        let models = [
            "z-ai/glm-5.2",
            "moonshotai/kimi-k2.6",
            "moonshotai/kimi-k2.5",
            "minimax/minimax-m3",
            "xiaomi/mimo-v2.5",
            "xiaomi/mimo-v2.5-pro",
            "qwen/qwen3.7-max",
            "qwen/qwen3.7-plus",
            "qwen/qwen3-vl-235b-a22b-instruct",
            "deepseek/deepseek-v4-pro",
            "deepseek/deepseek-v4-flash",
            "z-ai/glm-4.7",
        ];

        for model in models {
            assert_ne!(
                price_of(model),
                (DEFAULT_PROMPT_PER_M, DEFAULT_COMPLETION_PER_M),
                "{model} should not fall back to the default price"
            );
            assert_ne!(
                context_window_tokens(model),
                DEFAULT_CONTEXT_WINDOW_TOKENS,
                "{model} should not fall back to the default window"
            );
        }
    }

    /// Onboarding surfaces these slugs on the model-selection step
    /// (host-app/src/lib/providerPresets.ts) — they must be priced,
    /// not silently defaulted (ADR §D4-R1a).
    #[test]
    fn onboarding_surfaced_models_have_curated_prices() {
        let models = [
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-5.1",
            "google/gemini-3-pro",
            "deepseek/deepseek-v4",
        ];

        for model in models {
            assert!(is_priced_model(model), "{model} must have a metadata entry");
        }
    }

    /// The current frontier models referenced by this lane's seed task.
    #[test]
    fn frontier_2026_07_models_have_curated_prices() {
        assert!(is_priced_model("anthropic/claude-sonnet-5"));
        assert!(is_priced_model("openai/gpt-5.6"));
    }

    /// Every model_id used by the builtin agent roster must have a metadata
    /// entry — fails on drift between the roster and the pricing table
    /// (this caught the glm-5.1-vs-glm-5.2 mismatch during this lane's work).
    #[test]
    fn roster_sync_every_builtin_model_id_is_priced() {
        for model_id in host_roster_model_ids_for_test() {
            assert!(
                is_priced_model(model_id),
                "builtin roster model_id {model_id:?} has no model-metadata.toml entry"
            );
        }
    }

    /// Mirrors the model_id set declared across
    /// engine/crates/orchestration/src/builtins/roster/*.rs. Kept here (rather
    /// than depending on the orchestration crate, which would be a circular
    /// dependency onto `host_providers`) as a small hand-maintained list;
    /// the roster is small and changes rarely, and this test's whole purpose
    /// is to catch drift between the two.
    fn host_roster_model_ids_for_test() -> &'static [&'static str] {
        &[
            "z-ai/glm-5.2",
            "moonshotai/kimi-k2.6",
            "minimax/minimax-m3",
            "moonshotai/kimi-k2.5",
            "qwen/qwen3-vl-235b-a22b-instruct",
            "qwen/qwen3.7-max",
            "qwen/qwen3.7-plus",
            "xiaomi/mimo-v2.5",
            "xiaomi/mimo-v2.5-pro",
            "z-ai/glm-4.7",
            "deepseek/deepseek-v4-flash",
            "deepseek/deepseek-v4-pro",
        ]
    }
}
