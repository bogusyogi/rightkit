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
| `http` | no | **Parked** (the RightKit sync service is not being built; do not enable it in apps). `http::{HttpBackend, HttpAuth, TokenSource}`: blocking client of the `rightkit-sync-http/1` protocol (`HTTP-PROTOCOL.md`), over `rightkit-http`'s blocking client. |
| `drive` | no | The non-Apple backend (CloudKit is the Apple one): `drive::{DriveBackend, DriveAuth, DriveTokenSource, FnToken, DriveConfig}`, own-cloud sync through the user's Google Drive `appDataFolder` (scope `drive.appdata`), over `rightkit-http`'s blocking client. See "Google Drive backend" below. |

Always: `serde`, `serde_json`, and `rightkit-fs` (`fs` feature, for the durable ledger write, `src/storage.rs`). The record wire encoding (`wire`) is shared by the helper and HTTP protocols.

A provider that encrypts (`HttpBackend` + `EncryptingBackend`) must run its engine with `SyncConfig::require_consent_on_scope_change` (`http::sync_config()` sets it) and an `account_scope` that includes the key id. Plaintext downloaded records the engine could not apply yet are kept in the ledger's `pending_apply`; the ledger file is owner-only but not encrypted.

Every deletion an encrypting client applies is authenticated: a tombstone the server serves without its sealed `enc` is quarantined (counted in `CipherStats::unauthenticated_tombstones`), never applied. A server may compact tombstones but must keep their `enc`. `HttpBackend::max_batch` defaults to 40 (the Cloudflare free-plan save cap); raise it only up to the server's `max_save_records`.

## Google Drive backend (feature `drive`)

`DriveBackend` syncs through the user's own Google Drive: one zone is a folder inside the hidden `appDataFolder`, one record is one small JSON file (the shared `wire` encoding), tombstones are kept as records, and the cursor is the Drive changes page token (polling only). No service of ours is involved and the data uses the user's own quota. It is pure Rust and shared by Windows, macOS (when not using CloudKit) and Android. The caller supplies the access token through `DriveTokenSource` (desktop from `rightkit-oauth`'s loopback PKCE flow, Android from the Kotlin `AuthorizationClient`); this crate does not depend on `rightkit-oauth`. Run the engine with `drive::sync_config()` (consent required on an account change) and a scope from `drive::account_scope`. The `cipher` wrapper stays optional on top.

**No atomic compare-and-swap.** Google's Drive v3 reference documents no precondition on `files.update` or `files.create` (no `If-Match`/ETag, no revision precondition, no create-only flag), and file names are not unique within a folder. `SavePolicy::IfServerUnchanged` is therefore a check (`version` of the file must equal the record's tag, else `SyncError::Conflict` with the server record) followed by the write, with a race window of about one request round trip (and the upload time of the record body). A write by another device that commits inside that window is overwritten by the later `PATCH`:

- a lost live edit heals on the next pass of the device that made it when its conflict policy says local wins (always the case under last-writer-wins when it was the newer edit);
- a tombstone overwritten inside the window leaves a live record on the server, so a device with a reset ledger can see a deleted item again (devices holding the tombstone ignore it);
- two devices creating one new record name at once are reduced by listing again after the create (smallest Drive file id wins, the other creator deletes its file and conflicts); if Drive's listing has not yet indexed the rival file both survive and the engine's duplicate reduction ranks them by `updatedAt`.

