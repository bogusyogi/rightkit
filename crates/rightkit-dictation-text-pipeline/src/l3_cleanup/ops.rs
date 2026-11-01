//! Operation plumbing for the cleanup lanes: token/input budgets, timeout
//! scaling, the provider circuit wrappers around `state::CIRCUIT`, the
//! aggregate counters surfaced through `health()`, and the
//! content-free attempt/result/skip tracing events. Tuning overrides (timeouts, input cap,
//! circuit) come from `overrides`, which reads the environment in test and `test-mode` builds only.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, MutexGuard};

use super::overrides;
use super::prompts::{
    is_app_prompt_version, PROMPT_POLISH_VERSION, PROMPT_VERSION, SUMMARY_PROMPT_VERSION,
};
use super::state::{
    Circuit, CleanupHealth, Provider, ProviderSpec, ATTEMPTS, CIRCUIT, CIRCUIT_OPENS, FAILURES,
    LOCAL_FALLBACKS, SKIPS, SUCCESSES,
};

pub(crate) fn max_tokens_for(input: &str) -> u32 {
    let words_budget = input.split_whitespace().count() as u32 + 160;
    let char_budget = (input.chars().count() as u32 / 3) + 160;
    words_budget.max(char_budget).clamp(128, 1200)
}

pub(crate) fn max_input_chars() -> usize {
    overrides::max_input_chars()
        .unwrap_or(4_000)
        .clamp(256, 20_000) as usize
}

pub(crate) fn total_timeout() -> Duration {
    // Two sequential Groq models need two useful network windows. Keep the
    // default bounded at 3s while reserving 1.5s for each model.
    let timeout_ms = overrides::total_timeout_ms().unwrap_or(3_000);
    Duration::from_millis(timeout_ms.clamp(300, 15_000))
}

/// Cloud budget scaled to the input: a fixed sub-second budget cannot round-trip a long
/// dictation (measured: a 2,400-char dictation timed out on
/// both providers and fell back to local polish). Base 3s + 2ms/char,
/// capped at 8s — a long recording already took seconds to transcribe, so a
/// proportionate polish wait is the right trade. An explicit
/// RIGHTKIT_L3_TOTAL_TIMEOUT_MS override still wins (clamped like before).
pub(crate) fn total_timeout_for_input(input: &str) -> Duration {
    if overrides::total_timeout_ms().is_some() {
        return total_timeout();
    }
    let scaled = 3_000 + (input.chars().count() as u64).saturating_mul(2);
    Duration::from_millis(scaled.clamp(1_500, 8_000))
}

pub(crate) fn provider_timeout() -> Duration {
    let timeout_ms = overrides::provider_timeout_ms().unwrap_or(1_500);
    Duration::from_millis(timeout_ms.clamp(250, 15_000))
}

/// Per-attempt cap matched to the scaled total budget. Two sequential models
/// share the total budget, so half of it gives the fallback a real window while
/// preserving the operation deadline. An explicit provider timeout override
/// still wins.
pub(crate) fn provider_timeout_for_budget(total: Duration) -> Duration {
    if overrides::provider_timeout_ms().is_some() {
        return provider_timeout();
    }
    total / 2
}

/// Hard wall-clock ceiling for the whole cloud lane of one L1 dictation polish:
/// exactly one attempt, no same-provider fallback. Dictation is on the user's
/// stop-to-paste critical path (field evidence 2026-10-23: a 1.5 s timeout plus
/// a 1.0 s fallback cost 1.6 s on a 93 ms ASR), so a slow or rejected answer
/// falls through to the already-computed local L0 polish instead. The ceiling
/// scales with the text (Adrian, 2026-10-31): `L1_DICTATION_BASE_BUDGET` plus
/// `L1_DICTATION_PER_CHAR_MICROS` per character, never above
/// `L1_DICTATION_MAX_BUDGET`; a fixed 900 ms could not round-trip a long
/// dictation. An explicit RIGHTKIT_L3_TOTAL_TIMEOUT_MS /
/// RIGHTKIT_L3_PROVIDER_TIMEOUT_MS override still wins. L2 prompt polish and
/// L3 summary keep the scaled budget and the fallback roster.
///
/// This is separate from the "8 words / 3 s" gate in `text_polish`
/// (`should_try_l1_app_polish`), which decides whether the cloud lane runs at all.
pub(crate) const L1_DICTATION_BASE_BUDGET: Duration = Duration::from_millis(900);
pub(crate) const L1_DICTATION_PER_CHAR_MICROS: u64 = 500;
pub(crate) const L1_DICTATION_MAX_BUDGET: Duration = Duration::from_millis(2_500);

