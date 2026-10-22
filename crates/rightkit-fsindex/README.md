# rightkit-fsindex

Live whole-disk file index for RightKit apps (Pulse and others): fuzzy,
typo-tolerant **name search** plus **folder sizes** that are correct for a
storage app. Library API only; a daemon/IPC can come later. `published to crates.io` as 0.1.1; the 0.2.0 APIs below are source-only (see INDEX.md).

```rust
let mut cfg = Config::new(vec!["/".into()]);
cfg.state_dir = Some(app_support.join("fsindex")); // snapshot + mmap'd names; enables resume
let index = Index::open(cfg)?;                      // blocks for the first crawl or snapshot load

index.search("invoice 2024")?;                      // Vec<Hit>, ranked
index.folder_size("/Users/me/Library");            // Option<SizeInfo> { logical, allocated, allocated_unique, files, is_package }
index.children_sorted_by_size("/Users/me");        // Option<Vec<Item>>, allocated_unique desc by default
let changes = index.subscribe();                    // Receiver<ChangeBatch>
index.status();                                     // roots readable?, skipped/unreadable dirs, FDA, resume
```

## Platform support

macOS (v1): `getattrlistbulk` crawl (parallel, std threads), FSEvents upkeep with event-ID
resume from a snapshot, full-rescan fallback on dropped events, subtree rescan on
must-scan-subdirs/root-changed/mount events.

Windows (0.4.0): the `Backend` trait (`src/backend/mod.rs`) is the seam. The default backend is
`WindowsSearchBackend` (`src/backend/windows_search.rs`): it reads the Windows Search `SystemIndex`
through the OLE DB provider `Search.CollatorDSO`, so it sees only what the Search service indexes (by
default the user profile and Start menu; add folders in Indexing Options for more), reports logical
sizes only (`allocated_unique == allocated`, hard links not deduplicated), and has no change feed:
open the index with `Config::watch = false` and reopen to refresh. When the `WSearch` service is
stopped or missing, or the provider is not registered, `Index::open` and `check_supported` return
`Error::Unsupported` with the reason; `windows_search_status(scope)` reports the same state
(`service`, `provider_registered`, `scope_indexed`) without opening anything. Without an index,
`search_names(text, &SearchOptions)` and `folder_rollup(scope, max_rows)` query the catalog directly.
User text is escaped before it reaches the SQL (quotes doubled; LIKE wildcards neutralised and the
results re-filtered in Rust). Pulse wires this behind a setting that defaults to Off.

`backend::windows_usn` reads the NTFS USN journal for a volume (`UsnJournal::open('C')`, `query`,
`read`, `enumerate_files`). It needs an elevated process: otherwise it returns
`UsnError::PermissionDenied`; on a volume without a journal it returns `Unsupported` or
`JournalNotActive`. Resolving file reference numbers to paths and feeding `Backend::watch` from the
journal (the MFT/USN backend) is not implemented yet. Packages do not exist on Windows. Everything
above the trait (store, rollups, query, subscriptions, snapshot) is platform-neutral.

## Public APIs and semver (0.2.0, macOS)

Additive over 0.1.1; no existing item changed signature. Windows does not get these APIs
until its backend ships.

* `list_dir_fd(BorrowedFd, ListOptions) -> Result<Listing>`, with `ListOptions`, `Listing`,
  `Entry`, and `EntrySize` (`Bytes`, `NoSize`, `Dataless`). Reads from the descriptor's current
  position. Pass a fresh descriptor to list from the start. Sizes are data-fork only, so
  `logical == st_size`. `allocated` covers every fork and equals `st_blocks * 512` for ordinary
  files. The resource fork is not in `logical`; the index's own crawl keeps its data-plus-resource
  rule, so folder rollups do not change.
