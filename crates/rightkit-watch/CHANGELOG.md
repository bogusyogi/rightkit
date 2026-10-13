# Changelog

## Unreleased

### Fixed

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
