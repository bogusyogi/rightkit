use std::{collections::VecDeque, error::Error, fmt, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartMode {
    Automatic,
    OnDemand,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPolicy {
    mode: RestartMode,
    max_restarts: u32,
    window: Duration,
    base_backoff: Duration,
    max_backoff: Duration,
    consecutive: Option<ConsecutiveFailures>,
    reset_budget_on_ready: bool,
}

/// Cap on restarts granted in a row without the replacement ever reporting ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsecutiveFailures {
    /// Restarts in a row (no `RestartTracker::ready` between them) after which the cap applies.
    pub cap: u32,
    /// What the cap does: the next restart waits this long instead of the exponential backoff and a
    /// new consecutive count starts. `Duration::ZERO` ends the run (`RestartOutcome::Exhausted`).
    pub cooldown: Duration,
}

impl RestartPolicy {
    /// Add a consecutive-failure cap on top of the rolling-window budget (HeardRight's rule: a
    /// replacement that never becomes ready is not retried at backoff speed forever). The rolling
    /// budget still applies first: when it is spent the outcome is `Exhausted` whatever the cap says.
    /// `cap` must be at least 1.
    pub fn with_consecutive_failures(
        mut self,
        cap: u32,
        cooldown: Duration,
    ) -> Result<Self, InvalidRestartPolicy> {
        if cap == 0 {
            return Err(InvalidRestartPolicy);
        }
        self.consecutive = Some(ConsecutiveFailures { cap, cooldown });
        Ok(self)
    }
    /// When a generation reports ready ([`RestartTracker::ready`]), also forget the restarts counted
    /// in the rolling window, so the next crash starts again at the base backoff with a full budget.
    /// Off by default: a crash loop that reaches ready each time is then still limited by the window.
    pub fn with_reset_budget_on_ready(mut self) -> Self {
        self.reset_budget_on_ready = true;
        self
    }
    pub fn consecutive_failures(&self) -> Option<ConsecutiveFailures> {
        self.consecutive
    }
    pub fn resets_budget_on_ready(&self) -> bool {
        self.reset_budget_on_ready
    }
    pub fn mode(&self) -> RestartMode {
        self.mode
    }
    pub fn max_restarts(&self) -> u32 {
        self.max_restarts
    }
    pub fn window(&self) -> Duration {
        self.window
    }
    pub fn base_backoff(&self) -> Duration {
        self.base_backoff
    }
    pub fn max_backoff(&self) -> Duration {
        self.max_backoff
    }
}

impl Default for RestartPolicy {
    /// ScrapeRight's crash-recovery plan: restart automatically, at most 3
    /// unexpected restarts per rolling 60 s window, backoff 250 ms doubling to 5 s.
    fn default() -> Self {
        Self {
            mode: RestartMode::Automatic,
            max_restarts: 3,
            window: Duration::from_secs(60),
            base_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
            consecutive: None,
            reset_budget_on_ready: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRestartPolicy;

impl fmt::Display for InvalidRestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("restart window must be non-zero, max backoff must cover base backoff, and a consecutive-failure cap must be at least 1")
    }
}

impl Error for InvalidRestartPolicy {}

impl RestartPolicy {
    pub fn new(
        mode: RestartMode,
        max_restarts: u32,
        window: Duration,
        base_backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self, InvalidRestartPolicy> {
        if window.is_zero() || max_backoff < base_backoff {
            return Err(InvalidRestartPolicy);
        }
        Ok(Self {
            mode,
            max_restarts,
            window,
            base_backoff,
            max_backoff,
            consecutive: None,
            reset_budget_on_ready: false,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generation(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPermit {
    epoch: u64,
    delay: Duration,
    not_before: Duration,
}

impl RestartPermit {
    pub fn delay(&self) -> Duration {
        self.delay
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartOutcome {
    Restart(RestartPermit),
    AwaitingDemand,
    Exhausted,
    IgnoredStale,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    active: Option<Generation>,
    next_generation: u64,
    restart_times: VecDeque<Duration>,
    consecutive: u32,
    permit_epoch: u64,
    awaiting_demand: bool,
    shutdown: bool,
}

impl RestartTracker {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            active: None,
            next_generation: 0,
            restart_times: VecDeque::new(),
            consecutive: 0,
            permit_epoch: 0,
            awaiting_demand: false,
            shutdown: false,
        }
    }

    pub fn started(&mut self, _at: Duration) -> Generation {
        self.next_generation = self.next_generation.saturating_add(1);
        self.permit_epoch = self.permit_epoch.saturating_add(1);
        self.awaiting_demand = false;
        let generation = Generation(self.next_generation);
        self.active = Some(generation);
        generation
    }

    pub fn exited(&mut self, generation: Generation, at: Duration) -> RestartOutcome {
        if self.shutdown {
            return RestartOutcome::Stopped;
        }
        if self.active != Some(generation) {
            return RestartOutcome::IgnoredStale;
        }
        self.active = None;
        match self.policy.mode {
            RestartMode::Automatic => self.schedule(at),
            RestartMode::OnDemand => {
                self.awaiting_demand = true;
                RestartOutcome::AwaitingDemand
            }
            RestartMode::Disabled => RestartOutcome::Stopped,
        }
    }

    pub fn request_restart(&mut self, at: Duration) -> RestartOutcome {
        if self.shutdown || self.policy.mode == RestartMode::Disabled {
            return RestartOutcome::Stopped;
        }
        if self.policy.mode == RestartMode::OnDemand && !self.awaiting_demand {
            return RestartOutcome::AwaitingDemand;
        }
        self.awaiting_demand = false;
        self.schedule(at)
    }

    /// The running `generation` reported ready: the consecutive-failure count restarts from zero
    /// and, when the policy says so ([`RestartPolicy::with_reset_budget_on_ready`]), so does the
    /// rolling budget. A stale generation changes nothing.
    pub fn ready(&mut self, generation: Generation) {
        if self.active != Some(generation) {
            return;
        }
        self.consecutive = 0;
        if self.policy.reset_budget_on_ready {
            self.restart_times.clear();
        }
    }

    /// Restarts granted in a row since the last [`Self::ready`] (diagnostics).
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive
    }

    /// Restarts granted inside the rolling window ending at `at` (diagnostics).
    pub fn restarts_in_window(&self, at: Duration) -> u32 {
        let window_start = at.saturating_sub(self.policy.window);
        self.restart_times
            .iter()
            .filter(|t| **t >= window_start)
            .count() as u32
    }

    /// Forget the restart budget (a user-initiated start after a terminal stop).
    pub fn reset_budget(&mut self) {
        self.restart_times.clear();
        self.consecutive = 0;
        self.awaiting_demand = false;
        self.shutdown = false;
    }

    pub fn permit_valid(&self, permit: &RestartPermit, at: Duration) -> bool {
        !self.shutdown && permit.epoch == self.permit_epoch && at >= permit.not_before
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        self.active = None;
        self.awaiting_demand = false;
        self.permit_epoch = self.permit_epoch.saturating_add(1);
    }

    fn schedule(&mut self, at: Duration) -> RestartOutcome {
        let window_start = at.saturating_sub(self.policy.window);
        while self
            .restart_times
            .front()
            .is_some_and(|restart| *restart < window_start)
        {
            self.restart_times.pop_front();
        }
        if self.restart_times.len() >= self.policy.max_restarts as usize {
            return RestartOutcome::Exhausted;
        }

        let capped = match self.policy.consecutive {
            Some(c) if self.consecutive >= c.cap => {
                if c.cooldown.is_zero() {
                    return RestartOutcome::Exhausted;
                }
                Some(c.cooldown)
            }
            _ => None,
        };
        let delay = match capped {
            Some(cooldown) => {
                // The cap was served by this cooldown; a new consecutive count starts.
                self.consecutive = 0;
                cooldown
            }
            None => {
                let exponent = u32::try_from(self.restart_times.len()).unwrap_or(u32::MAX);
                let multiplier = 1_u32.checked_shl(exponent.min(31)).unwrap_or(u32::MAX);
                self.policy
                    .base_backoff
                    .checked_mul(multiplier)
                    .unwrap_or(self.policy.max_backoff)
                    .min(self.policy.max_backoff)
            }
        };
        self.consecutive = self.consecutive.saturating_add(1);
        self.restart_times.push_back(at);
        self.permit_epoch = self.permit_epoch.saturating_add(1);
        let permit = RestartPermit {
            epoch: self.permit_epoch,
            delay,
            not_before: at.checked_add(delay).unwrap_or(Duration::MAX),
        };
        RestartOutcome::Restart(permit)
    }
}