* `FsEventsWatcher::start(&[PathBuf], WatchOptions) -> Result<(FsEventsWatcher, Receiver<FsEvent>)>`,
  with `WatchOptions { since_event_id, latency, flags: WatchFlags }`, `FsEvent` (`#[non_exhaustive]`),
  `ItemFlags`, and `WatchFlags`. `FsEventsWatcher::last_event_id()` gives the highest id delivered.
  Persist it only after applying the events up to it, then resume with `since_event_id`.
  `FsEventsWatcher::current_event_id()` reads the position before a scan. Dropping the watcher
  disconnects the receiver.

Semver promise: from 0.2.0, items listed above change only in a minor release (or a major one).
Enum types marked `#[non_exhaustive]` may gain variants in a minor release, so match them with a
wildcard arm. Fields added to `FsEvent` variants or `Entry` are a breaking change. Behaviour that
the docs leave open, such as delivery timing within `latency` and history beyond FSEvents'
retention, may change without notice.

Limits: FSEvents history is finite. A `since_event_id` older than what macOS still keeps replays
what remains and gives no signal for the gap; `Dropped` and `RootChanged` are the signals to
re-crawl. Events are delivered over an unbounded channel. A consumer that stops draining it grows
its memory.

## Write budget and footprint cap (0.5.0)

`Config::write_budget = Some(WriteBudgetConfig::new(max_bytes_per_hour).with_max_footprint(bytes))` bounds what the index writes to `state_dir` and how large it may grow; `None` (default) enforces nothing.

* `Index::write_budget_status() -> WriteBudgetStatus` reports `bytes_last_hour` (rolling), `headroom_bytes`, `pending_write_bytes`, `footprint_bytes` and the deferral counters. `Index::would_exceed_write_budget(bytes) -> bool` and `WriteBudgetStatus::would_exceed(bytes)` let the caller hold back its own work. Writes are counted even when no budget is set.
* Counted bytes are the dirty pages each commit writes (tracked per column and name segment) plus what a rebuild writes. This is an estimate of disk writes, not a kernel measurement.
* With a budget set, a full rescan that would not fit (it rewrites about `footprint_bytes`) waits for headroom: `Status::rescan_deferred` carries the typed reason and one `ChangeBatch` announces it. Periodic commits that would pass the cap are coalesced (never more than ten minutes apart); `snapshot()`, `shrink()` and drop always commit. Compaction waits for the same headroom.
* Above the footprint cap, new entries are refused and `Status::footprint_capped` is set; nothing stored is removed. The store is re-measured every 256 adds.
* Steady-state persistence is already incremental (in-place column writes, only dirty pages flushed). The whole-store write is the full rescan or compaction, which the budget gates; the rescan still rebuilds a new store rather than diffing into the live one.

## Change handling, excludes, and rate limits (0.2.0)

* A folder is re-listed at most once per `Config::relist_min_interval` (default 5 s). While it keeps changing the interval doubles, up to `relist_max_interval` (60 s), and it returns to the floor once it is quiet. Changes inside the window are deferred to its end, not dropped. `status()` reports `relists_total`, `relist_rate_per_s` (last minute), and `backoff_folders`. Set the floor to zero to disable.
* Defaults: `watch_latency` 500 ms, `change_coalesce` 1 s.
* Build and cache folders are excluded by default: `target`, `node_modules`, `DerivedData`, `.build`, `.cache`, `.git/objects`, `Library/Caches`, `node_modules.*`, `.venv`, `__pycache__`, `.gradle`, `Pods`, `.turbo`, `.next`, `sccache`, `.sccache`, `.rightkit-managed`, `.git/logs`, `.git/lfs`, the scratch roots `/Volumes/D/scratch` (macOS) and `D:\scratch` (Windows), and any folder holding a valid `CACHEDIR.TAG`. Excluded folders stay in the index as empty, flagged entries. `Config::exclude_patterns` adds globs over path components (`*`, `?`). `Config::include_build_output` drops the defaults. The exact-path `Config::exclude` still works.

## Size semantics

