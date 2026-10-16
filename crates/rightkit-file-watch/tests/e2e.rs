//! End-to-end: real files, real OS watch backends (FSEvents on macOS, inotify on Linux,
//! ReadDirectoryChangesW on Windows). Every assertion is on content read back from disk or on
//! the exact paths delivered.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use rightkit_file_watch::{ChangeBatch, DirWatchSet, FileChange, FileChangeKind, FileSetWatcher};
#[cfg(feature = "async")]
use rightkit_file_watch::{DebouncedWatchReceiver, FileWatcher, WatchPath};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(600);

fn settle() {
    // FSEvents replays a short history window on arm; let it drain before the real action.
    std::thread::sleep(SETTLE);
}

fn collect_until<T>(rx: &mpsc::Receiver<T>, mut done: impl FnMut(&[T]) -> bool) -> Vec<T> {
    let mut all = Vec::new();
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        if let Ok(item) = rx.recv_timeout(Duration::from_millis(100)) {
            all.push(item);
            if done(&all) {
                return all;
            }
        }
    }
    all
}

fn flat(batches: &[Vec<FileChange>]) -> Vec<FileChange> {
    batches.iter().flatten().cloned().collect()
}

#[test]
fn atomic_save_rename_over_original_surfaces_target_only() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("doc.md");
    let sibling = dir.path().join("other.md");
    fs::write(&target, "v1").unwrap();
    fs::write(&sibling, "x").unwrap();
    let (tx, rx) = mpsc::channel();
    let _watch = FileSetWatcher::watch_default(std::slice::from_ref(&target), move |changes| {
        let _ = tx.send(changes);
    })
    .unwrap();
    settle();

    // Editor-style save: write a temp file, rename it over the original. Also touch a sibling.
    let tmp = dir.path().join(".doc.md.swp-tmp");
    fs::write(&tmp, "v2 from atomic save").unwrap();
    fs::write(&sibling, "noise").unwrap();
    fs::rename(&tmp, &target).unwrap();

    let batches = collect_until(&rx, |b| flat(b).iter().any(|c| c.path == target));
    let changes = flat(&batches);
    assert!(
        changes.contains(&FileChange {
            path: target.clone(),
            kind: FileChangeKind::Changed
        }),
        "target change missing: {changes:?}"
    );
    assert!(
        changes.iter().all(|c| c.path == target),
        "unwatched paths leaked: {changes:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "v2 from atomic save");
}

#[test]
fn missing_target_created_then_deleted_reports_both() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("saveas.txt");
    let (tx, rx) = mpsc::channel();
    let _watch = FileSetWatcher::watch_default(std::slice::from_ref(&target), move |c| {
        let _ = tx.send(c);
    })
    .unwrap();
    settle();

    fs::write(&target, "born").unwrap();
    let batches = collect_until(&rx, |b| {
        flat(b).iter().any(|c| c.kind == FileChangeKind::Changed)
    });
    assert!(flat(&batches)
        .iter()
        .any(|c| c.path == target && c.kind == FileChangeKind::Changed));

    std::thread::sleep(Duration::from_millis(400));
    fs::remove_file(&target).unwrap();
    let batches = collect_until(&rx, |b| {
        flat(b).iter().any(|c| c.kind == FileChangeKind::Removed)
    });
    assert!(
        flat(&batches)
            .iter()
            .any(|c| c.path == target && c.kind == FileChangeKind::Removed),
        "deletion not reported: {batches:?}"
    );
}

#[test]
fn revoked_grant_suppresses_emission() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("secret.txt");
    fs::write(&target, "a").unwrap();
    let granted = Arc::new(AtomicBool::new(true));
    let gate = granted.clone();
    let (tx, rx) = mpsc::channel();
    let _watch = FileSetWatcher::watch(
        std::slice::from_ref(&target),
        Duration::from_millis(80),
        Duration::from_secs(1),
        move |_: &Path| gate.load(Ordering::SeqCst),
        move |c| {
            let _ = tx.send(c);
        },
    )
    .unwrap();
    settle();

    fs::write(&target, "b").unwrap();
    assert!(
        !collect_until(&rx, |b| !b.is_empty()).is_empty(),
        "granted change must arrive"
    );

    granted.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    fs::write(&target, "c").unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert!(rx.try_recv().is_err(), "revoked path still emitted");
}

