// rightkit-file-watch 0.4.1 consumed by exact registry pin (default feature "async", the set
// ViewRight and CodeRight use), outside the workspace.
//
// Proves, through the public API only. Pure parts, no operating-system events needed:
// - constants and option defaults (DEFAULT_DEBOUNCE, DEFAULT_MAX_WAIT, EXPLORER_*, WatchOptions,
//   EventFilter that lets gaps through), WatchEvent / WatchError display and conversion from the
//   re-exported notify error, RetryPolicy backoff, WatchCursor encoding, TypedOptions and
//   WatchMetrics, WatchBackend and Granularity defaults;
// - ChangeCoalescer over raw notify events and real files: a file created and removed in one batch
//   is dropped, an existing file touched is changed, a removed one is removed, a rename chain
//   a -> b -> c collapses to a -> c, push_watch_event feeds typed events and a gap is returned as an
//   error while pending changes are kept;
// - DirWatchSet capacity: idempotent watch, oldest evicted past the cap, unwatch, a missing folder
//   is an error; FileSetWatcher refuses a set whose parent folders cannot be watched;
//   WatchSessionManager sessions (watch, replace, root_of, unwatch, release_owner); FileWatcher::noop
//   with a subscriber that ends when dropped.
// Live parts, against the operating system's own backend (inotify, FSEvents, ReadDirectoryChangesW).
// Each rewrites a file every 250 ms until the expected event arrives or 30 s pass, so a backend
// that starts late (FSEvents) still passes and one that never delivers fails:
// - TypedWatcher::watch delivers a Create or Modify for a new file, then an event after its removal,
//   and FileWatcher::watch_typed starts the same stream (read through try_recv_record);
//   watch_with delivers through a bounded queue (Notify backend and the default backend), honours
//   exclude_prefix, reports its queue capacity in the metrics and status, and WatchBackend::Native
//   is an error on a host without a native backend;
// - FileSetWatcher::watch reports Changed for the path as the caller spelled it, Removed after the
//   file is deleted, nothing for a different file in the same folder, and nothing at all when the
//   grant check refuses; watch_with_options delivers typed records and finishes when its receiver
//   is dropped;
// - DirWatchSet::watch reports a net ChangeBatch, watch_records typed records and watch_batches a
//   DirBatch; watch_batches_when_present and watch_records_when_present arm a folder that appears
//   later and report it lost (an Overflow record) first; WatchSessionManager::poll drains a
//   debounced batch;
// - the async FileWatcher: a subscriber registered on a folder receives a path set, also through
//   DebouncedWatchReceiver.
// Does not prove: Windows network shares or removable volumes, native event ordering, event
// overflow under load, FSEvents history replay (WatchCursor from a running watch), the
// 50,000-file runtime budget, or behaviour with a backend that is unavailable. Public API only.
// State: a per-process directory under the OS temp directory, removed at the end, and the watcher
// threads it starts. No network, no process spawn, no real user data.
use rightkit_file_watch::notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
use rightkit_file_watch::notify::{self, Event, EventKind};
use rightkit_file_watch::{
    ChangeBatch, ChangeCoalescer, DebouncedWatchReceiver, DirBatch, DirWatchSet, EventFilter,
    FileChange, FileChangeKind, FileSetWatcher, FileWatcher, Granularity, Rename, RetryPolicy,
    TypedOptions, TypedWatcher, WatchBackend, WatchCursor, WatchError, WatchEvent, WatchEventKind,
    WatchEventRecord, WatchMetrics, WatchOptions, WatchSessionManager, DEFAULT_DEBOUNCE,
    DEFAULT_MAX_WAIT, EXPLORER_DEBOUNCE, EXPLORER_WATCH_CAP,
};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);
const SLICE: Duration = Duration::from_millis(250);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("rk-file-watch-consumer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }

    /// A fresh empty folder for one scenario.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).expect("create scenario dir");
        dir
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() {
    let temp = TempDir::new();
    constants_and_value_types();
    coalescing(&temp);
    capacity_and_refusals(&temp);
    typed_stream(&temp);
    typed_options(&temp);
    file_set(&temp);
    directory_sets(&temp);
    sessions(&temp);
    subscribers(&temp);
    println!("OK rightkit-file-watch 0.4.1 (registry consumer)");
}

/// Call `act(round)` every 250 ms until `poll` finds what it waits for; fail after 30 s.
fn eventually<T>(what: &str, mut act: impl FnMut(u32), mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    let mut round = 0;
    loop {
        act(round);
        if let Some(found) = poll() {
            return found;
        }
        round += 1;
        assert!(
            Instant::now() < deadline,
            "no event for {what} within {WAIT:?}"
        );
    }
}

/// A write inside a retry loop: a scanner or the watcher itself may hold the file for a moment on
/// Windows, and the next round writes again.
fn put(path: &Path, content: impl AsRef<[u8]>) {
    let _ = fs::write(path, content);
}

/// Remove inside a retry loop, for the same reason.
fn take(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect()
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|n| n == name)
}

