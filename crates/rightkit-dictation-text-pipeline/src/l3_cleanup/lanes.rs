//! The three product lanes — L1 app polish, L2 prompt polish, L3 summary —
//! plus `transform_outcome`, the shared provider-fallback loop they all run
//! through: preflight gates, provider roster, circuit breaker, per-attempt
//! budgets, acceptance normalization, and success/failure accounting.

use std::sync::atomic::Ordering;
use std::time::Instant;

#[cfg(test)]
use parking_lot::Mutex;
use serde_json::Value;

use super::acceptance::{
    no_unsupported_numeric_value, normalize_output, normalize_output_with_extra, scrub_dashes,
    strip_field_text_echo,
};
use super::ops::{
    fair_attempt_budget, health, is_dictation_lane, lane_timeouts, max_input_chars,
    next_operation_id, provider_circuit_open, record_failure, record_skip, record_success,
    trace_attempt, trace_budget_exceeded, trace_result, trace_skip, trace_skip_with_operation,
};
use super::policy;
#[cfg(test)]
use super::prompts::PROMPT_VERSION;
use super::prompts::{PROMPT_POLISH_VERSION, SUMMARY_PROMPT_VERSION};
use super::providers::provider_specs;
#[cfg(test)]
use super::state::{CachedCleanup, CLEANUP_CACHE, CLEANUP_CACHE_MAX};
use super::state::{CleanupOutcome, PolishContext, Provider, ProviderSpec, ATTEMPTS, FAILURES};
use super::transport::{call_provider, emit_runtime_diagnostic};
#[cfg(test)]
use super::wire::build_payload;
use super::wire::{
    app_prompt_version, build_app_payload, build_prompt_payload, build_summary_payload,
    sanitize_context,
};

// Test-only entry point for the shared transform machinery (provider fallback,
// circuit breaker, cache). Not reachable from the product — the live lanes are
// app_polish_outcome / prompt_polish_outcome / summarize_outcome.
#[cfg(test)]
pub fn cleanup_outcome(input: &str) -> CleanupOutcome {
    let input = match preflight_input(input) {
        Ok(input) => input,
        Err(outcome) => return outcome,
    };
    if let Some(cleaned) = cached_cleanup(input) {
        trace_skip("cache_hit", "closed");
        return CleanupOutcome::Cleaned(cleaned);
    }

    transform_outcome(
        input,
        PROMPT_VERSION,
        "l3_cleanup_no_provider_available",
        "l3_cleanup_all_providers_failed",
        build_payload,
        normalize_output,
        store_cleanup,
    )
}

pub fn app_polish_outcome(input: &str, context: &PolishContext) -> CleanupOutcome {
    app_polish_outcome_with_receipts(input, context, None, None)
}

pub fn app_polish_outcome_with_receipts(
    input: &str,
    context: &PolishContext,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) -> CleanupOutcome {
    // Safety boundary: the transcript is user-dictated (consented by turning AI
    // polish on), but captured context is NOT — scrub secret-shaped window
    // titles / vocab terms and drop context entirely for password-manager apps
    // before anything leaves this function (Apple helper or cloud provider).
    let context = &sanitize_context(context);
    let prompt_version = app_prompt_version(context);
    transform_outcome(
        input,
        prompt_version,
        "l1_app_polish_no_provider_available",
        "l1_app_polish_all_providers_failed",
        |spec, input| {
            mark_context_request_attempt(request_attempted);
            let payload = build_app_payload(spec, input, context);
            mark_original_binding_from_payload(&payload, original_binding);
            payload
        },
        |input, output| {
            normalize_output_with_extra(input, output, 1_200)
                .map(|out| scrub_dashes(&out))
                .map(|out| match context.field_text.as_deref() {
                    Some(field) => strip_field_text_echo(field, input, &out),
                    None => out,
                })
                .filter(|out| no_unsupported_numeric_value(input, out))
        },
        |_, _| {},
    )
}

pub fn prompt_polish_outcome(input: &str, context: &PolishContext) -> CleanupOutcome {
    prompt_polish_outcome_with_receipts(input, context, None, None)
}