Per entry, 64-bit: `logical` = data fork + resource fork length (`ATTR_FILE_DATALENGTH` +
`ATTR_FILE_RSRCLENGTH`); `allocated` = `ATTR_FILE_ALLOCSIZE` (all forks, on-disk blocks);
`private` = `ATTR_CMNEXT_PRIVATESIZE` (bytes freed if only this file were deleted; equals
`allocated` where the filesystem has no such attribute); `cloneid` = `ATTR_CMNEXT_CLONEID`.
Directories have no own size; folders and packages are pure rollups. Packages (`.app`,
`.photoslibrary`, ... by extension, extendable via config) carry the same totals and
`is_package = true`.

Folder rollup fields:

* `logical`, `allocated`: sum over files in the subtree, hard links counted once, every APFS
  clone counted in full (what `du`-style tools show).
* `allocated_unique` ("allocated, clones counted once"): for each file, its `private` bytes,
  plus, for each clone group (files sharing a clone id), the group's **largest shared part once per
  folder** containing at least one member. So three clones of a 4 MiB file in one folder are
  4 MiB unique, but each of two folders holding one clone shows 4 MiB.
* Limits: (1) shared extents are approximated per clone group by the largest member's shared
  bytes, so clones that diverged partially can be off by the diverged shared part; (2) a folder
  count is exact for that folder but folder values are **not additive**: the unique sizes of
  sibling folders can sum to more than their parent's; (3) blocks shared with APFS snapshots or
  between files that are not clone-id related are only counted as shared when `private < allocated`,
  and are then grouped by file id; (4) sparse and compressed files report their real allocation.
* Hard links: an inode contributes through its first indexed link (the *owner*); other links are
  flagged `is_secondary_link` and add nothing to any rollup, so a folder holding only a secondary
  link shows 0 for it. Ownership passes to the next link when the owner is removed. A change to
  one link's metadata is seen when that link's directory is re-synced.
* `SizeInfo` of a single file: `allocated_unique == allocated`.

## Live updates and subscriptions

FSEvents (directory granularity) drives per-directory `getattrlistbulk` diffs; new subdirectories
are crawled, removed ones dropped, and rollups adjusted incrementally (no re-walk). Rename and move
are a remove plus an add. `subscribe()` returns `ChangeBatch`es coalesced to at most one per
`Config::change_coalesce` (default 1 s, `src/config.rs:137`), bounded (`max_change_paths`, `max_change_rollups`; the
overflow is counted, not listed), carrying the changed paths and the **current rollups of the affected
folders and their ancestors only**, shallowest first. Queues are bounded; a full queue sets
`missed_batches` on the next batch. `rescanned` signals a full rebuild after dropped events.

## Privacy and Full Disk Access

Only directories are opened. Name and size mode never opens file contents (tested with mode-000
files and a source guard). Without Full Disk Access the crate does not fail: unreadable roots and
directories are reported in `status()`, and consent-gated folders (Desktop, Documents, Downloads)
are skipped unless `allow_consent_prompts` is set, because opening them raises a system prompt.
`~/Library/Mail`, Messages, Safari, Calendars and similar stores are skipped unless
`include_protected` is set. Skipped folders appear as empty, `excluded` entries.

## Content search (opt-in)

Cargo feature `content` (not compiled otherwise) **and** `Config::content.enabled = true`. Allowlist of roots,
max file size, text-extension filter, exclusion list; trigram index kept current on changes;
`Index::content_search(needle, limit)` verifies candidates by re-reading them. This is the only module that opens file
contents.

## Attribution