// ---- constants, options, value types -------------------------------------------------------

fn constants_and_value_types() {
    assert_eq!(DEFAULT_DEBOUNCE, Duration::from_millis(100));
    assert_eq!(DEFAULT_MAX_WAIT, Duration::from_secs(2));
    assert_eq!(EXPLORER_DEBOUNCE, Duration::from_millis(200));
    assert_eq!(EXPLORER_WATCH_CAP, 32);

    let options = WatchOptions::default();
    assert_eq!(options.quiet, Duration::from_millis(100));
    assert_eq!(options.max_wait, Some(Duration::from_secs(2)));
    assert!(!options.tolerate_arm_failures && !options.discard_on_disconnect);
    assert!(options.reconcile && options.coalesce);
    assert_eq!(options.filter, EventFilter::default());

    let creates = EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]);
    assert!(creates.accepts(&WatchEvent::Create("a".into())));
    assert!(creates.accepts(&WatchEvent::Modify("a".into())));
    assert!(!creates.accepts(&WatchEvent::Delete("a".into())));
    assert!(!creates.accepts(&WatchEvent::Rename {
        from: "a".into(),
        to: "b".into()
    }));
    assert!(
        creates.accepts(&WatchEvent::Overflow),
        "a mutation filter cannot hide a gap"
    );
    assert!(creates.accepts(&WatchEvent::Error(WatchError::Overflow)));
    for kind in [
        WatchEventKind::Create,
        WatchEventKind::Modify,
        WatchEventKind::Delete,
        WatchEventKind::Rename,
    ] {
        assert!(EventFilter::default().accepts(&match kind {
            WatchEventKind::Create => WatchEvent::Create("a".into()),
            WatchEventKind::Modify => WatchEvent::Modify("a".into()),
            WatchEventKind::Delete => WatchEvent::Delete("a".into()),
            WatchEventKind::Rename => WatchEvent::Rename {
                from: "a".into(),
                to: "b".into()
            },
        }));
    }

    assert_eq!(
        WatchError::Overflow.to_string(),
        "filesystem events were dropped; rescan required"
    );
    let backend = WatchError::Backend {
        message: "unavailable".into(),
        paths: vec!["root".into()],
    };
    assert_eq!(backend.to_string(), "unavailable");
    let converted = WatchError::from(notify::Error::generic("boom").add_path("some/path".into()));
    match converted {
        WatchError::Backend { message, paths } => {
            assert!(message.contains("boom"), "{message}");
            assert_eq!(paths, [PathBuf::from("some/path")]);
        }
        other => panic!("expected a backend error, got {other:?}"),
    }
    let boxed: Box<dyn std::error::Error> = Box::new(WatchError::Overflow);
    assert!(boxed.to_string().contains("rescan"));

    let policy = RetryPolicy::new(Duration::from_millis(250), Duration::from_secs(30));
    assert_eq!(policy, RetryPolicy::default(), "250 ms doubling to 30 s");
    let delays: Vec<u64> = (0..9)
        .map(|attempt| policy.delay(attempt).as_millis() as u64)
        .collect();
    assert_eq!(
        delays,
        [250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
    );
    assert_eq!(
        policy.delay(500),
        Duration::from_secs(30),
        "huge attempt numbers saturate"
    );

    let mut bytes = [0u8; WatchCursor::ENCODED_LEN];
    bytes[0] = 1;
    bytes[1..17].copy_from_slice(&[7; 16]);
    bytes[17..].copy_from_slice(&42u64.to_le_bytes());
    let cursor = WatchCursor::from_bytes(&bytes).expect("version 1 cursor");
    assert_eq!((WatchCursor::ENCODED_LEN, cursor.event_id()), (25, 42));
    assert_eq!(cursor.to_bytes(), bytes);
    bytes[0] = 2;
    assert!(WatchCursor::from_bytes(&bytes).is_none(), "unknown version");
    assert!(
        WatchCursor::from_bytes(&bytes[..24]).is_none(),
        "wrong length"
    );

    assert_eq!(WatchBackend::default(), WatchBackend::Auto);
    assert_eq!(Granularity::default(), Granularity::File);
    let typed = TypedOptions::new()
        .backend(WatchBackend::Notify)
        .queue_capacity(NonZeroUsize::new(64).unwrap())
        .exclude_prefix("skip")
        .exclude_with(|path| path.ends_with("ignored"))
        .granularity(Granularity::Directory)
        .latency(Duration::from_millis(5))
        .replay_from(cursor)
        .windows_buffer_bytes(65_536)
        .windows_network_buffer_bytes(8_192);
    let shown = format!("{typed:?}");
    assert!(
        shown.contains("TypedOptions") && shown.contains("Notify") && shown.contains("Directory"),
        "{shown}"
    );
    assert!(format!("{:?}", TypedOptions::default()).contains("Auto"));
    assert_eq!(WatchMetrics::default().queue_capacity, 0);
    assert!(ChangeBatch::default().is_empty());
    assert_eq!(DirBatch::default(), DirBatch::default());
    assert!(!DirBatch::default().lost);
}

