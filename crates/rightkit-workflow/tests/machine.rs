use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use rightkit_workflow::{
    CancelToken, DriverError, Error, ExecContext, IdempotencyKey, ManualClock, Observation,
    ObservePolicy, Phase, Run, Scenario, StepDriver, StepRef, StepSpec,
};
use serde_json::{json, Value};

const RUN: &str = "run-1";
const TTL: u64 = 60_000;

/// Removes its directory on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("rk-scenario-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.jsonl")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn steady(step: &str, _key: &str) -> Value {
    json!({ "step": step, "frame": "original" })
}

fn drift(step: &str, key: &str) -> Value {
    // Any take other than the original shows a different frame for s1.
    if step == "s1" && !key.contains("take-0") {
        json!({ "step": step, "frame": "shifted" })
    } else {
        json!({ "step": step, "frame": "original" })
    }
}

/// Scripted driver. Counts performs and observes per key.
struct FakeDriver {
    performs: Vec<String>,
    polls: HashMap<String, u32>,
    pending_polls: u32,
    never_satisfied: bool,
    panic_on_perform: bool,
    cancel_on_poll: Option<(u32, CancelToken)>,
    value_for: fn(&str, &str) -> Value,
}

impl FakeDriver {
    fn new() -> Self {
        Self {
            performs: Vec::new(),
            polls: HashMap::new(),
            pending_polls: 0,
            never_satisfied: false,
            panic_on_perform: false,
            cancel_on_poll: None,
            value_for: steady,
        }
    }
}

impl StepDriver for FakeDriver {
    fn perform(&mut self, _step: &StepSpec, key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performs.push(key.as_str().to_string());
        if self.panic_on_perform {
            panic!("worker died after dispatch");
        }
        Ok(())
    }

    fn observe(
        &mut self,
        step: &StepSpec,
        key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        let count = self.polls.entry(key.as_str().to_string()).or_insert(0);
        *count += 1;
        let n = *count;
        if let Some((at, token)) = &self.cancel_on_poll {
            if n == *at {
                token.request(u64::MAX);
            }
        }
        if self.never_satisfied || n <= self.pending_polls {
            return Ok(Observation::Pending);
        }
        Ok(Observation::Satisfied((self.value_for)(
            &step.step_id,
            key.as_str(),
        )))
    }
}

fn spec(id: &str) -> StepSpec {
    StepSpec {
        step_id: id.into(),
        label: format!("step {id}"),
        action: Value::Null,
        expect: Value::Null,
    }
}

fn scenario() -> Scenario {
    Scenario {
        scenario_id: "demo".into(),
        steps: vec![spec("s0"), spec("s1"), spec("s2")],
    }
}

fn policy() -> ObservePolicy {
    ObservePolicy {
        timeout_ms: 5_000,
        interval_ms: 100,
    }
}

fn step(id: &str, take: u32) -> StepRef {
    StepRef {
        step: id.into(),
        take,
    }
}

fn key(id: &str, take: u32) -> String {
    format!("{RUN}/{id}/take-{take}")
}

#[test]
fn delayed_postcondition_does_not_repeat_perform() {
    let dir = TempDir::new("delayed");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };

    let lease = run.acquire(&step("s0", 0), "w1", TTL).unwrap();
    let mut driver = FakeDriver::new();
    driver.pending_polls = 5;
    let phase = run.execute(&lease, &spec("s0"), &mut driver, &ctx).unwrap();

    assert_eq!(phase, Phase::Completed);
    assert_eq!(driver.performs, vec![key("s0", 0)]);
    assert_eq!(driver.polls[&key("s0", 0)], 6);
    assert_eq!(run.value(&step("s0", 0)), Some(&steady("s0", "")));
}

