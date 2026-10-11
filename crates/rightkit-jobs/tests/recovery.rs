//! Per-job recovery class (`Recovery::Never`): a job that began an attempt and then lost its
//! runner (crash, lost lock) is failed with `interrupted` and is never handed to a handler again.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rightkit_jobs::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob, Recovery,
    RunOutcome, Runner, SubmitOptions, FAILED, INTERRUPTED,
};
use serde_json::{json, Value};

const CHILD_DIR: &str = "RIGHTKIT_JOBS_TEST_CRASH_DIR";
const CHILD_ID: &str = "RIGHTKIT_JOBS_TEST_CRASH_ID";

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "uninstall".into(),
        request: json!({"app": "x"}),
    }
}

struct Counter {
    calls: AtomicUsize,
}

impl Counter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl JobHandler for Counter {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ran": true}))
    }
}

struct Retrying(AtomicUsize);

impl JobHandler for Retrying {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(JobError::retryable("net_down", "network went away"))
    }
}

/// Appends one line per handler call, then dies without finishing (a crash).
struct Crasher(std::path::PathBuf);

impl JobHandler for Crasher {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .unwrap();
        writeln!(f, "call").unwrap();
        f.sync_all().unwrap();
        std::process::abort();
    }
}

#[test]
fn crash_child_entry() {
    let (Ok(dir), Ok(id)) = (std::env::var(CHILD_DIR), std::env::var(CHILD_ID)) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let store = Arc::new(JobStore::open(dir.join("jobs.db")).unwrap());
    let runner = Runner::new(
        store,
        JobLocks::new(dir.join("locks")),
        Arc::new(Crasher(dir.join("calls.log"))),
    );
    let _ = runner.run(&id);
}

fn rig(dir: &std::path::Path) -> (Arc<JobStore>, JobLocks) {
    (
        Arc::new(JobStore::open(dir.join("jobs.db")).unwrap()),
        JobLocks::new(dir.join("locks")),
    )
}

fn submit(store: &JobStore, recovery: Recovery) -> String {
    store
        .submit_with(
            &new_job(),
            &Default::default(),
            &SubmitOptions::new().recovery(recovery),
        )
        .unwrap()
        .record
        .job
        .job_id
}

/// What the runner does at the start of an attempt; dropping it models a crash with the lock free.
fn start_and_lose(store: &JobStore, id: &str) {
    store.begin_run_fenced(id, true).unwrap().unwrap();
}

fn error_code(job: &Job) -> Option<String> {
    job.error.as_ref()?["code"].as_str().map(String::from)
}

#[test]
fn never_job_is_not_rerun_after_a_real_process_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, Recovery::Never);
    drop(store);

    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_child_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_DIR, dir.path())
        .env(CHILD_ID, &id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "the child must have crashed");
    let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "the handler ran once in the child");

    // New process: reopen, resume.
    let store = Arc::new(JobStore::open(dir.path().join("jobs.db")).unwrap());
    assert_eq!(
        store.recovery(&id).unwrap(),
        Recovery::Never,
        "class is persisted"
    );
    let counter = Counter::new();
    let runner = Runner::new(store.clone(), locks, counter.clone());
    let outcomes = runner.resume_recoverable().unwrap();

    assert_eq!(
        counter.calls(),
        0,
        "a Never job is never handed to a handler again"
    );
    assert_eq!(outcomes.len(), 1);
    let RunOutcome::Finished(job) = &outcomes[0] else {
        panic!("expected a terminal outcome, got {:?}", outcomes[0]);
    };
    assert_eq!(job.status, FAILED);
    assert_eq!(error_code(job).as_deref(), Some(INTERRUPTED));
    assert!(store
        .events_after(&id, 0)
        .unwrap()
        .iter()
        .any(|e| e.stage == "interrupted"));

    // A second pass and an explicit run both leave it alone.
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert!(matches!(runner.run(&id).unwrap(), RunOutcome::Finished(_)));
    assert_eq!(counter.calls(), 0);
}

#[test]
fn recoverable_ids_interrupts_never_jobs_and_lists_resume_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let never = submit(&store, Recovery::Never);
    let resume = submit(&store, Recovery::Resume);
    start_and_lose(&store, &never);
    start_and_lose(&store, &resume);

    let runner = Runner::new(store.clone(), locks, Counter::new());
    assert_eq!(runner.recoverable_ids().unwrap(), vec![resume.clone()]);
    let job = store.get(&never).unwrap();
    assert_eq!(job.status, FAILED);
    assert_eq!(error_code(&job).as_deref(), Some(INTERRUPTED));
    assert!(!store.get(&resume).unwrap().is_terminal());
}

