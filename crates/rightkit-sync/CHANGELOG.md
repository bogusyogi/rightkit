# Changelog

## 0.1.0

- First release. Brand-neutral cloud sync engine: change tokens, tombstones, last-writer-wins conflict resolution, resumable batches, offline queue, retry and backoff, account-scoped ledger, adapter traits, and the `CloudKitHelper` line-JSON helper backend with a `MemoryBackend` reference zone.
- `FileStorage::save` now writes through `rightkit_fs::atomic_write_private` (unique temp file, `fsync` of file and directory, owner-only), replacing a fixed `.tmp` name with no `fsync`. Adds the `rightkit-fs` dependency (`fs` feature only).
- The `rightkit-sync-fake-helper` binary and `tests/helper_e2e.rs` are repository-only and are excluded from the published package.
