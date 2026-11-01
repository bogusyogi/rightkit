//! Token and cost accounting for a session.

/// Running token + cost totals for a session.
#[derive(Debug, Clone, Default)]
pub struct UsageAccount {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    /// Estimated context window occupancy in tokens.
    pub context_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Cumulative reasoning/thinking tokens (a subset of `completion_tokens`).
    /// They are never resent as context, so the pressure meter excludes them.
    pub reasoning_tokens: u64,
    /// Context window capacity for the active model (set via `set_window`).
    pub window_tokens: u64,
    /// Per-1k pricing for the active model.
    prompt_price_per_1k: f64,
    completion_price_per_1k: f64,
    cache_read_price_per_1k: Option<f64>,
    cache_creation_price_per_1k: Option<f64>,
    /// Whether `prompt_price_per_1k`/`completion_price_per_1k` came from a
    /// curated pricing entry rather than a conservative guess. When `false`,
    /// `cost_usd_or_unknown()` reports `None` instead of a misleading number —
    /// unknown pricing must never be conflated with zero cost.
    priced: bool,
}

impl UsageAccount {
    pub fn new(prompt_price_per_1k: f64, completion_price_per_1k: f64) -> Self {
        Self::with_known_pricing(prompt_price_per_1k, completion_price_per_1k, true)
    }

    /// Construct with an explicit `priced` flag — pass `false` when the
    /// caller could not resolve curated pricing for the active model (the
    /// per-1k values are then a fallback estimate, not a real quote).
    pub fn with_known_pricing(
        prompt_price_per_1k: f64,
        completion_price_per_1k: f64,
        priced: bool,
    ) -> Self {
        Self {
            prompt_price_per_1k,
            completion_price_per_1k,
            cache_read_price_per_1k: None,
            cache_creation_price_per_1k: None,
            priced,
            ..Default::default()
        }
    }

    /// Set provider, model and TTL-specific cache prices in USD per 1k tokens.
    /// Callers must leave these unset when provider pricing is unavailable.
    pub fn set_cache_prices_per_1k(&mut self, read: Option<f64>, creation: Option<f64>) {
        self.cache_read_price_per_1k = read;
        self.cache_creation_price_per_1k = creation;
    }

    /// Record the reasoning-token subset of the completion just recorded.
    pub fn record_reasoning(&mut self, reasoning_tokens: u32) {
        self.reasoning_tokens += reasoning_tokens as u64;
    }

    /// Record a model turn's usage, updating token totals and cost.
    pub fn record(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read: u32,
        cache_creation: u32,
    ) {
        self.prompt_tokens += prompt_tokens as u64;
        self.completion_tokens += completion_tokens as u64;
        // `prompt_tokens` EXCLUDES cached input (providers normalize to that), so
        // the full context occupied by this call is every input bucket plus output.
        self.context_tokens = prompt_tokens as u64
            + cache_read as u64
            + cache_creation as u64
            + completion_tokens as u64;
        self.cache_read_tokens += cache_read as u64;
        self.cache_creation_tokens += cache_creation as u64;
        self.cost_usd += (prompt_tokens as f64 / 1000.0) * self.prompt_price_per_1k
            + (completion_tokens as f64 / 1000.0) * self.completion_price_per_1k
            + self.cache_read_price_per_1k.unwrap_or(0.0) * (cache_read as f64 / 1000.0)
            + self.cache_creation_price_per_1k.unwrap_or(0.0) * (cache_creation as f64 / 1000.0);
    }

