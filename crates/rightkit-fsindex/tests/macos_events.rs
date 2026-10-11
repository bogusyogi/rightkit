//! Public `FsEventsWatcher` against real directories. macOS only.
#![cfg(target_os = "macos")]

use rightkit_fsindex::{FsEvent, FsEventsWatcher, WatchFlags, WatchOptions};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn tree() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn file_events(latency_ms: u64) -> WatchOptions {
    WatchOptions {
        since_event_id: None,
        latency: Duration::from_millis(latency_ms),
        flags: WatchFlags {
            file_events: true,
            ..WatchFlags::default()
        },
    }
}

/// Waits up to `timeout` for the first event that satisfies `wanted`.
fn next_match(
    rx: &Receiver<FsEvent>,
    timeout: Duration,
    wanted: impl Fn(&FsEvent) -> bool,
) -> Option<FsEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(e) if wanted(&e) => return Some(e),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[test]
fn create_then_modify_are_reported_as_item_events() {
    let (_d, root) = tree();
    let (_w, rx) = FsEventsWatcher::start(std::slice::from_ref(&root), file_events(50)).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let file = root.join("a.txt");
    fs::write(&file, b"one").unwrap();
    let created = next_match(&rx, Duration::from_secs(15), |e| {
        matches!(e, FsEvent::Changed { path, item, .. } if *path == file && (item.created || item.modified))
    })
    .expect("create event for the new file");

    std::thread::sleep(Duration::from_millis(400));
    let mut f = OpenOptions::new().append(true).open(&file).unwrap();
    f.write_all(b"two").unwrap();
    f.sync_all().unwrap();
    drop(f);
    let modified = next_match(
        &rx,
        Duration::from_secs(15),
        |e| matches!(e, FsEvent::Changed { path, item, .. } if *path == file && item.modified),
    )
    .expect("modify event for the appended file");
    assert!(
        modified.id() > created.id(),
        "modify must come after create"
    );
}

#[test]
fn since_replays_changes_made_after_the_recorded_id() {
    let (_d, root) = tree();
    let before = FsEventsWatcher::current_event_id();
    let file = root.join("missed.txt");
    fs::write(&file, b"while nobody watched").unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let (w, rx) = FsEventsWatcher::start(
        std::slice::from_ref(&root),
        WatchOptions {
            since_event_id: Some(before),
            ..file_events(50)
        },
    )
    .unwrap();
    let replayed = next_match(
        &rx,
        Duration::from_secs(20),
        |e| matches!(e, FsEvent::Changed { path, .. } if *path == file),
    )
    .expect("the missed change is replayed");
    assert!(replayed.id() > before);
    assert!(
        next_match(&rx, Duration::from_secs(20), |e| matches!(
            e,
            FsEvent::HistoryDone { .. }
        ))
        .is_some(),
        "replay ends with HistoryDone"
    );
    assert!(
        w.last_event_id() >= replayed.id(),
        "the persisted id covers the replayed event"
    );
}

#[test]
fn resuming_from_the_current_id_replays_nothing_older() {
    let (_d, root) = tree();
    let file = root.join("old.txt");
    fs::write(&file, b"before the resume point").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let now = FsEventsWatcher::current_event_id();

    let (_w, rx) = FsEventsWatcher::start(
        std::slice::from_ref(&root),
        WatchOptions {
            since_event_id: Some(now),
            ..file_events(50)
        },
    )
    .unwrap();
    let stale = next_match(
        &rx,
        Duration::from_millis(1500),
        |e| matches!(e, FsEvent::Changed { path, .. } if *path == file),
    );
    assert!(stale.is_none(), "nothing before the resume id is replayed");
}

#[test]
fn latency_defers_delivery_until_the_window_passes() {
    let (_d, root) = tree();
    // Deferral is opt-in: `WatchFlags::default()` sets `no_defer`, which delivers the leading
    // edge at once. This test asserts the deferred window, so it clears that flag.
    let mut options = file_events(1500);
    options.flags.no_defer = false;
    let (_w, rx) = FsEventsWatcher::start(std::slice::from_ref(&root), options).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let file = root.join("slow.txt");
    let wrote = Instant::now();
    fs::write(&file, b"late").unwrap();
    let early = next_match(
        &rx,
        Duration::from_millis(400),
        |e| matches!(e, FsEvent::Changed { path, .. } if *path == file),
    );
    assert!(early.is_none(), "no delivery inside the latency window");
    let late = next_match(
        &rx,
        Duration::from_secs(15),
        |e| matches!(e, FsEvent::Changed { path, .. } if *path == file),
    );
    assert!(late.is_some(), "delivered after the window");
    assert!(wrote.elapsed() >= Duration::from_millis(1000));
}

#[test]
fn last_event_id_advances_with_delivered_events() {
    let (_d, root) = tree();
    let (w, rx) = FsEventsWatcher::start(std::slice::from_ref(&root), file_events(50)).unwrap();
    let start = w.last_event_id();
    std::thread::sleep(Duration::from_millis(300));
    let file = root.join("tick.txt");
    fs::write(&file, b"tick").unwrap();
    let ev = next_match(
        &rx,
        Duration::from_secs(15),
        |e| matches!(e, FsEvent::Changed { path, .. } if *path == file),
    )
    .expect("event");
    assert!(w.last_event_id() >= ev.id());
    assert!(w.last_event_id() > start);
}

#[test]
fn removing_a_watched_root_reports_root_changed() {
    let (_d, base) = tree();
    let root = base.join("watched");
    fs::create_dir(&root).unwrap();
    let (_w, rx) =
        FsEventsWatcher::start(std::slice::from_ref(&root), WatchOptions::default()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    fs::remove_dir_all(&root).unwrap();
    let ev = next_match(&rx, Duration::from_secs(15), |e| {
        matches!(e, FsEvent::RootChanged { .. })
    });
    assert!(ev.is_some(), "RootChanged for the removed root");
}

#[test]
fn dropping_the_watcher_disconnects_the_receiver() {
    let (_d, root) = tree();
    let (w, rx) = FsEventsWatcher::start(&[root], WatchOptions::default()).unwrap();
    drop(w);
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_err());
}

#[test]
fn empty_roots_and_nul_roots_are_rejected() {
    assert!(FsEventsWatcher::start(&[], WatchOptions::default()).is_err());
    let bad = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/has\0nul"));
    assert!(FsEventsWatcher::start(&[bad], WatchOptions::default()).is_err());
}
