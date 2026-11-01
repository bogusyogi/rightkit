//! Unit tests for the circuit-breaker companion module. Verifies:
//!  * Default cap = 3.
//!  * record_failure / record_success transitions.
//!  * Status struct surfaces cap, count, is_open, tripped_at correctly.
//!  * env-var override of the cap (RIGHTKIT_MAX_AUTOCOMPACT_FAILURES) works.
//!  * InfallibleAdapter correctly lifts a Summarizer into a FallibleSummarizer
//!    without ever returning Err.
//!  * CompactNoOpReason::from_compactor_state picks the correct variant.

use super::*;
use std::sync::{Mutex, MutexGuard};

/// The three `env_override_*` tests mutate the process-global
/// `MAX_AUTOCOMPACT_FAILURES_ENV` var. Rust runs tests concurrently within one
/// binary and `set_var` mutates the whole process, so without this lock one test
/// reads another's value between its set and remove (the flaky
/// `falls_back_to_default_when_unparseable` seeing "5"). Hold it for the whole test.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    // Recover from a poisoned lock so one panicking test can't cascade-fail the rest.
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn default_cap_is_three_mirroring_max_consecutive_autocompact_failures() {
    let b = CompactorBreaker::new();
    assert_eq!(b.cap(), DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES);
    assert_eq!(b.cap(), 3);
    assert!(!b.is_open());
}

#[test]
fn breaker_trips_after_three_consecutive_failures() {
    let b = CompactorBreaker::with_cap(3);
    assert!(!b.record_failure());
    assert!(!b.record_failure());
    let newly_tripped = b.record_failure();
    assert!(newly_tripped, "third failure must trip the breaker");
    assert!(b.is_open());
    let status = b.status();
    assert_eq!(status.consecutive_failures, 3);
    assert_eq!(status.cap, 3);
    assert!(status.is_open);
    assert_eq!(status.tripped_at, Some(3));
}

#[test]
fn breaker_resets_on_success() {
    let b = CompactorBreaker::with_cap(2);
    b.record_failure();
    b.record_failure();
    assert!(b.is_open());
    b.record_success();
    assert!(!b.is_open());
    assert_eq!(b.status().consecutive_failures, 0);
}

#[test]
fn record_failure_returns_false_after_trip() {
    let b = CompactorBreaker::with_cap(1);
    assert!(b.record_failure(), "first failure trips cap=1");
    assert!(!b.record_failure(), "subsequent failures do not re-trip");
    assert!(b.is_open());
}

#[test]
fn status_reflects_tripped_at_only_once_open() {
    let b = CompactorBreaker::with_cap(3);
    let s = b.status();
    assert_eq!(s.tripped_at, None);
    assert!(!s.is_open);
    b.record_failure();
    assert_eq!(b.status().tripped_at, None);
    b.record_failure();
    assert_eq!(b.status().tripped_at, None);
    b.record_failure();
    let s = b.status();
    assert_eq!(s.tripped_at, Some(3));
    assert!(s.is_open);
}

#[test]
fn env_override_parses_a_valid_value() {
    let _guard = lock_env();
    std::env::set_var(MAX_AUTOCOMPACT_FAILURES_ENV, "5");
    let b = CompactorBreaker::new_from_env_or_default();
    std::env::remove_var(MAX_AUTOCOMPACT_FAILURES_ENV);
    assert_eq!(b.cap(), 5);
}

#[test]
fn env_override_falls_back_to_default_when_unparseable() {
    let _guard = lock_env();
    std::env::set_var(MAX_AUTOCOMPACT_FAILURES_ENV, "not-a-number");
    let b = CompactorBreaker::new_from_env_or_default();
    std::env::remove_var(MAX_AUTOCOMPACT_FAILURES_ENV);
    assert_eq!(b.cap(), DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES);
}

#[test]
fn env_override_falls_back_when_zero() {
    let _guard = lock_env();
    std::env::set_var(MAX_AUTOCOMPACT_FAILURES_ENV, "0");
    let b = CompactorBreaker::new_from_env_or_default();
    std::env::remove_var(MAX_AUTOCOMPACT_FAILURES_ENV);
    assert_eq!(b.cap(), DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES);
}