    /// Refund usage that was charged speculatively (for example, a cancelled
    /// sub-call). Counters saturate at zero, so duplicate or oversized refunds
    /// cannot create negative usage or reopen more headroom than was charged.
    /// Returns the actual dollar amount removed after token-count clamping.
    pub fn refund(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read: u32,
        cache_creation: u32,
    ) -> f64 {
        let prompt = (prompt_tokens as u64).min(self.prompt_tokens);
        let completion = (completion_tokens as u64).min(self.completion_tokens);
        let cache_read = (cache_read as u64).min(self.cache_read_tokens);
        let cache_creation = (cache_creation as u64).min(self.cache_creation_tokens);
        self.prompt_tokens -= prompt;
        self.completion_tokens -= completion;
        self.cache_read_tokens -= cache_read;
        self.cache_creation_tokens -= cache_creation;
        self.context_tokens = self
            .context_tokens
            .saturating_sub(prompt.saturating_add(completion));
        let amount = (prompt as f64 / 1000.0) * self.prompt_price_per_1k
            + (completion as f64 / 1000.0) * self.completion_price_per_1k
            + self.cache_read_price_per_1k.unwrap_or(0.0) * (cache_read as f64 / 1000.0)
            + self.cache_creation_price_per_1k.unwrap_or(0.0) * (cache_creation as f64 / 1000.0);
        let applied = amount.max(0.0).min(self.cost_usd.max(0.0));
        self.cost_usd = (self.cost_usd - applied).max(0.0);
        applied
    }

