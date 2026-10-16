//! The run state machine. State is a fold over the journal: `apply` is the only
//! place state changes, both on replay at open and after each append.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde_json::Value;

use crate::clock::Clock;
use crate::driver::{
    CancelToken, IdempotencyKey, Observation, ObservePolicy, Scenario, StepDriver, StepSpec,
};
use crate::error::{Error, Result};
use crate::journal::{Event, Journal, Record};

/// One step within one take. A retake creates a new take for the replayed
/// prefix and the retaken step; earlier takes are never rewritten.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StepRef {
    pub step: String,
    pub take: u32,
}

impl fmt::Display for StepRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#take-{}", self.step, self.take)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Pending,
    /// `Dispatched` is durable. The action may have happened, so only observe
    /// may run from here on.
    Dispatched,
    Performed,
    Completed,
    Failed,
    Unresolved,
    Cancelled,
}

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Phase::Completed | Phase::Failed | Phase::Unresolved | Phase::Cancelled
        )
    }
}

/// Permission to write for one step. Writes are valid only while `fence` is the
/// current grant and `now < expires_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub step: StepRef,
    pub worker: String,
    pub fence: u64,
    pub expires_ms: u64,
}

/// Execution parameters shared by a run of steps.
#[derive(Clone, Copy, Debug)]
pub struct ExecContext<'a> {
    pub worker: &'a str,
    pub lease_ttl_ms: u64,
    pub policy: ObservePolicy,
    pub cancel: &'a CancelToken,
}

#[derive(Clone, Debug, Default)]
struct StepState {
    phase: Phase,
    value: Option<Value>,
    note: Option<String>,
}

#[derive(Clone, Debug)]
struct Grant {
    worker: String,
    fence: u64,
    expires_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetakeReport {
    /// The new take that holds the replayed prefix and the retaken step.
    pub take: u32,
    /// Phase of the last step attempted in the new take.
    pub phase: Phase,
    /// `None` unless the retaken step completed and was compared.
    pub matches: Option<bool>,
    /// Downstream steps whose state no longer follows from the original take.
    pub invalidated: Vec<String>,
}

pub struct Run<C: Clock> {
    run_id: String,
    journal: Journal,
    clock: C,
    steps: BTreeMap<StepRef, StepState>,
    grants: BTreeMap<StepRef, Grant>,
}

impl<C: Clock> Run<C> {
    /// Opens the run's journal and rebuilds state from it.
    pub fn open(path: &Path, run_id: &str, clock: C) -> Result<Self> {
        let (journal, records) = Journal::open(path, run_id)?;
        let mut run = Run {
            run_id: run_id.to_string(),
            journal,
            clock,
            steps: BTreeMap::new(),
            grants: BTreeMap::new(),
        };
        for record in &records {
            run.apply(record);
        }
        Ok(run)
    }

    pub fn phase(&self, step: &StepRef) -> Phase {
        self.steps.get(step).map(|s| s.phase).unwrap_or_default()
    }

    pub fn value(&self, step: &StepRef) -> Option<&Value> {
        self.steps.get(step).and_then(|s| s.value.as_ref())
    }

    pub fn note(&self, step: &StepRef) -> Option<&str> {
        self.steps.get(step).and_then(|s| s.note.as_deref())
    }

    /// The first unused take number.
    pub fn next_take(&self) -> u32 {
        self.steps.keys().map(|s| s.take).max().map_or(0, |t| t + 1)
    }

    /// Grants a lease if the step is not terminal and no unexpired lease holds it.
    /// A grant after expiry raises the fence, which fences the old holder.
    pub fn acquire(&mut self, step: &StepRef, worker: &str, ttl_ms: u64) -> Result<Lease> {
        let phase = self.phase(step);
        if phase.is_terminal() {
            return Err(Error::Terminal {
                step: step.clone(),
                phase,
            });
        }
        let now = self.clock.now_ms();
        let fence = match self.grants.get(step) {
            Some(g) if now < g.expires_ms => {
                return Err(Error::Held {
                    step: step.clone(),
                    worker: g.worker.clone(),
                })
            }
            Some(g) => g.fence + 1,
            None => 1,
        };
        let expires_ms = now.saturating_add(ttl_ms);
        self.append(
            step,
            fence,
            Event::LeaseGranted {
                worker: worker.to_string(),
                expires_ms,
            },
        )?;
        Ok(Lease {
            step: step.clone(),
            worker: worker.to_string(),
            fence,
            expires_ms,
        })
    }

