//! Native Windows/ReadDirectoryChangesW & macOS/FSEvents coverage. Assert sets,
//! not backend event counts/order; deterministic duplicate/timing tests live in src/options.rs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use rightkit_file_watch::{
    EventFilter, FileSetReceiver, FileSetWatcher, TypedWatcher, WatchEvent, WatchEventKind,
    WatchEventRecord, WatchOptions,
};

const WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
const QUIET: Duration = Duration::from_millis(1500);

fn allow(_: &Path) -> bool {
    true
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => parent.canonicalize().unwrap().join(name),
            _ => path.to_path_buf(),
        })
}

fn quiet(rx: &FileSetReceiver) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "backend never became quiet");
        match rx.recv_timeout(QUIET) {
            Ok(records) => assert!(
                !records
                    .iter()
                    .any(|record| matches!(record.event, WatchEvent::Error(_))),
                "{records:?}"
            ),
            Err(RecvTimeoutError::Timeout) => return,
            Err(error) => panic!("unexpected disconnect: {error}"),
        }
    }
}

fn poll(rx: &FileSetReceiver, done: impl Fn(&[WatchEventRecord]) -> bool) -> Vec<WatchEventRecord> {
    let mut records = Vec::new();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(batch) => records.extend(batch),
            Err(RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("unexpected disconnect: {error}"),
        }
        if done(&records) {
            return records;
        }
    }
    panic!("timed out; observed {records:?}");
}

fn kinds(records: &[WatchEventRecord]) -> BTreeSet<(PathBuf, WatchEventKind)> {
    let mut seen = BTreeSet::new();
    for record in records {
        let (path, kind) = match &record.event {
            WatchEvent::Create(path) => (path, WatchEventKind::Create),
            WatchEvent::Modify(path) => (path, WatchEventKind::Modify),
            WatchEvent::Delete(path) => (path, WatchEventKind::Delete),
            WatchEvent::Rename { from, to } => {
                seen.insert((canonical(from), WatchEventKind::Rename));
                (to, WatchEventKind::Rename)
            }
            WatchEvent::Error(error) => panic!("unexpected backend error: {error}"),
            WatchEvent::Overflow => continue,
        };
        seen.insert((canonical(path), kind));
    }
    seen
}

#[test]
fn native_filter_keeps_chosen_kinds_and_raw_kind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("target");
    let options = WatchOptions {
        filter: EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]),
        ..WatchOptions::default()
    };
    let (_watch, rx) =
        FileSetWatcher::watch_with_options(std::slice::from_ref(&path), options, allow).unwrap();
    quiet(&rx);
    fs::write(&path, "created").unwrap();
    let records = poll(&rx, |records| {
        kinds(records).iter().any(|(seen, _)| seen == &path)
    });
    assert!(kinds(&records)
        .iter()
        .all(|(_, kind)| matches!(kind, WatchEventKind::Create | WatchEventKind::Modify)));
    // FSEvents parent-only hints also retain their triggering backend kind.
    assert!(records.iter().any(|record| record.raw_kind.is_some()
        && matches!(record.event, WatchEvent::Create(_) | WatchEvent::Modify(_))));
    quiet(&rx);
    fs::remove_file(&path).unwrap();
    let deadline = Instant::now() + QUIET * 2;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(records) => assert!(
                kinds(&records).is_empty(),
                "excluded deletion leaked: {records:?}"
            ),
            Err(RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("unexpected disconnect: {error}"),
        }
    }
}

#[test]
fn native_arm_failure_is_reported_while_other_parent_stays_watched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let good = root.join("good");
    let bad_parent = root.join("missing-parent");
    let bad = bad_parent.join("bad");
    assert!(FileSetWatcher::watch_with_options(
        &[good.clone(), bad.clone()],
        WatchOptions::default(),
        allow
    )
    .is_err());
    let (_watch, rx) = FileSetWatcher::watch_with_options(
        &[good.clone(), bad],
        WatchOptions {
            tolerate_arm_failures: true,
            ..WatchOptions::default()
        },
        allow,
    )
    .unwrap();
    let failures = poll(&rx, |records| {
        records
            .iter()
            .any(|record| matches!(record.event, WatchEvent::Error(_)))
    });
    assert!(failures.iter().any(|record| match &record.event {
        WatchEvent::Error(rightkit_file_watch::WatchError::Backend { paths, .. }) =>
            paths.contains(&bad_parent),
        _ => false,
    }));
    quiet(&rx);
    fs::write(&good, "still watched").unwrap();
    let records = poll(&rx, |records| {
        kinds(records).iter().any(|(path, _)| path == &good)
    });
    assert!(kinds(&records).iter().any(|(path, kind)| path == &good
        && matches!(kind, WatchEventKind::Create | WatchEventKind::Modify)));
}