// ---- coalescing over real files ------------------------------------------------------------

fn raw(kind: EventKind, paths: &[&Path]) -> Event {
    paths.iter().fold(Event::new(kind), |event, path| {
        event.add_path(path.to_path_buf())
    })
}

fn coalescing(temp: &TempDir) {
    let dir = temp.dir("coalesce");
    let existing = dir.join("existing.txt");
    let gone = dir.join("gone.txt");
    let fresh = dir.join("fresh.txt");
    let temp_file = dir.join(".save.tmp");
    fs::write(&existing, b"v1").unwrap();
    fs::write(&fresh, b"new").unwrap();

    let mut coalescer = ChangeCoalescer::new();
    assert!(coalescer.is_empty());
    coalescer.push(&raw(EventKind::Modify(ModifyKind::Any), &[&existing]));
    coalescer.push(&raw(EventKind::Remove(RemoveKind::File), &[&gone]));
    coalescer.push(&raw(EventKind::Create(CreateKind::File), &[&fresh]));
    coalescer.push(&raw(EventKind::Create(CreateKind::File), &[&temp_file]));
    coalescer.push(&raw(EventKind::Remove(RemoveKind::File), &[&temp_file]));
    coalescer.push(&raw(
        EventKind::Access(notify::event::AccessKind::Any),
        &[&dir.join("ignored")],
    ));
    assert!(!coalescer.is_empty());
    let batch = coalescer.drain();
    assert_eq!(
        names(&batch.changed),
        ["existing.txt", "fresh.txt"],
        "existing paths are changed, sorted"
    );
    assert_eq!(
        names(&batch.removed),
        ["gone.txt"],
        "a path created and gone again inside one batch is dropped"
    );
    assert!(batch.renamed.is_empty() && !batch.is_empty());
    assert!(
        coalescer.is_empty() && coalescer.drain().is_empty(),
        "drain resets"
    );

    // An editor save: temp file renamed over the original.
    let doc = dir.join("doc.md");
    fs::write(&doc, b"v2").unwrap();
    let swap = dir.join(".doc.md.swp");
    coalescer.push(&raw(EventKind::Create(CreateKind::File), &[&swap]));
    coalescer.push(&raw(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[&swap, &doc],
    ));
    let batch = coalescer.drain();
    assert_eq!(names(&batch.changed), ["doc.md"]);
    assert!(
        batch.renamed.is_empty(),
        "the source began life inside the batch"
    );

    // A rename chain a -> b -> c collapses to a -> c.
    let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
    fs::write(&c, b"payload").unwrap();
    coalescer.push(&raw(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[&a, &b],
    ));
    coalescer.push(&raw(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[&b, &c],
    ));
    let batch = coalescer.drain();
    assert_eq!(
        batch.renamed,
        [Rename {
            from: a.clone(),
            to: c.clone()
        }]
    );
    assert_eq!(names(&batch.changed), ["c"]);
    assert_eq!(names(&batch.removed), ["a", "b"]);

    // Typed events feed the same machinery; a gap is an error and keeps what is pending.
    for event in [
        WatchEvent::Create(fresh.clone()),
        WatchEvent::Modify(existing.clone()),
        WatchEvent::Delete(gone.clone()),
        WatchEvent::Rename {
            from: a.clone(),
            to: b.clone(),
        },
        WatchEvent::Rename {
            from: b.clone(),
            to: c.clone(),
        },
    ] {
        coalescer
            .push_watch_event(&event)
            .expect("typed event absorbed");
    }
    let gap = WatchError::Backend {
        message: "unavailable".into(),
        paths: vec![dir.clone()],
    };
    assert_eq!(
        coalescer.push_watch_event(&WatchEvent::Error(gap.clone())),
        Err(gap)
    );
    assert_eq!(
        coalescer.push_watch_event(&WatchEvent::Overflow),
        Err(WatchError::Overflow)
    );
    assert!(!coalescer.is_empty(), "pending changes survive a gap");
    let batch = coalescer.drain();
    assert_eq!(names(&batch.changed), ["c", "existing.txt", "fresh.txt"]);
    assert_eq!(names(&batch.removed), ["a", "b", "gone.txt"]);
    assert_eq!(batch.renamed, [Rename { from: a, to: c }]);
}

