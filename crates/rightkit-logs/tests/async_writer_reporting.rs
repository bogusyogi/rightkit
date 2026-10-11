//! `AsyncWriter` in `FailureReporting::Once` mode: one report per run of failures, a suppressed
//! count, and one report on recovery.

use rightkit_logs::{AsyncWriter, EventRecord, FailureReporting, JsonlStore, Level};
use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "rightkit-logs-writer-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn event(name: &str) -> EventRecord {
    EventRecord::new(1, Level::Info, name)
}

#[test]
fn consecutive_failures_report_once_and_recovery_reports_once() {
    let dir = temp_dir("once");
    // The store's parent is a regular file, so every write fails until it is removed.
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    let store = Arc::new(JsonlStore::new(blocker.join("events.jsonl"), 0));
    let writer = AsyncWriter::spawn_reporting(store.clone(), 64, FailureReporting::Once).unwrap();

    const N: u64 = 5;
    for i in 0..N {
        assert!(writer.append(event(&format!("fail-{i}"))));
    }
    assert!(writer.flush(Duration::from_secs(10)));
    let status = writer.failure_status();
    assert!(status.failing);
    assert_eq!(status.reports, 1, "one report for the run of failures");
    assert_eq!(
        status.suppressed,
        N - 1,
        "the other failures are suppressed and counted"
    );
    assert!(status.last_error.as_deref().unwrap().contains("append to"));

    // Writes recover: the first success is reported once.
    fs::remove_file(&blocker).unwrap();
    assert!(writer.append(event("recovered")));
    assert!(writer.flush(Duration::from_secs(10)));
    let status = writer.failure_status();
    assert!(!status.failing);
    assert_eq!(status.reports, 2, "one more report on recovery");
    assert_eq!(status.recoveries, 1);

    // Further successes are silent.
    assert!(writer.append(event("steady")));
    assert!(writer.flush(Duration::from_secs(10)));
    assert_eq!(writer.failure_status().reports, 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_second_run_of_failures_is_reported_again() {
    let dir = temp_dir("second-run");
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"x").unwrap();
    let store = Arc::new(JsonlStore::new(blocker.join("events.jsonl"), 0));
    let writer = AsyncWriter::spawn_reporting(store, 64, FailureReporting::Once).unwrap();
    writer.append(event("a"));
    writer.append(event("b"));
    assert!(writer.flush(Duration::from_secs(10)));
    fs::remove_file(&blocker).unwrap();
    writer.append(event("ok"));
    assert!(writer.flush(Duration::from_secs(10)));
    // The recovery created `blocker/` as a directory; block the store again with a file.
    fs::remove_dir_all(&blocker).unwrap();
    fs::write(&blocker, b"again").unwrap();
    writer.append(event("c"));
    assert!(writer.flush(Duration::from_secs(10)));
    let status = writer.failure_status();
    assert_eq!(status.reports, 3, "run one, recovery, run two");
    assert_eq!(status.recoveries, 1);
    let _ = fs::remove_dir_all(&dir);
}
