// Compaction -- reduces an over-long transcript to a summary behind a boundary.
//
// ## What this file (the orchestrator) owns
//
// * The [`Summarizer`] trait consumed by [`Compactor::compact`].
// * Two reference impls: [`StatisticalSummarizer`] (infallible count digest)
//   and [`LlmSummarizer`] (the production LLM path).
// * The [`Compactor`] orchestrator, including its new breaker-aware compact
//   path and the four builder methods that drive FEATURE-MATRIX rows 149 / 150:
//
//   | method | row | adds |
//   |---|---|---|
//   | `Compactor::with_token_budget` | 149 | NO-LLM, deterministic, token-budget digest via [`TokenBudgetSummarizer`] |
//   | `Compactor::with_output_budget` | 150 | Output-budget reservation (`max(model_max_output, 20_000)`) |
//   | `Compactor::with_failure_cap` | 150 | Circuit-breaker cap (`CompactorBreaker::with_cap`) |
//   | `Compactor::with_fallible_override` | 150 | Wire a `FallibleSummarizer` so breaker can count failures |
//
// ## Sibling include! modules
//
// * [`compactor_format`]        -- constants + formatters + `now_iso`
// * [`compactor_output_budget`]  -- `MIN_SUMMARY_TOKENS`, `reserved_summary_tokens`
// * [`compactor_breaker`]       -- `CompactorBreaker`, `CompactError`, `FallibleSummarizer`, `InfallibleAdapter`
// * [`compactor_preservation`]  -- pinned/skill/tool-block partition helpers
// * [`compactor_token_budget`]  -- `TokenBudgetSummarizer`
//
// ## Constraint reminder
//
// Every sibling file is <400 lines; the existing behaviour is preserved
// (`Compactor::new(trigger_len, keep_recent)` still infalls back to
// `StatisticalSummarizer`). 0 functionality removal.

use crate::transcript::Transcript;
use async_trait::async_trait;
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Sibling include! modules. Each file MUST stay <400 lines; if any grew over 400
// it would be split at item boundaries into another sibling.
// ---------------------------------------------------------------------------

include!("compactor_format.rs");
include!("compactor_output_budget.rs");
include!("compactor_breaker.rs");
include!("compactor_preservation.rs");
include!("compactor_token_budget.rs");
include!("compactor_memory_block.rs");
include!("compactor_authoritative.rs");
include!("compactor_prefire.rs");
include!("compactor_cheap_prune.rs");
include!("compactor_retry.rs");
include!("compactor_lenient_summary.rs");

// ---------------------------------------------------------------------------
// Trait + reference impls (statistical + LLM)
// ---------------------------------------------------------------------------

/// Summarizer trait for pluggable summarization strategies.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(
        &self,
        messages: &[TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> String;
}

/// Statistical summarizer -- the default fallback. Returns a summary of message counts.
pub struct StatisticalSummarizer;

#[async_trait]
impl Summarizer for StatisticalSummarizer {
    async fn summarize(
        &self,
        messages: &[TranscriptMessage],
        _prior_summary: Option<&str>,
    ) -> String {
        let user = messages
            .iter()
            .filter(|m| matches!(m, TranscriptMessage::User { .. }))
            .count();
        let assistant = messages
            .iter()
            .filter(|m| matches!(m, TranscriptMessage::Assistant { .. }))
            .count();
        let tool = messages
            .iter()
            .filter(|m| matches!(m, TranscriptMessage::Tool { .. }))
            .count();
        format!(
            "Summary of {} earlier messages ({} user, {} assistant, {} tool results).",
            messages.len(),
            user,
            assistant,
            tool
        )
    }
}

type LlmCallFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
pub type LlmCallFn = dyn Fn(Vec<serde_json::Value>) -> LlmCallFuture + Send + Sync;

/// LLM-powered summarizer. Uses anchored-iterative summarization with optional prior summary.
pub struct LlmSummarizer {
    pub compaction_prompt: String,
    /// Callback to invoke LLM. Receives a Vec of serde_json::Value (message objects)
    /// and returns the summarized text. Uses Result<String, String> for simplicity.
    pub call_fn: Arc<LlmCallFn>,
}

