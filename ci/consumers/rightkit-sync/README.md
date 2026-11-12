# rightkit-sync 0.3.0 registry consumer (compile-only)

Standalone consumer, outside the workspace, pinning `rightkit-sync =0.3.0` exactly with its default features (`helper` and `process-usage`, which is what HeardRight, ScrapeRight and Pulse's membrane runtime link). It installs the crate from the sealed archive through the offline directory registry and builds `src/main.rs`.

It is compile-only. Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data." and "My decision was no unit tests. Only e2e." The crate has no in-process backend any more and the published package never shipped a helper stand-in, so this program runs nothing: `main` prints one line.

## What it proves

- The sealed `rightkit-sync` archive installs through the registry and the default features resolve.
- The public surface an app links type-checks: `RecordMapper`, `LocalStore`, `ItemHeader`, `RawRecord` and `FieldValue`, `SyncEngineBuilder` (`auth`, `build`), `SyncEngine` (`note_changed`, `run_pass`), `PassOutcome`, `FileStorage`, `NullObserver`, and the real CloudKit helper types `CloudKitHelper`, `HelperConfig`, `HelperBackend` and `HelperAuth`.

## What it does not prove

- Any behaviour. The real run is the crate's own `tests/helper_e2e.rs` and `benches/budget.rs` (the signed `rightkit-cloudkit-helper` against the app's real CloudKit container and iCloud account) and `tests/vault_race_real_bucket.rs` (a real R2 or S3 bucket). They need a Mac, an entitled helper and credentials, and fail when those are missing.
- The `cipher`, `http`, `drive` and `vault` features; they are off.

State: none. No network, no credentials, no files.
