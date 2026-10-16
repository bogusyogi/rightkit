# @rightkit/sync-worker

Cloudflare Worker (D1 for records, R2 for large bodies) that implements the server side of `rightkit-sync-http/1`, the HTTP protocol used by the `rightkit-sync` `HttpBackend`. One shared worker for all RightKit apps, multi-tenant by `(app, account_id, zone)`, with accounts spread over several D1 databases (shards). HeardRight is the first consumer (design: `heardright/docs/architecture/2026-10-16-non-apple-sync.md`).

Private package: it is deployed, not published.

## What the server knows

Only ciphertext and ordering metadata. The client-side cipher keeps `name`, `type`, `updated_at`, `deleted_at` in clear and puts every other field into one AEAD blob, which is `enc` here (base64 on the wire, a BLOB or R2 object at rest). The worker never parses `enc`, never logs names or bodies, and cannot read transcripts.

## Protocol (rightkit-sync-http/1, as implemented here)

The normative spec is `crates/rightkit-sync/HTTP-PROTOCOL.md`; this README describes the reference implementation of it. Differences from the spec are listed under "Differences from the spec".

All routes except health need `Authorization: Bearer <sync token>`. Errors are `{"error":{"code","message"}}`.

| Route | Purpose |
| --- | --- |
| `GET /v1/sync/health` | Unauthenticated liveness: `{ok, protocol, max_save_records}` |
| `POST /v1/sync/zones/{zone}` | Idempotent prepare: `{zone, created}`. At most 16 zones per account (403 `zone_limit`) |
| `GET /v1/sync/zones/{zone}/changes?since=&limit=` | One page of changes in `seq` order |
| `GET /v1/sync/zones/{zone}/records/{name}` | Point read: `{record}` or `{record: null}` |
| `POST /v1/sync/zones/{zone}/save` | Batch save, up to `max_save_records` (40 on the free plan, 100 on paid; see Limits) |
| `DELETE /v1/sync/account` | Erase everything for the account (header `x-rightkit-confirm: erase-account`) |

Wire record: `{name, type, seq, change_tag, updated_at, deleted_at, enc}`. Names match `[A-Za-z0-9._:@-]{1,128}`, types `[A-Za-z0-9._-]{1,64}`, zones `[A-Za-z0-9._-]{1,64}`.

Changes: `new_token` is the cursor as decimal text (the client stores it as opaque bytes). `more_coming` is true when the page was cut by `limit` (max 200) or by a 4 MiB body budget. `deleted_record_names` is always empty: deletion is tombstones. When the page reaches the end, `new_token` is the zone's last issued seq. `since` absent or `0` replays from the start. A malformed cursor, a cursor beyond the zone's last seq, or a cursor below the compaction floor answers `410 token_expired`. A missing zone answers `404 zone_missing`.

Save body: `{"policy":"overwrite"|"if_server_unchanged","records":[{name,type,updated_at,deleted_at?,enc?,change_tag?}]}`. `change_tag` is the tag the client last saw (the server row version, decimal text). A name may appear once per save (a repeat is an `invalid_record` outcome).
- `overwrite`: write unconditionally. Used for tombstones.
- `if_server_unchanged`: no `change_tag` means the record must not exist; with a tag it must still match. A mismatch is a per-record `conflict` outcome carrying `server_record` (null when the record does not exist). The check is repeated in SQL inside the write batch, so a concurrent writer cannot slip through.

Response `{outcomes:[...]}`, one per input, in order: `{name,status:"ok",change_tag,seq}`, `{name,status:"conflict",server_record}` or `{name,status:"error",code,message}`. Per-record error codes: `invalid_record`, `record_too_large` (1 MiB), `quota_exceeded`, `blob_store_unavailable`, `blob_store_error`. More than `max_save_records` records answers `400 bad_batch` and the message names the limit.

Status mapping for the client (`HTTP-PROTOCOL.md` section 2):

| Status / code | Client treats it as |
| --- | --- |
| 429 with `Retry-After` | `RetryAfter` |
| 401 (`missing_token`, `invalid_token`, `session_expired`, `token_revoked`) | `Unavailable` (re-auth) |
| 403 (`app_not_allowed`, `zone_limit`) | `Unavailable`. Clients map every 403 to `Unavailable`, not `Cancelled`: the engine applies no backoff after `Cancelled`, so a standing refusal would spin |
| 410 `token_expired` | `TokenExpired` |
| 404 `zone_missing` | `ZoneMissing` |
| 5xx without `Retry-After` | `Transient` (502/503/504: `RetryAfter`) |

## Authentication

HS256 JWT minted by the account backend (design issue 7, not in this package). Claims: `sub` = account id, `app`, `aud` = `rightkit-sync`, `scp` = `sync`, `iat`, `exp` (lifetime at most 24 h; the design uses 1 h). `app` and the account id come only from the token, so a caller cannot address another tenant. `SYNC_JWT_SECRET` signs; `SYNC_JWT_SECRET_PREVIOUS` is also accepted during rotation. `ALLOWED_APPS` lists the apps enabled on this deployment.