    /// Set the context window capacity for the active model.
    pub fn set_window(&mut self, window_tokens: u64) {
        self.window_tokens = window_tokens;
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Fraction of a context window used, given its size in tokens.
    pub fn context_percent(&self, window_tokens: u64) -> f64 {
        let w = if window_tokens > 0 {
            window_tokens
        } else {
            self.window_tokens
        };
        if w == 0 {
            return 0.0;
        }
        (self.context_tokens as f64 / w as f64).min(1.0)
    }

    /// True when known, metered cost meets or exceeds a ceiling. Fallback
    /// estimates and subscription-backed routes must never stop a session.
    pub fn exceeds_budget(&self, ceiling_usd: f64) -> bool {
        ceiling_usd > 0.0
            && self
                .cost_usd_or_unknown()
                .is_some_and(|cost| cost >= ceiling_usd)
    }

    /// True when this account was constructed with curated pricing for the
    /// active model (as opposed to a conservative fallback estimate).
    pub fn is_priced(&self) -> bool {
        self.priced
    }

    /// Session-aggregate prompt-cache hit rate, 0.0-100.0:
    /// `cache_read / (uncached + cache_read + cache_creation)`. Zero when no
    /// input has been recorded.
    pub fn cache_hit_rate_pct(&self) -> f64 {
        cache_hit_rate_pct(
            self.prompt_tokens,
            self.cache_read_tokens,
            self.cache_creation_tokens,
        )
    }

    /// Dollar cost of `tokens_lost` cache-read tokens that were instead billed
    /// as cache writes (`creation_tokens` of them, at most) or plain uncached
    /// input. `None` when model or provider cache pricing is unknown: generic
    /// cache multipliers would present a guessed number as a cost.
    pub fn estimate_cache_miss_cost_usd(
        &self,
        tokens_lost: u64,
        creation_tokens: u64,
    ) -> Option<f64> {
        // An account that was never given a price (all zero) has no basis for
        // an estimate either.
        let (Some(read_price), Some(creation_price)) = (
            self.cache_read_price_per_1k,
            self.cache_creation_price_per_1k,
        ) else {
            return None;
        };
        if !self.priced || self.prompt_price_per_1k <= 0.0 {
            return None;
        }
        let as_creation = tokens_lost.min(creation_tokens);
        let as_uncached = tokens_lost - as_creation;
        let billed = (as_creation as f64 / 1000.0) * creation_price
            + (as_uncached as f64 / 1000.0) * self.prompt_price_per_1k;
        let would_have_cost = (tokens_lost as f64 / 1000.0) * read_price;
        Some((billed - would_have_cost).max(0.0))
    }

    /// Accumulated cost, or `None` when model or observed cache pricing is
    /// unknown. Callers surfacing cost to operators MUST use this instead of
    /// `cost_usd` directly; unknown rates never become a guessed total.
    pub fn cost_usd_or_unknown(&self) -> Option<f64> {
        (self.priced
            && (self.cache_read_tokens == 0 || self.cache_read_price_per_1k.is_some())
            && (self.cache_creation_tokens == 0 || self.cache_creation_price_per_1k.is_some()))
        .then_some(self.cost_usd)
    }
}

/// Prompt-cache hit rate, 0.0-100.0: `read / (uncached + read + creation)`.
/// `uncached` EXCLUDES cache reads and writes (the provider-normalised
/// `prompt_tokens`). Zero when the denominator is zero.
pub fn cache_hit_rate_pct(uncached: u64, read: u64, creation: u64) -> f64 {
    let total = uncached.saturating_add(read).saturating_add(creation);
    if total == 0 {
        return 0.0;
    }
    (read as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
}

#[cfg(test)]
mod cache_hit_tests {
    use super::*;

    #[test]
    fn hit_rate_is_read_over_every_input_bucket() {
        assert_eq!(cache_hit_rate_pct(0, 0, 0), 0.0);
        assert!((cache_hit_rate_pct(100, 900, 0) - 90.0).abs() < 1e-9);
        assert!((cache_hit_rate_pct(100, 800, 100) - 80.0).abs() < 1e-9);
        assert_eq!(cache_hit_rate_pct(500, 0, 500), 0.0);
    }

    #[test]
    fn session_hit_rate_aggregates_all_turns() {
        let mut u = UsageAccount::new(3.0, 15.0);
        u.record(2000, 30, 0, 0);
        u.record(300, 10, 1800, 0);
        // 1800 / (2300 + 1800)
        assert!((u.cache_hit_rate_pct() - 1800.0 / 4100.0 * 100.0).abs() < 1e-9);
    }

    #[test]
    fn miss_cost_prices_writes_and_uncached_reads_against_the_read_price() {
        let mut u = UsageAccount::new(3.0, 15.0);
        u.set_cache_prices_per_1k(Some(0.3), Some(3.75));
        // read 0.3/1k, write 3.75/1k, plain 3.0/1k.
        let all_written = u.estimate_cache_miss_cost_usd(10_000, 10_000).unwrap();
        assert!(
            (all_written - 10.0 * (3.75 - 0.3)).abs() < 1e-9,
            "{all_written}"
        );
        let all_plain = u.estimate_cache_miss_cost_usd(10_000, 0).unwrap();
        assert!((all_plain - 10.0 * (3.0 - 0.3)).abs() < 1e-9, "{all_plain}");
        let mixed = u.estimate_cache_miss_cost_usd(10_000, 4_000).unwrap();
        assert!(
            (mixed - (4.0 * 3.75 + 6.0 * 3.0 - 10.0 * 0.3)).abs() < 1e-9,
            "{mixed}"
        );
    }

    #[test]
    fn miss_cost_is_unknown_for_unpriced_models() {
        let mut u = UsageAccount::with_known_pricing(3.0, 15.0, false);
        u.set_cache_prices_per_1k(Some(0.3), Some(3.75));
        assert_eq!(u.estimate_cache_miss_cost_usd(10_000, 10_000), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_tokens() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.record(100, 50, 0, 0);
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens(), 150);
    }

    #[test]
    fn accumulates_across_turns() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.record(100, 50, 0, 0);
        u.record(200, 60, 0, 0);
        assert_eq!(u.prompt_tokens, 300);
        assert_eq!(u.completion_tokens, 110);
    }

    #[test]
    fn computes_cost() {
        let mut u = UsageAccount::new(3.0, 15.0); // $3/1k prompt, $15/1k completion
        u.record(1000, 1000, 0, 0);
        assert!((u.cost_usd - 18.0).abs() < 1e-9);
    }

    #[test]
    fn cache_tokens_accumulate() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.record(100, 50, 200, 300);
        u.record(100, 50, 100, 0);
        assert_eq!(u.cache_read_tokens, 300);
        assert_eq!(u.cache_creation_tokens, 300);
    }

