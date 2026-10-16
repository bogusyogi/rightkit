//! Per-job lock files do not outlive the job: the runner removes `<id>.lock` once the job is
//! terminal, and `JobLocks::remove` / `JobLocks::sweep` / `Runner::sweep_job_locks` clear what
//! crashed runs and older versions left behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rightkit_job_queue::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob, RunOutcome,
    Runner, StoreConfig,
};
use serde_json::{json, Value};

struct Probe {
    locks_dir: PathBuf,
    fail: bool,
    saw_lock_while_running: AtomicBool,
}

impl JobHandler for Probe {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        if self
            .locks_dir
            .join(format!("{}.lock", job.job_id))
            .exists()
        {
            self.saw_lock_while_running.store(true, Ordering::SeqCst);
        }
        if self.fail {
            return Err(JobError::new("boom", "handler failed"));
        }
        Ok(json!({}))
    }
}

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "k".into(),
        request: json!({}),
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    locks_dir: PathBuf,
    store: Arc<JobStore>,
    locks: JobLocks,
    handler: Arc<Probe>,
    runner: Runner,
}

fn rig(fail: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let locks_dir = base.join("locks");
    let store = Arc::new(JobStore::open_configured(base.join("jobs.db"), StoreConfig::new()).unwrap());
    let locks = JobLocks::new(&locks_dir);
    let handler = Arc::new(Probe {
        locks_dir: locks_dir.clone(),
        fail,
        saw_lock_while_running: AtomicBool::new(false),
    });
    let runner = Runner::new(store.clone(), locks.clone(), handler.clone());
    Rig {
        _dir: dir,
        locks_dir,
        store,
        locks,
        handler,
        runner,
    }
}

fn lock_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

#[test]
fn a_completed_job_leaves_no_lock_file() {
    let rig = rig(false);
    let id = rig.store.submit(&new_job()).unwrap().0.job_id;
    match rig.runner.run(&id).unwrap() {
        RunOutcome::Finished(job) => assert_eq!(job.status, "complete"),
        other => panic!("expected the job to finish, got {other:?}"),
    }
    assert!(
        rig.handler.saw_lock_while_running.load(Ordering::SeqCst),
        "the lock exists while the handler runs"
    );
    assert_eq!(lock_files(&rig.locks_dir), Vec::<String>::new());
}

#[test]
fn a_failed_job_leaves_no_lock_file() {
    let rig = rig(true);
    let id = rig.store.submit(&new_job()).unwrap().0.job_id;
    match rig.runner.run(&id).unwrap() {
        RunOutcome::Finished(job) => assert_eq!(job.status, "failed"),
        other => panic!("expected the job to finish, got {other:?}"),
    }
    assert_eq!(lock_files(&rig.locks_dir), Vec::<String>::new());
}

#[test]
fn remove_deletes_a_free_lock_file_and_never_a_held_one() {
    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("locks"));

    // Nothing there yet.
    assert!(!locks.remove("job-1").unwrap());

    drop(locks.try_lock("job-1").unwrap().unwrap());
    assert_eq!(lock_files(&dir.path().join("locks")), vec!["job-1.lock"]);
    assert!(locks.remove("job-1").unwrap());
    assert!(lock_files(&dir.path().join("locks")).is_empty());

    let held = locks.try_lock("job-2").unwrap().unwrap();
    assert!(!locks.remove("job-2").unwrap(), "a held lock is not removed");
    assert!(locks.try_lock("job-2").unwrap().is_none(), "still held");
    drop(held);
    assert!(locks.remove("job-2").unwrap());

    // An id that cannot be a lock name is refused like try_lock refuses it.
    assert!(locks.remove("../x").is_err());
}

#[test]
fn a_lock_can_be_taken_again_after_its_file_was_removed() {
    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("locks"));
    drop(locks.try_lock("job-1").unwrap().unwrap());
    assert!(locks.remove("job-1").unwrap());
    let again = locks.try_lock("job-1").unwrap();
    assert!(again.is_some());
    assert!(locks.try_lock("job-1").unwrap().is_none());
}

#[test]
fn sweep_removes_only_unheld_files_the_predicate_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let locks = JobLocks::new(&locks_dir);
    for id in ["live", "gone-a", "gone-b"] {
        drop(locks.try_lock(id).unwrap().unwrap());
    }
    let held = locks.try_lock("gone-held").unwrap().unwrap();
    std::fs::write(locks_dir.join("notes.txt"), "keep").unwrap();

    let removed = locks.sweep(|id| id == "live").unwrap();
    assert_eq!(removed, vec!["gone-a".to_string(), "gone-b".to_string()]);
    let left = lock_files(&locks_dir);
    assert!(left.contains(&"live.lock".to_string()));
    assert!(left.contains(&"gone-held.lock".to_string()));
    assert!(left.contains(&"notes.txt".to_string()));
    assert!(!left.contains(&"gone-a.lock".to_string()));
    drop(held);
}

#[test]
fn sweep_of_a_missing_directory_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("never-created"));
    assert!(locks.sweep(|_| false).unwrap().is_empty());
}

#[test]
fn runner_sweep_job_locks_keeps_queued_jobs_and_clears_finished_and_unknown_ones() {
    let rig = rig(false);
    let queued = rig.store.submit(&new_job()).unwrap().0.job_id;
    let finished = rig.store.submit(&new_job()).unwrap().0.job_id;
    rig.store.complete(&finished, &json!({})).unwrap();
    for id in [queued.as_str(), finished.as_str(), "unknown-job"] {
        drop(rig.locks.try_lock(id).unwrap().unwrap());
    }

    let mut removed = rig.runner.sweep_job_locks().unwrap();
    removed.sort();
    let mut want = vec![finished.clone(), "unknown-job".to_string()];
    want.sort();
    assert_eq!(removed, want);
    assert_eq!(
        lock_files(&rig.locks_dir),
        vec![format!("{queued}.lock")]
    );
}
