//! Public error-rate circuit breaker, keyed by endpoint (Orthic-Labs/rightkit#659).
//!
//! Unlike the breaker inside [`crate::failover::FailoverChain`], which counts fail-overs away
//! from a route, this one counts request outcomes. Per key it keeps the last
//! [`BreakerConfig::window`] outcomes. It trips open when there are at least
//! [`BreakerConfig::min_samples`] outcomes and at least [`BreakerConfig::error_ratio`] of them
//! failed. It stays open for [`BreakerConfig::open_for`], then lets exactly one half-open
//! probe through: a success closes it, a failure opens it again.
//!
//! Only transport failures, HTTP 5xx and HTTP 429 count as failures ([`Outcome::from_error`]).
//! Every other error is [`Outcome::Ignored`] and does not move the breaker.
//!
//! ```no_run
//! use rightkit_llm::breaker::{BreakerConfig, CircuitBreaker, Outcome};
//!
//! let breaker = CircuitBreaker::new(BreakerConfig::default());
//! if breaker.allow("api.example.com") {
//!     // ... send the request ...
//!     # let result: Result<(), rightkit_llm::LlmError> = Ok(());
//!     match &result {
//!         Ok(()) => breaker.record("api.example.com", Outcome::Success),
//!         Err(e) => breaker.record("api.example.com", Outcome::from_error(e)),
//!     }
//! }
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::LlmError;

/// Thresholds for a [`CircuitBreaker`]. The defaults match the CodeRight breaker.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakerConfig {
    /// Number of most recent outcomes kept per key. Default 20.
    pub window: usize,
    /// Fewest outcomes in the window before the breaker may trip. Default 5.
    pub min_samples: usize,
    /// Fraction of failed outcomes in the window that trips the breaker. Default 0.5.
    pub error_ratio: f64,
    /// How long the breaker stays open before one half-open probe. Default 60 s.
    pub open_for: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            window: 20,
            min_samples: 5,
            error_ratio: 0.5,
            open_for: Duration::from_secs(60),
        }
    }
}

/// The result of one request, as the breaker sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The request succeeded.
    Success,
    /// A transport failure, HTTP 5xx or HTTP 429.
    Failure,
    /// Any other error. It is not counted and does not move the breaker.
    Ignored,
}

impl Outcome {
    /// Classify an error. Transport failures ([`LlmError::Http`]), 5xx and 429 are
    /// [`Outcome::Failure`]; everything else is [`Outcome::Ignored`].
    pub fn from_error(error: &LlmError) -> Self {
        match error.root() {
            LlmError::Http(_) => Outcome::Failure,
            LlmError::Status { status, .. } if *status == 429 || *status >= 500 => Outcome::Failure,
            _ => Outcome::Ignored,
        }
    }
}

/// Where a key is in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Calls are allowed and outcomes are counted.
    Closed,
    /// Calls are refused until the open period ends.
    Open,
    /// One probe call is allowed; its outcome decides the next phase.
    HalfOpen,
}

#[derive(Debug)]
struct KeyState {
    /// `true` for a failure, most recent last.
    outcomes: VecDeque<bool>,
    phase: Phase,
    opened_at: Option<Instant>,
    probe_in_flight: bool,
}

impl KeyState {
    fn closed() -> Self {
        Self {
            outcomes: VecDeque::new(),
            phase: Phase::Closed,
            opened_at: None,
            probe_in_flight: false,
        }
    }
}

/// Error-rate circuit breaker keyed by a string (an endpoint or a route name). Thread-safe.
#[derive(Debug)]
pub struct CircuitBreaker {
    config: BreakerConfig,
    keys: Mutex<HashMap<String, KeyState>>,
}

impl CircuitBreaker {
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            keys: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &BreakerConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, KeyState>> {
        // A panic while the lock was held cannot leave the counters in a state that is
        // unsafe to read, so a poisoned lock is still used.
        self.keys.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current phase of `key`. A key never seen is [`Phase::Closed`]. An open key whose
    /// open period has ended still reports [`Phase::Open`] until [`CircuitBreaker::allow`]
    /// moves it to half-open.
    pub fn phase(&self, key: &str) -> Phase {
        self.lock().get(key).map_or(Phase::Closed, |s| s.phase)
    }

    /// Whether a call to `key` may start now. In the half-open phase only the first caller
    /// gets `true`; the others get `false` until that probe reports an outcome.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut g = self.lock();
        let state = g.entry(key.to_owned()).or_insert_with(KeyState::closed);
        match state.phase {
            Phase::Closed => true,
            Phase::Open => {
                let ready = state
                    .opened_at
                    .is_some_and(|t| now.duration_since(t) >= self.config.open_for);
                if ready {
                    state.phase = Phase::HalfOpen;
                    state.probe_in_flight = true;
                    true
                } else {
                    false
                }
            }
            Phase::HalfOpen => {
                if state.probe_in_flight {
                    false
                } else {
                    state.probe_in_flight = true;
                    true
                }
            }
        }
    }

