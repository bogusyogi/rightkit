import { config, ERASE_MARKER_TTL_MS, type Env } from './env.js';
import { event } from './http.js';
import { allShards, QueryBudget, type Shard } from './shards.js';

const DAY_MS = 24 * 3600 * 1000;
const SELECT_LIMIT = 50;
const MAX_ROUNDS = 20;

export interface CompactReport {
  compacted: number;
  zonesRaised: number;
  blobsDeleted: number;
  shardsVisited: number;
  /** True when the invocation's D1 query budget ran out before every shard was drained. */
  budgetExhausted: boolean;
}

interface Candidate {
  app: string;
  account_id: string;
  zone: string;
  name: string;
  seq: number;
  version: number;
  r2_key: string | null;
}

/** Shards in index order, rotated by day so a small per-invocation budget cannot starve a shard. */
function rotated(shards: Shard[], nowMs: number): Shard[] {
  if (shards.length === 0) return shards;
  const start = Math.floor(nowMs / DAY_MS) % shards.length;
  return [...shards.slice(start), ...shards.slice(0, start)];
}

/**
 * Compact tombstones older than TOMBSTONE_COMPACT_DAYS, shard by shard.
 *
 * What is dropped: only the R2 body of a tombstone (a tombstone that was saved with a body above
 * BLOB_SPILL_BYTES). The small sealed `enc` stored inline is KEPT: the client authenticates a
 * tombstone by opening it, so a tombstone without `enc` would let a malicious or compromised
 * server delete client data with an unauthenticated deletion. Tombstones are never deleted (the
 * engine needs them to block re-import).
 *
 * A tombstone with no R2 body only gets `compacted_at` set (nothing a client holds changes). A
 * zone's floor_seq is raised, BEFORE the rows change, only when a body is actually dropped: a client
 * with an older cursor then gets 410 token_expired and replays from the start. seq and version are
 * left alone.
 *
 * Every D1 statement is counted against `budget` (Cloudflare caps D1 queries per invocation: 50 on
 * the free plan, 1,000 on paid). Work not done today is picked up by the next run.
 */
export async function compactTombstones(
  env: Env,
  nowMs: number,
  budget: QueryBudget = new QueryBudget(config(env).queryBudget),
): Promise<CompactReport> {
  const cutoff = nowMs - config(env).tombstoneCompactDays * DAY_MS;
  const report: CompactReport = { compacted: 0, zonesRaised: 0, blobsDeleted: 0, shardsVisited: 0, budgetExhausted: false };

  for (const { db } of rotated(allShards(env), nowMs)) {
    report.shardsVisited++;
    for (let round = 0; round < MAX_ROUNDS; round++) {
      // One select, then at most two statements per candidate (floor raise + row update).
      const limit = Math.min(SELECT_LIMIT, Math.floor((budget.left - 1) / 2));
      if (limit < 1) {
        report.budgetExhausted = true;
        break;
      }
      budget.take(1);
      const found = await db
        .prepare(
          `SELECT app, account_id, zone, name, seq, version, r2_key FROM records
           WHERE deleted_at IS NOT NULL AND compacted_at IS NULL AND deleted_at < ?1
           LIMIT ?2`,
        )
        .bind(cutoff, limit)
        .all<Candidate>();
      const candidates = found.results;
      if (candidates.length === 0) break;

      const floors = new Map<string, { c: Candidate; seq: number }>();
      for (const c of candidates) {
        if (!c.r2_key) continue; // nothing is dropped, so no cursor is invalidated
        const key = `${c.app}\u0000${c.account_id}\u0000${c.zone}`;
        const prev = floors.get(key);
        if (!prev || c.seq > prev.seq) floors.set(key, { c, seq: c.seq });
      }
      const statements: D1PreparedStatement[] = [];
      for (const { c, seq } of floors.values()) {
        statements.push(
          db
            .prepare('UPDATE zones SET floor_seq = MAX(floor_seq, ?4) WHERE app = ?1 AND account_id = ?2 AND zone = ?3')
            .bind(c.app, c.account_id, c.zone, seq),
        );
      }
      const floorCount = statements.length;
      for (const c of candidates) {
        // enc is deliberately not touched. Only the R2 pointer goes, and size_bytes follows it.
        statements.push(
          db
            .prepare(
              `UPDATE records SET size_bytes = CASE WHEN r2_key IS NULL THEN size_bytes ELSE 0 END,
                 r2_key = NULL, compacted_at = ?5
               WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name = ?4
                 AND version = ?6 AND deleted_at IS NOT NULL AND compacted_at IS NULL`,
            )
            .bind(c.app, c.account_id, c.zone, c.name, nowMs, c.version),
        );
      }
      budget.take(statements.length);
      const results = await db.batch(statements);
      report.zonesRaised += floorCount;
      for (let k = 0; k < candidates.length; k++) {
        const changed = (results[floorCount + k]!.meta.changes ?? 0) > 0;
        if (!changed) continue; // rewritten concurrently; a later run sees the new state
        report.compacted++;
        const key = candidates[k]!.r2_key;
        if (key && env.BLOBS) {
          await env.BLOBS.delete(key).catch(() => undefined);
          report.blobsDeleted++;
        }
      }
      if (candidates.length < limit) break;
    }
    if (report.budgetExhausted) break;
  }

  event('sync.compacted', { ...report });
  return report;
}

/** Drop stale rate-limit windows and expired erase markers on every shard (2 statements per shard). */
export async function pruneHousekeeping(
  env: Env,
  nowMs: number,
  budget: QueryBudget = new QueryBudget(config(env).queryBudget),
): Promise<{ rateWindows: number; eraseMarkers: number }> {
  let rateWindows = 0;
  let eraseMarkers = 0;
  for (const { db } of allShards(env)) {
    if (budget.left < 2) break;
    budget.take(2);
    const results = await db.batch([
      db.prepare('DELETE FROM rate_limits WHERE window_start < ?1').bind(nowMs - 3600_000),
      db.prepare('DELETE FROM erased_accounts WHERE erased_at < ?1').bind(nowMs - ERASE_MARKER_TTL_MS),
    ]);
    rateWindows += results[0]!.meta.changes ?? 0;
    eraseMarkers += results[1]!.meta.changes ?? 0;
  }
  return { rateWindows, eraseMarkers };
}