Approach and code adapted from [fsearch](https://github.com/noahdunnagan/fsearch) by Noah Dunnagan
(MIT): the `getattrlistbulk` entry parser and FSEvents binding (`src/backend/macos.rs`) and the fuzzy
and one-typo scoring (`src/query.rs`). Full licence text in `THIRD-PARTY-NOTICES`. No git or
crates.io dependency on upstream. This crate keeps 64-bit logical and allocated sizes (`SizeInfo`, `src/index.rs:24`),
rolls folders and packages up, and dedupes hard links and clones.

## Memory and on-disk layout

All per-entry state (name id, parent, first-child/sibling links, sizes, flags, ids) and the per-folder rollups live in
memory-mapped fixed-width column files under `Config::state_dir/index/` (anonymous maps when there is no state dir). Names are
interned in mapped segments with an on-disk chained hash. Resident memory is only the pages a query touches, the FSEvents working
set, the hard-link and clone-group tables (proportional to hard-linked and cloned files, not to the disk), and query scratch.
Updates are in-place column writes plus slot appends; freed slots are reused and the worker rebuilds the store (`compaction`,
depth-first, drops garbage names) when more than a quarter of slots are free or unused capacity exceeds 64 MiB & 20%. Each commit's dirty-page bytes feed the write budget above.

Crash safety: `header.bin` carries a `dirty` flag and a generation. The flag is made durable (msync) before the first column
write after a commit; a commit flushes dirty columns, then writes the header clean with the next generation (every 30 s while
changing, and on drop). A dirty header at open means a torn update: the store is discarded and re-crawled. A crawl or compaction builds
`index.new` and swaps it in; an interrupted swap is recovered on open.

Declared figures, not re-run or recorded in this repository (harness `benches/footprint.rs`, run as `rightkit cargo bench -p rightkit-fsindex --bench footprint -- 7700000`; entries generated straight into the columns,
fresh process reopening the store, macOS): ~114 bytes/entry on disk (836 MB at 7.7M); after reopen 3.5 MB phys footprint /
22.6 MB resident; after a name search 24.4 MB footprint / 185 MB resident (clean mapped pages touched by the scan; footprint does
not count them), back to 24 MB footprint / 29 MB resident after `trim()`, which the worker also does after large searches and
after 30 s idle. That is ~3 bytes/entry footprint and ~4 bytes/entry resident at rest.

## Known gaps (v1)

Name search is name-only (no path tokens); a search touches clean mapped pages (transient resident spike, ~25 bytes/entry, released by the trim); hard-link
and clone-group tables are resident (large pnpm-style hard-link stores grow them); the event-ID resume cannot detect a purged FSEvents history beyond the snapshot age limit; no
daemon/IPC; Windows has the Windows Search backend only (no change feed, indexed locations only); MFT/USN path resolution is not done.

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 185.0 MiB.
- CPU ceiling (idle, steady state): 5%.
- Wake-ups ceiling: 3 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 1,000,000 entries or more.
- Required hosts: macOS only (rightkit-fsindex has no Windows backend (crates/rightkit-fsindex/README.md:25, :159 'no Windows backend'); the public APIs are macOS-only.).
- Status exposure: `Index::status()` in `src/index.rs`; exposes resident memory, idle CPU.
- Measured: not yet measured.

Oversized rescans and automatic compactions (estimated bytes above the entire hourly cap)
run once trailing-hour writes are at most one quarter of that cap, or two hours after
the first request, whichever comes first. Repeated requests do not reset this deadline.
Subtree rescans check replacement headroom before pruning. A full crawl cut short by the
footprint cap never replaces the old store; `RescanDeferral::FootprintCap` reports the hold.

## Storage (0.5.0)

Clones use device-qualified keys with separate clone-ID/file-ID namespaces; singletons allocate no multi-member group. Opening an older store preserves its legacy rollup algebra until compaction; `Index::storage_status().clone_rollups_need_rewrite` reports this. A rewrite recomputes rollups. New stores use RKFSIX03; older readers reject them and rebuild.

Large columns grow with 12.5% headroom; rewrites start at live need plus 6.25%. Name segments grow to need. Compaction also considers unused capacity (64 MiB and 20%). Dirty-column commits skip clean files; identical clean cursors write nothing until the hourly header refresh. Changed event IDs persist when committed. `snapshot` forces a header commit. Byte counters count successful logical flush ranges in page units, not physical device writes.