    /// Record the outcome of a call to `key`. [`Outcome::Ignored`] releases a half-open
    /// probe without counting it. Outcomes that arrive while the breaker is open (calls that
    /// were already in flight) are dropped.
    pub fn record(&self, key: &str, outcome: Outcome) {
        if outcome == Outcome::Ignored {
            if let Some(state) = self.lock().get_mut(key) {
                state.probe_in_flight = false;
            }
            return;
        }
        let now = Instant::now();
        let mut g = self.lock();
        let state = g.entry(key.to_owned()).or_insert_with(KeyState::closed);
        let failed = outcome == Outcome::Failure;
        match state.phase {
            Phase::Open => {}
            Phase::HalfOpen => {
                state.probe_in_flight = false;
                if failed {
                    state.phase = Phase::Open;
                    state.opened_at = Some(now);
                } else {
                    state.phase = Phase::Closed;
                    state.opened_at = None;
                    state.outcomes.clear();
                }
            }
            Phase::Closed => {
                state.outcomes.push_back(failed);
                while state.outcomes.len() > self.config.window {
                    state.outcomes.pop_front();
                }
                let total = state.outcomes.len();
                let failures = state.outcomes.iter().filter(|f| **f).count();
                if total >= self.config.min_samples
                    && failures as f64 >= self.config.error_ratio * total as f64
                {
                    state.phase = Phase::Open;
                    state.opened_at = Some(now);
                    state.outcomes.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(open_for: Duration) -> BreakerConfig {
        BreakerConfig {
            open_for,
            ..BreakerConfig::default()
        }
    }

    #[test]
    fn trips_at_half_failures_once_enough_samples() {
        let b = CircuitBreaker::new(config(Duration::from_secs(60)));
        for _ in 0..4 {
            b.record("k", Outcome::Failure);
        }
        // Four samples is below the minimum of five.
        assert_eq!(b.phase("k"), Phase::Closed);
        b.record("k", Outcome::Failure);
        assert_eq!(b.phase("k"), Phase::Open);
        assert!(!b.allow("k"));
    }

    #[test]
    fn successes_keep_it_closed_and_other_keys_are_independent() {
        let b = CircuitBreaker::new(config(Duration::from_secs(60)));
        for _ in 0..10 {
            b.record("ok", Outcome::Success);
        }
        b.record("bad", Outcome::Failure);
        assert!(b.allow("ok"));
        assert_eq!(b.phase("ok"), Phase::Closed);
        assert_eq!(b.phase("bad"), Phase::Closed);
    }

    #[test]
    fn ignored_outcomes_are_not_counted() {
        let b = CircuitBreaker::new(config(Duration::from_secs(60)));
        for _ in 0..20 {
            b.record("k", Outcome::Ignored);
        }
        assert_eq!(b.phase("k"), Phase::Closed);
    }

    #[test]
    fn half_open_admits_one_probe_then_closes_on_success() {
        let b = CircuitBreaker::new(config(Duration::ZERO));
        for _ in 0..5 {
            b.record("k", Outcome::Failure);
        }
        assert!(b.allow("k"));
        assert_eq!(b.phase("k"), Phase::HalfOpen);
        assert!(!b.allow("k"));
        b.record("k", Outcome::Success);
        assert_eq!(b.phase("k"), Phase::Closed);
        assert!(b.allow("k"));
    }

    #[test]
    fn failed_probe_reopens() {
        let b = CircuitBreaker::new(config(Duration::ZERO));
        for _ in 0..5 {
            b.record("k", Outcome::Failure);
        }
        assert!(b.allow("k"));
        b.record("k", Outcome::Failure);
        assert_eq!(b.phase("k"), Phase::Open);
    }

    #[test]
    fn classifies_transport_5xx_and_429_only() {
        let status = |s| LlmError::Status {
            status: s,
            message: String::new(),
        };
        assert_eq!(Outcome::from_error(&status(503)), Outcome::Failure);
        assert_eq!(Outcome::from_error(&status(429)), Outcome::Failure);
        assert_eq!(Outcome::from_error(&status(400)), Outcome::Ignored);
        assert_eq!(
            Outcome::from_error(&LlmError::Malformed("x".into())),
            Outcome::Ignored
        );
    }
}
