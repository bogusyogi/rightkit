# rightkit-cache

Cache primitives shared by Right Suite apps:

- `DiskCache`: a size-capped byte cache with a hard size cap, least-recently-used eviction that survives restarts, atomic writes, group invalidation, and fetch-or-cache (`get_or_fetch`). Use it for attachments, remote images and similar blobs.
- `CacheReservation` (`DiskCache::reserve_path`): a temp path a file producer writes, then `commit()` or `abort()`. Use it for thumbnails, frames and waveforms that ffmpeg or an encoder writes to disk.
- `CacheSet`: several cache members managed together, either several roots or namespaces under one root (`CacheSet::open_namespaces`). It aggregates stats and applies a combined byte budget (`enforce_budget`).
- `Key`: unambiguous slot keys (length-prefixed SHA-256), optionally grouped so a whole group can be cleared at once.
- `prefix`: stable-prefix prompt-cache keys and break tracking (`CacheTracker`) ported from CodeRight. Only the stable tier (identity and UTC day) enters the key. Volatile content such as per-turn memory or timestamps must stay out.

Catalog capability: `disk-cache`. Registry: crates.io 0.1.0; this source is 0.2.0 (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it for a bounded, restart-safe disk cache with explicit size and entry limits.
- Use `reserve_path` when a tool writes the file itself, so the cache receives the finished file atomically and never an empty or partial one.
- Use `CacheSet::open_namespaces` to give thumbnails, frames and waveforms separate budgets under one directory.
- Use `DiskCache::open_read_only` to inspect a cache (a settings screen, a diagnostic) without any filesystem writes.
- Use `prefix` to build stable cache keys for a provider's prompt cache.

Do not use it as an in-memory cache. Data that must never be evicted belongs in `put_pinned`, and the pinned total must stay under the cap.

## Cargo features

None. The crate has no `[features]` table. Its dependencies are `sha2`, `rightkit-fs` (`fs` feature, default features off) and `rightkit-process` (`Cargo.toml:13-16`).

## Usage

Byte entries (from `tests/disk_e2e.rs`):

```rust
use rightkit_cache::{Config, DiskCache, Key, PutOutcome};

let cache = DiskCache::open(root, Config::new(8).max_entry_bytes(8))?;
let key = Key::grouped(&["account-a"], &["old"]);
assert_eq!(cache.put(&key, b"1111")?, PutOutcome::Stored);
assert_eq!(cache.get(&key).as_deref(), Some(b"1111".as_slice()));
assert_eq!(cache.clear_group(&["account-a"]), 1);
```

File producers (from `tests/reservation_set.rs`):

```rust
let reservation = cache.reserve_path(&key, "jpg")?;      // <root>/.tmp/<pid>-<n>.jpg
run_ffmpeg_to(reservation.path())?;                        // the producer writes the file
match reservation.commit()? {                              // moved in atomically, size-accounted
    PutOutcome::Stored => {}
    PutOutcome::TooLarge => {}                             // over budget: nothing changed
}
let path = cache.get_path(&key);                           // Some(<root>/<hh>/<hash>-<group>.jpg)
```

Dropping a reservation without committing removes its temp file.

Shared root with separate budgets:

```rust
let set = CacheSet::open_namespaces(
    project.join(".cache"),
    &[("thumbs", Config::new(256 << 20)), ("waveform", Config::new(64 << 20))],
)?;
let totals = set.stats();          // entries, bytes, oldest, newest, per-member stats
set.enforce_budget();              // only when a combined budget was set with with_budget()
```

Read-only inspection:

```rust
let view = DiskCache::open_read_only(root)?;   // no creation, no cleanup, no writes
let stats = view.stats();
```

`Config::new(max_bytes)` sets the cap; `max_entry_bytes` sets the per-entry limit; `stale_tmp_after` sets how old a leftover temp file may be before `open` sweeps it (default 1 hour). An entry over the per-entry limit is refused without destroying existing entries.

`Config::owner_only()` creates entries, temp files and the cache's directories owner-only: `0600` files and `0700` directories on Unix; on Windows a protected DACL for the current user. The default keeps the platform's default permissions. A file a producer writes through `reserve_path` is restricted when it is committed, not when the producer creates it.

Streaming: `put_writer(key)` returns a `PutWriter` (an `io::Write`) and `put_reader(key, reader)` copies a reader in chunks, so a large value never has to be in memory. The bytes go to a temp file and are published atomically on `commit()`. A writer dropped without committing removes its temp file.

Pinning: `put_pinned`, `pin` and `unpin` make an entry exempt from size-pressure eviction. Pinned bytes count toward `max_bytes`. A write that would leave no room beside the pinned entries fails with a `PinnedCapacityError` (use `pinned_capacity_error(&err)` to read it, kind `StorageFull`) and writes nothing. Pins persist in `<root>/.pinned`. An absent list pins nothing; a list that exists but cannot be read fails `open` and `open_read_only` with the read error and evicts nothing, because the pins would be unknown. Use `pinned_bytes()` to read the total.

Foreign files: the root must be exclusive to the cache. The default `open` never deletes a file whose name the cache did not write, including in `.tmp/`, but it still indexes the `<hh>/` shards. `open_strict` refuses (with a `ForeignContentError`, see `foreign_content_error`) a root that holds any file the cache did not write, and touches nothing.

## On-disk layout

`<root>/<hh>/<keyhash>-<group>` for byte entries and `<root>/<hh>/<keyhash>-<group>.<ext>` for file entries, plus `<root>/.tmp/` for in-flight writes and `<root>/.pinned` for the pin list. `open` sweeps `.tmp` leftovers whose names the cache writes (dead owner or older than `stale_tmp_after`); `open_read_only` sweeps nothing. The root must be exclusive to the cache: keep other state in its own directory.

## Platform support

The crate uses `std::fs` and has no platform modules. Owner-only temp files set mode `0600` on Unix (`cfg(unix)`) and call `rightkit_fs::private::restrict_owner_only` elsewhere (`src/disk.rs:244-250`). Atomic write behaviour depends on the host filesystem's rename semantics. Reservations rename within the cache root, so `.tmp` and the shards stay on one volume.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are in [CHANGELOG.md](CHANGELOG.md).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 56.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 100,000 entries or more.
- Required hosts: macOS and Windows.
- Status exposure: `DiskCache::status()` in `src/disk.rs`; exposes resident memory, idle CPU, wake-ups, disk operations. Wake-ups are macOS only and disk operations are Windows and Linux only (`runtime-budget.json`, `status.note`).
- Measured: 0.2.0 on macOS arm64 (`runtime-budget.evidence/macos-arm64-0.2.0.txt`) and Windows x86_64 (`runtime-budget.evidence/windows-x86_64-0.2.0.txt`). `runtime-budget.json` lists no `measurements`, so the generator prints "not yet measured" until it does.
