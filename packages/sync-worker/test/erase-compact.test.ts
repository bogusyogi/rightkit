import { env, SELF } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import { compactTombstones, pruneHousekeeping } from '../src/compact.js';
import { b64, client, rec } from './helpers.js';

let n = 0;
const acct = () => `acct-ec-${++n}`;
const Z = 'transcripts';
const DAY = 24 * 3600 * 1000;
const CONFIRM = { 'x-rightkit-confirm': 'erase-account' };

async function count(table: string, account: string): Promise<number> {
  const row = await env.DB.prepare(`SELECT COUNT(*) AS c FROM ${table} WHERE account_id = ?1`).bind(account).first<{ c: number }>();
  return row!.c;
}

describe('account erase (#147)', () => {
  it('requires the confirmation header', async () => {
    const api = await client({ sub: acct() });
    const res = await api.call('DELETE', '/v1/sync/account');
    expect(res.status).toBe(400);
    expect(res.body.error.code).toBe('confirmation_required');
  });

  it('wipes rows, zones and R2 bodies for that account only', async () => {
    const mine = acct();
    const other = acct();
    const a = await client({ sub: mine });
    const b = await client({ sub: other });
    await a.prepare();
    await a.prepare('second');
    await b.prepare();
    const big = new Uint8Array(70_000).fill(3);
    await a.save(Z, 'overwrite', [rec('one'), rec('big', { enc: b64(big) })]);
    await b.save(Z, 'overwrite', [rec('keep'), rec('keep-big', { enc: b64(big) })]);

    const res = await a.call('DELETE', '/v1/sync/account', undefined, CONFIRM);
    expect(res.status).toBe(200);
    expect(res.body.erased).toEqual({ records: 2, zones: 2, blobs: 1 });

    expect(await count('records', mine)).toBe(0);
    expect(await count('zones', mine)).toBe(0);
    expect((await env.BLOBS!.list({ prefix: `heardright/${mine}/` })).objects).toHaveLength(0);

    expect(await count('records', other)).toBe(2);
    expect((await env.BLOBS!.list({ prefix: `heardright/${other}/` })).objects).toHaveLength(1);
    expect((await b.changes(Z)).body.records).toHaveLength(2);
  });

  it('revokes tokens issued before the erase but lets a new session start over', async () => {
    const id = acct();
    const old = await client({ sub: id });
    await old.prepare();
    await old.save(Z, 'overwrite', [rec('one')]);
    await old.call('DELETE', '/v1/sync/account', undefined, CONFIRM);

    const denied = await old.call('POST', `/v1/sync/zones/${Z}`);
    expect(denied.status).toBe(401);
    expect(denied.body.error.code).toBe('token_revoked');

    // Issued after the erase (iat is whole seconds, so step past the erase second).
    const fresh = await client({ sub: id, iatOffset: 5 });
    await fresh.prepare();
    expect((await fresh.changes(Z)).body.records).toEqual([]);
  });

  it('is idempotent and retryable with the revoked token', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    expect((await api.call('DELETE', '/v1/sync/account', undefined, CONFIRM)).status).toBe(200);
    const again = await api.call('DELETE', '/v1/sync/account', undefined, CONFIRM);
    expect(again.status).toBe(200);
    expect(again.body.erased).toEqual({ records: 0, zones: 0, blobs: 0 });
  });
});