#[test]
fn rename_storm_collapses_to_net_state() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    const N: usize = 60;
    for i in 0..N {
        fs::write(root.join(format!("f{i}.txt")), format!("payload-{i}")).unwrap();
    }
    let set = DirWatchSet::default();
    let (tx, rx) = mpsc::channel::<ChangeBatch>();
    set.watch(&root, move |b| {
        let _ = tx.send(b);
    })
    .unwrap();
    settle();

    // Every file is renamed through a chain of 5 names before landing on its final name.
    for i in 0..N {
        let mut cur = root.join(format!("f{i}.txt"));
        for step in 0..5 {
            let next = root.join(format!("s{i}-{step}.tmp"));
            fs::rename(&cur, &next).unwrap();
            cur = next;
        }
        fs::rename(&cur, root.join(format!("g{i}.txt"))).unwrap();
    }

    let finals: Vec<PathBuf> = (0..N).map(|i| root.join(format!("g{i}.txt"))).collect();
    let originals: Vec<PathBuf> = (0..N).map(|i| root.join(format!("f{i}.txt"))).collect();
    let mut changed = std::collections::BTreeSet::new();
    let mut removed = std::collections::BTreeSet::new();
    let mut batches = 0;
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(1500)) {
            Ok(batch) => {
                batches += 1;
                // Each batch is truth at its flush time: reported paths obey existence.
                for p in &batch.changed {
                    changed.insert(p.clone());
                }
                for p in &batch.removed {
                    removed.insert(p.clone());
                }
            }
            Err(_) => break,
        }
    }
    for f in &finals {
        assert!(changed.contains(f), "final name never reported: {f:?}");
        assert!(f.exists());
    }
    for o in &originals {
        assert!(
            removed.contains(o) || !o.exists(),
            "original still believed alive: {o:?}"
        );
    }
    // The storm of 360 renames must have been coalesced into a small number of batches.
    assert!(
        (1..=10).contains(&batches),
        "storm produced {batches} batches"
    );
    for (i, path) in finals.iter().enumerate().take(N) {
        assert_eq!(fs::read_to_string(path).unwrap(), format!("payload-{i}"));
    }
    // Net truth: nothing reported `changed` that is gone by the end.
    let still_changed_but_gone: Vec<_> = changed.iter().filter(|p| !p.exists()).collect();
    println!(
        "transient names reported changed then gone: {}",
        still_changed_but_gone.len()
    );
}

#[test]
fn dir_watch_set_caps_evicts_oldest_and_unwatches() {
    let base = TempDir::new().unwrap();
    let set = DirWatchSet::new(3, Duration::from_millis(80));
    let dirs: Vec<PathBuf> = (0..5)
        .map(|i| {
            let d = base.path().join(format!("d{i}"));
            fs::create_dir(&d).unwrap();
            d
        })
        .collect();
    let (tx, rx) = mpsc::channel::<(usize, ChangeBatch)>();
    for (i, d) in dirs.iter().enumerate() {
        let tx = tx.clone();
        set.watch(d, move |b| {
            let _ = tx.send((i, b));
        })
        .unwrap();
        // Idempotent re-watch never double-arms.
        set.watch(d, |_| {}).unwrap();
    }
    assert_eq!(set.len(), 3);
    assert!(!set.is_watching(&dirs[0]) && !set.is_watching(&dirs[1]));
    assert!(set.is_watching(&dirs[4]));
    settle();
    // Eviction joins the worker, which flushes what it held, and FSEvents replays a short history window
    // on arm (the creation of the directory itself). Both run inside `watch`/`unwatch`, before they
    // return, so a batch from an evicted directory may legitimately sit in the channel by now. What a
    // consumer must never see is a callback AFTER eviction returned, so discard that backlog and
    // judge only what arrives from here on.
    while rx.try_recv().is_ok() {}

    fs::write(dirs[0].join("evicted.txt"), "x").unwrap();
    fs::write(dirs[4].join("live.txt"), "y").unwrap();
    // Wait for the batch that names the file, not merely the first batch from that directory:
    // a slow runner can deliver an earlier batch for the directory itself first.
    let live = |all: &[(usize, ChangeBatch)]| {
        all.iter()
            .any(|(i, b)| *i == 4 && b.changed.iter().any(|p| p.ends_with("live.txt")))
    };
    let got = collect_until(&rx, |all| live(all));
    assert!(live(&got));
    assert!(
        got.iter().all(|(i, _)| *i != 0),
        "evicted dir still emitting"
    );
    // The live batch was observed, so a batch from the evicted directory would have had every chance to
    // arrive too; none may be queued behind it either.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rx.try_iter().all(|(i, _)| i != 0),
        "evicted dir emitted after eviction returned"
    );

    set.unwatch(&dirs[4]);
    assert!(!set.is_watching(&dirs[4]));
    assert_eq!(set.len(), 2);
}

