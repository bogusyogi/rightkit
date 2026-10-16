# Changelog

## 0.1.0

Initial version. Private package; deployed as a Cloudflare Worker, not published to npm.

- Server side of `rightkit-sync-http/1` (rightkit#146): multi-tenant by `(app, account_id, zone)`, opaque encrypted records only, D1 `records`/`zones` tables with a per-zone monotonic `seq` and a per-row `version` (the `change_tag`), R2 spill for bodies above 64 KiB.
- Routes: `POST /v1/sync/zones/{zone}` (prepare), `GET .../changes?since=&limit=`, `GET .../records/{name}`, `POST .../save` (`overwrite` or `if_server_unchanged`), `GET /v1/sync/health`.
- HS256 sync-token authentication scoped to one app and one account, with a previous-key slot for rotation.
- Per-account fixed-window rate limit (cost = records in a save) answering 429 with `Retry-After`, per-account storage cap, per-record size limit, zone-count limit.
- Account erase and tombstone compaction (rightkit#147): `DELETE /v1/sync/account` wipes rows and R2 bodies and revokes tokens issued before it; a daily cron strips tombstones older than `TOMBSTONE_COMPACT_DAYS` to metadata and raises the zone `floor_seq`, so older cursors get 410 `token_expired`.
- Content-free structured events: `sync.save`, `sync.conflict`, `sync.rate_limited`, `sync.account_erased`, `sync.compacted`.
- Sharding (rightkit#160): each `(app, account)` is routed by `SHA-256("app|account")` mod `SHARD_COUNT` to one of the D1 bindings `DB_0 .. DB_{SHARD_COUNT-1}` (default 1, at most 64; `wrangler.toml` ships 4 placeholders). The single `DB` binding is gone. Erase deletes rows on every shard; the cron jobs iterate every shard. Changing `SHARD_COUNT` on a populated deployment needs a data migration (README, "Sharding"). `pnpm migrate:local|remote` apply the schema to every declared shard.
- Save path uses D1 `batch()`: a save is one prefetch batch and one write batch (2 round trips, `N + 4` statements) instead of about `N + 1` round trips with `3N` statements; each write returns its row (`RETURNING`) so per-record outcomes come from the batch, and `seq` is assigned inside the batch. At most 12 bound parameters per statement (limit 100). A name may appear once per save.
- Plan-aware save size: `CF_PLAN` (`free` default, `paid`) sets the maximum records per save to 40 or 100, `MAX_SAVE_RECORDS` overrides it (1 to 100), above it answers `400 bad_batch`. Health reports `max_save_records`. Cron work (housekeeping and compaction) runs under a per-invocation D1 query budget (50 free, 1,000 paid) and resumes on the next run.
- Compaction keeps the sealed `enc` of tombstones and drops only R2 bodies, so clients can authenticate every deletion; the zone floor is raised only when a body is actually dropped.
- README documents the Cloudflare limits (checked 2026-10-16), the per-request statement cost, the sharding migration plan, the 403-as-`Unavailable` client mapping and the remaining differences from `HTTP-PROTOCOL.md`.
- Not yet run: no install, typecheck or test run has been done for this version.