    /// Extends a live lease. An expired or superseded lease cannot renew.
    pub fn renew(&mut self, lease: &Lease, ttl_ms: u64) -> Result<Lease> {
        self.check_lease(lease)?;
        let expires_ms = self.clock.now_ms().saturating_add(ttl_ms);
        self.write(lease, Event::LeaseRenewed { expires_ms })?;
        Ok(Lease {
            expires_ms,
            ..lease.clone()
        })
    }

    /// Executes one step under a lease.
    ///
    /// Terminal steps return immediately. A `Pending` step is dispatched once,
    /// with `Dispatched` made durable before `perform`. Any other non-terminal
    /// phase resumes into observe without calling `perform`.
    pub fn execute<D: StepDriver>(
        &mut self,
        lease: &Lease,
        spec: &StepSpec,
        driver: &mut D,
        ctx: &ExecContext<'_>,
    ) -> Result<Phase> {
        self.check_lease(lease)?;
        let phase = self.phase(&lease.step);
        if phase.is_terminal() {
            return Ok(phase);
        }
        if phase == Phase::Pending {
            if ctx.cancel.is_requested() {
                self.write(lease, Event::Cancelled)?;
                return Ok(Phase::Cancelled);
            }
            self.write(lease, Event::Dispatched)?;
            let key = IdempotencyKey::new(&self.run_id, &lease.step);
            match driver.perform(spec, &key) {
                Ok(()) => self.write(lease, Event::Performed)?,
                Err(e) => self.write(lease, Event::PerformErrored { reason: e.0 })?,
            }
        }
        let key = IdempotencyKey::new(&self.run_id, &lease.step);
        self.observe_until(lease, spec, driver, ctx, &key)
    }

    /// Replays the prefix into a new take, retakes `scenario.steps[index]`, and
    /// compares its postcondition with the original take's value. A difference
    /// is flagged and downstream steps are reported as invalidated. Nothing is
    /// spliced into the original take.
    pub fn retake<D: StepDriver>(
        &mut self,
        scenario: &Scenario,
        index: usize,
        original_take: u32,
        driver: &mut D,
        ctx: &ExecContext<'_>,
    ) -> Result<RetakeReport> {
        scenario.validate()?;
        let spec = scenario.steps.get(index).ok_or(Error::Index {
            index,
            len: scenario.steps.len(),
        })?;
        let original = StepRef {
            step: spec.step_id.clone(),
            take: original_take,
        };
        let original_value = match (self.phase(&original), self.value(&original)) {
            (Phase::Completed, Some(v)) => v.clone(),
            _ => return Err(Error::NoOriginal { step: original }),
        };

        let take = self.next_take();
        for prior in &scenario.steps[..index] {
            let phase = self.run_one(prior, take, driver, ctx)?;
            if phase != Phase::Completed {
                return Ok(RetakeReport {
                    take,
                    phase,
                    matches: None,
                    invalidated: downstream(scenario, index),
                });
            }
        }

        let phase = self.run_one(spec, take, driver, ctx)?;
        let retaken = StepRef {
            step: spec.step_id.clone(),
            take,
        };
        if phase != Phase::Completed {
            return Ok(RetakeReport {
                take,
                phase,
                matches: None,
                invalidated: downstream(scenario, index),
            });
        }

        let matches = self.value(&retaken) == Some(&original_value);
        self.append(
            &retaken,
            0,
            Event::RetakeCompared {
                original_take,
                matches,
            },
        )?;
        let invalidated = if matches {
            Vec::new()
        } else {
            downstream(scenario, index)
        };
        Ok(RetakeReport {
            take,
            phase,
            matches: Some(matches),
            invalidated,
        })
    }

    fn run_one<D: StepDriver>(
        &mut self,
        spec: &StepSpec,
        take: u32,
        driver: &mut D,
        ctx: &ExecContext<'_>,
    ) -> Result<Phase> {
        let step = StepRef {
            step: spec.step_id.clone(),
            take,
        };
        let lease = self.acquire(&step, ctx.worker, ctx.lease_ttl_ms)?;
        self.execute(&lease, spec, driver, ctx)
    }

