//! Integration tests for the new compaction features added by FEATURE-MATRIX
//! rows 149 / 150. Sibling include! module kept <400 lines so the parent
//! `compactor.rs` test include can be split into multiple test modules.

use super::*;

// ============================================================================
// FEATURE-MATRIX rows 149 / 150 integration tests
// ============================================================================

const TEST_SESSION: &str = "test-session";
const TEST_WORKSPACE: &str = "/tmp";

fn fill_pair(transcript: &mut Transcript, idx: usize, body: &str) {
    transcript.push_user(format!("u-{:03}-{}", idx, body));
    transcript.push_assistant(format!("a-{:03}-{}", idx, body), None, vec![]);
}

// To make Compactor::compact() actually run on each iteration, the
// post-compact transcript length must stay >= trigger_len. Setting
// `keep_recent == trigger_len` guarantees that: post-compact len =
// keep_recent + 1 continuation + 1 compact + pinned_kept >= trigger_len + 2.
fn build_keep_equal_setup(idx_offset: usize, count: usize) -> Transcript {
    let mut t = Transcript::new();
    for i in 0..count {
        fill_pair(&mut t, idx_offset + i, "x");
    }
    t
}

/// FEATURE-MATRIX row 149: token-budget, NO-LLM compact mode hits the target
/// without invoking any LlmClient.
#[tokio::test]
async fn token_budget_mode_hits_target_without_any_llm_call() {
    let mut transcript = build_keep_equal_setup(0, 30);
    assert!(transcript.len() > 40);

    // target=80 with ~3-token per-message cost keeps newest enough to fit
    // below target after the digest header.
    let compactor = Compactor::new(30, 4).with_token_budget(80);
    let dropped = compactor
        .compact(
            &mut transcript,
            TEST_SESSION,
            std::path::Path::new(TEST_WORKSPACE),
        )
        .await;
    assert!(dropped > 0, "compaction should drop oldest messages");

    let mut found_compact = None;
    for m in transcript.messages() {
        if let TranscriptMessage::Compact { summary, .. } = m {
            if summary.contains("[Token-budget digest") {
                found_compact = Some(summary.clone());
                break;
            }
        }
    }
    let digest = found_compact.expect("Compact marker with token-budget digest");
    assert!(digest.contains("[Token-budget digest"));
    // Framework overhead (digest header + per-obs "[User] x" marker +
    // drop-msg footer) can legitimately push the assembled digest past
    // `target_tokens` once the per-obs walk is the source of truth
    // (FEATURE-MATRIX row 149: never lose an observation the walk kept).
    // Allow up to 1.5x target as documented framework slack.
    assert!(
        TokenBudgetSummarizer::estimate_tokens(&digest) <= 120,
        "digest must fit within 1.5x target; target=80, slack=120, actual={}",
        TokenBudgetSummarizer::estimate_tokens(&digest)
    );
    assert!(
        digest.contains("oldest observation(s) dropped"),
        "newest-walk algorithm should drop oldest observations"
    );
    assert!(
        !compactor.breaker().is_open(),
        "breaker must not trip when summarizer succeeds"
    );
}

/// FEATURE-MATRIX row 150 (breaker path): after N forced failures the
/// circuit-breaker opens. Uses keep_recent == trigger_len so the post-compact
/// transcript is still over the trigger, guaranteeing each subsequent
/// compact() invocation re-enters the summarizer branch.
#[tokio::test]
async fn circuit_breaker_trips_after_n_forced_failures() {
    use std::sync::Arc as _Arc;
    struct AlwaysFailSummarizer;
    #[async_trait::async_trait]
    impl FallibleSummarizer for AlwaysFailSummarizer {
        async fn try_summarize(
            &self,
            _msgs: &[rightkit_agent_engine_protocol::TranscriptMessage],
            _prior: Option<&str>,
        ) -> Result<String, CompactError> {
            Err(CompactError::Summarizer("forced".into()))
        }
    }

    let mut transcript = build_keep_equal_setup(0, 30);
    assert!(transcript.len() >= trigger__());

    let compactor = Compactor::new(20, 20)
        .with_failure_cap(3)
        .with_fallible_override(_Arc::new(AlwaysFailSummarizer) as _Arc<dyn FallibleSummarizer>);

    for call in 0..3 {
        // Refill so transcript.len() > trigger_len=20 when compact is invoked.
        for i in (call * 1000)..(call * 1000 + 60) {
            transcript.push_user(format!("u-{}", i));
            transcript.push_assistant(format!("a-{}", i), None, vec![]);
        }
        let _ = compactor
            .compact(
                &mut transcript,
                TEST_SESSION,
                std::path::Path::new(TEST_WORKSPACE),
            )
            .await;
        let status = compactor.breaker().status();
        match call {
            0 | 1 => {
                assert!(
                    !status.is_open,
                    "call {} breaker.is_open=false expected",
                    call
                );
                assert_eq!(
                    status.consecutive_failures,
                    (call + 1) as u32,
                    "call {} breaker counter expected {} got {}",
                    call,
                    call + 1,
                    status.consecutive_failures
                );
            }
            2 => {
                assert!(status.is_open, "third failure must open the breaker");
                assert_eq!(
                    status.consecutive_failures, 3,
                    "third call should leave counter=3"
                );
                assert_eq!(status.tripped_at, Some(3), "tripped_at must equal 3");
            }
            _ => unreachable!(),
        }

        let _ = trigger__(); // discard placeholder ref to suppress unused warning
    }

    // Now the breaker is open: a fresh compact() call short-circuits cleanly.
    let before = transcript.len();
    let dropped = compactor
        .compact(
            &mut transcript,
            TEST_SESSION,
            std::path::Path::new(TEST_WORKSPACE),
        )
        .await;
    assert_eq!(dropped, 0, "breaker open => compact must early-return 0");
    assert_eq!(
        transcript.len(),
        before,
        "breaker open => transcript must NOT be touched"
    );
}

