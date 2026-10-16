import { decodeBase64, encodeBase64, sha256Hex, toArrayBuffer, toBytes } from './base64.js';
import {
  config,
  MAX_CHANGES_LIMIT,
  MAX_RECORD_BYTES,
  MAX_ZONES_PER_ACCOUNT,
  PAGE_BYTE_BUDGET,
  type Env,
} from './env.js';
import { event, HttpError, isObject } from './http.js';
import { shardFor } from './shards.js';
import type { Principal } from './auth.js';

const NAME = /^[A-Za-z0-9._:@-]{1,128}$/;
const TYPE = /^[A-Za-z0-9._-]{1,64}$/;
const TAG = /^[0-9]{1,15}$/;
const COLUMNS = 'name, type, seq, version, updated_at, deleted_at, enc, r2_key, size_bytes';

/** Wire record. `enc` is base64 ciphertext; everything else is ordering metadata. */
export interface WireRecord {
  name: string;
  type: string;
  seq: number;
  change_tag: string;
  updated_at: number;
  deleted_at: number | null;
  enc: string | null;
}

interface Row {
  name: string;
  type: string;
  seq: number;
  version: number;
  updated_at: number;
  deleted_at: number | null;
  enc: unknown;
  r2_key: string | null;
  size_bytes: number;
}

export interface ZoneRow {
  last_seq: number;
  floor_seq: number;
}

function zoneMissing(): HttpError {
  return new HttpError(404, 'zone_missing', 'zone does not exist; call prepare first');
}

async function rowToWire(env: Env, row: Row, withBody = true): Promise<WireRecord> {
  let enc: string | null = null;
  if (withBody) {
    const inline = toBytes(row.enc);
    if (inline) {
      enc = encodeBase64(inline);
    } else if (row.r2_key) {
      const object = await env.BLOBS?.get(row.r2_key);
      if (!object) throw new HttpError(500, 'blob_missing', 'stored body is unavailable');
      enc = encodeBase64(new Uint8Array(await object.arrayBuffer()));
    }
  }
  return {
    name: row.name,
    type: row.type,
    seq: row.seq,
    change_tag: String(row.version),
    updated_at: row.updated_at,
    deleted_at: row.deleted_at,
    enc,
  };
}

/** POST /v1/sync/zones/{zone}: idempotent. */
export async function prepareZone(env: Env, p: Principal, zone: string, nowMs: number): Promise<{ zone: string; created: boolean }> {
  const db = await shardFor(env, p);
  const results = await db.batch([
    db.prepare(
      `INSERT OR IGNORE INTO zones (app, account_id, zone, last_seq, floor_seq, created_at)
       SELECT ?1, ?2, ?3, 0, 0, ?4
       WHERE (SELECT COUNT(*) FROM zones WHERE app = ?1 AND account_id = ?2) < ?5`,
    ).bind(p.app, p.accountId, zone, nowMs, MAX_ZONES_PER_ACCOUNT),
    db.prepare('SELECT 1 AS present FROM zones WHERE app = ?1 AND account_id = ?2 AND zone = ?3').bind(
      p.app,
      p.accountId,
      zone,
    ),
  ]);
  if (results[1]!.results.length === 0) {
    throw new HttpError(403, 'zone_limit', `at most ${MAX_ZONES_PER_ACCOUNT} zones per account`);
  }
  return { zone, created: (results[0]!.meta.changes ?? 0) > 0 };
}

export interface ChangesResult {
  records: WireRecord[];
  deleted_record_names: string[];
  new_token: string;
  more_coming: boolean;
}