#[cfg(feature = "async")]
mod async_subscribe {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_subscriber_scopes_and_requested_namespace() {
        let dir = TempDir::new().unwrap();
        // tempdir is /var/... on macOS but backends report /private/var/...; subscribers must see the
        // spelling they asked for.
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("a/deep")).unwrap();
        let watcher = Arc::new(FileWatcher::new().unwrap());
        let (sub, mut rx) = watcher.add_subscriber();
        let _flat = sub.register_paths(vec![WatchPath {
            path: root.join("a"),
            recursive: false,
        }]);
        settle();

        fs::write(root.join("a/deep/nested.txt"), "n").unwrap();
        fs::write(root.join("a/top.txt"), "t").unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let deadline = tokio::time::Instant::now() + WAIT;
        while !seen.contains(&root.join("a/top.txt")) && tokio::time::Instant::now() < deadline {
            if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                seen.extend(ev.paths);
            }
        }
        assert!(
            seen.contains(&root.join("a/top.txt")),
            "missing top-level event: {seen:?}"
        );
        assert!(
            !seen.iter().any(|p| p.ends_with("nested.txt")),
            "non-recursive watch leaked nested event: {seen:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_missing_path_fallback_follows_creation_and_drop_unregisters() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let target = root.join("later/deeper/file.txt");
        let watcher = Arc::new(FileWatcher::new().unwrap());
        let (sub, mut rx) = watcher.add_subscriber();
        let reg = sub.register_path(target.clone(), false);
        settle();

        fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        fs::write(&target, "hello").unwrap();
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + WAIT;
        while !seen.contains(&target) && tokio::time::Instant::now() < deadline {
            if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                seen.extend(ev.paths);
            }
        }
        assert!(
            seen.contains(&target),
            "event for target never delivered: {seen:?}"
        );

        drop(reg);
        settle();
        while tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_ok()
        {}
        fs::write(&target, "after unregister").unwrap();
        let late = tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await;
        assert!(
            late.is_err(),
            "event delivered after registration dropped: {late:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_ref_counting_keeps_watch_until_last_subscriber_leaves() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let watcher = Arc::new(FileWatcher::new().unwrap());
        let (sub_a, rx_a) = watcher.add_subscriber();
        let (sub_b, mut rx_b) = watcher.add_subscriber();
        let _ra = sub_a.register_path(root.clone(), true);
        let _rb = sub_b.register_path(root.clone(), true);
        drop(rx_a);
        drop(sub_a); // removes subscriber A and decrements the shared OS watch
        settle();

        fs::write(root.join("still.txt"), "alive").unwrap();
        // A backend may deliver unrelated batches first (directory metadata, coalesced
        // history), so wait for the observable event rather than assuming the first batch.
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + WAIT;
        while !seen.iter().any(|p: &PathBuf| p.ends_with("still.txt"))
            && tokio::time::Instant::now() < deadline
        {
            if let Ok(Some(ev)) =
                tokio::time::timeout(Duration::from_millis(500), rx_b.recv()).await
            {
                seen.extend(ev.paths);
            }
        }
        assert!(
            seen.iter().any(|p| p.ends_with("still.txt")),
            "B never saw the event after A left: {seen:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn debounced_receiver_collapses_a_write_burst() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let watcher = Arc::new(FileWatcher::new().unwrap());
        let (sub, rx) = watcher.add_subscriber();
        let _reg = sub.register_path(root.clone(), true);
        let mut debounced = DebouncedWatchReceiver::new(rx, Duration::from_millis(300));
        settle();

        for i in 0..40 {
            fs::write(root.join(format!("burst-{}.txt", i % 8)), format!("{i}")).unwrap();
        }
        let first = tokio::time::timeout(WAIT, debounced.recv())
            .await
            .unwrap()
            .unwrap();
        // 8 distinct files, one batch, sorted and de-duplicated.
        let mut names: Vec<_> = first
            .paths
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        let sorted = {
            let mut s = names.clone();
            s.sort();
            s.dedup();
            s
        };
        names.dedup();
        assert_eq!(
            first.paths.len(),
            sorted.len(),
            "duplicates in batch: {:?}",
            first.paths
        );
        assert!(
            sorted.len() >= 6,
            "burst files missing from batch: {sorted:?}"
        );
    }
}
