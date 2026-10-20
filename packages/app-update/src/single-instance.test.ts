import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it } from 'vitest';
import {
  UPDATES_PACKAGE_VERSION,
  assertSingleUpdatesVersion,
  registerUpdatesInstance,
} from './single-instance.js';

afterEach(() => {
  delete (globalThis as Record<symbol, unknown>)[Symbol.for('@rightkit/app-update.instances')];
});

describe('single instance guard', () => {
  it('matches package.json', () => {
    const pkg = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')) as { version: string };
    expect(UPDATES_PACKAGE_VERSION).toBe(pkg.version);
  });
  it('tolerates repeated registration of one version', () => {
    expect(registerUpdatesInstance('0.2.5')).toEqual(['0.2.5']);
    expect(() => assertSingleUpdatesVersion('0.2.5')).not.toThrow();
  });
  it('registers the package version by default and is idempotent', () => {
    expect(() => assertSingleUpdatesVersion()).not.toThrow();
    expect(() => assertSingleUpdatesVersion()).not.toThrow();
    expect(registerUpdatesInstance()).toEqual([UPDATES_PACKAGE_VERSION]);
  });
  it('throws on the default path when another copy registered first', () => {
    registerUpdatesInstance('0.0.1');
    expect(() => assertSingleUpdatesVersion()).toThrow(new RegExp(`0\\.0\\.1, ${UPDATES_PACKAGE_VERSION.replaceAll('.', '\\.')}`));
  });
  it('throws when a second version registers', () => {
    registerUpdatesInstance('0.2.4');
    expect(() => assertSingleUpdatesVersion('0.2.5')).toThrow(/0\.2\.4, 0\.2\.5/);
  });
});
