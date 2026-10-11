# Changelog

## Unreleased

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