/** GET /v1/sync/zones/{zone}/changes?since=<seq>&limit=<n> */
export async function fetchChanges(
  env: Env,
  p: Principal,
  zone: string,
  sinceParam: string | null,
  limitParam: string | null,
): Promise<ChangesResult> {
  let limit = MAX_CHANGES_LIMIT;
  if (limitParam !== null) {
    const n = Number(limitParam);
    if (!Number.isSafeInteger(n) || n < 1) throw new HttpError(400, 'bad_limit', 'limit must be a positive integer');
    limit = Math.min(n, MAX_CHANGES_LIMIT);
  }
  let since = 0;
  let malformedCursor = false;
  if (sinceParam !== null && sinceParam !== '') {
    if (TAG.test(sinceParam)) since = Number(sinceParam);
    else malformedCursor = true;
  }

  const db = await shardFor(env, p);
  const [zoneRes, rowsRes] = await db.batch([
    db.prepare('SELECT last_seq, floor_seq FROM zones WHERE app = ?1 AND account_id = ?2 AND zone = ?3').bind(
      p.app,
      p.accountId,
      zone,
    ),
    db.prepare(
      `SELECT ${COLUMNS} FROM records
       WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND seq > ?4
       ORDER BY seq ASC LIMIT ?5`,
    ).bind(p.app, p.accountId, zone, since, limit + 1),
  ]);
  const z = zoneRes!.results[0] as ZoneRow | undefined;
  if (!z) throw zoneMissing();
  // since = 0 (or absent) is a replay from the start and is always valid.
  if (malformedCursor || since > z.last_seq || (since !== 0 && since < z.floor_seq)) {
    throw new HttpError(410, 'token_expired', 'cursor is no longer valid; replay from the start');
  }

  const rows = rowsRes!.results as unknown as Row[];
  let more = rows.length > limit;
  const page: Row[] = [];
  let bytes = 0;
  for (const row of rows.slice(0, limit)) {
    if (page.length > 0 && bytes + row.size_bytes > PAGE_BYTE_BUDGET) {
      more = true;
      break;
    }
    page.push(row);
    bytes += row.size_bytes;
  }
  const records: WireRecord[] = [];
  for (const row of page) records.push(await rowToWire(env, row));
  const lastIncluded = page[page.length - 1];
  const newToken = more && lastIncluded ? lastIncluded.seq : z.last_seq;
  return { records, deleted_record_names: [], new_token: String(newToken), more_coming: more };
}

/** GET /v1/sync/zones/{zone}/records/{name} -> { record } (null when absent). */
export async function fetchRecord(env: Env, p: Principal, zone: string, name: string): Promise<{ record: WireRecord | null }> {
  if (!NAME.test(name)) throw new HttpError(400, 'bad_name', 'invalid record name');
  const db = await shardFor(env, p);
  const [zoneRes, rowRes] = await db.batch([
    db.prepare('SELECT last_seq, floor_seq FROM zones WHERE app = ?1 AND account_id = ?2 AND zone = ?3').bind(
      p.app,
      p.accountId,
      zone,
    ),
    db.prepare(`SELECT ${COLUMNS} FROM records WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name = ?4`).bind(
      p.app,
      p.accountId,
      zone,
      name,
    ),
  ]);
  if (zoneRes!.results.length === 0) throw zoneMissing();
  const row = rowRes!.results[0] as unknown as Row | undefined;
  return { record: row ? await rowToWire(env, row) : null };
}

type Policy = 'overwrite' | 'if_server_unchanged';

interface Incoming {
  name: string;
  type: string;
  updatedAt: number;
  deletedAt: number | null;
  enc: Uint8Array | null;
  expectedTag: string | null;
}

export type SaveOutcome =
  | { name: string; status: 'ok'; change_tag: string; seq: number }
  | { name: string; status: 'conflict'; server_record: WireRecord | null }
  | { name: string; status: 'error'; code: string; message: string };

