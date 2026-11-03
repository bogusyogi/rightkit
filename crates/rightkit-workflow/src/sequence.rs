//! Generic linear, resumable run over non-GUI steps (feature `sequence`).
//!
//! A [`Sequence`] is an ordered list of [`StepSpec`]s. [`SequenceRun`] drives it
//! one step per [`SequenceRun::run_next`] call over the same [`Run`] machine,
//! journal and idempotency keys that the scenario runtime uses, so a restart at
//! any step boundary is normal:
//!
//! - `Dispatched` is durable before the driver's `perform` is called, and a
//!   step whose phase is already `Dispatched` or `Performed` resumes into
//!   observe without calling `perform` again.
//! - A [`Gate`] runs before a step is first dispatched (the app's budget
//!   reservation belongs there). A refusal stops the run with nothing written
//!   for that step, so the same call can be made again later.
//! - A step that ends `Unresolved` is never silently re-performed.
//!   [`ReplayPolicy::Unresolved`] reports it. [`ReplayPolicy::ReissueOnce`]
//!   allows exactly one further take with a new idempotency key.
//!
//! The journal is one file per run. "Commit once" therefore rests on the
//! idempotency key the app passes to its commit step, not on this crate.

use std::path::Path;

use serde_json::Value;

use crate::clock::Clock;
use crate::driver::{CancelToken, IdempotencyKey, ObservePolicy, StepDriver, StepSpec};
use crate::error::{Error, Result};
use crate::machine::{ExecContext, Phase, Run, StepRef};

/// An ordered, linear list of steps. Step ids key the journal, so they must be
/// non-empty and unique, and the list must not be empty.
#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub steps: Vec<StepSpec>,
}

impl Sequence {
    pub fn new(steps: Vec<StepSpec>) -> Self {
        Self { steps }
    }

    pub fn validate(&self) -> Result<()> {
        if self.steps.is_empty() {
            return Err(Error::Invalid("empty sequence".into()));
        }
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

/// What to do with a step that ended `Unresolved` (dispatched, outcome never
/// observed).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplayPolicy {
    /// Report `Unresolved` and stop. The default.
    #[default]
    Unresolved,
    /// Run the step once more in a new take (new idempotency key). A second
    /// unresolved outcome is reported as `Unresolved`.
    ReissueOnce,
}

/// Decision of a [`Gate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateDecision {
    Proceed,
    Stop(String),
}

/// Pre-step hook, called before a step's first dispatch in a take and never
/// when a dispatched step resumes into observe. The key is the one `perform`
/// will receive. Nothing is journalled for the step before the call, so after
/// a crash between the gate and the dispatch the gate is called again with the
/// same key: an implementation (a budget reservation, say) must be idempotent
/// on the key.
pub trait Gate {
    fn before_perform(&mut self, step: &StepSpec, key: &IdempotencyKey) -> GateDecision;
}

/// A gate that always proceeds.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGate;

impl Gate for NoGate {
    fn before_perform(&mut self, _step: &StepSpec, _key: &IdempotencyKey) -> GateDecision {
        GateDecision::Proceed
    }
}

