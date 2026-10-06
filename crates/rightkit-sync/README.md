# rightkit-sync

Generic cloud sync engine for Right Suite apps (change cursors, tombstones, last-writer-wins,
resumable batches, offline queue, backoff, account-scoped ledger), aligned with the HeardRight iOS
CloudKit record/conflict model.

- `API.md`: adapter traits, shared wire contract, helper protocol, per-app migration.
- `helper::{CloudKitHelper, HelperBackend, HelperAuth}`: the macOS CloudKit backend, over the
  Swift `rightkit-cloudkit-helper` (`tools/rightkit/swift/RightKitCloudKitHelper`).
- `MemoryBackend` and the `rightkit-sync-fake-helper` binary: reference zone and protocol fake for tests.