#[test]
fn restart_after_dispatch_resumes_into_observe() {
    let dir = TempDir::new("restart");
    let clock = ManualClock::new(0);
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };

    // Worker dies inside perform, after Dispatched is durable.
    {
        let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
        let lease = run.acquire(&step("s0", 0), "w1", TTL).unwrap();
        let mut driver = FakeDriver::new();
        driver.panic_on_perform = true;
        let crashed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run.execute(&lease, &spec("s0"), &mut driver, &ctx)
        }));
        assert!(crashed.is_err());
        assert_eq!(driver.performs.len(), 1);
    }

    // Lease expires; a restarted worker takes over.
    clock.advance(TTL + 1);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    assert_eq!(run.phase(&step("s0", 0)), Phase::Dispatched);
    let lease = run.acquire(&step("s0", 0), "w2", TTL).unwrap();
    let mut driver = FakeDriver::new();
    let phase = run.execute(&lease, &spec("s0"), &mut driver, &ctx).unwrap();

    assert_eq!(phase, Phase::Completed);
    assert!(
        driver.performs.is_empty(),
        "restart must not repeat perform"
    );
    assert_eq!(driver.polls[&key("s0", 0)], 1);
}

#[test]
fn restart_after_dispatch_without_observation_is_unresolved_not_completed() {
    let dir = TempDir::new("unresolved");
    let clock = ManualClock::new(0);
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: ObservePolicy {
            timeout_ms: 500,
            interval_ms: 100,
        },
        cancel: &cancel,
    };
    {
        let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
        let lease = run.acquire(&step("s0", 0), "w1", TTL).unwrap();
        let mut driver = FakeDriver::new();
        driver.panic_on_perform = true;
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run.execute(&lease, &spec("s0"), &mut driver, &ctx)
        }));
    }
    clock.advance(TTL + 1);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let lease = run.acquire(&step("s0", 0), "w2", TTL).unwrap();
    let mut driver = FakeDriver::new();
    driver.never_satisfied = true;
    let phase = run.execute(&lease, &spec("s0"), &mut driver, &ctx).unwrap();

    assert_eq!(phase, Phase::Unresolved);
    assert!(driver.performs.is_empty());
    assert_eq!(run.value(&step("s0", 0)), None);
}

#[test]
fn lease_expiry_and_stale_worker_write_is_fenced() {
    let dir = TempDir::new("fence");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "a",
        lease_ttl_ms: 1_000,
        policy: policy(),
        cancel: &cancel,
    };

    let lease_a = run.acquire(&step("s0", 0), "a", 1_000).unwrap();
    assert_eq!(lease_a.fence, 1);
    clock.advance(1_500);

    // Expired lease cannot write, so the perform never runs for worker A.
    let mut driver_a = FakeDriver::new();
    let err = run
        .execute(&lease_a, &spec("s0"), &mut driver_a, &ctx)
        .unwrap_err();
    assert!(matches!(err, Error::LeaseExpired { .. }), "{err}");
    assert!(driver_a.performs.is_empty());

    // Worker B takes over with a higher fence.
    let lease_b = run.acquire(&step("s0", 0), "b", TTL).unwrap();
    assert_eq!(lease_b.fence, 2);

    // Stale worker A is fenced on every write path.
    let err = run.renew(&lease_a, 1_000).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Fenced {
                presented: 1,
                current: 2,
                ..
            }
        ),
        "{err}"
    );
    let err = run
        .execute(&lease_a, &spec("s0"), &mut driver_a, &ctx)
        .unwrap_err();
    assert!(matches!(err, Error::Fenced { .. }), "{err}");
    assert!(driver_a.performs.is_empty());

    // B completes once, and its result is the only one recorded.
    let mut driver_b = FakeDriver::new();
    let phase = run
        .execute(&lease_b, &spec("s0"), &mut driver_b, &ctx)
        .unwrap();
    assert_eq!(phase, Phase::Completed);
    assert_eq!(driver_b.performs, vec![key("s0", 0)]);
}

#[test]
fn cancellation_mid_observe_is_cancelled_not_completed() {
    let dir = TempDir::new("cancel");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };

    let lease = run.acquire(&step("s0", 0), "w1", TTL).unwrap();
    let mut driver = FakeDriver::new();
    driver.pending_polls = 100;
    driver.cancel_on_poll = Some((2, cancel.clone()));
    let phase = run.execute(&lease, &spec("s0"), &mut driver, &ctx).unwrap();

    assert_eq!(phase, Phase::Cancelled);
    assert_eq!(driver.performs.len(), 1);
    assert_eq!(driver.polls[&key("s0", 0)], 2);
    assert_eq!(run.value(&step("s0", 0)), None);

    // The terminal state survives restart.
    drop(run);
    let run = Run::open(&dir.journal(), RUN, clock).unwrap();
    assert_eq!(run.phase(&step("s0", 0)), Phase::Cancelled);
}

