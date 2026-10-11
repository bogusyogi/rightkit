//! Resume without all-or-nothing, the claimed-path sweep, held jobs, and the cancel/late-error race.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rightkit_jobs::{
    ExtColumn, Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob,
    RunOutcome, Runner, StatusModel, StoreConfig, CANCELLED, FAILED,
};
use serde_json::{json, Value};

fn new_job(kind: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: kind.into(),
        request: json!({}),
    }
}

fn rig(dir: &std::path::Path) -> (Arc<JobStore>, JobLocks) {
    (
        Arc::new(JobStore::open(dir.join("jobs.db")).unwrap()),
        JobLocks::new(dir.join("locks")),
    )
}

fn submit(store: &JobStore, kind: &str) -> String {
    store.submit(&new_job(kind)).unwrap().0.job_id
}

/// Fails the job kinds named `bad`, completes the rest; counts every call.
struct Picky {
    calls: AtomicUsize,
}

impl JobHandler for Picky {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if job.kind == "bad" {
            Err(JobError::new("boom", "this one fails"))
        } else {
            Ok(json!({"ok": true}))
        }
    }
}

#[test]
fn one_failing_job_does_not_stop_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let ids = [
        submit(&store, "good"),
        submit(&store, "bad"),
        submit(&store, "good"),
    ];
    let handler = Arc::new(Picky {
        calls: AtomicUsize::new(0),
    });
    let runner = Runner::new(store.clone(), locks, handler.clone());

    let results = runner.resume_recoverable_each().unwrap();
    assert_eq!(handler.calls.load(Ordering::SeqCst), 3);
    let got: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(got, ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (id, r) in &results {
        let outcome = r.as_ref().unwrap();
        let job = outcome.job().unwrap();
        assert_eq!(&job.job_id, id);
        let want = if id == &ids[1] { FAILED } else { "complete" };
        assert_eq!(job.status, want);
    }
    assert!(runner.recoverable_ids().unwrap().is_empty());
}

#[test]
fn claimed_dir_survives_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, "good");
    let root = dir.path().join("scratch");
    let kept = root.join("kept");
    let stray = root.join("stray");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(kept.join("out.bin"), b"claimed").unwrap();
    std::fs::write(stray.join("junk.bin"), b"junk").unwrap();
    std::fs::write(root.join("loose.txt"), b"not a directory").unwrap();
    store
        .add_artifact(&id, &kept.join("out.bin"), "application/octet-stream")
        .unwrap();
    let runner = Runner::new(
        store.clone(),
        locks,
        Arc::new(Picky {
            calls: AtomicUsize::new(0),
        }),
    );

    let removed = runner.sweep_unclaimed(&root).unwrap();
    assert_eq!(removed, vec![stray.clone()]);
    assert!(kept.join("out.bin").exists());
    assert!(!stray.exists());
    assert!(root.join("loose.txt").exists());

    // A missing root is not an error and removes nothing.
    assert!(runner
        .sweep_unclaimed(&dir.path().join("absent"))
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn sweep_never_follows_a_symlink_out_of_root() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious.txt"), b"keep").unwrap();
    let root = dir.path().join("scratch");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    let runner = Runner::new(
        store,
        locks,
        Arc::new(Picky {
            calls: AtomicUsize::new(0),
        }),
    );

    let removed = runner.sweep_unclaimed(&root).unwrap();
    assert!(removed.is_empty());
    assert!(outside.join("precious.txt").exists());
    assert!(root.join("link").exists());
}

fn held_config() -> StoreConfig {
    StoreConfig::new()
        .column(ExtColumn::text("provider_job_id"))
        .external_id("provider_job_id")
        .statuses(
            StatusModel::new("queued", ["queued", "running", "held"])
                .allow("queued", "running")
                .hold("held")
                .run_status(Some("running")),
        )
}

#[test]
fn held_job_is_not_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        Arc::new(JobStore::open_configured(dir.path().join("jobs.db"), held_config()).unwrap());
    let locks = JobLocks::new(dir.path().join("locks"));
    let held = submit(&store, "good");
    let plain = submit(&store, "good");
    store.begin_submission(&held, 3).unwrap().unwrap();
    store
        .hold_submission(
            &held,
            &JobError::new("outcome_unknown", "may have been sent"),
            "held",
        )
        .unwrap()
        .unwrap();
    assert_eq!(store.get(&held).unwrap().status, "held");

    let handler = Arc::new(Picky {
        calls: AtomicUsize::new(0),
    });
    let runner = Runner::new(store.clone(), locks, handler.clone());

    assert_eq!(runner.recoverable_ids().unwrap(), vec![plain.clone()]);
    let results = runner.resume_recoverable_each().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, plain);
    assert_eq!(runner.resume_recoverable().unwrap().len(), 0);

    // An explicit run is refused with Paused and counts nothing.
    let attempts_before = store.get(&held).unwrap().attempts;
    match runner.run(&held).unwrap() {
        RunOutcome::Paused(job) => assert_eq!(job.status, "held"),
        other => panic!("expected Paused, got {other:?}"),
    }
    assert_eq!(
        handler.calls.load(Ordering::SeqCst),
        1,
        "only the plain job ran"
    );
    let after = store.get(&held).unwrap();
    assert_eq!(after.status, "held");
    assert_eq!(after.attempts, attempts_before);
}

/// Cancels its own job, then fails with an ordinary error.
struct CancelThenFail;

impl JobHandler for CancelThenFail {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        ctl.store().cancel(&job.job_id)?;
        Err(JobError::new("late", "failed after the cancel landed"))
    }
}

#[test]
fn late_error_after_cancel_ends_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, "good");
    let runner = Runner::new(store.clone(), locks, Arc::new(CancelThenFail));

    match runner.run(&id).unwrap() {
        RunOutcome::Finished(job) => {
            assert_eq!(job.status, CANCELLED);
            assert_ne!(job.status, FAILED);
        }
        other => panic!("expected Finished, got {other:?}"),
    }
    assert_eq!(store.get(&id).unwrap().status, CANCELLED);
}
