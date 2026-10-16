import { env } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import type { Principal } from '../src/auth.js';
import { config, type Env } from '../src/env.js';
import { prepareZone, saveRecords } from '../src/records.js';
import { allShards, shardBinding, shardCount, shardIndex } from '../src/shards.js';
import { b64, client, rec, shardOf } from './helpers.js';

const Z = 'transcripts';
const CONFIRM = { 'x-rightkit-confirm': 'erase-account' };
let n = 0;
const uniq = (prefix: string) => `${prefix}-${++n}-${Math.random().toString(36).slice(2, 8)}`;

async function perShard(sql: string, ...args: unknown[]): Promise<number[]> {
  const counts: number[] = [];
  for (const { db } of allShards(env)) {
    counts.push((await db.prepare(sql).bind(...args).first<{ c: number }>())!.c);
  }
  return counts;
}

/** An account id whose home shard is `index`. */
async function accountOnShard(index: number, app = 'heardright'): Promise<string> {
  for (let i = 0; i < 10_000; i++) {
    const id = `acct-s${index}-${i}`;
    if ((await shardIndex(app, id, shardCount(env))) === index) return id;
  }
  throw new Error('no account found for shard');
}

function principal(accountId: string, app = 'heardright'): Principal {
  return { app, accountId, iat: Math.floor(Date.now() / 1000) };
}

/** Wraps one shard so tests can count D1 statements and round trips. */
function counting(db: D1Database): { db: D1Database; stats: { statements: number; batches: number } } {
  const stats = { statements: 0, batches: 0 };
  const wrapped = {
    prepare: (q: string) => db.prepare(q),
    batch: (s: D1PreparedStatement[]) => {
      stats.batches++;
      stats.statements += s.length;
      return db.batch(s);
    },
  } as unknown as D1Database;
  return { db: wrapped, stats };
}

describe('shard routing', () => {
  it('is deterministic and equals SHA-256(app|account) mod N', async () => {
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode('heardright|acct-fixed')));
    const first32 = ((digest[0]! << 24) | (digest[1]! << 16) | (digest[2]! << 8) | digest[3]!) >>> 0;
    for (const count of [1, 2, 3, 4, 7, 64]) {
      const expected = count === 1 ? 0 : first32 % count;
      expect(await shardIndex('heardright', 'acct-fixed', count)).toBe(expected);
      expect(await shardIndex('heardright', 'acct-fixed', count)).toBe(expected);
    }
  });

  it('uses every shard, and the app is part of the key', async () => {
    const seen = new Set<number>();
    for (let i = 0; i < 200; i++) seen.add(await shardIndex('heardright', `acct-${i}`, 4));
    expect([...seen].sort()).toEqual([0, 1, 2, 3]);
    let differs = false;
    for (let i = 0; i < 50 && !differs; i++) {
      differs = (await shardIndex('heardright', `acct-${i}`, 4)) !== (await shardIndex('otherapp', `acct-${i}`, 4));
    }
    expect(differs).toBe(true);
  });

  it('routes the same tenant to the same shard on every request', async () => {
    const id = uniq('route');
    const api = await client({ sub: id });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('a')]);
    await api.save(Z, 'overwrite', [rec('b')]);
    const counts = await perShard('SELECT COUNT(*) AS c FROM records WHERE account_id = ?1', id);
    const home = await shardOf(id);
    expect(counts[home]).toBe(2);
    expect(counts.reduce((a, b) => a + b, 0)).toBe(2);
  });

  it('fails loudly when a shard binding is missing', () => {
    const broken = { ...env, SHARD_COUNT: '5' } as unknown as Env;
    expect(() => shardBinding(broken, 4)).toThrow(/DB_4/);
  });

  it('reads plan and shard configuration from env', () => {
    expect(config({ ...env, CF_PLAN: 'free' } as Env)).toMatchObject({ shardCount: 4, maxSaveRecords: 40, queryBudget: 50 });
    expect(config({ ...env, CF_PLAN: 'paid' } as Env)).toMatchObject({ maxSaveRecords: 100, queryBudget: 1000 });
    expect(config({ ...env, CF_PLAN: 'paid', MAX_SAVE_RECORDS: '7' } as Env).maxSaveRecords).toBe(7);
    expect(config({ ...env, MAX_SAVE_RECORDS: '500' } as Env).maxSaveRecords).toBe(100);
    expect(config({ ...env, CF_PLAN: undefined, MAX_SAVE_RECORDS: 'x' } as Env).maxSaveRecords).toBe(40);
    expect(config({ ...env, SHARD_COUNT: '1000' } as Env).shardCount).toBe(64);
  });
});

