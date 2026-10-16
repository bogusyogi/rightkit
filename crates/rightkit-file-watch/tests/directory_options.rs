//! Native directory arming & callback coverage; delivery policy is injected in options.rs.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rightkit_file_watch::{DirWatchSet, EventFilter, WatchEvent, WatchEventKind, WatchOptions};

#[test]
fn directory_arm_failures_are_strict_by_default_or_delivered_when_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().canonicalize().unwrap().join("missing");
    let strict = DirWatchSet::with_options(2, false, WatchOptions::default());
    assert!(strict.watch_records(&missing, |_| {}).is_err());
    assert!(!strict.is_watching(&missing));

    let tolerant = DirWatchSet::with_options(
        2,
        false,
        WatchOptions {
            tolerate_arm_failures: true,
            filter: EventFilter::new([]),
            ..WatchOptions::default()
        },
    );
    let (tx, rx) = mpsc::channel();
    tolerant
        .watch_records(&missing, move |records| {
            let _ = tx.send(records);
        })
        .unwrap();
    let records = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(records.iter().any(|record| match &record.event {
        WatchEvent::Error(rightkit_file_watch::WatchError::Backend { paths, .. }) =>
            paths.contains(&missing),
        _ => false,
    }));
    tolerant.unwatch(&missing);
    assert!(tolerant.is_empty());
}

#[test]
fn directory_options_observe_native_changes_and_remain_idempotent_and_capped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let set = DirWatchSet::with_options(
        1,
        false,
        WatchOptions {
            coalesce: false,
            max_wait: None,
            filter: EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]),
            ..WatchOptions::default()
        },
    );
    let (tx, rx) = mpsc::channel();
    set.watch_records(&first, move |records| {
        let _ = tx.send(records);
    })
    .unwrap();
    set.watch_records(&first, |_| {
        panic!("duplicate registration replaced callback")
    })
    .unwrap();
    assert_eq!(set.len(), 1);
    let file = first.join("document");
    std::fs::write(&file, "payload").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "no native directory hint arrived"
        );
        if let Ok(records) = rx.recv_timeout(Duration::from_millis(100)) {
            if records.iter().any(|record| match &record.event {
                WatchEvent::Create(path) | WatchEvent::Modify(path) => path == &file,
                _ => false,
            }) {
                break;
            }
        }
    }
    set.watch_records(&second, |_| {}).unwrap();
    assert!(!set.is_watching(&first));
    assert!(set.is_watching(&second));
    set.unwatch(&second);
    assert!(set.is_empty());
}
