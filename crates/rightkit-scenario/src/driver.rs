use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::machine::StepRef;

/// One scenario step. The interpreter treats `action` and `expect` as opaque;
/// the driver gives them meaning. `step_id` matches the content brief's
/// `steps[].stepId`, and is the stable identity for journal and retake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepSpec {
    pub step_id: String,
    pub label: String,
    pub action: Value,
    pub expect: Value,
}

/// An ordered scenario. `scenario_id` matches the brief's `scenario.scenarioId`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub scenario_id: String,
    pub steps: Vec<StepSpec>,
}

impl Scenario {
    /// Step ids key the journal, so they must be non-empty and unique.
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for step in &self.steps {
            if step.step_id.is_empty() {
                return Err(Error::Invalid("empty step_id".into()));
            }
            if !seen.insert(step.step_id.as_str()) {
                return Err(Error::Invalid(format!(
                    "duplicate step_id {}",
                    step.step_id
                )));
            }
        }
        Ok(())
    }
}

/// Stable key for one perform. It is `run/step/take-N`, so a driver or remote
/// worker can deduplicate across reconnects.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// The key for one `(run, step, take)`. The machine derives its own keys;
    /// this is public so a remote driver and its tests can name one.
    pub fn new(run_id: &str, step: &StepRef) -> Self {
        Self(format!("{run_id}/{}/take-{}", step.step, step.take))
    }

    /// Rebuilds a key received from the controller over the worker wire.
    pub(crate) fn from_wire(key: String) -> Self {
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Result of one postcondition check.
#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    /// The postcondition holds. The value is recorded as the step's result.
    Satisfied(Value),
    /// Not yet visible. Keep polling until the deadline.
    Pending,
    /// The state is definitively wrong. The step fails and is not retried.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverError(pub String);

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Supplies the real actions. Implementors must not assume `perform` is retried.
pub trait StepDriver {
    /// Performs the non-idempotent action. The machine calls this at most once
    /// per `(run, step, take)`, and only after `Dispatched` is durable.
    fn perform(
        &mut self,
        step: &StepSpec,
        key: &IdempotencyKey,
    ) -> std::result::Result<(), DriverError>;

    /// Checks the postcondition once. The machine decides how often to poll.
    /// `Err` is treated as transient and polled again until the deadline.
    fn observe(
        &mut self,
        step: &StepSpec,
        key: &IdempotencyKey,
    ) -> std::result::Result<Observation, DriverError>;
}

/// Bounds the observe loop for one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservePolicy {
    pub timeout_ms: u64,
    pub interval_ms: u64,
}

/// Cooperative cancellation with a deadline. Once requested, the machine stops
/// at its next poll and does not wait past `by_ms` to sleep. A running driver
/// call is not preempted.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    inner: Arc<Mutex<Option<u64>>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. The first request's deadline wins.
    pub fn request(&self, by_ms: u64) {
        let mut guard = self.lock();
        if guard.is_none() {
            *guard = Some(by_ms);
        }
    }

    pub fn is_requested(&self) -> bool {
        self.lock().is_some()
    }

    pub fn deadline_ms(&self) -> Option<u64> {
        *self.lock()
    }

    fn lock(&self) -> MutexGuard<'_, Option<u64>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}
