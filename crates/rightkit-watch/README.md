# rightkit-watch

Filesystem watching for Right Suite apps (see `API.md` in the lane folder for the contract).

* `subscribe` — async multi-subscriber watcher. Ported from CodeRight `coderight_tools::file_watcher`,
  itself derived from the Apache-2.0 codex `file-watcher` crate.
* `sync` — thread-based `FileSetWatcher` (grant-gated, parent-directory watching) and `DirWatchSet`
  (capped explorer watches). Derived from ViewRight `src-tauri/src/watcher.rs`.
* `coalesce` — rename-storm coalescing that reports net filesystem truth per batch.

Licensed MIT OR Apache-2.0.

## Also (0.3.1)

`DirWatchSet::with_options(cap, recursive, WatchOptions)` exposes the same policy
as file-set streams: `max_wait: None` for trailing quiet only, `Some(duration)`
for a forced deadline, `coalesce: false` for immediate backend hints, kind
filters, tolerated arm failures, discard on disconnect & snapshot reconciliation.

```rust
use std::{path::Path, time::Duration};
use rightkit_watch::{DirWatchSet, EventFilter, WatchEventKind, WatchOptions};

let dirs = DirWatchSet::with_options(32, false, WatchOptions {
    quiet: Duration::from_millis(200),
    max_wait: None,
    coalesce: false,
    reconcile: false,
    discard_on_disconnect: true,
    filter: EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]),
    ..WatchOptions::default()
});
dirs.watch_records(Path::new("."), |records| println!("{records:?}"))?;
```

`watch_records` delivers typed categories, raw backend kinds & errors/overflow
that bypass filters. Uncoalesced records preserve duplicate hints & turn rename
halves into Delete/Create. `watch` retains its sorted, duplicate-free
`ChangeBatch` callback; it logs gaps instead of exposing typed diagnostics.
Callbacks run on worker threads & must not synchronously unwatch their own set.

Legacy `new`, `with_recursive` & `default` keep coalescing on, strict arming,
all mutation kinds, `max_wait = Some(quiet * 10)`, flushing on disconnect & no
snapshot reconciliation. Explicit options use `WatchOptions` defaults instead.
Enabled directory reconciliation captures root/child existence, including
descendants only in recursive mode, then rescans on batches & gaps. It does not
follow directory symlinks or detect content changes without backend hints.
Snapshot failures report errors without treating unreadable trees as deleted.
Tolerated registration failures arrive as Error records without automatic retries;
watcher creation, snapshot startup & thread failures still return `Err`.
Unwatch, eviction, set drop & backend disconnect discard pending mutations when
requested, otherwise flush. Callback APIs have no separate consumer disconnect.

Directory tests inject events for duplicate delivery, net filtering, snapshot
reconciliation, recursion & discard/flush; native tests cover strict/tolerated
arming, callback delivery, idempotence & capacity eviction.

## Also (0.2.0)

Existing APIs remain source-compatible. New APIs let consumers handle native hints
without depending directly on `notify`.

1. **Typed recursive events.** `FileWatcher::watch_typed` owns a backend & a polling
   stream; it needs no Tokio runtime. Keep its handle alive. Paths use backend
   spelling under a canonicalized root. Tracker-correlated rename halves pair;
   adjacent untracked halves pair too. Unpaired halves become Delete/Create after
   250 ms. FSEvents supplies unpaired rename hints as Modify events.

   ```rust
   use rightkit_watch::{FileWatcher, WatchEvent};

   let watch = FileWatcher::watch_typed(".")?;
   match watch.recv()? {
       WatchEvent::Rename { from, to } => println!("{from:?} -> {to:?}"),
       event => println!("{event:?}"), // Create, Modify, Delete, Error, Overflow
   }
   ```

2. **Caller-visible failures & gaps.** Startup returns `Result`, including failed
   root registration. Backend errors preserve their message & paths. Overflow,
   dropped-event & rescan flags reach the stream even without paths. Reconcile a
   snapshot on either gap before reporting freshness. Receive errors describe
   channel state; backend errors arrive as `WatchEvent::Error`. The stream queue is
   unbounded; drain it regularly.

   ```rust
   use rightkit_watch::{FileWatcher, WatchEvent};

   let watch = FileWatcher::watch_typed(".")?; // registration errors propagate
   match watch.try_recv() {
       Ok(WatchEvent::Error(error)) => eprintln!("rescan required: {error}"),
       Ok(WatchEvent::Overflow) => eprintln!("events dropped; rescan required"),
       other => println!("{other:?}"),
   }
   ```

3. **Recursive directory sets.** Defaults stay nonrecursive. Opt in explicitly;
   capacity counts watched roots, including their descendants.

   ```rust
   use std::{path::Path, time::Duration};
   use rightkit_watch::DirWatchSet;

   let dirs = DirWatchSet::with_recursive(32, Duration::from_millis(200), true);
   dirs.watch(Path::new("."), |batch| println!("{batch:?}"))?;
   ```

4. **Feed coalescing without backend events.** `push_watch_event` preserves net
   filesystem semantics & collapsed rename chains. It returns `Err` on gaps,
   retaining pending changes so callers can reconcile explicitly.

   ```rust
   use rightkit_watch::{ChangeCoalescer, FileWatcher};

   let watch = FileWatcher::watch_typed(".")?;
   let mut changes = ChangeCoalescer::new();
   changes.push_watch_event(&watch.recv()?)?; // errors/overflow propagate
   let batch = changes.drain();
   println!("{batch:?}");
   ```

## Also (0.3.0)

Existing signatures, event shapes & constructor defaults remain unchanged.
`FileSetWatcher::watch_with_options` adds an opt-in typed stream over parent
directories, with requested path spelling & emission-time grant checks. Keep
its watch handle alive; receive batches through its separate `FileSetReceiver`.
This replaces ViewRight's direct backend/filter/debounce plumbing.

