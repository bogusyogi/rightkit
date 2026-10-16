# @rightkit/sync-worker

Cloudflare Worker (D1 for records, R2 for large bodies) that implements the server side of `rightkit-sync-http/1`, the HTTP protocol used by the `rightkit-sync` `HttpBackend`. One shared worker for all RightKit apps, multi-tenant by `(app, account_id, zone)`. HeardRight is the first consumer (design: `heardright/docs/architecture/2026-10-16-non-apple-sync.md`).

Private package: it is deployed, not published.

## What the server knows

Only ciphertext and ordering metadata. The client-side cipher keeps `name`, `type`, `updated_at`, `deleted_at` in clear and puts every other field into one AEAD blob, which is `enc` here (base64 on the wire, a BLOB or R2 object at rest). The worker never parses `enc`, never logs names or bodies, and cannot read transcripts.

## Protocol (rightkit-sync-http/1, as implemented here)

The normative spec is being written in `crates/rightkit-sync` by another agent and was not present when this was written; this package follows the design doc's endpoint list. Reconcile the two before release.

All routes except health need `Authorization: Bearer <sync token>`. Errors are `{"error":{"code","message"}}`.

| Route | Purpose |
| --- | --- |
| `GET /v1/sync/health` | Unauthenticated liveness: `{ok, protocol}` |
| `POST /v1/sync/zones/{zone}` | Idempotent prepare: `{zone, created}`. At most 16 zones per account (403 `zone_limit`) |
| `GET /v1/sync/zones/{zone}/changes?since=&limit=` | One page of changes in `seq` order |
| `GET /v1/sync/zones/{zone}/records/{name}` | Point read: `{record}` or `{record: null}` |
| `POST /v1/sync/zones/{zone}/save` | Batch save, up to 100 records |
| `DELETE /v1/sync/account` | Erase everything for the account (header `x-rightkit-confirm: erase-account`) |

Wire record: `{name, type, seq, change_tag, updated_at, deleted_at, enc}`. Names match `[A-Za-z0-9._:@-]{1,128}`, types `[A-Za-z0-9._-]{1,64}`, zones `[A-Za-z0-9._-]{1,64}`.

Changes: `new_token` is the cursor as decimal text (the client stores it as opaque bytes). `more_coming` is true when the page was cut by `limit` (max 200) or by a 4 MiB body budget. `deleted_record_names` is always empty: deletion is tombstones. When the page reaches the end, `new_token` is the zone's last issued seq. `since` absent or `0` replays from the start. A malformed cursor, a cursor beyond the zone's last seq, or a cursor below the compaction floor answers `410 token_expired`. A missing zone answers `404 zone_missing`.

Save body: `{"policy":"overwrite"|"if_server_unchanged","records":[{name,type,updated_at,deleted_at?,enc?,change_tag?}]}`. `change_tag` is the tag the client last saw (the server row version, decimal text).
- `overwrite`: write unconditionally. Used for tombstones.
- `if_server_unchanged`: no `change_tag` means the record must not exist; with a tag it must still match. A mismatch is a per-record `conflict` outcome carrying `server_record` (null when the record does not exist). The check is repeated in SQL, so a concurrent writer cannot slip through.

Response `{outcomes:[...]}`, one per input, in order: `{name,status:"ok",change_tag,seq}`, `{name,status:"conflict",server_record}` or `{name,status:"error",code,message}`. Per-record error codes: `invalid_record`, `record_too_large` (1 MiB), `quota_exceeded`, `blob_store_unavailable`, `blob_store_error`.

Status mapping for the client: 429 with `Retry-After` is `RetryAfter`; 401 (`missing_token`, `invalid_token`, `session_expired`, `token_revoked`) is `Unavailable` (re-auth); 403 `app_not_allowed` is `Cancelled`; 410 is `TokenExpired`; 404 `zone_missing` is `ZoneMissing`.

## Authentication

