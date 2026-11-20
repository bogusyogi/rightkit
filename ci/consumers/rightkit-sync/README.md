# rightkit-sync 0.4.1 registry consumer (compile-only)

Standalone consumer, outside the workspace, pinning `rightkit-sync =0.4.1` exactly with `transcript-host`. It installs the crate from the sealed archive through the offline directory registry and builds `src/main.rs`.

It never starts the helper or runs a sync. Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data." and "My decision was no unit tests. Only e2e." Existing sync surface is type-checked without calling it; new transcript-profile identity and digest APIs receive only in-memory inputs.

## What it proves

- The sealed `rightkit-sync` archive installs through the registry and the default features resolve.
- The public surface an app links type-checks: `RecordMapper`, `LocalStore`, `ItemHeader`, `RawRecord` and `FieldValue`, `SyncEngineBuilder` (`auth`, `build`), `SyncEngine` (`note_changed`, `run_pass`), `PassOutcome`, `FileStorage`, `NullObserver`, real CloudKit helper types, and the `transcripts` feature's profile identity and content hash APIs.

## What it does not prove

- Any behaviour. The real run is the crate's own `tests/helper_e2e.rs` and `benches/budget.rs` (the signed `rightkit-cloudkit-helper` against the app's real CloudKit container and iCloud account) and `tests/vault_race_real_bucket.rs` (a real R2 or S3 bucket). They need a Mac, an entitled helper and credentials, and fail when those are missing.
- The `cipher`, `http`, `drive` and `vault` features; they are off. No live sync or persistent state is touched.

State: none. No network, no credentials, no files.
