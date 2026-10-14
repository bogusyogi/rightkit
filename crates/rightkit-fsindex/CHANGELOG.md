# Changelog


## 0.2.1

- Republished on the renamed dependencies: `rightkit-child-process` (was `rightkit-process`, dev). The 0.2.0 on crates.io was packaged before the renames and still named the old crates.
## 0.2.0

- macOS: `list_dir_fd(fd, ListOptions { limit, want_creation_time, refuse_dataless }) -> Listing { entries, truncated }`. One `getattrlistbulk` pass over an open directory. `Entry` carries the data-fork size (equal to `st_size`; the resource fork is excluded), `allocated` across all forks, device and inode, modification time, optional creation time, and `EntrySize::Dataless` when a dataless file's size is refused. Entries with a kernel error are omitted.
- macOS: `FsEventsWatcher::start(roots, WatchOptions { since_event_id, latency, flags }) -> (FsEventsWatcher, Receiver<FsEvent>)`. Typed `FsEvent`s: `Changed` (with `ItemFlags` under `WatchFlags::file_events`), `MustScanSubDirs`, `RootChanged`, `Dropped { must_rescan }`, `IdsWrapped`, `HistoryDone`, `Mount`, `Unmount`. `last_event_id()` gives the id to persist.
- `Backend::watch` is a thin wrapper over the same FSEvents stream and keeps its behaviour: the index is unchanged, and the crawl keeps its data-plus-resource-fork size rule.
- The FSEvents stream now delivers raw batches to a sink. A watch root containing a NUL byte is an error instead of being dropped silently. A failed `FSEventStreamCreate` is an error.
- Public APIs are macOS-only in this release. Windows keeps the `Unsupported` stub.
- Additive release: no existing signature changed. Public names are listed in README under "Public APIs and semver".
- Change handling: each folder is re-listed at most once per interval. A folder that keeps changing backs off from `Config::relist_min_interval` (default 5 s) doubling to `relist_max_interval` (60 s). A quiet folder returns to the floor. Changes inside the window are deferred to its end, never dropped. Set the floor to zero to disable. `Status` reports `relists_total`, `relist_rate_per_s`, and `backoff_folders`.
- Default `watch_latency` is now 500 ms (was 50 ms) and `change_coalesce` 1 s (was 150 ms). Existing callers that set these keep their values.
- Excludes: `Config::exclude_patterns` (glob over path components, `*` and `?`, matched case-sensitively wherever their segments appear below a root). The defaults in `DEFAULT_EXCLUDE_PATTERNS` (`target`, `node_modules`, `DerivedData`, `.build`, `.cache`, `.git/objects`, `Library/Caches`) apply unless `Config::include_build_output` is set. The exact-path `Config::exclude` is unchanged. A root placed under a pattern's name is still indexed. The fingerprint includes the pattern set, so changing it rebuilds the snapshot.
- Config gained public fields. Code that builds `Config` with a struct literal instead of `Config::new` must add them.
- `Status` reports runtime figures: `resident_bytes { mapped, heap }` (mapped is file-backed column and name pages currently resident, measured with `mincore`; heap is the tracked allocation of the index's own collections, an estimate), `resident_per_entry`, `dir_reads_total` and `dir_reads_per_s` (one per directory listing), `cpu_seconds_total`, and `cpu_percent` (process-wide, since the previous `status()` call).
- `Index::shrink()` commits pending changes, drops the resident pages of the column and name mappings, and asks macOS to release free malloc pages.
- Layout is unchanged: RKFSIX03 stays frozen. The compact per-entry representation was not changed; the measured figures are in the README "Runtime budget" section.

## 0.1.1

- `trim` remaps the column files instead of `madvise(DONTNEED)` and runs after reopen and, at most every 2 s, after a search on stores above 500k slots; it takes the write lock briefly.
- Hidden diagnostics `aux_counts` / `special_count`; `footprint` bench takes the entry count after `--`.
- README documents memory and on-disk layout; package metadata (documentation, keywords, categories, include) completed.

## 0.1.0

- Initial release: macOS getattrlistbulk crawl, FSEvents upkeep with event-ID resume, fuzzy and one-typo name search,
  64-bit logical/allocated/allocated-unique folder rollups (APFS clone and hard-link aware), package flag,
  coalesced change subscription, opt-in `content` feature (trigram search).
- Index state lives in memory-mapped fixed-width column files with a generation header; clean commits reopen without a crawl.
