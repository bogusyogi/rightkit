//! `StoreConfig::work_root`: per-request work directories (`<root>/<request_id>`) are claimed by
//! `claimed_paths_with` and removed by `delete`, like the per-job scratch directory.

use std::path::PathBuf;
use std::sync::Arc;

use rightkit_job_queue::{
    ClaimScope, DeleteOptions, Job, JobControl, JobHandler, JobLocks, JobResult, JobStore, NewJob,
    Runner, StoreConfig,
};
use serde_json::{json, Value};

struct Idle;

impl JobHandler for Idle {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Ok(json!({}))
    }
}

fn new_job(request_id: Option<&str>) -> NewJob {
    NewJob {
        request_id: request_id.map(str::to_string),
        kind: "scrape".into(),
        request: json!({}),
    }
}

struct Env {
    _dir: tempfile::TempDir,
    base: PathBuf,
    work: PathBuf,
    store: Arc<JobStore>,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let work = base.join("output").join("_work");
    std::fs::create_dir_all(&work).unwrap();
    let cfg = StoreConfig::new().work_root(&work);
    let store = Arc::new(JobStore::open_configured(base.join("jobs.db"), cfg).unwrap());
    Env {
        _dir: dir,
        base,
        work,
        store,
    }
}

fn work_dir(env: &Env, request_id: &str) -> PathBuf {
    let d = env.work.join(request_id);
    std::fs::create_dir_all(d.join("parts")).unwrap();
    std::fs::write(d.join("parts").join("p0.tmp"), "x").unwrap();
    d
}

#[test]
fn claims_cover_the_work_directory_of_non_terminal_jobs_only_for_that_scope() {
    let env = env();
    let live = env
        .store
        .submit(&new_job(Some("req-live")))
        .unwrap()
        .0
        .job_id;
    let done = env
        .store
        .submit(&new_job(Some("req-done")))
        .unwrap()
        .0
        .job_id;
    env.store.complete(&done, &json!({})).unwrap();
    let _ = live;

    let non_terminal = env
        .store
        .claimed_paths_with(ClaimScope::NonTerminal)
        .unwrap();
    assert_eq!(non_terminal, vec![env.work.join("req-live")]);

    let all = env.store.claimed_paths_with(ClaimScope::All).unwrap();
    assert_eq!(
        all,
        vec![env.work.join("req-live"), env.work.join("req-done")]
    );
}

#[test]
fn a_job_without_a_request_id_or_with_an_unsafe_one_claims_no_work_directory() {
    let env = env();
    env.store.submit(&new_job(None)).unwrap();
    env.store.submit(&new_job(Some("../escape"))).unwrap();
    assert!(env
        .store
        .claimed_paths_with(ClaimScope::All)
        .unwrap()
        .is_empty());
}

#[test]
fn a_sweep_over_the_work_root_keeps_claimed_directories_and_removes_strays() {
    let env = env();
    let live = work_dir(&env, "req-live");
    let done = work_dir(&env, "req-done");
    let stray = work_dir(&env, "req-stray");
    env.store.submit(&new_job(Some("req-live"))).unwrap();
    let id = env
        .store
        .submit(&new_job(Some("req-done")))
        .unwrap()
        .0
        .job_id;
    env.store.complete(&id, &json!({})).unwrap();

    let runner = Runner::new(
        env.store.clone(),
        JobLocks::new(env.base.join("locks")),
        Arc::new(Idle),
    );
    let removed = runner
        .sweep_unclaimed_with(&env.work, ClaimScope::All)
        .unwrap();
    assert_eq!(removed, vec![stray.clone()]);
    assert!(live.exists() && done.exists() && !stray.exists());
}

