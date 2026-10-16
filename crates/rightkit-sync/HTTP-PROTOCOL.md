# rightkit-sync-http/1

Wire protocol between a `rightkit-sync` client (`http::HttpBackend`, feature `http`) and a record
server. The reference server is `packages/sync-worker` (Cloudflare Worker, D1, R2). The protocol is
the transport of the `Backend` contract in `API.md`: a zone with a delta feed, per-record
compare-and-swap, and tombstones as ordinary records. A server only ever sees ciphertext and
ordering metadata when the client wraps `HttpBackend` in `cipher::EncryptingBackend`.

Key words MUST and MAY are used in the RFC 2119 sense.

## 1. Transport

- HTTPS, JSON (`application/json`), UTF-8. Plain `http` is for loopback tests only.
- Every route except `GET /v1/sync/health` carries `Authorization: Bearer <sync token>`. The token
  is minted by the app's account backend (HS256 JWT in the reference server: `sub` = account id,
  `app`, `aud` = `rightkit-sync`, `scp` = `sync`, `iat`, `exp`). The server derives the tenant
  `(app, account id)` from the token only; a client never names it.
- A request body is at most a few MiB; a response page is bounded (section 4).
- Clients send `Accept: application/json` and MUST NOT retry a request on their own: the engine
  owns retry and backoff (`SyncError::RetryAfter`).

## 2. Errors

Any non-2xx response SHOULD carry `{"error":{"code":"<code>","message":"<text>"}}` and, for 429 and
503, a `Retry-After` header in seconds.

| Status / code | Client maps it to |
| --- | --- |
| 410, or code `token_expired` | `TokenExpired` (the engine drops its cursor once and replays the zone) |
| 404 `zone_missing` (any 404 without a code) | `ZoneMissing` (the engine calls prepare and replays) |
| 429 | `RetryAfter` (`Retry-After`, default 30 s) |
| 401 (`missing_token`, `invalid_token`, `session_expired`, `token_revoked`) | `Unavailable` (sign in again) |
| 403 (`app_not_allowed`, `zone_limit`, ...) | `Unavailable` (see note) |
| 502, 503, 504 | `RetryAfter` (`Retry-After`, default 30 s) |
| any other status | `Transient`, or `RetryAfter` when `Retry-After` is present |
| connection, TLS or timeout failure | `RetryAfter` 15 s |

Note: 403 is `Unavailable`, not `Cancelled`. The engine applies no backoff after `Cancelled`, so a
standing refusal would spin; `Unavailable` backs off and shows the reason.

## 3. Routes

`{zone}` matches `[A-Za-z0-9._-]{1,64}`. Record names match `[A-Za-z0-9._:@-]{1,128}`, types
`[A-Za-z0-9._-]{1,64}`. Clients percent-encode path segments and the `since` query value.

### `GET /v1/sync/health`

Unauthenticated liveness. `200 {"ok":true,"protocol":"rightkit-sync-http/1"}`. A client that finds
another `protocol` value MUST treat the server as unavailable.

### `POST /v1/sync/zones/{zone}` (prepare)

Idempotent. Body `{}`. `200 {"zone":"<zone>","created":<bool>}`. A server MAY cap zones per account
(403 `zone_limit`).

### `GET /v1/sync/zones/{zone}/changes?since=<cursor>&limit=<n>` (one delta page)

`since` absent or empty replays from the start. `limit` is at most 200. Response:

```json
{"records":[<record>...],"deleted_record_names":[],"new_token":"<cursor>","more_coming":false}
```

- Records are ordered by `seq`, ascending. A record appears once per page, in its current state.
- `new_token` is an opaque cursor, ASCII text. The client stores its bytes in the ledger and sends
  them back as `since`. When `more_coming` is true it is the last returned `seq`; otherwise the
  zone's last issued `seq`. A client advances its cursor only after it applied the whole pass.
- `deleted_record_names` is empty: deletion is a tombstone record. A client ignores unknown members.
- `more_coming` is true when the page was cut by `limit` or by a byte budget.
- A malformed cursor, a cursor beyond the zone's last `seq`, or a cursor below the zone's
  compaction floor answers `410 token_expired`. A missing zone answers `404 zone_missing`.

### `GET /v1/sync/zones/{zone}/records/{name}` (point read)

`200 {"record":<record>}` or `200 {"record":null}`. A missing zone answers `404 zone_missing`.

