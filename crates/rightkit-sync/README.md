# rightkit-sync

Generic cloud sync engine for Right Suite apps. It owns change cursors, tombstones, last-writer-wins conflict resolution, resumable batches, an offline queue, retry and backoff, and an account-scoped ledger. It is aligned with the HeardRight iOS CloudKit record and conflict model.

**Use it** when an app must sync records through a cloud store and needs the same tombstone, conflict, and retry rules as the other Right Suite apps.

**Do not use it** as a transport on its own. The engine takes adapter traits; the CloudKit backend is the macOS helper described below.

## Public surface

- `API.md` (in this crate): adapter traits, the shared wire contract, the helper protocol, and per-app migration. Read it before changing record or conflict behavior.
- `helper::{CloudKitHelper, HelperBackend, HelperAuth}`: the macOS CloudKit backend, over the Swift `rightkit-cloudkit-helper` (`swift/RightKitCloudKitHelper` at the RightKit root). `HelperAuth` is defined in `src/helper/mod.rs`.
- `MemoryBackend` (`src/memory.rs`) and the `rightkit-sync-fake-helper` binary (`src/bin/rightkit-sync-fake-helper.rs`): a reference zone and protocol fake for tests. The fake-helper binary and its end-to-end test live in the repository only; the published package ships `MemoryBackend` but not the binary.

## Features

| Feature | Default | Adds |
|---|---|---|
| `helper` | yes | the `helper` module: the CloudKit helper-process backend (`rightkit-sync-helper/1`). No dependency of its own; turn it off on hosts without a helper (Android). |
| `process-usage` | yes | `SyncStatus::process` and the `rightkit-child-process` dependency. Turn it off on Android. The pass counters in `SyncStatus::metrics` stay. |
| `cipher` | no | `cipher::{RecordCipher, EncryptingBackend, KeyProvider, StaticKeys, CipherKey}`: end-to-end encryption of record payloads (ChaCha20-Poly1305, HKDF-SHA256 from `ring`; `zeroize`). |
| `http` | no | `http::{HttpBackend, HttpAuth, TokenSource}`: blocking client of the `rightkit-sync-http/1` protocol (`HTTP-PROTOCOL.md`), over `rightkit-http`'s blocking client. |

Always: `serde`, `serde_json`, and `rightkit-fs` (`fs` feature, for the durable ledger write, `src/storage.rs`). The record wire encoding (`wire`) is shared by the helper and HTTP protocols.

A provider that encrypts (`HttpBackend` + `EncryptingBackend`) must run its engine with `SyncConfig::require_consent_on_scope_change` (`http::sync_config()` sets it) and an `account_scope` that includes the key id. Plaintext downloaded records the engine could not apply yet are kept in the ledger's `pending_apply`; the ledger file is owner-only but not encrypted.

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

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: undeclared.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 100,000 records or more.
- Required hosts: macOS and Windows.
- Status exposure: `SyncEngine::status()` in `src/engine.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