pub fn prompt_polish_outcome_with_receipts(
    input: &str,
    context: &PolishContext,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) -> CleanupOutcome {
    // Same safety boundary as app_polish_outcome: scrub captured context
    // before it reaches any provider payload.
    let context = &sanitize_context(context);
    transform_outcome(
        input,
        PROMPT_POLISH_VERSION,
        "l2_prompt_polish_no_provider_available",
        "l2_prompt_polish_all_providers_failed",
        |spec, input| {
            mark_context_request_attempt(request_attempted);
            let payload = build_prompt_payload(spec, input, context);
            mark_original_binding_from_payload(&payload, original_binding);
            payload
        },
        |input, output| {
            normalize_output_with_extra(input, output, 1_200)
                .map(|out| scrub_dashes(&out))
                .filter(|out| no_unsupported_numeric_value(input, out))
        },
        |_, _| {},
    )
}

fn transform_outcome<F, N, S>(
    input: &str,
    prompt_version: &'static str,
    no_provider_log: &'static str,
    failed_log: &'static str,
    build_payload: F,
    normalize: N,
    on_success: S,
) -> CleanupOutcome
where
    F: Fn(&ProviderSpec, &str) -> Value,
    N: Fn(&str, &str) -> Option<String>,
    S: Fn(&str, &str),
{
    let input = match preflight_input(input) {
        Ok(input) => input,
        Err(outcome) => return outcome,
    };

    ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let mut providers = provider_specs(prompt_version);
    limit_roster_for_lane(&mut providers, prompt_version);
    if providers.is_empty() {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(event = no_provider_log, "ai_polish_no_provider_available");
        emit_runtime_diagnostic("error", "polish", no_provider_log, true);
        return CleanupOutcome::Failed {
            error_class: "no_provider",
            circuit_open: false,
        };
    }

    let (total_timeout, budget_cap) = lane_timeouts(input, prompt_version);
    let start = Instant::now();
    let deadline = start + total_timeout;
    let operation_id = next_operation_id();
    let mut last_error = "not_attempted";
    let mut any_provider_attempted = false;

    for (attempt_index, spec) in providers.iter().enumerate() {
        let now = Instant::now();
        let same_provider_fallback = is_same_provider_fallback(&providers, attempt_index, spec);
        // The health circuit is provider-wide for compatibility. Do not let a
        // failed Groq primary suppress its different same-provider fallback.
        let circuit_open = provider_circuit_open(spec.provider);
        if circuit_open && !same_provider_fallback {
            record_skip();
            trace_skip_with_operation(&operation_id, "provider_circuit_open", "open");
            last_error = "circuit_open";
            continue;
        }
        any_provider_attempted = true;
        let remaining = deadline.saturating_duration_since(now);
        let attempts_left = providers.len().saturating_sub(attempt_index);
        // Reserve an equal share for every remaining candidate. This keeps
        // fallback latency useful after a primary timeout without exceeding
        // the operation's hard wall-clock deadline.
        let attempt_budget = fair_attempt_budget(remaining, attempts_left, budget_cap);
        if attempt_budget.is_zero() {
            last_error = "timeout";
            break;
        }
        trace_attempt(
            &operation_id,
            spec,
            total_timeout,
            attempt_index,
            attempt_budget,
            same_provider_fallback,
            prompt_version,
        );

        let attempt_start = Instant::now();
        match call_provider(spec, input, attempt_budget, &build_payload, prompt_version) {
            Ok(output) => {
                if let Some(output) = normalize(input, &output) {
                    on_success(input, &output);
                    record_success(spec.provider);
                    trace_result(
                        &operation_id,
                        spec,
                        "success",
                        attempt_index,
                        attempt_budget,
                        attempt_start.elapsed(),
                        "",
                        "closed",
                        prompt_version,
                    );
                    return CleanupOutcome::Cleaned(output);
                }
                last_error = "rejected_output";
                tracing::warn!(
                    operation_id = %operation_id,
                    provider = ?spec.provider,
                    model = %spec.model,
                    rule = super::acceptance::rejection_rule(input, &output, 1_200),
                    input_len = input.len(),
                    output_len = output.trim().len(),
                    "l3_output_rejected"
                );
                let circuit_opened = record_failure(spec.provider);
                trace_result(
                    &operation_id,
                    spec,
                    "failure",
                    attempt_index,
                    attempt_budget,
                    attempt_start.elapsed(),
                    last_error,
                    if circuit_opened { "open" } else { "closed" },
                    prompt_version,
                );
            }
            Err(failure) => {
                let error_class = failure.class;
                last_error = error_class;
                if error_class == "timeout" && is_dictation_lane(prompt_version) {
                    trace_budget_exceeded(
                        &operation_id,
                        spec,
                        attempt_budget,
                        attempt_start.elapsed(),
                        prompt_version,
                    );
                }
                let circuit_opened = record_failure(spec.provider);
                trace_result(
                    &operation_id,
                    spec,
                    "failure",
                    attempt_index,
                    attempt_budget,
                    attempt_start.elapsed(),
                    error_class,
                    if circuit_opened { "open" } else { "closed" },
                    prompt_version,
                );
                // The request was dispatched and its outcome is uncertain (it may have been
                // processed and billed): another paid provider is not asked the same question.
                // The one exception (Adrian, 2026-10-31) is the explicit "prompt" and
                // "summarize" commands, which must produce output: they get ONE retry on the
                // Groq backup model, the same provider the user already pays.
                if !failure.may_try_next_provider
                    && !uncertain_retry_on_backup_allowed(&providers, attempt_index, prompt_version)
                {
                    tracing::warn!(
                        error_class,
                        prompt_version,
                        "ai_polish_uncertain_delivery_not_replayed"
                    );
                    break;
                }
                if !failure.may_try_next_provider {
                    tracing::info!(
                        error_class,
                        prompt_version,
                        "ai_transform_uncertain_failure_retrying_on_backup_model"
                    );
                }
            }
        }
    }

    if !any_provider_attempted && last_error == "circuit_open" {
        return CleanupOutcome::Skipped {
            reason: "circuit_open",
            circuit_open: true,
        };
    }
    let health = health();
    tracing::warn!(
        error_class = last_error,
        circuit_open = health.circuit_open,
        consecutive_failures = health.consecutive_failures,
        prompt_version,
        event = failed_log,
        "ai_polish_all_providers_failed"
    );
    CleanupOutcome::Failed {
        error_class: last_error,
        circuit_open: health.circuit_open,
    }
}