#[async_trait]
impl Summarizer for LlmSummarizer {
    async fn summarize(
        &self,
        messages: &[TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> String {
        // Format the messages for the LLM.
        let mut formatted_messages = String::new();
        if let Some(prior) = prior_summary {
            formatted_messages.push_str("Prior summary:\n");
            formatted_messages.push_str(prior);
            formatted_messages.push_str("\n\nNew messages to integrate:\n");
        }

        for msg in messages {
            formatted_messages.push_str(&format_message_for_summarizer(msg));
            formatted_messages.push('\n');
        }

        let compaction_prompt = self.compaction_prompt.clone();

        // Build minimal message list for the LLM call.
        let llm_messages = vec![
            serde_json::json!({
                "$kind": "system",
                "content": compaction_prompt,
            }),
            serde_json::json!({
                "$kind": "user",
                "content": formatted_messages,
            }),
        ];

        // Invoke the LLM callback.
        match (self.call_fn)(llm_messages).await {
            Ok(summary) => summary,
            Err(_) => {
                // Fall back to statistical summary on LLM error.
                StatisticalSummarizer
                    .summarize(messages, prior_summary)
                    .await
            }
        }
    }
}

/// Compacts transcripts when they grow past a threshold.
pub struct Compactor {
    /// Number of messages above which compaction triggers.
    pub trigger_len: usize,
    /// How many recent messages to keep verbatim after the summary.
    pub keep_recent: usize,
    /// Summarization strategy (default: [`StatisticalSummarizer`]).
    summarizer: Arc<dyn Summarizer>,
    /// Optional failure-aware override. When set, `compact()` calls this
    /// instead of the infallible `self.summarizer`, routes errors through
    /// [`CompactorBreaker`], and falls back to `self.summarizer` on `Err`.
    /// When `None` (default), the breaker never trips -- existing callers
    /// have unchanged behaviour (this preserves "zero functionality
    /// removal").
    fallible_override: Option<Arc<dyn FallibleSummarizer>>,
    /// Fail-fast breaker surfaced for status / observability. Tests
    /// consult it directly to assert trip semantics.
    pub breaker: CompactorBreaker,
    /// Reservation in tokens for the compaction summary, mirrored from
    /// `max(model_max_output, MIN_SUMMARY_TOKENS)`. Status-only; the actual
    /// truncation is enforced by the downstream `LlmClient`.
    pub reserved_summary_tokens: u32,
    /// Optional post-compaction memory-block provider. When `Some`, both
    /// rebuild paths (`compact` and `compact_authoritative` / replay)
    /// invoke the provider and inject a fresh, sentinel-scoped memory
    /// block immediately after the post-compaction continuation marker.
    /// Re-compaction REPLACES any prior block (id
    /// `memory-block:<session>`); the block never accumulates. Invoked
    /// fresh on every compaction — output is **never** cached.
    memory_block_provider: Option<Arc<dyn MemoryBlockProvider>>,
    /// Mechanism 2 (prefire) cache: a background-prepared summary awaiting
    /// validation at the next `prepare_authoritative` hot-path trigger.
    /// Populated by callers via `store_prefired` (typically from a
    /// background task calling `prefire_prepare`); consumed (and cleared,
    /// valid or not) by `prepare_authoritative`.
    prefired: std::sync::Mutex<Option<PrefiredSummary>>,
}

impl Compactor {
    pub fn new(trigger_len: usize, keep_recent: usize) -> Self {
        Self {
            trigger_len,
            keep_recent,
            summarizer: Arc::new(StatisticalSummarizer),
            fallible_override: None,
            breaker: CompactorBreaker::new(),
            reserved_summary_tokens: MIN_SUMMARY_TOKENS,
            memory_block_provider: None,
            prefired: std::sync::Mutex::new(None),
        }
    }

    /// Mechanism 2 (prefire): stash a background-prepared summary so the
    /// next `prepare_authoritative` hot-path call can skip a model call
    /// entirely if the prefix it was built from is still current.
    pub fn store_prefired(&self, prefired: PrefiredSummary) {
        *self.prefired.lock().unwrap() = Some(prefired);
    }

    /// CR-17: read-only peek at the currently cached prefire fingerprint
    /// (if any), without consuming it. Used by the live prefire scheduler
    /// (`engine_session_controls::maybe_spawn_prefire`) to avoid spawning a
    /// redundant background prefire pass when a valid one for the exact
    /// same prefix is already cached.
    pub fn prefired_fingerprint(&self) -> Option<String> {
        self.prefired
            .lock()
            .unwrap()
            .as_ref()
            .map(|prefired| prefired.fingerprint.clone())
    }

    /// Test-only peek at the prefire cache. Used by
    /// `compaction_coordinator` tests to prove that state stored through one
    /// handle to a persistent `Compactor` is visible through another --
    /// this must never be called from non-test code, hence the `#[cfg(test)]`
    /// gate and the `_for_test` name.
    #[cfg(test)]
    pub fn take_prefired_for_test(&self) -> Option<PrefiredSummary> {
        self.prefired.lock().unwrap().clone()
    }

    /// Wire a post-compaction memory-block provider. When set, every
    /// rebuild path injects the provider's fresh output immediately after
    /// the post-compaction continuation marker, replacing any prior
    /// `memory-block:<session>` sentinel rather than accumulating it.
    pub fn with_memory_block_provider(mut self, provider: Arc<dyn MemoryBlockProvider>) -> Self {
        self.memory_block_provider = Some(provider);
        self
    }

    /// Read-only access to the wired memory-block provider.
    pub fn memory_block_provider(&self) -> Option<&Arc<dyn MemoryBlockProvider>> {
        self.memory_block_provider.as_ref()
    }

    /// Invoke the wired provider for one compaction. Returns the bounded
    /// block when the provider returns a non-empty string; otherwise `None`.
    /// The provider is allowed to fail closed by returning `None`; its panic
    /// cannot bubble out of this function because we use `AssertUnwindSafe`
    /// + `catch_unwind` and treat any panic as an absent block (the
    ///
    /// compaction loop must not break on a misbehaving provider).
    fn fresh_memory_block(&self, session_id: &str, workspace_root: &Path) -> Option<String> {
        let provider = self.memory_block_provider.as_ref()?;
        let session = SessionRef {
            session_id,
            workspace_root,
            task_id: None,
            request_id: None,
            generation: None,
            capacity: Default::default(),
        };
        // The provider runs in a catch_unwind so a panic never poisons
        // the compaction loop. Catch_unwind requires UnwindSafe; the
        // captured Arc<dyn MemoryBlockProvider> + session inputs are not
        // strictly UnwindSafe, but the worst a panicking provider can do
        // is abort the *current* compaction's memory re-injection — the
        // surrounding transcript and summary have already been computed
        // and are not affected.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            provider.memory_block(session)
        }));
        match outcome {
            Ok(Some(block)) => {
                if block.trim().is_empty() {
                    None
                } else {
                    Some(block)
                }
            }
            Ok(None) => None,
            Err(_) => {
                eprintln!(
                    "[compactor] memory_block_provider panicked; \
                     proceeding without memory re-injection"
                );
                None
            }
        }
    }

    /// Set a custom summarizer.
    pub fn with_summarizer(mut self, summarizer: Arc<dyn Summarizer>) -> Self {
        self.summarizer = summarizer;
        self
    }

    /// Replace the summarizer with [`TokenBudgetSummarizer::with_target`].
    /// FEATURE-MATRIX row 149: token-budget, NO-LLM compact mode.
    pub fn with_token_budget(self, target_tokens: u32) -> Self {
        self.with_summarizer(Arc::new(TokenBudgetSummarizer::with_target(target_tokens)))
    }

    /// Reserve `max(model_max_output, MIN_SUMMARY_TOKENS)` tokens for the
    /// summary. Status-only; smallclamp at >= [`MIN_SUMMARY_TOKENS`] so an
    /// operator can't silently under-reserve.
    /// FEATURE-MATRIX row 150 (output-budget path).
    pub fn with_output_budget(mut self, reserved_tokens: u32) -> Self {
        self.reserved_summary_tokens = reserved_summary_tokens(Some(reserved_tokens));
        self
    }

    /// Tighten / loosen the breaker cap. Mirrors
    /// `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES` in the FEATURE MATRIX.
    pub fn with_failure_cap(mut self, cap: u32) -> Self {
        self.breaker = CompactorBreaker::with_cap(cap);
        self
    }

    /// Wire a failure-aware summarizer so `compact()` can count failures.
    /// Persists the original `self.summarizer` as the *fallback* path used
    /// when the override returns `Err`. Reset via [`Self::clear_fallible_override`].
    pub fn with_fallible_override(mut self, override_summ: Arc<dyn FallibleSummarizer>) -> Self {
        self.fallible_override = Some(override_summ);
        self
    }

    /// Remove the failure-aware override and re-enter the existing
    /// infallible path (preserves pre-PR behaviour).
    pub fn clear_fallible_override(mut self) -> Self {
        self.fallible_override = None;
        self
    }

    /// Read-only access to the underlying [`CompactorBreaker`].
    pub fn breaker(&self) -> &CompactorBreaker {
        &self.breaker
    }

    /// Read-only access to the resolved summary-token reservation.
    pub fn reserved_summary_tokens(&self) -> u32 {
        self.reserved_summary_tokens
    }

    /// Whether the transcript should be compacted now. Returns false if the
    /// breaker is open so a tripped breaker halts auto-compaction cleanly.
    pub fn should_compact(&self, transcript: &Transcript) -> bool {
        if self.breaker.is_open() {
            return false;
        }
        transcript.len() > self.trigger_len
    }

    /// Compact the transcript in place. Returns the number of messages dropped (folded into
    /// the summary). Pinned messages are always preserved. Idempotent: a transcript that
    /// doesn't exceed the trigger is left untouched and returns 0.
    ///
    /// When a [`MemoryBlockProvider`] is wired AND `session_id`/`workspace_root`
    /// resolve to a non-empty block, the rebuild path injects a fresh,
    /// sentinel-scoped memory block immediately after the post-compaction
    /// continuation marker (see `compactor_memory_block`).
    ///
    /// Breaker semantics: if `should_compact` returns false (either below
    /// trigger or breaker open), returns 0 without inspecting the
    /// summarizer. Otherwise the compactor consults the fallible override
    /// (when set) and falls back to the infallible summarizer on `Err`,
    /// recording each failure on the breaker. A successful path resets the
    /// breaker.
    pub async fn compact(
        &self,
        transcript: &mut Transcript,
        session_id: &str,
        workspace_root: &Path,
    ) -> usize {
        if !self.should_compact(transcript) {
            return 0;
        }

        // Snapshot full tagged obligations before observation masking. The
        // legacy/offline path must retain these independently of summary prose
        // just like the authoritative path.
        let open_obligation_messages = collect_open_obligation_messages(transcript.messages());
        {
            let masker = crate::compaction_tuning::ObservationMasker::new(self.keep_recent);
            masker.mask(transcript);
        }

        let messages = transcript.messages().to_vec();
        let total = messages.len();
        let keep_from = total.saturating_sub(self.keep_recent);
        let preserved_skill_read_indices_set = preserved_skill_read_indices(&messages, keep_from);
        let preserved_tool_block_indices_set = preserved_tool_block_indices(&messages, keep_from);
        let preserved_latest_user_index = latest_user_index_before(&messages, keep_from);

        // Partition into (to-summarize, to-keep), but always keep pinned messages verbatim.
        let mut summarized = Vec::new();
        let mut kept: Vec<TranscriptMessage> = Vec::new();

        for (i, msg) in messages.into_iter().enumerate() {
            let pinned = is_pinned(&msg);
            if pinned
                || i >= keep_from
                || preserved_skill_read_indices_set.contains(&i)
                || preserved_tool_block_indices_set.contains(&i)
                || preserved_latest_user_index == Some(i)
            {
                kept.push(msg);
            } else {
                summarized.push(msg);
            }
        }

        let dropped = summarized.len();
        if dropped == 0 {
            return 0;
        }

        let kept = protect_open_obligation_messages(kept, &open_obligation_messages);

        // Extract prior summary if one exists.
        let prior_summary = extract_prior_summary(transcript);

        // Use the summarizer to generate the summary, routed through the
        // breaker when a fallible override is configured. On `Err` from the
        // override we fall back to the infallible summarizer so the existing
        // statistical-fallback path still fires -- but the breaker still
        // sees the failure so it can trip if the override is consistently
        // bad.
        //
        // Note: breaker tracking is **only** active when a `fallible_override`
        // is configured. The default `Compactor::new(...)` instance has no
        // override and no breaker activity -- callers who want LLM-failure
        // visibility must opt-in via `with_fallible_override`.
        let breaker_enabled = self.fallible_override.is_some();
        let raw_summary: String = if let Some(override_summ) = &self.fallible_override {
            match override_summ
                .try_summarize(&summarized, prior_summary.as_deref())
                .await
            {
                Ok(s) => {
                    if breaker_enabled {
                        self.breaker.record_success();
                    }
                    s
                }
                Err(err) => {
                    if breaker_enabled {
                        self.breaker.record_failure();
                    }
                    eprintln!("[compactor] override summarizer failed: {err}");
                    self.summarizer
                        .summarize(&summarized, prior_summary.as_deref())
                        .await
                }
            }
        } else {
            // No override configured: preservation path. LlmSummarizer
            // already does its own internal fallback, so this returns a
            // String unconditionally. The breaker is inert here.
            self.summarizer
                .summarize(&summarized, prior_summary.as_deref())
                .await
        };
        // Mechanism 9: every public compaction strategy, including this
        // legacy/offline path, applies the final safety normalization before
        // constructing the boundary that will be written.
        let summary = core_safety_pass(&raw_summary);

        let compact_msg = TranscriptMessage::Compact {
            id: "compact-1".into(),
            created_at: now_iso(),
            summary,
            dropped_count: dropped as i32,
            pinned: true,
        };

        // New transcript: pinned/summary boundary first, then recent kept tail.
        // Keep pinned items that were interleaved at the front; place compact marker before
        // the non-pinned recent tail.
        let (pinned_kept, recent_kept): (Vec<_>, Vec<_>) = kept.into_iter().partition(is_pinned);

        let rebuilt = self.rebuild_after_compaction(
            pinned_kept,
            recent_kept,
            compact_msg,
            session_id,
            workspace_root,
        );

        transcript.replace(rebuilt);
        dropped
    }

    /// Shared rebuild helper used by BOTH rebuild paths
    /// ([`Self::compact`] primary + [`rebuild_authoritative_messages`] for the
    /// authoritative / replay paths). Mirrors the existing
    /// `post-compaction-continuation` insert with one extra step: when a
    /// [`MemoryBlockProvider`] is wired AND the provider returns a fresh,
    /// non-empty block, the helper strips any prior `memory-block:<session>`
    /// sentinel from the pinned set and injects the new block immediately
    /// after the continuation marker, before the compact summary.
    ///
    /// Idempotent across re-compaction because the sentinel id is
    /// `memory-block:<session_id>` — the strip step guarantees exactly one
    /// block per session regardless of how many times compaction fires.
    fn rebuild_after_compaction(
        &self,
        mut pinned_kept: Vec<TranscriptMessage>,
        recent_kept: Vec<TranscriptMessage>,
        compact_msg: TranscriptMessage,
        session_id: &str,
        workspace_root: &Path,
    ) -> Vec<TranscriptMessage> {
        let has_continuation = pinned_kept.iter().any(|message| {
            matches!(
                message,
                TranscriptMessage::System { id, .. } if id == POST_COMPACTION_CONTINUATION_ID
            )
        });
        let has_prior_memory = has_memory_block(&pinned_kept, session_id);

        let memory_block = if has_prior_memory || self.memory_block_provider.is_some() {
            // Strip any prior sentinel first so a fresh block replaces — never
            // accumulates — even when the provider is absent or returns None.
            pinned_kept = strip_prior_memory_block(pinned_kept, session_id);
            self.fresh_memory_block(session_id, workspace_root)
        } else {
            None
        };

        let capacity = pinned_kept.len() + 3 + recent_kept.len();
        let mut rebuilt = Vec::with_capacity(capacity);
        rebuilt.extend(pinned_kept);
        if !has_continuation {
            rebuilt.push(post_compaction_continuation_message());
        }
        if let Some(block) = memory_block {
            rebuilt.push(memory_block_message(session_id, &block));
        }
        rebuilt.push(compact_msg);
        rebuilt.extend(recent_kept);
        rebuilt
    }
}

