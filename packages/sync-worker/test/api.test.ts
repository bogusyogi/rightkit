import { env, SELF } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import { b64, client, dbOf, mint, rec } from './helpers.js';

let n = 0;
const acct = () => `acct-api-${++n}`;
const Z = 'transcripts';

describe('health and routing', () => {
  it('serves health without auth', async () => {
    const res = await SELF.fetch('https://sync.test/v1/sync/health');
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true, protocol: 'rightkit-sync-http/1', max_save_records: 40 });
  });

  it('404s unknown routes and 405s wrong methods', async () => {
    const api = await client({ sub: acct() });
    expect((await api.call('GET', '/v1/sync/nope')).status).toBe(404);
    expect((await api.call('GET', `/v1/sync/zones/${Z}`)).status).toBe(405);
  });

  it('rejects an invalid zone name', async () => {
    const api = await client({ sub: acct() });
    expect((await api.call('POST', '/v1/sync/zones/bad%20zone')).body.error.code).toBe('bad_zone');
  });
});

describe('authentication', () => {
  const paths = `/v1/sync/zones/${Z}/changes`;
  const get = (headers: Record<string, string>) => SELF.fetch(`https://sync.test${paths}`, { headers });

  it('requires a bearer token', async () => {
    expect((await get({})).status).toBe(401);
  });

  it('rejects a token signed with the wrong key', async () => {
    const token = await mint({ secret: 'some-other-secret-0123456789abcdef' });
    expect((await get({ authorization: `Bearer ${token}` })).status).toBe(401);
  });

  it('rejects other algorithms, audiences, scopes and expired or future tokens', async () => {
    for (const opts of [
      { alg: 'none' },
      { aud: 'other' },
      { scp: 'admin' },
      { iatOffset: -7200, lifetime: 3600 },
      { iatOffset: 3600 },
      { lifetime: 3 * 24 * 3600 },
    ]) {
      const token = await mint(opts);
      expect((await get({ authorization: `Bearer ${token}` })).status).toBe(401);
    }
  });

  it('forbids an app that is not enabled', async () => {
    const token = await mint({ app: 'unknown-app' });
    const res = await get({ authorization: `Bearer ${token}` });
    expect(res.status).toBe(403);
  });

  it('accepts a token signed with the previous key (rotation)', async () => {
    const api = await client({ sub: acct(), secret: env.SYNC_JWT_SECRET_PREVIOUS });
    expect((await api.call('POST', `/v1/sync/zones/${Z}`)).status).toBe(200);
  });
});

describe('zones', () => {
  it('prepare is idempotent', async () => {
    const api = await client({ sub: acct() });
    expect((await api.call('POST', `/v1/sync/zones/${Z}`)).body).toEqual({ zone: Z, created: true });
    expect((await api.call('POST', `/v1/sync/zones/${Z}`)).body).toEqual({ zone: Z, created: false });
  });

  it('caps zones per account', async () => {
    const api = await client({ sub: acct() });
    for (let i = 0; i < 16; i++) expect((await api.call('POST', `/v1/sync/zones/z${i}`)).status).toBe(200);
    // 16 prepares use 16 of 25 rate-limit units; the 17th is still inside the budget.
    const res = await api.call('POST', '/v1/sync/zones/z16');
    expect(res.status).toBe(403);
    expect(res.body.error.code).toBe('zone_limit');
  });

  it('answers 404 zone_missing before prepare', async () => {
    const api = await client({ sub: acct() });
    expect((await api.changes(Z)).status).toBe(404);
    expect((await api.call('GET', `/v1/sync/zones/${Z}/records/a`)).body.error.code).toBe('zone_missing');
    expect((await api.call('POST', `/v1/sync/zones/${Z}/save`, { policy: 'overwrite', records: [] })).status).toBe(404);
  });
});