// ---- capacity and refusals -----------------------------------------------------------------

fn capacity_and_refusals(temp: &TempDir) {
    let set = DirWatchSet::new(2, Duration::from_millis(50));
    assert!(set.is_empty());
    let (d1, d2, d3) = (temp.dir("cap-1"), temp.dir("cap-2"), temp.dir("cap-3"));
    set.watch(&d1, |_| {}).expect("watch d1");
    set.watch(&d2, |_| {}).expect("watch d2");
    assert_eq!(set.len(), 2);
    set.watch(&d2, |_| {})
        .expect("a second watch of the same folder is idempotent");
    assert_eq!(set.len(), 2);
    set.watch(&d3, |_| {})
        .expect("past the cap the oldest watch is evicted, never an error");
    assert_eq!(set.len(), 2);
    assert!(!set.is_watching(&d1) && set.is_watching(&d2) && set.is_watching(&d3));
    set.unwatch(&d2);
    assert!(!set.is_watching(&d2) && set.len() == 1);
    set.unwatch(&d2);
    assert!(
        set.watch(&temp.0.join("not-created"), |_| {}).is_err(),
        "a folder that does not exist cannot be watched"
    );
    assert_eq!(set.len(), 1);
    drop(set);
    assert!(DirWatchSet::default().is_empty());
    let recursive = DirWatchSet::with_recursive(4, Duration::from_millis(50), true);
    recursive.watch(&d1, |_| {}).unwrap();
    assert_eq!(recursive.len(), 1);
    let explicit = DirWatchSet::with_options(0, false, WatchOptions::default());
    explicit.watch(&d1, |_| {}).unwrap();
    explicit.watch(&d2, |_| {}).unwrap();
    assert_eq!(explicit.len(), 1, "a cap of zero is raised to one");

    let nowhere = temp.0.join("missing-parent").join("doc.md");
    let refused = FileSetWatcher::watch_default(std::slice::from_ref(&nowhere), |_| {});
    assert!(refused.is_err(), "no parent folder could be watched");

    let mut manager = WatchSessionManager::new();
    assert!(manager.is_empty());
    manager.watch("client-1", "w1", &d1, true).expect("watch");
    manager.watch("client-1", "w2", &d2, false).expect("watch");
    manager.watch("client-2", "w1", &d3, false).expect("watch");
    assert_eq!(manager.len(), 3);
    assert_eq!(manager.root_of("client-1", "w1"), Some(d1.as_path()));
    assert_eq!(manager.root_of("client-9", "w1"), None);
    manager
        .watch("client-1", "w1", &d3, true)
        .expect("an existing key is replaced");
    assert_eq!(
        (manager.len(), manager.root_of("client-1", "w1")),
        (3, Some(d3.as_path()))
    );
    assert!(manager.unwatch("client-1", "w2"));
    assert!(!manager.unwatch("client-1", "w2"));
    assert_eq!(manager.release_owner("client-1"), 1);
    assert_eq!(manager.release_owner("client-1"), 0);
    assert_eq!(manager.len(), 1);
    assert!(
        manager.poll("client-1", "w1").is_empty(),
        "an unknown session has nothing to drain"
    );
    assert!(
        manager.poll("client-2", "w1").is_empty(),
        "nothing pending, nothing due"
    );
    assert!(manager
        .watch("client-3", "w1", temp.0.join("not-created"), false)
        .is_err());

    // The inert watcher: a subscriber's receiver ends once the subscriber is gone.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let watcher = Arc::new(FileWatcher::noop());
        let (subscriber, mut receiver) = watcher.add_subscriber();
        let registration = subscriber.register_path(d1.clone(), true);
        drop(registration);
        drop(subscriber);
        assert_eq!(receiver.recv().await, None);
    });
}

