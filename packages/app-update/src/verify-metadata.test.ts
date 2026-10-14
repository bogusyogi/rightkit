import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type RuntimeUpdate } from './index.js';

function patch(download = vi.fn(async () => {})): RuntimeUpdate & { download: typeof download } {
  return { version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: download, download } as never;
}

describe('verifyMetadata hook', () => {
  it('rejects the update when the hook returns false', async () => {
    const u = patch();
    const onError = vi.fn();
    const client = createAutoUpdateClient({ check: async () => u, verifyMetadata: () => false, onError });
    const status = await client.checkOnce();
    expect(status.kind).toBe('error');
    expect(onError).toHaveBeenCalled();
    expect((await client.installAvailable()).kind).toBe('up_to_date');
    expect(u.download).not.toHaveBeenCalled();
  });
  it('rejects when the hook throws, passing update and headers', async () => {
    const verify = vi.fn(async () => { throw new Error('bad sig'); });
    const client = createAutoUpdateClient({
      check: async () => patch(),
      getHeaders: () => ({ A: 'b' }),
      verifyMetadata: verify,
    });
    const status = await client.checkOnce();
    expect(status).toMatchObject({ kind: 'error' });
    expect(verify).toHaveBeenCalledWith(expect.objectContaining({ version: '1.2.3' }), { headers: { A: 'b' } });
  });
  it('is a no-op when absent or when it returns true/undefined', async () => {
    for (const verifyMetadata of [undefined, () => true, () => undefined]) {
      const client = createAutoUpdateClient({ check: async () => patch(), verifyMetadata });
      expect((await client.checkOnce()).kind).toBe('available');
    }
  });
});
