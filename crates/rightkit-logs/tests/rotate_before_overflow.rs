//! `JsonlStore` rotate-before-overflow.

use rightkit_logs::JsonlStore;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-logs-overflow-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn line_len(value: &serde_json::Value) -> u64 {
    serde_json::to_vec(value).unwrap().len() as u64 + 1
}

#[test]
fn rotates_before_the_next_line_would_overflow() {
    let dir = temp_dir("before");
    let value = json!({"k": "0123456789"});
    let one = line_len(&value);
    // Room for two lines and a half: the third must rotate first.
    let store = JsonlStore::new(dir.join("events.jsonl"), one * 2 + one / 2)
        .with_rotate_before_overflow(true);
    for _ in 0..3 {
        store.append_value(&value).unwrap();
    }
    let current = fs::metadata(dir.join("events.jsonl")).unwrap().len();
    let rotated = fs::metadata(dir.join("events.jsonl.1")).unwrap().len();
    assert_eq!(rotated, one * 2);
    assert_eq!(current, one);
    assert!(rotated <= one * 2 + one / 2);
}

#[test]
fn default_rotates_only_after_the_limit_is_reached() {
    let dir = temp_dir("default");
    let value = json!({"k": "0123456789"});
    let one = line_len(&value);
    let store = JsonlStore::new(dir.join("events.jsonl"), one * 2 + one / 2);
    for _ in 0..3 {
        store.append_value(&value).unwrap();
    }
    assert!(!dir.join("events.jsonl.1").exists());
    assert_eq!(
        fs::metadata(dir.join("events.jsonl")).unwrap().len(),
        one * 3
    );
}

#[test]
fn append_bounded_rotates_whatever_the_setting() {
    let dir = temp_dir("bounded");
    let value = json!({"k": "0123456789"});
    let one = line_len(&value);
    let store = JsonlStore::new(dir.join("events.jsonl"), one + 1);
    store.append_bounded(&value).unwrap();
    store.append_bounded(&value).unwrap();
    assert_eq!(fs::metadata(dir.join("events.jsonl.1")).unwrap().len(), one);
    assert_eq!(fs::metadata(dir.join("events.jsonl")).unwrap().len(), one);
}

#[test]
fn a_single_oversized_line_is_written_alone() {
    let dir = temp_dir("oversize");
    let store = JsonlStore::new(dir.join("events.jsonl"), 8).with_rotate_before_overflow(true);
    let big = json!({"k": "a line far longer than eight bytes"});
    store.append_value(&big).unwrap();
    assert!(!dir.join("events.jsonl.1").exists());
    store.append_value(&big).unwrap();
    assert_eq!(
        fs::metadata(dir.join("events.jsonl.1")).unwrap().len(),
        line_len(&big)
    );
}

#[test]
fn zero_limit_never_rotates() {
    let dir = temp_dir("zero");
    let store = JsonlStore::new(dir.join("events.jsonl"), 0).with_rotate_before_overflow(true);
    for _ in 0..3 {
        store.append_value(&json!({"a": 1})).unwrap();
    }
    assert!(!dir.join("events.jsonl.1").exists());
}
