import { describe, expect, it, vi } from 'vitest';
import { createLicenseClient, LicenseClientError, licenseMessage } from './index.js';
import type { LicenseInvoke, LicenseListen } from './index.js';
import { createLicenseMock } from './testing.js';
const sentinel = 'SECRET_RENDERER_REDACTION_SENTINEL';
const fixture = async () => (await createLicenseMock().client.activate('test-key'));
const listen: LicenseListen = async () => () => {};
function reply(value: unknown): LicenseInvoke { return async <T>() => value as T; }

describe('native invoke client', () => {
  it('uses frozen commands & input names only', async () => {
    const mock = createLicenseMock();
    const spy = vi.fn(mock.bridge.invoke);
    const client = createLicenseClient({ invoke: spy as LicenseInvoke, listen: mock.bridge.listen });
    await client.status(); await client.activate('key'); await client.refresh();
    await client.requestTrial('qa@example.test'); await client.deactivate();
    expect(spy.mock.calls).toEqual([
      ['rightkit_license_status', undefined], ['rightkit_license_activate', { key: 'key' }],
      ['rightkit_license_refresh', undefined], ['rightkit_license_request_trial', { email: 'qa@example.test' }],
      ['rightkit_license_deactivate', undefined],
    ]);
  });
  it('projects safe response fields without secret-bearing extras', async () => {
    const view = await fixture();
    const client = createLicenseClient({ invoke: reply({ ...view, activationToken: sentinel, arbitrary: sentinel }), listen });
    expect(await client.status()).toEqual(view);
    expect(JSON.stringify(await client.status())).not.toContain(sentinel);
  });
  it.each(['notNative', null, 1])('rejects malformed lifecycle %s', async lifecycle => {
    const client = createLicenseClient({ invoke: reply({ ...await fixture(), lifecycle }), listen });
    await expect(client.status()).rejects.toMatchObject({ code: 'unknown', message: 'Something went wrong.' });
  });
  it.each([NaN, Infinity, 1.5, Number.MAX_SAFE_INTEGER])('rejects invalid timestamp %s', async validUntil => {
    const client = createLicenseClient({ invoke: reply({ ...await fixture(), validUntil }), listen });
    await expect(client.status()).rejects.toBeInstanceOf(LicenseClientError);
  });
  it.each([sentinel, new Error(sentinel), { code: sentinel, message: sentinel }, { code: 'network', message: sentinel, body: sentinel }])('redacts unknown rejection', async error => {
    const invoke: LicenseInvoke = async () => { throw error; };
    try { await createLicenseClient({ invoke, listen }).status(); expect.unreachable(); }
    catch (caught) {
      expect(caught).toBeInstanceOf(LicenseClientError);
      expect(String(caught)).toBe('LicenseClientError: Something went wrong.');
      expect(JSON.stringify(caught)).not.toContain(sentinel);
      expect((caught as Error).cause).toBeUndefined();
    }
  });
  it('maps known codes to fixed text & discards unknown message codes', async () => {
    expect(licenseMessage('busy')).toContain('Another licence operation');
    expect(licenseMessage(sentinel)).toBe('Something went wrong.');
    const invoke: LicenseInvoke = async () => { throw { code: 'rejected', reason: 'deviceLimit', message: 'deviceLimit', detail: sentinel }; };
    await expect(createLicenseClient({ invoke, listen }).status()).rejects.toMatchObject({ code: 'rejected', reason: 'deviceLimit', message: licenseMessage('deviceLimit') });
  });
  it('projects trial & refresh results', async () => {
    const view = await fixture();
    const invoke: LicenseInvoke = async <T>(command: string) => (command === 'rightkit_license_refresh'
      ? { view: { ...view, secret: sentinel }, outcome: 'stale', secret: sentinel }
      : { accepted: false, message: sentinel, secret: sentinel }) as T;
    const client = createLicenseClient({ invoke, listen });
    expect(await client.refresh()).toEqual({ view, outcome: 'stale' });
    expect(await client.requestTrial('qa@example.test')).toEqual({ accepted: false, message: 'unknown' });
  });
  it('cleans up once after malformed event before async registration resolves', async () => {
    const release = vi.fn(); const onError = vi.fn(); const callback = vi.fn();
    const synchronous: LicenseListen = async <T>(_event: string, cb: (event: { payload: T }) => void) => {
      cb({ payload: { view: { lifecycle: sentinel } } as T }); return release;
    };
    const stop = await createLicenseClient({ invoke: reply(null), listen: synchronous }).onChange(callback, onError);
    stop(); expect(release).toHaveBeenCalledTimes(1); expect(callback).not.toHaveBeenCalled();
    expect(onError.mock.calls[0][0].message).toBe('Something went wrong.');
  });
  it('projects events & returns idempotent cleanup', async () => {
    const mock = createLicenseMock(); const callback = vi.fn();
    const stop = await mock.client.onChange(callback);
    await mock.client.activate('key'); expect(callback).toHaveBeenCalledTimes(1);
    stop(); stop(); await mock.client.deactivate(); expect(callback).toHaveBeenCalledTimes(1);
  });
});