function parseIncoming(raw: unknown): Incoming | { name: string; code: string; message: string } {
  const name = isObject(raw) && typeof raw.name === 'string' ? raw.name : '';
  const bad = (message: string) => ({ name, code: 'invalid_record', message });
  if (!isObject(raw)) return bad('record must be an object');
  if (!NAME.test(name)) return bad('invalid name');
  if (typeof raw.type !== 'string' || !TYPE.test(raw.type)) return bad('invalid type');
  const updatedAt = raw.updated_at;
  if (typeof updatedAt !== 'number' || !Number.isSafeInteger(updatedAt) || updatedAt < 0) return bad('invalid updated_at');
  const deletedAt = raw.deleted_at ?? null;
  if (deletedAt !== null && (typeof deletedAt !== 'number' || !Number.isSafeInteger(deletedAt) || deletedAt < 0)) {
    return bad('invalid deleted_at');
  }
  let enc: Uint8Array | null = null;
  if (raw.enc !== undefined && raw.enc !== null) {
    if (typeof raw.enc !== 'string') return bad('enc must be a base64 string');
    enc = decodeBase64(raw.enc);
    if (!enc) return bad('enc is not valid base64');
  }
  const tag = raw.change_tag ?? null;
  if (tag !== null && typeof tag !== 'string') return bad('change_tag must be a string');
  return { name, type: raw.type, updatedAt, deletedAt: deletedAt as number | null, enc, expectedTag: tag };
}


interface PendingWrite {
  /** Index into the outcomes array. */
  pos: number;
  item: Incoming;
  current: Row | null;
  r2Key: string | null;
  newlyPut: boolean;
  stmt: D1PreparedStatement;
}

/**
 * POST /v1/sync/zones/{zone}/save
 *
 * D1 cost of one save (statements count against the per-invocation query limit, 50 free / 1,000 paid):
 * admit 2 + prefetch 3 + one write per accepted record + 1 zone seq bump (+ 1 re-read only when a
 * concurrent writer won a race) = N + 6, or N + 7. The prefetch is one batch and the writes plus the
 * bump are one batch, so the whole save is two D1 round trips (three on a lost race) regardless of N.
 * `maxSaveRecords` (plan aware, default 40 free / 100 paid) keeps N + 7 under the plan's limit.
 *
 * Sequence numbers: inside the write batch the zone's last_seq is not yet bumped, so the k-th write
 * (1-based) takes seq = last_seq + k, and the final statement sets last_seq = last_seq + N. A batch is
 * one transaction and D1 serialises writers per database, so no other writer can interleave. Writes
 * that turn out to be no-ops (precondition lost a race) leave a gap in seq, which readers tolerate.
 * Every write statement has at most 12 bound parameters (D1 allows 100).
 */
