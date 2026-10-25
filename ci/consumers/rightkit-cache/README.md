# rightkit-cache 0.2.3 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-cache =0.2.3` exactly. The crate has no features, so none are selected; this is the dependency MailRight and CodeRight declare. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion. Every scenario runs in its own directory under one per-process temporary directory.

## What it proves

- Keys. `Key::new`, `Key::grouped` and `Key::url` give the length-prefixed SHA-256 the crate documents: several literal digests computed independently of the crate, no collision between `("ab","c")` and `("a","bc")`, `group_id` (16 hex characters), and a grouped key is a different slot from an ungrouped one.
- Byte entries. `DiskCache::open`, `put`, `get`, `contains`, `remove`, `clear_group`, `clear` (`Cleared`) and `clear_count`; replacing an entry keeps one copy; hits, misses and evictions in `Stats`; the hard cap with least-recently-used eviction in which a `get` refreshes recency; the per-entry limit, where `PutOutcome::TooLarge` leaves the existing value alone; `Config` defaults and builders; `get_or_fetch` (fetch once, a hit never fetches, a fetch error caches nothing); `set_budget` shrinking and growing; `status`, `config`, `root` and `location`.
- Restart. Recency written to disk decides what a reopen with a smaller cap evicts, and a second shrink keeps only what fits. Operations are separated by 120 ms so file modification times are distinct.
- Streaming writes. `put_writer` with `commit`, nothing published before it, a writer dropped without commit leaves no entry, `put_reader`, and an over-limit write reporting `TooLarge`.
- Pinning. `put_pinned`, `pin`, `unpin` and `pinned_bytes`; a write that leaves no room fails with kind `StorageFull`, read through `pinned_capacity_error` (`PinnedCapacityError`), and writes and evicts nothing; size pressure never evicts a pinned entry; pins survive a restart.
- File producers. `reserve_path` with the producer's extension, `commit` (then `get_path` and `get` return the file), an empty or missing producer file is an error, `abort` and drop remove the temporary file, an invalid extension is refused, and an over-limit file is `TooLarge` and removed.
- Inspection and ownership. `open_read_only` serves reads and stats and refuses or ignores every mutation (`PermissionDenied`), a missing root is `NotFound` and is not created; `open_strict` refuses a root holding a file the cache did not write (`ForeignContentError`, read through `foreign_content_error`) and touches nothing, the default `open` never deletes such a file, `foreign_files` and `clear_foreign`; `move_to` relocates entries and pins and keeps the handle working, is a no-op for the current root, and refuses a nested one; `Config::owner_only` entries read back, and on Unix are `0600` files.
- Cache sets. `CacheSet::open_namespaces`, `valid_namespace`, `get`, aggregated `stats`, `add` refusing a repeated name or directory and an invalid name, an invalid name creating nothing, and `enforce_budget` evicting the globally least recently used entry across two roots.
- Prompt-cache keys. `fnv1a_64` reference vectors, the unchanged legacy `fnv1a` basis, `StableCacheKey` and `ConductorStableKey` strings (literals computed independently), `CONDUCTOR_CACHE_KEY_PREFIX`, `system_prompt_only_cache_key`, `parse_iso_day`, and `CacheTracker` `Fresh`, `Hit` and `Break` with the break attribution (`tool-defs-changed`, `system-prefix-changed`).

## What it deliberately does not prove

- The 100,000-entry runtime budget, behaviour under concurrent writers, and a cache on a removable or network volume.
- Crash recovery: the sweep of leftover temporary files at open, and a half-written entry.
- Mode bits on Windows: owner-only entries are only read back there, since the protected access list is not inspected.
- The failure of `open` when the pin list cannot be read.

State: a per-process directory under the OS temp directory, removed at the end. No network, no process spawn, no real user data.
