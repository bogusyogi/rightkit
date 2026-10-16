# rightkit-sync API and shared record/conflict contract

Internal crate (`publish = false`). Deps: `serde`, `serde_json`, `rightkit-fs`; optional by feature: `rightkit-child-process` (`process-usage`), `ring` and `zeroize` (`cipher`), `rightkit-http` (`http` or `drive`); see README "Features". Lives at `rightkit/crates/rightkit-sync`.

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
| `AuthProvider` | `enabled`, `availability`, `generation`, `account_scope`, `bearer` | iCloud account status + toggle; generation bumps on account change |
| `LedgerStorage` | `load`/`save` bytes (`FileStorage` atomic, `MemoryStorage`) | `FileStorage(<dir>/icloud-sync-ledger.json)` |
| `PassHooks` | `outbound` (device announce), `on_account_switch`, `on_foreign_records` (QueueRequest claim/status flow, uses `extras` for request ledger) | ScrapeRight coordinator's request code, unchanged domain logic |
| `Clock`, `SyncObserver` | time, diagnostics | `tracing` bridge |

Acknowledgement rule: `on_foreign_records` responses are written to the zone before the hook's `extras` are persisted, and a failed response write fails the pass (`PassOutcome::Failed`, backoff) without advancing the change token, so the request replays. Every local effect (tombstone removal, `apply_remote`) and the final token advance re-check the auth generation; a changed generation returns `Cancelled` and commits no cursor progress. Hooks must therefore be idempotent over replay.

`PassOutcome`: `Synced`, `Pending(reason)`, `Deferred{secs}`, `Unavailable`, `Disabled`, `Cancelled`, `Failed`. `Pending` means the token advanced but queued uploads/tombstones/deferred applies remain.

Ledger JSON is a new document (`Ledger`); the ScrapeRight `icloud-sync-ledger.json` is not read. First run after migration replays the zone; remote tombstones restore deletions, `published` is rebuilt from remote versions.

## 3. Backends

| Backend | Where | Use |
|---|---|---|
| `MemoryBackend` | this crate | reference zone, in-process tests |
| `helper::HelperBackend` + `HelperAuth` (via `CloudKitHelper`) | this crate | any zone behind a helper process speaking `rightkit-sync-helper/1`; the macOS CloudKit backend |
| `rightkit-cloudkit-helper` | `rightkit/swift/RightKitCloudKitHelper` | production helper: Swift CLI over `RightKitCloudSync.CloudKitSyncTransport` |
| `rightkit-sync-fake-helper` | this crate (`src/bin`) | protocol fake with fault injection (E2E fixture) |
| `http::HttpBackend` + `HttpAuth` (feature `http`, **parked**) | this crate | any server speaking `rightkit-sync-http/1` (`HTTP-PROTOCOL.md`); reference server `packages/sync-worker`. Blocking; refuses plaintext fields. The sync service is parked; not a supported backend |
| `drive::DriveBackend` + `DriveAuth` (feature `drive`) | this crate | the non-Apple backend: Google Drive v3 `appDataFolder` (scope `drive.appdata`), the user's own quota. Zone = folder, record = one JSON file in the `wire` encoding, tombstones kept, cursor = Drive changes page token (a first full listing spans pages, cursors name their folder). Token from the caller (`DriveTokenSource`). Blocking. Drive has no atomic compare-and-swap, so `IfServerUnchanged` is check-then-write; see below |
| `cipher::EncryptingBackend` (feature `cipher`) | this crate | wraps any backend; the provider stores only ciphertext plus `name`, `type`, `change_tag`, `updatedAt`, `deletedAt`. Required in front of `HttpBackend` |

An encrypting provider runs its engine with `require_consent_on_scope_change` and an `account_scope` that includes the key id (`http::sync_config()`, `http::account_scope`). A missing key fails the pass (cursor stays); corrupt ciphertext is quarantined, and so is a tombstone without `enc` (`CipherStats::unauthenticated_tombstones`), so every deletion is authenticated. `HttpBackend::max_batch` defaults to 40 (server `max_save_records`, health). Plaintext of downloads the engine could not apply yet sits in the ledger's `pending_apply` (owner-only file, not encrypted).

```rust
let helper = CloudKitHelper::new(
    HelperConfig::new(path_to_helper).cloudkit("iCloud.com.example.app", "Items", "items-sub"),
    observer,
);
SyncEngineBuilder::new(mapper, Arc::new(helper.backend()), local, storage)
    .auth(Arc::new(helper.auth()))   // status, account scope, generation, user toggle
    .build()?;
```

`HelperAuth::set_enabled(false)` is the user toggle (cancels an in-flight pass). `availability()` calls
the helper's `status`; a changed `scope` bumps the generation and the engine resets its ledger.

### Account/scope isolation

`AuthProvider::account_scope()` (new) returns `sha256("container|zone|userRecordName")` from the
helper. The ledger stores it (`Ledger.scope`) and the engine binds every pass to it:

- first scoped pass on a pre-scope ledger: cursor, remote versions, aliases and queued tombstones are
  dropped (never imported into the account), tombstones kept;
