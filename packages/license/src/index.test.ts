import { describe, it, expect } from 'vitest';
import {
  createLicenseClient,
  hasAnyFeature,
  hasFeature,
  isLicensed,
  isPro,
  satisfiesRequirement,
  tier,
  tierAtLeast,
  type StoredLicense,
  type LicenseStorage,
} from './index.js';
import publicVector from '../test-vectors/license-v2.json';

function memStorage(): LicenseStorage & { value: StoredLicense | null } {
  return { value: null, async get() { return this.value; }, async set(l) { this.value = l; } };
}
const jsonResponse = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
function clientWith(fetchImpl: typeof fetch, storage = memStorage()) {
  const c = createLicenseClient({ app: 'coderight', host: 'coderight.cc', appVersion: '0.1.0', storage, deviceId: () => 'dev-1', fetchImpl });
  return { c, storage };
}

describe('activate', () => {
  it('stores an active license + entitlements and drops the raw key', async () => {
    const fetchImpl = (async () => jsonResponse(200, { ok: true, status: 'active', activationToken: 'sr_act_x', licenseId: 'lic_1', validUntil: null, entitlements: { v: 1, tier: 'pro', features: ['routing'] } })) as unknown as typeof fetch;
    const { c, storage } = clientWith(fetchImpl);
    const lic = await c.activate('AD-XXXX');
    expect(lic.status).toBe('active');
    expect(lic.activationToken).toBe('sr_act_x');
    expect(lic.key).toBeUndefined();
    expect(lic.entitlements?.features).toContain('routing');
    expect((await storage.get())?.status).toBe('active');
  });
  it('carries the portable v2 signed token unchanged into native storage', async () => {
    const fetchImpl = (async () => jsonResponse(200, { ok: true, status: 'active', activationToken: 'tok', licenseId: 'lic_1', validUntil: null, entitlements: { v: 1, tier: 'pro', features: publicVector.expectedFeatures }, signedToken: publicVector.token })) as unknown as typeof fetch;
    const { c, storage } = clientWith(fetchImpl);
    expect((await c.activate('AD-XXXX')).signedToken).toBe(publicVector.token);
    expect((await storage.get())?.signedToken).toBe(publicVector.token);
  });
  it('stores the signed lifecycle hints returned with a provisional lifetime token', async () => {
    const fetchImpl = (async () => jsonResponse(200, {
      ok: true,
      status: 'active',
      activationToken: 'tok',
      licenseId: 'lic_life',
      validUntil: '2026-07-31T00:00:00.000Z',
      licenseKind: 'lifetime',
      entitlementStage: 'provisional',
      licenseIssuedAt: '2026-07-01T00:00:00.000Z',
      confirmationDueAt: '2026-07-31T00:00:00.000Z',
      entitlements: { v: 1, tier: 'pro', features: [] },
      signedToken: 'TOKEN.SIG',
    })) as unknown as typeof fetch;
    const { c } = clientWith(fetchImpl);
    expect(await c.activate('AD-XXXX')).toMatchObject({
      licenseKind: 'lifetime',
      entitlementStage: 'provisional',
      licenseIssuedAt: '2026-07-01T00:00:00.000Z',
      confirmationDueAt: '2026-07-31T00:00:00.000Z',
    });
  });
  it('maps a non-active response to invalid', async () => {
    const fetchImpl = (async () => jsonResponse(200, { ok: false, status: 'invalid', message: 'nope' })) as unknown as typeof fetch;
    const { c } = clientWith(fetchImpl);
    expect((await c.activate('BAD')).status).toBe('invalid');
  });
  it('reports an unstructured 404 without inventing server availability', async () => {
    const fetchImpl = (async () => jsonResponse(404, {})) as unknown as typeof fetch;
    const { c } = clientWith(fetchImpl);
    const lic = await c.activate('X');
    expect(lic.status).toBe('failed');
    expect(lic.message).toMatch(/HTTP 404/);
    expect(lic.message).not.toMatch(/not live/i);
  });
  it.each([
    [404, 'not_found', 'License not found or inactive'],
    [409, 'seat_limit', 'All device seats are in use'],
  ])('preserves structured HTTP %s rejection', async (status, rejection, message) => {
    const { c } = clientWith((async () => jsonResponse(status, { ok: false, status: rejection, message })) as typeof fetch);
    expect(await c.activate('BAD')).toMatchObject({ status: 'invalid', message });
  });
  it('never grants activation from an active body on an error response', async () => {
    const { c } = clientWith((async () => jsonResponse(404, { ok: false, status: 'active', activationToken: 'untrusted' })) as typeof fetch);
    expect((await c.activate('BAD')).status).toBe('failed');
  });
  it('handles a proxy HTML error without calling it a rejected key', async () => {
    const { c } = clientWith((async () => new Response('<html>not found</html>', { status: 404 })) as typeof fetch);
    expect((await c.activate('BAD')).status).toBe('failed');
  });

});

