import { event } from './http.js';
import type { Env } from './env.js';
import { allShards, shardCount, shardFor } from './shards.js';
import type { Principal } from './auth.js';

export interface EraseReport {
  records: number;
  zones: number;
  blobs: number;
}

async function deleteBlobs(env: Env, prefix: string): Promise<number> {
  if (!env.BLOBS) return 0;
  let deleted = 0;
  let cursor: string | undefined;
  for (;;) {
    const page = await env.BLOBS.list({ prefix, limit: 1000, cursor });
    if (page.objects.length > 0) {
      await env.BLOBS.delete(page.objects.map((o) => o.key));
      deleted += page.objects.length;
    }
    if (!page.truncated) return deleted;
    cursor = page.cursor;
  }
}

/**
 * DELETE /v1/sync/account. Idempotent and safe to retry after a partial failure:
 * 1. write the erase marker (revokes every token issued up to now, including a racing writer's),
 * 2. delete R2 bodies by account prefix,
 * 3. delete the D1 rows on every shard,
 * 4. sweep R2 again for a write that raced step 2.
 * The marker is kept for ERASE_MARKER_TTL_MS, longer than any token lifetime, then pruned by cron.
 */
export async function eraseAccount(env: Env, p: Principal, nowMs: number): Promise<EraseReport> {
  // The marker lives on the account's home shard, which is where admit() reads it.
  const home = await shardFor(env, p);
  await home
    .prepare(
      `INSERT INTO erased_accounts (app, account_id, erased_at) VALUES (?1, ?2, ?3)
       ON CONFLICT (app, account_id) DO UPDATE SET erased_at = MAX(erased_at, excluded.erased_at)`,
    )
    .bind(p.app, p.accountId, nowMs)
    .run();

  const prefix = `${p.app}/${p.accountId}/`;
  let blobs = await deleteBlobs(env, prefix);

  // Rows are deleted on EVERY shard, not only the home shard: it is one batch of three statements
  // per shard, and it also removes strays left behind if SHARD_COUNT was ever changed without a
  // migration. Cost: 3 x SHARD_COUNT statements, which bounds SHARD_COUNT on the free plan (README).
  let records = 0;
  let zones = 0;
  for (const { db } of allShards(env)) {
    const results = await db.batch([
      db.prepare('DELETE FROM records WHERE app = ?1 AND account_id = ?2').bind(p.app, p.accountId),
      db.prepare('DELETE FROM zones WHERE app = ?1 AND account_id = ?2').bind(p.app, p.accountId),
      db.prepare('DELETE FROM rate_limits WHERE app = ?1 AND account_id = ?2').bind(p.app, p.accountId),
    ]);
    records += results[0]!.meta.changes ?? 0;
    zones += results[1]!.meta.changes ?? 0;
  }

  blobs += await deleteBlobs(env, prefix);

  const report = { records, zones, blobs };
  event('sync.account_erased', { app: p.app, shards: shardCount(env), ...report });
  return report;
}
