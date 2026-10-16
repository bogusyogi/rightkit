# Changelog

## 0.2.0

Nothing compiled or run locally; verification is the CI run.

- **New: consent gate on an account or scope change** (rightkit#124). `SyncConfig::require_consent_on_scope_change` (default `false`, so existing behaviour is unchanged). When on and the signed-in scope differs from the one the ledger is bound to, the pass returns the new `PassOutcome::ConsentRequired(ConsentRequest { from, to })` before any backend call: nothing is prepared, downloaded, uploaded or tombstoned, and the ledger is not reset. The request is stored in the ledger (`Ledger.pending_consent`, with the bound `Ledger.scope`), reported once through the observer as `consent_required`, and readable with `SyncEngine::consent_request()`.
- **New: `SyncEngine::grant_consent(to, ConsentChoice)`**. `Merge` resets the ledger for the new scope and publishes local items into it; `StartFresh` resets it too but withholds every item present locally (`Ledger.withheld`) from upload, deleting nothing. Refused when nothing is pending, `to` is not the pending target, or the signed-in scope has moved on again.
- Signing back in to the bound account clears a pending request. First bind and pre-scope ledgers need no consent.
- **Breaking for exhaustive matches**: `PassOutcome` gains `ConsentRequired`, and `SyncConfig` gains a field (use `..SyncConfig::default()`). New exports: `ConsentChoice`, `ConsentRequest`. Tests: `tests/consent.rs` (fake provider with two zones behind one switchable account).
- Not done: the Development-signed CloudKit helper variant for tests named in the issue is outside this crate's source.

## 0.1.1

- New `SyncEngine::metrics()` returning `SyncMetrics`: pass and failed-pass counts, last pass duration, downloaded, uploaded and tombstone totals, and ledger queue sizes (remote versions, tombstones, pending uploads, pending tombstones, deferred applies). `SyncMetrics` is re-exported. `run_pass` records each pass; sync behaviour is unchanged.
- New `benches/budget.rs` (`rightkit cargo bench -p rightkit-sync --bench budget`) measures the runtime budget: a 100,000-record `MemoryBackend` zone with steady-state two-way passes. Figures are in README "Runtime budget".

## 0.1.0

- First release. Brand-neutral cloud sync engine: change tokens, tombstones, last-writer-wins conflict resolution, resumable batches, offline queue, retry and backoff, account-scoped ledger, adapter traits, and the `CloudKitHelper` line-JSON helper backend with a `MemoryBackend` reference zone.
- `FileStorage::save` now writes through `rightkit_fs::atomic_write_private` (unique temp file, `fsync` of file and directory, owner-only), replacing a fixed `.tmp` name with no `fsync`. Adds the `rightkit-fs` dependency (`fs` feature only).
- The `rightkit-sync-fake-helper` binary and `tests/helper_e2e.rs` are repository-only and are excluded from the published package.
