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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::driver::{DriverError, Observation};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempJournal(PathBuf);

    impl TempJournal {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            Self(std::env::temp_dir().join(format!(
                "rightkit-workflow-seq-{}-{n}.jsonl",
                std::process::id()
            )))
        }
    }

    impl Drop for TempJournal {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn spec(id: &str) -> StepSpec {
        StepSpec {
            step_id: id.into(),
            label: id.into(),
            action: json!({}),
            expect: json!({}),
        }
    }

    fn seq(ids: &[&str]) -> Sequence {
        Sequence::new(ids.iter().map(|i| spec(i)).collect())
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Ok,
        Never,
        NeverOnTakeZero,
        Fail,
    }

    struct Mock {
        performed: Vec<String>,
        mode: std::collections::BTreeMap<String, Mode>,
    }

    impl Mock {
        fn new() -> Self {
            Self {
                performed: Vec::new(),
                mode: Default::default(),
            }
        }
        fn with(mut self, step: &str, mode: Mode) -> Self {
            self.mode.insert(step.into(), mode);
            self
        }
    }

    impl StepDriver for Mock {
        fn perform(
            &mut self,
            _step: &StepSpec,
            key: &IdempotencyKey,
        ) -> std::result::Result<(), DriverError> {
            self.performed.push(key.as_str().to_string());
            Ok(())
        }

        fn observe(
            &mut self,
            step: &StepSpec,
            key: &IdempotencyKey,
        ) -> std::result::Result<Observation, DriverError> {
            let mode = self.mode.get(&step.step_id).copied().unwrap_or(Mode::Ok);
            Ok(match mode {
                Mode::Ok => Observation::Satisfied(json!(step.step_id)),
                Mode::Never => Observation::Pending,
                Mode::NeverOnTakeZero if key.as_str().ends_with("take-0") => Observation::Pending,
                Mode::NeverOnTakeZero => Observation::Satisfied(json!("second")),
                Mode::Fail => Observation::Failed("bad".into()),
            })
        }
    }

    fn config(replay: ReplayPolicy) -> SequenceConfig {
        SequenceConfig {
            worker: "w".into(),
            lease_ttl_ms: 1_000_000,
            policy: ObservePolicy {
                timeout_ms: 30,
                interval_ms: 10,
            },
            replay,
        }
    }

    fn open(
        path: &TempJournal,
        clock: &ManualClock,
        replay: ReplayPolicy,
    ) -> SequenceRun<ManualClock> {
        SequenceRun::open(&path.0, "r1", clock.clone())
            .unwrap()
            .with_config(config(replay))
    }

    #[test]
    fn runs_one_step_per_call_to_done() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        let s = seq(&["a", "b"]);
        let mut d = Mock::new();
        let out = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert_eq!(
            out,
            SequenceOutcome::Advanced {
                step: "a".into(),
                value: json!("a")
            }
        );
        assert_eq!(d.performed, vec!["r1/a/take-0"]);
        let out = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert_eq!(out, SequenceOutcome::Done(json!("b")));
        let out = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert_eq!(out, SequenceOutcome::Done(json!("b")));
        assert_eq!(d.performed.len(), 2);
    }

    #[test]
    fn resumes_after_restart_without_repeating_steps() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a", "b"]);
        let mut d = Mock::new();
        {
            let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
            run.run_next(&s, &mut d, &mut NoGate).unwrap();
        }
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        let out = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert_eq!(out, SequenceOutcome::Done(json!("b")));
        assert_eq!(d.performed, vec!["r1/a/take-0", "r1/b/take-0"]);
        let reopened = open(&j, &clock, ReplayPolicy::Unresolved);
        assert_eq!(
            reopened.run().phase(&StepRef {
                step: "b".into(),
                take: 0
            }),
            Phase::Completed
        );
    }

    #[test]
    fn unresolved_policy_reports_and_never_reperforms() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a"]);
        let mut d = Mock::new().with("a", Mode::Never);
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        let first = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert!(matches!(first, SequenceOutcome::Unresolved { .. }));
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        let again = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert!(matches!(again, SequenceOutcome::Unresolved { .. }));
        assert_eq!(d.performed.len(), 1);
    }

    #[test]
    fn reissue_once_takes_a_new_key_exactly_once() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a"]);
        let mut d = Mock::new().with("a", Mode::NeverOnTakeZero);
        let mut run = open(&j, &clock, ReplayPolicy::ReissueOnce);
        assert!(matches!(
            run.run_next(&s, &mut d, &mut NoGate).unwrap(),
            SequenceOutcome::Unresolved { .. }
        ));
        let out = run.run_next(&s, &mut d, &mut NoGate).unwrap();
        assert_eq!(out, SequenceOutcome::Done(json!("second")));
        assert_eq!(d.performed, vec!["r1/a/take-0", "r1/a/take-1"]);

        // A reissue that is also unresolved is not reissued again.
        let j2 = TempJournal::new();
        let mut d2 = Mock::new().with("a", Mode::Never);
        let mut run2 = open(&j2, &clock, ReplayPolicy::ReissueOnce);
        run2.run_next(&s, &mut d2, &mut NoGate).unwrap();
        assert!(matches!(
            run2.run_next(&s, &mut d2, &mut NoGate).unwrap(),
            SequenceOutcome::Unresolved { .. }
        ));
        assert!(matches!(
            run2.run_next(&s, &mut d2, &mut NoGate).unwrap(),
            SequenceOutcome::Unresolved { .. }
        ));
        assert_eq!(d2.performed.len(), 2);
    }

    struct Budget {
        allow: bool,
        calls: Vec<(String, String)>,
    }

    impl Gate for Budget {
        fn before_perform(&mut self, step: &StepSpec, key: &IdempotencyKey) -> GateDecision {
            self.calls
                .push((step.step_id.clone(), key.as_str().to_string()));
            if self.allow || step.step_id == "a" {
                GateDecision::Proceed
            } else {
                GateDecision::Stop("budget".into())
            }
        }
    }

    #[test]
    fn gate_refusal_stops_before_dispatch_and_can_be_retried() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a", "b"]);
        let mut d = Mock::new();
        let mut gate = Budget {
            allow: false,
            calls: Vec::new(),
        };
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        run.run_next(&s, &mut d, &mut gate).unwrap();
        let out = run.run_next(&s, &mut d, &mut gate).unwrap();
        assert_eq!(
            out,
            SequenceOutcome::Stopped(StopReason::Gated {
                step: "b".into(),
                reason: "budget".into()
            })
        );
        assert_eq!(d.performed, vec!["r1/a/take-0"]);
        assert_eq!(
            run.run().phase(&StepRef {
                step: "b".into(),
                take: 0
            }),
            Phase::Pending
        );
        assert_eq!(gate.calls[1], ("b".to_string(), "r1/b/take-0".to_string()));

        gate.allow = true;
        let out = run.run_next(&s, &mut d, &mut gate).unwrap();
        assert_eq!(out, SequenceOutcome::Done(json!("b")));
        assert_eq!(d.performed.len(), 2);
    }

    #[test]
    fn gate_is_not_called_for_completed_steps() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a"]);
        let mut d = Mock::new();
        let mut gate = Budget {
            allow: true,
            calls: Vec::new(),
        };
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved);
        run.run_next(&s, &mut d, &mut gate).unwrap();
        assert_eq!(gate.calls.len(), 1);
        run.run_next(&s, &mut d, &mut gate).unwrap();
        assert_eq!(gate.calls.len(), 1);
    }

    #[test]
    fn failed_step_stops_the_run() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a", "b"]);
        let mut d = Mock::new().with("a", Mode::Fail);
        let mut run = open(&j, &clock, ReplayPolicy::ReissueOnce);
        let want = SequenceOutcome::Stopped(StopReason::Failed {
            step: "a".into(),
            reason: "bad".into(),
        });
        assert_eq!(run.run_next(&s, &mut d, &mut NoGate).unwrap(), want);
        assert_eq!(run.run_next(&s, &mut d, &mut NoGate).unwrap(), want);
        assert_eq!(d.performed.len(), 1);
    }

    #[test]
    fn cancelled_run_records_cancel_without_gate_or_perform() {
        let j = TempJournal::new();
        let clock = ManualClock::new(1000);
        let s = seq(&["a"]);
        let token = CancelToken::new();
        token.request(5000);
        let mut run = open(&j, &clock, ReplayPolicy::Unresolved).with_cancel(token);
        let mut d = Mock::new();
        let mut gate = Budget {
            allow: true,
            calls: Vec::new(),
        };
        let out = run.run_next(&s, &mut d, &mut gate).unwrap();
        assert_eq!(
            out,
            SequenceOutcome::Stopped(StopReason::Cancelled { step: "a".into() })
        );
        assert!(gate.calls.is_empty());
        assert!(d.performed.is_empty());
    }

    #[test]
    fn invalid_sequences_are_rejected() {
        assert!(seq(&[]).validate().is_err());
        assert!(seq(&["a", "a"]).validate().is_err());
        assert!(seq(&[""]).validate().is_err());
        assert!(seq(&["a", "b"]).validate().is_ok());
    }
}