describe('refresh (server authority)', () => {
  it('drops to invalid when the server revokes', async () => {
    const storage = memStorage();
    storage.value = { status: 'active', deviceId: 'd', activationToken: 'tok', validUntil: null, entitlements: { v: 1, tier: 'pro', features: [] } };
    const fetchImpl = (async () => jsonResponse(200, { ok: false, status: 'invalid', message: 'revoked' })) as unknown as typeof fetch;
    const c = createLicenseClient({ app: 'x', host: 'x.cc', appVersion: '0', storage, deviceId: () => 'd', fetchImpl });
    expect((await c.refresh())?.status).toBe('invalid');
  });
  it('keeps the cached license when the server is unreachable (offline grace)', async () => {
    const storage = memStorage();
    storage.value = { status: 'active', deviceId: 'd', activationToken: 'tok', validUntil: null };
    const fetchImpl = (async () => { throw new Error('network down'); }) as unknown as typeof fetch;
    const c = createLicenseClient({ app: 'x', host: 'x.cc', appVersion: '0', storage, deviceId: () => 'd', fetchImpl });
    expect((await c.refresh())?.status).toBe('active');
  });
  it('sends deviceId on refresh + adopts a freshly re-minted signedToken', async () => {
    const storage = memStorage();
    storage.value = { status: 'active', deviceId: 'dev-1', activationToken: 'tok', validUntil: null, signedToken: 'OLD.SIG' };
    let sentBody: Record<string, unknown> = {};
    const fetchImpl = (async (_url: string, init: RequestInit) => {
      sentBody = JSON.parse(init.body as string) as Record<string, unknown>;
      return jsonResponse(200, { ok: true, status: 'active', validUntil: null, entitlements: { v: 1, tier: 'pro', features: [] }, signedToken: 'NEW.SIG' });
    }) as unknown as typeof fetch;
    const c = createLicenseClient({ app: 'x', host: 'x.cc', appVersion: '0', storage, deviceId: () => 'dev-1', fetchImpl });
    const lic = await c.refresh();
    expect(sentBody.deviceId).toBe('dev-1');
    expect(lic?.signedToken).toBe('NEW.SIG');
  });
  it.each([404, 503])('distinguishes authoritative revocation from HTTP %s outage', async (status) => {
    const storage = memStorage();
    storage.value = { status: 'active', deviceId: 'd', activationToken: 'tok' };
    const { c } = clientWith((async () => jsonResponse(status, { ok: false, status: 'not_found', message: 'License inactive' })) as typeof fetch, storage);
    expect((await c.refresh())?.status).toBe(status === 404 ? 'invalid' : 'active');
  });

});