#[test]
fn run_refuses_a_started_never_job() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, Recovery::Never);
    start_and_lose(&store, &id);
    let counter = Counter::new();
    let runner = Runner::new(store.clone(), locks, counter.clone());

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("run must end terminal");
    };
    assert_eq!(job.status, FAILED);
    assert_eq!(error_code(&job).as_deref(), Some(INTERRUPTED));
    assert_eq!(counter.calls(), 0);
    assert_eq!(store.get(&id).unwrap().attempts, 1, "no attempt was spent");
}

#[test]
fn never_job_that_never_started_still_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, Recovery::Never);
    let counter = Counter::new();
    let runner = Runner::new(store.clone(), locks, counter.clone());
    assert_eq!(runner.recoverable_ids().unwrap(), vec![id.clone()]);
    let out = runner.resume_recoverable().unwrap();
    assert_eq!(counter.calls(), 1);
    assert!(matches!(&out[0], RunOutcome::Finished(j) if j.status == "complete"));
}

#[test]
fn never_job_does_not_pause_on_a_retryable_error() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let id = submit(&store, Recovery::Never);
    let handler = Arc::new(Retrying(AtomicUsize::new(0)));
    let runner = Runner::new(store.clone(), locks, handler.clone());

    let RunOutcome::Finished(job) = runner.run(&id).unwrap() else {
        panic!("a Never job must not pause after its handler ran");
    };
    assert_eq!(job.status, FAILED);
    assert_eq!(error_code(&job).as_deref(), Some(INTERRUPTED));
    assert!(job.error.as_ref().unwrap()["message"]
        .as_str()
        .unwrap()
        .contains("net_down"));
    assert!(runner.resume_recoverable().unwrap().is_empty());
    assert_eq!(handler.0.load(Ordering::SeqCst), 1);
}

#[test]
fn default_class_resumes_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let (store, locks) = rig(dir.path());
    let (job, _) = store.submit(&new_job()).unwrap();
    assert_eq!(store.recovery(&job.job_id).unwrap(), Recovery::Resume);
    start_and_lose(&store, &job.job_id);
    let counter = Counter::new();
    let runner = Runner::new(store.clone(), locks, counter.clone());
    assert_eq!(runner.recoverable_ids().unwrap(), vec![job.job_id.clone()]);
    runner.resume_recoverable().unwrap();
    assert_eq!(counter.calls(), 1);
}

#[test]
fn rows_written_by_0_2_7_read_as_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    {
        let conn = rightkit_jobs::rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE jobs (
  job_id TEXT PRIMARY KEY, request_id TEXT UNIQUE, kind TEXT NOT NULL,
  request_json TEXT NOT NULL, status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  error_json TEXT, result_json TEXT, acked_by TEXT, acked_at TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, attempt_gen INTEGER NOT NULL DEFAULT 0);
INSERT INTO jobs (job_id, kind, request_json, status, attempts, created_at, updated_at, attempt_gen)
  VALUES ('old-1', 'k', '{}', 'running', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1);",
        )
        .unwrap();
    }
    let store = Arc::new(JobStore::open(&path).unwrap());
    assert!(
        store.adoption().added_columns.is_empty(),
        "the app's jobs table is not altered: the class lives in a crate-owned side table"
    );
    assert_eq!(store.recovery("old-1").unwrap(), Recovery::Resume);
    let counter = Counter::new();
    let runner = Runner::new(
        store,
        JobLocks::new(dir.path().join("locks")),
        counter.clone(),
    );
    runner.resume_recoverable().unwrap();
    assert_eq!(
        counter.calls(),
        1,
        "an old started row is resumed, as in 0.2.7"
    );
}

#[test]
fn deduplicated_submit_keeps_the_original_class() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = rig(dir.path());
    let n = NewJob {
        request_id: Some("r1".into()),
        kind: "k".into(),
        request: json!({}),
    };
    let a = store
        .submit_with(
            &n,
            &Default::default(),
            &SubmitOptions::new().recovery(Recovery::Never),
        )
        .unwrap();
    let b = store
        .submit_with(&n, &Default::default(), &SubmitOptions::new())
        .unwrap();
    assert!(b.deduplicated);
    assert_eq!(a.record.job.job_id, b.record.job.job_id);
    assert_eq!(
        store.recovery(&a.record.job.job_id).unwrap(),
        Recovery::Never
    );
}