describe('isolation across shards', () => {
  it('keeps each tenant in its own shard, with one tenant per shard visible only to itself', async () => {
    const apis = [];
    const ids: string[] = [];
    for (let s = 0; s < shardCount(env); s++) {
      const id = await accountOnShard(s);
      ids.push(id);
      const api = await client({ sub: id });
      await api.prepare();
      await api.save(Z, 'overwrite', [rec(`only-${s}`)]);
      apis.push(api);
    }
    for (let s = 0; s < ids.length; s++) {
      const counts = await perShard('SELECT COUNT(*) AS c FROM records WHERE account_id = ?1', ids[s]);
      expect(counts).toEqual(ids.map((_, i) => (i === s ? 1 : 0)));
      const feed = (await apis[s]!.changes(Z)).body.records.map((r: any) => r.name);
      expect(feed).toEqual([`only-${s}`]);
    }
  });

  it('separates two apps that share an account id', async () => {
    const id = uniq('shared');
    const a = await client({ sub: id, app: 'heardright' });
    const b = await client({ sub: id, app: 'otherapp' });
    await a.prepare();
    await b.prepare();
    await a.save(Z, 'overwrite', [rec('from-a')]);
    expect((await b.changes(Z)).body.records).toEqual([]);
    const total = (await perShard('SELECT COUNT(*) AS c FROM records WHERE account_id = ?1', id)).reduce((x, y) => x + y, 0);
    expect(total).toBe(1);
  });

  it('keeps the rate limit and erase marker on the home shard only', async () => {
    const id = await accountOnShard(2);
    const api = await client({ sub: id });
    await api.prepare();
    const rl = await perShard('SELECT COUNT(*) AS c FROM rate_limits WHERE account_id = ?1', id);
    expect(rl).toEqual([0, 0, 1, 0]);
    await api.call('DELETE', '/v1/sync/account', undefined, CONFIRM);
    const markers = await perShard('SELECT COUNT(*) AS c FROM erased_accounts WHERE account_id = ?1', id);
    expect(markers).toEqual([0, 0, 1, 0]);
  });
});

describe('erase across shards', () => {
  it('erases the account on every shard, including strays, and leaves other shards alone', async () => {
    const mine = await accountOnShard(1);
    const neighbour = await accountOnShard(3);
    const a = await client({ sub: mine });
    const b = await client({ sub: neighbour });
    await a.prepare();
    await b.prepare();
    await a.save(Z, 'overwrite', [rec('one'), rec('two')]);
    await b.save(Z, 'overwrite', [rec('keep')]);

    // A stray copy of the account on a non-home shard (what a changed SHARD_COUNT would leave behind).
    const stray = shardBinding(env, 0);
    await stray.batch([
      stray.prepare("INSERT INTO zones (app, account_id, zone, last_seq, floor_seq, created_at) VALUES ('heardright', ?1, 'old', 1, 0, 0)").bind(mine),
      stray
        .prepare(
          `INSERT INTO records (app, account_id, zone, name, type, seq, version, updated_at, enc, size_bytes, created_at)
           VALUES ('heardright', ?1, 'old', 'stray', 'T', 1, 1, 1, X'01', 1, 0)`,
        )
        .bind(mine),
    ]);

    const res = await a.call('DELETE', '/v1/sync/account', undefined, CONFIRM);
    expect(res.status).toBe(200);
    expect(res.body.erased).toEqual({ records: 3, zones: 2, blobs: 0 });

    expect(await perShard('SELECT COUNT(*) AS c FROM records WHERE account_id = ?1', mine)).toEqual([0, 0, 0, 0]);
    expect(await perShard('SELECT COUNT(*) AS c FROM zones WHERE account_id = ?1', mine)).toEqual([0, 0, 0, 0]);
    expect(await perShard('SELECT COUNT(*) AS c FROM records WHERE account_id = ?1', neighbour)).toEqual([0, 0, 0, 1]);
    expect((await b.changes(Z)).body.records.map((r: any) => r.name)).toEqual(['keep']);

    // Retryable and idempotent with the now-revoked token.
    const again = await a.call('DELETE', '/v1/sync/account', undefined, CONFIRM);
    expect(again.body.erased).toEqual({ records: 0, zones: 0, blobs: 0 });
  });
});