### `POST /v1/sync/zones/{zone}/save` (batch)

```json
{"policy":"overwrite"|"if_server_unchanged","records":[<write>...]}
```

At most 100 records (`400 bad_batch` beyond). Non-atomic: one outcome per input record, in order.

- `overwrite` writes unconditionally (tombstones use it).
- `if_server_unchanged` is compare-and-swap on `change_tag`. A write without `change_tag` requires
  that the record does not exist; with one, that it is still the server's. A mismatch is a per-record
  `conflict` outcome carrying the server's record (`null` when the record does not exist). The check
  MUST be atomic with the write.

Response `200 {"outcomes":[...]}`, each one of:

```json
{"name":"n1","status":"ok","change_tag":"4","seq":17}
{"name":"n1","status":"conflict","server_record":<record>|null}
{"name":"n1","status":"error","code":"quota_exceeded","message":"..."}
```

Per-record error codes: `invalid_record` (maps to `Transient`), `record_too_large` (`Transient`; the
reference limit is 1 MiB), `quota_exceeded` (`Unavailable`), `blob_store_unavailable` and
`blob_store_error` (`RetryAfter` 30 s). Unknown codes map to `Transient`.

### `DELETE /v1/sync/account`

Header `x-rightkit-confirm: erase-account` is required. Erases everything the server holds for the
token's account, in every zone. Idempotent; stays callable with a token revoked by the erase so a
partial failure can be retried. `200 {"erased":{...}}`. Used by `HttpBackend::erase_account`.

## 4. Records

Wire record (responses and the `server_record` of a conflict):

```json
{"name":"n1","type":"Note","seq":17,"change_tag":"4","updated_at":1760000000000,
 "deleted_at":null,"enc":"<base64>"}
```

Write record (a save): `{"name","type","updated_at","deleted_at"?,"enc"?,"change_tag"?}`.

| Member | Meaning |
| --- | --- |
| `name`, `type` | record identity; clear, chosen by the client |
| `updated_at` | integer epoch ms, the engine's last-writer-wins version; required on a write |
| `deleted_at` | integer epoch ms, present exactly on tombstones; clear so the server and engine can order them |
| `enc` | base64 of one opaque sealed body (a ciphertext), or null/absent |
| `change_tag` | the server's row version as decimal text; the client echoes the tag it last saw |
| `seq` | per-zone sequence of the last write; server-assigned; informational for clients |

Mapping to `RawRecord`: `updated_at` -> field `updatedAt` (Date), `deleted_at` -> `deletedAt` (Date),
`enc` -> field `enc` (Bytes), `change_tag` -> `RawRecord::change_tag`. The client sends **no other
field**: `HttpBackend` refuses a record that has one (`SyncError::Persistence`, nothing sent), which
makes plaintext leaks impossible by construction. `createdAt`, `id`, text and every domain field live
inside `enc` (see `cipher.rs` for the sealed layout). Records without `updatedAt` (hook records of
other types) cannot be sent.

### Compaction

A server MAY compact tombstones older than a retention period: drop `enc`, keep
`(name, type, updated_at, deleted_at)`, never delete the row (the engine needs tombstones to block
re-import). Before it does, it raises the zone's compaction floor to the highest compacted `seq`; a
cursor below the floor then answers 410 and clients replay from the start. A replay delivers
compacted tombstones with `enc` null. `EncryptingBackend` passes a record with `deleted_at` and no
`enc` through as a deletion (id = record name); it does not quarantine it.

Trade-off: a deletion without `enc` is not authenticated, so a malicious server can delete records
on clients that way. It could always withhold or drop them; this is the only path where encryption
does not cover a deletion. A server that keeps the small sealed `enc` on tombstones (they carry only
an id) closes it.

## 5. Client obligations

- Run the engine with `SyncConfig::require_consent_on_scope_change` (`http::sync_config()`) and an
  `AuthProvider::account_scope` from `http::account_scope(app, zone, account_id, key_id)`. A switch of
  account or key then pauses for consent instead of publishing local history into the new zone.
- Wrap the backend in `cipher::EncryptingBackend` with the zone name as cipher zone.
- Use one blocking thread for the engine; `HttpBackend` calls block.

## 6. Versioning

The protocol id is `rightkit-sync-http/1`, reported by health. A breaking change takes a new id and
route prefix (`/v2`). Servers and clients ignore unknown JSON members.
