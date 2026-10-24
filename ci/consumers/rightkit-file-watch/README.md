# rightkit-file-watch 0.4.1 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-file-watch =0.4.1` exactly with the default feature set (`async`), the set ViewRight and CodeRight use. It also names `tokio` (runtime and timer only) to drive the async watcher. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion.

## What it proves

Without any operating-system event:

- Constants and defaults: `DEFAULT_DEBOUNCE`, `DEFAULT_MAX_WAIT`, `EXPLORER_DEBOUNCE`, `EXPLORER_WATCH_CAP`, `WatchOptions::default`, and `EventFilter`, which never hides a gap. `WatchEvent` and `WatchError` display and conversion from the re-exported `notify` error, `RetryPolicy` backoff and saturation, the `WatchCursor` byte encoding, `TypedOptions` builders and `Debug`, `WatchBackend` and `Granularity` defaults.
- `ChangeCoalescer` over raw `notify` events and real files: a file created and removed in one batch is dropped, existing files are changed, a missing one is removed, a rename chain `a -> b -> c` collapses to `a -> c`, `push_watch_event` feeds typed events, and a gap is returned as an error while pending changes are kept.
- `DirWatchSet` capacity (idempotent watch, the oldest evicted past the cap, `unwatch`, a missing folder is an error, a cap of zero becomes one), `FileSetWatcher` refusing a set whose parent folders cannot be watched, `WatchSessionManager` sessions (`watch`, replacement, `root_of`, `unwatch`, `release_owner`), and `FileWatcher::noop` with a subscriber whose receiver ends when the subscriber is dropped.

Against the operating system's own backend (inotify, FSEvents or ReadDirectoryChangesW). Each live check rewrites a file every 250 ms until the expected event arrives or 30 seconds pass, so a backend that starts late still passes and one that never delivers fails:

- `TypedWatcher::watch` delivers a `Create` or `Modify` for a new file under the canonical root and an event after its removal, with metrics, `status` and `cursor` for the unbounded constructor. `TypedWatcher::watch_with` delivers through a bounded queue on the `Notify` backend and on the default backend, honours `exclude_prefix` and `exclude_with`, and reports its queue capacity; `WatchBackend::Native` is an error on a host without a native backend and succeeds on macOS and Windows.
- `FileWatcher::watch_typed` starts the same typed stream and delivers a record to `try_recv_record`.
- `FileSetWatcher::watch` reports `Changed` for the path as the caller spelled it after an editor-style rename-over-original save, nothing for another file in the same folder, `Removed` after deletion, and nothing at all when the grant check refuses. `watch_with_options` delivers typed records that respect the event filter and finishes when its receiver is dropped.
- `DirWatchSet::watch` reports a net `ChangeBatch` for a change and a removal, `watch_records` delivers typed records and `watch_batches` a `DirBatch` for a change, and `watch_batches_when_present` and `watch_records_when_present` arm a folder that appears later and report it `lost` (an `Overflow` record for the typed form) first. `WatchSessionManager::poll` drains a debounced batch.
- The async `FileWatcher`: a subscriber registered on a folder receives a path set in the spelling it registered, also through `DebouncedWatchReceiver`.

## What it deliberately does not prove

- Windows network shares and removable volumes, native event ordering, event overflow under load, FSEvents history replay (a `WatchCursor` taken from a running watch), and the 50,000-file runtime budget.
- Behaviour with a backend that is unavailable, and `ThrottledWatchReceiver`.
- Exact event kinds for a removal: backends differ (a delete, a modify or a rename hint), so the typed-stream check accepts any of them and then asserts the file is gone. The net `Removed` of `FileSetWatcher` and `DirWatchSet` is exact.

State: a per-process directory under the OS temp directory, removed at the end, and the watcher threads it starts. No network, no process spawn, no real user data. A run takes about nine seconds on Linux.
