import { describe, expect, it } from 'vitest';
import {
  createLicenseClient,
  type LicenseStorage,
  type RefreshOutcome,
  type StoredLicense,
} from './index.js';

const NOW = Date.parse('2026-10-11T00:00:00.000Z');
const iso = (ms: number) => new Date(ms).toISOString();

/** An unsigned `payload.signature` pair; the JS client only ever reads `exp` from it. */
function tokenExpiring(expSeconds: number, label = 'x'): string {
  const payload = Buffer.from(JSON.stringify({ v: 2, id: label, exp: expSeconds })).toString('base64url');
  return `${payload}.c2ln`;
}
const live = tokenExpiring(NOW / 1000 + 3600, 'live');
const lapsed = tokenExpiring(NOW / 1000 - 3600, 'lapsed');
const fresh = tokenExpiring(NOW / 1000 + 7200, 'fresh');

function memStorage(initial: StoredLicense | null): LicenseStorage & { value: StoredLicense | null; writes: number } {
  return {
    value: initial,
    writes: 0,
    async get() { return this.value; },
    async set(l) { this.value = l; this.writes += 1; },
  };
}

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

function setup(initial: StoredLicense | null, respond: () => Response | Promise<Response>) {
  const storage = memStorage(initial);
  const requests: Array<{ url: string; body: Record<string, unknown> }> = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    requests.push({ url, body: JSON.parse(init.body as string) as Record<string, unknown> });
    return respond();
  }) as unknown as typeof fetch;
  const client = createLicenseClient({
    app: 'heardright', host: 'heardright.app', appVersion: '1.0.0',
    storage, deviceId: () => 'dev-1', fetchImpl, now: () => NOW,
  });
  return { client, storage, requests };
}

const stored = (patch: Partial<StoredLicense> = {}): StoredLicense => ({
  status: 'active', deviceId: 'dev-1', activationToken: 'act', validUntil: null, signedToken: live, ...patch,
});

const active = (extra: Record<string, unknown> = {}) => json(200, { ok: true, status: 'active', ...extra });