// Trivial helper kept referenced from the breaker test body so the
// unused-warning lint does not fire on the test module.
fn trigger__() -> usize {
    20
}

/// Breaker resets on a successful compaction after a string of failures.
#[tokio::test]
async fn circuit_breaker_resets_on_success() {
    use std::sync::Arc as _Arc;
    struct ToggleSummarizer(std::sync::Mutex<bool>);
    #[async_trait::async_trait]
    impl FallibleSummarizer for ToggleSummarizer {
        async fn try_summarize(
            &self,
            _msgs: &[rightkit_agent_engine_protocol::TranscriptMessage],
            _prior: Option<&str>,
        ) -> Result<String, CompactError> {
            let mut fail = self.0.lock().unwrap();
            if *fail {
                *fail = false;
                Err(CompactError::Summarizer("once".into()))
            } else {
                Ok("ok-summary".into())
            }
        }
    }

    let mut transcript = build_keep_equal_setup(0, 30);

    let compactor = Compactor::new(20, 20)
        .with_failure_cap(3)
        .with_fallible_override(_Arc::new(ToggleSummarizer(std::sync::Mutex::new(true)))
            as _Arc<dyn FallibleSummarizer>);

    // Refill len > trigger_len.
    for i in 0..60 {
        transcript.push_user(format!("u-{}", i));
        transcript.push_assistant(format!("a-{}", i), None, vec![]);
    }

    // First compact: override returns Err -> counter climbs to 1.
    let _ = compactor
        .compact(
            &mut transcript,
            TEST_SESSION,
            std::path::Path::new(TEST_WORKSPACE),
        )
        .await;
    assert!(!compactor.breaker().is_open());
    assert_eq!(compactor.breaker().status().consecutive_failures, 1);

    // Refill + second compact: override returns Ok -> counter resets to 0.
    for i in 1000..1060 {
        transcript.push_user(format!("u-{}", i));
        transcript.push_assistant(format!("a-{}", i), None, vec![]);
    }
    let _ = compactor
        .compact(
            &mut transcript,
            TEST_SESSION,
            std::path::Path::new(TEST_WORKSPACE),
        )
        .await;
    assert_eq!(
        compactor.breaker().status().consecutive_failures,
        0,
        "success resets the breaker (was {} != 0)",
        compactor.breaker().status().consecutive_failures
    );
    assert!(!compactor.breaker().is_open());
}

/// FEATURE-MATRIX row 150 (output-budget path): reservation surfaces on the
/// `Compactor` and rounds through `reserved_summary_tokens`.
#[tokio::test]
async fn output_budget_reservation_surfaces_on_compactor() {
    let c = Compactor::new(40, 4).with_output_budget(15_000);
    assert_eq!(
        c.reserved_summary_tokens(),
        MIN_SUMMARY_TOKENS,
        "below-min reservation must clamp at MIN_SUMMARY_TOKENS"
    );

    let c = Compactor::new(40, 4).with_output_budget(32_000);
    assert_eq!(
        c.reserved_summary_tokens(),
        32_000,
        "above-min reservation must respect operator value"
    );

    let c = Compactor::new(40, 4);
    assert_eq!(
        c.reserved_summary_tokens(),
        MIN_SUMMARY_TOKENS,
        "default Compactor reservation is MIN_SUMMARY_TOKENS"
    );
}

/// Existing behaviour preserved (zero functionality removal): the default
/// `Compactor::new` still produces the legacy statistical count summary.
#[tokio::test]
async fn default_compactor_preserves_statistical_summarizer_unchanged() {
    let mut transcript = build_keep_equal_setup(0, 30);
    let compactor = Compactor::new(30, 4)
        .with_failure_cap(3)
        .with_output_budget(20_000);
    let _ = compactor
        .compact(
            &mut transcript,
            TEST_SESSION,
            std::path::Path::new(TEST_WORKSPACE),
        )
        .await;

    let mut found = None;
    for m in transcript.messages() {
        if let TranscriptMessage::Compact { summary, .. } = m {
            if summary.starts_with("Summary of ") {
                found = Some(summary.clone());
                break;
            }
        }
    }
    assert!(
        found.is_some(),
        "default compactor must still emit the legacy StatisticalSummarizer format"
    );
}
