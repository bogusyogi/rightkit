//! rightkit-file-watch: a real OS watch on a scratch directory. Files are written, changed and removed; the debounced
//! batches (`DirWatchSet`) and the typed stream (`TypedWatcher`) must report them within a deadline, and a watch that was
//! removed must go silent.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rightkit_file_watch::{DirBatch, DirWatchSet, TypedWatcher, WatchEvent};
use serde_json::{json, Value};

const DEADLINE: Duration = Duration::from_secs(15);

fn names(paths: &[std::path::PathBuf]) -> BTreeSet<String> {
    paths
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Drain batches until `done(changed, removed)` holds or the deadline passes.
fn collect(
    rx: &mpsc::Receiver<DirBatch>,
    mut done: impl FnMut(&BTreeSet<String>, &BTreeSet<String>) -> bool,
) -> Result<(BTreeSet<String>, BTreeSet<String>, usize), String> {
    let start = Instant::now();
    let (mut changed, mut removed, mut batches) = (BTreeSet::new(), BTreeSet::new(), 0usize);
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(b) => {
                batches += 1;
                changed.extend(names(&b.changes.changed));
                removed.extend(names(&b.changes.removed));
                if done(&changed, &removed) {
                    return Ok((changed, removed, batches));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the watch channel closed".into()),
        }
    }
    Err(format!(
        "no matching debounced batch within {}s (changed {changed:?}, removed {removed:?}, {batches} batches)",
        DEADLINE.as_secs()
    ))
}

fn write(path: &Path, body: &str) -> Result<(), String> {
    std::fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    // ---- debounced batches over a recursive directory watch
    let root = ctx.dir("file-watch")?;
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    let set = DirWatchSet::with_recursive(4, Duration::from_millis(100), true);
    let (tx, rx) = mpsc::channel::<DirBatch>();
    set.watch_batches(&root, move |b| {
        let _ = tx.send(b);
    })
    .map_err(|e| format!("DirWatchSet::watch_batches({}): {e}", root.display()))?;
    if !set.is_watching(&root) || set.len() != 1 {
        return Err("DirWatchSet does not report the directory it was asked to watch".into());
    }

    write(&root.join("a.txt"), "one")?;
    write(&root.join("b.txt"), "two")?;
    write(&sub.join("nested.txt"), "three")?;
    let (created, _, create_batches) = collect(&rx, |c, _| ["a.txt", "b.txt", "nested.txt"].iter().all(|n| c.contains(*n)))
        .map_err(|e| format!("create phase: {e}"))?;

    write(&root.join("a.txt"), "one, changed with more bytes")?;
    std::fs::remove_file(root.join("b.txt")).map_err(|e| e.to_string())?;
    let (changed, removed, _) = collect(&rx, |c, r| c.contains("a.txt") && r.contains("b.txt"))
        .map_err(|e| format!("change/remove phase: {e}"))?;

    // After unwatch returns, no callback for the directory runs: a later write must stay silent.
    set.unwatch(&root);
    while rx.try_recv().is_ok() {}
    write(&root.join("after-unwatch.txt"), "x")?;
    std::thread::sleep(Duration::from_millis(800));
    if let Ok(b) = rx.try_recv() {
        return Err(format!("a batch arrived after unwatch: {:?}", b.changes));
    }
    if set.is_watching(&root) {
        return Err("DirWatchSet still reports the directory after unwatch".into());
    }

    // ---- the typed stream
    let typed_dir = ctx.dir("file-watch-typed")?;
    let watcher = TypedWatcher::watch(&typed_dir).map_err(|e| format!("TypedWatcher::watch: {e}"))?;
    write(&typed_dir.join("typed.txt"), "hello")?;
    let start = Instant::now();
    let mut seen = None;
    while start.elapsed() < DEADLINE && seen.is_none() {
        if let Ok(ev) = watcher.recv_timeout(Duration::from_millis(250)) {
            match ev {
                WatchEvent::Create(p) | WatchEvent::Modify(p)
                    if p.file_name().map(|n| n == "typed.txt").unwrap_or(false) =>
                {
                    seen = Some(format!("{ev:?}"));
                }
                _ => {}
            }
        }
    }
    let seen = seen.ok_or("TypedWatcher reported no Create/Modify for typed.txt within the deadline")?;
    let metrics = watcher.metrics();
    if metrics.queued_total == 0 || metrics.received_total == 0 {
        return Err(format!("TypedWatcher metrics did not count the delivered event: {metrics:?}"));
    }
    drop(watcher);

    Ok(json!({
        "created_seen": created, "create_batches": create_batches,
        "changed_seen": changed, "removed_seen": removed,
        "silent_after_unwatch": true, "typed_event": seen,
        "typed_metrics": { "queued": metrics.queued_total, "received": metrics.received_total },
    }))
}
