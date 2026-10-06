# rightkit-sync API and shared record/conflict contract

Internal crate (`publish = false`). Deps: `serde`, `serde_json` only. Lives at `tools/rightkit/crates/rightkit-sync`.

## 1. Shared wire contract (desktop and iOS must both obey)

Transport: CloudKit private database, one custom zone per app, one record type per syncable item (`Transcript` today). Apps keep their own container, zone, subscription ids.

| Rule | Contract |
|---|---|
| Record name | the item id (lowercased UUID); `RecordMapper::record_name` may prefix (`qr-<id>`, `device-<id>` stay app-owned) |
| Standard fields | `id` (text), `createdAt` (date, **immutable origin time**), `updatedAt` (date, LWW version), `deletedAt` (date, present only on tombstones) |
| Domain fields | owned by the mapper. Large bodies move to an asset (`textAsset`/`mdAsset` desktop, none yet on iOS); decoders accept inline or asset |
| Tombstone | same record: keep `id`, set `updatedAt = deletedAt = deletion time`, clear everything else. Decoders must not require any other field on a tombstone (`write_tombstone`/`read_tombstone`) |
| Tombstone wins | a tombstone beats any live record regardless of `updatedAt`; blocks re-import and re-upload permanently; deleting a local row on receipt |
| Aliases | several record names may carry one logical `id` (legacy). Reduce by total order `(is_tombstone, version, record_name)` max. Tombstones are written to every known alias |
| LWW | live vs live: higher `updatedAt` wins. Upload only if local is strictly newer; download only if remote is strictly newer. Equal versions do nothing (iOS provenance-repair ties are a `ConflictPolicy` override) |
| Save policy | full-field overwrite (`.allKeys`), non-atomic batches, per-record outcomes |
| Token | opaque; advances only after deletions applied, uploads attempted, local merge landed; a failed pass replays the page |
| Expiry | `changeTokenExpired` => clear token once, replay whole zone, never discard local data. Zone missing => re-prepare once, replay |
| Order | publish oldest `createdAt` first, ties by record name; first publication of never-seen rows is one record per save and stops for the pass at the first failure; edits go in 200-record batches; tombstones 50 |
| Bad record | quarantine (count, observer event), never block the page, never crash |
| Retry | merge server `retryAfter` with exponential backoff (15s base, 900s cap); pending work => 30s floor; network 15s, rate/busy/unavailable 30s hints from the backend |

Known divergences found while comparing (fixed by this crate, apps must migrate):
- Desktop never compared `updatedAt` (no edit propagation either direction) and stamped `createdAt = now()` at publish; both break the contract.
- Desktop swallowed ledger write errors; the engine now fails the pass instead.
- iOS keeps `originDeviceID` inside `metadataJSON` (production schema lacks the field); desktop used a dedicated `originDeviceID` field. Both are mapper details; `ItemHeader.origin_device` carries it. Do not add the field to production CloudKit schema without a schema deploy.
- iOS has no `QueueRequest`/`Device` records in HeardRight; those remain ScrapeRight-only through `PassHooks`.

## 2. Rust adapter interface

```rust
SyncEngineBuilder::new(mapper, backend, local, storage)   // required
    .auth(..).policy(..).clock(..).observer(..).hooks(..).config(..)
    .build()? -> SyncEngine<M>
engine.run_pass() -> PassReport   // call from ONE worker thread
engine.note_changed(id)           // offline queue (durable)
engine.note_deleted(id)           // tombstone queue (durable)
engine.request_full_replay_once("flag")
```

| Trait | Responsibility | ScrapeRight implementer |
|---|---|---|
| `RecordMapper` | `record_type`, `header(item)`, `encode`, `decode`, optional `record_name`/`tombstone_keep_fields` | `TranscriptPayload` codec from `schema.rs` (title/text/md/timedJSON/source/kind/duration/origin/request ids; asset spill >200 KB) |
| `LocalStore<T>` | `all`, `remove(id)`, `apply_remote(items)->Applied/Deferred` | adapter over `EngineBridge`: history rows, `delete_transcript`, `import_transcript` (Deferred when the engine RPC is unavailable) |
| `ConflictPolicy<T>` | `upload_local_over_remote`, `accept_remote_over_local` (default `LastWriterWins`) | default |
| `Backend` | `prepare`, `fetch_changes` (one page), `fetch_record`, `save_records`, `max_batch` | `CloudKitBackend` (see 3) |
| `AuthProvider` | `enabled`, `availability`, `generation`, `bearer` | iCloud account status + toggle; generation bumps on account change |
| `LedgerStorage` | `load`/`save` bytes (`FileStorage` atomic, `MemoryStorage`) | `FileStorage(<dir>/icloud-sync-ledger.json)` |
| `PassHooks` | `outbound` (device announce), `on_foreign_records` (QueueRequest claim/status flow, uses `extras` for request ledger) | ScrapeRight coordinator's request code, unchanged domain logic |
| `Clock`, `SyncObserver` | time, diagnostics | `tracing` bridge |

