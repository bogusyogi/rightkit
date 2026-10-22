# Changelog

## 0.5.0 (2026-10-22)

Write budget and footprint cap, for the Pulse hub index that reached ~1 GB on disk and wrote 1.4 to 2.2 GB/h during a crawl. Defaults keep today's behaviour: with `Config::write_budget` unset nothing is deferred or refused. Existing `RKFSIX02` stores open without rewriting their header or rollups & retain legacy clone keys through incremental commits. Fresh stores & rewrites use `RKFSIX03` for device-qualified clone keys; column element layouts stay compatible, while name segments may be shorter. Older readers reject `RKFSIX03` & rebuild.

- `Config::write_budget: Option<WriteBudgetConfig>` (default `None`). `WriteBudgetConfig::new(max_bytes_per_hour)` and `.with_max_footprint(bytes)` (`#[non_exhaustive]`). A zero hourly cap means none.
- Accounting: columns and name segments track the pages written since the last commit; a commit adds the bytes of those pages (plus the header) to a rolling-hour counter, and a rebuild (full rescan, compaction) adds what it wrote. Counted even with no budget. It is the dirty-page byte count the crate flushes, an estimate of what reaches the disk (the kernel may also write a page earlier or again).
- `Index::write_budget_status() -> WriteBudgetStatus` (`bytes_last_hour`, `bytes_total`, `headroom_bytes`, `pending_write_bytes`, `footprint_bytes`, `footprint_capped`, `refused_adds`, `commits_deferred`, `rescans_deferred`, `rescan_deferred`, caps), `WriteBudgetStatus::would_exceed(bytes)` and `Index::would_exceed_write_budget(bytes)`.
- Full rescans: with a budget, a wanted full rescan waits until the rolling hour has room for the store's footprint (what it rewrites). The request stays pending, the cooldown does not grow, and the wait is reported as `Status::rescan_deferred = Some(RescanDeferral::WriteBudget { needed_bytes, headroom_bytes, retry_in })` and once as `ChangeBatch::rescan_deferred`. A rescan larger than the whole cap runs once trailing-hour writes fall to one quarter of the cap, or two hours after its first request, whichever comes first. Incremental changes keep applying meanwhile. Compaction waits for the same headroom.
- Periodic commits: a commit that would pass the cap is held back and coalesced into the next one; ten minutes without a commit forces one. `Index::snapshot`, `shrink` and drop always commit.
- Footprint cap: above `max_footprint_bytes`, new entries are refused (`Status::footprint_capped`, `WriteBudgetStatus::refused_adds`); stored entries are never removed. The size is re-measured every 256 adds, so the cap can be passed by that many entries.
- Not changed: the in-place column update plus commit already writes only dirty pages, so steady-state persistence is incremental. The full rescan still rebuilds into a new store (the whole-footprint write the budget now gates); a diff-into-live-store rescan was not built.
- Clone fix: clone groups are keyed by device-qualified file identity with separate clone-ID and file-ID namespaces, so two volumes no longer merge unrelated groups (the 12,038,144 B unique-rollup over-count). Singletons allocate no multi-member group; a group is created when a second member joins and promotion/demotion is handled. Stores written before this keep their legacy rollup algebra until the next rewrite; `Index::storage_status().clone_rollups_need_rewrite` reports it. New generations use RKFSIX03; older readers rebuild rather than interpret the new clone semantics.
- Capacity: large columns grow with 12.5% headroom instead of doubling, name segments are sized to need, rewrites start at live need plus 6.25%, and compaction also considers unused capacity (at least 64 MiB and 20%). Unreferenced names count as reclaimable.
- Changed-column commits: a commit writes only dirty columns, a clean commit is a no-op, changed event cursors persist immediately, the header refreshes hourly, and `snapshot` forces a header commit. Byte counters count successful logical flush ranges in page units, not physical writes.
- `Index::storage_status() -> StorageStatus` (`#[non_exhaustive]`; capacity, live, unused and per-entry bytes, slots, names, `wants_compaction`, `clone_rollups_need_rewrite`) is additive; no public struct gained a field for it.
- Public additions: `Config::write_budget`, `WriteBudgetConfig`, `WriteBudgetStatus`, `RescanDeferral` (`#[non_exhaustive]`), `Status::{rescan_deferred, footprint_capped}`, `ChangeBatch::rescan_deferred`. New public fields on `Config`, `Status` and `ChangeBatch` make this a minor (0.x breaking) release for code that builds them with struct literals.

## 0.4.1 (2026-10-17)

- Depend on rightkit-child-process 0.5.0 (and rightkit-sandbox 0.5.0 where used); no behaviour change.

## 0.4.0

