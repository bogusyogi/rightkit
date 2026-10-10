# rightkit-fsindex

Live whole-disk file index for RightKit apps (Pulse and others): fuzzy,
typo-tolerant **name search** plus **folder sizes** that are correct for a
storage app. Library API only; a daemon/IPC can come later. `published to crates.io`.

```rust
let mut cfg = Config::new(vec!["/".into()]);
cfg.state_dir = Some(app_support.join("fsindex")); // snapshot + mmap'd names; enables resume
let index = Index::open(cfg)?;                      // blocks for the first crawl or snapshot load

index.search("invoice 2024")?;                      // Vec<Hit>, ranked
index.folder_size("/Users/me/Library")?;            // SizeInfo { logical, allocated, allocated_unique, files, is_package }
index.children_sorted_by_size("/Users/me")?;        // Vec<Item>, allocated_unique desc by default
let changes = index.subscribe();                    // Receiver<ChangeBatch>
index.status();                                     // roots readable?, skipped/unreadable dirs, FDA, resume
```

## Platform support

macOS (v1): `getattrlistbulk` crawl (parallel, std threads), FSEvents upkeep with event-ID
resume from a snapshot, full-rescan fallback on dropped events, subtree rescan on
must-scan-subdirs/root-changed/mount events.

Windows (planned, only a stub today): the `Backend` trait (`src/backend/mod.rs`) is the seam.
`UnsupportedBackend` returns `Error::Unsupported`. The plan: enumerate the NTFS MFT
(`FSCTL_ENUM_USN_DATA` or direct `$MFT` read with elevation, falling back to
`FindFirstFileExW` with `FIND_FIRST_EX_LARGE_FETCH` when not elevated) for the initial crawl, using
`GetFileInformationByHandleEx`/`FileStandardInfo` for `AllocationSize` and `EndOfFile`, and the volume
file ID for hard-link dedupe (`NumberOfLinks`); follow the USN journal (`FSCTL_READ_USN_JOURNAL`)
for changes, resuming from a stored (journal id, USN) and rescanning when the journal id changed
or the USN fell off the journal; clones map to ReFS block cloning where available, otherwise
`allocated_unique == allocated`. Packages do not exist on Windows. Everything above the trait (store,
rollups, query, subscriptions, snapshot) is platform-neutral.

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
`Config::change_coalesce` (default 150 ms), bounded (`max_change_paths`, `max_change_rollups`; the
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
crates.io dependency on upstream. Unlike upstream this crate keeps 64-bit logical and allocated sizes,
rolls folders and packages up, and dedupes hard links and clones.

## Memory and on-disk layout

All per-entry state (name id, parent, first-child/sibling links, sizes, flags, ids) and the per-folder rollups live in
memory-mapped fixed-width column files under `Config::state_dir/index/` (anonymous maps when there is no state dir). Names are
interned in mapped segments with an on-disk chained hash. Resident memory is only the pages a query touches, the FSEvents working
set, the hard-link and clone-group tables (proportional to hard-linked and cloned files, not to the disk), and query scratch.
Updates are in-place column writes plus slot appends; freed slots are reused and the worker rebuilds the store (`compaction`,
depth-first, drops garbage names) when more than a quarter of the slots are free.

Crash safety: `header.bin` carries a `dirty` flag and a generation. The flag is made durable (msync) before the first column
write after a commit; a commit flushes every column, then writes the header clean with the next generation (every 30 s while
changing, and on drop). A dirty header at open means a torn update: the store is discarded and re-crawled. A crawl or compaction builds
`index.new` and swaps it in; an interrupted swap is recovered on open.

Measured (`rightkit cargo bench -p rightkit-fsindex --bench footprint -- 7700000`, entries generated straight into the columns,
fresh process reopening the store, macOS): ~114 bytes/entry on disk (836 MB at 7.7M); after reopen 3.5 MB phys footprint /
22.6 MB resident; after a name search 24.4 MB footprint / 185 MB resident (clean mapped pages touched by the scan; footprint does
not count them), back to 24 MB footprint / 29 MB resident after `trim()`, which the worker also does after large searches and
after 30 s idle. That is ~3 bytes/entry footprint and ~4 bytes/entry resident at rest.

## Known gaps (v1)

Name search is name-only (no path tokens); a search touches clean mapped pages (transient resident spike, ~25 bytes/entry, released by the trim); hard-link
and clone-group tables are resident (large pnpm-style hard-link stores grow them); the event-ID resume cannot detect a purged FSEvents history beyond the snapshot age limit; no
daemon/IPC; no Windows backend.