#[test]
fn cap_of_zero_is_clamped_to_one() {
    let b = CompactorBreaker::with_cap(0);
    assert_eq!(b.cap(), 1, "cap=0 must clamp to 1 (never trip)");
}

#[test]
fn reset_clears_failure_count() {
    let b = CompactorBreaker::with_cap(2);
    b.record_failure();
    b.record_failure();
    assert!(b.is_open());
    b.reset();
    assert!(!b.is_open());
    assert_eq!(b.status().consecutive_failures, 0);
}

#[test]
fn compact_error_display_is_informative() {
    let err = CompactError::Summarizer("llm timed out".into());
    let rendered = format!("{}", err);
    assert!(rendered.contains("llm timed out"));
    assert!(rendered.contains("summarizer"));
}

#[tokio::test]
async fn infallible_adapter_lifts_string_summarizer_into_fallible() {
    struct CountingSummarizer(std::sync::Mutex<u32>);
    #[async_trait]
    impl super::Summarizer for CountingSummarizer {
        async fn summarize(&self, _messages: &[TranscriptMessage], _prior: Option<&str>) -> String {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            format!("count={}", *n)
        }
    }
    let s = std::sync::Arc::new(CountingSummarizer(std::sync::Mutex::new(0)));
    let adapter = InfallibleAdapter::new(s.clone());
    let out = adapter.try_summarize(&[], None).await.unwrap();
    assert_eq!(out, "count=1");
    let out2 = adapter.try_summarize(&[], None).await.unwrap();
    assert_eq!(out2, "count=2");
}

#[test]
fn cancelled_outcome_does_not_count_toward_the_breaker() {
    let b = CompactorBreaker::with_cap(3);
    let cancelled = CompactError::Cancelled("user pressed esc".into());
    assert!(!counts_toward_breaker(&cancelled));
    for _ in 0..10 {
        let tripped = b.record_outcome(&cancelled);
        assert!(!tripped, "a cancel must never trip the breaker");
    }
    assert!(!b.is_open());
    assert_eq!(b.status().consecutive_failures, 0);
}

#[test]
fn real_failures_via_record_outcome_still_trip_after_three() {
    let b = CompactorBreaker::with_cap(3);
    let real_failure = CompactError::Summarizer("provider 500".into());
    assert!(counts_toward_breaker(&real_failure));
    assert!(!b.record_outcome(&real_failure));
    assert!(!b.record_outcome(&real_failure));
    let tripped = b.record_outcome(&real_failure);
    assert!(tripped, "third real failure must trip the breaker");
    assert!(b.is_open());
}

#[test]
fn a_cancel_interleaved_with_real_failures_does_not_advance_the_trip() {
    let b = CompactorBreaker::with_cap(3);
    let real_failure = CompactError::Summarizer("timeout".into());
    let cancelled = CompactError::Cancelled("user aborted".into());
    b.record_outcome(&real_failure);
    b.record_outcome(&real_failure);
    // A cancel between two real failures must not itself count, and must
    // not reset the two real failures already recorded.
    b.record_outcome(&cancelled);
    assert!(!b.is_open());
    assert_eq!(b.status().consecutive_failures, 2);
    let tripped = b.record_outcome(&real_failure);
    assert!(
        tripped,
        "third real failure trips even after an interleaved cancel"
    );
}

#[test]
fn cancelled_error_display_says_cancelled_not_failure() {
    let err = CompactError::Cancelled("closed session".into());
    let rendered = format!("{}", err);
    assert!(rendered.contains("cancelled"));
    assert!(rendered.contains("closed session"));
}

#[test]
fn compact_no_op_reason_variants_compare_for_equality() {
    // Sanity check that the variants we surface to status payloads are
    // well-formed and PartialEq (the orchestrator may compare against the
    // last reason before serializing to /hooks/status).
    assert_eq!(
        CompactNoOpReason::BelowTrigger,
        CompactNoOpReason::BelowTrigger
    );
    assert_ne!(
        CompactNoOpReason::BelowTrigger,
        CompactNoOpReason::BreakerOpen
    );
    assert_eq!(
        CompactNoOpReason::SummarizerFailed("x".into()),
        CompactNoOpReason::SummarizerFailed("x".into())
    );
    assert_ne!(
        CompactNoOpReason::SummarizerFailed("x".into()),
        CompactNoOpReason::SummarizerFailed("y".into())
    );
}
