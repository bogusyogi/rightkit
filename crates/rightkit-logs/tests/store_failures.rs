//! `JsonlStore` failure handling: best-effort appends are counted (never printed), and a failed
//! rotation still appends to the current file up to a hard cap, with the failure reported.

use rightkit_logs::{EventRecord, JsonlStore, Level};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "rightkit-logs-store-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn event(name: &str) -> EventRecord {
    EventRecord::new(1, Level::Info, name)
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

#[test]
fn a_failed_best_effort_append_is_counted_with_its_last_error() {
    let dir = temp_dir("append-fails");
    // A regular file where the store needs a directory: every append fails.
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    let store = JsonlStore::new(blocker.join("events.jsonl"), 0);

    assert_eq!(store.status().failed_appends, 0);
    store.append_best_effort(&event("first"));
    store.append_best_effort(&event("second"));

    let status = store.status();
    assert_eq!(status.failed_appends, 2);
    assert_eq!(status.failed_rotations, 0);
    let last = status.last_error.expect("the last failure is kept");
    assert!(
        last.contains("second"),
        "last error names the event: {last}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_rotation_still_appends_to_the_current_file_and_is_reported() {
    let dir = temp_dir("rotation-fails");
    let path = dir.join("events.jsonl");
    let store = JsonlStore::new(&path, 40);
    store.append(&event("before")).unwrap();
    assert!(
        fs::metadata(&path).unwrap().len() >= 40,
        "file is over the rotation size"
    );

    // The rotation target cannot be removed or replaced: it is a non-empty directory.
    let predecessor = dir.join("events.jsonl.1");
    fs::create_dir_all(&predecessor).unwrap();
    fs::write(predecessor.join("keep"), b"x").unwrap();

    store.append_best_effort(&event("after-failed-rotation"));

    let names: Vec<String> = lines(&path)
        .iter()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        names,
        ["before", "after-failed-rotation"],
        "the event is not dropped"
    );
    let status = store.status();
    assert_eq!(status.failed_rotations, 1);
    assert_eq!(status.failed_appends, 0);
    assert!(status.last_error.unwrap().contains("rotate"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_rotation_that_keeps_failing_is_bounded_at_twice_the_rotation_size() {
    let dir = temp_dir("rotation-bound");
    let path = dir.join("events.jsonl");
    let max = 200u64;
    let store = JsonlStore::new(&path, max);
    let predecessor = dir.join("events.jsonl.1");
    fs::create_dir_all(&predecessor).unwrap();
    fs::write(predecessor.join("keep"), b"x").unwrap();

    for i in 0..200 {
        store.append_best_effort(&event(&format!("e{i}")));
    }

    let size = fs::metadata(&path).unwrap().len();
    let one_line = (serde_json::to_string(&event("e199")).unwrap().len() + 1) as u64;
    assert!(
        size < 2 * max + one_line,
        "file stays under the hard cap of 2 x {max} bytes plus one line, was {size}"
    );
    let status = store.status();
    assert!(
        status.failed_appends > 0,
        "refused appends are counted, not silent"
    );
    assert!(status.failed_rotations > 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_successful_rotation_reports_no_failure() {
    let dir = temp_dir("rotation-ok");
    let path = dir.join("events.jsonl");
    let store = JsonlStore::new(&path, 40);
    store.append(&event("one")).unwrap();
    store.append(&event("two")).unwrap();
    assert!(dir.join("events.jsonl.1").exists());
    assert_eq!(store.status(), Default::default());
    let _ = fs::remove_dir_all(&dir);
}
