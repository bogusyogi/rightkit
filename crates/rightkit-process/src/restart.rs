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
}

impl RestartPolicy {
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
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRestartPolicy;

impl fmt::Display for InvalidRestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("restart window must be non-zero and max backoff must cover base backoff")
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

        let exponent = u32::try_from(self.restart_times.len()).unwrap_or(u32::MAX);
        let multiplier = 1_u32.checked_shl(exponent.min(31)).unwrap_or(u32::MAX);
        let delay = self
            .policy
            .base_backoff
            .checked_mul(multiplier)
            .unwrap_or(self.policy.max_backoff)
            .min(self.policy.max_backoff);
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