    #[test]
    fn context_percent_clamped() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.record(8000, 2000, 0, 0); // context_tokens = 10000
        assert!((u.context_percent(20000) - 0.5).abs() < 1e-9);
        assert_eq!(u.context_percent(5000), 1.0); // clamped
        assert_eq!(u.context_percent(0), 0.0);
    }

    #[test]
    fn context_percent_uses_window_tokens_fallback() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.set_window(20000);
        u.record(10000, 0, 0, 0);
        assert!((u.context_percent(0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn context_counts_cached_and_written_input_not_only_uncached() {
        let mut u = UsageAccount::new(0.0, 0.0);
        u.set_window(10_000);
        // 100 uncached + 900 cache-read + 50 cache-write input, 20 output.
        u.record(100, 20, 900, 50);
        assert_eq!(u.context_tokens, 1070);
        assert!((u.context_percent(0) - 0.107).abs() < 1e-9);
    }

    #[test]
    fn budget_detection() {
        let mut u = UsageAccount::new(10.0, 10.0);
        u.record(1000, 0, 0, 0); // $10
        assert!(u.exceeds_budget(10.0));
        assert!(u.exceeds_budget(5.0));
        assert!(!u.exceeds_budget(20.0));
        assert!(!u.exceeds_budget(0.0)); // no ceiling
    }

    #[test]
    fn known_pricing_reports_real_cost() {
        let mut u = UsageAccount::with_known_pricing(3.0, 15.0, true);
        u.record(1000, 1000, 0, 0);
        assert!(u.is_priced());
        assert_eq!(u.cost_usd_or_unknown(), Some(18.0));
    }

    #[test]
    fn unpriced_model_reports_none_not_zero() {
        let mut u = UsageAccount::with_known_pricing(1.0, 3.0, false);
        u.record(1000, 1000, 0, 0);
        assert!(!u.is_priced());
        // The estimate remains available for diagnostics, but cannot trip a
        // real-dollar ceiling because it is not a billable amount.
        assert!(u.cost_usd > 0.0);
        assert_eq!(u.cost_usd_or_unknown(), None);
        assert!(!u.exceeds_budget(0.01));
    }

    #[test]
    fn default_new_is_priced() {
        let u = UsageAccount::new(1.0, 2.0);
        assert!(u.is_priced());
    }

    #[test]
    fn refund_saturates_usage_and_cost() {
        let mut u = UsageAccount::new(3.0, 15.0);
        u.record(1000, 1000, 100, 200);
        let refunded = u.refund(5000, 5000, 5000, 5000);
        assert!((refunded - 18.0).abs() < 1e-9);
        assert_eq!(u.total_tokens(), 0);
        assert_eq!(u.cache_read_tokens, 0);
        assert_eq!(u.cache_creation_tokens, 0);
        assert_eq!(u.cost_usd, 0.0);
        assert_eq!(u.refund(1, 1, 1, 1), 0.0);
    }

    #[test]
    fn refund_preserves_priced_flag_and_unknown_cost_semantics() {
        let mut priced = UsageAccount::with_known_pricing(3.0, 15.0, true);
        priced.record(1000, 0, 0, 0);
        priced.refund(500, 0, 0, 0);
        assert!(priced.is_priced());
        assert!(priced.cost_usd_or_unknown().is_some());
        let mut unpriced = UsageAccount::with_known_pricing(3.0, 15.0, false);
        unpriced.record(1000, 0, 0, 0);
        unpriced.refund(500, 0, 0, 0);
        assert!(!unpriced.is_priced());
        assert_eq!(unpriced.cost_usd_or_unknown(), None);
    }

    #[test]
    fn usage_refund_is_idempotent_and_saturates_at_zero() {
        let mut u = UsageAccount::new(10.0, 10.0);
        u.record(100, 100, 10, 10);
        let first = u.refund(100, 100, 10, 10);
        assert!(first > 0.0);
        assert_eq!(u.total_tokens(), 0);
        assert_eq!(u.refund(100, 100, 10, 10), 0.0);
    }
}
