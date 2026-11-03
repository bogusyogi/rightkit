//! Staged rollout + auto-rollback — the change-governance state machine.
//!
//! A promoted model is not applied wholesale. It rolls out in stages
//! (Canary → Partial → Full), gathering live success samples at each stage. A
//! stage advances only if the new model holds at or above the champion baseline
//! (within tolerance); a drop triggers automatic rollback to the previous model.
//! Pure and deterministic so the decision logic is fully unit-tested.

/// The stage a rollout is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutStage {
    Canary,
    Partial,
    Full,
    RolledBack,
}

impl RolloutStage {
    pub fn as_str(self) -> &'static str {
        match self {
            RolloutStage::Canary => "Canary",
            RolloutStage::Partial => "Partial",
            RolloutStage::Full => "Full",
            RolloutStage::RolledBack => "RolledBack",
        }
    }

    /// True once the rollout has reached a terminal stage.
    pub fn is_terminal(self) -> bool {
        matches!(self, RolloutStage::Full | RolloutStage::RolledBack)
    }
}

/// What happened when a sample was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutEvent {
    /// More samples needed at the current stage.
    Pending,
    /// Advanced to a later stage.
    Advanced(RolloutStage),
    /// Degraded below baseline — rolled back to the previous model.
    RolledBack,
}

#[derive(Debug, Clone, Copy)]
pub struct RolloutConfig {
    /// Samples required to clear the Canary stage.
    pub canary_samples: u32,
    /// Samples required to clear the Partial stage.
    pub partial_samples: u32,
    /// How far below baseline the new model may drop before rollback.
    pub regression_tolerance: f64,
}

impl Default for RolloutConfig {
    fn default() -> Self {
        Self {
            canary_samples: 3,
            partial_samples: 5,
            regression_tolerance: 0.1,
        }
    }
}

/// A model rollout in progress for one role.
#[derive(Debug, Clone)]
pub struct Rollout {
    pub role: String,
    pub from_model: String,
    pub to_model: String,
    pub stage: RolloutStage,
    /// Champion's success rate the new model must hold against.
    pub baseline_rate: f64,
    samples: u32,
    successes: u32,
    cfg: RolloutConfig,
}

impl Rollout {
    pub fn start(
        role: impl Into<String>,
        from_model: impl Into<String>,
        to_model: impl Into<String>,
        baseline_rate: f64,
        cfg: RolloutConfig,
    ) -> Self {
        Self {
            role: role.into(),
            from_model: from_model.into(),
            to_model: to_model.into(),
            stage: RolloutStage::Canary,
            baseline_rate,
            samples: 0,
            successes: 0,
            cfg,
        }
    }

    fn stage_quota(&self) -> u32 {
        match self.stage {
            RolloutStage::Canary => self.cfg.canary_samples,
            RolloutStage::Partial => self.cfg.partial_samples,
            _ => u32::MAX,
        }
    }

    fn stage_rate(&self) -> f64 {
        if self.samples == 0 {
            1.0
        } else {
            self.successes as f64 / self.samples as f64
        }
    }

    /// Record one live result for the new model. Advances, rolls back, or waits.
    pub fn record(&mut self, success: bool) -> RolloutEvent {
        if self.stage.is_terminal() {
            return RolloutEvent::Pending;
        }
        self.samples += 1;
        if success {
            self.successes += 1;
        }
        if self.samples < self.stage_quota() {
            return RolloutEvent::Pending;
        }
        // Stage quota reached — decide.
        if self.stage_rate() + self.cfg.regression_tolerance < self.baseline_rate {
            self.stage = RolloutStage::RolledBack;
            return RolloutEvent::RolledBack;
        }
        // Advance, resetting per-stage counters.
        self.samples = 0;
        self.successes = 0;
        self.stage = match self.stage {
            RolloutStage::Canary => RolloutStage::Partial,
            RolloutStage::Partial => RolloutStage::Full,
            other => other,
        };
        RolloutEvent::Advanced(self.stage)
    }
}