// ---- the typed stream ----------------------------------------------------------------------

fn typed_stream(temp: &TempDir) {
    let dir = temp.dir("typed");
    let watch = TypedWatcher::watch(&dir).expect("start the typed watch");
    assert!(TypedWatcher::watch(temp.0.join("not-created")).is_err());
    let target = dir.join("live.txt");
    let created = eventually(
        "a new file",
        |round| put(&target, format!("round {round}")),
        || match watch.recv_timeout(SLICE) {
            Ok(WatchEvent::Create(path) | WatchEvent::Modify(path)) if named(&path, "live.txt") => {
                Some(path)
            }
            Ok(_) | Err(_) => None,
        },
    );
    assert!(named(&created, "live.txt"));
    assert!(
        created.starts_with(dir.canonicalize().unwrap()),
        "paths are under the canonical root: {created:?}"
    );

    while watch.recv_timeout(Duration::from_millis(1_000)).is_ok() {}
    eventually(
        "a removed file",
        |_| take(&target),
        || match watch.recv_timeout(SLICE) {
            Ok(WatchEvent::Delete(path) | WatchEvent::Modify(path)) if named(&path, "live.txt") => {
                Some(())
            }
            Ok(WatchEvent::Rename { from, .. }) if named(&from, "live.txt") => Some(()),
            Ok(_) | Err(_) => None,
        },
    );
    assert!(!target.exists());
    let metrics = watch.metrics();
    assert!(metrics.queued_total >= 2 && metrics.received_total >= 2);
    assert_eq!(
        metrics.queue_depth,
        metrics.queued_total - metrics.received_total
    );
    assert_eq!(
        (metrics.queue_capacity, metrics.dropped_total),
        (0, 0),
        "the plain constructor is unbounded"
    );
    assert!(
        watch.cursor().is_none(),
        "no resume position without a native backend"
    );
    assert!(!watch.is_replaying());
    assert_eq!(watch.status().metrics.queue_capacity, 0);
    assert!(TypedWatcher::current_cursor(&dir).is_none() || cfg!(target_os = "macos"));
    let record = {
        eventually(
            "a record",
            |round| put(&dir.join("record.txt"), format!("{round}")),
            || match watch.recv_record_timeout(SLICE) {
                Ok(record) if matches!(&record.event, WatchEvent::Create(p) | WatchEvent::Modify(p) if named(p, "record.txt")) => {
                    Some(record)
                }
                Ok(_) | Err(_) => None,
            },
        )
    };
    assert!(
        record.raw_kind.is_some(),
        "a backend event keeps its raw kind"
    );
    drop(watch);

    // FileWatcher::watch_typed is the same typed stream; read it through the non-blocking receiver.
    let typed = FileWatcher::watch_typed(&dir).expect("FileWatcher::watch_typed");
    assert!(FileWatcher::watch_typed(temp.0.join("not-created")).is_err());
    let via = dir.join("via.txt");
    eventually(
        "an event through FileWatcher::watch_typed",
        |round| put(&via, format!("round {round}")),
        || {
            std::thread::sleep(SLICE);
            while let Ok(record) = typed.try_recv_record() {
                if matches!(&record.event, WatchEvent::Create(p) | WatchEvent::Modify(p) if named(p, "via.txt"))
                {
                    return Some(());
                }
            }
            None
        },
    );
    drop(typed);
}

