//! `JobStore::delete` owns cleanup of the job's artifact files and scratch directory, inside
//! the configured roots only, in a crash-safe order.

use std::path::PathBuf;

use rightkit_jobs::{DeleteOptions, JobStore, NewJob, StoreConfig, PATH_OUTSIDE_ROOT};
use serde_json::json;

fn new_job(tag: &str) -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({"tag": tag}),
    }
}

struct Env {
    dir: tempfile::TempDir,
    artifacts: PathBuf,
    scratch: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let artifacts = base.join("artifacts");
        let scratch = base.join("scratch");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        Self {
            dir,
            artifacts,
            scratch,
        }
    }
    fn base(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }
    fn cfg(&self) -> StoreConfig {
        StoreConfig::new()
            .artifact_root(&self.artifacts)
            .scratch_root(&self.scratch)
    }
    fn open(&self) -> JobStore {
        JobStore::open_configured(self.base().join("j.db"), self.cfg()).unwrap()
    }
}

/// A finished job with one artifact at `<artifacts>/<id>/out.bin` and a scratch directory.
fn finished(env: &Env, store: &JobStore, tag: &str) -> (String, PathBuf, PathBuf) {
    let id = store.submit(&new_job(tag)).unwrap().0.job_id;
    let art_dir = env.artifacts.join(&id);
    std::fs::create_dir_all(&art_dir).unwrap();
    let file = art_dir.join("out.bin");
    std::fs::write(&file, tag).unwrap();
    store.add_artifact(&id, &file, "text/plain").unwrap();
    let scratch = env.scratch.join(&id);
    std::fs::create_dir_all(scratch.join("sub")).unwrap();
    std::fs::write(scratch.join("sub").join("tmp.dat"), "x").unwrap();
    store.complete(&id, &json!({"tag": tag})).unwrap();
    (id, file, scratch)
}

fn job_gone(store: &JobStore, id: &str) -> bool {
    store.get(id).is_err()
}

#[test]
fn delete_removes_artifact_and_scratch_files() {
    let env = Env::new();
    let store = env.open();
    let (id, file, scratch) = finished(&env, &store, "a");
    let report = store.delete_with(&id, &DeleteOptions::default()).unwrap();
    assert!(!file.exists());
    assert!(!scratch.exists());
    assert!(
        !env.artifacts.join(&id).exists(),
        "the emptied job dir goes"
    );
    assert!(env.artifacts.exists() && env.scratch.exists(), "roots stay");
    assert_eq!(report.removed, vec![file]);
    assert_eq!(report.scratch_removed, Some(scratch));
    assert_eq!(report.prune.jobs, vec![id.clone()]);
    assert!(job_gone(&store, &id));
    assert!(store.pending_deletions().unwrap().is_empty());
}

#[test]
fn plain_delete_also_cleans_up_when_roots_are_configured() {
    let env = Env::new();
    let store = env.open();
    let (id, file, scratch) = finished(&env, &store, "b");
    let pr = store.delete(&id).unwrap();
    assert_eq!(pr.artifacts.len(), 1);
    assert!(!file.exists() && !scratch.exists());
}

#[test]
fn keep_files_removes_rows_only() {
    let env = Env::new();
    let store = env.open();
    let (id, file, scratch) = finished(&env, &store, "c");
    let report = store
        .delete_with(&id, &DeleteOptions { keep_files: true })
        .unwrap();
    assert!(file.exists() && scratch.exists());
    assert_eq!(report.kept, vec![file]);
    assert!(report.removed.is_empty() && report.scratch_removed.is_none());
    assert!(job_gone(&store, &id));
}

#[test]
fn a_store_without_roots_leaves_files_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("j.db"), StoreConfig::new()).unwrap();
    let id = store.submit(&new_job("d")).unwrap().0.job_id;
    let file = dir.path().join("out.bin");
    std::fs::write(&file, "d").unwrap();
    store.add_artifact(&id, &file, "text/plain").unwrap();
    store.complete(&id, &json!({})).unwrap();
    let report = store.delete_with(&id, &DeleteOptions::default()).unwrap();
    assert!(file.exists());
    assert_eq!(report.kept, vec![file]);
}

#[test]
fn a_path_outside_the_roots_is_an_error_and_deletes_nothing() {
    let env = Env::new();
    let store = env.open();
    let (id, file, scratch) = finished(&env, &store, "e");
    let outside = env.base().join("precious.txt");
    std::fs::write(&outside, "keep me").unwrap();
    store.add_artifact(&id, &outside, "text/plain").unwrap();
    let err = store
        .delete_with(&id, &DeleteOptions::default())
        .unwrap_err();
    assert_eq!(err.code, PATH_OUTSIDE_ROOT);
    assert!(outside.exists() && file.exists() && scratch.exists());
    assert!(!job_gone(&store, &id), "the job is untouched");
    assert!(store.pending_deletions().unwrap().is_empty());
    // The caller can still drop the rows and keep every file.
    store
        .delete_with(&id, &DeleteOptions { keep_files: true })
        .unwrap();
    assert!(outside.exists());
}