- scope changed: `PassHooks::on_account_switch(from, to)`, then a fresh ledger for the new scope. The
  old account's queued deletions and cursor can never reach the new account;
- every helper request carries `expected_scope`; the helper refuses a mismatch before any I/O and
  checks again after it, so a request that straddles an account change returns `scope_changed`
  (engine outcome `Cancelled`, no cursor progress committed).

#### Consent gate (`SyncConfig::require_consent_on_scope_change`, default off)

Apps syncing personal data should not publish local history into an account the user has not
approved. With the option on, a scope that differs from the bound `Ledger.scope` does not reset the
ledger. The pass reports `PassOutcome::ConsentRequired(ConsentRequest { from, to })` and does no
backend I/O: no `prepare`, no download, no upload, no tombstone. The observer gets
`consent_required` once per distinct request, and `SyncEngine::consent_request()` returns it (it is
stored in the ledger, so it survives a restart; the bound identity is `Ledger.scope`). Offline
queueing (`note_changed`, `note_deleted`) keeps working.

The app records the user's answer with `SyncEngine::grant_consent(to, choice)`, `to` being the
request's `to` (a stale or mismatched `to`, or none pending, is refused):

- `ConsentChoice::Merge`: `on_account_switch(from, to)`, fresh ledger for the new scope; local items
  publish into the new account and its content downloads.
- `ConsentChoice::StartFresh`: same reset, but every item present locally at that moment is withheld
  (`Ledger.withheld`) and never uploaded; nothing local is deleted. Items created afterwards sync
  normally.

Signing back in to the bound account clears the request without consent. A first bind (no prior
scope) and pre-scope ledgers need no consent.

### Wire protocol `rightkit-sync-helper/1`

Transport: child process, UTF-8, one JSON object per line on stdin (requests) and stdout (responses);
stderr is free-form diagnostics (last line is appended to errors). Responses carry the request `id`;
the client kills and respawns the helper on timeout (default 120 s), EOF, crash or a malformed line
(retry 15 s). Lines over 128 MiB are rejected. Helper state across restarts is irrelevant: the change
cursor lives in the Rust ledger and travels in each request.

Request: `{"v":1,"id":N,"cmd":"...", ...}`. Response: `{"id":N,"ok":true, ...}` or
`{"id":N,"ok":false,"error":{"kind","message","retry_after_secs?"}}`.

| cmd | params | ok response |
|---|---|---|
| `status` | none | `status:"ready"` + `scope`, or `status:"unavailable"` + `detail` |
| `prepare` | `expected_scope` | `subscription_error?` (non-fatal) |
| `fetch_changes` | `expected_scope`, `since` (base64 or null) | `records[]`, `deleted[]`, `token` (base64), `more_coming`, `failed_records[]`, `failed_retry_after_secs?`, `undecodable_records[]` |
| `fetch_record` | `expected_scope`, `record_name` | `record` or null |
| `save_records` | `expected_scope`, `policy:"overwrite"\|"if_server_unchanged"`, `records[]` (each with its `change_tag`) | `outcomes[{name, ok, error?, server_record?}]` (non-atomic, per record) |
| `shutdown` | none | `{}` then exit |

Every non-status ok response echoes `scope`; the client treats a different value as `Cancelled`.

Record: `{"type","name","fields":{key:{"t":"text|i64|f64|date|bytes|strings","v":...}},"modified_ms?","created_ms?","change_tag?"}`.
`date` is epoch ms, `bytes` is base64 (a `CKAsset` in CloudKit), `strings` is a list field (an empty list
is omitted: CloudKit CKError 12). A save carries the full field set: fields the record held that are
absent from the write are cleared (this is how tombstones erase content).

Error kinds -> `SyncError`: `unavailable|not_authenticated|permission_denied|quota_exceeded` ->
`Unavailable`; `network` (15 s) / `busy` (30 s) / `retry_after` -> `RetryAfter` (server hint wins);
`token_expired` -> `TokenExpired`; `zone_missing` -> `ZoneMissing`; `scope_changed` -> `Cancelled`;
anything else -> `Transient`.

Cursor/H13 rule: `failed_records` non-empty (a record the feed listed but the helper could not
fetch) fails the whole page with `RetryAfter` (hint or 30 s) so the cursor never advances past it; the
page replays. `undecodable_records` (permanent) are reported through the observer and skipped.

Engine rule added with the zone-loss path: on `ZoneMissing` the ledger also forgets remote versions and
aliases (acknowledgements describe records that no longer exist), so local items republish.

### Conditional saves (no blind overwrite)

Every fetched record carries `change_tag` (CloudKit: archived `CKRecord` system fields, base64; the
Rust side only echoes it). Edits and first publications are saved with `SavePolicy::IfServerUnchanged`
(`.ifServerRecordUnchanged`): a record whose tag is no longer the server's (or an insert of a name that
now exists) is refused with `SyncError::Conflict` and the server's record in
`RecordOutcome::server_record`. The engine then (`resolve_conflict`, max 4 rounds): tombstone wins ->
bury and remove locally; otherwise `ConflictPolicy::upload_local_over_remote` -> re-save on top of the
server's tag; else `accept_remote_over_local` -> apply the server record locally; equal -> keep the
server's. Rounds exhausted or undecodable -> upload stays pending. Tombstones and hook records stay
`Overwrite` (deletion is authoritative).

