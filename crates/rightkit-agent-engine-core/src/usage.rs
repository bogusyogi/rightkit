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
