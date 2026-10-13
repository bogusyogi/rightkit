//! Multi-generation rotation for `JsonlStore`, and redacted, prefixed stderr capture into a
//! rotating text log.

use rightkit_logs::{capture_stderr, EventRecord, JsonlStore, Level, RotatingTextWriter};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-logs-gen-{label}-{}-{}",
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

fn event(name: &str) -> EventRecord {
    EventRecord::new(1, Level::Info, name)
}

#[test]
fn default_store_keeps_exactly_one_predecessor() {
    let dir = temp_dir("default");
    let path = dir.join("events.jsonl");
    let store = JsonlStore::new(&path, 20);
    for i in 0..5 {
        store.append(&event(&format!("e{i}"))).unwrap();
    }
    assert!(dir.join("events.jsonl.1").exists());
    assert!(!dir.join("events.jsonl.2").exists());
}

#[test]
fn store_keeps_configured_generations_oldest_dropped() {
    let dir = temp_dir("n");
    let path = dir.join("events.jsonl");
    let store = JsonlStore::new(&path, 20).with_generations(3);
    for i in 0..6 {
        store.append(&event(&format!("e{i}"))).unwrap();
    }
    for n in 1..=3 {
        assert!(dir.join(format!("events.jsonl.{n}")).exists(), "gen {n}");
    }
    assert!(!dir.join("events.jsonl.4").exists());
    let newest = fs::read_to_string(dir.join("events.jsonl.1")).unwrap();
    let oldest = fs::read_to_string(dir.join("events.jsonl.3")).unwrap();
    assert!(newest.contains("e4"), "{newest}");
    assert!(oldest.contains("e2"), "{oldest}");
}

#[test]
fn clear_removes_every_generation() {
    let dir = temp_dir("clear");
    let path = dir.join("events.jsonl");
    let store = JsonlStore::new(&path, 20).with_generations(3);
    for i in 0..6 {
        store.append(&event(&format!("e{i}"))).unwrap();
    }
    store.clear().unwrap();
    for name in [
        "events.jsonl",
        "events.jsonl.1",
        "events.jsonl.2",
        "events.jsonl.3",
    ] {
        assert!(!dir.join(name).exists(), "{name}");
    }
}

#[test]
fn captured_stderr_is_prefixed_scrubbed_and_rotated() {
    let dir = temp_dir("stderr");
    let writer = Arc::new(RotatingTextWriter::new(dir.join("daemon.log"), 80).with_generations(2));
    let stream =
        b"boot at /Users/grace/work\nnotify grace@example.org\nplain line\nanother plain line\n";
    let capture =
        capture_stderr(Cursor::new(stream.to_vec()), writer.clone(), "[daemon] ").unwrap();
    assert_eq!(capture.join(), 4);
    let mut all = String::new();
    for name in ["daemon.log", "daemon.log.1", "daemon.log.2"] {
        all.push_str(&fs::read_to_string(dir.join(name)).unwrap_or_default());
    }
    assert!(!all.contains("grace@example.org"), "{all}");
    assert!(!all.contains("/Users/grace"), "{all}");
    assert!(all.contains("[daemon] plain line"), "{all}");
    assert!(dir.join("daemon.log.1").exists());
    assert_eq!(writer.status().failed_appends, 0);
}

#[test]
fn capture_survives_invalid_utf8_and_overlong_lines() {
    let dir = temp_dir("bytes");
    let writer = Arc::new(RotatingTextWriter::new(dir.join("x.log"), 0));
    let mut stream = vec![0xff, 0xfe, b'\n'];
    stream.extend(std::iter::repeat(b'a').take(70 * 1024));
    stream.push(b'\n');
    let capture = capture_stderr(Cursor::new(stream), writer, "> ").unwrap();
    assert_eq!(capture.join(), 3);
    assert!(fs::read_to_string(dir.join("x.log"))
        .unwrap()
        .starts_with("> "));
}