`PassOutcome`: `Synced`, `Pending(reason)`, `Deferred{secs}`, `Unavailable`, `Disabled`, `Cancelled`, `Failed`. `Pending` means the token advanced but queued uploads/tombstones/deferred applies remain.

Ledger JSON is a new document (`Ledger`); the ScrapeRight `icloud-sync-ledger.json` is not read. First run after migration replays the zone; remote tombstones restore deletions, `published` is rebuilt from remote versions.

## 3. Backends

`MemoryBackend` ships here as the reference zone/test fixture. A real `CloudKitBackend` is NOT in this crate (needs `objc2-cloud-kit`, entitlements, app container). Call map (from ScrapeRight `cloudkit.rs` header, validated against iOS):
- `prepare`: `CKModifyRecordZonesOperation` + zone subscription (subscription failure non-fatal).
- `fetch_changes`: `CKFetchRecordZoneChangesOperation`; `moreComing` => set `more_coming`; `.changeTokenExpired` => `TokenExpired`; `.zoneNotFound/.userDeletedZone` => `ZoneMissing`; per-record fetch failure counted by backend, not fatal.
- `save_records`: `CKModifyRecordsOperation(savePolicy: .allKeys, atomically: false)`; map `.networkUnavailable/.networkFailure` => `RetryAfter 15`, `.requestRateLimited/.zoneBusy/.serviceUnavailable` => `RetryAfter 30` (or `CKErrorRetryAfterKey`).
- `FieldValue::Bytes` <-> `CKAsset` (temp file on upload).
- Blocking: wait on a completion channel (~120 s timeout).
The change token is stored as opaque bytes; it never round-trips across platforms.

## 4. Per-app migration (no app edited by this lane; do not publish)

| App | Change | Chat that must act |
|---|---|---|
| ScrapeRight desktop | Replace `cloud_sync/{engine,ledger,transport,coordinator}` sync logic with `rightkit-sync`; keep `schema.rs` as mapper + `PassHooks` (QueueRequest/Device) and `EngineBridge` as `LocalStore`; build the macOS `CloudKitBackend`; fix `created_at = now()`. Gate: crate stays internal until Windows+macOS gate | ScrapeRight desktop chat (also owns CloudKit backend) |
| HeardRight desktop (Tauri) | Adopt crate when a desktop sync exists; mapper over `HRLibraryTranscript` schema fields (text/rawText/tags/folder/metadataJSON) | HeardRight chat |
| HeardRight iOS | Stays Swift. No code change required; contract checks: tombstone fields, LWW, alias reduction, order, retry floors already match. Add `deletedAt`-only tombstone decode test vectors shared with `write_tombstone` | HeardRight iOS chat (optional vector tests) |
| ScrapeRight iOS | Stays Swift; its `SyncLedger.swift` already mirrors the desktop ledger. Add fields `createdAt` immutability test | ScrapeRight iOS chat |
| ViewRight, CutRight, GenRight | None (no cloud sync exists) | none |
| RightKit integrator | Apply `registration.patch` (docs row; root `Cargo.lock` gains the `rightkit-sync` entry automatically on first workspace build, `crates/*` glob already includes it) | RightKit owner chat |

## 5. Tests (`tests/convergence.rs`, run via managed cargo)

1. Two devices over a paged zone: initial sync, concurrent edit (LWW), delete vs concurrent edit (tombstone wins, no re-upload, tombstone clears content).
2. Offline then partial batch then restart: backoff window honoured with server hint, partial confirmation persisted, restart uploads only the remainder.
3. Token expiry replay + malformed record quarantined without blocking.
4. Desktop-style local-deletion inference writes exactly one tombstone and propagates.