fn typed_options(temp: &TempDir) {
    let dir = temp.dir("typed-options");
    let skipped = dir.join("skipped");
    fs::create_dir_all(&skipped).unwrap();
    for backend in [WatchBackend::Notify, WatchBackend::Auto] {
        let options = TypedOptions::new()
            .backend(backend)
            .queue_capacity(NonZeroUsize::new(4_096).unwrap())
            .exclude_prefix(&skipped);
        let watch = TypedWatcher::watch_with(&dir, options).expect("watch_with");
        let mut events: Vec<WatchEvent> = Vec::new();
        eventually(
            "a file next to an excluded folder",
            |round| {
                put(&skipped.join("noise.txt"), format!("{round}"));
                put(&dir.join("kept.txt"), format!("{round}"));
            },
            || match watch.recv_timeout(SLICE) {
                Ok(event) => {
                    let found = matches!(&event, WatchEvent::Create(p) | WatchEvent::Modify(p) if named(p, "kept.txt"));
                    events.push(event);
                    found.then_some(())
                }
                Err(_) => None,
            },
        );
        while let Ok(event) = watch.recv_timeout(Duration::from_millis(500)) {
            events.push(event);
        }
        let leaked = events.iter().any(|event| match event {
            WatchEvent::Create(p) | WatchEvent::Modify(p) | WatchEvent::Delete(p) => {
                p.components().any(|c| c.as_os_str() == "skipped") && named(p, "noise.txt")
            }
            _ => false,
        });
        assert!(
            !leaked,
            "{backend:?}: an excluded prefix produced events: {events:?}"
        );
        assert_eq!(watch.metrics().queue_capacity, 4_096, "{backend:?}");
        assert_eq!(watch.status().metrics.dropped_total, 0);
        drop(watch);
    }
    let by_predicate = TypedOptions::new()
        .backend(WatchBackend::Notify)
        .exclude_with(|path| path.extension().is_some_and(|e| e == "log"));
    let watch = TypedWatcher::watch_with(&dir, by_predicate).expect("watch_with");
    let mut saw_log = false;
    eventually(
        "a file the predicate keeps",
        |round| {
            put(&dir.join("debug.log"), format!("{round}"));
            put(&dir.join("kept.txt"), format!("{round}"));
        },
        || match watch.recv_timeout(SLICE) {
            Ok(WatchEvent::Create(p) | WatchEvent::Modify(p)) => {
                saw_log |= named(&p, "debug.log");
                named(&p, "kept.txt").then_some(())
            }
            Ok(_) | Err(_) => None,
        },
    );
    assert!(!saw_log, "exclude_with drops what the predicate accepts");
    drop(watch);

    let native = TypedWatcher::watch_with(&dir, TypedOptions::new().backend(WatchBackend::Native));
    if cfg!(any(target_os = "macos", windows)) {
        assert!(native.is_ok(), "this host has a native backend");
    } else {
        assert!(
            native.is_err(),
            "a host without a native backend is an error, never a fallback"
        );
    }
}

// ---- file sets -----------------------------------------------------------------------------

