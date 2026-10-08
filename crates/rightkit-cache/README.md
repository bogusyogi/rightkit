# rightkit-cache

Cache primitives shared by Right Suite apps:

- `DiskCache`: a size-capped byte cache with a hard size cap, least-recently-used eviction that survives restarts, atomic writes, group invalidation, and fetch-or-cache (`get_or_fetch`). Use it for attachments, remote images and similar blobs.
- `Key`: unambiguous slot keys (length-prefixed SHA-256), optionally grouped so a whole group can be cleared at once.
- `prefix`: stable-prefix prompt-cache keys and break tracking (`CacheTracker`) ported from CodeRight. Only the stable tier (identity and UTC day) enters the key. Volatile content such as per-turn memory or timestamps must stay out.

Catalog capability: `disk-cache`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it for a bounded, restart-safe disk cache with explicit size and entry limits.
- Use `prefix` to build stable cache keys for a provider's prompt cache.

Do not use it as an in-memory cache, and do not use it for data that must never be evicted.

## Cargo features

None. The crate has no `[features]` table. Its only dependency is `sha2`.

## Usage

Taken from `tests/disk_e2e.rs`, which runs against a real temporary directory:

```rust
use rightkit_cache::{Config, DiskCache, Key, PutOutcome};

let cache = DiskCache::open(root, Config::new(8).max_entry_bytes(8))?;
let key = Key::grouped(&["account-a"], &["old"]);
assert_eq!(cache.put(&key, b"1111")?, PutOutcome::Stored);
assert_eq!(cache.get(&key).as_deref(), Some(b"1111".as_slice()));
assert_eq!(cache.clear_group(&["account-a"]), 1);
```

`Config::new(max_bytes)` sets the cap; `max_entry_bytes` sets the per-entry limit. The test shows that an entry over the per-entry limit is refused without destroying existing entries.

## Platform support

Pure `std::fs` code with no platform-specific modules. Atomic write behaviour depends on the host filesystem's rename semantics.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
