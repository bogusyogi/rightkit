//! Unit tests for the output-budget reservation module. Verifies:
//!  * None  -> 20_000.
//!  * Some(<20_000)  -> 20_000 (clamped up).
//!  * Some(>20_000)  -> as given.
//!  * env var override applied correctly with parsing tolerance.

use super::*;
use std::sync::{Mutex, MutexGuard};

/// The four `env_override_*` tests mutate the process-global
/// `MODEL_MAX_OUTPUT_TOKENS_ENV` var. Rust runs tests concurrently within one
/// binary and `set_var` mutates the whole process, so without this lock one test
/// reads another's value between its set and remove (observed: the round-trip
/// test seeing "15000"). Same fix as `compactor_breaker_tests`. Hold it for the
/// whole test.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    // Recover from a poisoned lock so one panicking test can't cascade-fail the rest.
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn reservation_defaults_to_min_when_model_max_output_is_none() {
    assert_eq!(reserved_summary_tokens(None), MIN_SUMMARY_TOKENS);
    assert_eq!(reserved_summary_tokens(None), 20_000);
}

#[test]
fn reservation_clamps_up_to_min_when_model_max_output_is_below_min() {
    assert_eq!(reserved_summary_tokens(Some(0)), MIN_SUMMARY_TOKENS);
    assert_eq!(reserved_summary_tokens(Some(1)), MIN_SUMMARY_TOKENS);
    assert_eq!(reserved_summary_tokens(Some(10_000)), MIN_SUMMARY_TOKENS);
    assert_eq!(reserved_summary_tokens(Some(19_999)), MIN_SUMMARY_TOKENS);
}

#[test]
fn reservation_trusts_model_max_output_when_above_min() {
    assert_eq!(reserved_summary_tokens(Some(20_000)), 20_000);
    assert_eq!(reserved_summary_tokens(Some(20_001)), 20_001);
    assert_eq!(reserved_summary_tokens(Some(64_000)), 64_000);
    assert_eq!(reserved_summary_tokens(Some(128_000)), 128_000);
}

#[test]
fn env_override_round_trips_through_reserved_summary_tokens_from_env() {
    let _guard = lock_env();
    std::env::set_var(MODEL_MAX_OUTPUT_TOKENS_ENV, "32000");
    assert_eq!(reserved_summary_tokens_from_env(), 32_000);
    std::env::remove_var(MODEL_MAX_OUTPUT_TOKENS_ENV);
}

#[test]
fn env_override_clamps_up_when_below_min() {
    let _guard = lock_env();
    std::env::set_var(MODEL_MAX_OUTPUT_TOKENS_ENV, "15000");
    assert_eq!(reserved_summary_tokens_from_env(), MIN_SUMMARY_TOKENS);
    std::env::remove_var(MODEL_MAX_OUTPUT_TOKENS_ENV);
}

#[test]
fn env_override_falls_back_on_unparseable_value() {
    let _guard = lock_env();
    std::env::set_var(MODEL_MAX_OUTPUT_TOKENS_ENV, "not-a-number");
    assert_eq!(reserved_summary_tokens_from_env(), MIN_SUMMARY_TOKENS);
    std::env::remove_var(MODEL_MAX_OUTPUT_TOKENS_ENV);
}

#[test]
fn env_override_falls_back_on_zero() {
    let _guard = lock_env();
    std::env::set_var(MODEL_MAX_OUTPUT_TOKENS_ENV, "0");
    assert_eq!(reserved_summary_tokens_from_env(), MIN_SUMMARY_TOKENS);
    std::env::remove_var(MODEL_MAX_OUTPUT_TOKENS_ENV);
}

#[test]
fn min_summary_tokens_constant_is_canonical() {
    // Pin the canonical value: changing this is a breaking change to the
    // default compaction summary budget.
    assert_eq!(MIN_SUMMARY_TOKENS, 20_000);
    assert_eq!(MODEL_MAX_OUTPUT_TOKENS_ENV, "RIGHTKIT_MODEL_MAX_OUTPUT");
}