describe('refreshWithOutcome', () => {
  it('tokenReplaced: adopts the freshly minted token', async () => {
    const { client, storage, requests } = setup(stored(), () => active({ signedToken: fresh }));
    const { outcome, license } = await client.refreshWithOutcome();
    expect(outcome).toEqual({ kind: 'tokenReplaced' });
    expect(license?.signedToken).toBe(fresh);
    expect(license?.lastRefreshOutcome).toBe('tokenReplaced');
    expect(license?.lastRefreshedAt).toBe(iso(NOW));
    expect(storage.value).toEqual(license);
    expect(requests).toEqual([{
      url: 'https://heardright.app/v1/licenses/refresh',
      body: { app: 'heardright', activationToken: 'act', deviceId: 'dev-1' },
    }]);
  });

  it('tokenKept: server omitted the token and the cached one is still within its exp', async () => {
    for (const reply of [{}, { signedToken: null }, { signedToken: '   ' }]) {
      const { client } = setup(stored(), () => active(reply));
      const { outcome, license } = await client.refreshWithOutcome();
      expect(outcome).toEqual({
        kind: 'tokenKept', reason: 'server_omitted_token', validUntil: iso(NOW + 3600_000),
      });
      expect(license?.signedToken).toBe(live);
      expect(license?.lastRefreshOutcome).toBe('tokenKept');
    }
  });

  it('tokenKept: server echoed the token we already hold', async () => {
    const { client } = setup(stored(), () => active({ signedToken: live }));
    const { outcome } = await client.refreshWithOutcome();
    expect(outcome).toMatchObject({ kind: 'tokenKept', reason: 'server_returned_same_token' });
  });

  it('tokenDropped: server omitted the token and the cached one is past its exp', async () => {
    const { client, storage } = setup(stored({ signedToken: lapsed }), () => active());
    const { outcome, license } = await client.refreshWithOutcome();
    expect(outcome).toEqual({ kind: 'tokenDropped', reason: 'cached_token_lapsed' });
    expect(license?.status).toBe('active');
    expect(license).not.toHaveProperty('signedToken');
    expect(storage.value).not.toHaveProperty('signedToken');
  });

  it('tokenDropped: an echoed token that has itself lapsed is not kept', async () => {
    const { client } = setup(stored({ signedToken: lapsed }), () => active({ signedToken: lapsed }));
    expect((await client.refreshWithOutcome()).outcome).toEqual({ kind: 'tokenDropped', reason: 'cached_token_lapsed' });
  });

  it('tokenDropped: an undecodable cached token is not kept', async () => {
    for (const junk of ['OLD.SIG', 'nodot', '!!!.sig', `${Buffer.from('{"exp":"soon"}').toString('base64url')}.s`]) {
      const { client } = setup(stored({ signedToken: junk }), () => active());
      expect((await client.refreshWithOutcome()).outcome).toEqual({ kind: 'tokenDropped', reason: 'cached_token_lapsed' });
    }
  });

  it('tokenDropped: nothing cached and nothing minted', async () => {
    const { client } = setup(stored({ signedToken: undefined }), () => active());
    const { outcome, license } = await client.refreshWithOutcome();
    expect(outcome).toEqual({ kind: 'tokenDropped', reason: 'no_cached_token' });
    expect(license?.status).toBe('active');
  });

  it('stale: network failure keeps the record, stamps staleSince once, and a later success clears it', async () => {
    let online = false;
    const { client, storage } = setup(stored(), () => {
      if (!online) throw new Error('network down');
      return active({ signedToken: fresh });
    });
    const first = await client.refreshWithOutcome();
    expect(first.outcome).toEqual({ kind: 'stale', staleSince: iso(NOW), error: 'network down' });
    expect(first.license).toMatchObject({ status: 'active', signedToken: live, staleSince: iso(NOW), lastRefreshOutcome: 'stale' });

    // A second failure later keeps the original stamp.
    storage.value = { ...storage.value!, staleSince: '2026-10-01T00:00:00.000Z' };
    const second = await client.refreshWithOutcome();
    expect(second.outcome).toMatchObject({ kind: 'stale', staleSince: '2026-10-01T00:00:00.000Z' });

    online = true;
    const recovered = await client.refreshWithOutcome();
    expect(recovered.outcome.kind).toBe('tokenReplaced');
    expect(recovered.license).not.toHaveProperty('staleSince');
  });

  it.each([500, 503])('stale: HTTP %s is an outage, not a revocation', async (status) => {
    const { client } = setup(stored(), () => json(status, { ok: false, status: 'error', message: 'boom' }));
    expect((await client.refreshWithOutcome()).outcome).toMatchObject({ kind: 'stale', error: 'boom' });
  });

  it('stale: a non-JSON proxy error is an outage', async () => {
    const { client } = setup(stored(), () => new Response('<html>bad gateway</html>', { status: 502 }));
    expect((await client.refreshWithOutcome()).outcome).toMatchObject({
      kind: 'stale', error: 'Activation request failed (HTTP 502).',
    });
  });

  it('revoked: an authoritative rejection invalidates the record and clears staleSince', async () => {
    const { client, storage } = setup(stored({ staleSince: '2026-10-01T00:00:00.000Z' }), () =>
      json(404, { ok: false, status: 'not_found', message: 'License inactive' }));
    const { outcome, license } = await client.refreshWithOutcome();
    expect(outcome).toEqual({ kind: 'revoked', message: 'License inactive' });
    expect(license).toMatchObject({ status: 'invalid', message: 'License inactive', lastRefreshOutcome: 'revoked' });
    expect(license).not.toHaveProperty('staleSince');
    expect(storage.value).toEqual(license);
  });

  it('revoked: a 200 with a non-active status also revokes, with a default message', async () => {
    const { client } = setup(stored(), () => json(200, { ok: false, status: 'invalid' }));
    expect((await client.refreshWithOutcome()).outcome).toEqual({ kind: 'revoked', message: 'License is no longer active' });
  });

  it('noCredential: sends nothing and writes nothing', async () => {
    for (const initial of [null, { status: 'invalid', deviceId: 'd', key: 'k' } as StoredLicense]) {
      const { client, storage, requests } = setup(initial, () => active());
      const { outcome, license } = await client.refreshWithOutcome();
      expect(outcome).toEqual({ kind: 'noCredential' });
      expect(license).toEqual(initial);
      expect(requests).toHaveLength(0);
      expect(storage.writes).toBe(0);
    }
  });
});

describe('refresh() keeps its record-returning contract and surfaces the outcome on the record', () => {
  it('returns the stored record with lastRefreshOutcome / staleSince', async () => {
    const offline = setup(stored(), () => { throw new Error('down'); });
    const stale = await offline.client.refresh();
    expect(stale).toMatchObject({ status: 'active', lastRefreshOutcome: 'stale', staleSince: iso(NOW) });

    const kept = await setup(stored(), () => active()).client.refresh();
    expect(kept).toMatchObject({ status: 'active', signedToken: live, lastRefreshOutcome: 'tokenKept' });

    const replaced = await setup(stored(), () => active({ signedToken: fresh })).client.refresh();
    expect(replaced?.lastRefreshOutcome).toBe('tokenReplaced');

    const none = await setup(null, () => active()).client.refresh();
    expect(none).toBeNull();
  });

  it('typed outcomes are exhaustive over the documented kinds', () => {
    const kinds = new Set<RefreshOutcome['kind']>([
      'tokenReplaced', 'tokenKept', 'tokenDropped', 'stale', 'revoked', 'noCredential',
    ]);
    expect(kinds.size).toBe(6);
  });
});
