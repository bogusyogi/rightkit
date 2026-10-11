//! Progress events may be written with relaxed durability and coalesced; transitions never are.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rightkit_jobs::{
    Job, JobControl, JobHandler, JobLocks, JobResult, JobStore, NewJob, ProgressPolicy, Runner,
    StoreConfig,
};
use serde_json::{json, Value};

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({}),
    }
}

fn open(dir: &std::path::Path, policy: ProgressPolicy) -> JobStore {
    JobStore::open_configured(dir.join("j.db"), StoreConfig::new().progress_policy(policy)).unwrap()
}

fn synchronous(store: &JobStore) -> i64 {
    store
        .read(|c| Ok(c.query_row("PRAGMA synchronous", [], |r| r.get(0))?))
        .unwrap()
}

fn started(store: &JobStore) -> (String, i64) {
    let id = store.submit(&new_job()).unwrap().0.job_id;
    let (_, gen) = store.begin_run_fenced(&id, true).unwrap().unwrap();
    (id, gen)
}

const FULL: i64 = 2;

#[test]
fn default_policy_writes_every_progress_event_fully_durable() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), ProgressPolicy::default());
    let (id, gen) = started(&store);
    for i in 0..10 {
        let ev = store
            .progress_fenced(&id, gen, "work", Some(i as f64 / 10.0), None)
            .unwrap();
        assert!(ev.is_some());
    }
    assert_eq!(store.events_after(&id, 0).unwrap().len(), 10);
    let stats = store.progress_stats();
    assert_eq!((stats.relaxed_writes, stats.coalesced), (0, 0));
    assert_eq!(synchronous(&store), FULL);
}

#[test]
fn coalescing_keeps_the_latest_progress_per_interval_and_flushes_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().coalesce(Duration::from_secs(60)),
    );
    let (id, gen) = started(&store);
    for i in 0..100 {
        store
            .progress_fenced(&id, gen, "work", Some(i as f64 / 100.0), None)
            .unwrap();
    }
    let evs = store.events_after(&id, 0).unwrap();
    assert_eq!(
        evs.len(),
        1,
        "only the first tick is written inside the interval"
    );
    assert_eq!(evs[0].progress, Some(0.0));
    assert_eq!(store.progress_stats().coalesced, 99);

    store.flush_progress(&id).unwrap();
    let evs = store.events_after(&id, 0).unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[1].progress, Some(0.99), "the latest tick wins");
}

#[test]
fn a_transition_flushes_pending_progress_before_it_commits() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().coalesce(Duration::from_secs(60)),
    );
    let (id, gen) = started(&store);
    for i in 0..5 {
        store
            .progress_fenced(&id, gen, "work", Some(i as f64 / 10.0), None)
            .unwrap();
    }
    store.complete_fenced(&id, gen, &json!({})).unwrap();
    let evs = store.events_after(&id, 0).unwrap();
    assert_eq!(evs.last().unwrap().progress, Some(0.4));
    assert_eq!(
        evs.last().unwrap().status,
        "running",
        "flushed before the status change"
    );
}

#[test]
fn due_progress_is_visible_to_a_polling_reader() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().coalesce(Duration::from_millis(50)),
    );
    let (id, gen) = started(&store);
    store
        .progress_fenced(&id, gen, "work", Some(0.1), None)
        .unwrap();
    store
        .progress_fenced(&id, gen, "work", Some(0.2), None)
        .unwrap();
    assert_eq!(store.events_after(&id, 0).unwrap().len(), 1);
    std::thread::sleep(Duration::from_millis(120));
    let evs = store.events_after(&id, 0).unwrap();
    assert_eq!(evs.last().unwrap().progress, Some(0.2));
}

#[test]
fn events_without_a_fraction_are_not_coalesced() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().coalesce(Duration::from_secs(60)),
    );
    let (id, gen) = started(&store);
    for i in 0..5 {
        store
            .progress_fenced(&id, gen, "stage", None, Some(&format!("m{i}")))
            .unwrap();
    }
    assert_eq!(store.events_after(&id, 0).unwrap().len(), 5);
}

#[test]
fn pending_progress_of_a_superseded_attempt_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().coalesce(Duration::from_secs(60)),
    );
    let (id, gen) = started(&store);
    store
        .progress_fenced(&id, gen, "work", Some(0.1), None)
        .unwrap();
    store
        .progress_fenced(&id, gen, "work", Some(0.9), None)
        .unwrap();
    store.fence(&id, gen).unwrap();
    store.flush_progress(&id).unwrap();
    assert_eq!(
        store.events_after(&id, 0).unwrap().len(),
        1,
        "stale tick never lands"
    );
}

#[test]
fn relaxed_durability_applies_to_progress_only_and_restores_full() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(
        dir.path(),
        ProgressPolicy::default().relaxed_durability(true),
    );
    let (id, gen) = started(&store);
    for i in 0..4 {
        store
            .progress_fenced(&id, gen, "work", Some(i as f64 / 4.0), None)
            .unwrap();
    }
    assert_eq!(store.events_after(&id, 0).unwrap().len(), 4);
    assert_eq!(store.progress_stats().relaxed_writes, 4);
    assert_eq!(
        synchronous(&store),
        FULL,
        "the connection is back to full durability"
    );
    let before = store.progress_stats().relaxed_writes;
    store.complete_fenced(&id, gen, &json!({})).unwrap();
    store.add_event(&id, "marker", None, None).unwrap();
    assert_eq!(
        store.progress_stats().relaxed_writes,
        before,
        "transitions and plain events are never relaxed"
    );
}

struct Ticker(AtomicUsize);

impl JobHandler for Ticker {
    fn run(&self, _job: &Job, ctl: &JobControl) -> JobResult<Value> {
        for i in 0..50 {
            ctl.progress("work", Some(i as f64 / 50.0), None);
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(json!({"ok": true}))
    }
}

#[test]
fn runner_lifecycle_events_stay_durable_under_coalescing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open(
        dir.path(),
        ProgressPolicy::default()
            .coalesce(Duration::from_secs(60))
            .relaxed_durability(true),
    ));
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Ticker(AtomicUsize::new(0))),
    );
    let id = store.submit(&new_job()).unwrap().0.job_id;
    runner.run(&id).unwrap();
    let stages: Vec<String> = store
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect();
    assert_eq!(stages.first().map(String::as_str), Some("started"));
    assert_eq!(stages.last().map(String::as_str), Some("complete"));
    let work = stages.iter().filter(|s| *s == "work").count();
    assert!(
        (1..=3).contains(&work),
        "coalesced to the first and latest tick, got {work}"
    );
    assert_eq!(synchronous(&store), FULL);
}
