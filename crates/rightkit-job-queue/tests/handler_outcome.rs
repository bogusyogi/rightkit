//! `HandlerOutcome`: a handler that already settled its job (or handed it on) owns the outcome,
//! and the runner writes nothing more for it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_job_queue::rusqlite::Connection;
use rightkit_job_queue::{
    Event, EventSink, ExtColumn, HandlerOutcome, Job, JobControl, JobError, JobHandler, JobLocks,
    JobRecord, JobResult, JobStore, NewJob, PoolConfig, Recovery, RunOutcome, Runner, Staged,
    StagedHandler, StagedPool, StatusModel, StoreConfig, StoreHook, SubmitOptions, Transition,
    COMPLETE, FAILED, HANDLER_UNSETTLED, RUNNING,
};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(30);

fn new_job(mode: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "outcome".into(),
        request: json!({"mode": mode}),
    }
}

/// Counts every status change the store hook sees.
#[derive(Default)]
struct Moves {
    n: AtomicUsize,
}

impl StoreHook for Moves {
    fn on_transition(&self, _c: &Connection, _b: &JobRecord, _a: &JobRecord) -> JobResult<()> {
        self.n.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn held_config(moves: Arc<Moves>) -> StoreConfig {
    StoreConfig::new()
        .column(ExtColumn::text("provider_job_id"))
        .external_id("provider_job_id")
        .statuses(
            StatusModel::new("queued", ["queued", "running", "held"])
                .allow("queued", "running")
                .hold("held")
                .run_status(Some("running")),
        )
        .hook(moves)
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Arc<JobStore>,
    locks: JobLocks,
    moves: Arc<Moves>,
    events: Arc<Mutex<Vec<Event>>>,
    sink: EventSink,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let moves = Arc::new(Moves::default());
    let store = Arc::new(
        JobStore::open_configured(dir.path().join("jobs.db"), held_config(moves.clone())).unwrap(),
    );
    let locks = JobLocks::new(dir.path().join("locks"));
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let sink: EventSink = Arc::new(move |ev: &Event| seen.lock().unwrap().push(ev.clone()));
    Rig {
        _dir: dir,
        store,
        locks,
        moves,
        events,
        sink,
    }
}

impl Rig {
    fn runner(&self, handler: Arc<dyn JobHandler>) -> Runner {
        Runner::new(self.store.clone(), self.locks.clone(), handler).with_sink(self.sink.clone())
    }
    fn stages(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.stage.clone())
            .collect()
    }
}

/// A handler that settles its own jobs, or hands them on, as the request says.
struct Owns;

impl JobHandler for Owns {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Err(JobError::new("unused", "the runner calls run_outcome"))
    }

    fn run_outcome(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
        match job.request["mode"].as_str().unwrap_or("") {
            "complete" => {
                let t = Transition::to(COMPLETE)
                    .result(json!({"by": "handler"}))
                    .error(None);
                ctl.transition(&t)?;
                Ok(HandlerOutcome::Settled)
            }
            "fail" => {
                let t = Transition::to(FAILED).error(Some(
                    json!({"code": "mine", "message": "the handler failed it", "retryable": false}),
                ));
                ctl.transition(&t)?;
                Ok(HandlerOutcome::Settled)
            }
            "settle_then_error" => {
                let t = Transition::to(COMPLETE).result(json!({"by": "handler"}));
                ctl.transition(&t.error(None))?;
                Err(JobError::new("late", "an error after the job was settled"))
            }
            "lie" => Ok(HandlerOutcome::Settled),
            "pending" => Ok(HandlerOutcome::Pending(
                "waiting for the provider callback".into(),
            )),
            "hold" => {
                ctl.store().hold_submission_fenced(
                    &job.job_id,
                    ctl.generation(),
                    &JobError::new("outcome_unknown", "may have been sent"),
                    "held",
                )?;
                Ok(HandlerOutcome::Pending("held for an operator".into()))
            }
            "hold_then_settled" => {
                ctl.store().hold_submission_fenced(
                    &job.job_id,
                    ctl.generation(),
                    &JobError::new("outcome_unknown", "may have been sent"),
                    "held",
                )?;
                Ok(HandlerOutcome::Settled)
            }
            _ => Ok(HandlerOutcome::Done(json!({"by": "runner"}))),
        }
    }
}

fn submit(rig: &Rig, mode: &str) -> String {
    rig.store.submit(&new_job(mode)).unwrap().0.job_id
}