fn file_set(temp: &TempDir) {
    let dir = temp.dir("file-set");
    let target = dir.join("doc.md");
    let sibling = dir.join("other.md");
    fs::write(&target, b"v1").unwrap();
    fs::write(&sibling, b"x").unwrap();
    let (tx, rx) = mpsc::channel::<Vec<FileChange>>();
    let watch = FileSetWatcher::watch(
        std::slice::from_ref(&target),
        Duration::from_millis(80),
        Duration::from_secs(1),
        |_: &Path| true,
        move |changes| {
            let _ = tx.send(changes);
        },
    )
    .expect("arm the file set");
    assert!(!watch.is_finished());
    let mut seen: Vec<FileChange> = Vec::new();
    eventually(
        "a changed file",
        |round| {
            put(&sibling, format!("noise {round}"));
            // An editor-style save: write a temp file, rename it over the original.
            let swap = dir.join(".doc.md.swap");
            put(&swap, format!("v2 round {round}"));
            let _ = fs::rename(&swap, &target);
        },
        || {
            while let Ok(batch) = rx.recv_timeout(SLICE) {
                seen.extend(batch);
            }
            seen.iter()
                .any(|c| c.kind == FileChangeKind::Changed)
                .then_some(())
        },
    );
    assert!(
        seen.iter().all(|c| c.path == target),
        "only the watched file, spelled as requested: {seen:?}"
    );
    assert!(seen.contains(&FileChange {
        path: target.clone(),
        kind: FileChangeKind::Changed
    }));
    assert!(fs::read_to_string(&target).unwrap().starts_with("v2 round"));

    seen.clear();
    while rx.recv_timeout(Duration::from_millis(1_000)).is_ok() {}
    eventually(
        "a removed file",
        |_| take(&target),
        || {
            while let Ok(batch) = rx.recv_timeout(SLICE) {
                seen.extend(batch);
            }
            seen.iter()
                .any(|c| c.kind == FileChangeKind::Removed)
                .then_some(())
        },
    );
    assert!(seen.contains(&FileChange {
        path: target.clone(),
        kind: FileChangeKind::Removed
    }));
    drop(watch);

    // A grant check that refuses everything keeps every change from being emitted.
    fs::write(&target, b"v3").unwrap();
    let emitted = Arc::new(AtomicBool::new(false));
    let flag = emitted.clone();
    let gated = FileSetWatcher::watch(
        std::slice::from_ref(&target),
        Duration::from_millis(50),
        Duration::from_millis(500),
        |_: &Path| false,
        move |_| flag.store(true, Ordering::SeqCst),
    )
    .expect("arm");
    for round in 0..8 {
        put(&target, format!("v{round}"));
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        !emitted.load(Ordering::SeqCst),
        "a refused grant must never emit"
    );
    drop(gated);

    // The typed stream over the same folder: records carry the requested spelling.
    let (watch, receiver) = FileSetWatcher::watch_with_options(
        std::slice::from_ref(&target),
        WatchOptions {
            quiet: Duration::from_millis(80),
            max_wait: Some(Duration::from_secs(1)),
            filter: EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]),
            ..WatchOptions::default()
        },
        |_: &Path| true,
    )
    .expect("typed file-set stream");
    let records = eventually(
        "typed records",
        |round| put(&target, format!("typed {round}")),
        || {
            receiver.recv_timeout(SLICE).ok().filter(|records| records.iter().any(|r| matches!(&r.event, WatchEvent::Create(p) | WatchEvent::Modify(p) if *p == target)))
        },
    );
    assert!(records.iter().all(|r| EventFilter::new([
        WatchEventKind::Create,
        WatchEventKind::Modify
    ])
    .accepts(&r.event)));
    assert!(!watch.is_finished());
    drop(receiver);
    let deadline = Instant::now() + WAIT;
    while !watch.is_finished() {
        assert!(
            Instant::now() < deadline,
            "dropping the receiver must stop the worker"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---- directory sets ------------------------------------------------------------------------

fn directory_sets(temp: &TempDir) {
    let dir = temp.dir("dir-set");
    let batches = Arc::new(Mutex::new(Vec::<ChangeBatch>::new()));
    let sink = batches.clone();
    let set = DirWatchSet::new(4, Duration::from_millis(80));
    set.watch(&dir, move |batch| sink.lock().unwrap().push(batch))
        .expect("watch");
    let target = dir.join("listing.txt");
    eventually(
        "a changed listing",
        |round| put(&target, format!("{round}")),
        || {
            std::thread::sleep(SLICE);
            batches
                .lock()
                .unwrap()
                .iter()
                .any(|b| names(&b.changed).contains(&"listing.txt".to_owned()))
                .then_some(())
        },
    );
    eventually(
        "a removed listing",
        |_| take(&target),
        || {
            std::thread::sleep(SLICE);
            batches
                .lock()
                .unwrap()
                .iter()
                .any(|b| names(&b.removed).contains(&"listing.txt".to_owned()))
                .then_some(())
        },
    );
    set.unwatch(&dir);
    assert!(set.is_empty());

    // The typed-record and lost-aware batch forms of the same watch.
    let record_dir = temp.dir("dir-set-records");
    let records = Arc::new(Mutex::new(Vec::<WatchEventRecord>::new()));
    let batched = Arc::new(Mutex::new(Vec::<DirBatch>::new()));
    let record_set = DirWatchSet::new(4, Duration::from_millis(80));
    let sink = records.clone();
    record_set
        .watch_records(&record_dir, move |batch| sink.lock().unwrap().extend(batch))
        .expect("watch_records");
    let batch_set = DirWatchSet::new(4, Duration::from_millis(80));
    let sink = batched.clone();
    batch_set
        .watch_batches(&record_dir, move |batch| sink.lock().unwrap().push(batch))
        .expect("watch_batches");
    let note = record_dir.join("records.txt");
    eventually(
        "typed records and a batch",
        |round| put(&note, format!("{round}")),
        || {
            std::thread::sleep(SLICE);
            let typed = records.lock().unwrap().iter().any(|r| {
                matches!(&r.event, WatchEvent::Create(p) | WatchEvent::Modify(p) if named(p, "records.txt"))
            });
            let batch = batched
                .lock()
                .unwrap()
                .iter()
                .any(|b| names(&b.changes.changed).contains(&"records.txt".to_owned()));
            (typed && batch).then_some(())
        },
    );
    record_set.unwatch(&record_dir);
    batch_set.unwatch(&record_dir);

    // A folder that does not exist yet is armed when it appears, and reported lost first.
    let late = temp.0.join("late-folder");
    let (tx, rx) = mpsc::channel::<DirBatch>();
    let late_set = DirWatchSet::with_options(
        4,
        false,
        WatchOptions {
            quiet: Duration::from_millis(80),
            reconcile: false,
            ..WatchOptions::default()
        },
    );
    late_set
        .watch_batches_when_present(
            &late,
            RetryPolicy::new(Duration::from_millis(50), Duration::from_millis(200)),
            move |batch| {
                let _ = tx.send(batch);
            },
        )
        .expect("a missing folder is not an error");
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "nothing is reported before the folder exists"
    );
    fs::create_dir_all(&late).unwrap();
    let first = rx.recv_timeout(WAIT).expect("the folder appeared");
    assert!(
        first.lost,
        "the first report after a late arm says to rescan: {first:?}"
    );
    drop(late_set);

    // The typed-record form: the first callback after a late arm carries an Overflow record.
    let late_records = temp.0.join("late-records");
    let (tx, rx) = mpsc::channel::<Vec<WatchEventRecord>>();
    let late_record_set = DirWatchSet::with_options(
        4,
        false,
        WatchOptions {
            quiet: Duration::from_millis(80),
            reconcile: false,
            ..WatchOptions::default()
        },
    );
    late_record_set
        .watch_records_when_present(
            &late_records,
            RetryPolicy::new(Duration::from_millis(50), Duration::from_millis(200)),
            move |records| {
                let _ = tx.send(records);
            },
        )
        .expect("a missing folder is not an error");
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "nothing is reported before the folder exists"
    );
    fs::create_dir_all(&late_records).unwrap();
    let first = rx.recv_timeout(WAIT).expect("the folder appeared");
    assert!(
        first
            .iter()
            .any(|r| matches!(r.event, WatchEvent::Overflow)),
        "the first records after a late arm say to rescan: {first:?}"
    );
    drop(late_record_set);
}