describe('batched save', () => {
  it('returns per-record outcomes in order from one write batch, with contiguous seq', async () => {
    const id = uniq('batch');
    const api = await client({ sub: id });
    await api.prepare();
    await api.save(Z, 'if_server_unchanged', [rec('exists')]);
    const out = await api.save(Z, 'if_server_unchanged', [
      rec('new-1'),
      rec('exists'), // no tag but it exists: conflict
      { name: 'bad name', type: 'Transcript', updated_at: 1 }, // invalid
      rec('new-2'),
      rec('new-1'), // duplicate within the save
      rec('ghost', { change_tag: '9' }), // tag for a missing record: conflict
      rec('new-3'),
    ]);
    expect(out.outcomes.map((o: any) => o.status)).toEqual(['ok', 'conflict', 'error', 'ok', 'error', 'conflict', 'ok']);
    expect(out.outcomes.map((o: any) => o.name)).toEqual(['new-1', 'exists', 'bad name', 'new-2', 'new-1', 'ghost', 'new-3']);
    expect(out.outcomes[4]).toMatchObject({ code: 'invalid_record' });
    expect(out.outcomes[1].server_record).toMatchObject({ name: 'exists', change_tag: '1' });
    // The earlier save used seq 1; only the three accepted writes consume seq 2..4.
    expect([out.outcomes[0].seq, out.outcomes[3].seq, out.outcomes[6].seq]).toEqual([2, 3, 4]);
    const feed = await api.changes(Z);
    expect(feed.body.new_token).toBe('4');
    expect(feed.body.records.map((r: any) => r.name)).toEqual(['exists', 'new-1', 'new-2', 'new-3']);
  });

  it('lets exactly one of two concurrent blind inserts win and reports the other as a conflict', async () => {
    const id = uniq('race');
    const a = await client({ sub: id });
    const b = await client({ sub: id });
    await a.prepare();
    const [x, y] = await Promise.all([
      a.save(Z, 'if_server_unchanged', [rec('same', { enc: b64('from-a') })]),
      b.save(Z, 'if_server_unchanged', [rec('same', { enc: b64('from-b') })]),
    ]);
    const statuses = [x.outcomes[0].status, y.outcomes[0].status].sort();
    expect(statuses).toEqual(['conflict', 'ok']);
    const loser = x.outcomes[0].status === 'conflict' ? x : y;
    expect(loser.outcomes[0].server_record).toMatchObject({ name: 'same', change_tag: '1' });
  });

  it('saves the plan maximum in two write round trips with N + 4 statements (free: 40)', async () => {
    const id = uniq('free40');
    const p = principal(id);
    await prepareZone(env as Env, p, Z, Date.now());
    const home = await shardOf(id);
    const { db, stats } = counting(shardBinding(env, home));
    const wrapped = { ...env, [`DB_${home}`]: db } as unknown as Env;
    const records = Array.from({ length: 40 }, (_, i) => rec(`r${i}`));
    const out = await saveRecords(wrapped, p, Z, { policy: 'overwrite', records }, Date.now());
    expect(out.outcomes.every((o) => o.status === 'ok')).toBe(true);
    expect(out.outcomes.map((o) => (o as any).seq)).toEqual(Array.from({ length: 40 }, (_, i) => i + 1));
    expect(stats.batches).toBe(2); // prefetch, then writes plus the seq bump
    expect(stats.statements).toBe(3 + 40 + 1);
    const last = await shardBinding(env, home).prepare('SELECT last_seq FROM zones WHERE account_id = ?1').bind(id).first<{ last_seq: number }>();
    expect(last!.last_seq).toBe(40);
  });

  it('refuses a save above the plan maximum and accepts 100 on the paid plan', async () => {
    const id = uniq('cap');
    const p = principal(id);
    await prepareZone(env as Env, p, Z, Date.now());
    const records = Array.from({ length: 41 }, (_, i) => rec(`r${i}`));
    await expect(saveRecords(env as Env, p, Z, { policy: 'overwrite', records }, Date.now())).rejects.toMatchObject({
      status: 400,
      code: 'bad_batch',
    });
    const custom = { ...env, MAX_SAVE_RECORDS: '3' } as unknown as Env;
    await expect(saveRecords(custom, p, Z, { policy: 'overwrite', records: records.slice(0, 4) }, Date.now())).rejects.toMatchObject({
      code: 'bad_batch',
    });

    const paid = { ...env, CF_PLAN: 'paid' } as unknown as Env;
    const hundred = Array.from({ length: 100 }, (_, i) => rec(`p${i}`));
    const out = await saveRecords(paid, p, Z, { policy: 'overwrite', records: hundred }, Date.now());
    expect(out.outcomes.filter((o) => o.status === 'ok')).toHaveLength(100);
    // 101 is still above the protocol maximum on any plan.
    await expect(
      saveRecords(paid, p, Z, { policy: 'overwrite', records: [...hundred, rec('extra')] }, Date.now()),
    ).rejects.toMatchObject({ code: 'bad_batch' });
  });

  it('updates with a tag inside a batch and keeps tag and seq in step', async () => {
    const id = uniq('upd');
    const api = await client({ sub: id });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('a'), rec('b')]);
    const out = await api.save(Z, 'if_server_unchanged', [
      rec('a', { change_tag: '1', updated_at: 2_000 }),
      rec('b', { change_tag: '7' }), // stale
    ]);
    expect(out.outcomes[0]).toMatchObject({ status: 'ok', change_tag: '2', seq: 3 });
    expect(out.outcomes[1]).toMatchObject({ status: 'conflict', server_record: { change_tag: '1' } });
  });
});