Windows backend (Orthic-Labs/rightkit#228, for Pulse #177). Additive: no existing item changed. Pulse wires it behind a default-Off setting; nothing indexes a Windows disk unless the host opts in.

- Windows Search first: `WindowsSearchBackend` queries the `SystemIndex` catalog through the OLE DB provider `Search.CollatorDSO` with Windows Search SQL (`SELECT System.ItemPathDisplay, System.Size, System.DateModified, System.FileAttributes ... WHERE SCOPE='file:...'` / `DIRECTORY='file:...'` / `System.FileName LIKE ...`). It is the default backend on Windows, so `Index::open` works there with `Config::watch = false` (there is no change feed yet; `watch` returns `Unsupported`).
- Direct APIs on every platform (COM calls report `Unsupported` off Windows): `search_names(text, &SearchOptions) -> Vec<WindowsSearchHit>` (the crate's `Hit` plus `modified_unix`), `folder_rollup(scope, max_rows) -> WindowsRollup` (`SizeInfo`, row and folder counts, `truncated`), `windows_search_status(scope) -> WindowsSearchStatus { service, provider_registered, scope_indexed, reason }` with `available()`. The status separates a missing or stopped `WSearch` service, an unregistered provider, and a folder the index does not cover; `Backend::check_supported` returns `Error::Unsupported(reason)` in those cases.
- SQL safety: `escape_sql_literal` doubles single quotes and replaces control characters; `like_pattern` turns the LIKE wildcards `% _ [ ]` into `_` (Windows Search SQL has no ESCAPE clause), and every returned name is re-checked against the original text, so the neutralised pattern can only over-select. Scopes must be absolute paths.
- Sizes are the index's logical `System.Size`: `allocated` and `allocated_unique` equal `logical`, hard links are not deduplicated, and only indexed locations are visible (by default the profile and Start menu, not the whole disk).
- USN journal reader, `backend::windows_usn`: `UsnJournal::open(letter)`, `query()` (`FSCTL_QUERY_USN_JOURNAL`), `read(start_usn, reason_mask, journal_id)` (`FSCTL_READ_USN_JOURNAL`, V2 records) and `enumerate_files(sink)` (`FSCTL_ENUM_USN_DATA`). Opening `\.\X:` needs an elevated token: otherwise `UsnError::PermissionDenied`; FAT/exFAT/network volumes give `UsnError::Unsupported`; a missing journal gives `JournalNotActive`; a USN that fell off the journal gives `JournalEntryDeleted` (rescan). `parse_usn_records` and `UsnError` are available on every platform; `UsnError` converts into `Error`.
- Not done yet: resolving file reference numbers to paths and driving `Backend::watch` from the journal (the MFT/USN backend proper). Documented as TODO in `windows_usn.rs`.
- New target dependency on Windows: `windows` 0.62 (the version the workspace already uses) with the `Win32_System_Search`, `Win32_Storage_IndexServer`, `Win32_System_Com`, `Win32_System_Ole`, `Win32_System_Variant`, `Win32_System_Services`, `Win32_System_IO`, `Win32_Storage_FileSystem`, `Win32_Security` and `Win32_Foundation` features.
- `tests/unsupported.rs`: the "default backend is Unsupported" test now excludes Windows too.

## 0.3.0

Churn limits, after a pulse-hub ran at ~700% CPU for 15 h on a busy USB APFS volume.

- Crawl threads: `Config::crawl_threads` is clamped to 1..=8, and crawls draw their workers from one process-wide budget of 8. Concurrent crawls queue instead of multiplying threads.
- Full rescans are coalesced. FSEvents drop flags only mark a rescan wanted; it runs at once the first time, then at most once per cooldown (30 s, doubling to 10 min, back to 30 s after 30 quiet minutes). A failed rescan is retried after the cooldown. Incremental changes keep applying while one is waiting.
- `Config::relist_budget: Option<(burst, interval)>` (default `Some((8, 5 s))`; `None`, a zero burst or a zero interval disables it). `Status::relists_pending` and `Status::rescan_pending` say when the index is consistent again: both clear (0 and false) means no subtree re-list or full rescan is waiting or running. Call `Index::flush()` first. A deferred re-list is drained by a worker timer, not by the next event batch, so a quiet volume after a storm still converges.
- `Config::rescan_cooldown` (default 30 s) sets the base cooldown; zero disables it, so every drop rescans at once as before.
- Subtree re-lists (must-scan, root-changed, mount, unmount) pass a global token bucket (burst 8, one more per 5 s). The excess waits in a pending set and drains under the budget; nothing is dropped. A pending folder absorbs pending folders below it.
- `Status` gained `rescans_coalesced` and `rescan_backoff_ms` (`full_rescans` already existed).
- Events under `Config::state_dir` are skipped, and the directory is excluded when it lies under a root. The typed `FsEventsWatcher` is unchanged.
- Default excludes added: `node_modules.*`, `.venv`, `__pycache__`, `.gradle`, `Pods`, `.turbo`, `.next`, `sccache`, `.sccache`, `.rightkit-managed`, `.git/logs`, `.git/lfs`; the absolute scratch roots `/Volumes/D/scratch` (macOS) and `D:\scratch` (Windows, case-insensitive) as path prefixes; and any directory holding a valid `CACHEDIR.TAG` (reads at most 43 bytes of that file). All drop with `Config::include_build_output`. `Excludes` gained `ci_prefixes` and `honor_cachedir_tag`. The fingerprint changes, so existing snapshots rebuild once.
- `runtime-budget.json` declares idle-CPU and wake-up ceilings and records the caps.


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
