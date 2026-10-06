# rightkit-watch

Filesystem watching for Right Suite apps (see `API.md` in the lane folder for the contract).

* `subscribe` — async multi-subscriber watcher. Ported from CodeRight `coderight_tools::file_watcher`,
  itself derived from the Apache-2.0 codex `file-watcher` crate.
* `sync` — thread-based `FileSetWatcher` (grant-gated, parent-directory watching) and `DirWatchSet`
  (capped explorer watches). Derived from ViewRight `src-tauri/src/watcher.rs`.
* `coalesce` — rename-storm coalescing that reports net filesystem truth per batch.

Licensed MIT OR Apache-2.0.