describe('isLicensed — per-kind grace (riskiest assumption)', () => {
  const day = 86_400_000;
  const perpetual: StoredLicense = { status: 'active', deviceId: 'd', validUntil: null };
  const sub = (untilIso: string): StoredLicense => ({ status: 'active', deviceId: 'd', validUntil: untilIso });
  it('perpetual stays licensed indefinitely (never bricks offline)', () => {
    expect(isLicensed(perpetual, 7, Date.now() + 100 * 365 * day)).toBe(true);
  });
  it('subscription within grace is licensed', () => {
    expect(isLicensed(sub(new Date(Date.now() - 2 * day).toISOString()), 7)).toBe(true);
  });
  it('subscription past grace lapses', () => {
    expect(isLicensed(sub(new Date(Date.now() - 10 * day).toISOString()), 7)).toBe(false);
  });
  it('does not apply offline grace past a signed trial or provisional refund-check boundary', () => {
    const expired = new Date(Date.now() - 1).toISOString();
    expect(isLicensed({
      status: 'active', deviceId: 'd', validUntil: expired,
      licenseKind: 'trial', entitlementStage: 'time_bound',
    }, 7)).toBe(false);
    expect(isLicensed({
      status: 'active', deviceId: 'd', validUntil: expired,
      licenseKind: 'lifetime', entitlementStage: 'provisional',
    }, 7)).toBe(false);
  });
  it('expires trial and provisional UI state at the exact signed boundary', () => {
    const boundary = Date.now();
    const validUntil = new Date(boundary).toISOString();
    expect(isLicensed({
      status: 'active', deviceId: 'd', validUntil,
      licenseKind: 'trial', entitlementStage: 'time_bound',
    }, 7, boundary)).toBe(false);
    expect(isLicensed({
      status: 'active', deviceId: 'd', validUntil,
      licenseKind: 'lifetime', entitlementStage: 'provisional',
    }, 7, boundary)).toBe(false);
  });
  it('non-active / null is never licensed', () => {
    expect(isLicensed({ status: 'invalid', deviceId: 'd', validUntil: null }, 7)).toBe(false);
    expect(isLicensed(null, 7)).toBe(false);
  });
  it('rejects an unparsable non-null validUntil for every kind, keeping null perpetual', () => {
    for (const patch of [
      {},
      { licenseKind: 'term', entitlementStage: 'time_bound' },
      { licenseKind: 'trial', entitlementStage: 'time_bound' },
      { licenseKind: 'lifetime', entitlementStage: 'provisional' },
      { licenseKind: 'lifetime', entitlementStage: 'final' },
    ] as const) {
      expect(isLicensed({ status: 'active', deviceId: 'd', validUntil: 'not-a-date', ...patch }, 7)).toBe(false);
      expect(isLicensed({ status: 'active', deviceId: 'd', validUntil: '', ...patch }, 7)).toBe(false);
      expect(isLicensed({ status: 'active', deviceId: 'd', validUntil: null, ...patch }, 7)).toBe(true);
    }
  });
  it('keeps plus below pro (no plus-to-pro normalization)', () => {
    const plus: StoredLicense = { status: 'active', deviceId: 'd', validUntil: null, entitlements: { v: 1, tier: 'plus', features: [] } };
    expect(isPro(plus)).toBe(false);
    expect(tierAtLeast(plus, 'pro')).toBe(false);
    expect(tierAtLeast(plus, 'plus')).toBe(true);
    expect(satisfiesRequirement(plus, { tier: 'pro' })).toBe(false);
  });
});

describe('entitlement readers', () => {
  const lic: StoredLicense = { status: 'active', deviceId: 'd', entitlements: { v: 1, tier: 'pro', features: ['a', 'b'] } };
  it('tier + feature helpers', () => {
    expect(tier(lic)).toBe('pro');
    expect(isPro(lic)).toBe(true);
    expect(tierAtLeast(lic, 'plus')).toBe(true);
    expect(hasFeature(lic, 'a')).toBe(true);
    expect(hasAnyFeature(lic, ['z', 'b'])).toBe(true);
    expect(hasFeature(lic, 'z')).toBe(false);
    expect(tier(null)).toBeNull();
  });
  it('upgrades a provisional lifetime record to final after the one mandatory check', async () => {
    const storage = memStorage();
    storage.value = {
      status: 'active', deviceId: 'dev-1', activationToken: 'tok',
      licenseKind: 'lifetime', entitlementStage: 'provisional',
      validUntil: '2026-07-31T00:00:00.000Z', signedToken: 'PROVISIONAL.SIG',
    };
    const fetchImpl = (async () => jsonResponse(200, {
      ok: true, status: 'active', validUntil: null,
      licenseKind: 'lifetime', entitlementStage: 'final',
      licenseIssuedAt: '2026-07-01T00:00:00.000Z',
      confirmationDueAt: '2026-07-31T00:00:00.000Z',
      signedToken: 'FINAL.SIG',
    })) as unknown as typeof fetch;
    const c = createLicenseClient({ app: 'x', host: 'x.cc', appVersion: '0', storage, deviceId: () => 'dev-1', fetchImpl });
    expect(await c.refresh()).toMatchObject({
      entitlementStage: 'final', validUntil: null, signedToken: 'FINAL.SIG',
    });
  });

  it('satisfies shared UI-side requirements only for licensed users', () => {
    expect(satisfiesRequirement(lic, { tier: 'pro', allFeatures: ['a', 'b'] })).toBe(true);
    expect(satisfiesRequirement(lic, { feature: 'z' })).toBe(false);
    expect(satisfiesRequirement({ ...lic, status: 'invalid' }, { tier: 'pro' })).toBe(false);
  });
});