The exact analysis, the Drive endpoints used and the error mapping (401 to `Unavailable`, 403 rate limits, 429 and 5xx to `RetryAfter`, 404 to absent or `ZoneMissing`, other 4xx to `Unavailable`) are in the `drive` module docs. Removals seen in the change feed are never turned into deletions (a user clearing the app's hidden Drive data must not delete local data); a removed zone folder yields `ZoneMissing` and the engine republishes local items. What goes into a record (transcripts only? attachments?) and the per-record size cap (`DriveConfig::max_record_bytes`, 1 MiB) are decided by the app. Tests run against an in-process fake (`tests/common/fake_drive.rs`, `tests/drive_backend.rs`, `tests/drive_two_engines.rs`); Drive's real behaviour has not been exercised.

## Scheduling and Android (WorkManager)

`sync_once(&mut engine, &SyncBudget, &SchedulePolicy) -> SyncReport` (module `schedule`, always compiled, platform-neutral) runs one bounded unit of sync and tells the host what to do next. It never sleeps and spawns no thread; it checks the elapsed budget between passes only (it cannot interrupt a running pass; the backend's HTTP timeouts bound that). The default budget is one pass. The engine's own backoff is kept in the ledger, so it survives the process being killed between WorkManager runs.

`SyncReport` carries `passes`, the last `outcome`, the summed `uploaded`/`downloaded`/`tombstones_*`/`quarantined`, `elapsed_ms`, a `work: WorkOutcome`, `next_delay_secs: Option<f64>` and `consent_required`. The pure helpers `next_delay`, `work_outcome` and `should_continue` are public and unit tested. Suggested delay: `idle_secs` (900) after `Synced`, the engine's figure after `Pending`, `Deferred` and failures (exponential 15 s doubling to 900 s, a server hint never shortened), never below `min_secs` (15), and `None` for `Disabled` and `ConsentRequired`.

**Kotlin call contract.** `rightkit-android` exposes only its own bridge (init, paths, Keystore, logcat); it exposes no other crate, so this crate is not added to it. The app's own cdylib (which links `rightkit-android`) wraps `sync_once` in its UniFFI surface, for example `fun syncOnce(maxPasses: UInt, maxElapsedMs: ULong): SyncReportFfi`, and the app's `CoroutineWorker`/`Worker` calls it:

1. `RightKit.init(context, ...)` ran in `Application.onCreate` (this also initialises the TLS verifier, see below). Build the engine once per process (a `DriveBackend` over a `rightkit-http` `Client`, `FileStorage` under `filesDir`).
2. In `doWork()` (a background thread, never the main thread), fetch a fresh Drive access token from `AuthorizationClient` and hand it to the `DriveTokenSource`; call `syncOnce`.
3. Map `work`: `Success` -> `Result.success()`, `Retry` -> `Result.retry()`, `Failure` -> `Result.failure()` (surface `consentRequired` in the UI and call `grant_consent`).
4. Enqueue the next run as a `OneTimeWorkRequest` with `setInitialDelay(nextDelaySecs)` (WorkManager's periodic minimum is 15 minutes and its `retry()` backoff is its own; a one-time chain keeps the engine's delay) and a network constraint. Do not enqueue when `nextDelaySecs` is `None`.

**TLS roots on Android (rightkit#149).** `rightkit-http` defaults to the OS verifier (`rustls-platform-verifier`), which on Android needs the `JavaVM` and `Context` initialised once per process before the first request (`rightkit-android`'s `platform-verifier` feature does it inside `RightKit.init`; the verifier's Kotlin artifact must be on the classpath). Until then requests fail closed. An app that has not wired it builds its `Client` with `TrustRoots::Bundled`. Build the crate for Android with `--no-default-features --features drive` (plus `cipher` if used). Nothing here is compiled for an Android target yet.

## Platform support

- macOS: the CloudKit helper backend. The helper is a separate Swift binary.
- Android: the `drive` backend (no cfgs in this crate; see "Scheduling and Android"). Not built for an Android target yet.
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

- Resident memory ceiling: 1024.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 100,000 records or more.
- Required hosts: macOS only (Adrian 2026-10-14 local-build grant (.rightkit-local-build.json); Windows covered by the next CI run).
- Status exposure: `SyncEngine::status()` in `src/engine.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: darwin/arm64 (Mac18,5), rightkit-sync 0.2.2 at 5ab2baa539bc, 100,000 records (zone of 100000 Note records over MemoryBackend: full pull, 100 passes of 100 remote + 100 local edits, then 30 s idle), synthetic, `rustright cargo test -p rightkit-sync --profile dev --bench budget -- 100000 100 100 30`, 2026-10-18, evidence fa724e41ea92: resident memory 645.2 MiB, idle CPU 0%, wake-ups 0.033 per second, disk operations n/a (not measurable on this host).
