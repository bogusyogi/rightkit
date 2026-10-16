//! `ClaimScope`: which jobs' files a sweep treats as owned. `NonTerminal` is what the sweep has
//! always done; `All` also claims the artifact and work paths of finished jobs still in the store.

use std::path::PathBuf;
use std::sync::Arc;

use rightkit_job_queue::{
    ClaimScope, Job, JobControl, JobHandler, JobLocks, JobResult, JobStore, NewJob, Runner,
    StoreConfig,
};
use serde_json::{json, Value};

struct Idle;

impl JobHandler for Idle {
    fn run(&self, _job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        Ok(json!({}))
    }
}

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "media".into(),
        request: json!({"tag": tag}),
    }
}

struct Rig {
    dir: tempfile::TempDir,
    store: Arc<JobStore>,
    runner: Runner,
    /// A running job with a file, a finished job with a file, and a stray directory.
    active_file: PathBuf,
    done_file: PathBuf,
    stray: PathBuf,
    active: String,
    done: String,
}

fn rig(scratch: Option<&str>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = StoreConfig::new();
    if let Some(name) = scratch {
        cfg = cfg.scratch_root(dir.path().join(name));
    }
    let store = Arc::new(JobStore::open_configured(dir.path().join("jobs.db"), cfg).unwrap());
    let media = dir.path().join("media");
    let make = |name: &str| {
        let d = media.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("out.bin");
        std::fs::write(&f, name).unwrap();
        f
    };
    let (active_file, done_file) = (make("active"), make("done"));
    let stray = media.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("junk.bin"), b"junk").unwrap();

    let active = store.submit(&new_job("active")).unwrap().0.job_id;
    store.add_artifact(&active, &active_file, "x/y").unwrap();
    let done = store.submit(&new_job("done")).unwrap().0.job_id;
    store.add_artifact(&done, &done_file, "x/y").unwrap();
    store.complete(&done, &json!({})).unwrap();

    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Idle),
    );
    Rig {
        dir,
        store,
        runner,
        active_file,
        done_file,
        stray,
        active,
        done,
    }
}

impl Rig {
    fn media(&self) -> PathBuf {
        self.dir.path().join("media")
    }
}

#[test]
fn the_existing_calls_keep_their_scope() {
    let rig = rig(None);
    assert_eq!(
        rig.store.claimed_paths().unwrap(),
        vec![rig.active_file.clone()]
    );
    assert_eq!(
        rig.store
            .claimed_paths_with(ClaimScope::NonTerminal)
            .unwrap(),
        vec![rig.active_file.clone()]
    );
    assert_eq!(ClaimScope::default(), ClaimScope::NonTerminal);

    // The default sweep still treats a finished job's directory as unclaimed.
    let mut removed = rig.runner.sweep_unclaimed(&rig.media()).unwrap();
    removed.sort();
    let mut want = vec![rig.stray.clone(), rig.media().join("done")];
    want.sort();
    assert_eq!(removed, want);
    assert!(rig.active_file.exists());
    assert!(!rig.done_file.exists());
}

#[test]
fn all_claims_the_files_of_finished_jobs_still_in_the_store() {
    let rig = rig(None);
    let mut all = rig.store.claimed_paths_with(ClaimScope::All).unwrap();
    all.sort();
    let mut want = vec![rig.active_file.clone(), rig.done_file.clone()];
    want.sort();
    assert_eq!(all, want);

    let removed = rig
        .runner
        .sweep_unclaimed_with(&rig.media(), ClaimScope::All)
        .unwrap();
    assert_eq!(removed, vec![rig.stray.clone()]);
    assert!(rig.active_file.exists(), "the running job's file stays");
    assert!(rig.done_file.exists(), "the finished job's media stays");

    // Once the finished job is gone from the store, its directory is an orphan again.
    rig.store
        .delete_with(
            &rig.done,
            &rightkit_job_queue::DeleteOptions { keep_files: true },
        )
        .unwrap();
    let removed = rig
        .runner
        .sweep_unclaimed_with(&rig.media(), ClaimScope::All)
        .unwrap();
    assert_eq!(removed, vec![rig.media().join("done")]);
    assert!(rig.active_file.exists());
}

#[test]
fn all_also_claims_the_work_directory_of_every_row() {
    let rig = rig(Some("work"));
    let work = rig.dir.path().join("work");
    for id in [&rig.active, &rig.done] {
        std::fs::create_dir_all(work.join(id)).unwrap();
    }
    std::fs::create_dir_all(work.join("orphan")).unwrap();

    // Non-terminal scope claims artifacts only: the work directories are unclaimed.
    let nt = rig
        .store
        .claimed_paths_with(ClaimScope::NonTerminal)
        .unwrap();
    assert!(!nt.iter().any(|p| p.starts_with(&work)), "{nt:?}");

    let all = rig.store.claimed_paths_with(ClaimScope::All).unwrap();
    assert!(all.contains(&work.join(&rig.active)));
    assert!(all.contains(&work.join(&rig.done)));
    assert!(!all.contains(&work.join("orphan")));

    let removed = rig
        .runner
        .sweep_unclaimed_with(&work, ClaimScope::All)
        .unwrap();
    assert_eq!(removed, vec![work.join("orphan")]);
    assert!(work.join(&rig.active).exists() && work.join(&rig.done).exists());
}
