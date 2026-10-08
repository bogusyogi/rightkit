//! Native watch tests: stage operations across backend quiet windows, poll with
//! generous deadlines, compare canonical paths, and tolerate coalesced kinds.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use rightkit_watch::{ChangeBatch, DirWatchSet, FileWatcher, TypedWatcher, WatchEvent};

const WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
const QUIET: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Create,
    Modify,
    Delete,
    Rename,
}

// Deleted names cannot canonicalize; their parents still resolve /var aliases.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => canonical(parent).join(name),
            _ => path.to_path_buf(),
        })
}

fn observe(event: WatchEvent, seen: &mut BTreeSet<(PathBuf, Kind)>) {
    match event {
        WatchEvent::Create(path) => {
            seen.insert((canonical(&path), Kind::Create));
        }
        WatchEvent::Modify(path) => {
            seen.insert((canonical(&path), Kind::Modify));
        }
        WatchEvent::Delete(path) => {
            seen.insert((canonical(&path), Kind::Delete));
        }
        WatchEvent::Rename { from, to } => {
            seen.insert((canonical(&from), Kind::Rename));
            seen.insert((canonical(&to), Kind::Rename));
        }
        WatchEvent::Error(error) => panic!("unexpected watch error: {error}"),
        WatchEvent::Overflow => panic!("unexpected backend rescan signal"),
    }
}

fn has(seen: &BTreeSet<(PathBuf, Kind)>, path: &Path, kinds: &[Kind]) -> bool {
    let path = canonical(path);
    kinds
        .iter()
        .any(|kind| seen.contains(&(path.clone(), *kind)))
}

fn poll_until(
    watcher: &TypedWatcher,
    done: impl Fn(&BTreeSet<(PathBuf, Kind)>) -> bool,
) -> BTreeSet<(PathBuf, Kind)> {
    let mut seen = BTreeSet::new();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match watcher.recv_timeout(POLL) {
            Ok(event) => observe(event, &mut seen),
            Err(RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("watch stream disconnected: {error}"),
        }
        if done(&seen) {
            return seen;
        }
    }
    panic!("timed out waiting for event kinds; observed: {seen:?}");
}

fn quiet(watcher: &TypedWatcher) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "backend never became quiet");
        match watcher.recv_timeout(QUIET) {
            Ok(event) => observe(event, &mut BTreeSet::new()),
            Err(RecvTimeoutError::Timeout) => return,
            Err(error) => panic!("watch stream disconnected: {error}"),
        }
    }
}

#[test]
fn typed_recursive_stream_observes_create_modify_rename_delete() {
    let dir = tempfile::tempdir().unwrap();
    // Register through the original spelling to exercise macOS temp-dir aliases.
    let watcher = FileWatcher::watch_typed(dir.path()).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let from = root.join("source.txt");
    let to = root.join("destination.txt");
    quiet(&watcher);

    fs::write(&from, "created").unwrap();
    let created = poll_until(&watcher, |seen| {
        has(seen, &from, &[Kind::Create, Kind::Modify])
    });
    assert!(has(&created, &from, &[Kind::Create, Kind::Modify]));
    quiet(&watcher);

    fs::write(&from, "modified with more bytes").unwrap();
    let modified = poll_until(&watcher, |seen| has(seen, &from, &[Kind::Modify]));
    assert!(has(&modified, &from, &[Kind::Modify]));
    quiet(&watcher);

    fs::rename(&from, &to).unwrap();
    // Windows/inotify pair names. FSEvents can emit Modify hints for both names
    // (or Delete/Create) without a pair. Require both endpoint sets, no ordering.
    let renamed = poll_until(&watcher, |seen| {
        has(seen, &from, &[Kind::Rename, Kind::Delete, Kind::Modify])
            && has(seen, &to, &[Kind::Rename, Kind::Create, Kind::Modify])
    });
    assert!(has(
        &renamed,
        &from,
        &[Kind::Rename, Kind::Delete, Kind::Modify]
    ));
    assert!(has(
        &renamed,
        &to,
        &[Kind::Rename, Kind::Create, Kind::Modify]
    ));
    assert_eq!(fs::read_to_string(&to).unwrap(), "modified with more bytes");
    quiet(&watcher);

    fs::remove_file(&to).unwrap();
    let deleted = poll_until(&watcher, |seen| has(seen, &to, &[Kind::Delete]));
    assert!(has(&deleted, &to, &[Kind::Delete]));
}

#[test]
fn typed_recursive_stream_observes_nested_file() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    let watcher = FileWatcher::watch_typed(dir.path()).unwrap();
    quiet(&watcher);
    let target = nested.join("child.txt");
    fs::write(&target, "nested").unwrap();
    let seen = poll_until(&watcher, |seen| {
        has(seen, &target, &[Kind::Create, Kind::Modify])
    });
    assert!(has(&seen, &target, &[Kind::Create, Kind::Modify]));
}

fn batches_until(rx: &mpsc::Receiver<ChangeBatch>, target: &Path) -> BTreeSet<PathBuf> {
    let deadline = Instant::now() + WAIT;
    let mut changed = BTreeSet::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(batch) => changed.extend(batch.changed.iter().map(|path| canonical(path))),
            Err(RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("directory watch disconnected: {error}"),
        }
        if changed.contains(&canonical(target)) {
            return changed;
        }
    }
    panic!(
        "nested path {} missing from changed set: {changed:?}",
        target.display()
    );
}

#[test]
fn recursive_directory_set_observes_existing_and_new_nested_directories() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("existing").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    let existing = nested.join("existing.txt");
    fs::write(&existing, "before watch").unwrap();
    let set = DirWatchSet::with_recursive(1, Duration::from_millis(200), true);
    let (tx, rx) = mpsc::channel();
    set.watch(dir.path(), move |batch| {
        let _ = tx.send(batch);
    })
    .unwrap();
    // Let FSEvents' registration history settle before mutating the file.
    std::thread::sleep(QUIET);
    while rx.try_recv().is_ok() {}

    fs::write(&existing, "after watch").unwrap();
    assert!(batches_until(&rx, &existing).contains(&canonical(&existing)));
    let new_dir = nested.join("created_after_registration");
    fs::create_dir(&new_dir).unwrap();
    // Waiting for its callback also lets inotify arm this new directory before
    // its child is created; writing immediately could race native registration.
    batches_until(&rx, &new_dir);
    let new_file = new_dir.join("new.txt");
    fs::write(&new_file, "new subtree").unwrap();
    assert!(batches_until(&rx, &new_file).contains(&canonical(&new_file)));
    assert_eq!(
        set.len(),
        1,
        "recursive descendants do not consume root capacity"
    );
}

#[test]
fn typed_start_returns_failure_for_unavailable_root() {
    let dir = tempfile::tempdir().unwrap();
    assert!(FileWatcher::watch_typed(dir.path().join("missing")).is_err());
}
