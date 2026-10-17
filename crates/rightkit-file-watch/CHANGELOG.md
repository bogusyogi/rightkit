# Changelog

## Unreleased

### Added (session manager, rightkit#547)

- `WatchSessionManager` (`session` module, re-exported at the root): owner-keyed watches with `watch(owner, id, path, recursive)`, `unwatch(owner, id)`, `poll(owner, id) -> Vec<ChangeBatch>` and `release_owner(owner)`, for the CodeRight `FsWatchManager` port. Each session is one OS watch whose events fold through `ChangeCoalescer`; `with_debounce` sets the quiet window (default `DEFAULT_DEBOUNCE`), and a session pending longer than `DEFAULT_MAX_WAIT` flushes anyway. Re-watching an existing key replaces it. A backend error or rescan request is reported as a batch whose `changed` includes the watched root, so the caller rescans it. Uses only the existing `notify` dependency.

### Added (0.4.1, additive)

- `DirWatchSet::watch_batches(dir, cb)`: like `watch`, but the callback receives a `DirBatch { changes: ChangeBatch, lost: bool }`. `lost` is set when the backend reported a buffer overflow, a rescan request or an error, and a batch carrying only the flag is still delivered, so a lost report is no longer log-only. `watch` is unchanged (it still logs gaps). `ChangeBatch` is unchanged.
- `DirWatchSet::watch_batches_when_present` and `watch_records_when_present` take a `RetryPolicy` (`initial`, `max`, doubling; default 250 ms to 30 s). A folder that does not exist yet is polled with backoff by a worker thread and armed when it appears; the first callback after a wait carries `lost` (one `Overflow` record), because changes between creation and arming are not reported. `unwatch`, eviction and drop stop the worker; a folder deleted after attaching is not re-armed. The existing `watch`/`watch_records` still return the arm error for a missing folder.
- Quiet window with maximum hold was already present (`WatchOptions::quiet`/`max_wait`, `DirWatchSet::with_options`); new unit tests pin that a constant stream flushes at first event plus `max_wait`.
- Windows: finding. notify 8.2.0's `ReadDirectoryChangesW` backend uses a fixed 16 KiB buffer and ignores the completion's byte count, so it never reports an overflow (Windows signals one as a successful completion with 0 bytes, or `ERROR_NOTIFY_ENUM_DIR`).
- Windows native directory reader (`src/native/windows.rs`, `DirWatcher`): overlapped `ReadDirectoryChangesW` (directory opened with `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED`, 64 KiB DWORD-aligned buffer, recursive per the watch set) with Win32 declared by hand in `kernel32` (no new dependency). A 0-byte or `ERROR_NOTIFY_ENUM_DIR` completion becomes a rescan event, so `DirWatchSet::watch_batches`/`watch_records` now deliver `lost = true` / `WatchEvent::Overflow` on Windows. Records feed the same debounce, quiet window, max hold and rename pairing as the notify path. If the native open fails, the watch falls back to notify. Stop and drop cancel the pending request (`CancelIoEx`), wait for it to complete, then free the buffer and close the handles.
- Windows typed stream: `TypedWatcher::watch_with` now uses the native reader. `WatchBackend::Native` no longer returns `WatchError::Backend` on Windows, and `WatchBackend::Auto` is native there (as on macOS). A `DirWatcher` feeds a pump thread that runs the same `BackendSink` (translator, exclusions, `Granularity::Directory` folding, bounded queue), so a lost report (zero-byte completion or `ERROR_NOTIFY_ENUM_DIR`) becomes `WatchEvent::Overflow` with `raw_kind: Some(EventKind::Other)`. `TypedOptions::windows_buffer_bytes` is honoured, rounded down to a multiple of 4 and clamped to [4 KiB, 64 KiB] (64 KiB is the most a network-share request accepts, so the 262,144 default becomes 64 KiB); if the first arm fails and `windows_network_buffer_bytes` is smaller, it is retried once with that. If the native open or arm still fails, `watch_with` falls back to notify (including for `Native`; on a host with no native backend `Native` stays an error). A `replay_from` cursor is answered with a leading `Overflow`, as on notify. Dropping the `TypedWatcher` cancels and waits out the pending request, then joins the reader and the pump; `translator_threads()` is 0 for the native Windows stream. `FileSetWatcher`, `FileSetReceiver` and `subscribe` still use notify on Windows and still cannot report an overflow.
- Windows tests: `FILE_NOTIFY_INFORMATION` parser (chained records, `NextEntryOffset`, UTF-16 and surrogate pairs, truncated and inconsistent buffers), the pure completion classifier (zero bytes and `ERROR_NOTIFY_ENUM_DIR` mean lost), a real temp-directory change through `DirWatcher` and through `watch_batches`, non-recursive filtering, and repeated start/drop with no leaked handle; a typed `watch_with` stream receives a real change and releases the folder on drop, `Native` and `Auto` open, a lost report becomes `Overflow` through the sink, and the buffer clamp as a pure function.

