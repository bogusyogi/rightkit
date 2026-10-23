import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';
import { createNativeUpdateClient, NativeUpdateClientError, UPDATE_MESSAGES } from './index.js';
import type { NativeUpdateTransport, Unlisten } from './index.js';

const check = { available: true, kind: 'update', version: '2.0.0', tier: 'pro', eligible: true, refusal: 'none', handleId: 'native_123', checkedAt: 1_700_000_000_000, fresh: true };
function fixture(responses: Record<string, unknown> = {}) {
  let callback: ((event: { payload: unknown }) => void) | undefined;
  const unlisten = vi.fn();
  const invoke = vi.fn(async (command: string, _args?: Record<string, unknown>) => responses[command]);
  const listen = vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => { callback = handler; return unlisten; });
  const transport: NativeUpdateTransport = {
    invoke: async <T>(command: string, args?: Record<string, unknown>) => await invoke(command, args) as T,
    listen: async <T>(event: string, handler: (event: { payload: T }) => void) => listen(event, handler as (event: { payload: unknown }) => void),
  };
  return { client: createNativeUpdateClient(transport), invoke, listen, unlisten, emit: (payload: unknown) => callback?.({ payload }) };
}

describe('native updater renderer bridge', () => {
  it('uses frozen commands and only a validated opaque handle argument', async () => {
    const f = fixture({
      rightkit_update_check: check,
      rightkit_update_status: { state: 'downloaded', handleId: 'native_123', kind: 'update', version: '2.0.0', message: 'none' },
      rightkit_update_download: { handleId: 'native_123', state: 'downloaded', message: 'none' },
      rightkit_update_install: { state: 'installing', message: 'none' },
      rightkit_update_cancel: { cancelled: true },
    });
    await f.client.check(); await f.client.status(); await f.client.download('native_123');
    await f.client.install('native_123'); await f.client.cancel('native_123');
    expect(f.invoke.mock.calls).toEqual([
      ['rightkit_update_check', undefined], ['rightkit_update_status', undefined],
      ['rightkit_update_download', { handleId: 'native_123' }],
      ['rightkit_update_install', { handleId: 'native_123' }], ['rightkit_update_cancel', { handleId: 'native_123' }],
    ]);
    for (const bad of ['', '../secret', 'a'.repeat(257), 'native handle', 'https://private']) {
      await expect(f.client.download(bad)).rejects.toBeInstanceOf(NativeUpdateClientError);
      await expect(f.client.install(bad)).rejects.toBeInstanceOf(NativeUpdateClientError);
      await expect(f.client.cancel(bad)).rejects.toBeInstanceOf(NativeUpdateClientError);
    }
    expect(f.invoke).toHaveBeenCalledTimes(5);
  });

  it('projects DTO fields without transferring extra native material or cached authority', async () => {
    const extras = { headers: { Authorization: 'secret' }, url: 'secret', signature: 'secret', path: 'secret', token: 'secret', cached: check };
    const f = fixture({
      rightkit_update_check: { ...check, ...extras },
      rightkit_update_status: { state: 'available', message: 'none', ...extras },
      rightkit_update_download: { handleId: 'native_123', state: 'refused', reason: 'expired', message: 'updateRefused', ...extras },
      rightkit_update_install: { state: 'refused', reason: 'stale', message: 'updateRefused', ...extras },
      rightkit_update_cancel: { cancelled: false, ...extras },
    });
    expect(await f.client.check()).toEqual(check);
    expect(await f.client.status()).toEqual({ state: 'available', message: 'none' });
    expect(await f.client.download('native_123')).toEqual({ handleId: 'native_123', state: 'refused', reason: 'expired', message: 'updateRefused' });
    expect(await f.client.install('native_123')).toEqual({ state: 'refused', reason: 'stale', message: 'updateRefused' });
    expect(await f.client.cancel('native_123')).toEqual({ cancelled: false });
  });

  it('preserves native tier, eligibility, refusal and patch/update distinction', async () => {
    const f = fixture({ rightkit_update_check: { ...check, kind: 'patch', tier: 'free', eligible: false, refusal: 'busy', handleId: undefined } });
    const result = await f.client.check();
    expect(result).toMatchObject({ kind: 'patch', tier: 'free', eligible: false, refusal: 'busy', checkedAt: check.checkedAt });
    expect(result).not.toHaveProperty('handleId');
  });

  it('rejects malformed fields and closed enums with one fixed safe error', async () => {
    const malformed = [null, [], { ...check, available: 1 }, { ...check, eligible: 'yes' },
      { ...check, kind: 'private' }, { ...check, refusal: 'expired' }, { ...check, refusal: 'please-use-secret' }, { ...check, checkedAt: NaN },
      { ...check, checkedAt: Infinity }, { ...check, checkedAt: -1 }, { ...check, checkedAt: 0.5 },
      { ...check, checkedAt: Number.MAX_SAFE_INTEGER + 1 }, { ...check, fresh: false },
      { ...check, eligible: false }, { ...check, available: false },
      { ...check, notesSummary: 'a'.repeat(8193) }, { ...check, tier: '\u0000secret' }];
    for (const value of malformed) {
      const f = fixture({ rightkit_update_check: value });
      await expect(f.client.check()).rejects.toMatchObject({ code: 'unknown', messageCode: 'unknown', message: 'Something went wrong.' });
    }
    for (const value of [{ state: 'surprise', message: 'none' }, { state: 'downloading', message: 'none', received: -1 },
      { state: 'downloading', message: 'none', received: 11, total: 10 }]) {
      await expect(fixture({ rightkit_update_status: value }).client.status()).rejects.toBeInstanceOf(NativeUpdateClientError);
    }
    expect(await fixture({ rightkit_update_status: { state: 'failed', message: 'native-secret' } }).client.status())
      .toEqual({ state: 'failed', message: 'unknown' });
  });

  it('replaces native rejection text, causes and unknown codes with stable typed errors', async () => {
    for (const error of ['secret', new Error('secret'), { code: 'native-secret', message: 'secret', cause: 'secret' }]) {
      const client = createNativeUpdateClient({ invoke: async () => { throw error; }, listen: async () => () => {} });
      try { await client.check(); throw new Error('expected rejection'); }
      catch (caught) {
        expect(caught).toBeInstanceOf(NativeUpdateClientError);
        expect(caught).toMatchObject({ code: 'unknown', messageCode: 'unknown', message: 'Something went wrong.' });
        expect(caught).not.toHaveProperty('cause');
        expect(JSON.stringify(caught)).not.toContain('secret');
      }
    }
    const client = createNativeUpdateClient({ invoke: async () => { throw { code: 'forbidden', message: 'forbidden', cause: 'secret' }; }, listen: async () => () => {} });
    await expect(client.check()).rejects.toMatchObject({ code: 'forbidden', messageCode: 'forbidden', message: 'This window cannot manage updates.' });
    // The licence-flavoured codes of the previous contract are not part of the native vocabulary any more.
    for (const code of ['rejected', 'clock']) {
      const old = createNativeUpdateClient({ invoke: async () => { throw { code, message: 'expired' }; }, listen: async () => () => {} });
      await expect(old.check()).rejects.toMatchObject({ code: 'unknown', messageCode: 'unknown' });
    }
  });

  it('uses frozen error defaults and replaces unrecognized message codes', async () => {
    for (const [error, expected] of [
      [{ code: 'network' }, { code: 'network', messageCode: 'networkUnavailable' }],
      [{ code: 'store' }, { code: 'store', messageCode: 'storeFailed' }],
      [{ code: 'verify', message: 'native-secret' }, { code: 'verify', messageCode: 'unknown' }],
      [{ code: 'busy', message: 'keyNotAccepted' }, { code: 'busy', messageCode: 'unknown' }],
      // The native host's `unknown` code keeps its message code instead of degrading to the generic text.
      [{ code: 'unknown', message: 'updateFailed' }, { code: 'unknown', messageCode: 'updateFailed' }],
      [{ code: 'unknown' }, { code: 'unknown', messageCode: 'unknown' }],
    ]) {
      const client = createNativeUpdateClient({ invoke: async () => { throw error; }, listen: async () => () => {} });
      await expect(client.check()).rejects.toMatchObject(expected);
    }
  });

  it('passes only valid projected progress and does not synthesize state from events', async () => {
    const f = fixture({ rightkit_update_status: { state: 'idle', message: 'none' } });
    const received = vi.fn();
    const stop = await f.client.onProgress(received);
    expect(f.listen.mock.calls[0][0]).toBe('rightkit://update-progress');
    f.emit({ handleId: 'native_123', received: 5, total: 10, headers: 'secret' });
    f.emit({ handleId: 'native_123', received: NaN });
    f.emit({ handleId: 'native_123', received: 20, total: 10 });
    f.emit({ handleId: '../secret', received: 1 });
    expect(received.mock.calls).toEqual([[{ handleId: 'native_123', received: 5, total: 10 }]]);
    expect(await f.client.status()).toEqual({ state: 'idle', message: 'none' });
    stop(); stop(); f.emit({ handleId: 'native_123', received: 6 });
    expect(f.unlisten).toHaveBeenCalledTimes(1);
    expect(received).toHaveBeenCalledTimes(1);
  });

  it('does not swallow or invoke twice a consumer callback that throws', async () => {
    const f = fixture();
    const callback = vi.fn(() => { throw new Error('consumer'); });
    await f.client.onProgress(callback);
    expect(() => f.emit({ handleId: 'native_123', received: 1 })).toThrow('consumer');
    expect(callback).toHaveBeenCalledTimes(1);
    f.client.dispose();
  });

  it('cleans late listener registration when disposed before listen resolves', async () => {
    let finish!: (stop: Unlisten) => void;
    let emit!: (event: { payload: unknown }) => void;
    const unlisten = vi.fn();
    const client = createNativeUpdateClient({
      invoke: async <T>() => undefined as T,
      listen: <T>(_event: string, callback: (event: { payload: T }) => void) => {
        emit = callback as (event: { payload: unknown }) => void;
        return new Promise<Unlisten>(resolve => { finish = resolve; });
      },
    });
    const callback = vi.fn();
    const registration = client.onProgress(callback);
    client.dispose(); client.dispose();
    emit({ payload: { handleId: 'native_123', received: 1 } });
    finish(unlisten);
    const stop = await registration;
    stop(); client.dispose();
    expect(callback).not.toHaveBeenCalled();
    expect(unlisten).toHaveBeenCalledTimes(1);
    await expect(client.onProgress(callback)).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('sanitizes asynchronous registration failures', async () => {
    const client = createNativeUpdateClient({ invoke: async <T>() => undefined as T, listen: async () => { throw new Error('native-secret'); } });
    await expect(client.onProgress(() => {})).rejects.toMatchObject({ code: 'unknown', message: 'Something went wrong.' });
    client.dispose();
  });

  it('reads current native status after reconnect and every subsequent request', async () => {
    let state = { state: 'downloaded', handleId: 'native_live', message: 'none' };
    const invoke = vi.fn(async () => state);
    const transport: NativeUpdateTransport = { invoke: async <T>() => await invoke() as T, listen: async () => () => {} };
    const previous = createNativeUpdateClient(transport);
    expect(await previous.status()).toEqual(state);
    previous.dispose();
    state = { state: 'idle', handleId: 'native_new', message: 'none' };
    const reconnected = createNativeUpdateClient(transport);
    expect(await reconnected.status()).toEqual(state);
    state = { state: 'failed', handleId: 'native_new', message: 'updateFailed' };
    expect(await reconnected.status()).toEqual(state);
    expect(invoke).toHaveBeenCalledTimes(3);
  });
});

describe('UPDATE_MESSAGES', () => {
  const nativeKeys = (): string[] | undefined => {
    const path = new URL('../../../../crates/rightkit-tauri-shell/src/update.rs', import.meta.url);
    if (!existsSync(path)) return undefined;
    const source = readFileSync(path, 'utf8');
    const body = /pub enum UpdateMessage \{([^}]*)\}/u.exec(source)?.[1];
    if (body === undefined) throw new Error('UpdateMessage enum not found in update.rs');
    return [...body.matchAll(/^\s*([A-Z][A-Za-z]*),/gmu)].map(m => m[1]![0]!.toLowerCase() + m[1]!.slice(1));
  };

  it('has exactly the keys the native update host sends', () => {
    const native = nativeKeys();
    if (native === undefined) return;
    expect(native.length).toBeGreaterThan(0);
    expect(Object.keys(UPDATE_MESSAGES).sort()).toEqual([...native].sort());
  });

  it("is frozen and names no licence, key, account, trial or email", () => {
    expect(Object.isFrozen(UPDATE_MESSAGES)).toBe(true);
    for (const text of Object.values(UPDATE_MESSAGES)) expect(text).not.toMatch(/licen[cs]e|\bkeys?\b|account|trial|email/iu);
    expect(UPDATE_MESSAGES.none).toBe('');
  });

  it('gives every code except none a non-empty text', () => {
    for (const [code, text] of Object.entries(UPDATE_MESSAGES)) if (code !== 'none') expect(text.length).toBeGreaterThan(0);
  });
});

describe('native release status bridge', () => {
  const status = { schema: 1, appKey: 'viewright', platform: 'darwin', patchVersion: null, updateVersion: '2.0.0' };

  it('invokes rightkit_update_release_status with no arguments and returns a validated status', async () => {
    const f = fixture({ rightkit_update_release_status: status });
    await expect(f.client.releaseStatus()).resolves.toEqual(status);
    expect(f.invoke.mock.calls).toEqual([['rightkit_update_release_status', undefined]]);
  });

  it('rejects an off-schema result with a safe native error', async () => {
    for (const bad of [
      { ...status, schema: 2 },
      { ...status, extra: 'https://private.example' },
      { ...status, appKey: '' },
      { ...status, updateVersion: 3 },
      [status],
      null,
    ]) {
      const f = fixture({ rightkit_update_release_status: bad });
      const error = await f.client.releaseStatus().catch((e: unknown) => e);
      expect(error).toBeInstanceOf(NativeUpdateClientError);
      expect((error as Error).message).not.toMatch(/private\.example/u);
    }
  });
});
