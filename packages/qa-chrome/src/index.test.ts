import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import {
  CHROME_FOR_TESTING_ENV,
  ChromeForTestingNotFoundError,
  chromeForTestingCandidates,
  chromeProcessesForProfile,
  codeSignCloneEntries,
  findChromeForTesting,
  launchChromeForTesting,
  resolveChromeForTesting,
} from './index.js';

const scratch = mkdtempSync(path.join(tmpdir(), 'rk-qa-cft-test-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

function touch(file: string): string {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, '');
  return file;
}

describe('resolveChromeForTesting', () => {
  const home = path.join(scratch, 'home');
  const macExe = 'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';
  const base = { env: {}, platform: 'darwin' as const, arch: 'arm64', homedir: home };

  it('honours the explicit option first, then the env var', () => {
    const explicit = touch(path.join(scratch, 'explicit', 'chrome'));
    const fromEnv = touch(path.join(scratch, 'env', 'chrome'));
    expect(resolveChromeForTesting({ ...base, executablePath: explicit, env: { [CHROME_FOR_TESTING_ENV]: fromEnv } })).toBe(explicit);
    expect(resolveChromeForTesting({ ...base, env: { [CHROME_FOR_TESTING_ENV]: fromEnv } })).toBe(fromEnv);
  });

  it('fails loudly on a missing explicit or env path instead of searching on', () => {
    expect(() => resolveChromeForTesting({ ...base, executablePath: path.join(scratch, 'nope') })).toThrow(ChromeForTestingNotFoundError);
    expect(() => resolveChromeForTesting({ ...base, env: { [CHROME_FOR_TESTING_ENV]: path.join(scratch, 'nope') } })).toThrow(/points at a missing file/);
  });

  it('prefers the newest Puppeteer build, then Playwright, from known locations', () => {
    const pup = path.join(home, '.cache', 'puppeteer', 'chrome');
    touch(path.join(pup, 'mac_arm-99.0.1.2', 'chrome-mac-arm64', macExe));
    const newest = touch(path.join(pup, 'mac_arm-152.0.7977.75', 'chrome-mac-arm64', macExe));
    touch(path.join(home, 'Library', 'Caches', 'ms-playwright', 'chromium-1243', 'chrome-mac-arm64', macExe));
    const candidates = chromeForTestingCandidates(base);
    expect(candidates.slice(0, 2)).toEqual([path.join('/Applications', macExe), path.join(home, 'Applications', macExe)]);
    expect(candidates.indexOf(newest)).toBeLessThan(candidates.findIndex((c) => c.includes('ms-playwright')));
    if (!existsSync(candidates[0])) expect(resolveChromeForTesting(base)).toBe(newest);
  });

  it('the RightKit-managed folder wins, newest version first, over app installs and caches', () => {
    const managed = path.join(home, 'Library', 'Application Support', 'Orthic Labs', 'shared', 'chrome-for-testing');
    touch(path.join(managed, '153.0.1.1', 'chrome-mac-arm64', macExe));
    const current = touch(path.join(managed, '155.0.8059.39', 'chrome-mac-arm64', macExe));
    const candidates = chromeForTestingCandidates(base);
    expect(candidates[0]).toBe(current);
    expect(resolveChromeForTesting(base)).toBe(current);
    const custom = path.join(scratch, 'custom-root');
    const pinned = touch(path.join(custom, '154.0.1.2', 'chrome-mac-arm64', macExe));
    expect(chromeForTestingCandidates({ ...base, env: { RIGHTKIT_CHROME_FOR_TESTING_ROOT: custom } })[0]).toBe(pinned);
  });

  it('never resolves regular Google Chrome and explains how to install', () => {
    const empty = { env: {}, platform: 'linux' as const, arch: 'x64', homedir: path.join(scratch, 'empty-home') };
    expect(chromeForTestingCandidates({ ...base, homedir: path.join(scratch, 'empty-home') }).some((c) => c.includes('Google Chrome.app'))).toBe(false);
    const error = (() => { try { resolveChromeForTesting(empty); } catch (e) { return e as Error; } })();
    expect(error).toBeInstanceOf(ChromeForTestingNotFoundError);
    expect(error!.message).toMatch(/rightkit-qa-chrome install/);
    expect(error!.message).toMatch(new RegExp(CHROME_FOR_TESTING_ENV));
  });
});

const installed = findChromeForTesting();
if (!installed.path) console.warn(`[chrome.test] real-browser test skipped: ${installed.reason}`);

describe.skipIf(!installed.path)('launchChromeForTesting + graceful close (real Chrome for Testing)', () => {
  it('closes through CDP, leaves no process tree, profile dir, or new code-sign clone', async () => {
    const before = new Set(codeSignCloneEntries());
    const browser = await launchChromeForTesting({ executablePath: installed.path, profilePrefix: 'rk-qa-cft-close-' });
    expect(browser.executablePath).toMatch(/Chrome for Testing|chrome(\.exe)?$/);
    const version = (await (await fetch(`http://127.0.0.1:${browser.port}/json/version`)).json()) as { Browser: string };
    expect(version.Browser).toMatch(/Chrome/);
    expect(existsSync(browser.profileDir)).toBe(true);

    const report = await browser.close();
    expect(report).toMatchObject({ step: 'cdp', exited: true, profileRemoved: true });
    expect(existsSync(browser.profileDir)).toBe(false);
    if (process.platform !== 'win32') {
      expect(() => process.kill(-browser.pid, 0)).toThrow();
    } else {
      // Windows: no browser, renderer, GPU or crashpad process may outlive close().
      expect(chromeProcessesForProfile(browser.profileDir)).toEqual([]);
    }
    // Idempotent.
    expect(await browser.close()).toBe(report);
    const created = codeSignCloneEntries().filter((entry) => !before.has(entry));
    expect(created).toEqual([]);
  }, 60_000);
});