## Limits

Cloudflare limits this worker is designed around (developers.cloudflare.com, checked 2026-10-16):

| Cloudflare limit | Value | How the worker respects it |
| --- | --- | --- |
| D1 database size | 10 GB per database (500 MB on free) | Accounts are spread over `SHARD_COUNT` databases (Sharding) |
| D1 databases per account | 50,000 | One database per shard; `SHARD_COUNT` is at most 64 |
| D1 queries per Worker invocation | 1,000 (50 on free) | A save is `N + 7` statements at most (below); `max_save_records` is 40 on free; cron work is budgeted |
| Bound parameters per query | 100 | The widest statement binds 12 |
| Max row size | 2 MB | Records are capped at 1 MiB; bodies above `BLOB_SPILL_BYTES` go to R2 |

Worker settings:

| Setting | Default | Notes |
| --- | --- | --- |
| `SHARD_COUNT` | 1 (4 in `wrangler.toml`) | Number of D1 shards, 1 to 64, bindings `DB_0 .. DB_{SHARD_COUNT-1}` |
| `CF_PLAN` | `free` | `free` or `paid`. Selects the default save size and the cron query budget (50 or 1,000) |
| `MAX_SAVE_RECORDS` | 40 on `free`, 100 on `paid` | Optional override, 1 to 100. Also reported by health as `max_save_records` |
| `RATE_LIMIT_PER_MINUTE` | 1200 | Per account, fixed 60 s window, D1 counter on the account's shard. A save costs its record count. 429 with `Retry-After` |
| `ACCOUNT_STORAGE_CAP_BYTES` | 52428800 | Stored ciphertext per account across zones. Growth beyond it is refused per record; tombstones always pass |
| `BLOB_SPILL_BYTES` | 65536 | Larger bodies go to R2 under `{app}/{account}/{zone}/{sha256(name)}/{sha256(body)}` |
| `TOMBSTONE_COMPACT_DAYS` | 90 | See compaction |

Statement cost per request (each statement in a `batch()` counts as a query):

| Request | D1 statements | Round trips |
| --- | --- | --- |
| any authenticated request | 2 (admission: erase marker and rate counter) | 1 |
| prepare, point read, changes | 2 | 1 |
| save of N accepted records | 3 prefetch + N writes + 1 zone `seq` bump = `N + 4` | 2 |
| save, extra when a concurrent writer won a race | +1 re-read | +1 |
| erase | 1 marker + 3 per shard | 1 + `SHARD_COUNT` |

So a save costs at most `N + 2 + 4 + 1 = N + 7` statements per invocation: 47 for 40 records (free limit 50), 107 for 100 (paid limit 1,000). On the free plan `max_save_records` must stay at 43 or below; the default is 40. Erase costs `2 + 1 + 3 x SHARD_COUNT` statements, so on the free plan `SHARD_COUNT` must stay at 15 or below. R2 calls for large bodies are subrequests and count against the Workers subrequest limit separately; keep spilled bodies rare on the free plan.

The numbers above are Cloudflare's published limits on 2026-10-16, not measured here. Re-check them before changing plan or shard settings.

## Sharding

Each `(app, account)` lives in exactly one D1 database, chosen by `SHA-256("app|account_id")`, first 32 bits big endian, modulo `SHARD_COUNT` (`src/shards.ts`). Everything for the account is in that database: zones, records, rate-limit counters and the erase marker. A request therefore touches one database and `seq` and the version checks stay per-database transactions. The bindings are `DB_0 .. DB_{SHARD_COUNT-1}` and the schema (`migrations/`) is identical on every shard. `pnpm migrate:local` and `pnpm migrate:remote` apply it to every binding declared in `wrangler.toml` (the script refuses a mismatch with `SHARD_COUNT`).

Why a hash and not a routing table: a routing table in a control database adds a query to every request, makes that database a single point of failure and a hot spot, and needs its own sharding story. A pure function needs nothing and cannot disagree with itself between Worker instances. What it costs is that `SHARD_COUNT` is part of the data layout.

**Changing `SHARD_COUNT` on a deployment that holds data re-routes most accounts and makes their data invisible until it is moved.** Plan for it before launch:
1. Pick `SHARD_COUNT` for the lifetime of the deployment: 4 shards give 40 GB of headroom on paid databases (2 GB on free), well past the 50 MB per-account cap times the expected accounts. Over-provisioning shards is cheap (each is an empty database).
2. To grow later, deploy a migration job first: for every account on every old shard, copy its `zones`, `records` and `rate_limits` rows to the shard the new count assigns it, verify row counts, switch `SHARD_COUNT`, then delete the originals. Quiesce writes for the account during the copy (an erase marker plus 401 is not suitable; use a maintenance flag in the deployment). R2 keys do not depend on the shard and need no move.
3. Erase deletes the account's rows on every shard, so a stray copy left by an interrupted migration cannot survive an account erase.

No migration tool ships in this package; it is deliberately a one-off per deployment.

