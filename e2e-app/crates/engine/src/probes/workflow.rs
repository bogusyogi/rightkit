//! rightkit-workflow: a durable scenario step on a real journal: acquired under a lease, performed exactly once even when its
//! postcondition is late (observed pending, then satisfied), completed with its value; reopening the journal recovers the value
//! without performing the step again.
use rightkit_workflow::{
    CancelToken, DriverError, ExecContext, IdempotencyKey, ManualClock, Observation, ObservePolicy, Phase, Run, StepDriver, StepRef, StepSpec,
};
use serde_json::{json, Value};

struct Driver {
    performs: u32,
    polls: u32,
}

impl StepDriver for Driver {
    fn perform(&mut self, _step: &StepSpec, _key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performs += 1;
        Ok(())
    }
    fn observe(&mut self, step: &StepSpec, _key: &IdempotencyKey) -> Result<Observation, DriverError> {
        self.polls += 1;
        Ok(if self.polls <= 2 { Observation::Pending } else { Observation::Satisfied(json!({ "step": step.step_id, "ok": true })) })
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let journal = ctx.dir("workflow")?.join("journal.jsonl");
    let clock = ManualClock::new(0);
    let cancel = CancelToken::new();
    let exec = ExecContext { worker: "w1", lease_ttl_ms: 60_000, policy: ObservePolicy { timeout_ms: 5_000, interval_ms: 10 }, cancel: &cancel };
    let spec = StepSpec { step_id: "s0".into(), label: "probe step".into(), action: Value::Null, expect: Value::Null };
    let at = StepRef { step: "s0".into(), take: 0 };
    let mut driver = Driver { performs: 0, polls: 0 };
    {
        let mut run = Run::open(&journal, "run-1", clock.clone()).map_err(|e| format!("open: {e}"))?;
        let lease = run.acquire(&at, "w1", 60_000).map_err(|e| format!("acquire: {e}"))?;
        let phase = run.execute(&lease, &spec, &mut driver, &exec).map_err(|e| format!("execute: {e}"))?;
        if phase != Phase::Completed || driver.performs != 1 {
            return Err(format!("phase {phase:?} after {} performs and {} polls", driver.performs, driver.polls));
        }
    }
    let reopened = Run::open(&journal, "run-1", clock).map_err(|e| format!("reopen: {e}"))?;
    let value = reopened.value(&at).cloned().ok_or("the completed step's value was not recovered from the journal")?;
    Ok(json!({ "performs": driver.performs, "polls": driver.polls, "recovered": value }))
}