/// L1 dictation polish is one attempt on the primary route: a rejected or slow
/// answer goes to local L0, never to a second model (that doubled the
/// stop-to-paste wait). Fallback models stay for the explicit L2 prompt and L3
/// summary transforms.
pub(crate) fn limit_roster_for_lane(providers: &mut Vec<ProviderSpec>, prompt_version: &str) {
    if is_dictation_lane(prompt_version) {
        providers.truncate(1);
    }
}

/// After an uncertain failure of attempt `attempt_index`, whether the loop may still run the
/// next roster entry. Only the explicit L2 `prompt` and L3 `summarize` lanes, and only when that
/// next entry is the same-provider backup model and the failed attempt was not already a retry.
/// L1 dictation never reaches this (its roster is one attempt), and every other failure class
/// keeps the existing rule.
pub(crate) fn uncertain_retry_on_backup_allowed(
    providers: &[ProviderSpec],
    attempt_index: usize,
    prompt_version: &str,
) -> bool {
    let transform_lane =
        prompt_version == PROMPT_POLISH_VERSION || prompt_version == SUMMARY_PROMPT_VERSION;
    let next = attempt_index + 1;
    transform_lane
        && attempt_index == 0
        && providers
            .get(next)
            .is_some_and(|spec| is_same_provider_fallback(providers, next, spec))
}

pub(crate) fn is_same_provider_fallback(
    providers: &[ProviderSpec],
    attempt_index: usize,
    spec: &ProviderSpec,
) -> bool {
    attempt_index > 0
        && spec.provider == Provider::Groq
        && providers[..attempt_index]
            .iter()
            .any(|previous| previous.provider == spec.provider && previous.model != spec.model)
}

pub fn summarize_outcome(input: &str, context: &PolishContext) -> CleanupOutcome {
    summarize_outcome_with_receipts(input, context, None, None)
}

pub fn summarize_outcome_with_receipts(
    input: &str,
    context: &PolishContext,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) -> CleanupOutcome {
    let context = &sanitize_context(context);
    transform_outcome(
        input,
        SUMMARY_PROMPT_VERSION,
        "l3_summary_no_provider_available",
        "l3_summary_all_providers_failed",
        |spec, input| {
            mark_context_request_attempt(request_attempted);
            let payload = build_summary_payload(spec, input, context);
            mark_original_binding_from_payload(&payload, original_binding);
            payload
        },
        // Summary may drop source values, but it may never introduce an
        // unsupported value. The one-directional guard permits omission.
        |input, output| {
            normalize_output(input, output)
                .map(|out| scrub_dashes(&out))
                .filter(|out| no_unsupported_numeric_value(input, out))
                .filter(|out| is_material_summary(input, out))
        },
        |_, _| {},
    )
}

