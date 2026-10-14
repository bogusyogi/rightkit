//! A job's terminal event is announced once, from data the runner holds, and its row cannot be
//! pruned before that announcement has gone out. The case that broke an adopter: more active
//! rows than the cap, so the first completion was pruned the moment it landed and its terminal
//! notification was dropped.

use std::sync::{Arc, Mutex};

use rightkit_job_queue::{
    Event, EventSink, Job, JobControl, JobHandler, JobLocks, JobResult, JobStore, NewJob,
    PrunePolicy, RunOutcome, Runner, SubmitOptions, COMPLETE,
};
use serde_json::{json, Value};

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "announce".into(),
        request: json!({"tag": tag}),
    }
}

struct Done;

impl JobHandler for Done {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Ok(json!({"ok": true}))
    }
}

/// What the sink saw when a `complete` event reached it.
#[derive(Debug, Default)]
struct Seen {
    complete_events: usize,
    /// Status of the row, read from inside the sink.
    row_in_sink: Option<String>,
    /// Jobs a prune made from inside the sink removed.
    pruned_in_sink: Vec<String>,
}

fn rig(
    dir: &std::path::Path,
    prune_in_sink: Option<PrunePolicy>,
) -> (Arc<JobStore>, Runner, Arc<Mutex<Seen>>) {
    let store = Arc::new(JobStore::open(dir.join("jobs.db")).unwrap());
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (s, st) = (seen.clone(), store.clone());
    let sink: EventSink = Arc::new(move |ev: &Event| {
        if ev.stage != "complete" {
            return;
        }
        let mut seen = s.lock().unwrap();
        seen.complete_events += 1;
        seen.row_in_sink = st.get(&ev.job_id).ok().map(|j| j.status);
        if let Some(policy) = &prune_in_sink {
            seen.pruned_in_sink = st.prune(policy).unwrap().jobs;
        }
    });
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.join("locks")),
        Arc::new(Done),
    )
    .with_sink(sink);
    (store, runner, seen)
}

#[test]
fn the_first_completion_beside_more_active_jobs_than_the_cap_is_announced_once() {
    let dir = tempfile::tempdir().unwrap();
    // The cap is applied from inside the sink, as an adopter does when it trims after a job ends.
    let (store, runner, seen) = rig(dir.path(), Some(PrunePolicy::new().keep_total(200)));
    let ids: Vec<String> = (0..201)
        .map(|i| store.submit(&new_job(&format!("j{i}"))).unwrap().0.job_id)
        .collect();

    let RunOutcome::Finished(job) = runner.run(&ids[0]).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, COMPLETE);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.complete_events, 1, "announced exactly once");
    assert_eq!(
        seen.row_in_sink.as_deref(),
        Some(COMPLETE),
        "the row is readable by whoever receives the event"
    );
    assert!(seen.pruned_in_sink.is_empty(), "{:?}", seen.pruned_in_sink);
    // The row is still there afterwards, and the 200 active rows beside it were never touched.
    assert_eq!(store.get(&ids[0]).unwrap().status, COMPLETE);
    assert_eq!(store.non_terminal_ids().unwrap().len(), 200);
}

#[test]
fn a_terminal_row_is_not_pruned_before_its_event_has_gone_out() {
    let dir = tempfile::tempdir().unwrap();
    // The most aggressive bound there is: every terminal row goes.
    let (store, runner, seen) = rig(dir.path(), Some(PrunePolicy::new().keep_latest(0)));
    let id = store.submit(&new_job("only")).unwrap().0.job_id;

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("expected Finished");
    };
    assert_eq!(job.status, COMPLETE);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.complete_events, 1);
        assert_eq!(seen.row_in_sink.as_deref(), Some(COMPLETE));
        assert!(
            seen.pruned_in_sink.is_empty(),
            "a prune from inside the sink skipped the row being announced"
        );
    }
    assert_eq!(store.get(&id).unwrap().status, COMPLETE);

    // Once the run has ended the row is an ordinary finished row.
    let report = store.prune(&PrunePolicy::new().keep_latest(0)).unwrap();
    assert_eq!(report.jobs, vec![id.clone()]);
    assert_eq!(store.get(&id).unwrap_err().code, "job_not_found");
}

#[test]
fn a_submit_that_prunes_leaves_active_rows_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("jobs.db")).unwrap();
    for i in 0..201 {
        store.submit(&new_job(&format!("j{i}"))).unwrap();
    }
    let s = store
        .submit_with(
            &new_job("one more"),
            &Default::default(),
            &SubmitOptions::new().prune(PrunePolicy::new().keep_total(200)),
        )
        .unwrap();
    assert!(s.pruned.jobs.is_empty());
    assert_eq!(store.non_terminal_ids().unwrap().len(), 202);
}
