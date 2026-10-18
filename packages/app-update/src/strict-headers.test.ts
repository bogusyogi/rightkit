import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type AutoUpdateClientConfig, type RuntimeUpdate, type UpdateInstallOptions } from './index.js';

describe('strictInstallHeaders with native retained check headers', () => {
  for (const prepared of [false, true]) {
    async function run(extra: Partial<AutoUpdateClientConfig>) {
      let retainedHeaders: Record<string, string> = {};
      let artifactHeaders: Record<string, string> | undefined;
      const download = vi.fn(async (_event?: unknown, options?: UpdateInstallOptions) => {
        // Tauri 2.10.1 clones Update with check headers; only Some(headers) replaces them.
        artifactHeaders = { ...(options?.headers ?? retainedHeaders) };
      });
      const update: RuntimeUpdate = {
        version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: download,
        ...(prepared ? { download, install: vi.fn(async () => {}) } : {}),
      };
      const client = createAutoUpdateClient({
        check: async (options) => { retainedHeaders = { ...options?.headers }; return update; },
        getHeaders: () => ({ Authorization: 'Bearer manifest' }),
        ...extra,
      });
      await client.checkOnce();
      if (prepared) await client.downloadAvailable();
      await client.installAvailable();
      expect(download).toHaveBeenCalledTimes(1);
      return artifactHeaders;
    }
    const adapter = prepared ? 'prepared' : 'combined';
    it(`${adapter}: explicit false preserves deprecated manifest auth reuse`, async () => {
      expect(await run({ strictInstallHeaders: false })).toEqual({ Authorization: 'Bearer manifest' });
    });
    it(`${adapter}: default clears retained manifest authorization`, async () => {
      expect(await run({})).toEqual({});
    });
    it(`${adapter}: explicit strict clears retained manifest authorization`, async () => {
      expect(await run({ strictInstallHeaders: true })).toEqual({});
    });
    it(`${adapter}: undefined callback result clears retained authorization even in legacy mode`, async () => {
      expect(await run({ strictInstallHeaders: false, getInstallHeaders: () => undefined })).toEqual({});
    });
    it(`${adapter}: strict async undefined callback result clears retained authorization`, async () => {
      expect(await run({ strictInstallHeaders: true, getInstallHeaders: async () => undefined })).toEqual({});
    });
    it(`${adapter}: explicit artifact callback replaces retained headers`, async () => {
      expect(await run({ getInstallHeaders: () => ({ X: '1' }) })).toEqual({ X: '1' });
    });
  }
});
