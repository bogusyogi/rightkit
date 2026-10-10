# rightkit-cache

Cache primitives shared by Right Suite apps:

- `DiskCache`: a size-capped byte cache with a hard size cap, least-recently-used eviction that survives restarts, atomic writes, group invalidation, and fetch-or-cache (`get_or_fetch`). Use it for attachments, remote images and similar blobs.
- `CacheReservation` (`DiskCache::reserve_path`): a temp path a file producer writes, then `commit()` or `abort()`. Use it for thumbnails, frames and waveforms that ffmpeg or an encoder writes to disk.
- `CacheSet`: several cache members managed together, either several roots or namespaces under one root (`CacheSet::open_namespaces`). It aggregates stats and applies a combined byte budget (`enforce_budget`).
- `Key`: unambiguous slot keys (length-prefixed SHA-256), optionally grouped so a whole group can be cleared at once.
- `prefix`: stable-prefix prompt-cache keys and break tracking (`CacheTracker`) ported from CodeRight. Only the stable tier (identity and UTC day) enters the key. Volatile content such as per-turn memory or timestamps must stay out.

Catalog capability: `disk-cache`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it for a bounded, restart-safe disk cache with explicit size and entry limits.
- Use `reserve_path` when a tool writes the file itself, so the cache receives the finished file atomically and never an empty or partial one.
- Use `CacheSet::open_namespaces` to give thumbnails, frames and waveforms separate budgets under one directory.
- Use `DiskCache::open_read_only` to inspect a cache (a settings screen, a diagnostic) without any filesystem writes.
- Use `prefix` to build stable cache keys for a provider's prompt cache.

Do not use it as an in-memory cache, and do not use it for data that must never be evicted.

## Cargo features

None. The crate has no `[features]` table. Its only dependency is `sha2`.

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

`Config::new(max_bytes)` sets the cap; `max_entry_bytes` sets the per-entry limit. An entry over the per-entry limit is refused without destroying existing entries.

## On-disk layout

`<root>/<hh>/<keyhash>-<group>` for byte entries and `<root>/<hh>/<keyhash>-<group>.<ext>` for file entries, plus `<root>/.tmp/` for in-flight writes. `open` removes `.tmp` leftovers; `open_read_only` does not.

## Platform support

Pure `std::fs` code with no platform-specific modules. Atomic write behaviour depends on the host filesystem's rename semantics. Reservations rename within the cache root, so `.tmp` and the shards stay on one volume.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are in [CHANGELOG.md](CHANGELOG.md).