`WatchOptions::default()` accepts all four mutation categories, with 100 ms quiet,
`Some(2 seconds)` max-wait, strict arming (`tolerate_arm_failures: false`),
`discard_on_disconnect: false`, `reconcile: true` & `coalesce: true`. Legacy
`FileSetWatcher::watch` still skips individual arm failures & errors if none arm.
Streams are unbounded; drain regularly. Errors & overflow always bypass filters.

1. **Kind filters & raw provenance.** Select any subset of Create/Modify/Delete/Rename.
   Filtering applies to delivered typed categories, after net coalescing & target
   projection. `raw_kind` preserves backend subtypes. Coalesced records keep the
   last contributing raw kind; parent-only reconciliation keeps its triggering
   hint's kind. Snapshot-only records & backend/arm errors use `None`.
   Existing `TypedWatcher` also offers `recv_record`, `try_recv_record` &
   `recv_record_timeout`; these consume the same queue as its existing methods.

   ```rust
   use std::{path::Path, path::PathBuf};
   use rightkit_watch::{EventFilter, FileSetWatcher, WatchEventKind, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md")];
   let options = WatchOptions {
       filter: EventFilter::new([WatchEventKind::Create, WatchEventKind::Modify]),
       ..WatchOptions::default()
   };
   let (_watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   for record in rx.recv()? {
       println!("{:?}: {:?}", record.raw_kind, record.event);
   }
   ```

2. **Quiet-only debounce.** `max_wait: None` removes the forced flush deadline.
   Each translated mutation resets the trailing quiet window; continuous traffic
   can postpone delivery indefinitely. Gaps are reported immediately & trigger
   an enabled rescan; mutation output still obeys debounce policy. Normal input disconnect flushes pending changes unless
   discard-on-disconnect is enabled.

   ```rust
   use std::{path::{Path, PathBuf}, time::Duration};
   use rightkit_watch::{FileSetWatcher, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md")];
   let options = WatchOptions {
       quiet: Duration::from_millis(100),
       max_wait: None,
       ..WatchOptions::default()
   };
   let (_watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   println!("{:?}", rx.recv()?);
   ```

3. **Tolerated arm failures.** Failed parent-directory registrations produce
   `WatchEvent::Error` records with failed paths while successfully armed parents
   remain watched. Even an entirely unavailable set can return a handle & its
   failure records. Backend creation/thread startup failures still return `Err`.

   ```rust
   use std::path::{Path, PathBuf};
   use rightkit_watch::{FileSetWatcher, WatchEvent, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md"), PathBuf::from("./unavailable/doc.md")];
   let options = WatchOptions { tolerate_arm_failures: true, ..WatchOptions::default() };
   let (_watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   for record in rx.recv()? {
       if let WatchEvent::Error(error) = record.event { eprintln!("{error}"); }
   }
   ```

4. **Discard & stop on disconnect.** With `discard_on_disconnect: true`, input
   disconnect/handle drop discards pending changes. Receiver drop silently stops
   its worker & disarms its backend, even with no incoming events or during a
   long quiet window. Sends use an unbounded channel & never wait on consumers.
   With `false`, input disconnect flushes pending changes; consumer disconnect
   emits a tracing warning & stops. `is_finished()` reports worker completion.

   ```rust
   use std::path::{Path, PathBuf};
   use rightkit_watch::{FileSetWatcher, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md")];
   let options = WatchOptions { discard_on_disconnect: true, ..WatchOptions::default() };
   let (watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   drop(rx); // Stops & disarms asynchronously, without requiring another event.
   drop(watch); // Joins completion.
   ```

5. **Disable reconciliation.** `reconcile: false` skips initial target-existence
   capture & subsequent snapshot/rescan passes. Backend errors & overflow still
   arrive. Enabled coalescing continues resolving only touched paths against disk;
   disabling both options provides hint-only delivery. FSEvents can report only
   a parent path, so skipped reconciliation may leave child deletion unreported.

   ```rust
   use std::path::{Path, PathBuf};
   use rightkit_watch::{FileSetWatcher, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md")];
   let options = WatchOptions { reconcile: false, ..WatchOptions::default() };
   let (_watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   println!("{:?}", rx.recv()?);
   ```

6. **Uncoalesced delivery.** `coalesce: false` bypasses debounce, deduplication &
   rename pairing/echo suppression, delivering each backend result's translated
   path hints immediately. Explicit paired renames stay Rename; From/To halves
   become Delete/Create & retain their exact raw Name kind. This does not undo
   native FSEvents coalescing. Disable reconciliation too for backend hints only.

   ```rust
   use std::path::{Path, PathBuf};
   use rightkit_watch::{FileSetWatcher, WatchOptions};

   let paths = vec![PathBuf::from("./doc.md")];
   let options = WatchOptions { coalesce: false, reconcile: false, ..WatchOptions::default() };
   let (_watch, rx) = FileSetWatcher::watch_with_options(&paths, options, |_: &Path| true)?;
   for record in rx.recv()? { println!("{record:?}"); }
   ```

7. **Backend re-export.** Consumers can name backend types from this crate's
   `notify` dependency (currently the `8.2.0` requirement), without adding their own.

   ```rust
   use rightkit_watch::{notify, WatchEventRecord};

   fn raw_kind(record: &WatchEventRecord) -> Option<notify::EventKind> {
       record.raw_kind
   }
   ```

Policy tests inject backend events & time to verify exact filtering, subtype
provenance, quiet-only deadlines, duplicate preservation, discard/stop & skipped
reconciliation. Native tests use canonicalized temporary paths, staged operations,
kind sets & 20-second polling deadlines for Windows & macOS backend differences.
