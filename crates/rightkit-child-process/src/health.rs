use std::{error::Error, fmt, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthProbe {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthOutcome {
    Starting,
    Healthy,
    Unhealthy { consecutive_failures: u32 },
    StartupTimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthPolicy {
    startup_timeout: Duration,
    unhealthy_threshold: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidHealthPolicy;

impl fmt::Display for InvalidHealthPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("startup timeout and unhealthy threshold must be non-zero")
    }
}

impl Error for InvalidHealthPolicy {}

impl HealthPolicy {
    pub fn new(
        startup_timeout: Duration,
        unhealthy_threshold: u32,
    ) -> Result<Self, InvalidHealthPolicy> {
        if startup_timeout.is_zero() || unhealthy_threshold == 0 {
            return Err(InvalidHealthPolicy);
        }
        Ok(Self {
            startup_timeout,
            unhealthy_threshold,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HealthTracker {
    policy: HealthPolicy,
    started_at: Duration,
    ever_healthy: bool,
    consecutive_failures: u32,
}

impl HealthTracker {
    pub fn new(policy: HealthPolicy, started_at: Duration) -> Self {
        Self {
            policy,
            started_at,
            ever_healthy: false,
            consecutive_failures: 0,
        }
    }

    pub fn observe(&mut self, at: Duration, probe: HealthProbe) -> HealthOutcome {
        match probe {
            HealthProbe::Success => {
                self.ever_healthy = true;
                self.consecutive_failures = 0;
            }
            HealthProbe::Failure => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            }
        }
        self.outcome(at)
    }

    pub fn outcome(&self, at: Duration) -> HealthOutcome {
        if !self.ever_healthy && at.saturating_sub(self.started_at) >= self.policy.startup_timeout {
            return HealthOutcome::StartupTimedOut;
        }
        if self.consecutive_failures >= self.policy.unhealthy_threshold {
            return HealthOutcome::Unhealthy {
                consecutive_failures: self.consecutive_failures,
            };
        }
        if self.ever_healthy {
            HealthOutcome::Healthy
        } else {
            HealthOutcome::Starting
        }
    }
}
