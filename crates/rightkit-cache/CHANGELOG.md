# Changelog

## 0.2.3 — unpublished staging

- Added `prefix::fnv1a_64`: standard FNV-1a 64 (offset basis 14695981039346656037), checked against the FNV reference vectors. `prefix::fnv1a` used a non-standard offset basis (1469598103934665603, last digit missing); it is unchanged, so existing cache-slot names and persisted keys keep their values, and is now `#[deprecated]` with that explanation. New keys use `fnv1a_64` (PostRight #M15 gap).

## 0.2.2 (2026-10-17)

- Depend on rightkit-child-process 0.5.0 (and rightkit-sandbox 0.5.0 where used); no behaviour change.

## 0.2.1 (2026-10-17)

- Added `DiskCache::move_to(to) -> io::Result<MoveReport>` (rightkit#241, for Ember): relocate every entry to a new root, keeping keys, contents, recency (mtime), pins and the in-memory index, so the same handle keeps working. Each entry is renamed, falling back to copy then remove across volumes; an existing destination file is kept and counted `redundant`; per-entry failures land in `MoveReport::failed` and those entries leave the cache. The pin list is written to the new root before anything moves, and the old root's `.pinned`, empty shards and empty `.tmp` are removed afterwards (the old directory and foreign files stay). Refused with nothing moved: read-only handles, and a root nested in or containing the current one; moving to the current root is a no-op. Writes in flight during a move may fail to commit. New `DiskCache::location()` returns the current root; `root()` still returns the opening root (kept for compatibility), and `CacheSet::add` now compares `location()`.
- Added `DiskCache::set_budget(max_bytes) -> io::Result<Cleared>` (rightkit#241): change the cap at runtime and evict least-recently-used unpinned entries down to it, returning what was evicted. Pinned entries are never evicted, so pinned bytes above the new cap leave the cache over it and unpinned writes then fail with `PinnedCapacityError` until the cap grows or pins are released. `config()` and `status()` report the current cap. Not persisted; a reopen uses the `Config` given to `open`. Read-only handles fail with `PermissionDenied`. `Cleared` and `MoveReport` are now re-exported at the crate root.
- Foreign directories (rightkit#241 question): unchanged. `open` already tolerates foreign files (it indexes only `<hh>/<keyhash>-<group>` names and sweeps only its own `.tmp` names), and `open_strict` is the refusing variant; existing Ember `<2 hex>/<32 hex>.jpg` files are not cache entries (they are reported by `foreign_files` and removable with `clear_foreign`). The module doc's exclusive-root rule stays.
- Tests: `tests/move_and_budget.rs` (move keeps entries, pins, recency and the index; redundant destination; failed entry; nested, same and read-only roots; shrink evicts LRU first; grow evicts nothing; pinned bytes respected above and below the cap; read-only refused). Not compiled or run; verification is the CI run. Cargo.lock still lists 0.2.0 and needs the version refresh.
- Fix: `DiskCache::open`, `open_strict` and `open_read_only` no longer treat a pin list `<root>/.pinned` that cannot be read as empty. Only an absent file means no pins. Any other read error (a permission error, a sharing violation while another process replaces the file, an I/O error) now fails the open with that error before anything is evicted; before, the pins were taken as empty and the open-time trim to `max_bytes` could evict pinned entries. The read is not retried here. Not compiled or run locally; verification is the CI run.
- Eviction no longer sorts the whole index on every over-cap write: the index keeps the eviction order (last use, then base name) and the pinned total current as entries change, so a put costs the entries it evicts plus a logarithmic step. The evicted set is unchanged; `recency_snapshot` reads the same order without a sort.

## 0.2.0 - 2026-10-11

Additive release for file producers and multi-root apps. The 0.1.0 byte API is unchanged.

- Streaming writes (`put_writer` / `PutWriter`, `put_reader`), a pinned class (`put_pinned`, `pin`, `unpin`, `pinned_bytes`, persisted in `.pinned`; pinned bytes count toward the cap, are never evicted, and make an unpinned write fail with `PinnedCapacityError`), owner-only entries and temp files (`Config::owner_only()`, default off), and `open_strict` refusing a root with foreign files (`ForeignContentError`); the default sweep now touches only temp names the cache writes, so an old foreign file in `.tmp/` is kept. Breaking: `Config` gains `owner_only` (struct literals must set it).
- Added `DiskCache::reserve_path(key, ext)` and `CacheReservation`: a temp path inside `<root>/.tmp/` that a producer (ffmpeg, an image encoder) writes. `commit()` moves it into the cache atomically with size accounting; `abort()`, or dropping the reservation, removes it. Empty or missing producer output is an error. Over-budget output returns `PutOutcome::TooLarge`.
- Added `DiskCache::get_path(key)`: the stored file path for a reserved entry, marked most recently used.
- Stored entries may carry an extension (`<hash>-<group>.<ext>`, 1 to 8 ASCII alphanumerics). Entries written with `put` keep the extension-less name. Cache directories written by 0.1.0 open unchanged.
- Added `DiskCache::open_read_only(root)`: indexes an existing cache with zero filesystem writes. It does not create the root, remove `.tmp` leftovers, trim to the cap, or refresh mtimes. Mutating calls fail or do nothing. `DiskCache::is_writable()` reports the mode.
- `Stats` gained `oldest` and `newest` (last-use times of the least and most recently used entries). This is a struct-literal breaking change for code that constructs `Stats` directly; reads are unaffected.
- Added `CacheSet`: several members (roots, or namespaces under one root) with `add`, `get`, aggregated `stats()` (`SetStats` with entries, bytes, oldest, newest and per-member stats), and `enforce_budget()`, a combined byte budget that evicts the globally least recently used entries across members.
- Added `CacheSet::open_namespaces(root, specs)`: namespaces `<root>/<name>` with separate budgets, so thumbnails, frames and waveforms can share one root.
- Member caps still hold on every write; the combined budget is applied by `enforce_budget()`.
- `DiskCache::open` no longer deletes `<root>/.tmp/`. It sweeps temp files whose owner pid is dead,
  or that are older than `Config::stale_tmp_after` (default 1 hour; new builder, `DEFAULT_STALE_TMP_AGE`); a young temp file of a live owner is kept, so a second process
  opening the cache cannot delete another producer's in-flight output. Reservations and puts record
  their owner pid in a `<temp>.owner-pid` sidecar, removed with the temp file.
- Added `DiskCache::foreign_files() -> io::Result<ForeignFiles { count, bytes }>`: regular files under
  the root that are neither cache entries nor `.tmp/` contents. `DiskCache::clear_foreign()` deletes
  them and returns what it removed; it fails with `PermissionDenied` on a read-only handle. Cache
  entries and `.tmp/` are never touched.
- Breaking: `Config` gains a `stale_tmp_after` field; a struct literal must set it (`Config::new` is unchanged).
- Breaking: `DiskCache::clear()` returns `Cleared { entries, bytes }` instead of `usize`. The old count
  is `clear_count()`, or `clear().entries`.

## 0.1.0

- Initial publication: size-capped `DiskCache`, `Key`, and the `prefix` stable-prefix prompt-cache keys.