// ---- sessions ------------------------------------------------------------------------------

fn sessions(temp: &TempDir) {
    let dir = temp.dir("sessions");
    let mut manager = WatchSessionManager::new().with_debounce(Duration::from_millis(100));
    manager.watch("client-1", "w1", &dir, true).expect("watch");
    let target = dir.join("session.txt");
    let mut drained: Vec<ChangeBatch> = Vec::new();
    eventually(
        "a drained batch",
        |round| put(&target, format!("{round}")),
        || {
            std::thread::sleep(SLICE);
            drained.extend(manager.poll("client-1", "w1"));
            drained
                .iter()
                .any(|b| names(&b.changed).contains(&"session.txt".to_owned()))
                .then_some(())
        },
    );
    assert_eq!(manager.root_of("client-1", "w1"), Some(dir.as_path()));
    assert_eq!(manager.release_owner("client-1"), 1);
    assert!(manager.is_empty());
}

// ---- the async watcher ---------------------------------------------------------------------

fn subscribers(temp: &TempDir) {
    let dir = temp.dir("subscribers");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let watcher = Arc::new(FileWatcher::new().expect("start the watcher"));
        let (subscriber, mut receiver) = watcher.add_subscriber();
        let _registration = subscriber.register_path(dir.clone(), true);
        let target = dir.join("async.txt");
        let deadline = Instant::now() + WAIT;
        let mut got = None;
        let mut round = 0;
        while got.is_none() {
            put(&target, format!("{round}"));
            round += 1;
            if let Ok(Some(event)) = tokio::time::timeout(SLICE, receiver.recv()).await {
                got = event.paths.into_iter().find(|p| named(p, "async.txt"));
            }
            assert!(
                Instant::now() < deadline,
                "no subscriber event within {WAIT:?}"
            );
        }
        assert!(
            got.unwrap().starts_with(&dir),
            "events use the spelling the subscriber registered"
        );

        let (subscriber, receiver) = watcher.add_subscriber();
        let _registration = subscriber.register_path(dir.clone(), false);
        let mut debounced = DebouncedWatchReceiver::new(receiver, Duration::from_millis(100));
        let mut got = None;
        while got.is_none() {
            put(&target, format!("{round}"));
            round += 1;
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_secs(2), debounced.recv()).await
            {
                got = event.paths.into_iter().find(|p| named(p, "async.txt"));
            }
            assert!(
                Instant::now() < deadline,
                "no debounced event within {WAIT:?}"
            );
        }
    });
}
