//! Design test 5: the bounded queue, exclusions and rename degradation of `watch_with`, on every
//! backend this host offers. Real filesystem, no mocks.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_file_watch::notify::EventKind;
use rightkit_file_watch::{TypedOptions, TypedWatcher, WatchBackend, WatchEvent, WatchEventRecord};

const WAIT: Duration = Duration::from_secs(30);

fn paths_of(event: &WatchEvent) -> Vec<&Path> {
    match event {
        WatchEvent::Create(p) | WatchEvent::Modify(p) | WatchEvent::Delete(p) => vec![p],
        WatchEvent::Rename { from, to } => vec![from, to],
        WatchEvent::Error(_) | WatchEvent::Overflow => vec![],
    }
}

/// Receives until `done` accepts a record, returning everything seen.
fn receive_until(
    watch: &TypedWatcher,
    mut done: impl FnMut(&WatchEventRecord) -> bool,
) -> Vec<WatchEventRecord> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match watch.recv_record_timeout(left) {
            Ok(record) => {
                let stop = done(&record);
                seen.push(record);
                if stop {
                    return seen;
                }
            }
            Err(_) => break,
        }
    }
    panic!("condition not reached; saw {seen:#?}");
}

fn drain_quiet(watch: &TypedWatcher, quiet: Duration) -> Vec<WatchEventRecord> {
    let mut seen = Vec::new();
    while let Ok(record) = watch.recv_record_timeout(quiet) {
        seen.push(record);
    }
    seen
}

fn run(backend: WatchBackend) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let kept = root.join("kept");
    let excluded = root.join("excluded");
    std::fs::create_dir(&kept).unwrap();
    std::fs::create_dir(&excluded).unwrap();

    let options = TypedOptions::new()
        .backend(backend)
        .queue_capacity(NonZeroUsize::new(8).unwrap())
        .exclude_prefix(&excluded)
        .exclude_with(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("skip_"))
        });
    let watch = TypedWatcher::watch_with(&root, options).unwrap();
    // Only the notify backend owns a translator thread; the native path translates inline.
    let native = backend == WatchBackend::Auto && cfg!(target_os = "macos");
    assert_eq!(watch.translator_threads(), usize::from(!native));
    assert_eq!(watch.metrics().queue_capacity, 8);

    // 500 files, nobody draining: memory stays at capacity plus the one gap record.
    for i in 0..500 {
        std::fs::write(kept.join(format!("f_{i}.txt")), b"x").unwrap();
        let slot = i % 20;
        std::fs::write(excluded.join(format!("e_{slot}.txt")), b"x").unwrap();
        if i % 5 == 0 {
            std::fs::write(kept.join(format!("skip_{i}.txt")), b"x").unwrap();
        }
        let m = watch.metrics();
        assert!(
            m.queue_depth <= 9,
            "depth {} at file {i}: {m:?}",
            m.queue_depth
        );
    }
    let deadline = Instant::now() + WAIT;
    while watch.metrics().dropped_total == 0 {
        assert!(
            Instant::now() < deadline,
            "no drop recorded: {:?}",
            watch.metrics()
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(watch.metrics().queue_depth <= 9);
    }
    let m = watch.metrics();
    assert!(m.dropped_total > 0 && m.queue_depth <= 9, "{m:?}");

    let first = watch.recv_record_timeout(WAIT).unwrap();
    assert_eq!(first.event, WatchEvent::Overflow);
    assert_eq!(
        first.raw_kind, None,
        "queue overflow carries no backend kind"
    );

    let mut seen = drain_quiet(&watch, Duration::from_secs(2));
    // Only non-excluded paths ever appear.
    for record in &seen {
        for path in paths_of(&record.event) {
            assert!(
                !path.starts_with(&excluded),
                "excluded path leaked: {record:?}"
            );
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                !name.starts_with("skip_"),
                "predicate-excluded path leaked: {record:?}"
            );
        }
    }
    // Backend rescans keep their own raw kind, distinguishable from the queue's.
    for record in &seen {
        if record.event == WatchEvent::Overflow {
            assert!(matches!(record.raw_kind, None | Some(EventKind::Other)));
        }
    }

    // A later create still arrives once the consumer has caught up.
    std::fs::write(kept.join("late.txt"), b"x").unwrap();
    let late = kept.join("late.txt");
    seen = receive_until(&watch, |r| r.event == WatchEvent::Create(late.clone()));
    assert!(seen
        .iter()
        .any(|r| r.event == WatchEvent::Create(late.clone())));
    drain_quiet(&watch, Duration::from_secs(1));

    // A rename out of an excluded directory degrades to Create of the surviving side.
    let from = excluded.join("mv.txt");
    let to = kept.join("mv.txt");
    std::fs::write(&from, b"x").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    drain_quiet(&watch, Duration::from_millis(300));
    std::fs::rename(&from, &to).unwrap();
    let records = receive_until(&watch, |r| paths_of(&r.event).contains(&to.as_path()));
    let touched: HashSet<PathBuf> = records
        .iter()
        .flat_map(|r| paths_of(&r.event).into_iter().map(Path::to_path_buf))
        .collect();
    assert!(
        !touched.iter().any(|p| p.starts_with(&excluded)),
        "{records:#?}"
    );
    let hit = records
        .iter()
        .find(|r| paths_of(&r.event).contains(&to.as_path()))
        .unwrap();
    // FSEvents through notify reports renames as unpaired path hints, so only the native macOS
    // backend (which pairs them) and the other platforms' rename-aware backends can show Create.
    if native || !cfg!(target_os = "macos") {
        assert_eq!(hit.event, WatchEvent::Create(to.clone()), "{records:#?}");
    } else {
        assert!(
            matches!(hit.event, WatchEvent::Create(_) | WatchEvent::Modify(_)),
            "{hit:?}"
        );
    }
}

#[test]
fn bounded_queue_and_exclusions_on_the_notify_backend() {
    run(WatchBackend::Notify);
}

#[test]
fn bounded_queue_and_exclusions_on_the_default_backend() {
    run(WatchBackend::Auto);
}

#[test]
fn native_backend_on_an_unsupported_host_is_an_error_not_a_fallback() {
    if cfg!(any(target_os = "macos", windows)) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = TypedWatcher::watch_with(
        dir.path(),
        TypedOptions::new().backend(WatchBackend::Native),
    )
    .err()
    .expect("Native must not silently fall back");
    assert!(matches!(
        error,
        rightkit_file_watch::WatchError::Backend { .. }
    ));
}
