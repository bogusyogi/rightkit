# Changelog

## 0.1.0

Initial version. Private package; deployed as a Cloudflare Worker, not published to npm.

- Server side of `rightkit-sync-http/1` (rightkit#146): multi-tenant by `(app, account_id, zone)`, opaque encrypted records only, D1 `records`/`zones` tables with a per-zone monotonic `seq` and a per-row `version` (the `change_tag`), R2 spill for bodies above 64 KiB.
- Routes: `POST /v1/sync/zones/{zone}` (prepare), `GET .../changes?since=&limit=`, `GET .../records/{name}`, `POST .../save` (`overwrite` or `if_server_unchanged`), `GET /v1/sync/health`.
- HS256 sync-token authentication scoped to one app and one account, with a previous-key slot for rotation.
- Per-account fixed-window rate limit (cost = records in a save) answering 429 with `Retry-After`, per-account storage cap, per-record size limit, zone-count limit.
- Account erase and tombstone compaction (rightkit#147): `DELETE /v1/sync/account` wipes rows and R2 bodies and revokes tokens issued before it; a daily cron strips tombstones older than `TOMBSTONE_COMPACT_DAYS` to metadata and raises the zone `floor_seq`, so older cursors get 410 `token_expired`.
- Content-free structured events: `sync.save`, `sync.conflict`, `sync.rate_limited`, `sync.account_erased`, `sync.compacted`.
- Not yet run: no install, typecheck or test run has been done for this version.
