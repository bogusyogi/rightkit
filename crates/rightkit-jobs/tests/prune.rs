//! Caller-invoked pruning of finished jobs and an optional bound applied at submit time.

use std::time::Duration;

use rightkit_jobs::{
    ExtColumn, JobStore, NewJob, PrunePolicy, StoreConfig, SubmitOptions, COMPLETE, FAILED,
};
use serde_json::json;

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({"tag": tag}),
    }
}

fn open(dir: &std::path::Path) -> JobStore {
    JobStore::open_configured(dir.join("j.db"), StoreConfig::new()).unwrap()
}

/// A finished job with an event and an artifact file on disk.
fn finished(store: &JobStore, dir: &std::path::Path, tag: &str) -> String {
    let id = store.submit(&new_job(tag)).unwrap().0.job_id;
    store.add_event(&id, "work", Some(0.5), None).unwrap();
    let file = dir.join(format!("{tag}.bin"));
    std::fs::write(&file, tag).unwrap();
    store.add_artifact(&id, &file, "text/plain").unwrap();
    store.complete(&id, &json!({"tag": tag})).unwrap();
    id
}

fn set_updated(store: &JobStore, id: &str, at: &str) {
    store
        .write(|c| {
            c.execute(
                "UPDATE jobs SET updated_at=?2, created_at=?2 WHERE job_id=?1",
                [id, at],
            )?;
            Ok(())
        })
        .unwrap();
}

fn count(store: &JobStore, table: &str) -> i64 {
    store
        .read(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn a_policy_without_a_limit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    finished(&store, dir.path(), "a");
    let e = store.prune(&PrunePolicy::new()).unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert_eq!(count(&store, "jobs"), 1);
}

#[test]
fn older_than_removes_terminal_jobs_with_events_and_artifact_rows_but_not_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let old = finished(&store, dir.path(), "old");
    let fresh = finished(&store, dir.path(), "fresh");
    let running = store.submit(&new_job("running")).unwrap().0.job_id;
    set_updated(&store, &old, "2020-01-01T00:00:00Z");
    set_updated(&store, &running, "2020-01-01T00:00:00Z");

    let report = store
        .prune(&PrunePolicy::new().older_than(Duration::from_secs(3600)))
        .unwrap();
    assert_eq!(report.jobs, vec![old.clone()]);
    assert_eq!(report.events, 1);
    assert_eq!(report.artifacts.len(), 1);
    let path = std::path::Path::new(&report.artifacts[0].path);
    assert!(path.exists(), "the caller owns artifact files; they are returned, not deleted");
    assert!(store.get(&old).is_err());
    assert!(store.get(&fresh).is_ok());
    assert!(
        !store.get(&running).unwrap().is_terminal(),
        "a non-terminal job is never pruned, however old"
    );
    assert_eq!(count(&store, "job_events"), 1);
    assert_eq!(count(&store, "artifacts"), 1);
}

#[test]
fn keep_latest_keeps_the_newest_terminal_jobs_and_every_live_job() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let ids: Vec<String> = (0..5)
        .map(|i| {
            let id = finished(&store, dir.path(), &format!("j{i}"));
            set_updated(&store, &id, &format!("2024-01-0{}T00:00:00Z", i + 1));
            id
        })
        .collect();
    let live = store.submit(&new_job("live")).unwrap().0.job_id;
    set_updated(&store, &live, "2000-01-01T00:00:00Z");

    let report = store.prune(&PrunePolicy::new().keep_latest(2)).unwrap();
    let mut gone = report.jobs.clone();
    gone.sort();
    let mut want = vec![ids[0].clone(), ids[1].clone(), ids[2].clone()];
    want.sort();
    assert_eq!(gone, want);
    assert!(store.get(&ids[3]).is_ok() && store.get(&ids[4]).is_ok());
    assert!(store.get(&live).is_ok());
    assert_eq!(report.artifacts.len(), 3);
}

#[test]
fn both_limits_apply_independently() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let a = finished(&store, dir.path(), "a");
    let b = finished(&store, dir.path(), "b");
    let c = finished(&store, dir.path(), "c");
    set_updated(&store, &a, "2020-01-01T00:00:00Z");
    set_updated(&store, &b, "2999-01-02T00:00:00Z");
    set_updated(&store, &c, "2999-01-03T00:00:00Z");
    // a is too old; b is beyond the newest one even though it is young.
    let report = store
        .prune(
            &PrunePolicy::new()
                .older_than(Duration::from_secs(3600))
                .keep_latest(1),
        )
        .unwrap();
    let mut gone = report.jobs;
    gone.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(gone, want);
    assert!(store.get(&c).is_ok());
}

#[test]
fn failed_and_cancelled_jobs_are_terminal_too() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let f = store.submit(&new_job("f")).unwrap().0.job_id;
    store
        .fail(&f, &rightkit_jobs::JobError::new("boom", "x"))
        .unwrap();
    let c = store.submit(&new_job("c")).unwrap().0.job_id;
    store.cancel(&c).unwrap();
    let report = store.prune(&PrunePolicy::new().keep_latest(0)).unwrap();
    assert_eq!(report.jobs.len(), 2);
    assert_eq!(store.get(&f).unwrap_err().code, "job_not_found");
    let _ = (COMPLETE, FAILED);
}

#[test]
fn prune_is_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let a = finished(&store, dir.path(), "a");
    let b = finished(&store, dir.path(), "b");
    // Deleting b's job row (the last step) fails after every event and artifact row already went.
    store
        .write(|c| {
            c.execute_batch(&format!(
                "CREATE TRIGGER block_b BEFORE DELETE ON jobs WHEN OLD.job_id='{b}' \
                 BEGIN SELECT RAISE(ABORT,'blocked'); END;"
            ))?;
            Ok(())
        })
        .unwrap();
    assert!(store.prune(&PrunePolicy::new().keep_latest(0)).is_err());
    assert!(store.get(&a).is_ok() && store.get(&b).is_ok());
    assert_eq!(count(&store, "jobs"), 2);
    assert_eq!(count(&store, "job_events"), 2);
    assert_eq!(count(&store, "artifacts"), 2);
}

#[test]
fn submit_time_bound_prunes_in_the_submit_transaction_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let a = finished(&store, dir.path(), "a");
    let b = finished(&store, dir.path(), "b");
    let c = finished(&store, dir.path(), "c");
    set_updated(&store, &a, "2024-01-01T00:00:00Z");
    set_updated(&store, &b, "2024-01-02T00:00:00Z");
    set_updated(&store, &c, "2024-01-03T00:00:00Z");
    let s = store
        .submit_with(
            &new_job("new"),
            &Default::default(),
            &SubmitOptions::new().prune(PrunePolicy::new().keep_latest(1)),
        )
        .unwrap();
    let mut gone = s.pruned.jobs.clone();
    gone.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(gone, want);
    assert_eq!(s.pruned.artifacts.len(), 2);
    assert!(store.get(&c).is_ok());
    assert!(!store.get(&s.record.job.job_id).unwrap().is_terminal());
}

#[test]
fn prune_removes_submission_rows_and_extension_data_with_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = StoreConfig::new()
        .column(ExtColumn::text("provider_job_id"))
        .external_id("provider_job_id");
    let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
    let id = store.submit(&new_job("s")).unwrap().0.job_id;
    store.begin_submission(&id, 3).unwrap();
    store.complete(&id, &json!({})).unwrap();
    let report = store.prune(&PrunePolicy::new().keep_latest(0)).unwrap();
    assert_eq!(report.submissions, 1);
    assert_eq!(count(&store, "rk_job_submissions"), 0);
}
