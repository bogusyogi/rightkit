import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type RuntimeUpdate } from './index.js';

function patch(download = vi.fn(async () => {})): RuntimeUpdate & { download: typeof download } {
  return { version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: download, download } as never;
}

describe('relaunchAfterInstall', () => {
  it('does not relaunch by default, even with a relaunch callback', async () => {
    const relaunch = vi.fn();
    const client = createAutoUpdateClient({ check: async () => patch(), relaunch });
    await client.checkOnce();
    expect((await client.installAvailable()).kind).toBe('ready');
    expect(relaunch).not.toHaveBeenCalled();
  });
  it('relaunches after ready when enabled', async () => {
    const order: string[] = [];
    const relaunch = vi.fn(() => { order.push('relaunch'); });
    const client = createAutoUpdateClient({
      check: async () => patch(),
      relaunch,
      relaunchAfterInstall: true,
      onStatus: (s) => order.push(s.kind),
    });
    await client.checkOnce();
    await client.installAvailable();
    expect(order.slice(-2)).toEqual(['ready', 'relaunch']);
  });
  it('does not relaunch when the install fails', async () => {
    const relaunch = vi.fn();
    const client = createAutoUpdateClient({
      check: async () => patch(vi.fn(async () => { throw new Error('disk'); })),
      relaunch,
      relaunchAfterInstall: true,
    });
    await client.checkOnce();
    expect((await client.installAvailable()).kind).toBe('error');
    expect(relaunch).not.toHaveBeenCalled();
  });
  it('reports a relaunch failure as an error status', async () => {
    const onError = vi.fn();
    const client = createAutoUpdateClient({
      check: async () => patch(),
      relaunch: () => { throw new Error('no relaunch'); },
      relaunchAfterInstall: true,
      onError,
    });
    await client.checkOnce();
    expect((await client.installAvailable()).kind).toBe('error');
    expect(onError).toHaveBeenCalled();
  });
});