/// The L1 dictation cloud budget for `input`: 900 ms + 0.5 ms per character, capped at 2.5 s.
pub(crate) fn l1_dictation_cloud_budget(input: &str) -> Duration {
    let chars = input.chars().count() as u64;
    let micros = (L1_DICTATION_BASE_BUDGET.as_micros() as u64)
        .saturating_add(chars.saturating_mul(L1_DICTATION_PER_CHAR_MICROS));
    Duration::from_micros(micros).min(L1_DICTATION_MAX_BUDGET)
}

/// True for the L1 dictation lane (the app-aware polish prompt).
pub(crate) fn is_dictation_lane(prompt_version: &str) -> bool {
    is_app_prompt_version(prompt_version)
}

/// `(total deadline, per-attempt cap)` for one lane run. Dictation: the total is
/// `l1_dictation_cloud_budget(input)` and the single attempt may use all of
/// it (no half-split reserved for a fallback that will not run).
pub(crate) fn lane_timeouts(input: &str, prompt_version: &str) -> (Duration, Duration) {
    let scaled = total_timeout_for_input(input);
    if !is_dictation_lane(prompt_version) {
        return (scaled, provider_timeout_for_budget(scaled));
    }
    let total = if overrides::total_timeout_ms().is_some() {
        scaled
    } else {
        l1_dictation_cloud_budget(input)
    };
    let cap = if overrides::provider_timeout_ms().is_some() {
        provider_timeout()
    } else {
        total
    };
    (total, cap)
}

pub(crate) fn trace_budget_exceeded(
    operation_id: &str,
    spec: &ProviderSpec,
    attempt_budget: Duration,
    elapsed: Duration,
    prompt_version: &'static str,
) {
    tracing::warn!(
        operation_id = %operation_id,
        product_level = product_level(prompt_version),
        provider = spec.provider.as_str(),
        model = %spec.model,
        attempt_budget_ms = attempt_budget.as_millis() as u64,
        elapsed_ms = elapsed.as_millis() as u64,
        prompt_version,
        "l3_budget_exceeded"
    );
}

pub(crate) fn fair_attempt_budget(
    remaining: Duration,
    attempts_left: usize,
    cap: Duration,
) -> Duration {
    let attempts_left = attempts_left.max(1) as u32;
    (remaining / attempts_left).min(cap)
}

fn circuit_threshold() -> u32 {
    overrides::circuit_fails().unwrap_or(3).clamp(1, 20) as u32
}

fn circuit_cooldown() -> Duration {
    Duration::from_millis(
        overrides::circuit_cooldown_ms()
            .unwrap_or(60_000)
            .clamp(1_000, 600_000),
    )
}

pub(crate) fn provider_circuit_open(provider: Provider) -> bool {
    let circuit = CIRCUIT.get_or_init(|| Mutex::new(Circuit::default()));
    lock_circuit(circuit).is_open(provider, Instant::now())
}

pub(crate) fn record_success(provider: Provider) {
    SUCCESSES.fetch_add(1, Ordering::Relaxed);
    if let Some(circuit) = CIRCUIT.get() {
        let mut guard = lock_circuit(circuit);
        guard.record_success(provider);
    }
}

pub(crate) fn record_failure(provider: Provider) -> bool {
    FAILURES.fetch_add(1, Ordering::Relaxed);
    let circuit = CIRCUIT.get_or_init(|| Mutex::new(Circuit::default()));
    let mut guard = lock_circuit(circuit);
    if guard.record_failure(
        provider,
        circuit_threshold(),
        circuit_cooldown(),
        Instant::now(),
    ) {
        CIRCUIT_OPENS.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            provider = provider.as_str(),
            cooldown_ms = circuit_cooldown().as_millis() as u64,
            "l3_cleanup_provider_circuit_opened"
        );
        return true;
    }
    false
}