## Account erase (#147)

`DELETE /v1/sync/account` with `x-rightkit-confirm: erase-account`, called with the account's own sync token (the account backend mints one on account deletion). Order: write an erase marker on the account's home shard (revokes every token issued at or before now), delete the account's R2 bodies by prefix, delete `records`, `zones` and `rate_limits` rows on every shard, sweep R2 again. Response `{erased:{records,zones,blobs}}` (summed over shards). It stays callable with the now-revoked token, so a partial failure can be retried; repeating it is a no-op. A token issued after the erase works again, so the same account can start over. Markers are pruned after 7 days (longer than any token lifetime). Tokens are issued in whole seconds, so a token minted in the same second as the erase is also revoked.

## Tombstone compaction (#147)

The `scheduled` handler (cron in `wrangler.toml`) runs daily. Tombstones older than `TOMBSTONE_COMPACT_DAYS` are visited shard by shard (starting shard rotates daily) and marked compacted. Compaction keeps the small sealed `enc` of a tombstone: only an R2 body (a tombstone saved with a body above `BLOB_SPILL_BYTES`) is dropped. This is a security property, not a size tweak: the client authenticates a tombstone by opening its `enc`, and a tombstone without one would let a malicious or compromised server delete client data with an unauthenticated deletion. Tombstones are never deleted, because the engine needs them to block re-import.

When an R2 body is dropped, the zone's `floor_seq` is first raised to the highest such `seq`; a cursor below it gets 410 `token_expired` and the client replays from the start. A tombstone with an inline `enc` changes nothing a client holds, so compaction only marks it and the floor is not raised. `seq` and `change_tag` do not change. A later rewrite of a compacted record clears the mark. The same run prunes rate-limit windows older than an hour and expired erase markers on every shard.

The cron invocation has one D1 query budget (50 on `free`, 1,000 on `paid`) shared by housekeeping (2 statements per shard) and compaction (1 select plus at most 2 statements per candidate). When the budget runs out the run stops and the next day's run, which starts on a different shard, continues.

Client contract: after replay, a tombstone whose R2 body was dropped arrives with `enc: null`. `EncryptingBackend` passes a record with `deleted_at` set and no `enc` through as a deletion (`HTTP-PROTOCOL.md` section 4); a client that authenticates tombstones will treat that one as unauthenticated, so keep tombstone bodies small enough to stay inline (they carry only an id).

## Schema

`migrations/0001_init.sql` creates `zones` (per-zone `last_seq`), `records` (primary key `(app, account_id, zone, name)`, `seq`, `version`, `updated_at`, `deleted_at`, `enc` or `r2_key`, `size_bytes`) and `rate_limits`. `migrations/0002_erase_and_compaction.sql` adds `zones.floor_seq`, `records.compacted_at` and `erased_accounts`. The code requires both on every shard. A save writes its records and bumps `zones.last_seq` in one D1 batch (a transaction): the k-th write takes `seq = last_seq + k` and the last statement of the batch sets `last_seq = last_seq + N`, so `seq` is gap-tolerant (a write that lost a race leaves a gap) but never out of order.

## Differences from the spec

Compared with `crates/rightkit-sync/HTTP-PROTOCOL.md` as of 2026-10-16:

- Save size: the spec says at most 100 records. This worker accepts at most `max_save_records` (40 by default on the free plan) and answers `400 bad_batch` above it. A client must split larger saves, or read `max_save_records` from health.
- Health: the worker adds `max_save_records` to the health body. The spec says clients ignore unknown members (section 6), so this is compatible, but the spec does not list the member.
- Save atomicity: the spec calls save non-atomic. The worker writes all accepted records of one save in one transaction (all or nothing at the storage level), still reporting one outcome per record. If that transaction fails, the whole request fails with `500 internal` and no record is written (clients treat it as `Transient`).
- Duplicate names in one save: the spec is silent; the worker answers `invalid_record` for the second occurrence.
- Compaction: the spec says a server MAY drop `enc` on old tombstones. This worker never drops an inline `enc` (see above); the spec's compaction paragraph describes the weaker behavior and should say that a tombstone's sealed `enc` is kept.
- 503 `Retry-After`: this worker never answers 503; Cloudflare platform errors reach clients as 5xx without a code.

## Deploy

1. Create `SHARD_COUNT` D1 databases (`rightkit-sync-0` ...) and the R2 bucket, and put their ids in `wrangler.toml` (placeholders only are committed). The `DB_n` binding names and `SHARD_COUNT` must agree.
2. Set `CF_PLAN` (`free` or `paid`).
3. `wrangler secret put SYNC_JWT_SECRET` (and the same key in the account backend).
4. `pnpm migrate:remote`, then `pnpm deploy`.

## Develop

`pnpm typecheck`, `pnpm test` (vitest with `@cloudflare/vitest-pool-workers`, four local D1 shards and local R2 via Miniflare). Tests use fixed test-only secrets, `SHARD_COUNT=4` and `CF_PLAN=free` set in `vitest.config.ts`.