describe('tombstone compaction (#147)', () => {
  async function seed(id: string) {
    const api = await client({ sub: id });
    await api.prepare();
    const big = new Uint8Array(70_000).fill(9);
    await api.save(Z, 'overwrite', [
      rec('live'),
      rec('old-tomb', { enc: b64(big) }),
      rec('recent-tomb'),
    ]);
    const now = Date.now();
    await api.save(Z, 'overwrite', [
      { name: 'old-tomb', type: 'Transcript', updated_at: now - 100 * DAY, deleted_at: now - 100 * DAY, enc: b64('tiny') },
      { name: 'recent-tomb', type: 'Transcript', updated_at: now - DAY, deleted_at: now - DAY, enc: b64('tiny') },
    ]);
    return { api, now };
  }

  it('strips old tombstones to metadata, keeps live and recent rows, raises the floor', async () => {
    const id = acct();
    const { api, now } = await seed(id);
    const report = await compactTombstones(env, now);
    expect(report.compacted).toBe(1);

    const old = await api.call('GET', `/v1/sync/zones/${Z}/records/old-tomb`);
    expect(old.body.record).toMatchObject({ name: 'old-tomb', type: 'Transcript', deleted_at: now - 100 * DAY, enc: null });
    const recent = await api.call('GET', `/v1/sync/zones/${Z}/records/recent-tomb`);
    expect(recent.body.record.enc).toBe(b64('tiny'));
    const live = await api.call('GET', `/v1/sync/zones/${Z}/records/live`);
    expect(live.body.record.enc).toBe(b64('cipher:live'));

    expect((await env.BLOBS!.list({ prefix: `heardright/${id}/` })).objects).toHaveLength(0);
    const zone = await env.DB.prepare('SELECT floor_seq FROM zones WHERE account_id = ?1').bind(id).first<{ floor_seq: number }>();
    expect(zone!.floor_seq).toBeGreaterThan(0);
  });

  it('expires cursors below the floor, still serves cursors at or above it and full replays', async () => {
    const id = acct();
    const { api, now } = await seed(id);
    await compactTombstones(env, now);
    const floor = (await env.DB.prepare('SELECT floor_seq FROM zones WHERE account_id = ?1').bind(id).first<{ floor_seq: number }>())!.floor_seq;

    const expired = await api.changes(Z, floor - 1);
    expect(expired.status).toBe(410);
    expect(expired.body.error.code).toBe('token_expired');
    expect((await api.changes(Z, floor)).status).toBe(200);
    const replay = await api.changes(Z);
    expect(replay.status).toBe(200);
    expect(replay.body.records.map((r: any) => r.name).sort()).toEqual(['live', 'old-tomb', 'recent-tomb']);
    expect((await api.changes(Z, 0)).status).toBe(200);
  });

  it('is idempotent and a later rewrite clears the compacted mark', async () => {
    const id = acct();
    const { api, now } = await seed(id);
    expect((await compactTombstones(env, now)).compacted).toBe(1);
    expect((await compactTombstones(env, now)).compacted).toBe(0);

    await api.save(Z, 'overwrite', [rec('old-tomb', { updated_at: now })]);
    const row = await env.DB.prepare('SELECT compacted_at, deleted_at FROM records WHERE account_id = ?1 AND name = ?2')
      .bind(id, 'old-tomb')
      .first<{ compacted_at: number | null; deleted_at: number | null }>();
    expect(row).toEqual({ compacted_at: null, deleted_at: null });
  });

  it('runs from the scheduled handler path and prunes housekeeping rows', async () => {
    const now = Date.now();
    await env.DB.prepare('INSERT INTO rate_limits (app, account_id, window_start, count) VALUES (?1, ?2, ?3, 1)')
      .bind('heardright', 'hk', now - 2 * 3600_000)
      .run();
    await env.DB.prepare('INSERT INTO erased_accounts (app, account_id, erased_at) VALUES (?1, ?2, ?3), (?1, ?4, ?5)')
      .bind('heardright', 'hk-old', now - 8 * DAY, 'hk-new', now - DAY)
      .run();
    const pruned = await pruneHousekeeping(env, now);
    expect(pruned.rateWindows).toBeGreaterThanOrEqual(1);
    expect(pruned.eraseMarkers).toBe(1);
    const left = await env.DB.prepare("SELECT account_id FROM erased_accounts WHERE account_id IN ('hk-old','hk-new')").all<{ account_id: string }>();
    expect(left.results.map((r) => r.account_id)).toEqual(['hk-new']);
  });
});

describe('wire shape', () => {
  it('the feed exposes only ordering metadata plus the opaque body', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('x')]);
    const res = await SELF.fetch(`https://sync.test/v1/sync/zones/${Z}/changes`, { headers: { authorization: `Bearer ${api.token}` } });
    const body = (await res.json()) as { records: Record<string, unknown>[] };
    expect(Object.keys(body.records[0]!).sort()).toEqual(['change_tag', 'deleted_at', 'enc', 'name', 'seq', 'type', 'updated_at']);
  });
});
