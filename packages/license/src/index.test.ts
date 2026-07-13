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
  it('maps a non-active response to invalid', async () => {
    const fetchImpl = (async () => jsonResponse(200, { ok: false, status: 'invalid', message: 'nope' })) as unknown as typeof fetch;
    const { c } = clientWith(fetchImpl);
    expect((await c.activate('BAD')).status).toBe('invalid');
  });
  it('maps a 404 to a "not live" failure', async () => {
    const fetchImpl = (async () => jsonResponse(404, {})) as unknown as typeof fetch;
    const { c } = clientWith(fetchImpl);
    const lic = await c.activate('X');
    expect(lic.status).toBe('failed');
    expect(lic.message).toMatch(/not live/i);
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
  it('non-active / null is never licensed', () => {
    expect(isLicensed({ status: 'invalid', deviceId: 'd', validUntil: null }, 7)).toBe(false);
    expect(isLicensed(null, 7)).toBe(false);
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

  it('satisfies shared UI-side requirements only for licensed users', () => {
    expect(satisfiesRequirement(lic, { tier: 'pro', allFeatures: ['a', 'b'] })).toBe(true);
    expect(satisfiesRequirement(lic, { feature: 'z' })).toBe(false);
    expect(satisfiesRequirement({ ...lic, status: 'invalid' }, { tier: 'pro' })).toBe(false);
  });
});