**Drive backend and the conditional save.** Drive v3 documents no precondition on `files.update` or
`files.create` (no `If-Match`/ETag, no revision precondition, no create-only flag; names are not unique
in a folder), so `DriveBackend` implements `IfServerUnchanged` as: read the file `version` (the tag is
`"<file id>:<version>"`), refuse with `Conflict` plus the current record on a mismatch, else `PATCH`.
A write that commits between the read and the `PATCH` is overwritten (window: one round trip plus the
upload time). The engine's per-pass reconciliation heals a lost edit when local still wins; a tombstone
overwritten in the window can leave a live record for a device with a reset ledger. Creation re-lists the
name and the smallest file id wins. Details: `drive` module docs.

Donor semantics intentionally not carried over: per-request process spawn (now one long-lived helper),
and the helper-side persisted cursor with `commit_pull` (the Rust ledger owns the cursor, committed with
the local merge).

## 4. Per-app migration (no app edited by this lane; do not publish)

| App | Change | Chat that must act |
|---|---|---|
| HeardRight desktop (Tauri, `src-tauri/src/cloud_sync*.rs`) | Replace `cloud_sync.rs` (pull/push/ledgers/tombstones), `cloud_sync_ack*.rs` (ack ledger = `Ledger.remote_versions`/`pending_uploads`), `cloud_sync_backoff.rs` (= `BackoffPolicy`, hint is a floor, uncapped) and `helper_process.rs` call sites with `SyncEngine<TranscriptMapper>` + `CloudKitHelper`. Mapper: `Transcript` record (`text`,`rawText`,`source`,`tags` as `FieldValue::Strings`,`folder`, aliases = engine aliases; `logical_cloud_id` UUID normalisation stays in the mapper's `record_name`/`header`). `LocalStore` over the history store (`remove` = delete row, `apply_remote`). Keep `cloud_sync_scheduler.rs` as the wake/coalesce loop calling `run_pass()` from one worker. Delete `macos-native/HeardRightCloudSync` and the `HeardRight*` container/zone constants; ship `rightkit-cloudkit-helper` packaged per `swift/RightKitCloudKitHelper/README.md` with `entitlements-cloudsync.plist` values. First run replays the zone (ledger is new); remote tombstones restore deletions. The donor's ack/tombstone/identity contract tests map to `tests/helper_e2e.rs` cases; port app-specific vectors as E2E over the fake helper | HeardRight desktop chat |
| ScrapeRight desktop | Drop the stub `CloudKitBackend` plan; use `CloudKitHelper` as the transport; replace `cloud_sync/{engine,ledger,transport,coordinator}` with the engine; keep `schema.rs` as mapper and `PassHooks` (QueueRequest/Device), `EngineBridge` as `LocalStore`; fix `created_at = now()`; crate stays internal until the Windows+macOS gate | ScrapeRight desktop chat |
| HeardRight iOS, ScrapeRight iOS | Stay Swift on `RightKitCloudSync`; the helper uses the same transport, so wire fields stay identical. Optional: shared tombstone vectors | iOS chats (optional) |
| MailRight (Gmail-specific) | none; not CloudKit | none |
| ViewRight, CutRight, GenRight, CodeRight | none (no cloud sync) | none |
| RightKit integrator | apply `registration.patch` (capability-map row); `crates/*` glob already includes the crate; no new workspace member or dependency | RightKit owner chat |

## 5. Tests (run via managed cargo; `tests/convergence.rs`, `tests/helper_e2e.rs`)

`convergence.rs` (in-memory zone): two-device convergence and tombstone semantics, offline and partial
batch resume, token expiry and quarantine, local-deletion inference, acknowledgement and generation
fencing.

`helper_e2e.rs` (real child processes, files, line-JSON):
1. Two Macs on one on-disk zone through two helper processes: paged feed, tags list + 300 KB asset-sized
   bytes, LWW edit, delete-vs-edit tombstone wins with content cleared and no re-upload.
2. Per-record fetch failure never advances the cursor; server retry hint honoured; replay lands all.
3. Account switch: scope-bound ledger resets, the old account's queued deletion is not written to the
   new account's zone, the old zone is untouched.
4. Account flips while a request is in flight: `Cancelled`, no cursor progress, nothing written.
5. Helper crash, hung request (deadline), throttle (hint beats base backoff), signed-out status, helper
   respawn.
6. Token expiry replay, zone loss republish, undecodable record reported.
7. Conditional saves: another device writes between the engine's fetch and save (newer remote, older
   remote, remote tombstone, racing first publication): no lost update on either device; conflict
   counter proves the race happened.
8. Missing helper binary reports `Unavailable`.
9. The real Swift `rightkit-cloudkit-helper --backend memory` (when built) converges two engines
   through the same protocol (tombstone content clearing, scope binding) and refuses a stale
   conditional save, returning the server record and a new change tag.

Real CloudKit: manual gate in `swift/RightKitCloudKitHelper/README.md`.