    fn observe_until<D: StepDriver>(
        &mut self,
        lease: &Lease,
        spec: &StepSpec,
        driver: &mut D,
        ctx: &ExecContext<'_>,
        key: &IdempotencyKey,
    ) -> Result<Phase> {
        let deadline = self.clock.now_ms().saturating_add(ctx.policy.timeout_ms);
        let mut note = String::from("postcondition not observed before deadline");
        loop {
            if ctx.cancel.is_requested() {
                self.write(lease, Event::Cancelled)?;
                return Ok(Phase::Cancelled);
            }
            match driver.observe(spec, key) {
                Ok(Observation::Satisfied(value)) => {
                    self.write(lease, Event::Completed { value })?;
                    return Ok(Phase::Completed);
                }
                Ok(Observation::Failed(reason)) => {
                    self.write(lease, Event::Failed { reason })?;
                    return Ok(Phase::Failed);
                }
                Ok(Observation::Pending) => {}
                Err(e) => note = format!("observe error: {e}"),
            }
            let now = self.clock.now_ms();
            if now >= deadline {
                self.write(lease, Event::Unresolved { reason: note })?;
                return Ok(Phase::Unresolved);
            }
            let mut wait = ctx.policy.interval_ms.min(deadline - now).max(1);
            if let Some(by) = ctx.cancel.deadline_ms() {
                wait = wait.min(by.saturating_sub(now).max(1));
            }
            self.clock.sleep_ms(wait);
        }
    }

    /// Rejects a write unless the lease is the current, unexpired grant.
    fn check_lease(&self, lease: &Lease) -> Result<()> {
        let grant = self.grants.get(&lease.step).ok_or_else(|| Error::Fenced {
            step: lease.step.clone(),
            presented: lease.fence,
            current: 0,
            reason: "step has no lease grant".into(),
        })?;
        if grant.fence != lease.fence || grant.worker != lease.worker {
            return Err(Error::Fenced {
                step: lease.step.clone(),
                presented: lease.fence,
                current: grant.fence,
                reason: format!("lease superseded by {}", grant.worker),
            });
        }
        if self.clock.now_ms() >= grant.expires_ms {
            return Err(Error::LeaseExpired {
                step: lease.step.clone(),
                fence: lease.fence,
            });
        }
        Ok(())
    }

    fn write(&mut self, lease: &Lease, event: Event) -> Result<()> {
        self.check_lease(lease)?;
        self.append(&lease.step, lease.fence, event)
    }

    fn append(&mut self, step: &StepRef, fence: u64, event: Event) -> Result<()> {
        let at_ms = self.clock.now_ms();
        let record = self
            .journal
            .append(&step.step, step.take, fence, at_ms, event)?;
        self.apply(&record);
        Ok(())
    }

    fn apply(&mut self, record: &Record) {
        let step = StepRef {
            step: record.step.clone(),
            take: record.take,
        };
        let state = self.steps.entry(step.clone()).or_default();
        match &record.event {
            Event::LeaseGranted { worker, expires_ms } => {
                self.grants.insert(
                    step,
                    Grant {
                        worker: worker.clone(),
                        fence: record.fence,
                        expires_ms: *expires_ms,
                    },
                );
            }
            Event::LeaseRenewed { expires_ms } => {
                if let Some(grant) = self.grants.get_mut(&step) {
                    grant.expires_ms = *expires_ms;
                }
            }
            Event::Dispatched => state.phase = Phase::Dispatched,
            Event::PerformErrored { reason } => state.note = Some(reason.clone()),
            Event::Performed => {
                if state.phase == Phase::Dispatched {
                    state.phase = Phase::Performed;
                }
            }
            Event::Completed { value } => {
                state.phase = Phase::Completed;
                state.value = Some(value.clone());
            }
            Event::Failed { reason } => {
                state.phase = Phase::Failed;
                state.note = Some(reason.clone());
            }
            Event::Unresolved { reason } => {
                state.phase = Phase::Unresolved;
                state.note = Some(reason.clone());
            }
            Event::Cancelled => state.phase = Phase::Cancelled,
            Event::RetakeCompared { .. } => {}
        }
    }
}

fn downstream(scenario: &Scenario, index: usize) -> Vec<String> {
    scenario.steps[index + 1..]
        .iter()
        .map(|s| s.step_id.clone())
        .collect()
}
