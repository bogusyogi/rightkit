import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { createWdioConfig, defineRightQaConfig } from './native.js';

describe('shared native QA configuration', () => {
  it('creates a single-instance embedded-provider run with stable evidence paths', () => {
    const app = defineRightQaConfig({
      app: 'coderight',
      appRoot: 'D:/apps/coderight',
      windowLabel: 'main',
      specs: ['qa/native/**/*.e2e.mjs'],
      binaries: {
        win32: 'src-tauri/target/debug/coderight.exe',
        darwin: 'src-tauri/target/debug/coderight',
      },
    });
    const config = createWdioConfig(app, { platform: 'win32', embeddedPort: 4555 });

    expect(config.runner).toBe('local');
    expect(config.maxInstances).toBe(1);
    expect(config.specs).toEqual([path.resolve('D:/apps/coderight', 'qa/native/**/*.e2e.mjs')]);
    expect(config.services).toEqual([
      ['@wdio/tauri-service', expect.objectContaining({
        appBinaryPath: path.resolve('D:/apps/coderight', 'src-tauri/target/debug/coderight.exe'),
        driverProvider: 'embedded',
        embeddedPort: 4555,
        windowLabel: 'main',
      })],
    ]);
    expect(config.capabilities).toEqual([
      expect.objectContaining({
        browserName: 'tauri',
        'tauri:options': {
          application: path.resolve('D:/apps/coderight', 'src-tauri/target/debug/coderight.exe'),
        },
      }),
    ]);
    expect(config.outputDir).toBe(path.resolve('D:/apps/coderight', '.cache/rightkit-qa/wdio'));
  });

  it('rejects configs that omit either desktop binary', () => {
    expect(() => defineRightQaConfig({
      app: 'broken',
      appRoot: 'D:/apps/broken',
      windowLabel: 'main',
      specs: ['qa/native/smoke.e2e.mjs'],
      binaries: { win32: 'broken.exe', darwin: '' },
    })).toThrow(/darwin/i);
  });
});
