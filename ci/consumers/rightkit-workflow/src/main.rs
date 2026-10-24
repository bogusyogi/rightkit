// rightkit-workflow consumed by the exact registry pin in Cargo.toml, outside the workspace, with the one public
// feature the crate has ("sequence"; features only add API, so everything the default build offers
// is proven here too).
//
// Proves the durable step machine, with ManualClock time and no sockets:
//   * model: StepSpec and Scenario wire names, Scenario::validate, IdempotencyKey, StepRef ordering
//     and display, Phase::is_terminal, ManualClock, SystemClock, CancelToken (first deadline wins).
//   * Run: perform at most once per (run, step, take) with Dispatched durable first; observe polled
//     on the clock until satisfied, failed, cancelled or the deadline (Unresolved); a transient
//     observe error and a perform error both end in observe; leases carry fencing tokens (Held,
//     renew, expiry, a superseded lease is Fenced, a terminal step cannot be leased); a lease that
//     expires mid-step leaves the step Performed and a restart from the journal resumes into
//     observe without performing again; retake replays the prefix into a new take and flags a
//     differing postcondition and the downstream steps it invalidates, never splicing it.
//   * Journal: one JSON object per line with contiguous sequence numbers, a torn unacknowledged tail
//     truncated on open, a wrong run, a corrupt line and a sequence gap refused, Event wire shape.
//   * sequence feature: SequenceRun over a linear Sequence advances one step per call, reports Done
//     again without performing, resumes after a restart, stops at a Gate before anything is
//     journalled, reports Failed, Cancelled and Unresolved, and ReplayPolicy::ReissueOnce allows
//     exactly one further take.
//   * remote worker wire, over an in-memory byte pipe: framing limits, a sealed Channel (tamper and
//     replay refused), enrolment with a one-time code (burned on any attempt, label and TTL
//     checked), discovery, a step driven by RemoteDriver through Run::execute performed exactly
//     once, fencing and perform deduplication across connections, resumable bundle transfer with
//     digest and size checks, reconnect with the acknowledged position, WrongWorker and NotConnected.
//   * net: the loopback-only policy (is_loopback, check_addr, refusal in bind and connect) without
//     opening a socket.
// Public API only.
// Does not prove: real sockets (bind and connect to a loopback address, accept), Run against a GUI
// driver, a process kill between journal write and fsync, directory fsync on Unix, or the wire
// across a LAN (the crate refuses it unless the caller opts in).
// State: journal files under a per-process directory in the OS temp directory, removed at the end;
// worker threads that serve in-memory pipes and are joined. No network, no process spawn, no
// wall-clock waits other than one 5 ms sleep, no real user data.
use rightkit_workflow::clock::{Clock, ManualClock, SystemClock};
use rightkit_workflow::net::{self, Exposure, IO_TIMEOUT};
use rightkit_workflow::wire::{
    self, Channel, CONTROLLER_TO_WORKER, DEFAULT_CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_FRAME_BYTES,
    MAX_HANDSHAKE_BYTES, WORKER_TO_CONTROLLER,
};
use rightkit_workflow::worker::{
    BundleStore, DEFAULT_MAX_BUNDLE_BYTES, MAX_ENROLMENT_ATTEMPTS, MAX_PARTIAL_BUNDLES,
};
use rightkit_workflow::{
    BundleRef, CancelToken, Capability, Controller, ControllerLink, DriverError, EnrolmentRequest,
    Error, Event, ExecContext, Gate, GateDecision, IdempotencyKey, Journal, NoGate, Observation,
    ObservePolicy, Phase, ReconnectState, RemoteDriver, ReplayPolicy, Run, Scenario, Sequence,
    SequenceConfig, SequenceOutcome, SequenceRun, StepDriver, StepRef, StepSpec, StopReason,
    TransportError, WorkerEndpoint, WorkerIdentity, WorkerTransport,
};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{self, Cursor, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rk-workflow-consumer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }

    fn journal(&self, name: &str) -> PathBuf {
        self.0.join(format!("{name}.journal"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn spec(id: &str) -> StepSpec {
    StepSpec {
        step_id: id.to_owned(),
        label: format!("step {id}"),
        action: json!({ "click": id }),
        expect: json!({ "visible": id }),
    }
}

fn at(step: &str, take: u32) -> StepRef {
    StepRef {
        step: step.to_owned(),
        take,
    }
}

fn policy() -> ObservePolicy {
    ObservePolicy {
        timeout_ms: 1_000,
        interval_ms: 100,
    }
}

fn ctx<'a>(worker: &'a str, cancel: &'a CancelToken) -> ExecContext<'a> {
    ExecContext {
        worker,
        lease_ttl_ms: 60_000,
        policy: policy(),
        cancel,
    }
}

/// A scripted driver: it counts performs and observes, and plays back observations in order,
/// answering Pending when the script is empty.
#[derive(Default)]
struct Script {
    performs: u32,
    observes: u32,
    keys: Vec<String>,
    perform_error: Option<String>,
    plan: VecDeque<Result<Observation, DriverError>>,
    /// Runs on every observe, before the answer is chosen.
    hook: Option<Box<dyn FnMut()>>,
    /// The value reported for a step is {"id": step, "v": version}.
    version: Option<u32>,
    fail_step: Option<String>,
}

impl Script {
    fn answering(plan: Vec<Result<Observation, DriverError>>) -> Self {
        Script {
            plan: plan.into(),
            ..Script::default()
        }
    }

    fn versioned(version: u32) -> Self {
        Script {
            version: Some(version),
            ..Script::default()
        }
    }
}

impl StepDriver for Script {
    fn perform(&mut self, _step: &StepSpec, key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performs += 1;
        self.keys.push(key.as_str().to_owned());
        match &self.perform_error {
            Some(reason) => Err(DriverError(reason.clone())),
            None => Ok(()),
        }
    }

    fn observe(
        &mut self,
        step: &StepSpec,
        _key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        self.observes += 1;
        if let Some(hook) = self.hook.as_mut() {
            hook();
        }
        if self.fail_step.as_deref() == Some(step.step_id.as_str()) {
            return Ok(Observation::Failed("scripted failure".to_owned()));
        }
        if let Some(version) = self.version {
            return Ok(Observation::Satisfied(
                json!({ "id": step.step_id, "v": version }),
            ));
        }
        self.plan.pop_front().unwrap_or(Ok(Observation::Pending))
    }
}

fn run_step<D: StepDriver>(
    run: &mut Run<ManualClock>,
    spec: &StepSpec,
    take: u32,
    driver: &mut D,
) -> Phase {
    let step = at(&spec.step_id, take);
    let lease = run.acquire(&step, "w1", 60_000).expect("lease");
    let cancel = CancelToken::new();
    run.execute(&lease, spec, driver, &ctx("w1", &cancel))
        .expect("execute")
}

fn model() {
    let step = spec("s0");
    let json = serde_json::to_value(&step).unwrap();
    assert_eq!(json["stepId"], "s0");
    assert_eq!(json["label"], "step s0");
    assert_eq!(
        serde_json::from_value::<StepSpec>(json).unwrap(),
        step,
        "StepSpec round-trips under its camelCase names"
    );
    let scenario = Scenario {
        scenario_id: "sc-1".to_owned(),
        steps: vec![spec("s0"), spec("s1")],
    };
    let json = serde_json::to_value(&scenario).unwrap();
    assert_eq!(json["scenarioId"], "sc-1");
    assert_eq!(serde_json::from_value::<Scenario>(json).unwrap(), scenario);
    assert!(scenario.validate().is_ok());
    let mut empty_id = scenario.clone();
    empty_id.steps[1].step_id.clear();
    assert_eq!(
        empty_id.validate().unwrap_err().to_string(),
        "invalid: empty step_id"
    );
    let mut duplicate = scenario.clone();
    duplicate.steps[1] = spec("s0");
    assert!(matches!(duplicate.validate(), Err(Error::Invalid(m)) if m == "duplicate step_id s0"));

    let key = IdempotencyKey::new("run-1", &at("s0", 0));
    assert_eq!(key.as_str(), "run-1/s0/take-0");
    assert_eq!(key, IdempotencyKey::new("run-1", &at("s0", 0)));
    assert_ne!(key, IdempotencyKey::new("run-1", &at("s0", 1)));
    assert_ne!(key, IdempotencyKey::new("run-2", &at("s0", 0)));
    assert_eq!(at("s0", 3).to_string(), "s0#take-3");
    assert!(at("a", 9) < at("b", 0) && at("a", 0) < at("a", 1));

    for (phase, terminal) in [
        (Phase::Pending, false),
        (Phase::Dispatched, false),
        (Phase::Performed, false),
        (Phase::Completed, true),
        (Phase::Failed, true),
        (Phase::Unresolved, true),
        (Phase::Cancelled, true),
    ] {
        assert_eq!(phase.is_terminal(), terminal, "{phase:?}");
    }
    assert_eq!(Phase::default(), Phase::Pending);

    let clock = ManualClock::new(5);
    let shared = clock.clone();
    assert_eq!(clock.now_ms(), 5);
    clock.advance(10);
    assert_eq!(shared.now_ms(), 15, "clones share one timeline");
    shared.sleep_ms(5);
    assert_eq!(clock.now_ms(), 20, "sleeping advances a manual clock");
    assert_eq!(ManualClock::default().now_ms(), 0);
    let system = SystemClock;
    assert!(system.now_ms() > 0);
    system.sleep_ms(0);

    let token = CancelToken::new();
    let clone = token.clone();
    assert!(!token.is_requested() && token.deadline_ms().is_none());
    clone.request(500);
    clone.request(900);
    assert!(token.is_requested(), "clones share one request");
    assert_eq!(token.deadline_ms(), Some(500), "the first deadline wins");

    let ok = Observation::Satisfied(json!(1));
    assert_ne!(ok, Observation::Pending);
    assert_eq!(DriverError("x".to_owned()).to_string(), "x");
}

fn performing_and_observing(dir: &TempDir) {
    let clock = ManualClock::new(1_000);
    let mut run = Run::open(&dir.journal("observe"), "run-1", clock.clone()).unwrap();
    let step = spec("s0");

    // Pending twice, then satisfied: the action runs once and the value is recorded.
    let mut driver = Script::answering(vec![
        Ok(Observation::Pending),
        Ok(Observation::Pending),
        Ok(Observation::Satisfied(json!({ "panel": "open" }))),
    ]);
    assert_eq!(run.phase(&at("s0", 0)), Phase::Pending);
    assert_eq!(run_step(&mut run, &step, 0, &mut driver), Phase::Completed);
    assert_eq!(run.phase(&at("s0", 0)), Phase::Completed);
    assert_eq!(run.value(&at("s0", 0)), Some(&json!({ "panel": "open" })));
    assert_eq!((driver.performs, driver.observes), (1, 3));
    assert_eq!(driver.keys, ["run-1/s0/take-0"]);
    assert_eq!(
        clock.now_ms(),
        1_200,
        "two polls slept two intervals on the manual clock"
    );
    assert_eq!(run.next_take(), 1);

    // A definite failure is not retried.
    let mut failing = Script::answering(vec![Ok(Observation::Failed("wrong panel".to_owned()))]);
    assert_eq!(
        run_step(&mut run, &spec("s1"), 0, &mut failing),
        Phase::Failed
    );
    assert_eq!(run.note(&at("s1", 0)), Some("wrong panel"));
    assert_eq!(run.value(&at("s1", 0)), None);

    // Never observed before the deadline: Unresolved, never Completed.
    let before = clock.now_ms();
    let mut silent = Script::default();
    assert_eq!(
        run_step(&mut run, &spec("s2"), 0, &mut silent),
        Phase::Unresolved
    );
    assert_eq!(
        run.note(&at("s2", 0)),
        Some("postcondition not observed before deadline")
    );
    assert_eq!(
        clock.now_ms(),
        before + 1_000,
        "the loop ends at the deadline"
    );
    assert_eq!(silent.performs, 1);

    // A transient observe error is polled again, and its last message explains the Unresolved.
    let mut flaky = Script::answering(
        (0..30)
            .map(|_| Err(DriverError("flaky".to_owned())))
            .collect(),
    );
    assert_eq!(
        run_step(&mut run, &spec("s3"), 0, &mut flaky),
        Phase::Unresolved
    );
    assert_eq!(run.note(&at("s3", 0)), Some("observe error: flaky"));

    // A perform error leaves the outcome unknown, so the step still resumes into observe.
    let mut errored = Script::answering(vec![Ok(Observation::Satisfied(json!("seen")))]);
    errored.perform_error = Some("boom".to_owned());
    assert_eq!(
        run_step(&mut run, &spec("s4"), 0, &mut errored),
        Phase::Completed
    );
    assert_eq!((errored.performs, errored.observes), (1, 1));
    assert_eq!(run.note(&at("s4", 0)), Some("boom"));

    // A terminal step cannot be leased again, and executing it returns its phase.
    assert!(matches!(
        run.acquire(&at("s0", 0), "w1", 1),
        Err(Error::Terminal {
            phase: Phase::Completed,
            ..
        })
    ));
}

fn cancelling(dir: &TempDir) {
    let mut run = Run::open(&dir.journal("cancel"), "run-c", ManualClock::new(0)).unwrap();
    let cancel = CancelToken::new();
    cancel.request(5_000);
    let lease = run.acquire(&at("s0", 0), "w1", 60_000).unwrap();
    let mut never = Script::default();
    let phase = run
        .execute(&lease, &spec("s0"), &mut never, &ctx("w1", &cancel))
        .unwrap();
    assert_eq!(phase, Phase::Cancelled);
    assert_eq!(
        never.performs, 0,
        "a step cancelled before dispatch is never performed"
    );

    // Cancellation during observe stops at the next poll.
    let cancel = CancelToken::new();
    let lease = run.acquire(&at("s1", 0), "w1", 60_000).unwrap();
    let mut driver = Script::default();
    let token = cancel.clone();
    driver.hook = Some(Box::new(move || token.request(10)));
    let phase = run
        .execute(&lease, &spec("s1"), &mut driver, &ctx("w1", &cancel))
        .unwrap();
    assert_eq!(phase, Phase::Cancelled);
    assert_eq!((driver.performs, driver.observes), (1, 1));
}

fn leases_and_fencing(dir: &TempDir) {
    let clock = ManualClock::new(0);
    let mut run = Run::open(&dir.journal("lease"), "run-l", clock.clone()).unwrap();
    let step = at("s0", 0);
    let first = run.acquire(&step, "w1", 60_000).unwrap();
    assert_eq!(
        (first.fence, first.worker.as_str(), first.expires_ms),
        (1, "w1", 60_000)
    );
    assert_eq!(first.step, step);
    match run.acquire(&step, "w2", 60_000) {
        Err(Error::Held { step: held, worker }) => {
            assert_eq!((held, worker.as_str()), (step.clone(), "w1"));
        }
        other => panic!("a live lease cannot be taken: {other:?}"),
    }
    assert_eq!(
        Error::Held {
            step: step.clone(),
            worker: "w1".to_owned()
        }
        .to_string(),
        "s0#take-0 is leased by w1"
    );

    clock.advance(10_000);
    let renewed = run.renew(&first, 30_000).unwrap();
    assert_eq!((renewed.fence, renewed.expires_ms), (1, 40_000));

    // After expiry another worker is granted a higher fence, which fences the old holder.
    clock.advance(40_000);
    let second = run.acquire(&step, "w2", 60_000).unwrap();
    assert_eq!(second.fence, 2);
    let cancel = CancelToken::new();
    let mut driver = Script::default();
    match run.execute(&first, &spec("s0"), &mut driver, &ctx("w1", &cancel)) {
        Err(Error::Fenced {
            presented: 1,
            current: 2,
            ..
        }) => {}
        other => panic!("a superseded lease must be fenced: {other:?}"),
    }
    assert!(matches!(run.renew(&first, 1), Err(Error::Fenced { .. })));
    assert_eq!(driver.performs, 0, "a fenced write performs nothing");

    // An expired lease with no successor is refused too.
    clock.advance(100_000);
    assert!(matches!(
        run.execute(&second, &spec("s0"), &mut driver, &ctx("w2", &cancel)),
        Err(Error::LeaseExpired { fence: 2, .. })
    ));
    let never = forged_lease(&mut run);
    assert!(
        matches!(never, Err(Error::Fenced { current: 0, .. })),
        "{never:?}"
    );
}

fn forged_lease(run: &mut Run<ManualClock>) -> Result<Phase, Error> {
    // A lease value the machine never issued has no grant behind it.
    let forged = rightkit_workflow::Lease {
        step: at("ghost", 0),
        worker: "w9".to_owned(),
        fence: 7,
        expires_ms: u64::MAX,
    };
    let cancel = CancelToken::new();
    run.execute(
        &forged,
        &spec("ghost"),
        &mut Script::default(),
        &ctx("w9", &cancel),
    )
}

fn restart_after_dispatch(dir: &TempDir) {
    let path = dir.journal("restart");
    let clock = ManualClock::new(0);
    let mut run = Run::open(&path, "run-r", clock.clone()).unwrap();
    let step = spec("s0");

    // The lease expires while the postcondition is being observed: the write is refused and the
    // step stays Performed, because its outcome was never recorded.
    let lease = run.acquire(&at("s0", 0), "w1", 1_000).unwrap();
    let mut driver = Script::answering(vec![Ok(Observation::Satisfied(json!({ "ok": true })))]);
    let late = clock.clone();
    driver.hook = Some(Box::new(move || late.advance(5_000)));
    let cancel = CancelToken::new();
    assert!(matches!(
        run.execute(&lease, &step, &mut driver, &ctx("w1", &cancel)),
        Err(Error::LeaseExpired { .. })
    ));
    assert_eq!(run.phase(&at("s0", 0)), Phase::Performed);
    assert_eq!(driver.performs, 1);
    drop(run);

    // A new process opens the same journal: the dispatched step resumes into observe and is
    // not performed again.
    let mut run = Run::open(&path, "run-r", clock.clone()).unwrap();
    assert_eq!(
        run.phase(&at("s0", 0)),
        Phase::Performed,
        "state is a fold over the journal"
    );
    let mut fresh = Script::answering(vec![Ok(Observation::Satisfied(json!({ "ok": true })))]);
    let lease = run.acquire(&at("s0", 0), "w1", 60_000).unwrap();
    assert_eq!(lease.fence, 2, "a grant after expiry raises the fence");
    let phase = run
        .execute(&lease, &step, &mut fresh, &ctx("w1", &cancel))
        .unwrap();
    assert_eq!(phase, Phase::Completed);
    assert_eq!((fresh.performs, fresh.observes), (0, 1));
    assert_eq!(run.value(&at("s0", 0)), Some(&json!({ "ok": true })));
    drop(run);

    let (_journal, records) = Journal::open(&path, "run-r").unwrap();
    let kinds: Vec<&str> = records
        .iter()
        .map(|r| match r.event {
            Event::LeaseGranted { .. } => "lease_granted",
            Event::Dispatched => "dispatched",
            Event::Performed => "performed",
            Event::Completed { .. } => "completed",
            ref other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "lease_granted",
            "dispatched",
            "performed",
            "lease_granted",
            "completed"
        ]
    );
    assert_eq!(
        records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(
        records.iter().map(|r| r.fence).collect::<Vec<_>>(),
        [1, 1, 1, 2, 2]
    );
    assert!(records
        .iter()
        .all(|r| r.run == "run-r" && r.step == "s0" && r.take == 0));
}

fn retaking(dir: &TempDir) {
    let scenario = Scenario {
        scenario_id: "sc".to_owned(),
        steps: vec![spec("s0"), spec("s1"), spec("s2")],
    };
    let mut run = Run::open(&dir.journal("retake"), "run-t", ManualClock::new(0)).unwrap();
    let mut first = Script::versioned(1);
    for step in &scenario.steps {
        assert_eq!(run_step(&mut run, step, 0, &mut first), Phase::Completed);
    }
    let cancel = CancelToken::new();
    let context = ctx("w1", &cancel);

    // The same postcondition on retake matches and invalidates nothing.
    let mut same = Script::versioned(1);
    let report = run.retake(&scenario, 1, 0, &mut same, &context).unwrap();
    assert_eq!(report.take, 1);
    assert_eq!(report.phase, Phase::Completed);
    assert_eq!(report.matches, Some(true));
    assert!(report.invalidated.is_empty());
    assert_eq!(
        same.keys,
        ["run-t/s0/take-1", "run-t/s1/take-1"],
        "the prefix is replayed into the new take"
    );

    // A different postcondition is flagged, the later steps are invalidated, and the original
    // take is left exactly as it was.
    let mut changed = Script::versioned(2);
    let report = run.retake(&scenario, 1, 0, &mut changed, &context).unwrap();
    assert_eq!((report.take, report.matches), (2, Some(false)));
    assert_eq!(report.invalidated, ["s2"]);
    assert_eq!(
        run.value(&at("s1", 0)),
        Some(&json!({ "id": "s1", "v": 1 }))
    );
    assert_eq!(
        run.value(&at("s1", 2)),
        Some(&json!({ "id": "s1", "v": 2 }))
    );

    // A prefix step that fails in the new take stops the retake without a comparison.
    let mut broken = Script::versioned(3);
    broken.fail_step = Some("s0".to_owned());
    let report = run.retake(&scenario, 2, 0, &mut broken, &context).unwrap();
    assert_eq!((report.phase, report.matches), (Phase::Failed, None));
    assert_eq!(report.invalidated, Vec::<String>::new());

    // Refusals.
    assert!(matches!(
        run.retake(&scenario, 1, 7, &mut Script::versioned(1), &context),
        Err(Error::NoOriginal { .. })
    ));
    assert!(matches!(
        run.retake(&scenario, 9, 0, &mut Script::versioned(1), &context),
        Err(Error::Index { index: 9, len: 3 })
    ));
    let mut duplicate = scenario.clone();
    duplicate.steps[2] = spec("s0");
    assert!(matches!(
        run.retake(&duplicate, 1, 0, &mut Script::versioned(1), &context),
        Err(Error::Invalid(_))
    ));
}

fn journal(dir: &TempDir) {
    let path = dir.journal("raw");
    let (mut journal, records) = Journal::open(&path, "run-j").unwrap();
    assert!(records.is_empty());
    let first = journal.append("s0", 0, 1, 10, Event::Dispatched).unwrap();
    assert_eq!(
        (first.seq, first.run.as_str(), first.at_ms, first.fence),
        (1, "run-j", 10, 1)
    );
    journal
        .append(
            "s0",
            0,
            1,
            11,
            Event::Completed {
                value: json!({ "n": 1 }),
            },
        )
        .unwrap();
    drop(journal);

    // One JSON object per line, and the event wire shape is stable.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(text.lines().count(), 2);
    let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["event"], json!({ "kind": "dispatched" }));
    assert_eq!(
        (line["seq"].as_u64(), line["run"].as_str()),
        (Some(1), Some("run-j"))
    );
    assert_eq!(
        serde_json::to_value(Event::LeaseGranted {
            worker: "w1".to_owned(),
            expires_ms: 5
        })
        .unwrap(),
        json!({ "kind": "lease_granted", "worker": "w1", "expires_ms": 5 })
    );
    assert_eq!(
        serde_json::to_value(Event::RetakeCompared {
            original_take: 0,
            matches: true
        })
        .unwrap(),
        json!({ "kind": "retake_compared", "original_take": 0, "matches": true })
    );
    for event in [
        Event::LeaseRenewed { expires_ms: 9 },
        Event::PerformErrored {
            reason: "x".to_owned(),
        },
        Event::Performed,
        Event::Failed {
            reason: "x".to_owned(),
        },
        Event::Unresolved {
            reason: "x".to_owned(),
        },
        Event::Cancelled,
    ] {
        let again: Event = serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        assert_eq!(again, event);
    }

    // A torn tail (bytes after the last newline) was never acknowledged: it is truncated on open.
    let intact = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"seq\":3,\"ru")
        .unwrap();
    let (mut journal, records) = Journal::open(&path, "run-j").unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), intact);
    assert_eq!(
        journal
            .append("s0", 1, 0, 12, Event::Cancelled)
            .unwrap()
            .seq,
        3,
        "appends continue the sequence"
    );
    drop(journal);

    match Journal::open(&path, "someone-else") {
        Err(Error::WrongRun { expected, found }) => {
            assert_eq!(
                (expected.as_str(), found.as_str()),
                ("someone-else", "run-j")
            );
        }
        Err(other) => panic!("another run's journal must be refused: {other}"),
        Ok(_) => panic!("another run's journal must be refused"),
    }
    let garbage = dir.journal("garbage");
    std::fs::write(&garbage, b"not json\n").unwrap();
    assert!(matches!(
        Journal::open(&garbage, "run-j"),
        Err(Error::Corrupt { line: 1, .. })
    ));
    let gap = dir.journal("gap");
    std::fs::write(
        &gap,
        b"{\"seq\":5,\"run\":\"run-j\",\"at_ms\":0,\"step\":\"s\",\"take\":0,\"fence\":0,\"event\":{\"kind\":\"cancelled\"}}\n",
    )
    .unwrap();
    match Journal::open(&gap, "run-j") {
        Err(Error::Corrupt { line: 1, reason }) => {
            assert!(reason.contains("seq 5 where 1 expected"), "{reason}")
        }
        Err(other) => panic!("a sequence gap is corruption: {other}"),
        Ok(_) => panic!("a sequence gap is corruption"),
    }
    assert!(Error::Poisoned.to_string().contains("poisoned"));
    let as_error: &dyn std::error::Error = &Error::Index { index: 4, len: 2 };
    assert_eq!(as_error.to_string(), "step index 4 out of 2");
}