#[test]
fn native_all_arm_failures_can_be_tolerated_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("missing")
        .join("file");
    let (_watch, rx) = FileSetWatcher::watch_with_options(
        &[missing],
        WatchOptions {
            tolerate_arm_failures: true,
            ..WatchOptions::default()
        },
        allow,
    )
    .unwrap();
    let records = poll(&rx, |records| {
        records
            .iter()
            .any(|record| matches!(record.event, WatchEvent::Error(_)))
    });
    assert!(records
        .iter()
        .any(|record| matches!(record.event, WatchEvent::Error(_))));
}

#[test]
fn native_receiver_disconnect_stops_live_backend_without_another_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("target");
    let (watch, rx) = FileSetWatcher::watch_with_options(
        &[path],
        WatchOptions {
            quiet: Duration::from_secs(60),
            discard_on_disconnect: true,
            ..WatchOptions::default()
        },
        allow,
    )
    .unwrap();
    drop(rx);
    let deadline = Instant::now() + WAIT;
    while !watch.is_finished() {
        assert!(Instant::now() < deadline, "disconnected watch did not stop");
        std::thread::sleep(POLL);
    }
    drop(watch); // Joins an already stopped worker.
}

#[test]
fn native_uncoalesced_stream_observes_each_staged_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths: Vec<_> = (0..3).map(|i| root.join(format!("target-{i}"))).collect();
    let (_watch, rx) = FileSetWatcher::watch_with_options(
        &paths,
        WatchOptions {
            coalesce: false,
            ..WatchOptions::default()
        },
        allow,
    )
    .unwrap();
    quiet(&rx);
    let mut seen = BTreeSet::new();
    for path in &paths {
        fs::write(path, "created").unwrap();
        let records = poll(&rx, |records| {
            kinds(records).iter().any(|(seen, kind)| {
                seen == path && matches!(kind, WatchEventKind::Create | WatchEventKind::Modify)
            })
        });
        assert!(records.iter().all(|record| record.raw_kind.is_some()));
        seen.extend(kinds(&records));
        quiet(&rx); // FSEvents may merge writes; separate stages with a quiet window.
    }
    let targets: BTreeSet<_> = seen.into_iter().map(|(path, _)| path).collect();
    assert_eq!(targets, paths.into_iter().collect());
}

#[test]
fn typed_record_api_preserves_backend_kind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let watch = TypedWatcher::watch(&root).unwrap();
    // Drain FSEvents registration history with a generous quiet window.
    let settle_deadline = Instant::now() + WAIT;
    loop {
        assert!(
            Instant::now() < settle_deadline,
            "backend never became quiet"
        );
        match watch.recv_record_timeout(QUIET) {
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => break,
            Err(error) => panic!("unexpected disconnect: {error}"),
        }
    }
    let target = root.join("typed-target");
    fs::write(&target, "created").unwrap();
    let deadline = Instant::now() + WAIT;
    let mut seen = BTreeSet::new();
    while Instant::now() < deadline {
        match watch.recv_record_timeout(POLL) {
            Ok(record) => {
                if matches!(&record.event, WatchEvent::Create(path) | WatchEvent::Modify(path) if canonical(path) == target)
                {
                    assert!(record.raw_kind.is_some());
                    seen.extend(kinds(&[record]));
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("unexpected disconnect: {error}"),
        }
    }
    assert!(
        seen.iter().any(|(path, kind)| path == &target
            && matches!(kind, WatchEventKind::Create | WatchEventKind::Modify)),
        "{seen:?}"
    );
}
