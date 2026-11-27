//! Token and cost accounting for a session.

/// A 1-hour prompt-cache write over a 5-minute one: 2x the input price against 1.25x (Anthropic
/// prompt-caching pricing).
pub const ONE_HOUR_CACHE_WRITE_FACTOR: f64 = 2.0 / 1.25;

/// The per-1k prices one model is billed at, as one value so a request can be priced at the
/// rates of the model that answered it (plan-model, a `/model` switch) and not only the
/// session's first model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPrices {
    /// USD per 1k uncached input tokens.
    pub prompt_per_1k: f64,
    /// USD per 1k output tokens.
    pub completion_per_1k: f64,
    /// Whether the figures are a curated quote (see `UsageAccount::cost_is_known`).
    pub priced: bool,
    /// Prompt-cache read and 5-minute write prices, when the model has them.
    pub cache_per_1k: Option<(f64, f64)>,
    /// `(threshold, prompt multiplier, completion multiplier)` of the long-prompt tier, when the
    /// model has one.
    pub long_context_tier: Option<(u64, f64, f64)>,
}

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
    /// `(threshold, prompt multiplier, completion multiplier)`: a request whose input exceeds the
    /// threshold is billed at the multiples of every rate (long-prompt tier).
    long_context_tier: Option<(u64, f64, f64)>,
    /// Whether `prompt_price_per_1k`/`completion_price_per_1k` came from a
    /// curated pricing entry rather than a conservative guess. When `false`,
    /// `cost_usd_or_unknown()` reports `None` instead of a misleading number —
    /// unknown pricing must never be conflated with zero cost.
    priced: bool,
    /// Requests whose cost is the provider's own charge, and requests priced from the table.
    billed_requests: u64,
    estimated_requests: u64,
    /// `cost_usd` before the latest request was added: `bill_last_record` swaps that request's
    /// estimate for the provider's figure.
    cost_before_last_request: f64,
    /// The session's own prices, kept while a request is priced at another model's rates.
    base_prices: Option<ModelPrices>,
}

impl UsageAccount {
    fn current_prices(&self) -> ModelPrices {
        ModelPrices {
            prompt_per_1k: self.prompt_price_per_1k,
            completion_per_1k: self.completion_price_per_1k,
            priced: self.priced,
            cache_per_1k: match (
                self.cache_read_price_per_1k,
                self.cache_creation_price_per_1k,
            ) {
                (None, None) => None,
                (read, creation) => Some((read.unwrap_or(0.0), creation.unwrap_or(0.0))),
            },
            long_context_tier: self.long_context_tier,
        }
    }

    fn apply_prices(&mut self, prices: &ModelPrices) {
        self.prompt_price_per_1k = prices.prompt_per_1k;
        self.completion_price_per_1k = prices.completion_per_1k;
        self.priced = prices.priced;
        self.cache_read_price_per_1k = prices.cache_per_1k.map(|(read, _)| read);
        self.cache_creation_price_per_1k = prices.cache_per_1k.map(|(_, creation)| creation);
        self.long_context_tier = prices.long_context_tier;
    }