describe('save and preconditions', () => {
  it('inserts, then refuses a blind insert over an existing record', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    const first = await api.save(Z, 'if_server_unchanged', [rec('r1')]);
    expect(first.outcomes[0]).toMatchObject({ name: 'r1', status: 'ok', change_tag: '1', seq: 1 });
    const second = await api.save(Z, 'if_server_unchanged', [rec('r1', { updated_at: 2_000 })]);
    expect(second.outcomes[0].status).toBe('conflict');
    expect(second.outcomes[0].server_record).toMatchObject({ name: 'r1', change_tag: '1', updated_at: 1_000 });
  });

  it('updates with the right tag and bumps tag and seq', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'if_server_unchanged', [rec('r1')]);
    const out = await api.save(Z, 'if_server_unchanged', [rec('r1', { change_tag: '1', updated_at: 2_000 })]);
    expect(out.outcomes[0]).toMatchObject({ status: 'ok', change_tag: '2', seq: 2 });
  });

  it('conflicts on a stale tag and on a tag for a missing record', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'if_server_unchanged', [rec('r1')]);
    await api.save(Z, 'if_server_unchanged', [rec('r1', { change_tag: '1' })]);
    const stale = await api.save(Z, 'if_server_unchanged', [rec('r1', { change_tag: '1' }), rec('ghost', { change_tag: '4' })]);
    expect(stale.outcomes[0]).toMatchObject({ status: 'conflict', server_record: { change_tag: '2' } });
    expect(stale.outcomes[1]).toEqual({ name: 'ghost', status: 'conflict', server_record: null });
  });

  it('overwrite replaces with a tombstone and keeps working for new names', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('r1')]);
    const out = await api.save(Z, 'overwrite', [{ name: 'r1', type: 'Transcript', updated_at: 5_000, deleted_at: 5_000, enc: null }]);
    expect(out.outcomes[0]).toMatchObject({ status: 'ok', change_tag: '2' });
    const got = await api.call('GET', `/v1/sync/zones/${Z}/records/r1`);
    expect(got.body.record).toMatchObject({ name: 'r1', deleted_at: 5_000, enc: null, updated_at: 5_000 });
  });

  it('returns per-record errors without failing the batch', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    const out = await api.save(Z, 'overwrite', [
      rec('ok'),
      { name: 'bad name!', type: 'Transcript', updated_at: 1 },
      rec('badb64', { enc: '***' }),
      rec('badtime', { updated_at: -4 }),
    ]);
    expect(out.outcomes.map((o: any) => o.status)).toEqual(['ok', 'error', 'error', 'error']);
    expect(out.outcomes[1].code).toBe('invalid_record');
  });

  it('validates the request shape', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    expect((await api.call('POST', `/v1/sync/zones/${Z}/save`, { policy: 'nope', records: [] })).body.error.code).toBe('bad_policy');
    expect((await api.call('POST', `/v1/sync/zones/${Z}/save`, { policy: 'overwrite', records: 'x' })).body.error.code).toBe('bad_batch');
  });

  it('stores ciphertext verbatim (opaque to the server)', async () => {
    const acc = acct();
    const api = await client({ sub: acc });
    await api.prepare();
    const bytes = Uint8Array.from([0, 255, 1, 254, 128, 7]);
    await api.save(Z, 'overwrite', [rec('bin', { enc: b64(bytes) })]);
    const got = await api.call('GET', `/v1/sync/zones/${Z}/records/bin`);
    expect(got.body.record.enc).toBe(b64(bytes));
  });
});

describe('change feed', () => {
  it('pages in seq order with an opaque cursor and reports more_coming', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('a'), rec('b'), rec('c')]);
    const p1 = await api.changes(Z, undefined, 2);
    expect(p1.body.records.map((r: any) => r.name)).toEqual(['a', 'b']);
    expect(p1.body).toMatchObject({ more_coming: true, new_token: '2', deleted_record_names: [] });
    const p2 = await api.changes(Z, p1.body.new_token, 2);
    expect(p2.body.records.map((r: any) => r.name)).toEqual(['c']);
    expect(p2.body).toMatchObject({ more_coming: false, new_token: '3' });
    const p3 = await api.changes(Z, p2.body.new_token);
    expect(p3.body).toMatchObject({ records: [], more_coming: false, new_token: '3' });
  });

  it('moves a rewritten record to the head of the feed', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('a'), rec('b')]);
    await api.save(Z, 'overwrite', [rec('a', { updated_at: 9_000 })]);
    const page = await api.changes(Z, 2);
    expect(page.body.records.map((r: any) => r.name)).toEqual(['a']);
  });

  it('answers 410 token_expired for malformed or future cursors, accepts 0', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    await api.save(Z, 'overwrite', [rec('a')]);
    expect((await api.changes(Z, 'abc')).status).toBe(410);
    expect((await api.changes(Z, 99)).body.error.code).toBe('token_expired');
    expect((await api.changes(Z, 0)).status).toBe(200);
  });

  it('rejects a bad limit', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    expect((await api.changes(Z, undefined, 0)).status).toBe(400);
  });

  it('isolates accounts and apps that share a zone name', async () => {
    const a = await client({ sub: acct() });
    const b = await client({ sub: acct() });
    const c = await client({ sub: 'shared-id', app: 'heardright' });
    const d = await client({ sub: 'shared-id', app: 'otherapp' });
    for (const x of [a, b, c, d]) await x.prepare();
    await a.save(Z, 'overwrite', [rec('mine')]);
    await c.save(Z, 'overwrite', [rec('c-only')]);
    expect((await b.changes(Z)).body.records).toEqual([]);
    expect((await d.changes(Z)).body.records).toEqual([]);
    expect((await b.call('GET', `/v1/sync/zones/${Z}/records/mine`)).body.record).toBeNull();
  });
});

