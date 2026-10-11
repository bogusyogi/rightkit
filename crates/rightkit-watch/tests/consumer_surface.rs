//! Source-compatibility guard: the exact `rightkit-watch` usages of the apps that pin earlier
//! versions must keep compiling. Apps cannot take a path dependency, so this file mirrors them.
//!
//! * ViewRight `=0.3.1`: `src-tauri/src/watcher.rs` (full `WatchOptions` literal, `FileSetWatcher`,
//!   `DirWatchSet`).
//! * membrane-blueprint `=0.3.0`: `src/watch.rs` (exhaustive `WatchEvent` match, `FileWatcher::watch_typed`).
//! * CodeRight engine `=0.1.0`: `bins/coderight/src/api_fs_watch.rs` (`subscribe` imports).

use std::time::Duration;

#[cfg(feature = "async")]
#[allow(unused_imports)]
use rightkit_watch::{
    DebouncedWatchReceiver, FileWatcher, FileWatcherSubscriber, WatchPath, WatchRegistration,
};
use rightkit_watch::{
    DirWatchSet, EventFilter, FileChangeKind, FileSetWatcher, TypedWatcher,
    WatchEvent as BackendWatchEvent, WatchEventKind, WatchOptions, EXPLORER_DEBOUNCE,
    EXPLORER_WATCH_CAP,
};

/// ViewRight builds `WatchOptions` as a full struct literal: a new public field would break it.
fn viewright_explorer_watch_set() -> DirWatchSet {
    DirWatchSet::with_options(
        EXPLORER_WATCH_CAP,
        false,
        WatchOptions {
            filter: EventFilter::new([
                WatchEventKind::Create,
                WatchEventKind::Modify,
                WatchEventKind::Delete,
                WatchEventKind::Rename,
            ]),
            quiet: EXPLORER_DEBOUNCE,
            max_wait: None,
            tolerate_arm_failures: false,
            discard_on_disconnect: true,
            reconcile: false,
            coalesce: true,
        },
    )
}

/// membrane-blueprint matches `WatchEvent` with no wildcard arm: a new variant would break it.
fn membrane_blueprint_event(event: BackendWatchEvent) -> &'static str {
    match event {
        BackendWatchEvent::Create(_) => "create",
        BackendWatchEvent::Delete(_) => "delete",
        BackendWatchEvent::Modify(_) => "modify",
        BackendWatchEvent::Rename { from: _, to: _ } => "rename",
        BackendWatchEvent::Error(_) | BackendWatchEvent::Overflow => "gap",
    }
}

/// `FileChangeKind` stays exhaustively matchable by ViewRight's mapping.
fn viewright_kind(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Changed => "changed",
        FileChangeKind::Removed => "removed",
    }
}

#[test]
fn the_apps_usages_still_compile_and_run() {
    let set = viewright_explorer_watch_set();
    assert!(set.is_empty());
    // ViewRight's call shape: targets, quiet, max wait, grant closure, change callback.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, b"x").unwrap();
    let watcher = FileSetWatcher::watch(
        std::slice::from_ref(&file),
        Duration::from_millis(50),
        Duration::from_secs(1),
        |_: &std::path::Path| true,
        move |changes| {
            for change in changes {
                if change.kind == FileChangeKind::Changed {
                    let _ = change.path.to_string_lossy();
                }
            }
        },
    )
    .map_err(|e| e.to_string())
    .unwrap();
    drop(watcher);
    assert_eq!(viewright_kind(FileChangeKind::Changed), "changed");
    assert_eq!(membrane_blueprint_event(BackendWatchEvent::Overflow), "gap");

    let dir = tempfile::tempdir().unwrap();
    let watch = TypedWatcher::watch(dir.path()).unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    let event = watch.recv_timeout(Duration::from_secs(20)).unwrap();
    let _ = membrane_blueprint_event(event);
    // The legacy constructors stay unbounded: no queue capacity, nothing dropped.
    let metrics = watch.metrics();
    assert_eq!(metrics.queue_capacity, 0);
    assert_eq!(metrics.dropped_total, 0);
    assert!(
        watch.cursor().is_none(),
        "legacy constructors have no resume position"
    );
}

#[test]
fn typed_watcher_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<TypedWatcher>();
}
