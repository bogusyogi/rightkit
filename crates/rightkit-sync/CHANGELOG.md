# Changelog

## 0.1.1

- New `SyncEngine::metrics()` returning `SyncMetrics`: pass and failed-pass counts, last pass duration, downloaded, uploaded and tombstone totals, and ledger queue sizes (remote versions, tombstones, pending uploads, pending tombstones, deferred applies). `SyncMetrics` is re-exported. `run_pass` records each pass; sync behaviour is unchanged.
- New `benches/budget.rs` (`rightkit cargo bench -p rightkit-sync --bench budget`) measures the runtime budget: a 100,000-record `MemoryBackend` zone with steady-state two-way passes. Figures are in README "Runtime budget".

## 0.1.0

- First release. Brand-neutral cloud sync engine: change tokens, tombstones, last-writer-wins conflict resolution, resumable batches, offline queue, retry and backoff, account-scoped ledger, adapter traits, and the `CloudKitHelper` line-JSON helper backend with a `MemoryBackend` reference zone.
- `FileStorage::save` now writes through `rightkit_fs::atomic_write_private` (unique temp file, `fsync` of file and directory, owner-only), replacing a fixed `.tmp` name with no `fsync`. Adds the `rightkit-fs` dependency (`fs` feature only).
- The `rightkit-sync-fake-helper` binary and `tests/helper_e2e.rs` are repository-only and are excluded from the published package.