/// Completes every step with {"done": id}; Pending forever for steps named in `stuck`.
#[derive(Default)]
struct Steps {
    performed: Vec<String>,
    stuck: Vec<&'static str>,
    failing: Vec<&'static str>,
}

impl StepDriver for Steps {
    fn perform(&mut self, _step: &StepSpec, key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performed.push(key.as_str().to_owned());
        Ok(())
    }

    fn observe(
        &mut self,
        step: &StepSpec,
        _key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        if self.failing.contains(&step.step_id.as_str()) {
            return Ok(Observation::Failed("nope".to_owned()));
        }
        if self.stuck.contains(&step.step_id.as_str()) {
            return Ok(Observation::Pending);
        }
        Ok(Observation::Satisfied(json!({ "done": step.step_id })))
    }
}

struct StopAt {
    step: &'static str,
    keys: Vec<String>,
}

impl Gate for StopAt {
    fn before_perform(&mut self, step: &StepSpec, key: &IdempotencyKey) -> GateDecision {
        self.keys.push(key.as_str().to_owned());
        if step.step_id == self.step {
            GateDecision::Stop("budget exhausted".to_owned())
        } else {
            GateDecision::Proceed
        }
    }
}

fn sequence_config(replay: ReplayPolicy) -> SequenceConfig {
    SequenceConfig {
        worker: "seq-worker".to_owned(),
        lease_ttl_ms: 60_000,
        policy: ObservePolicy {
            timeout_ms: 200,
            interval_ms: 100,
        },
        replay,
    }
}