#[test]
fn a_handler_settled_job_gets_no_second_write() {
    let rig = rig();
    let id = submit(&rig, "complete");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, COMPLETE);
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.status, COMPLETE);
    assert_eq!(
        row.result,
        Some(json!({"by": "handler"})),
        "the handler's result stands; the runner wrote none"
    );
    assert!(row.error.is_none());

    // queued -> running (the runner) and running -> complete (the handler): nothing else moved.
    assert_eq!(rig.moves.n.load(Ordering::SeqCst), 2);

    // One lifecycle event for the terminal state, with its own reason; no FAILED, no second one.
    assert_eq!(rig.stages(), vec!["started", "complete"]);
    let stored = rig.store.events_after(&id, 0).unwrap();
    let stored: Vec<(&str, Option<&str>)> = stored
        .iter()
        .map(|e| (e.stage.as_str(), e.message.as_deref()))
        .collect();
    assert_eq!(
        stored,
        vec![("started", None), ("complete", Some("settled by handler"))]
    );
    // The claim is released.
    assert!(rig.locks.try_lock(&id).unwrap().is_some());
}

#[test]
fn a_handler_failure_is_not_overwritten_by_the_runner() {
    let rig = rig();
    let id = submit(&rig, "fail");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, FAILED);
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.error.as_ref().unwrap()["code"], "mine");
    assert_eq!(rig.moves.n.load(Ordering::SeqCst), 2);
    assert_eq!(rig.stages(), vec!["started", "failed"]);
}

#[test]
fn an_error_after_the_handler_settled_the_job_changes_nothing() {
    let rig = rig();
    let id = submit(&rig, "settle_then_error");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, COMPLETE);
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.result, Some(json!({"by": "handler"})));
    assert!(row.error.is_none(), "no FAILED over a settled job");
    assert_eq!(rig.moves.n.load(Ordering::SeqCst), 2);
}

#[test]
fn settled_but_not_terminal_is_a_typed_handler_bug() {
    let rig = rig();
    let id = submit(&rig, "lie");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, FAILED);
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.status, FAILED);
    assert_eq!(row.error.as_ref().unwrap()["code"], HANDLER_UNSETTLED);
    assert!(row.error.as_ref().unwrap()["message"]
        .as_str()
        .unwrap()
        .contains("still running"));
    assert_eq!(rig.stages(), vec!["started", "failed"]);
    // Nothing is left running with no owner.
    assert!(rig.store.non_terminal_ids().unwrap().is_empty());
}

#[test]
fn pending_writes_nothing_and_releases_the_claim() {
    let rig = rig();
    let id = submit(&rig, "pending");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Paused(job) = runner.run(&id).unwrap() else {
        panic!("expected Paused");
    };
    assert_eq!(
        job.status, RUNNING,
        "the row is exactly as the handler left it"
    );
    let row = rig.store.get(&id).unwrap();
    assert_eq!(row.status, RUNNING);
    assert!(row.error.is_none() && row.result.is_none());
    assert_eq!(
        rig.moves.n.load(Ordering::SeqCst),
        1,
        "only queued -> running"
    );
    let stored = rig.store.events_after(&id, 0).unwrap();
    let last = stored.last().unwrap();
    assert_eq!(last.stage, "paused");
    assert_eq!(
        last.message.as_deref(),
        Some("pending: waiting for the provider callback")
    );
    assert!(rig.locks.try_lock(&id).unwrap().is_some(), "claim released");
    // A job that is not held stays recoverable: a polling handler resumes through recovery.
    assert_eq!(runner.recoverable_ids().unwrap(), vec![id.clone()]);

    // The callback settles it later; that is the only terminal write.
    rig.store.complete(&id, &json!({"by": "callback"})).unwrap();
    assert_eq!(rig.store.get(&id).unwrap().status, COMPLETE);
}

#[test]
fn pending_in_a_held_status_waits_for_resolve() {
    let rig = rig();
    let id = submit(&rig, "hold");
    let runner = rig.runner(Arc::new(Owns));

    let RunOutcome::Paused(job) = runner.run(&id).unwrap() else {
        panic!("expected Paused");
    };
    assert_eq!(job.status, "held");
    assert_eq!(rig.store.get(&id).unwrap().status, "held");
    assert!(runner.recoverable_ids().unwrap().is_empty());
    assert!(matches!(
        runner.run(&id).unwrap(),
        RunOutcome::Paused(j) if j.status == "held"
    ));

    // A handler that says "settled" about a row it parked is understood as pending, not failed.
    let id2 = submit(&rig, "hold_then_settled");
    let RunOutcome::Paused(job) = runner.run(&id2).unwrap() else {
        panic!("expected Paused");
    };
    assert_eq!(job.status, "held");
}

