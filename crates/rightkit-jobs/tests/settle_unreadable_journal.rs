//! A settle-later journal file that cannot be read is not a bad file: it stays in place, is
//! reported, and is read again on an explicit flush. Only content that was read and failed to
//! parse is set aside. A directory at `<job>.json` makes the read fail with an error that is not
//! `NotFound` (`IsADirectory` on Unix, `PermissionDenied` on Windows) on every platform.

use std::path::{Path, PathBuf};

use rightkit_jobs::{JobStore, NewJob, COMPLETE, RUNNING};
use serde_json::json;

fn open(dir: &Path) -> JobStore {
    JobStore::open_configured(dir.join("jobs.db"), rightkit_jobs::StoreConfig::new()).unwrap()
}

fn journal(dir: &Path) -> PathBuf {
    dir.join("jobs.db.settlements")
}

/// A running job, its store closed, and the journal path of its terminal outcome.
fn running_job(dir: &Path) -> (String, PathBuf) {
    let store = open(dir);
    let (job, _) = store
        .submit(&NewJob {
            request_id: None,
            kind: "settle".into(),
            request: json!({}),
        })
        .unwrap();
    store.begin_run(&job.job_id, true).unwrap().unwrap();
    std::fs::create_dir_all(journal(dir)).unwrap();
    let file = journal(dir).join(format!("{}.json", job.job_id));
    (job.job_id, file)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn an_unreadable_journal_file_stays_pending_and_is_read_again_on_flush() {
    let dir = tempfile::tempdir().unwrap();
    let (id, file) = running_job(dir.path());
    std::fs::create_dir(&file).unwrap();

    let store = open(dir.path());
    assert_eq!(names(&journal(dir.path())), vec![format!("{id}.json")]);
    assert!(file.is_dir(), "not renamed to .json.bad");
    assert_eq!(store.get(&id).unwrap().status, RUNNING);
    let held = store.pending_settlements();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].job_id, id);
    assert!(held[0].status.is_empty() && held[0].durable && !held[0].claimed);
    assert!(held[0].last_error.contains("could not be read"));
    assert!(!store.shutdown_detailed().is_clean());

    // Still unreadable (the shutdown report above already flushed once): still listed, still
    // in place, one more failed attempt.
    let before = store.pending_settlements()[0].attempts;
    let report = store.flush_pending_settlements();
    assert_eq!((report.settled, report.pending), (0, 0));
    assert!(file.is_dir());
    assert_eq!(store.pending_settlements()[0].attempts, before + 1);

    // Readable now: the recorded outcome is held and written.
    std::fs::remove_dir(&file).unwrap();
    std::fs::write(
        &file,
        json!({"version": 1, "job_id": id, "to": COMPLETE, "result": {"n": 1}}).to_string(),
    )
    .unwrap();
    let report = store.flush_pending_settlements();
    assert_eq!(report.settled, 1);
    assert!(store.pending_settlements().is_empty());
    assert_eq!(store.get(&id).unwrap().status, COMPLETE);
    assert!(names(&journal(dir.path())).is_empty());
}

#[test]
fn an_unreadable_journal_file_that_vanishes_is_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let (_, file) = running_job(dir.path());
    std::fs::create_dir(&file).unwrap();
    let store = open(dir.path());
    assert_eq!(store.pending_settlements().len(), 1);
    std::fs::remove_dir(&file).unwrap();
    store.flush_pending_settlements();
    assert!(store.pending_settlements().is_empty());
}

#[test]
fn content_that_was_read_and_does_not_parse_is_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let (id, file) = running_job(dir.path());
    std::fs::write(&file, b"not json").unwrap();
    let store = open(dir.path());
    assert!(store.pending_settlements().is_empty());
    assert_eq!(names(&journal(dir.path())), vec![format!("{id}.json.bad")]);
    assert_eq!(store.get(&id).unwrap().status, RUNNING);
}
