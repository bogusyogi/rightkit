import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { afterAll, describe, expect, it } from 'vitest';
import { chromeForTestingCandidates } from './index.js';
import {
  ChromeForTestingDigestMismatchError,
  ChromeForTestingDigestMissingError,
  PINNED_CHROME_FOR_TESTING,
  assertSafeArchiveMembers,
  installChromeForTesting,
  managedCurrentDir,
  managedExecutable,
  managedPreviousDir,
  type PinnedChrome,
} from './install.js';

const scratch = mkdtempSync(path.join(tmpdir(), 'rk-qa-cft-install-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

const linux = { platform: 'linux' as const, arch: 'x64' };

function root(name: string): string {
  const dir = path.join(scratch, name);
  mkdirSync(dir, { recursive: true });
  return dir;
}

function pinFor(sha256: string | undefined): PinnedChrome {
  return { version: '155.0.1.1', downloads: [{ platform: 'linux64', url: 'https://example.invalid/chrome-linux64.zip', sha256 }] };
}

describe('fixed managed layout', () => {
  it('current and current.previous sit directly under the managed root', () => {
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: path.join(scratch, 'r') };
    expect(managedCurrentDir({ env })).toBe(path.join(scratch, 'r', 'current'));
    expect(managedPreviousDir({ env })).toBe(path.join(scratch, 'r', 'current.previous'));
  });

  it('the resolver lists current, then current.previous, before legacy version folders', () => {
    const r = root('cand');
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: r, PUPPETEER_CACHE_DIR: path.join(scratch, 'none') };
    mkdirSync(path.join(r, '999.0.0.0'));
    const candidates = chromeForTestingCandidates({ env, ...linux, homedir: path.join(scratch, 'home') });
    expect(candidates.slice(0, 3)).toEqual([
      managedExecutable(path.join(r, 'current'), 'linux64'),
      managedExecutable(path.join(r, 'current.previous'), 'linux64'),
      managedExecutable(path.join(r, '999.0.0.0'), 'linux64'),
    ]);
  });
});

describe('installChromeForTesting', () => {
  it('ships a pin table whose missing digests are refused, never guessed', async () => {
    for (const d of PINNED_CHROME_FOR_TESTING.downloads) expect(d.url.startsWith('https://')).toBe(true);
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: root('todo') };
    let fetched = false;
    await expect(
      installChromeForTesting({ env, ...linux, pin: pinFor(undefined), download: async () => { fetched = true; } }),
    ).rejects.toBeInstanceOf(ChromeForTestingDigestMissingError);
    expect(fetched).toBe(false);
    expect(existsSync(path.join(env.RIGHTKIT_CHROME_FOR_TESTING_ROOT, 'current.staging'))).toBe(false);
  });

  it('rejects a malformed digest and a non-https URL', async () => {
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: root('bad') };
    await expect(installChromeForTesting({ env, ...linux, pin: pinFor('xyz') })).rejects.toThrow(/64 hex/);
    const http: PinnedChrome = { version: '1.2.3.4', downloads: [{ platform: 'linux64', url: 'http://example.invalid/c.zip', sha256: 'a'.repeat(64) }] };
    await expect(installChromeForTesting({ env, ...linux, pin: http })).rejects.toThrow(/https/);
  });

  it('a digest mismatch extracts nothing and leaves current untouched', async () => {
    const r = root('mismatch');
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: r };
    const exe = managedExecutable(path.join(r, 'current'), 'linux64');
    mkdirSync(path.dirname(exe), { recursive: true });
    writeFileSync(exe, 'good');
    const err = await installChromeForTesting({
      env, ...linux,
      pin: pinFor(createHash('sha256').update('expected').digest('hex')),
      download: async (_url, dest) => { writeFileSync(dest, 'tampered'); },
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ChromeForTestingDigestMismatchError);
    expect(readFileSync(exe, 'utf8')).toBe('good');
    for (const leftover of ['current.staging', 'current.download', 'current.previous']) {
      expect(existsSync(path.join(r, leftover))).toBe(false);
    }
  });
});

describe('assertSafeArchiveMembers', () => {
  it('accepts members under chrome-<platform>/ only', () => {
    expect(() => assertSafeArchiveMembers(['chrome-linux64/', 'chrome-linux64/chrome', 'chrome-linux64/locales/en-US.pak'], 'linux64')).not.toThrow();
    for (const bad of ['../x', '/chrome-linux64/chrome', 'chrome-linux64/../../x', 'other/chrome', 'chrome-linux64\\chrome', 'chrome-linux64/a:b']) {
      expect(() => assertSafeArchiveMembers([bad], 'linux64'), bad).toThrow(/unsafe archive member/);
    }
    expect(() => assertSafeArchiveMembers([], 'linux64')).toThrow(/empty/);
  });
});
