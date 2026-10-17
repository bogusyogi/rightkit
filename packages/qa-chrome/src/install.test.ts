import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { afterAll, describe, expect, it } from 'vitest';
import { CHROME_HEADLESS_SHELL_ENV, ChromeForTestingNotFoundError, chromeForTestingCandidates, chromeHeadlessShellCandidates, resolveChromeHeadlessShell } from './index.js';
import {
  ChromeForTestingDigestMismatchError,
  ChromeForTestingDigestMissingError,
  PINNED_CHROME_FOR_TESTING,
  assertSafeArchiveMembers,
  installChromeForTesting,
  managedCurrentDir,
  managedExecutable,
  managedHeadlessShellExecutable,
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

describe('chrome-headless-shell', () => {
  const rustSource = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'crates', 'rightkit-browser', 'src', 'install.rs'), 'utf8');

  it('pins the same platforms, URLs and digests as rightkit-browser, win64 headless shell included', () => {
    const headless = PINNED_CHROME_FOR_TESTING.downloads.filter((d) => d.kind === 'headless_shell');
    expect(headless.map((d) => d.platform)).toEqual(['win64']);
    expect(headless[0].url).toBe('https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.39/win64/chrome-headless-shell-win64.zip');
    expect(headless[0].sha256).toBe('20798f7c51a22def0b3d02e526a8f52e7ee2c7eddcde9d81a4c23f618e8b5436');
    for (const d of PINNED_CHROME_FOR_TESTING.downloads) {
      expect(d.sha256, `${d.kind ?? 'chrome'} ${d.platform}`).toMatch(/^[0-9a-f]{64}$/);
      expect(rustSource, `${d.kind ?? 'chrome'} ${d.platform} digest is pinned in install.rs`).toContain(`"${d.sha256}"`);
    }
    expect(rustSource).toContain(`version: "${PINNED_CHROME_FOR_TESTING.version}"`);
  });

  it('lays the headless shell out beside chrome-<key>/ inside current', () => {
    expect(managedHeadlessShellExecutable(path.join('r', 'current'), 'win64')).toBe(path.join('r', 'current', 'chrome-headless-shell-win64', 'chrome-headless-shell.exe'));
    expect(managedHeadlessShellExecutable(path.join('r', 'current'), 'linux64')).toBe(path.join('r', 'current', 'chrome-headless-shell-linux64', 'chrome-headless-shell'));
  });

  it('refuses a headless-shell pin without a digest before downloading anything', async () => {
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: root('headless-todo') };
    const pin: PinnedChrome = {
      version: '155.0.1.1',
      downloads: [
        { platform: 'linux64', url: 'https://example.invalid/chrome-linux64.zip', sha256: 'a'.repeat(64) },
        { platform: 'linux64', kind: 'headless_shell', url: 'https://example.invalid/chrome-headless-shell-linux64.zip' },
      ],
    };
    let fetched = false;
    const err = await installChromeForTesting({ env, ...linux, pin, download: async () => { fetched = true; } }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ChromeForTestingDigestMissingError);
    expect((err as ChromeForTestingDigestMissingError).kind).toBe('headless_shell');
    expect(fetched).toBe(false);
  });

  it('rejects two downloads of one kind for the platform', async () => {
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: root('headless-dup') };
    const one = { platform: 'linux64', kind: 'headless_shell' as const, url: 'https://example.invalid/h.zip', sha256: 'b'.repeat(64) };
    const pin: PinnedChrome = {
      version: '155.0.1.1',
      downloads: [{ platform: 'linux64', url: 'https://example.invalid/c.zip', sha256: 'a'.repeat(64) }, one, one],
    };
    await expect(installChromeForTesting({ env, ...linux, pin })).rejects.toThrow(/two headless_shell downloads/);
  });

  it('a headless-shell digest mismatch extracts nothing and leaves current untouched', async () => {
    const r = root('headless-mismatch');
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: r };
    const exe = managedExecutable(path.join(r, 'current'), 'linux64');
    mkdirSync(path.dirname(exe), { recursive: true });
    writeFileSync(exe, 'good');
    // The headless shell is listed first so its archive is the one fetched and checked first.
    const pin: PinnedChrome = {
      version: '155.0.1.1',
      downloads: [
        { platform: 'linux64', kind: 'headless_shell', url: 'https://example.invalid/h.zip', sha256: createHash('sha256').update('expected').digest('hex') },
        { platform: 'linux64', url: 'https://example.invalid/c.zip', sha256: 'a'.repeat(64) },
      ],
    };
    const err = await installChromeForTesting({ env, ...linux, pin, download: async (_url, dest) => { writeFileSync(dest, 'tampered'); } }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ChromeForTestingDigestMismatchError);
    expect(readFileSync(exe, 'utf8')).toBe('good');
    for (const leftover of ['current.staging', 'current.download', 'current.previous']) {
      expect(existsSync(path.join(r, leftover))).toBe(false);
    }
  });

  it('the resolver lists the headless shell in current, then current.previous, then legacy folders', () => {
    const r = root('headless-cand');
    const env = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: r };
    mkdirSync(path.join(r, '999.0.0.0'));
    expect(chromeHeadlessShellCandidates({ env, ...linux, homedir: path.join(scratch, 'home') })).toEqual([
      managedHeadlessShellExecutable(path.join(r, 'current'), 'linux64'),
      managedHeadlessShellExecutable(path.join(r, 'current.previous'), 'linux64'),
      managedHeadlessShellExecutable(path.join(r, '999.0.0.0'), 'linux64'),
    ]);
  });

  it('resolveChromeHeadlessShell finds the installed shell, honours the env override, and throws with the searched paths', () => {
    const r = root('headless-resolve');
    const env: NodeJS.ProcessEnv = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: r };
    const options = { env, ...linux, homedir: path.join(scratch, 'home') };
    expect(() => resolveChromeHeadlessShell(options)).toThrow(ChromeForTestingNotFoundError);
    const shell = managedHeadlessShellExecutable(path.join(r, 'current'), 'linux64');
    mkdirSync(path.dirname(shell), { recursive: true });
    writeFileSync(shell, 'x');
    expect(resolveChromeHeadlessShell(options)).toBe(shell);
    const other = path.join(r, 'other-shell');
    writeFileSync(other, 'x');
    expect(resolveChromeHeadlessShell({ ...options, env: { ...env, [CHROME_HEADLESS_SHELL_ENV]: other } })).toBe(other);
    expect(() => resolveChromeHeadlessShell({ ...options, env: { ...env, [CHROME_HEADLESS_SHELL_ENV]: path.join(r, 'missing') } })).toThrow(/points at a missing file/);
  });
});