#[test]
fn dot_dot_and_relative_paths_are_refused() {
    let env = Env::new();
    let store = env.open();
    let id = store.submit(&new_job("f")).unwrap().0.job_id;
    let outside = env.base().join("precious.txt");
    std::fs::write(&outside, "keep me").unwrap();
    let sneaky: PathBuf = env.artifacts.join("..").join("precious.txt");
    store.add_artifact(&id, &sneaky, "text/plain").unwrap();
    store.complete(&id, &json!({})).unwrap();
    let err = store.delete(&id).unwrap_err();
    assert_eq!(err.code, PATH_OUTSIDE_ROOT);
    assert!(outside.exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_cannot_lead_out_of_the_root() {
    use std::os::unix::fs::symlink;
    let env = Env::new();
    let store = env.open();
    let outside_dir = env.base().join("elsewhere");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let secret = outside_dir.join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();
    let link = env.artifacts.join("link");
    symlink(&outside_dir, &link).unwrap();
    let id = store.submit(&new_job("g")).unwrap().0.job_id;
    store
        .add_artifact(&id, &link.join("secret.txt"), "text/plain")
        .unwrap();
    store.complete(&id, &json!({})).unwrap();
    let err = store.delete(&id).unwrap_err();
    assert_eq!(err.code, PATH_OUTSIDE_ROOT);
    assert!(secret.exists());
    assert!(!job_gone(&store, &id));
}

#[cfg(unix)]
#[test]
fn a_symlink_artifact_is_unlinked_and_its_target_survives() {
    use std::os::unix::fs::symlink;
    let env = Env::new();
    let store = env.open();
    let target = env.base().join("target.txt");
    std::fs::write(&target, "t").unwrap();
    let link = env.artifacts.join("ln.txt");
    symlink(&target, &link).unwrap();
    let id = store.submit(&new_job("h")).unwrap().0.job_id;
    store.add_artifact(&id, &link, "text/plain").unwrap();
    store.complete(&id, &json!({})).unwrap();
    store.delete(&id).unwrap();
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link is gone"
    );
    assert!(target.exists(), "what it pointed at is not touched");
}

#[cfg(unix)]
#[test]
fn a_scratch_symlink_is_unlinked_not_followed() {
    use std::os::unix::fs::symlink;
    let env = Env::new();
    let store = env.open();
    let id = store.submit(&new_job("i")).unwrap().0.job_id;
    let outside_dir = env.base().join("elsewhere");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("keep.txt"), "k").unwrap();
    symlink(&outside_dir, env.scratch.join(&id)).unwrap();
    store.complete(&id, &json!({})).unwrap();
    store.delete(&id).unwrap();
    assert!(outside_dir.join("keep.txt").exists());
    assert!(std::fs::symlink_metadata(env.scratch.join(&id)).is_err());
}

#[test]
fn a_file_recorded_by_another_job_is_left_for_it() {
    let env = Env::new();
    let store = env.open();
    let shared = env.artifacts.join("shared.bin");
    std::fs::write(&shared, "s").unwrap();
    let a = store.submit(&new_job("j1")).unwrap().0.job_id;
    let b = store.submit(&new_job("j2")).unwrap().0.job_id;
    store.add_artifact(&a, &shared, "x/y").unwrap();
    store.add_artifact(&b, &shared, "x/y").unwrap();
    store.complete(&a, &json!({})).unwrap();
    store.complete(&b, &json!({})).unwrap();
    let report = store.delete_with(&a, &DeleteOptions::default()).unwrap();
    assert!(shared.exists());
    assert_eq!(report.shared, vec![shared.clone()]);
    store.delete(&b).unwrap();
    assert!(!shared.exists(), "the last job to record it removes it");
}

#[test]
fn an_interrupted_delete_is_finished_by_recovery() {
    let env = Env::new();
    let (id, file, scratch) = {
        let store = env.open();
        let (id, file, scratch) = finished(&env, &store, "k");
        // Step 1 only: the process "dies" before any file is removed.
        store.begin_delete(&id, &DeleteOptions::default()).unwrap();
        assert_eq!(store.pending_deletions().unwrap(), vec![id.clone()]);
        assert!(file.exists() && scratch.exists());
        (id, file, scratch)
    };
    // A partly finished step 2: the artifact is already gone, the scratch directory is not.
    std::fs::remove_file(&file).unwrap();
    let store = env.open();
    assert_eq!(store.pending_deletions().unwrap(), vec![id.clone()]);
    let done = store.finish_deletions().unwrap();
    assert_eq!(done.len(), 1);
    let report = done.into_iter().next().unwrap().1.unwrap();
    assert_eq!(report.already_gone, vec![file]);
    assert_eq!(report.scratch_removed, Some(scratch.clone()));
    assert!(!scratch.exists());
    assert!(job_gone(&store, &id));
    assert!(store.pending_deletions().unwrap().is_empty());
    assert!(store.finish_deletions().unwrap().is_empty());
}

#[test]
fn recovery_honours_keep_files() {
    let env = Env::new();
    let (id, file) = {
        let store = env.open();
        let (id, file, _) = finished(&env, &store, "l");
        store
            .begin_delete(&id, &DeleteOptions { keep_files: true })
            .unwrap();
        (id, file)
    };
    let store = env.open();
    store.finish_deletions().unwrap();
    assert!(file.exists());
    assert!(job_gone(&store, &id));
}
