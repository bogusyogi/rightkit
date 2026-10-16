import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type RuntimeUpdate } from './index.js';

function patch(download = vi.fn(async () => {})): RuntimeUpdate & { download: typeof download } {
  return { version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: download, download } as never;
}

describe('strictInstallHeaders', () => {
  async function run(extra: object) {
    const u = patch();
    const client = createAutoUpdateClient({
      check: async () => u,
      getHeaders: () => ({ Authorization: 'Bearer manifest' }),
      ...extra,
    });
    await client.checkOnce();
    await client.installAvailable();
    return u.download.mock.calls[0] as unknown as [unknown, { headers?: Record<string, string> } | undefined];
  }
  it('keeps the legacy reuse by default', async () => {
    const call = await run({});
    expect(call[1]).toEqual({ headers: { Authorization: 'Bearer manifest' } });
  });
  it('sends no headers to the artifact when strict', async () => {
    const call = await run({ strictInstallHeaders: true });
    expect(call[1]).toBeUndefined();
  });
  it('an explicit getInstallHeaders still wins when strict', async () => {
    const call = await run({ strictInstallHeaders: true, getInstallHeaders: () => ({ X: '1' }) });
    expect(call[1]).toEqual({ headers: { X: '1' } });
  });
});