fn sequences(dir: &TempDir) {
    let defaults = SequenceConfig::default();
    assert_eq!(defaults.worker, "sequence");
    assert_eq!(defaults.lease_ttl_ms, 60_000);
    assert_eq!(
        defaults.policy,
        ObservePolicy {
            timeout_ms: 30_000,
            interval_ms: 100
        }
    );
    assert_eq!(defaults.replay, ReplayPolicy::Unresolved);
    assert_eq!(ReplayPolicy::default(), ReplayPolicy::Unresolved);
    assert!(
        matches!(Sequence::new(Vec::new()).validate(), Err(Error::Invalid(m)) if m == "empty sequence")
    );
    assert!(Sequence::new(vec![spec("a"), spec("a")])
        .validate()
        .is_err());
    assert!(Sequence::new(vec![spec("")]).validate().is_err());
    assert_eq!(Sequence::new(vec![spec("a")]).steps.len(), 1);

    // One step per call; Done is reported again without performing anything.
    let sequence = Sequence::new(vec![spec("a"), spec("b"), spec("c")]);
    let mut driver = Steps::default();
    let mut runner =
        SequenceRun::open(&dir.journal("seq-1"), "seq-1", ManualClock::new(0)).unwrap();
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        SequenceOutcome::Advanced {
            step: "a".to_owned(),
            value: json!({ "done": "a" })
        }
    );
    assert!(
        matches!(runner.run_next(&sequence, &mut driver, &mut NoGate).unwrap(), SequenceOutcome::Advanced { step, .. } if step == "b")
    );
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        SequenceOutcome::Done(json!({ "done": "c" }))
    );
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        SequenceOutcome::Done(json!({ "done": "c" }))
    );
    assert_eq!(
        driver.performed,
        ["seq-1/a/take-0", "seq-1/b/take-0", "seq-1/c/take-0"]
    );
    assert_eq!(runner.run().phase(&at("b", 0)), Phase::Completed);
    assert_eq!(
        runner.run().value(&at("a", 0)),
        Some(&json!({ "done": "a" }))
    );

    // A restart at a step boundary skips what is complete.
    let path = dir.journal("seq-2");
    let mut first = Steps::default();
    let mut runner = SequenceRun::open(&path, "seq-2", ManualClock::new(0)).unwrap();
    runner.run_next(&sequence, &mut first, &mut NoGate).unwrap();
    drop(runner);
    let mut second = Steps::default();
    let mut runner = SequenceRun::open(&path, "seq-2", ManualClock::new(0)).unwrap();
    assert!(
        matches!(runner.run_next(&sequence, &mut second, &mut NoGate).unwrap(), SequenceOutcome::Advanced { step, .. } if step == "b")
    );
    assert_eq!(second.performed, ["seq-2/b/take-0"]);

    // A gate refusal writes nothing for the step, so the same call can be made again.
    let mut runner =
        SequenceRun::open(&dir.journal("seq-3"), "seq-3", ManualClock::new(0)).unwrap();
    let mut driver = Steps::default();
    let mut gate = StopAt {
        step: "b",
        keys: Vec::new(),
    };
    runner.run_next(&sequence, &mut driver, &mut gate).unwrap();
    assert_eq!(
        runner.run_next(&sequence, &mut driver, &mut gate).unwrap(),
        SequenceOutcome::Stopped(StopReason::Gated {
            step: "b".to_owned(),
            reason: "budget exhausted".to_owned()
        })
    );
    assert_eq!(
        gate.keys,
        ["seq-3/a/take-0", "seq-3/b/take-0"],
        "the gate sees the key perform will receive"
    );
    assert_eq!(runner.run().phase(&at("b", 0)), Phase::Pending);
    assert_eq!(driver.performed.len(), 1);
    assert!(
        matches!(runner.run_next(&sequence, &mut driver, &mut NoGate).unwrap(), SequenceOutcome::Advanced { step, .. } if step == "b")
    );

    // A failed step stops the run, and stays stopped.
    let mut runner =
        SequenceRun::open(&dir.journal("seq-4"), "seq-4", ManualClock::new(0)).unwrap();
    let mut driver = Steps {
        failing: vec!["a"],
        ..Steps::default()
    };
    let stopped = SequenceOutcome::Stopped(StopReason::Failed {
        step: "a".to_owned(),
        reason: "nope".to_owned(),
    });
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        stopped
    );
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        stopped
    );
    assert_eq!(driver.performed.len(), 1);

    // Cancellation before dispatch performs nothing.
    let token = CancelToken::new();
    token.request(0);
    let mut runner = SequenceRun::open(&dir.journal("seq-5"), "seq-5", ManualClock::new(0))
        .unwrap()
        .with_cancel(token);
    let mut driver = Steps::default();
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        SequenceOutcome::Stopped(StopReason::Cancelled {
            step: "a".to_owned()
        })
    );
    assert!(driver.performed.is_empty());

    // An unresolved step is never silently performed again; ReissueOnce allows exactly one more take.
    let reason = "postcondition not observed before deadline".to_owned();
    let unresolved = SequenceOutcome::Unresolved {
        step: "a".to_owned(),
        reason,
    };
    let mut runner = SequenceRun::open(&dir.journal("seq-6"), "seq-6", ManualClock::new(0))
        .unwrap()
        .with_config(sequence_config(ReplayPolicy::Unresolved));
    let mut driver = Steps {
        stuck: vec!["a"],
        ..Steps::default()
    };
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        unresolved
    );
    assert_eq!(
        runner
            .run_next(&sequence, &mut driver, &mut NoGate)
            .unwrap(),
        unresolved
    );
    assert_eq!(driver.performed, ["seq-6/a/take-0"]);
    let mut runner = SequenceRun::open(&dir.journal("seq-7"), "seq-7", ManualClock::new(0))
        .unwrap()
        .with_config(sequence_config(ReplayPolicy::ReissueOnce));
    let mut driver = Steps {
        stuck: vec!["a"],
        ..Steps::default()
    };
    for _ in 0..4 {
        assert!(matches!(
            runner
                .run_next(&sequence, &mut driver, &mut NoGate)
                .unwrap(),
            SequenceOutcome::Unresolved { .. }
        ));
    }
    assert_eq!(
        driver.performed,
        ["seq-7/a/take-0", "seq-7/a/take-1"],
        "one reissue with a new key, then it stays unresolved"
    );

    let mut invalid =
        SequenceRun::open(&dir.journal("seq-8"), "seq-8", ManualClock::new(0)).unwrap();
    assert!(invalid
        .run_next(
            &Sequence::new(Vec::new()),
            &mut Steps::default(),
            &mut NoGate
        )
        .is_err());
}