describe('assertSafeArchiveMembers', () => {
  it('accepts headless-shell members under chrome-headless-shell-<platform>/ only', () => {
    expect(() => assertSafeArchiveMembers(['chrome-headless-shell-win64/', 'chrome-headless-shell-win64/chrome-headless-shell.exe'], 'win64', 'headless_shell')).not.toThrow();
    expect(() => assertSafeArchiveMembers(['chrome-win64/chrome.exe'], 'win64', 'headless_shell')).toThrow(/unsafe archive member/);
    expect(() => assertSafeArchiveMembers(['chrome-headless-shell-win64/chrome-headless-shell.exe'], 'win64')).toThrow(/unsafe archive member/);
  });

  it('accepts members under chrome-<platform>/ only', () => {
    expect(() => assertSafeArchiveMembers(['chrome-linux64/', 'chrome-linux64/chrome', 'chrome-linux64/locales/en-US.pak'], 'linux64')).not.toThrow();
    for (const bad of ['../x', '/chrome-linux64/chrome', 'chrome-linux64/../../x', 'other/chrome', 'chrome-linux64\\chrome', 'chrome-linux64/a:b']) {
      expect(() => assertSafeArchiveMembers([bad], 'linux64'), bad).toThrow(/unsafe archive member/);
    }
    expect(() => assertSafeArchiveMembers([], 'linux64')).toThrow(/empty/);
  });
});
