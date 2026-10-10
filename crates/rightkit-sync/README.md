# rightkit-sync

Generic cloud sync engine for Right Suite apps. It owns change cursors, tombstones, last-writer-wins conflict resolution, resumable batches, an offline queue, retry and backoff, and an account-scoped ledger. It is aligned with the HeardRight iOS CloudKit record and conflict model.

**Use it** when an app must sync records through a cloud store and needs the same tombstone, conflict, and retry rules as the other Right Suite apps.

**Do not use it** as a transport on its own. The engine takes adapter traits; the CloudKit backend is the macOS helper described below.

## Public surface

- `API.md` (in this crate): adapter traits, the shared wire contract, the helper protocol, and per-app migration. Read it before changing record or conflict behavior.
- `helper::{CloudKitHelper, HelperBackend, HelperAuth}`: the macOS CloudKit backend, over the Swift `rightkit-cloudkit-helper` (`swift/RightKitCloudKitHelper` at the RightKit root). `HelperAuth` is defined in `src/helper/mod.rs`.
- `MemoryBackend` (`src/memory.rs`) and the `rightkit-sync-fake-helper` binary (`src/bin/rightkit-sync-fake-helper.rs`): a reference zone and protocol fake for tests. The fake-helper binary and its end-to-end test live in the repository only; the published package ships `MemoryBackend` but not the binary.

## Features

None. The crate has no Cargo features. Dependencies: `serde`, `serde_json`, and `rightkit-fs` (`fs` feature, for the durable ledger write).

## Platform support

- macOS: the CloudKit helper backend. The helper is a separate Swift binary.
- Windows: `cfg(windows)` process handling in `src/helper/process.rs`.
- The engine, `MemoryBackend`, and the fake helper (repository only) are portable Rust.

Published version: see `INDEX.md` at the repository root. Changes are recorded in `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/convergence.rs` (engine behavior) and, in the repository only, `tests/helper_e2e.rs` (helper protocol through the fake helper).

```rust,ignore
// Build a sync engine over MemoryBackend in tests, or over CloudKitHelper
// in the app's macOS host, and drive it with the app's record mapper.
```
