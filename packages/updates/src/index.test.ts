import { describe, expect, it, vi } from 'vitest';
import {
  UPDATE_HOST_CONTRACT_VECTORS,
  createAutoUpdateClient,
  fetchReleaseStatus,
  type RuntimeUpdate,
} from './index.js';

function update(version = '1.2.3'): RuntimeUpdate {
  return {
    version,
    rawJson: { tier: 'patch' },
    async downloadAndInstall(onEvent) {
      onEvent?.({ event: 'Started', data: { contentLength: 100 } });
      onEvent?.({ event: 'Progress', data: { chunkLength: 40 } });
      onEvent?.({ event: 'Progress', data: { chunkLength: 60 } });
      onEvent?.({ event: 'Finished' });
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe('shared host-adapter contract vectors', () => {
  it('locks the public patch and Pro feature-update outcomes for every app adapter', () => {
    expect(UPDATE_HOST_CONTRACT_VECTORS).toEqual([
      { name: 'free patch', tier: 'patch', pro: false, eligible: true, reason: null },
      { name: 'pro patch', tier: 'patch', pro: true, eligible: true, reason: null },
      { name: 'free feature update', tier: 'update', pro: false, eligible: false, reason: 'pro_required' },
      { name: 'pro feature update', tier: 'update', pro: true, eligible: true, reason: null },
    ]);
  });
});

describe('createAutoUpdateClient', () => {
  it('surfaces a Pro-update nudge when Tauri has no eligible payload', async () => {
    const client = createAutoUpdateClient({
      check: async () => null,
      getProUpdateVersion: async () => '2.0.0',
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      version: '2.0.0',
      tier: 'update',
      eligible: false,
      reason: 'pro_required',
    });
  });

  it('classifies the exact Tauri manifest without a second tier probe', async () => {
    const client = createAutoUpdateClient({
      check: async () => update(),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      tier: 'patch',
      eligible: true,
    });
  });

  it('gates a feature update from the exact Tauri manifest by default', async () => {
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), rawJson: { tier: 'update' } }),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      tier: 'update',
      eligible: false,
      reason: 'pro_required',
    });
  });

  it.each([undefined, null, 'stable', 1])('fails closed for malformed manifest tier %j', async (tier) => {
    const install = vi.fn();
    const getInstallHeaders = vi.fn(() => ({ Authorization: 'Bearer signed-entitlement' }));
    const client = createAutoUpdateClient({
      check: async () => ({
        ...update(),
        rawJson: tier === undefined ? {} : { tier },
        downloadAndInstall: install,
      }),
      getInstallHeaders,
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'error' });
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'up_to_date' });
    expect(getInstallHeaders).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();
  });

  it('clears a previously available update when a later manifest is malformed', async () => {
    const install = vi.fn();
    let rawJson: Record<string, unknown> = { tier: 'patch' };
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), rawJson, downloadAndInstall: install }),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'available' });
    rawJson = {};
    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'error' });
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'up_to_date' });
    expect(install).not.toHaveBeenCalled();
  });

  it('checks patches without auto-installing by default', async () => {
    const statuses: string[] = [];
    const client = createAutoUpdateClient({
      check: vi.fn(async () => update()),
      getTier: () => 'patch',
      onStatus: (status) => statuses.push(status.kind),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      version: '1.2.3',
      eligible: true,
    });
    expect(statuses).toEqual(['checking', 'available']);
  });

  it('manually installs an eligible available patch', async () => {
    const statuses: string[] = [];
    const client = createAutoUpdateClient({
      check: vi.fn(async () => update()),
      getTier: () => 'patch',
      onStatus: (status) => statuses.push(status.kind),
    });

    await client.checkOnce();
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'ready', version: '1.2.3' });
    expect(statuses).toEqual(['checking', 'available', 'downloading', 'downloading', 'downloading', 'ready']);
  });

  it('auto-installs patches when opt-in policy allows it', async () => {
    const statuses: string[] = [];
    const client = createAutoUpdateClient({
      check: vi.fn(async () => update()),
      getTier: () => 'patch',
      shouldAutoInstall: () => true,
      onStatus: (status) => statuses.push(status.kind),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'ready', version: '1.2.3' });
    expect(statuses).toEqual(['checking', 'available', 'downloading', 'downloading', 'downloading', 'ready']);
  });

  it('does not install paid updates for ineligible users', async () => {
    const install = vi.fn();
    const getInstallHeaders = vi.fn(() => ({ Authorization: 'Bearer signed-entitlement' }));
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
      getTier: () => 'update',
      getInstallHeaders,
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      tier: 'update',
      eligible: false,
      reason: 'pro_required',
    });
    await expect(client.installAvailable()).resolves.toMatchObject({
      kind: 'available',
      tier: 'update',
      eligible: false,
      reason: 'pro_required',
    });
    expect(getInstallHeaders).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();
  });

  it('defers patch install when the host says the app is busy', async () => {
    const install = vi.fn();
    let safe = false;
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
      getTier: () => 'patch',
      canInstall: () => safe,
      deferReason: () => 'recording',
      shouldAutoInstall: () => true,
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'deferred', reason: 'recording' });
    expect(install).not.toHaveBeenCalled();
    safe = true;
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'ready' });
    expect(install).toHaveBeenCalledTimes(1);
  });

  it('resolves a fresh signed entitlement only when installing', async () => {
    const check = vi.fn(async () => update());
    let installOptions: unknown;
    let signedEntitlement = 'stale-entitlement';
    const getInstallHeaders = vi.fn(() => ({ Authorization: `Bearer ${signedEntitlement}` }));
    const client = createAutoUpdateClient({
      check: async (options) => {
        check(options);
        return {
          version: '2.0.0',
          async downloadAndInstall(_onEvent, options) {
            installOptions = options;
          },
        };
      },
      getHeaders: () => ({ Authorization: 'Bearer activation-token' }),
      getInstallHeaders,
      getTier: () => 'update',
      isEligible: (tier, context) => tier === 'update' && Boolean(context.headers),
    });

    await client.checkOnce();
    expect(getInstallHeaders).not.toHaveBeenCalled();
    signedEntitlement = 'fresh-entitlement';
    await client.installAvailable();
    expect(check).toHaveBeenCalledWith({ headers: { Authorization: 'Bearer activation-token' } });
    expect(getInstallHeaders).toHaveBeenCalledWith('update', {
      checkHeaders: { Authorization: 'Bearer activation-token' },
    });
    expect(installOptions).toEqual({ headers: { Authorization: 'Bearer fresh-entitlement' } });
  });

  it('retains check headers for install when no install-header callback is configured', async () => {
    let installOptions: unknown;
    const client = createAutoUpdateClient({
      check: async () => ({
        ...update(),
        async downloadAndInstall(_onEvent, options) {
          installOptions = options;
        },
      }),
      getHeaders: () => ({ Authorization: 'Bearer activation-token' }),
    });

    await client.checkOnce();
    await client.installAvailable();
    expect(installOptions).toEqual({ headers: { Authorization: 'Bearer activation-token' } });
  });

  it('allows a public patch to install without artifact headers', async () => {
    let installOptions: unknown = 'not-called';
    const client = createAutoUpdateClient({
      check: async () => ({
        ...update(),
        async downloadAndInstall(_onEvent, options) {
          installOptions = options;
        },
      }),
      getHeaders: () => ({ Authorization: 'Bearer activation-token' }),
      getInstallHeaders: () => undefined,
    });

    await client.checkOnce();
    await client.installAvailable();
    expect(installOptions).toBeUndefined();
  });

  it('fails closed when install-header resolution throws', async () => {
    const install = vi.fn();
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
      getInstallHeaders: () => {
        throw new Error('entitlement unavailable');
      },
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'available' });
    await expect(client.installAvailable()).resolves.toMatchObject({
      kind: 'error',
      message: expect.stringContaining('entitlement unavailable'),
    });
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'up_to_date' });
    expect(install).not.toHaveBeenCalled();
  });

  it('keeps a newer check handle when an older concurrent check finishes last', async () => {
    const firstResult = deferred<RuntimeUpdate | null>();
    const installed: string[] = [];
    const statuses: string[] = [];
    let checks = 0;
    const runtimeUpdate = (version: string): RuntimeUpdate => ({
      ...update(version),
      async downloadAndInstall() {
        installed.push(version);
      },
    });
    const client = createAutoUpdateClient({
      check: async () => {
        checks += 1;
        return checks === 1 ? firstResult.promise : runtimeUpdate('2.0.0');
      },
      onStatus: (status) => {
        if (status.kind === 'available') statuses.push(status.version);
      },
    });

    const olderCheck = client.checkOnce();
    await vi.waitFor(() => expect(checks).toBe(1));
    await expect(client.checkOnce()).resolves.toMatchObject({ version: '2.0.0' });
    firstResult.resolve(runtimeUpdate('1.0.0'));
    await olderCheck;
    await client.installAvailable();

    expect(statuses).toEqual(['2.0.0']);
    expect(installed).toEqual(['2.0.0']);
  });

  it('atomically claims an available update so overlapping installs run once', async () => {
    const downloadFinished = deferred<void>();
    const install = vi.fn(async () => downloadFinished.promise);
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
    });

    await client.checkOnce();
    const firstInstall = client.installAvailable();
    const secondInstall = client.installAvailable();
    await vi.waitFor(() => expect(install).toHaveBeenCalled());
    downloadFinished.resolve();
    const results = await Promise.all([firstInstall, secondInstall]);
    expect(results.filter((result) => result.kind === 'ready')).toHaveLength(1);
    expect(results.filter((result) => result.kind === 'up_to_date')).toHaveLength(1);
    expect(install).toHaveBeenCalledTimes(1);
  });

  it('does not restore a deferred claim over a newer check handle', async () => {
    const firstSafety = deferred<boolean>();
    const installed: string[] = [];
    let checks = 0;
    let safetyChecks = 0;
    const client = createAutoUpdateClient({
      check: async () => {
        checks += 1;
        const version = checks === 1 ? '1.0.0' : '2.0.0';
        return {
          ...update(version),
          async downloadAndInstall() {
            installed.push(version);
          },
        };
      },
      canInstall: async () => {
        safetyChecks += 1;
        return safetyChecks === 1 ? firstSafety.promise : true;
      },
    });

    await client.checkOnce();
    const olderInstall = client.installAvailable();
    await vi.waitFor(() => expect(safetyChecks).toBe(1));
    await client.checkOnce();
    firstSafety.resolve(false);
    await olderInstall;
    await client.installAvailable();

    expect(installed).toEqual(['2.0.0']);
  });

  it('aborts a claimed install when a newer check starts before download', async () => {
    const firstSafety = deferred<boolean>();
    const getInstallHeaders = vi.fn(() => ({ Authorization: 'Bearer signed-entitlement' }));
    const installed: string[] = [];
    let checks = 0;
    let safetyChecks = 0;
    const client = createAutoUpdateClient({
      check: async () => {
        checks += 1;
        const version = checks === 1 ? '1.0.0' : '2.0.0';
        return {
          ...update(version),
          async downloadAndInstall() {
            installed.push(version);
          },
        };
      },
      canInstall: async () => {
        safetyChecks += 1;
        return safetyChecks === 1 ? firstSafety.promise : true;
      },
      getInstallHeaders,
    });

    await client.checkOnce();
    const olderInstall = client.installAvailable();
    await vi.waitFor(() => expect(safetyChecks).toBe(1));
    await client.checkOnce();
    firstSafety.resolve(true);
    await expect(olderInstall).resolves.toMatchObject({ kind: 'idle' });
    await client.installAvailable();

    expect(getInstallHeaders).toHaveBeenCalledTimes(1);
    expect(installed).toEqual(['2.0.0']);
  });

  it('auto-installs with separate check and artifact headers at the install boundary', async () => {
    const order: string[] = [];
    const check = vi.fn(async () => ({
      ...update(),
      async downloadAndInstall(_onEvent: unknown, options: unknown) {
        order.push('download');
        expect(options).toEqual({ headers: { Authorization: 'Bearer signed-entitlement' } });
      },
    }));
    const client = createAutoUpdateClient({
      check,
      getHeaders: () => ({ Authorization: 'Bearer activation-token' }),
      shouldAutoInstall: () => true,
      canInstall: () => {
        order.push('canInstall');
        return true;
      },
      getInstallHeaders: () => {
        order.push('installHeaders');
        return { Authorization: 'Bearer signed-entitlement' };
      },
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'ready' });
    expect(check).toHaveBeenCalledWith({ headers: { Authorization: 'Bearer activation-token' } });
    expect(order).toEqual(['canInstall', 'installHeaders', 'download']);
  });
});

describe('fetchReleaseStatus', () => {
  it('accepts the public version-only RightApps response', async () => {
    const fetchImpl = vi.fn(async () => new Response(JSON.stringify({
      schema: 1,
      appKey: 'viewright',
      platform: 'windows',
      patchVersion: '1.4.2',
      updateVersion: '2.0.0',
    }), { status: 200, headers: { 'content-type': 'application/json' } }));

    await expect(fetchReleaseStatus('https://viewright.cc/v1/apps/viewright/releases/status.json', fetchImpl)).resolves.toEqual({
      schema: 1,
      appKey: 'viewright',
      platform: 'windows',
      patchVersion: '1.4.2',
      updateVersion: '2.0.0',
    });
  });

  it('rejects status responses containing private artifact coordinates', async () => {
    const fetchImpl = vi.fn(async () => new Response(JSON.stringify({
      schema: 1,
      appKey: 'viewright',
      platform: 'windows',
      patchVersion: null,
      updateVersion: '2.0.0',
      url: 'https://private.example/update',
    }), { status: 200, headers: { 'content-type': 'application/json' } }));

    await expect(fetchReleaseStatus('https://example/status.json', fetchImpl)).rejects.toThrow('release status');
  });
});