#[test]
fn delete_removes_the_work_directory_and_reports_it() {
    let env = env();
    let dir = work_dir(&env, "req-1");
    let other = work_dir(&env, "req-2");
    let id = env.store.submit(&new_job(Some("req-1"))).unwrap().0.job_id;
    env.store.complete(&id, &json!({})).unwrap();

    let report = env
        .store
        .delete_with(&id, &DeleteOptions::default())
        .unwrap();
    assert!(!dir.exists(), "the job's work directory goes");
    assert!(other.exists(), "another request's directory stays");
    assert!(env.work.exists(), "the root stays");
    assert_eq!(report.removed, vec![dir]);
    assert!(env.store.get(&id).is_err());
}

#[test]
fn delete_cancels_a_live_job_and_removes_its_work_directory() {
    let env = env();
    let dir = work_dir(&env, "req-live");
    let id = env
        .store
        .submit(&new_job(Some("req-live")))
        .unwrap()
        .0
        .job_id;
    env.store.delete(&id).unwrap();
    assert!(!dir.exists());
}

#[test]
fn keep_files_leaves_the_work_directory() {
    let env = env();
    let dir = work_dir(&env, "req-1");
    let id = env.store.submit(&new_job(Some("req-1"))).unwrap().0.job_id;
    env.store.complete(&id, &json!({})).unwrap();
    env.store
        .delete_with(&id, &DeleteOptions { keep_files: true })
        .unwrap();
    assert!(dir.exists());
    assert!(env.store.get(&id).is_err());
}

#[test]
fn a_missing_work_directory_is_not_an_error() {
    let env = env();
    let id = env
        .store
        .submit(&new_job(Some("req-none")))
        .unwrap()
        .0
        .job_id;
    env.store.complete(&id, &json!({})).unwrap();
    let report = env
        .store
        .delete_with(&id, &DeleteOptions::default())
        .unwrap();
    assert!(report.removed.is_empty());
}

#[test]
fn an_artifact_inside_the_work_directory_is_deletable_and_one_outside_every_root_is_refused() {
    let env = env();
    let dir = work_dir(&env, "req-1");
    let part = dir.join("parts").join("p0.tmp");
    let id = env.store.submit(&new_job(Some("req-1"))).unwrap().0.job_id;
    env.store.add_artifact(&id, &part, "text/plain").unwrap();
    env.store.complete(&id, &json!({})).unwrap();

    let outside = env.base.join("precious.txt");
    std::fs::write(&outside, "keep").unwrap();
    env.store.add_artifact(&id, &outside, "text/plain").unwrap();
    let err = env
        .store
        .delete_with(&id, &DeleteOptions::default())
        .unwrap_err();
    assert_eq!(err.code, rightkit_job_queue::PATH_OUTSIDE_ROOT);
    assert!(outside.exists() && part.exists() && dir.exists());
}

#[test]
fn a_relative_work_root_is_refused_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = StoreConfig::new().work_root("relative/_work");
    let err = JobStore::open_configured(dir.path().join("jobs.db"), cfg)
        .err()
        .expect("a relative root is refused");
    assert_eq!(err.code, "config");
}

#[test]
fn a_store_without_a_work_root_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("jobs.db"), StoreConfig::new()).unwrap();
    store.submit(&new_job(Some("req-1"))).unwrap();
    assert!(store
        .claimed_paths_with(ClaimScope::All)
        .unwrap()
        .is_empty());
    assert!(StoreConfig::new().work_roots().is_empty());
}

#[test]
fn files_recorded_inside_the_work_directory_are_removed_with_it() {
    let env = env();
    let dir = work_dir(&env, "req-1");
    let part = dir.join("parts").join("p0.tmp");
    let id = env.store.submit(&new_job(Some("req-1"))).unwrap().0.job_id;
    env.store.add_artifact(&id, &part, "text/plain").unwrap();
    env.store.complete(&id, &json!({})).unwrap();
    let report = env
        .store
        .delete_with(&id, &DeleteOptions::default())
        .unwrap();
    assert!(!part.exists() && !dir.exists());
    assert_eq!(report.removed, vec![part, dir]);
}