Renamed from rightkit-watch; 0.3.1 remains published under the old name.

### Fixed

- `FileWatcher` missing-path fallback arm race: a target found by the post-arm stat now also records `last_exists`, so a later parent deletion without a file event is reported as removal; and after arming, the watch is re-resolved and moved again (bounded by the path depth) if an intermediate directory appeared before the arm, so the watch no longer stays one level short.
- `FileWatcher` missing-path fallback: when the watch moves to a newly created directory, the target is now stat'ed after the native watch is armed and reported if it already exists. Previously a file created between the ancestor event and the arm produced no event at all. A racing native event may deliver the same path again in a later `recv`; paths within one batch are coalesced.

- `DirWatchSet::watch_records` docs state the existing delivery guarantee: eviction and `unwatch` join the watch thread, so the final flush (and FSEvents' history replay on arm) is delivered inside that call and no callback for the directory runs after it returns. `dir_watch_set_caps_evicts_oldest_and_unwatches` no longer treats that in-call flush as post-eviction emission (it failed once on macOS CI); it drains the backlog after eviction and then asserts that a write into the evicted directory produces no batch.

## 0.4.0

Purely additive: every existing constructor stays on notify with unchanged behaviour, and no existing field, variant or signature changed.

- New `TypedWatcher::watch_with(path, TypedOptions)`: a recursive typed stream with a bounded queue (default 32,768 records), path exclusions (`exclude_prefix`, `exclude_with`), `Granularity::Directory`, and a `WatchBackend` choice (`Auto`, `Native`, `Notify`). A full queue is cleared and the next receive returns one `WatchEvent::Overflow` with `raw_kind: None`; backend rescans keep `Some(EventKind::Other)`. Memory is bounded at capacity plus one record.
- New macOS native backend (FSEvents on a serial dispatch queue, no crate thread) behind `WatchBackend::Auto` and `Native`: renames are paired, history is resumable. `TypedOptions::latency` sets the FSEvents batching interval; `NoDefer` is set only for zero latency, because it would otherwise deliver the first event immediately.
- New resume cursor: `WatchCursor` (opaque, 25-byte `to_bytes`/`from_bytes`), `TypedOptions::replay_from`, `TypedWatcher::cursor`, `TypedWatcher::current_cursor` and `TypedWatcher::is_replaying`. A cursor that cannot be honoured (other volume, from the future, backend without replay) starts the stream live with `Overflow` first. Delivery after a resume is at-least-once.
- `WatchMetrics` gains `dropped_total` and `queue_capacity` (0 means unbounded). `WatchMetrics` and `WatchStatus` are now `#[non_exhaustive]`; both first appeared in 0.3.3, which was not published.
- Windows: `WatchBackend::Native` returns `WatchError::Backend` and `Auto` uses notify until the Windows native backend lands.
- `TypedWatcher` stays `Send` (compile-time assertion). New `tests/consumer_surface.rs` compiles the usages of ViewRight, membrane-blueprint and the CodeRight engine.
- New `THIRD-PARTY-NOTICES` (FSEvents binding adapted from rightkit-fsindex and fsearch, MIT).

## 0.3.3

- New `TypedWatcher::metrics()` returning `WatchMetrics`: records queued and received, the unread backlog (`queue_depth`), and overflow and error counts. The typed queue is unbounded, so the backlog is the figure that grows memory. `WatchMetrics` is re-exported from the crate root.
- New `tests/metrics.rs` checks the counter invariants against real file events.
- New `benches/budget.rs` (`rightkit cargo bench -p rightkit-watch --bench budget`) measures the runtime budget: 50,000 files, 400 changes per second for 60 s. Figures are in README "Runtime budget".

## 0.3.2

- New default-on `async` feature gates the `subscribe` module (`FileWatcher`, `FileWatcherSubscriber`,
  `FileWatcherEvent`, `Receiver`, `DebouncedWatchReceiver`, `ThrottledWatchReceiver`, `WatchPath`,
  `WatchRegistration`) and the `tokio` dependency. Default builds are unchanged;
  `default-features = false` compiles no async runtime.
- New `TypedWatcher::watch(path)`: the runtime-free constructor for the typed recursive stream.
  Previously the only way to build a `TypedWatcher` was `FileWatcher::watch_typed`, which now sits
  behind `async`; that method still exists and delegates to `watch`.