pub fn health() -> CleanupHealth {
    let (
        circuit_open,
        consecutive_failures,
        groq_circuit_open,
        nvidia_circuit_open,
        openrouter_circuit_open,
    ) = if let Some(circuit) = CIRCUIT.get() {
        let mut guard = lock_circuit(circuit);
        let now = Instant::now();
        let groq_circuit_open = guard.is_open(Provider::Groq, now);
        let nvidia_circuit_open = guard.is_open(Provider::Nvidia, now);
        let openrouter_circuit_open = guard.is_open(Provider::OpenRouter, now);
        (
            groq_circuit_open || nvidia_circuit_open || openrouter_circuit_open,
            guard.consecutive_failures(),
            groq_circuit_open,
            nvidia_circuit_open,
            openrouter_circuit_open,
        )
    } else {
        (false, 0, false, false, false)
    };
    CleanupHealth {
        circuit_open,
        consecutive_failures,
        groq_circuit_open,
        nvidia_circuit_open,
        openrouter_circuit_open,
        attempts: ATTEMPTS.load(Ordering::Relaxed),
        successes: SUCCESSES.load(Ordering::Relaxed),
        failures: FAILURES.load(Ordering::Relaxed),
        skips: SKIPS.load(Ordering::Relaxed),
        local_fallbacks: LOCAL_FALLBACKS.load(Ordering::Relaxed),
        circuit_opens: CIRCUIT_OPENS.load(Ordering::Relaxed),
    }
}

pub fn record_local_fallback() {
    LOCAL_FALLBACKS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_skip() {
    SKIPS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn lock_circuit(circuit: &Mutex<Circuit>) -> MutexGuard<'_, Circuit> {
    circuit.lock()
}

pub(crate) fn next_operation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("o{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

pub(crate) fn trace_attempt(
    operation_id: &str,
    spec: &ProviderSpec,
    total_timeout: Duration,
    attempt_index: usize,
    attempt_budget: Duration,
    same_provider_fallback: bool,
    prompt_version: &'static str,
) {
    tracing::info!(
        operation_id = %operation_id,
        product_level = product_level(prompt_version),
        provider = spec.provider.as_str(),
        model = %spec.model,
        deadline_ms = total_timeout.as_millis() as u64,
        attempt_index,
        attempt_budget_ms = attempt_budget.as_millis() as u64,
        same_provider_fallback,
        prompt_version,
        "l3_cleanup_attempt"
    );
}

pub(crate) fn trace_result(
    operation_id: &str,
    spec: &ProviderSpec,
    outcome: &'static str,
    attempt_index: usize,
    attempt_budget: Duration,
    latency: Duration,
    error_class: &'static str,
    circuit_state: &'static str,
    prompt_version: &'static str,
) {
    tracing::info!(
        operation_id = %operation_id,
        product_level = product_level(prompt_version),
        provider = spec.provider.as_str(),
        model = %spec.model,
        outcome,
        status = outcome,
        attempt_index,
        attempt_budget_ms = attempt_budget.as_millis() as u64,
        attempt_duration_ms = latency.as_millis() as u64,
        latency_ms = latency.as_millis() as u64,
        error_class,
        circuit_state,
        prompt_version,
        "l3_cleanup_result"
    );
}

pub(crate) fn trace_skip(reason: &'static str, circuit_state: &'static str) {
    tracing::debug!(
        reason,
        circuit_state,
        product_level = product_level(PROMPT_VERSION),
        prompt_version = PROMPT_VERSION,
        "l3_cleanup_skip"
    );
}

pub(crate) fn trace_skip_with_operation(
    operation_id: &str,
    reason: &'static str,
    circuit_state: &'static str,
) {
    tracing::debug!(
        operation_id = %operation_id,
        reason,
        circuit_state,
        product_level = product_level(PROMPT_VERSION),
        prompt_version = PROMPT_VERSION,
        "l3_cleanup_skip"
    );
}

fn product_level(prompt_version: &str) -> &'static str {
    if is_app_prompt_version(prompt_version) {
        "l1"
    } else {
        match prompt_version {
            PROMPT_POLISH_VERSION => "l2",
            SUMMARY_PROMPT_VERSION => "l3",
            _ => "l1",
        }
    }
}
