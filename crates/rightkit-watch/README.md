# rightkit-watch

Filesystem watching for Right Suite apps (see `API.md` in the lane folder for the contract).

* `subscribe` — async multi-subscriber watcher. Ported from CodeRight `coderight_tools::file_watcher`,
  itself derived from the Apache-2.0 codex `file-watcher` crate.
* `sync` — thread-based `FileSetWatcher` (grant-gated, parent-directory watching) and `DirWatchSet`
  (capped explorer watches). Derived from ViewRight `src-tauri/src/watcher.rs`.
* `coalesce` — rename-storm coalescing that reports net filesystem truth per batch.

Licensed MIT OR Apache-2.0.

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