/// Why a call stopped. `Gated` is not durable and can be retried; `Failed` and
/// `Cancelled` are terminal journal states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    Gated { step: String, reason: String },
    Failed { step: String, reason: String },
    Cancelled { step: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SequenceOutcome {
    /// A non-final step completed. Call `run_next` again.
    Advanced {
        step: String,
        value: Value,
    },
    /// The final step completed. Carries its value. Repeated calls return it again.
    Done(Value),
    Stopped(StopReason),
    /// A step's outcome was never observed and the replay policy does not
    /// allow another attempt.
    Unresolved {
        step: String,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct SequenceConfig {
    pub worker: String,
    pub lease_ttl_ms: u64,
    pub policy: ObservePolicy,
    pub replay: ReplayPolicy,
}

impl Default for SequenceConfig {
    fn default() -> Self {
        Self {
            worker: "sequence".into(),
            lease_ttl_ms: 60_000,
            policy: ObservePolicy {
                timeout_ms: 30_000,
                interval_ms: 100,
            },
            replay: ReplayPolicy::default(),
        }
    }
}

pub struct SequenceRun<C: Clock> {
    run_id: String,
    run: Run<C>,
    config: SequenceConfig,
    cancel: CancelToken,
}

impl<C: Clock> SequenceRun<C> {
    /// Opens the run's journal and rebuilds state, with default configuration.
    pub fn open(path: &Path, run_id: &str, clock: C) -> Result<Self> {
        Ok(Self {
            run_id: run_id.to_string(),
            run: Run::open(path, run_id, clock)?,
            config: SequenceConfig::default(),
            cancel: CancelToken::new(),
        })
    }

    pub fn with_config(mut self, config: SequenceConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// The underlying machine, for read access to phases, values and notes.
    pub fn run(&self) -> &Run<C> {
        &self.run
    }

    /// Runs at most one step to a result. Completed steps are skipped, an
    /// in-flight step resumes into observe, and the next pending step is
    /// gated, dispatched and observed.
    pub fn run_next<D: StepDriver, G: Gate>(
        &mut self,
        sequence: &Sequence,
        driver: &mut D,
        gate: &mut G,
    ) -> Result<SequenceOutcome> {
        sequence.validate()?;
        let last = sequence.steps.len() - 1;
        for (index, spec) in sequence.steps.iter().enumerate() {
            let takes = self.run.takes_of(&spec.step_id);
            let take = match takes.last().copied() {
                None => 0,
                Some(latest) => {
                    let current = StepRef {
                        step: spec.step_id.clone(),
                        take: latest,
                    };
                    match self.run.phase(&current) {
                        Phase::Completed => continue,
                        Phase::Failed => {
                            return Ok(SequenceOutcome::Stopped(StopReason::Failed {
                                step: spec.step_id.clone(),
                                reason: self.run.note(&current).unwrap_or_default().to_string(),
                            }))
                        }
                        Phase::Cancelled => {
                            return Ok(SequenceOutcome::Stopped(StopReason::Cancelled {
                                step: spec.step_id.clone(),
                            }))
                        }
                        Phase::Unresolved => {
                            if self.config.replay == ReplayPolicy::ReissueOnce && takes.len() == 1 {
                                self.run.next_take()
                            } else {
                                return Ok(SequenceOutcome::Unresolved {
                                    step: spec.step_id.clone(),
                                    reason: self.run.note(&current).unwrap_or_default().to_string(),
                                });
                            }
                        }
                        Phase::Pending | Phase::Dispatched | Phase::Performed => latest,
                    }
                }
            };

            let step = StepRef {
                step: spec.step_id.clone(),
                take,
            };
            if self.run.phase(&step) == Phase::Pending && !self.cancel.is_requested() {
                let key = IdempotencyKey::new(&self.run_id, &step);
                if let GateDecision::Stop(reason) = gate.before_perform(spec, &key) {
                    return Ok(SequenceOutcome::Stopped(StopReason::Gated {
                        step: spec.step_id.clone(),
                        reason,
                    }));
                }
            }

            let lease = self
                .run
                .acquire(&step, &self.config.worker, self.config.lease_ttl_ms)?;
            let ctx = ExecContext {
                worker: &self.config.worker,
                lease_ttl_ms: self.config.lease_ttl_ms,
                policy: self.config.policy,
                cancel: &self.cancel,
            };
            let phase = self.run.execute(&lease, spec, driver, &ctx)?;
            return match phase {
                Phase::Completed => {
                    let value = self.run.value(&step).cloned().unwrap_or(Value::Null);
                    if index == last {
                        Ok(SequenceOutcome::Done(value))
                    } else {
                        Ok(SequenceOutcome::Advanced {
                            step: spec.step_id.clone(),
                            value,
                        })
                    }
                }
                Phase::Failed => Ok(SequenceOutcome::Stopped(StopReason::Failed {
                    step: spec.step_id.clone(),
                    reason: self.run.note(&step).unwrap_or_default().to_string(),
                })),
                Phase::Cancelled => Ok(SequenceOutcome::Stopped(StopReason::Cancelled {
                    step: spec.step_id.clone(),
                })),
                Phase::Unresolved => Ok(SequenceOutcome::Unresolved {
                    step: spec.step_id.clone(),
                    reason: self.run.note(&step).unwrap_or_default().to_string(),
                }),
                other => Err(Error::Invalid(format!(
                    "{step} returned non-terminal phase {other:?}"
                ))),
            };
        }

        // Every step is completed: report the final step's value again.
        let spec = &sequence.steps[last];
        let value = self
            .run
            .takes_of(&spec.step_id)
            .last()
            .and_then(|t| {
                self.run
                    .value(&StepRef {
                        step: spec.step_id.clone(),
                        take: *t,
                    })
                    .cloned()
            })
            .unwrap_or(Value::Null);
        Ok(SequenceOutcome::Done(value))
    }
}