describe('large bodies and quota', () => {
  it('spills large bodies to R2, serves them back and cleans up replaced objects', async () => {
    const acc = acct();
    const api = await client({ sub: acc });
    await api.prepare();
    const big = new Uint8Array(70_000).fill(7);
    await api.save(Z, 'overwrite', [rec('big', { enc: b64(big) })]);
    const stored = await (await dbOf(acc)).prepare('SELECT enc, r2_key FROM records WHERE account_id = ?1 AND name = ?2')
      .bind(acc, 'big')
      .first<{ enc: unknown; r2_key: string | null }>();
    expect(stored?.enc).toBeNull();
    expect(stored?.r2_key).toBeTruthy();
    expect(await env.BLOBS!.head(stored!.r2_key!)).not.toBeNull();

    const page = await api.changes(Z);
    expect(page.body.records[0].enc).toBe(b64(big));

    const bigger = new Uint8Array(71_000).fill(8);
    await api.save(Z, 'overwrite', [rec('big', { enc: b64(bigger) })]);
    expect(await env.BLOBS!.head(stored!.r2_key!)).toBeNull();
    const listed = await env.BLOBS!.list({ prefix: `heardright/${acc}/` });
    expect(listed.objects).toHaveLength(1);
  });

  it('refuses a record over the size limit', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    const huge = new Uint8Array(1_048_577);
    const out = await api.save(Z, 'overwrite', [rec('huge', { enc: b64(huge) })]);
    expect(out.outcomes[0]).toMatchObject({ status: 'error', code: 'record_too_large' });
  });

  it('enforces the account storage cap, and lets tombstones through', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    const chunk = new Uint8Array(200_000).fill(1);
    const first = await api.save(Z, 'overwrite', [rec('one', { enc: b64(chunk) })]);
    expect(first.outcomes[0].status).toBe('ok');
    const second = await api.save(Z, 'overwrite', [rec('two', { enc: b64(chunk) })]);
    expect(second.outcomes[0]).toMatchObject({ status: 'error', code: 'quota_exceeded' });
    const tomb = await api.save(Z, 'overwrite', [{ name: 'one', type: 'Transcript', updated_at: 2, deleted_at: 2 }]);
    expect(tomb.outcomes[0].status).toBe('ok');
    const retry = await api.save(Z, 'overwrite', [rec('two', { enc: b64(chunk) })]);
    expect(retry.outcomes[0].status).toBe('ok');
  });
});

describe('rate limiting', () => {
  it('returns 429 with Retry-After once the window budget is spent', async () => {
    const api = await client({ sub: acct() });
    let limited: Awaited<ReturnType<typeof api.call>> | undefined;
    for (let i = 0; i < 40 && !limited; i++) {
      const r = await api.call('GET', '/v1/sync/zones/none/changes');
      if (r.status === 429) limited = r;
    }
    expect(limited).toBeDefined();
    expect(limited!.body.error.code).toBe('rate_limited');
    expect(Number(limited!.headers.get('retry-after'))).toBeGreaterThanOrEqual(1);
  });

  it('charges a save by its record count', async () => {
    const api = await client({ sub: acct() });
    await api.prepare();
    const records = Array.from({ length: 30 }, (_, i) => rec(`r${i}`));
    const res = await api.call('POST', `/v1/sync/zones/${Z}/save`, { policy: 'overwrite', records });
    expect(res.status).toBe(429);
  });
});