fn wire_basics() {
    assert_eq!((CONTROLLER_TO_WORKER, WORKER_TO_CONTROLLER), (b'C', b'W'));
    assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    assert_eq!(MAX_HANDSHAKE_BYTES, 4 * 1024);
    assert_eq!(
        (DEFAULT_CHUNK_BYTES, MAX_CHUNK_BYTES),
        (64 * 1024, 256 * 1024)
    );
    assert_eq!((MAX_ENROLMENT_ATTEMPTS, MAX_PARTIAL_BUNDLES), (3, 16));
    assert_eq!(DEFAULT_MAX_BUNDLE_BYTES, 1024 * 1024 * 1024);

    assert_eq!(wire::to_hex(&[0, 1, 0xab, 0xff]), "0001abff");
    assert_eq!(wire::from_hex("0001abff"), Some(vec![0, 1, 0xab, 0xff]));
    for bad in ["abc", "zz", "é1"] {
        assert_eq!(wire::from_hex(bad), None, "{bad:?}");
    }
    assert!(wire::from_hex_32(&"11".repeat(32)).is_ok());
    assert!(wire::from_hex_32(&"11".repeat(31)).is_err());
    assert!(
        wire::ct_eq(b"same", b"same")
            && !wire::ct_eq(b"same", b"sama")
            && !wire::ct_eq(b"a", b"ab")
    );
    assert_ne!(
        wire::transcript(&[b"ab", b"c"]),
        wire::transcript(&[b"a", b"bc"]),
        "parts are length-prefixed"
    );
    assert_ne!(wire::random::<16>().unwrap(), wire::random::<16>().unwrap());

    // A frame is a 4-byte big-endian length and the body; the length is checked before allocation.
    let mut bytes = Vec::new();
    wire::write_frame(&mut bytes, b"hello").unwrap();
    assert_eq!(bytes, [0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o']);
    assert_eq!(
        wire::read_frame(&mut Cursor::new(bytes.clone())).unwrap(),
        b"hello"
    );
    assert_eq!(
        wire::read_frame_limited(&mut Cursor::new(bytes.clone()), 4)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    let mut truncated = bytes;
    truncated.pop();
    assert_eq!(
        wire::read_frame(&mut Cursor::new(truncated))
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
    let announced = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
    assert_eq!(
        wire::read_frame(&mut Cursor::new(announced))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(wire::write_frame(&mut Vec::new(), &vec![0; MAX_FRAME_BYTES + 1]).is_err());
    let mut plain = Vec::new();
    wire::write_json(&mut plain, &json!({ "kind": "hello" })).unwrap();
    assert_eq!(
        wire::read_json::<_, Value>(&mut Cursor::new(plain)).unwrap(),
        json!({ "kind": "hello" })
    );
    assert!(matches!(
        wire::write_json(&mut Vec::new(), &json!("x".repeat(MAX_HANDSHAKE_BYTES))),
        Err(TransportError::Protocol(_))
    ));

    // A sealed channel authenticates every frame: tamper, replay and reordering are refused.
    let root = [7u8; 32];
    let mut sender = Channel::new(
        Tape::reading(Vec::new()),
        root,
        CONTROLLER_TO_WORKER,
        WORKER_TO_CONTROLLER,
    );
    sender.send(&json!({ "n": 1 })).unwrap();
    sender.send(&json!({ "n": 2 })).unwrap();
    let sealed = sender.into_stream().output;
    let receiver = |stream: Vec<u8>, key: [u8; 32]| {
        Channel::new(
            Tape::reading(stream),
            key,
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        )
    };
    let mut good = receiver(sealed.clone(), root);
    assert_eq!(good.recv::<Value>().unwrap(), json!({ "n": 1 }));
    assert_eq!(good.recv::<Value>().unwrap(), json!({ "n": 2 }));
    assert!(
        matches!(good.recv::<Value>(), Err(TransportError::Io(_))),
        "end of stream"
    );
    assert!(
        matches!(good.recv::<Value>(), Err(TransportError::NotConnected)),
        "a failed receive poisons the channel"
    );
    assert!(
        matches!(
            receiver(sealed.clone(), [8u8; 32]).recv::<Value>(),
            Err(TransportError::Auth(_))
        ),
        "wrong key"
    );
    let mut flipped = sealed.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let mut tampered = receiver(flipped, root);
    assert_eq!(tampered.recv::<Value>().unwrap(), json!({ "n": 1 }));
    assert!(
        matches!(tampered.recv::<Value>(), Err(TransportError::Auth(_))),
        "tampered body"
    );
    let first_len = 4 + u32::from_be_bytes(sealed[..4].try_into().unwrap()) as usize;
    let mut replayed = sealed[..first_len].to_vec();
    replayed.extend_from_slice(&sealed[..first_len]);
    let mut channel = receiver(replayed, root);
    channel.recv::<Value>().unwrap();
    assert!(
        matches!(channel.recv::<Value>(), Err(TransportError::Protocol(m)) if m.contains("replayed"))
    );
    let mut reordered = sealed[first_len..].to_vec();
    reordered.extend_from_slice(&sealed[..first_len]);
    assert!(
        matches!(receiver(reordered, root).recv::<Value>(), Err(TransportError::Protocol(m)) if m.contains("out of order"))
    );
    let mut reflected = Channel::new(
        Tape::reading(sealed),
        root,
        CONTROLLER_TO_WORKER,
        WORKER_TO_CONTROLLER,
    );
    assert!(
        matches!(reflected.recv::<Value>(), Err(TransportError::Auth(_))),
        "a frame cannot be reflected at its sender"
    );

    for (error, text) in [
        (TransportError::NotConnected, "worker link is not connected"),
        (
            TransportError::Fenced { current: 3 },
            "request fenced (current fence 3)",
        ),
        (
            TransportError::Refused("no".to_owned()),
            "worker refused: no",
        ),
        (
            TransportError::Digest("bad".to_owned()),
            "bundle rejected: bad",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
}

/// A byte stream that reads from a recording and keeps what is written to it.
struct Tape {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Tape {
    fn reading(bytes: Vec<u8>) -> Self {
        Tape {
            input: Cursor::new(bytes),
            output: Vec::new(),
        }
    }
}

impl Read for Tape {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Tape {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn net_policy() {
    assert_eq!(Exposure::default(), Exposure::LoopbackOnly);
    assert_eq!(IO_TIMEOUT, Duration::from_secs(30));
    for text in [
        "127.0.0.1:1",
        "127.9.9.9:1",
        "[::1]:1",
        "[::ffff:127.0.0.1]:1",
    ] {
        let addr: SocketAddr = text.parse().unwrap();
        assert!(net::is_loopback(&addr.ip()), "{text}");
        assert!(
            net::check_addr(&addr, Exposure::LoopbackOnly).is_ok(),
            "{text}"
        );
    }
    for text in [
        "192.168.1.20:7000",
        "[2001:db8::1]:7000",
        "0.0.0.0:7000",
        "[::ffff:10.0.0.1]:7000",
    ] {
        let addr: SocketAddr = text.parse().unwrap();
        assert!(!net::is_loopback(&addr.ip()), "{text}");
        let refused = net::check_addr(&addr, Exposure::LoopbackOnly).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{text}");
        assert!(refused.to_string().contains("not loopback"), "{refused}");
        assert!(
            net::check_addr(&addr, Exposure::UnencryptedLan).is_ok(),
            "{text}"
        );
        // Both refuse before any socket is opened.
        assert_eq!(
            net::bind(addr, Exposure::LoopbackOnly).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            net::connect(addr, Exposure::LoopbackOnly)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}

/// One end of an in-memory duplex byte pipe. Dropping an end closes both directions.
struct End {
    read: Arc<Half>,
    write: Arc<Half>,
}

#[derive(Default)]
struct Half {
    state: Mutex<(VecDeque<u8>, bool)>,
    ready: Condvar,
}

fn duplex() -> (End, End) {
    let a = Arc::new(Half::default());
    let b = Arc::new(Half::default());
    (
        End {
            read: Arc::clone(&a),
            write: Arc::clone(&b),
        },
        End { read: b, write: a },
    )
}

impl Read for End {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut state = self.read.state.lock().unwrap();
        loop {
            if !state.0.is_empty() {
                let n = out.len().min(state.0.len());
                for slot in &mut out[..n] {
                    *slot = state.0.pop_front().unwrap();
                }
                return Ok(n);
            }
            if state.1 {
                return Ok(0);
            }
            state = self.read.ready.wait(state).unwrap();
        }
    }
}

impl Write for End {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut state = self.write.state.lock().unwrap();
        if state.1 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        state.0.extend(data.iter().copied());
        self.write.ready.notify_all();
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for End {
    fn drop(&mut self) {
        for half in [&self.read, &self.write] {
            half.state.lock().unwrap().1 = true;
            half.ready.notify_all();
        }
    }
}

/// Counts performs and reports satisfied once a perform has happened.
#[derive(Clone, Default)]
struct Gui {
    performs: Arc<AtomicU32>,
}

impl StepDriver for Gui {
    fn perform(&mut self, _step: &StepSpec, _key: &IdempotencyKey) -> Result<(), DriverError> {
        self.performs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn observe(
        &mut self,
        _step: &StepSpec,
        _key: &IdempotencyKey,
    ) -> Result<Observation, DriverError> {
        if self.performs.load(Ordering::SeqCst) > 0 {
            Ok(Observation::Satisfied(json!({ "panel": "open" })))
        } else {
            Ok(Observation::Pending)
        }
    }
}

type Served = Arc<Mutex<Vec<JoinHandle<Result<(), TransportError>>>>>;
type Link = ControllerLink<End, Box<dyn FnMut() -> io::Result<End>>>;

/// A worker endpoint served over in-memory pipes: every connection the controller opens is served
/// on its own thread, one at a time, by the same endpoint.
struct Worker {
    served: Served,
    bundles: BundleStore,
    gui: Gui,
    endpoint: Arc<Mutex<WorkerEndpoint<Gui>>>,
}

fn capabilities() -> Vec<Capability> {
    vec![Capability {
        name: "gui-click".to_owned(),
        version: 1,
    }]
}

impl Worker {
    fn new(code: &str, max_bundle_bytes: Option<u64>) -> Self {
        let gui = Gui::default();
        let mut endpoint =
            WorkerEndpoint::awaiting_enrolment("gui-worker", code, gui.clone(), capabilities())
                .expect("worker endpoint");
        if let Some(max) = max_bundle_bytes {
            endpoint = endpoint.with_max_bundle_bytes(max);
        }
        assert_eq!(endpoint.worker_id(), None, "not enrolled yet");
        Worker {
            served: Arc::default(),
            bundles: endpoint.bundles(),
            gui,
            endpoint: Arc::new(Mutex::new(endpoint)),
        }
    }

    fn connector(&self) -> Box<dyn FnMut() -> io::Result<End>> {
        let served = Arc::clone(&self.served);
        let endpoint = Arc::clone(&self.endpoint);
        Box::new(move || {
            let (controller_end, worker_end) = duplex();
            let endpoint = Arc::clone(&endpoint);
            served.lock().unwrap().push(thread::spawn(move || {
                endpoint.lock().unwrap().serve_connection(worker_end)
            }));
            Ok(controller_end)
        })
    }

    /// Joins every serving thread. Call after the link is dropped, so each pipe is closed.
    fn finish(&self) -> Vec<Result<(), TransportError>> {
        let handles: Vec<_> = self.served.lock().unwrap().drain(..).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("serving thread panicked"))
            .collect()
    }
}

fn request(issued: &rightkit_workflow::IssuedEnrolment) -> EnrolmentRequest {
    EnrolmentRequest {
        worker_label: "gui-worker".to_owned(),
        enrolment_id: issued.enrolment_id.clone(),
    }
}

fn enrolled(controller: Controller, worker: &Worker) -> Link {
    ControllerLink::new(controller, worker.connector())
}

fn remote_worker(dir: &TempDir) {
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").expect("issue");
    assert!(issued.enrolment_id.starts_with("e-"));
    assert!(issued
        .code
        .starts_with(&format!("{}:", issued.enrolment_id)));
    let worker = Worker::new(&issued.code, None);
    let mut link = enrolled(controller, &worker).with_chunk_bytes(1_000);
    assert!(!link.is_connected());
    assert_eq!(link.acked_seq(), 0);

    // Enrolment with the one-time code, then discovery on a session.
    let identity = link.enrol(&request(&issued)).expect("enrol");
    assert!(identity.worker_id.starts_with("w-"));
    assert_eq!(identity.key_fingerprint.len(), 16);
    assert!(link.controller().is_enrolled(&identity.worker_id));
    assert!(!link.controller().is_enrolled("w-nobody"));
    assert_eq!(link.discover(&identity).expect("discover"), capabilities());
    assert!(link.is_connected());
    assert_eq!(link.acked_seq(), 1);
    assert!(
        matches!(
            link.enrol(&request(&issued)),
            Err(TransportError::Auth(m)) if m.contains("already used")
        ),
        "a one-time grant is spent"
    );

    // A step through Run::execute, performed exactly once on the worker.
    let mut run = Run::open(&dir.journal("remote"), "run-remote", ManualClock::new(0)).unwrap();
    let step = at("open", 0);
    let lease = run.acquire(&step, "controller", 60_000).unwrap();
    link.grant(&step, lease.fence).expect("grant");
    let cancel = CancelToken::new();
    let mut driver = RemoteDriver::new(&mut link, step.clone(), lease.fence);
    let phase = run
        .execute(
            &lease,
            &spec("open"),
            &mut driver,
            &ctx("controller", &cancel),
        )
        .unwrap();
    assert_eq!(phase, Phase::Completed);
    assert_eq!(run.value(&step), Some(&json!({ "panel": "open" })));
    assert_eq!(worker.gui.performs.load(Ordering::SeqCst), 1);

    // The worker deduplicates a perform by idempotency key, so a resend never repeats the action.
    let key = IdempotencyKey::new("run-remote", &step);
    let mut again = RemoteDriver::new(&mut link, step.clone(), lease.fence);
    again
        .perform(&spec("open"), &key)
        .expect("a repeated perform answers from the record");
    assert_eq!(worker.gui.performs.load(Ordering::SeqCst), 1);
    assert_eq!(
        again.observe(&spec("open"), &key).unwrap(),
        Observation::Satisfied(json!({ "panel": "open" }))
    );

    // Fences are a high-water mark: a lower grant and a stale lease are both refused.
    link.grant(&step, lease.fence + 1).expect("a higher grant");
    assert!(
        matches!(link.grant(&step, lease.fence), Err(TransportError::Fenced { current }) if current == lease.fence + 1)
    );
    assert!(matches!(
        link.grant(&step, 0),
        Err(TransportError::Fenced { .. })
    ));
    let mut stale = RemoteDriver::new(&mut link, step.clone(), lease.fence);
    let refused = stale.perform(
        &spec("open"),
        &IdempotencyKey::new("run-remote", &at("open", 1)),
    );
    assert!(
        matches!(refused, Err(DriverError(ref m)) if m.contains("fenced")),
        "{refused:?}"
    );
    let mut ungranted = RemoteDriver::new(&mut link, at("never-granted", 0), 1);
    assert!(ungranted.observe(&spec("never-granted"), &key).is_err());

    // Reconnect returns the acknowledged position; a worker that lost acknowledged work is an error.
    let before = link.acked_seq();
    let state = link
        .reconnect(
            &identity,
            ReconnectState {
                last_acked_seq: before,
            },
        )
        .expect("reconnect");
    assert!(state.last_acked_seq >= before);
    assert!(matches!(
        link.reconnect(&identity, ReconnectState { last_acked_seq: 1_000_000 }),
        Err(TransportError::Protocol(m)) if m.contains("acknowledged")
    ));
    let other = WorkerIdentity {
        worker_id: "w-other".to_owned(),
        key_fingerprint: identity.key_fingerprint.clone(),
    };
    assert!(matches!(
        link.discover(&other),
        Err(TransportError::WrongWorker { expected, found }) if expected == identity.worker_id && found == "w-other"
    ));

    // A bundle moves in resumable chunks and is kept only after its whole digest verifies.
    let payload: Vec<u8> = (0..=255u8).cycle().take(5_000).collect();
    let bundle = BundleRef {
        bundle_id: "bundle-1".to_owned(),
        blake3: blake3::hash(&payload).to_hex().to_string(),
        bytes: payload.len() as u64,
    };
    let report = link
        .transfer_bundle(&identity, &bundle, &payload)
        .expect("transfer");
    assert_eq!((report.resumed_from, report.bytes), (0, 5_000));
    assert_eq!(
        worker.bundles.lock().unwrap().get("bundle-1"),
        Some(&payload)
    );
    let wrong_length = BundleRef {
        bytes: 4_999,
        ..bundle.clone()
    };
    assert!(
        matches!(link.transfer_bundle(&identity, &wrong_length, &payload), Err(TransportError::Digest(m)) if m.contains("length"))
    );
    let wrong_digest = BundleRef {
        blake3: blake3::hash(b"other").to_hex().to_string(),
        ..bundle.clone()
    };
    assert!(
        matches!(link.transfer_bundle(&identity, &wrong_digest, &payload), Err(TransportError::Digest(m)) if m.contains("digest"))
    );
    let other_bytes = vec![9u8; 5_000];
    let impostor = BundleRef {
        bundle_id: "bundle-1".to_owned(),
        blake3: blake3::hash(&other_bytes).to_hex().to_string(),
        bytes: 5_000,
    };
    assert!(
        matches!(
            link.transfer_bundle(&identity, &impostor, &other_bytes),
            Err(TransportError::Digest(_))
        ),
        "a different bundle cannot reuse a verified id"
    );
    assert_eq!(
        worker.bundles.lock().unwrap().get("bundle-1"),
        Some(&payload)
    );

    drop(link);
    let results = worker.finish();
    assert!(
        results.len() >= 3,
        "enrolment, the session, and the reconnects each dialled the worker"
    );
    assert_eq!(
        worker.endpoint.lock().unwrap().worker_id(),
        Some(identity.worker_id.as_str()),
        "the worker holds the identity the controller issued"
    );
    assert!(
        results.iter().all(|r| r.is_ok()),
        "every connection ends cleanly: {results:?}"
    );
}

fn enrolment_refusals() {
    // A worker holding a different code: the controller rejects the proof and the grant is burned.
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").unwrap();
    let wrong_code = format!("{}:{}", issued.enrolment_id, "11".repeat(32));
    let worker = Worker::new(&wrong_code, None);
    let mut link = enrolled(controller, &worker);
    assert!(
        matches!(link.enrol(&request(&issued)), Err(TransportError::Auth(m)) if m.contains("rejected"))
    );
    assert!(
        matches!(link.enrol(&request(&issued)), Err(TransportError::Auth(m)) if m.contains("already used"))
    );
    assert!(!link.is_connected());
    drop(link);
    worker.finish();

    // The grant names its worker label, and an unknown grant is refused before any connection.
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").unwrap();
    let worker = Worker::new(&issued.code, None);
    let mut link = enrolled(controller, &worker);
    let renamed = EnrolmentRequest {
        worker_label: "someone-else".to_owned(),
        ..request(&issued)
    };
    assert!(matches!(link.enrol(&renamed), Err(TransportError::Auth(m)) if m.contains("label")));
    let unknown = EnrolmentRequest {
        enrolment_id: "e-unknown".to_owned(),
        ..request(&issued)
    };
    assert!(matches!(link.enrol(&unknown), Err(TransportError::Auth(m)) if m.contains("unknown")));
    assert!(
        worker.served.lock().unwrap().is_empty(),
        "nothing was dialled"
    );
    drop(link);
    worker.finish();

    // An expired grant is refused.
    let mut controller = Controller::new().with_enrolment_ttl(Duration::ZERO);
    let issued = controller.issue_enrolment("gui-worker").unwrap();
    let worker = Worker::new(&issued.code, None);
    let mut link = enrolled(controller, &worker);
    thread::sleep(Duration::from_millis(5));
    assert!(
        matches!(link.enrol(&request(&issued)), Err(TransportError::Auth(m)) if m.contains("expired"))
    );
    drop(link);
    worker.finish();

    // A link that never enrolled is not connected, and a short code is refused by the worker.
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").unwrap();
    let worker = Worker::new(&issued.code, None);
    let mut link = enrolled(controller, &worker);
    let stranger = WorkerIdentity {
        worker_id: "w-x".to_owned(),
        key_fingerprint: "0".repeat(16),
    };
    assert!(matches!(
        link.discover(&stranger),
        Err(TransportError::NotConnected)
    ));
    assert!(matches!(
        link.transfer_bundle(
            &stranger,
            &BundleRef {
                bundle_id: "b".to_owned(),
                blake3: String::new(),
                bytes: 0
            },
            b""
        ),
        Err(TransportError::NotConnected)
    ));
    assert!(matches!(
        link.reconnect(&stranger, ReconnectState { last_acked_seq: 0 }),
        Err(TransportError::NotConnected)
    ));
    assert!(matches!(
        WorkerEndpoint::awaiting_enrolment("w", "e-1:abcd", Gui::default(), Vec::new()),
        Err(TransportError::Protocol(m)) if m.contains("too short")
    ));
    assert!(matches!(
        WorkerEndpoint::awaiting_enrolment("w", "no-colon", Gui::default(), Vec::new()),
        Err(TransportError::Protocol(_))
    ));
    drop(link);
    worker.finish();

    // A worker caps the declared size of a bundle before keeping any byte of it.
    let mut controller = Controller::new();
    let issued = controller.issue_enrolment("gui-worker").unwrap();
    let worker = Worker::new(&issued.code, Some(1_024));
    let mut link = enrolled(controller, &worker);
    let identity = link.enrol(&request(&issued)).unwrap();
    let payload = vec![1u8; 2_048];
    let bundle = BundleRef {
        bundle_id: "big".to_owned(),
        blake3: blake3::hash(&payload).to_hex().to_string(),
        bytes: 2_048,
    };
    assert!(matches!(
        link.transfer_bundle(&identity, &bundle, &payload),
        Err(TransportError::Digest(_))
    ));
    assert!(worker.bundles.lock().unwrap().is_empty());
    let small = vec![2u8; 512];
    let accepted = BundleRef {
        bundle_id: "small".to_owned(),
        blake3: blake3::hash(&small).to_hex().to_string(),
        bytes: 512,
    };
    assert_eq!(
        link.transfer_bundle(&identity, &accepted, &small)
            .unwrap()
            .bytes,
        512
    );
    drop(link);
    worker.finish();
}

fn main() {
    // A guard against a hung worker thread: the recipe never legitimately runs for this long.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(50));
        eprintln!("rightkit-workflow consumer exceeded its 50 second budget");
        std::process::exit(2);
    });
    let dir = TempDir::new();
    model();
    performing_and_observing(&dir);
    cancelling(&dir);
    leases_and_fencing(&dir);
    restart_after_dispatch(&dir);
    retaking(&dir);
    journal(&dir);
    sequences(&dir);
    wire_basics();
    net_policy();
    remote_worker(&dir);
    enrolment_refusals();
    println!("OK rightkit-workflow (registry consumer)");
}
