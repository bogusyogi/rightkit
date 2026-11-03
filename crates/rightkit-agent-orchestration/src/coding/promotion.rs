//! Model promotion — the champion/challenger decision engine.
//!
//! A role runs against a benchmark (k repeats) under two models: the incumbent
//! **champion** and a candidate **challenger**. The challenger is promoted only
//! if it clears every gate:
//!
//! 1. **Sample gate** — both runs have at least `min_repeats` samples (k).
//! 2. **Absolute gate** — the challenger reaches at least `min_absolute` success.
//! 3. **Margin gate** — the challenger beats the champion by at least `min_delta`.
//!
//! The decision is pure and deterministic so it is fully unit-tested; the daemon
//! supplies the [`BenchmarkReport`]s by actually running the agent.

/// The outcome of running a benchmark suite for one (role, model) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkReport {
    pub role: String,
    pub model: String,
    pub repeats: u32,
    pub successes: u32,
    pub benchmark_suite: String,
    /// Version of the agent that ran this benchmark, if known.
    pub agent_version: Option<String>,
}

impl BenchmarkReport {
    pub fn new(
        role: impl Into<String>,
        model: impl Into<String>,
        repeats: u32,
        successes: u32,
    ) -> Self {
        Self {
            role: role.into(),
            model: model.into(),
            repeats,
            successes: successes.min(repeats),
            benchmark_suite: "cr-internal-tbench-shaped@2026-06".into(),
            agent_version: None,
        }
    }

    pub fn with_suite(mut self, suite: impl Into<String>) -> Self {
        self.benchmark_suite = suite.into();
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.agent_version = Some(version.into());
        self
    }

    /// Fraction of repeats that succeeded (0.0 when there were no repeats).
    pub fn success_rate(&self) -> f64 {
        if self.repeats == 0 {
            0.0
        } else {
            self.successes as f64 / self.repeats as f64
        }
    }
}

/// Gates governing whether a challenger may replace the champion.
#[derive(Debug, Clone, Copy)]
pub struct PromotionConfig {
    /// Minimum repeats (k) required of both champion and challenger.
    pub min_repeats: u32,
    /// Minimum absolute success rate the challenger must reach.
    pub min_absolute: f64,
    /// Minimum margin by which the challenger must beat the champion.
    pub min_delta: f64,
}

impl Default for PromotionConfig {
    fn default() -> Self {
        Self {
            min_repeats: 3,
            min_absolute: 0.5,
            min_delta: 0.1,
        }
    }
}

/// The verdict of a champion/challenger comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionDecision {
    Promote {
        role: String,
        from_model: String,
        to_model: String,
        delta: f64,
    },
    Reject {
        reason: String,
    },
}

impl PromotionDecision {
    pub fn is_promote(&self) -> bool {
        matches!(self, PromotionDecision::Promote { .. })
    }
}

/// Decide whether `challenger` should replace `champion` for their role.
pub fn decide_promotion(
    champion: &BenchmarkReport,
    challenger: &BenchmarkReport,
    cfg: &PromotionConfig,
) -> PromotionDecision {
    if champion.repeats < cfg.min_repeats || challenger.repeats < cfg.min_repeats {
        return PromotionDecision::Reject {
            reason: format!(
                "insufficient samples (need {} each; champion={}, challenger={})",
                cfg.min_repeats, champion.repeats, challenger.repeats
            ),
        };
    }
    let chal = challenger.success_rate();
    if chal < cfg.min_absolute {
        return PromotionDecision::Reject {
            reason: format!(
                "challenger below absolute bar: {:.2} < {:.2}",
                chal, cfg.min_absolute
            ),
        };
    }
    let delta = chal - champion.success_rate();
    if delta >= cfg.min_delta {
        PromotionDecision::Promote {
            role: challenger.role.clone(),
            from_model: champion.model.clone(),
            to_model: challenger.model.clone(),
            delta,
        }
    } else {
        PromotionDecision::Reject {
            reason: format!(
                "challenger not better by margin: delta {:.2} < {:.2}",
                delta, cfg.min_delta
            ),
        }
    }
}
