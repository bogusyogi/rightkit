import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type RuntimeUpdate } from './index.js';

function update(version = '1.2.3'): RuntimeUpdate {
  return {
    version,
    async downloadAndInstall(onEvent) {
      onEvent?.({ event: 'Started', data: { contentLength: 100 } });
      onEvent?.({ event: 'Progress', data: { chunkLength: 40 } });
      onEvent?.({ event: 'Progress', data: { chunkLength: 60 } });
      onEvent?.({ event: 'Finished' });
    },
  };
}

describe('createAutoUpdateClient', () => {
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