#[cfg(test)]
#[path = "compactor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compactor_tests_features.rs"]
mod features_tests;

#[cfg(test)]
mod legacy_compaction_safety_tests {
    use super::*;

    struct FixedLegacySummary;

    #[async_trait]
    impl Summarizer for FixedLegacySummary {
        async fn summarize(
            &self,
            _messages: &[TranscriptMessage],
            _prior_summary: Option<&str>,
        ) -> String {
            "  legacy summary  ".into()
        }
    }

    #[tokio::test]
    async fn legacy_compaction_safety_pass_runs_before_boundary_write() {
        let compactor = Compactor::new(0, 1).with_summarizer(Arc::new(FixedLegacySummary));
        let mut transcript = Transcript::new();
        transcript.push_user("old request");
        transcript.push_assistant("old answer", None, Vec::new());
        transcript.push_user("latest request");

        compactor
            .compact(&mut transcript, "session", Path::new("/tmp"))
            .await;

        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::Compact { summary, .. } if summary == "legacy summary"
        )));
    }

    #[tokio::test]
    async fn legacy_compaction_retains_open_obligation_outside_summary() {
        let compactor = Compactor::new(0, 1).with_summarizer(Arc::new(FixedLegacySummary));
        let mut transcript = Transcript::new();
        transcript.push_user("old request");
        transcript.push_tool_result("call-1", "OPEN OBLIGATION: finish release", false);
        transcript.push_user("latest request");

        compactor
            .compact(&mut transcript, "session", Path::new("/tmp"))
            .await;

        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::Tool {
                id, summary, pinned: true, ..
            } if id == "tool-2" && summary == "OPEN OBLIGATION: finish release"
        )));
    }
}