#[test]
fn retake_replays_prefix_and_matches_postcondition() {
    let dir = TempDir::new("retake-match");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };
    let scenario = scenario();

    let mut original = FakeDriver::new();
    for s in &scenario.steps {
        let lease = run.acquire(&step(&s.step_id, 0), "w1", TTL).unwrap();
        run.execute(&lease, s, &mut original, &ctx).unwrap();
    }

    let mut retake = FakeDriver::new();
    let report = run.retake(&scenario, 1, 0, &mut retake, &ctx).unwrap();

    assert_eq!(report.take, 1);
    assert_eq!(report.phase, Phase::Completed);
    assert_eq!(report.matches, Some(true));
    assert!(report.invalidated.is_empty());
    // Prefix s0 and retaken s1 are replayed; s2 is not run.
    assert_eq!(retake.performs, vec![key("s0", 1), key("s1", 1)]);
}

#[test]
fn retake_with_differing_postcondition_is_flagged_not_spliced() {
    let dir = TempDir::new("retake-drift");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };
    let scenario = scenario();

    let mut original = FakeDriver::new();
    for s in &scenario.steps {
        let lease = run.acquire(&step(&s.step_id, 0), "w1", TTL).unwrap();
        run.execute(&lease, s, &mut original, &ctx).unwrap();
    }
    let original_value = run.value(&step("s1", 0)).cloned();

    let mut retake = FakeDriver::new();
    retake.value_for = drift;
    let report = run.retake(&scenario, 1, 0, &mut retake, &ctx).unwrap();

    assert_eq!(report.matches, Some(false));
    assert_eq!(report.invalidated, vec!["s2".to_string()]);
    // The original take is untouched.
    assert_eq!(run.value(&step("s1", 0)).cloned(), original_value);
    assert_ne!(run.value(&step("s1", 1)), original_value.as_ref());
}

#[test]
fn retake_needs_completed_original() {
    let dir = TempDir::new("retake-none");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal(), RUN, clock).unwrap();
    let cancel = CancelToken::new();
    let ctx = ExecContext {
        worker: "w1",
        lease_ttl_ms: TTL,
        policy: policy(),
        cancel: &cancel,
    };
    let mut driver = FakeDriver::new();
    let err = run
        .retake(&scenario(), 1, 0, &mut driver, &ctx)
        .unwrap_err();
    assert!(matches!(err, Error::NoOriginal { .. }), "{err}");
    assert!(driver.performs.is_empty());
}

#[test]
fn torn_final_record_is_discarded_on_reopen() {
    let dir = TempDir::new("torn");
    let clock = ManualClock::new(0);
    {
        let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
        let cancel = CancelToken::new();
        let ctx = ExecContext {
            worker: "w1",
            lease_ttl_ms: TTL,
            policy: policy(),
            cancel: &cancel,
        };
        let lease = run.acquire(&step("s0", 0), "w1", TTL).unwrap();
        let mut driver = FakeDriver::new();
        run.execute(&lease, &spec("s0"), &mut driver, &ctx).unwrap();
    }
    // Simulate a crash mid-append: a partial line with no newline.
    let mut file = OpenOptions::new().append(true).open(dir.journal()).unwrap();
    file.write_all(br#"{"seq":99,"run":"run-1","at_ms":0,"ste"#)
        .unwrap();
    drop(file);

    let run = Run::open(&dir.journal(), RUN, clock).unwrap();
    assert_eq!(run.phase(&step("s0", 0)), Phase::Completed);
    assert_eq!(run.next_take(), 1);
}

#[test]
fn journal_belongs_to_one_run() {
    let dir = TempDir::new("wrong-run");
    let clock = ManualClock::new(0);
    {
        let mut run = Run::open(&dir.journal(), RUN, clock.clone()).unwrap();
        run.acquire(&step("s0", 0), "w1", TTL).unwrap();
    }
    let err = Run::open(&dir.journal(), "other", clock)
        .err()
        .expect("wrong run must be refused");
    assert!(matches!(err, Error::WrongRun { .. }), "{err}");
}

#[test]
fn scenario_rejects_duplicate_step_ids() {
    let bad = Scenario {
        scenario_id: "demo".into(),
        steps: vec![spec("s0"), spec("s0")],
    };
    assert!(bad.validate().is_err());
}
