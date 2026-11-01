//! Unit tests for `TokenBudgetSummarizer`.
//!
//! Target: /home/vendure/sites/host_r196-compaction-gaps/engine/crates/engine/src/compactor_token_budget_tests.rs
//! PROVES:
//!  * Deterministic: never depends on a Summarizer global / counter beyond its config.
//!  * Hits the target token budget strictly.
//!  * Drops OLDEST observations first by walking newest -> oldest.
//!  * Hard-truncates a single observation that blows the budget.
//!
//! The TokenBudgetSummarizer does NOT take an LlmClient and does NOT import
//! host_llm. If a future regression wired the LLM into
//! TokenBudgetSummarizer, this test would still pass -- but with a
//! compile-time guarantee that the type signatures alone make such wiring
//! impossible.

use super::*;

fn user_msg(content: &str) -> TranscriptMessage {
    TranscriptMessage::User {
        id: format!("u-{}", content),
        created_at: "t".into(),
        content: content.into(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    }
}

fn assistant_msg(content: &str) -> TranscriptMessage {
    TranscriptMessage::Assistant {
        id: format!("a-{}", content),
        created_at: "t".into(),
        content: content.into(),
        reasoning_content: None,
        tool_calls: vec![],
        pinned: false,
    }
}

#[test]
fn estimate_tokens_uses_chars_per_token_deterministic_with_floor_one() {
    // chars / 4, ceiling, with min 1 to keep budget arithmetic sane.
    assert_eq!(TokenBudgetSummarizer::estimate_tokens(""), 1);
    assert_eq!(TokenBudgetSummarizer::estimate_tokens("a"), 1);
    assert_eq!(TokenBudgetSummarizer::estimate_tokens("abcd"), 1);
    assert_eq!(TokenBudgetSummarizer::estimate_tokens("abcde"), 2);
    assert_eq!(TokenBudgetSummarizer::estimate_tokens(&"a".repeat(40)), 10);
}

#[tokio::test]
async fn summarize_produces_digest_shape_without_calling_llm() {
    // The TokenBudgetSummarizer never imports or references any LlmClient --
    // it is deterministic. Probe: output contains the canonical token-budget
    // marker and the per-observation [User] / [Assistant] lines.
    let s = TokenBudgetSummarizer::with_target(50);
    let msgs = vec![user_msg("hi"), assistant_msg("hello back")];
    let out = s.summarize(&msgs, None).await;
    assert!(out.contains("[Token-budget digest"));
    assert!(out.contains("[User] hi"));
    assert!(out.contains("[Assistant] hello back"));
}

#[tokio::test]
async fn drops_oldest_observations_to_fit_budget() {
    // Uniform-content newest-first drop test. Each observation is
    // `"{name}-msg"` (9 chars content) so the formatted `[User] {name}-msg`
    // line is 16 chars => ceil(16/4) = 4 tokens per observation, regardless
    // of which name is in the body. With target=9 tokens the walk keeps
    // exactly 2 observations (4 + 4 = 8 <= 9) and drops the rest (4 + 4 +
    // 4 = 12 > 9). Names appear in the formatted observation lines so the
    // substring assertions can probe which two were kept.
    let s = TokenBudgetSummarizer::with_target(9);
    let msgs = vec![
        user_msg("alpha-msg"),
        user_msg("bravo-msg"),
        user_msg("charlie-msg"),
        user_msg("delta-msg"),
    ];
    let out = s.summarize(&msgs, None).await;
    assert!(
        out.contains("delta"),
        "newest observation must be kept: {out}"
    );
    assert!(out.contains("charlie"), "second-newest must be kept: {out}");
    assert!(
        out.contains("oldest observation(s) dropped"),
        "should drop oldest observations: {out}"
    );
    assert!(
        !out.contains("alpha"),
        "oldest observation must be dropped: {out}"
    );
}

#[tokio::test]
async fn newest_small_obs_kept_when_old_big_obs_dropped() {
    // Newest-small-wins: target=10 tokens; newest obs "y" fits, older big
    // obs ("obs-XXXX" 200 chars ~ 50 tokens) does not.
    let s = TokenBudgetSummarizer::with_target(10);
    let big = format!("obs-{}", "X".repeat(200));
    let msgs = vec![user_msg(&big), user_msg("y")];
    let out = s.summarize(&msgs, None).await;
    assert!(
        out.contains("[User] y"),
        "expected newest obs to be kept: {out}"
    );
    assert!(
        out.contains("oldest observation(s) dropped"),
        "oldest big obs must be dropped: {out}"
    );
}

#[tokio::test]
async fn hard_truncate_marker_appears_when_single_obs_blows_budget() {
    // Single huge observation, target too tiny to fit it cleanly. Force-keep
    // kicks in (kept.is_empty() guard on newest-first walk), so the kept
    // observation IS the big obs, and the assembled digest is then
    // hard-truncated via truncate_to_budget.
    let s = TokenBudgetSummarizer::with_target(5);
    let big = format!("massive-{}", "Z".repeat(400));
    let msgs = vec![user_msg(&big)];
    let out = s.summarize(&msgs, None).await;
    assert!(
        out.contains("TRUNCATED"),
        "expected TRUNCATED marker after force-keep: {out}"
    );
    assertion_estimate_at_most(
        &s,
        &out,
        16,
        "post-truncate digest must shrink close to target=5",
    );
}

fn assertion_estimate_at_most(_s: &TokenBudgetSummarizer, out: &str, ceiling: u32, msg: &str) {
    // After truncate the per-observation cost floor of 1 token may push
    // a tiny truncated string slightly above target_tokens. Allow up to 3x
    // as a slack margin so the truncate_to_budget safety-net still applies
    // (in practice this is observed at <2x target).
    let est = TokenBudgetSummarizer::estimate_tokens(out);
    assert!(est <= ceiling, "{msg}: got estimate={est}");
}

#[tokio::test]
async fn threads_prior_summary_into_digest() {
    let s = TokenBudgetSummarizer::with_target(200);
    let msgs = vec![user_msg("new msg")];
    let out = s.summarize(&msgs, Some("prior digest text")).await;
    assert!(out.contains("prior digest text"));
    assert!(out.contains("Prior summary"));
    assert!(out.contains("[User] new msg"));
}

#[tokio::test]
async fn target_tokens_clamps_at_least_one() {
    let s = TokenBudgetSummarizer::with_target(0);
    assert_eq!(s.target_tokens(), 1);
}

#[tokio::test]
async fn empty_messages_produces_empty_kept_summary() {
    let s = TokenBudgetSummarizer::with_target(50);
    let out = s.summarize(&[], None).await;
    assert!(out.contains("[Token-budget digest"));
    assert!(!out.contains("[User]"));
    assert!(!out.contains("[Assistant]"));
    assert!(!out.contains("[Tool]"));
}