    /// Price the next request at `prices` (another model's rates), or at the session's own
    /// when `None`. The session's own prices are kept and restored; nothing already recorded
    /// is repriced.
    pub fn use_prices(&mut self, prices: Option<&ModelPrices>) {
        match prices {
            Some(prices) => {
                if self.base_prices.is_none() {
                    self.base_prices = Some(self.current_prices());
                }
                self.apply_prices(prices);
            }
            None => {
                if let Some(base) = self.base_prices.take() {
                    self.apply_prices(&base);
                }
            }
        }
    }

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
            long_context_tier: None,
            priced,
            ..Default::default()
        }
    }

    /// Set provider and model cache prices in USD per 1k tokens: the read price and the 5-minute
    /// write price. A 1-hour write is billed at [`ONE_HOUR_CACHE_WRITE_FACTOR`] times the 5-minute
    /// one (see [`record_with_retention`](Self::record_with_retention)). Callers must leave these
    /// unset when provider pricing is unavailable.
    pub fn set_cache_prices_per_1k(&mut self, read: Option<f64>, creation: Option<f64>) {
        self.cache_read_price_per_1k = read;
        self.cache_creation_price_per_1k = creation;
    }

    /// Set the long-prompt tier: requests with more input tokens than `threshold` cost
    /// `prompt_multiplier` times the input and cache rates and `completion_multiplier` times the output rate.
    pub fn set_long_context_tier(&mut self, tier: Option<(u64, f64, f64)>) {
        self.long_context_tier = tier;
    }

    /// Record the reasoning-token subset of the completion just recorded.
    pub fn record_reasoning(&mut self, reasoning_tokens: u32) {
        self.reasoning_tokens += reasoning_tokens as u64;
    }

    /// Record a model turn's usage, pricing its cache writes at the lifetime the request asked for.
    pub fn record_with_retention(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read: u32,
        cache_creation: u32,
        retention: crate::llm::CacheRetention,
    ) {
        self.record(prompt_tokens, completion_tokens, cache_read, cache_creation);
        if retention == crate::llm::CacheRetention::Long {
            // `record` priced the writes at the 5-minute rate; add the 1-hour premium.
            self.cost_usd += self.cache_creation_price_per_1k.unwrap_or(0.0)
                * (ONE_HOUR_CACHE_WRITE_FACTOR - 1.0)
                * (cache_creation as f64 / 1000.0);
        }
    }

    /// Record a model turn's usage, updating token totals and cost. Cache writes are priced at the
    /// 5-minute rate.
    pub fn record(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read: u32,
        cache_creation: u32,
    ) {
        self.cost_before_last_request = self.cost_usd;
        self.estimated_requests += 1;
        self.prompt_tokens += prompt_tokens as u64;
        self.completion_tokens += completion_tokens as u64;
        let input_tokens = prompt_tokens as u64 + cache_read as u64 + cache_creation as u64;
        // `prompt_tokens` EXCLUDES cached input (providers normalize to that), so
        // the full context occupied by this call is every input bucket plus output. A host that
        // sets `compaction_full_reserve_trigger` counts every input bucket only: completion
        // (incl. reasoning) is output, not context occupancy (audit C8).
        self.context_tokens = if crate::host_options::compaction_full_reserve_trigger() {
            input_tokens
        } else {
            input_tokens + completion_tokens as u64
        };
        self.cache_read_tokens += cache_read as u64;
        self.cache_creation_tokens += cache_creation as u64;
        let (input_factor, output_factor) = match self.long_context_tier {
            Some((threshold, input, output)) if input_tokens > threshold => (input, output),
            _ => (1.0, 1.0),
        };
        self.cost_usd += ((prompt_tokens as f64 / 1000.0) * self.prompt_price_per_1k
            + self.cache_read_price_per_1k.unwrap_or(0.0) * (cache_read as f64 / 1000.0)
            + self.cache_creation_price_per_1k.unwrap_or(0.0) * (cache_creation as f64 / 1000.0))
            * input_factor
            + (completion_tokens as f64 / 1000.0) * self.completion_price_per_1k * output_factor;
    }

    /// The provider reported its own charge for the request just recorded: that figure replaces
    /// the table estimate for it, and the request counts as billed.
    pub fn bill_last_record(&mut self, billed_usd: f64) {
        if !billed_usd.is_finite() || billed_usd < 0.0 || self.estimated_requests == 0 {
            return;
        }
        self.cost_usd = self.cost_before_last_request + billed_usd;
        self.estimated_requests -= 1;
        self.billed_requests += 1;
    }

    /// Every request so far carried the provider's own charge.
    pub fn cost_is_billed(&self) -> bool {
        self.billed_requests > 0 && self.estimated_requests == 0
    }

    /// Where the cost came from: `"provider"` when every request was billed by the provider,
    /// `"estimated"` when any was priced from the table, `None` before the first request.
    pub fn cost_source(&self) -> Option<&'static str> {
        match (self.billed_requests, self.estimated_requests) {
            (0, 0) => None,
            (_, 0) => Some("provider"),
            _ => Some("estimated"),
        }
    }

    /// The cost is a quote, not a guess: curated pricing, or the provider's own charge.
    pub fn cost_is_known(&self) -> bool {
        self.priced || self.cost_is_billed()
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
        let refunded_context = if crate::host_options::compaction_full_reserve_trigger() {
            prompt
        } else {
            prompt.saturating_add(completion)
        };
        self.context_tokens = self.context_tokens.saturating_sub(refunded_context);
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
        (self.cost_is_billed()
            || (self.priced
                && (self.cache_read_tokens == 0 || self.cache_read_price_per_1k.is_some())
                && (self.cache_creation_tokens == 0 || self.cache_creation_price_per_1k.is_some())))
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
