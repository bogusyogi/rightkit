# Changelog

## 0.2.0

Nothing compiled or run locally; verification is the CI run.

- **New: consent gate on an account or scope change** (rightkit#124). `SyncConfig::require_consent_on_scope_change` (default `false`, so existing behaviour is unchanged). When on and the signed-in scope differs from the one the ledger is bound to, the pass returns the new `PassOutcome::ConsentRequired(ConsentRequest { from, to })` before any backend call: nothing is prepared, downloaded, uploaded or tombstoned, and the ledger is not reset. The request is stored in the ledger (`Ledger.pending_consent`, with the bound `Ledger.scope`), reported once through the observer as `consent_required`, and readable with `SyncEngine::consent_request()`.
- **New: `SyncEngine::grant_consent(to, ConsentChoice)`**. `Merge` resets the ledger for the new scope and publishes local items into it; `StartFresh` resets it too but withholds every item present locally (`Ledger.withheld`) from upload, deleting nothing. Refused when nothing is pending, `to` is not the pending target, or the signed-in scope has moved on again.
- Signing back in to the bound account clears a pending request. First bind and pre-scope ledgers need no consent.
- **Breaking for exhaustive matches**: `PassOutcome` gains `ConsentRequired`, and `SyncConfig` gains a field (use `..SyncConfig::default()`). New exports: `ConsentChoice`, `ConsentRequest`. Tests: `tests/consent.rs` (fake provider with two zones behind one switchable account).
- **New: `cipher` module and `EncryptingBackend`** (feature `cipher`, rightkit#141). Wraps any `Backend` so a provider only stores ciphertext. Per record: random 16-byte salt, HKDF-SHA256 to a one-use ChaCha20-Poly1305 key and nonce (`ring`), envelope `[format 1][key version][salt][ciphertext][tag]` in one `enc` field. `name`, `type`, `change_tag`, `updatedAt` and `deletedAt` stay in clear; every other field is sealed. Associated data binds format, key version, zone, record type, record name, `updatedAt` and `deletedAt`, so ciphertexts cannot be moved between records, types or zones, re-versioned, or turned into or out of tombstones. Keys come from a `KeyProvider` (`StaticKeys` for tests); a key-version byte supports rotation. A missing key or key version fails the call with `Unavailable` and the cursor does not advance; corrupt ciphertext is quarantined (field-less shell, never a tombstone); a tombstone without `enc` (server-compacted) passes through as a deletion. `server_record` of conflicts is opened too. `CipherStats` counts outcomes. Tests: `tests/cipher.rs`.
- **New: `http` module** (feature `http`, rightkit#143): blocking `HttpBackend`, `HttpAuth`/`TokenSource`, `account_scope(app, zone, account, key_id)`, `sync_config()` (consent required), `erase_account()`, `health()`. Protocol: `HTTP-PROTOCOL.md` (`rightkit-sync-http/1`, as implemented by `packages/sync-worker`). `HttpBackend` refuses any plaintext field. Tests: `tests/http_backend.rs`, `tests/http_encrypted.rs` against an in-process fake server (`tests/common/fake_server.rs`).
- **New: `tests/two_engines.rs`** (rightkit#142): two engines over one local store (CloudKit plus HTTP bridge) converge across both zones with edits and deletes in both directions, with no upload ping-pong, and a record one engine quarantined is never published by the other.
- **New features**: `helper` and `process-usage` (both default on, so current users are unchanged) gate the helper module and the `rightkit-child-process` dependency (`SyncStatus::process`). The record wire encoding moved to `wire` (still reachable as `helper::wire`).
- Cargo.lock needs regenerating (new optional dependencies `ring`, `zeroize`, `rightkit-http`, and `rightkit-child-process` becoming optional).
- Not done: the Development-signed CloudKit helper variant for tests named in the issue is outside this crate's source.

## 0.1.1

- New `SyncEngine::metrics()` returning `SyncMetrics`: pass and failed-pass counts, last pass duration, downloaded, uploaded and tombstone totals, and ledger queue sizes (remote versions, tombstones, pending uploads, pending tombstones, deferred applies). `SyncMetrics` is re-exported. `run_pass` records each pass; sync behaviour is unchanged.
- New `benches/budget.rs` (`rightkit cargo bench -p rightkit-sync --bench budget`) measures the runtime budget: a 100,000-record `MemoryBackend` zone with steady-state two-way passes. Figures are in README "Runtime budget".

## 0.1.0

- First release. Brand-neutral cloud sync engine: change tokens, tombstones, last-writer-wins conflict resolution, resumable batches, offline queue, retry and backoff, account-scoped ledger, adapter traits, and the `CloudKitHelper` line-JSON helper backend with a `MemoryBackend` reference zone.
- `FileStorage::save` now writes through `rightkit_fs::atomic_write_private` (unique temp file, `fsync` of file and directory, owner-only), replacing a fixed `.tmp` name with no `fsync`. Adds the `rightkit-fs` dependency (`fs` feature only).
- The `rightkit-sync-fake-helper` binary and `tests/helper_e2e.rs` are repository-only and are excluded from the published package.
