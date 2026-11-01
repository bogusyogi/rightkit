// Deterministic, NO-LLM, token-budget-aware summarizer.
//
// The "token-budget mode" alternative to the LLM-backed `LlmSummarizer`: when
// a session must be compacted but the LLM is unavailable / disallowed / too
// expensive, this summarizer produces a bounded digest from the kept
// observations using a deterministic chars/4 token estimator. No network /
// model call is ever made.
//
// Sibling include! module.

const CHARS_PER_TOKEN: f64 = 4.0;
const DEFAULT_TARGET_TOKENS: u32 = 2_048;

/// Deterministic, NO-LLM summarizer that walks NEWEST -> OLDEST and keeps
/// observations greedily until the configured `target_tokens` budget is
/// filled, then drops the OLDEST remaining observations and inserts a
/// "N oldest observation(s) dropped" footer in the digest. The newest-first
/// walk matches the contract of FEATURE-MATRIX row 149.
pub struct TokenBudgetSummarizer {
    target_tokens: u32,
}

impl TokenBudgetSummarizer {
    pub fn new() -> Self {
        Self::with_target(DEFAULT_TARGET_TOKENS)
    }

    pub fn with_target(target_tokens: u32) -> Self {
        Self {
            target_tokens: target_tokens.max(1),
        }
    }

    pub fn target_tokens(&self) -> u32 {
        self.target_tokens
    }

    /// Deterministic chars/4 ceiling estimator with a floor of 1 so the
    /// producer context -- budget arithmetic inside `summarize` -- never
    /// silently drops a single-byte observation by virtue of its estimate
    /// returning zero.
    pub fn estimate_tokens(text: &str) -> u32 {
        let chars = text.chars().count() as f64;
        let raw = (chars / CHARS_PER_TOKEN).ceil() as i64;
        (1_i64).max(raw.max(0)) as u32
    }

    /// Truncates `text` so the token estimator returns at most
    /// `target_tokens`. Preserves head + tail semantics with a
    /// `[TRUNCATED N chars]` marker so callers can detect the cut.
    fn truncate_to_budget(&self, text: &str) -> String {
        let budget_chars_tmp = (self.target_tokens as f64 * CHARS_PER_TOKEN) as usize;
        let budget_chars = if budget_chars_tmp < 1 {
            1
        } else {
            budget_chars_tmp
        };
        let total_chars = text.chars().count();
        if total_chars <= budget_chars {
            return text.to_string();
        }
        let head_budget = (budget_chars as f64 * 0.7) as usize;
        let tail_budget = budget_chars.saturating_sub(head_budget);
        let head: String = text.chars().take(head_budget).collect();
        let tail_rev: String = text.chars().rev().take(tail_budget).collect::<String>();
        let tail: String = tail_rev.chars().rev().collect();
        let dropped = total_chars.saturating_sub(budget_chars);
        if tail_budget == 0 || tail.is_empty() {
            format!("{}\n...[TRUNCATED {} chars]...", head, dropped)
        } else {
            format!("{}\n...[TRUNCATED {} chars]...\n{}", head, dropped, tail)
        }
    }
}

impl Default for TokenBudgetSummarizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Summarizer for TokenBudgetSummarizer {
    async fn summarize(
        &self,
        messages: &[TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> String {
        let mut kept_newest_first: Vec<&TranscriptMessage> = Vec::new();
        let mut used: u32 = 0;
        // Walk newest -> oldest; the first iteration force-keeps because
        // we always want to keep at least the most-recent context. After
        // that, every further kept observation pushes us closer to the
        // target. Once an observation would exceed the budget we DROP it
        // -- modelling "drop oldest by walking from newest".
        //
        // Per-observation cost is just the chars/4 estimator applied to
        // the formatted line (`[User] X`, `[Assistant] X`, etc). NO `+ 1`
        // marker overhead -- the floor-of-1 in `estimate_tokens` already
        // prevents an empty observation from being treated as zero cost,
        // and adding a fixed `+1` per obs inflates the budget math so
        // much that the budget appears to fit fewer observations than
        // it actually does (which then triggers the bad path where the
        // head/tail post-truncate nukes the kept observations).
        for msg in messages.iter().rev() {
            let formatted = format_message_for_summarizer(msg);
            let cost = Self::estimate_tokens(&formatted);
            if used + cost <= self.target_tokens || kept_newest_first.is_empty() {
                kept_newest_first.push(msg);
                used += cost;
            }
            // Else: drop this observation (it's the oldest remaining
            // because we are walking newest -> oldest).
        }

        let kept_count = kept_newest_first.len();
        let dropped_count = messages.len().saturating_sub(kept_count);

        let mut out = String::new();
        out.push_str(&format!(
            "[Token-budget digest target={} tokens, kept={}/{} observations]\n",
            self.target_tokens,
            kept_count,
            messages.len(),
        ));
        if let Some(prior) = prior_summary {
            out.push_str("[Prior summary]\n");
            out.push_str(prior);
            out.push('\n');
        }
        out.push_str("[New observations, newest first -> oldest kept]\n");
        for msg in kept_newest_first.iter() {
            out.push_str(&format_message_for_summarizer(msg));
            out.push('\n');
        }
        if dropped_count > 0 {
            out.push_str(&format!(
                "[...{} oldest observation(s) dropped to fit target budget...]\n",
                dropped_count
            ));
        }

        // Hard-clamp policy -- only fire when a SINGLE kept observation
        // truly dwarfs the budget. In all other cases (multi-obs or
        // single-but-fits) the per-obs walk is the source of truth and
        // we trust it; framework overhead (headers / footer) is
        // acceptable slack.
        //
        //   * kept_count == 0 -- unreachable: the empty-keep guard in
        //     the walk always retains at least the newest message.
        //   * kept_count == 1 -- truncate only when THIS observation's
        //     formatted size alone exceeds `self.target_tokens`. This
        //     is the only case where `truncate_to_budget` can recover a
        //     usable digest; in the single-fits case the framework
        //     overhead going above the target is tolerable framework
        //     slack and skipping truncate preserves the `[User] <obs>`
        //     marker verbatim.
        //   * kept_count > 1 -- never truncate; the walk already
        //     budgeted per obs and stripping middle observations via
        //     head/tail cut would silently lose content that the walk
        //     explicitly kept (a contract violation of FEATURE-MATRIX
        //     row 149).
        let needs_truncate = if kept_newest_first.len() == 1 {
            let only = kept_newest_first[0];
            let formatted = format_message_for_summarizer(only);
            Self::estimate_tokens(&formatted) > self.target_tokens
        } else {
            false
        };
        if needs_truncate {
            self.truncate_to_budget(&out)
        } else {
            out
        }
    }
}

#[cfg(test)]
#[path = "compactor_token_budget_tests.rs"]
mod token_budget_tests;
