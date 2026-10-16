import { config, ERASE_MARKER_TTL_MS, type Env } from './env.js';
import { event } from './http.js';

const DAY_MS = 24 * 3600 * 1000;
const SELECT_LIMIT = 500;
const STATEMENTS_PER_BATCH = 100;
const MAX_ROUNDS = 20;

export interface CompactReport {
  compacted: number;
  zonesRaised: number;
  blobsDeleted: number;
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

/**
 * Reduce tombstones older than TOMBSTONE_COMPACT_DAYS to (name, type, updated_at, deleted_at):
 * the body and any R2 object are dropped. Tombstones are never deleted (the engine needs them to
 * block re-import). Each zone's floor_seq is raised to the highest compacted seq BEFORE the
 * rows change, so a client holding an older cursor gets 410 token_expired and replays from the
 * start. seq and version are left alone: a client at or past a row's seq already saw its content.
 */
export async function compactTombstones(env: Env, nowMs: number): Promise<CompactReport> {
  const cutoff = nowMs - config(env).tombstoneCompactDays * DAY_MS;
  const report: CompactReport = { compacted: 0, zonesRaised: 0, blobsDeleted: 0 };

  for (let round = 0; round < MAX_ROUNDS; round++) {
    const found = await env.DB.prepare(
      `SELECT app, account_id, zone, name, seq, version, r2_key FROM records
       WHERE deleted_at IS NOT NULL AND compacted_at IS NULL AND deleted_at < ?1
       LIMIT ?2`,
    )
      .bind(cutoff, SELECT_LIMIT)
      .all<Candidate>();
    const candidates = found.results;
    if (candidates.length === 0) break;

    for (let i = 0; i < candidates.length; i += STATEMENTS_PER_BATCH / 2) {
      const chunk = candidates.slice(i, i + STATEMENTS_PER_BATCH / 2);
      const floors = new Map<string, { c: Candidate; seq: number }>();
      for (const c of chunk) {
        const key = `${c.app}\u0000${c.account_id}\u0000${c.zone}`;
        const prev = floors.get(key);
        if (!prev || c.seq > prev.seq) floors.set(key, { c, seq: c.seq });
      }
      const statements: D1PreparedStatement[] = [];
      for (const { c, seq } of floors.values()) {
        statements.push(
          env.DB.prepare(
            'UPDATE zones SET floor_seq = MAX(floor_seq, ?4) WHERE app = ?1 AND account_id = ?2 AND zone = ?3',
          ).bind(c.app, c.account_id, c.zone, seq),
        );
      }
      const floorCount = statements.length;
      for (const c of chunk) {
        statements.push(
          env.DB.prepare(
            `UPDATE records SET enc = NULL, r2_key = NULL, size_bytes = 0, compacted_at = ?5
             WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name = ?4
               AND version = ?6 AND deleted_at IS NOT NULL AND compacted_at IS NULL`,
          ).bind(c.app, c.account_id, c.zone, c.name, nowMs, c.version),
        );
      }
      const results = await env.DB.batch(statements);
      report.zonesRaised += floorCount;
      for (let k = 0; k < chunk.length; k++) {
        const changed = (results[floorCount + k]!.meta.changes ?? 0) > 0;
        if (!changed) continue; // rewritten concurrently; a later run sees the new state
        report.compacted++;
        const key = chunk[k]!.r2_key;
        if (key && env.BLOBS) {
          await env.BLOBS.delete(key).catch(() => undefined);
          report.blobsDeleted++;
        }
      }
    }
    if (candidates.length < SELECT_LIMIT) break;
  }

  event('sync.compacted', { ...report });
  return report;
}

/** Drop stale rate-limit windows and expired erase markers. */
export async function pruneHousekeeping(env: Env, nowMs: number): Promise<{ rateWindows: number; eraseMarkers: number }> {
  const results = await env.DB.batch([
    env.DB.prepare('DELETE FROM rate_limits WHERE window_start < ?1').bind(nowMs - 3600_000),
    env.DB.prepare('DELETE FROM erased_accounts WHERE erased_at < ?1').bind(nowMs - ERASE_MARKER_TTL_MS),
  ]);
  return { rateWindows: results[0]!.meta.changes ?? 0, eraseMarkers: results[1]!.meta.changes ?? 0 };
}