fn mark_context_request_attempt(request_attempted: Option<&std::sync::atomic::AtomicBool>) {
    if let Some(receipt) = request_attempted {
        receipt.store(true, Ordering::Release);
    }
}

fn mark_original_binding_from_payload(
    payload: &Value,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) {
    let has_field_context = payload["messages"][1]["content"]
        .as_str()
        .is_some_and(|content| {
            content.lines().any(|line| {
                let Some(json) = line.strip_prefix("FIELD_CONTEXT_JSON:") else {
                    return false;
                };
                serde_json::from_str::<Value>(json)
                    .ok()
                    .is_some_and(|value| {
                        value["preceding_field_text"]
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty())
                    })
            })
        });
    if has_field_context {
        if let Some(receipt) = original_binding {
            receipt.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
#[path = "lanes_context_receipt_tests.rs"]
mod context_receipt_tests;

pub(crate) fn is_material_summary(input: &str, output: &str) -> bool {
    let input_words = input.split_whitespace().count();
    if input_words <= 24 {
        return true;
    }
    let output_words = output.split_whitespace().count();
    // A model can return output the same length as the input, changing
    // punctuation only. Long-source L3 output must be at
    // least 15% shorter or it is copyediting, not summarization.
    output_words.saturating_mul(100) <= input_words.saturating_mul(85)
}

/// Whether AI polish is enabled at all — the privacy gate for capturing any
/// machine-read context (focused-field text) in the first place.
pub fn cleanup_enabled() -> bool {
    policy::cleanup_enabled()
}

pub(crate) fn preflight_input(input: &str) -> Result<&str, CleanupOutcome> {
    if input.trim().is_empty() {
        record_skip();
        trace_skip("empty_input", "closed");
        return Err(CleanupOutcome::Skipped {
            reason: "empty_input",
            circuit_open: false,
        });
    }
    if !policy::cleanup_enabled() {
        record_skip();
        trace_skip("disabled", "closed");
        return Err(CleanupOutcome::Skipped {
            reason: "disabled",
            circuit_open: false,
        });
    }
    if !policy::cloud_consent() {
        record_skip();
        trace_skip("missing_consent", "closed");
        return Err(CleanupOutcome::Skipped {
            reason: "missing_consent",
            circuit_open: false,
        });
    }
    if input.chars().count() > max_input_chars() {
        record_skip();
        trace_skip("input_too_large", "closed");
        return Err(CleanupOutcome::Skipped {
            reason: "input_too_large",
            circuit_open: false,
        });
    }
    Ok(input)
}

#[cfg(test)]
pub(crate) fn cached_cleanup(input: &str) -> Option<String> {
    CLEANUP_CACHE
        .get()
        .and_then(|cache| {
            cache
                .lock()
                .iter()
                .find(|entry| entry.input == input)
                .cloned()
        })
        .map(|entry| entry.cleaned)
}

#[cfg(test)]
pub fn cached_prefix(input: &str) -> Option<(usize, String)> {
    CLEANUP_CACHE
        .get()
        .and_then(|cache| {
            cache
                .lock()
                .iter()
                .filter(|entry| entry.input.len() < input.len())
                .filter(|entry| input.starts_with(&entry.input))
                .filter(|entry| {
                    input[entry.input.len()..]
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_whitespace() || ch.is_ascii_punctuation())
                })
                .max_by_key(|entry| entry.input.len())
                .cloned()
        })
        .map(|entry| (entry.input.len(), entry.cleaned))
}

#[cfg(test)]
pub(crate) fn store_cleanup(input: &str, cleaned: &str) {
    let cache = CLEANUP_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = cache.lock();
    if let Some(entry) = guard.iter_mut().find(|entry| entry.input == input) {
        entry.cleaned = cleaned.to_string();
        return;
    }
    guard.push(CachedCleanup {
        input: input.to_string(),
        cleaned: cleaned.to_string(),
    });
    if guard.len() > CLEANUP_CACHE_MAX {
        let overflow = guard.len() - CLEANUP_CACHE_MAX;
        guard.drain(0..overflow);
    }
}
