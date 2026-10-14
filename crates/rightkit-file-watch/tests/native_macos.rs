//! macOS native backend: cursor replay (4a), latency (4b) and directory granularity (4c).
//! Real FSEvents on the real filesystem.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_file_watch::notify::EventKind;
use rightkit_file_watch::{
    Granularity, TypedOptions, TypedWatcher, WatchBackend, WatchCursor, WatchEvent,
    WatchEventRecord,
};

const WAIT: Duration = Duration::from_secs(30);

fn names(records: &[WatchEventRecord]) -> Vec<String> {
    records
        .iter()
        .flat_map(|r| match &r.event {
            WatchEvent::Create(p) | WatchEvent::Modify(p) | WatchEvent::Delete(p) => vec![p],
            WatchEvent::Rename { from, to } => vec![from, to],
            _ => vec![],
        })
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Receives until every wanted file name has appeared, then until replay has finished.
fn collect(watch: &TypedWatcher, wanted: &[&str]) -> Vec<WatchEventRecord> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while !wanted.iter().all(|w| names(&seen).iter().any(|n| n == w)) {
        let left = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("missing {wanted:?}; saw {seen:#?}"));
        seen.push(watch.recv_record_timeout(left).unwrap_or_else(|e| {
            panic!("{e}: missing {wanted:?}; saw {seen:#?}");
        }));
    }
    while watch.is_replaying() {
        assert!(Instant::now() < deadline, "history never finished");
        if let Ok(record) = watch.recv_record_timeout(Duration::from_millis(100)) {
            seen.push(record);
        }
    }
    while let Ok(record) = watch.recv_record_timeout(Duration::from_millis(500)) {
        seen.push(record);
    }
    seen
}

fn replay_options(cursor: WatchCursor) -> TypedOptions {
    TypedOptions::new()
        .backend(WatchBackend::Native)
        .replay_from(cursor)
}

#[test]
fn cursor_replays_history_then_resumes_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    let start = TypedWatcher::current_cursor(&root).expect("a cursor on a local volume");
    let bytes = start.to_bytes();
    assert_eq!(bytes.len(), WatchCursor::ENCODED_LEN);
    assert_eq!(WatchCursor::from_bytes(&bytes), Some(start));
    assert_eq!(WatchCursor::from_bytes(&bytes[..24]), None);
    let mut wrong_version = bytes;
    wrong_version[0] ^= 0xff;
    assert_eq!(WatchCursor::from_bytes(&wrong_version), None);

    // Run 1: A and B happen with no watcher at all; the cursor brings them back.
    std::fs::write(root.join("A.txt"), b"a").unwrap();
    std::fs::write(root.join("B.txt"), b"b").unwrap();
    let first = TypedWatcher::watch_with(&root, replay_options(start)).unwrap();
    let seen = collect(&first, &["A.txt", "B.txt"]);
    assert!(!first.is_replaying());
    assert!(
        !seen.iter().any(|r| r.event == WatchEvent::Overflow),
        "{seen:#?}"
    );
    let after_first = first.cursor().expect("cursor after receiving records");
    assert!(
        after_first.event_id() > start.event_id(),
        "{after_first:?} vs {start:?}"
    );
    assert_eq!(
        WatchCursor::from_bytes(&after_first.to_bytes()),
        Some(after_first),
        "byte round-trip"
    );
    drop(first);

    // Run 2: restart from the persisted cursor; only C is new.
    std::fs::write(root.join("C.txt"), b"c").unwrap();
    let second = TypedWatcher::watch_with(&root, replay_options(after_first)).unwrap();
    let seen = collect(&second, &["C.txt"]);
    let got = names(&seen);
    assert!(
        !got.iter().any(|n| n == "A.txt" || n == "B.txt"),
        "replayed old events: {got:?}"
    );
    assert!(second.cursor().unwrap().event_id() > after_first.event_id());
    drop(second);

    // Run 3: a cursor from another volume cannot be honoured: Overflow first, then live.
    let mut corrupted = after_first.to_bytes();
    corrupted[1] ^= 0xff;
    let foreign = WatchCursor::from_bytes(&corrupted).unwrap();
    let third = TypedWatcher::watch_with(&root, replay_options(foreign)).unwrap();
    let head = third.recv_record_timeout(WAIT).unwrap();
    assert_eq!(head.event, WatchEvent::Overflow);
    assert!(!third.is_replaying());
    std::fs::write(root.join("D.txt"), b"d").unwrap();
    assert!(names(&collect(&third, &["D.txt"]))
        .iter()
        .any(|n| n == "D.txt"));
    drop(third);

    // A cursor from the future is refused the same way.
    let mut future = after_first.to_bytes();
    future[17..25].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
    let future = WatchCursor::from_bytes(&future).unwrap();
    let fourth = TypedWatcher::watch_with(&root, replay_options(future)).unwrap();
    assert_eq!(
        fourth.recv_record_timeout(WAIT).unwrap().event,
        WatchEvent::Overflow
    );
    assert!(!fourth.is_replaying());
}