export async function saveRecords(
  env: Env,
  p: Principal,
  zone: string,
  body: unknown,
  nowMs: number,
): Promise<{ outcomes: SaveOutcome[] }> {
  if (!isObject(body)) throw new HttpError(400, 'bad_request', 'body must be an object');
  const policy = body.policy;
  if (policy !== 'overwrite' && policy !== 'if_server_unchanged') {
    throw new HttpError(400, 'bad_policy', 'policy must be "overwrite" or "if_server_unchanged"');
  }
  const cfg = config(env);
  if (!Array.isArray(body.records)) {
    throw new HttpError(400, 'bad_batch', 'records must be an array');
  }
  if (body.records.length > cfg.maxSaveRecords) {
    throw new HttpError(400, 'bad_batch', `records must be an array of at most ${cfg.maxSaveRecords}`);
  }
  const db = await shardFor(env, p);
  const incoming = body.records.map(parseIncoming);

  // Prefetch the zone, the account's stored bytes and the existing rows in one round trip.
  const names = JSON.stringify(incoming.filter((r): r is Incoming => 'updatedAt' in r).map((r) => r.name));
  const [zoneRes, usedRes, existingRes] = await db.batch([
    db.prepare('SELECT last_seq, floor_seq FROM zones WHERE app = ?1 AND account_id = ?2 AND zone = ?3').bind(
      p.app,
      p.accountId,
      zone,
    ),
    db.prepare('SELECT COALESCE(SUM(size_bytes), 0) AS used FROM records WHERE app = ?1 AND account_id = ?2').bind(
      p.app,
      p.accountId,
    ),
    db.prepare(
      `SELECT ${COLUMNS} FROM records
       WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name IN (SELECT value FROM json_each(?4))`,
    ).bind(p.app, p.accountId, zone, names),
  ]);
  if (zoneRes!.results.length === 0) throw zoneMissing();
  let used = (usedRes!.results[0] as { used: number }).used;
  const existing = new Map<string, Row>();
  for (const row of existingRes!.results as unknown as Row[]) existing.set(row.name, row);

  const outcomes: (SaveOutcome | null)[] = incoming.map(() => null);
  const pending: PendingWrite[] = [];
  const seen = new Set<string>();
  let conflicts = 0;
  let errors = 0;
  const seqSql = '(SELECT last_seq FROM zones WHERE app = ?1 AND account_id = ?2 AND zone = ?3)';

  for (let pos = 0; pos < incoming.length; pos++) {
    const item = incoming[pos]!;
    if (!('updatedAt' in item)) {
      errors++;
      outcomes[pos] = { name: item.name, status: 'error', code: item.code, message: item.message };
      continue;
    }
    const fail = (code: string, message: string): void => {
      errors++;
      outcomes[pos] = { name: item.name, status: 'error', code, message };
    };
    // The writes of one save run in a single batch against the prefetched state, so a name may
    // appear once per save; a second occurrence would see stale preconditions.
    if (seen.has(item.name)) {
      fail('invalid_record', 'duplicate name in one save');
      continue;
    }
    seen.add(item.name);

    const size = item.enc?.length ?? 0;
    if (size > MAX_RECORD_BYTES) {
      fail('record_too_large', `record body exceeds ${MAX_RECORD_BYTES} bytes`);
      continue;
    }
    const current = existing.get(item.name) ?? null;
    const currentSize = current?.size_bytes ?? 0;
    if (size > currentSize && used + (size - currentSize) > cfg.storageCapBytes) {
      fail('quota_exceeded', 'account storage cap reached');
      continue;
    }

    // Precondition (also enforced again in SQL against concurrent writers).
    const guarded = policy === 'if_server_unchanged';
    let expectedVersion: number | null = null;
    if (guarded) {
      const mismatch =
        item.expectedTag === null ? current !== null : current === null || String(current.version) !== item.expectedTag;
      if (mismatch || (item.expectedTag !== null && !TAG.test(item.expectedTag))) {
        conflicts++;
        outcomes[pos] = { name: item.name, status: 'conflict', server_record: current ? await rowToWire(env, current) : null };
        continue;
      }
      if (item.expectedTag !== null) expectedVersion = Number(item.expectedTag);
    }

    // Spill large bodies to R2 before the D1 write; the key is per record and per content.
    let inline: ArrayBuffer | null = null;
    let r2Key: string | null = null;
    let newlyPut = false;
    if (item.enc && size > cfg.blobSpillBytes) {
      if (!env.BLOBS) {
        fail('blob_store_unavailable', 'large bodies are not enabled on this worker');
        continue;
      }
      r2Key = `${p.app}/${p.accountId}/${zone}/${await sha256Hex(item.name)}/${await sha256Hex(item.enc)}`;
      try {
        if (r2Key !== current?.r2_key) {
          await env.BLOBS.put(r2Key, toArrayBuffer(item.enc));
          newlyPut = true;
        }
      } catch {
        fail('blob_store_error', 'could not store body');
        continue;
      }
    } else if (item.enc) {
      inline = toArrayBuffer(item.enc);
    }

    // ?1..?10 identify and describe the row, ?11 is created_at (or the expected version), ?12 the
    // 1-based position of this write in the batch. Each statement returns the stored row.
    const offset = pending.length + 1;
    const args = [p.app, p.accountId, zone, item.name, item.type, item.updatedAt, item.deletedAt, inline, r2Key, size];
    let stmt: D1PreparedStatement;
    if (policy === 'overwrite') {
      stmt = db
        .prepare(
          `INSERT INTO records (app, account_id, zone, name, type, seq, version, updated_at, deleted_at, enc, r2_key, size_bytes, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ${seqSql} + ?12, 1, ?6, ?7, ?8, ?9, ?10, ?11)
           ON CONFLICT (app, account_id, zone, name) DO UPDATE SET
             type = excluded.type, seq = excluded.seq, version = records.version + 1,
             updated_at = excluded.updated_at, deleted_at = excluded.deleted_at, enc = excluded.enc,
             r2_key = excluded.r2_key, size_bytes = excluded.size_bytes, compacted_at = NULL
           RETURNING ${COLUMNS}`,
        )
        .bind(...args, nowMs, offset);
    } else if (expectedVersion === null) {
      stmt = db
        .prepare(
          `INSERT INTO records (app, account_id, zone, name, type, seq, version, updated_at, deleted_at, enc, r2_key, size_bytes, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ${seqSql} + ?12, 1, ?6, ?7, ?8, ?9, ?10, ?11)
           ON CONFLICT (app, account_id, zone, name) DO NOTHING
           RETURNING ${COLUMNS}`,
        )
        .bind(...args, nowMs, offset);
    } else {
      stmt = db
        .prepare(
          `UPDATE records SET type = ?5, seq = ${seqSql} + ?12, version = version + 1, updated_at = ?6, deleted_at = ?7,
             enc = ?8, r2_key = ?9, size_bytes = ?10, compacted_at = NULL
           WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name = ?4 AND version = ?11
           RETURNING ${COLUMNS}`,
        )
        .bind(...args, expectedVersion, offset);
    }
    used += size - currentSize;
    pending.push({ pos, item, current, r2Key, newlyPut, stmt });
  }

  let saved = 0;
  if (pending.length > 0) {
    const statements = pending.map((w) => w.stmt);
    statements.push(
      db
        .prepare('UPDATE zones SET last_seq = last_seq + ?4 WHERE app = ?1 AND account_id = ?2 AND zone = ?3')
        .bind(p.app, p.accountId, zone, pending.length),
    );
    let results: D1Result[];
    try {
      results = (await db.batch(statements)) as D1Result[];
    } catch (e) {
      // The transaction rolled back: drop bodies this save put into R2, then surface the failure.
      for (const w of pending) if (w.newlyPut && w.r2Key) await env.BLOBS?.delete(w.r2Key).catch(() => undefined);
      throw e;
    }

    const lost: PendingWrite[] = [];
    for (let i = 0; i < pending.length; i++) {
      const w = pending[i]!;
      const row = results[i]!.results[0] as unknown as Row | undefined;
      if (!row) {
        lost.push(w);
        continue;
      }
      if (w.current?.r2_key && w.current.r2_key !== w.r2Key) await env.BLOBS?.delete(w.current.r2_key).catch(() => undefined);
      saved++;
      outcomes[w.pos] = { name: w.item.name, status: 'ok', change_tag: String(row.version), seq: row.seq };
    }

    if (lost.length > 0) {
      // A concurrent writer won: report the server's current record (one extra query for all of them).
      const lostNames = JSON.stringify(lost.map((w) => w.item.name));
      const fresh = await db
        .prepare(
          `SELECT ${COLUMNS} FROM records
           WHERE app = ?1 AND account_id = ?2 AND zone = ?3 AND name IN (SELECT value FROM json_each(?4))`,
        )
        .bind(p.app, p.accountId, zone, lostNames)
        .all<Row>();
      const byName = new Map(fresh.results.map((r) => [r.name, r]));
      for (const w of lost) {
        if (w.newlyPut && w.r2Key) await env.BLOBS?.delete(w.r2Key).catch(() => undefined);
        const row = byName.get(w.item.name);
        conflicts++;
        outcomes[w.pos] = { name: w.item.name, status: 'conflict', server_record: row ? await rowToWire(env, row) : null };
      }
    }
  }

  event('sync.save', { app: p.app, saved, conflicts, errors });
  if (conflicts > 0) event('sync.conflict', { app: p.app, count: conflicts });
  return { outcomes: outcomes as SaveOutcome[] };
}
