import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, fetchReleaseStatus, type RuntimeUpdate } from './index.js';

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
    const client = createAutoUpdateClient({
      check: async () => ({
        ...update(),
        rawJson: tier === undefined ? {} : { tier },
        downloadAndInstall: install,
      }),
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'error' });
    await client.installAvailable();
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
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
      getTier: () => 'update',
    });

    await expect(client.checkOnce()).resolves.toMatchObject({
      kind: 'available',
      tier: 'update',
      eligible: false,
      reason: 'pro_required',
    });
    await client.installAvailable();
    expect(install).not.toHaveBeenCalled();
  });

  it('defers patch install when the host says the app is busy', async () => {
    const install = vi.fn();
    const client = createAutoUpdateClient({
      check: async () => ({ ...update(), downloadAndInstall: install }),
      getTier: () => 'patch',
      canInstall: () => false,
      deferReason: () => 'recording',
      shouldAutoInstall: () => true,
    });

    await expect(client.checkOnce()).resolves.toMatchObject({ kind: 'deferred', reason: 'recording' });
    expect(install).not.toHaveBeenCalled();
  });

  it('passes license headers through check and install for gated updates when enabled', async () => {
    const check = vi.fn(async () => update());
    let installOptions: unknown;
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
      getHeaders: () => ({ Authorization: 'Bearer token' }),
      getTier: () => 'update',
      isEligible: (tier, context) => tier === 'update' && Boolean(context.headers),
      shouldAutoInstall: (tier) => tier === 'update',
    });

    await client.checkOnce();
    expect(check).toHaveBeenCalledWith({ headers: { Authorization: 'Bearer token' } });
    expect(installOptions).toEqual({ headers: { Authorization: 'Bearer token' } });
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