HS256 JWT minted by the account backend (design issue 7, not in this package). Claims: `sub` = account id, `app`, `aud` = `rightkit-sync`, `scp` = `sync`, `iat`, `exp` (lifetime at most 24 h; the design uses 1 h). `app` and the account id come only from the token, so a caller cannot address another tenant. `SYNC_JWT_SECRET` signs; `SYNC_JWT_SECRET_PREVIOUS` is also accepted during rotation. `ALLOWED_APPS` lists the apps enabled on this deployment.

## Limits

| Setting | Default | Notes |
| --- | --- | --- |
| `RATE_LIMIT_PER_MINUTE` | 1200 | Per account, fixed 60 s window, D1 counter. A save costs its record count. 429 with `Retry-After` |
| `ACCOUNT_STORAGE_CAP_BYTES` | 52428800 | Stored ciphertext per account across zones. Growth beyond it is refused per record; tombstones always pass |
| `BLOB_SPILL_BYTES` | 65536 | Larger bodies go to R2 under `{app}/{account}/{zone}/{sha256(name)}/{sha256(body)}` |
| `TOMBSTONE_COMPACT_DAYS` | 90 | See compaction |

Limits are application-level defaults chosen without checking current Cloudflare D1, R2 and Workers limits or pricing (the design requires verifying them first). A save does one D1 batch per record plus one prefetch, so a 100-record save is about 101 subrequests; check that against the plan's subrequest limit.

## Account erase (#147)

`DELETE /v1/sync/account` with `x-rightkit-confirm: erase-account`, called with the account's own sync token (the account backend mints one on account deletion). Order: write an erase marker (revokes every token issued at or before now), delete the account's R2 bodies by prefix, delete `records`, `zones` and `rate_limits` rows, sweep R2 again. Response `{erased:{records,zones,blobs}}`. It stays callable with the now-revoked token, so a partial failure can be retried; repeating it is a no-op. A token issued after the erase works again, so the same account can start over. Markers are pruned after 7 days (longer than any token lifetime). Tokens are issued in whole seconds, so a token minted in the same second as the erase is also revoked.

## Tombstone compaction (#147)

The `scheduled` handler (cron in `wrangler.toml`) runs daily. Tombstones older than `TOMBSTONE_COMPACT_DAYS` lose their body and R2 object and keep `(name, type, updated_at, deleted_at)`. They are never deleted, because the engine needs them to block re-import. Before rows change, the zone's `floor_seq` is raised to the highest compacted `seq`; a cursor below it gets 410 `token_expired` and the client replays from the start. `seq` and `change_tag` do not change. A later rewrite of a compacted record clears the mark. The same run prunes rate-limit windows older than an hour and expired erase markers.

Client contract: after replay the client receives compacted tombstones with `enc: null`. The `EncryptingBackend` cipher in `rightkit-sync` must pass a record with `deleted_at` set and no `enc` through unchanged instead of quarantining it as undecryptable.

## Schema

`migrations/0001_init.sql` creates `zones` (per-zone `last_seq`), `records` (primary key `(app, account_id, zone, name)`, `seq`, `version`, `updated_at`, `deleted_at`, `enc` or `r2_key`, `size_bytes`) and `rate_limits`. `migrations/0002_erase_and_compaction.sql` adds `zones.floor_seq`, `records.compacted_at` and `erased_accounts`. The code requires both. Every write bumps `zones.last_seq` and writes the row in one D1 batch (a transaction), so `seq` is gap-tolerant but never out of order.

## Deploy

1. Create the D1 database and R2 bucket, and put their names and id in `wrangler.toml` (placeholders only are committed).
2. `wrangler secret put SYNC_JWT_SECRET` (and the same key in the account backend).
3. `pnpm migrate:remote`, then `pnpm deploy`.

## Develop

`pnpm typecheck`, `pnpm test` (vitest with `@cloudflare/vitest-pool-workers`, local D1 and R2 via Miniflare). Tests use fixed test-only secrets set in `vitest.config.ts`.