#[test]
fn latency_delays_the_first_record() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let watch = TypedWatcher::watch_with(
        &root,
        TypedOptions::new()
            .backend(WatchBackend::Native)
            .latency(Duration::from_secs(1)),
    )
    .unwrap();
    std::fs::write(root.join("slow.txt"), b"x").unwrap();
    assert!(
        watch
            .recv_record_timeout(Duration::from_millis(500))
            .is_err(),
        "a 1 s latency must hold the first record back"
    );
    let record = watch
        .recv_record_timeout(Duration::from_secs(10))
        .expect("a record within 10 s");
    assert!(
        matches!(record.event, WatchEvent::Create(_) | WatchEvent::Modify(_)),
        "{record:?}"
    );
}

#[test]
fn directory_granularity_reports_modified_directories_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let watch = TypedWatcher::watch_with(
        &root,
        TypedOptions::new()
            .backend(WatchBackend::Native)
            .granularity(Granularity::Directory),
    )
    .unwrap();
    for i in 0..200 {
        std::fs::write(sub.join(format!("f{i}.txt")), b"x").unwrap();
    }
    let mut records = Vec::new();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match watch.recv_record_timeout(Duration::from_secs(2)) {
            Ok(record) => records.push(record),
            Err(_) if !records.is_empty() => break,
            Err(_) => {}
        }
    }
    assert!(!records.is_empty(), "no directory events");
    let mut dirs: Vec<PathBuf> = Vec::new();
    for record in &records {
        let WatchEvent::Modify(path) = &record.event else {
            panic!("directory granularity yields only Modify, got {record:?}");
        };
        assert!(
            path.is_dir(),
            "{} is not an existing directory",
            path.display()
        );
        assert_eq!(record.raw_kind, Some(EventKind::Any), "{record:?}");
        dirs.push(path.clone());
    }
    assert!(dirs.contains(&sub), "{dirs:?}");
    assert!(
        records.len() < 200,
        "{} records for 200 files",
        records.len()
    );
}

/// `/` with exclusions for ten seconds: a smoke run for noisy hosts, not a CI gate.
#[test]
#[ignore = "watches the whole filesystem; run by hand"]
fn watching_the_root_with_exclusions_stays_bounded() {
    let excluded: [&Path; 3] = [
        Path::new("/System"),
        Path::new("/private/var"),
        Path::new("/Library"),
    ];
    let mut options = TypedOptions::new()
        .backend(WatchBackend::Native)
        .granularity(Granularity::Directory)
        .queue_capacity(std::num::NonZeroUsize::new(1024).unwrap());
    for prefix in excluded {
        options = options.exclude_prefix(prefix);
    }
    let watch = TypedWatcher::watch_with("/", options).unwrap();
    std::thread::sleep(Duration::from_secs(10));
    let metrics = watch.metrics();
    println!("{metrics:?}");
    assert!(metrics.queue_depth <= 1025);
    let mut seen = 0;
    while let Ok(record) = watch.try_recv_record() {
        if let WatchEvent::Modify(path) | WatchEvent::Create(path) = &record.event {
            assert!(
                !excluded.iter().any(|e| path.starts_with(e)),
                "{}",
                path.display()
            );
        }
        seen += 1;
    }
    println!("{seen} records");
}

/// Probe, not a gate: FSEvents' answer to a valid-volume cursor that predates retained history.
/// The header is silent on this case (it only says a different UUID voids stored ids), so the
/// outcome is printed for the record. Run by hand: `-- --ignored --nocapture`.
#[test]
#[ignore = "probe: prints how FSEvents answers a very old event id"]
fn probe_cursor_older_than_retained_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let now = TypedWatcher::current_cursor(&root).unwrap();
    let mut bytes = now.to_bytes();
    bytes[17..25].copy_from_slice(&1u64.to_le_bytes());
    let old = WatchCursor::from_bytes(&bytes).unwrap();
    let watch = TypedWatcher::watch_with(&root, replay_options(old)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut records = Vec::new();
    while watch.is_replaying() && Instant::now() < deadline {
        if let Ok(record) = watch.recv_record_timeout(Duration::from_millis(200)) {
            records.push(record);
        }
    }
    println!(
        "replaying_after_20s={} records={} overflow_seen={} first={:?}",
        watch.is_replaying(),
        records.len(),
        records.iter().any(|r| r.event == WatchEvent::Overflow),
        records.first()
    );
}