#[test]
fn a_never_job_cannot_be_pending_outside_a_held_status() {
    let rig = rig();
    let id = rig
        .store
        .submit_with(
            &new_job("pending"),
            &Default::default(),
            &SubmitOptions::new().recovery(Recovery::Never),
        )
        .unwrap()
        .record
        .job
        .job_id;
    let runner = rig.runner(Arc::new(Owns));

    // Recovery would fail it as `interrupted` and lose the pending outcome: failed up front.
    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, FAILED);
    assert_eq!(
        rig.store.get(&id).unwrap().error.as_ref().unwrap()["code"],
        HANDLER_UNSETTLED
    );
}

/// Implements only `run`: nothing about it changes.
struct Plain;

impl JobHandler for Plain {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        match job.request["mode"].as_str() {
            Some("error") => Err(JobError::new("boom", "this one fails")),
            _ => Ok(json!({"by": "runner"})),
        }
    }
}

#[test]
fn existing_handlers_behave_as_before() {
    let rig = rig();
    let ok = submit(&rig, "ok");
    let bad = submit(&rig, "error");
    let runner = rig.runner(Arc::new(Plain));

    let RunOutcome::Finished(done) = runner.run(&ok).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(done.status, COMPLETE);
    assert_eq!(done.result, Some(json!({"by": "runner"})));
    let RunOutcome::Finished(failed) = runner.run(&bad).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(failed.status, FAILED);
    assert_eq!(failed.error.as_ref().unwrap()["code"], "boom");
    assert_eq!(
        rig.stages(),
        vec!["started", "complete", "started", "failed"]
    );
}

/// Settles in `prepare` or in `compute`, as the request says.
struct StagedOwns;

impl StagedHandler for StagedOwns {
    type Prepared = ();
    type Lane = ();

    fn new_lane(&self) {}

    fn prepare(&self, job: &Job, ctl: &JobControl) -> JobResult<Staged<()>> {
        if job.request["mode"] == "prepare" {
            ctl.transition(&Transition::to(COMPLETE).result(json!({"in": "prepare"})))?;
            Ok(Staged::Handled(HandlerOutcome::Settled))
        } else {
            Ok(Staged::Compute(()))
        }
    }

    fn compute(
        &self,
        _lane: &mut (),
        _job: &Job,
        _work: (),
        _ctl: &JobControl,
    ) -> JobResult<Value> {
        Err(JobError::new("unused", "the lane calls compute_outcome"))
    }

    fn compute_outcome(
        &self,
        _lane: &mut (),
        job: &Job,
        _work: (),
        ctl: &JobControl,
    ) -> JobResult<HandlerOutcome> {
        ctl.transition(&Transition::to(COMPLETE).result(json!({"in": "compute"})))?;
        assert_eq!(job.request["mode"], "compute");
        Ok(HandlerOutcome::Settled)
    }
}

#[test]
fn the_staged_pool_carries_handler_owned_outcomes() {
    let rig = rig();
    let (tx, rx) = mpsc::channel::<(String, JobResult<RunOutcome>)>();
    let tx = Mutex::new(tx);
    let pool = StagedPool::start(
        rig.store.clone(),
        rig.locks.clone(),
        Arc::new(StagedOwns),
        PoolConfig::new(|| 2).on_outcome(Arc::new(move |id: &str, res: JobResult<RunOutcome>| {
            let _ = tx.lock().unwrap().send((id.to_string(), res));
        })),
        |runner| runner,
    )
    .unwrap();

    let a = submit(&rig, "prepare");
    let b = submit(&rig, "compute");
    pool.enqueue(&a).unwrap();
    pool.enqueue(&b).unwrap();
    for _ in 0..2 {
        let (id, res) = rx.recv_timeout(WAIT).expect("an outcome");
        let RunOutcome::Finished(job) = res.unwrap() else {
            panic!("expected Finished for {id}");
        };
        assert_eq!(job.status, COMPLETE, "{id}");
    }
    pool.shutdown();
    assert_eq!(
        rig.store.get(&a).unwrap().result,
        Some(json!({"in": "prepare"}))
    );
    assert_eq!(
        rig.store.get(&b).unwrap().result,
        Some(json!({"in": "compute"}))
    );
    // Per job: queued -> running, running -> complete. No runner write on top.
    assert_eq!(rig.moves.n.load(Ordering::SeqCst), 4);
}
