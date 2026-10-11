import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import {
  CHROME_FOR_TESTING_ENV,
  ChromeForTestingNotFoundError,
  chromeForTestingCandidates,
  chromeProcessesForProfile,
  closeChromeGracefully,
  codeSignCloneEntries,
  findChromeForTesting,
  launchChromeForTesting,
  resolveChromeForTesting,
  settleProfileProcesses,
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
    expect(report).toMatchObject({ exited: true, profileRemoved: true });
    // Windows: under load Chrome's children can outlive CDP close past the short profile grace,
    // and the forced sweep then reports `sigkill`; the tree and profile are still gone.
    expect(process.platform === 'win32' ? ['cdp', 'sigkill'] : ['cdp']).toContain(report.step);
    expect(existsSync(browser.profileDir)).toBe(false);
    expect(report.leftoverPids).toEqual([]);
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

describe.skipIf(process.platform === 'win32')('chromeProcessesForProfile on POSIX (process-list scan)', () => {
  it('finds a leftover process whose parent was killed without cleanup, and only for the exact profile', () => {
    const root = mkdtempSync(path.join(tmpdir(), 'rk-qa-orphan-'));
    try {
      const profile = path.join(root, 'profile');
      const child = path.join(root, 'child.mjs');
      const parent = path.join(root, 'parent.mjs');
      // The child stands in for a Chrome helper: its argv carries the profile flag, like Chrome's children.
      writeFileSync(child, 'setInterval(() => {}, 1000);\n');
      writeFileSync(parent, [
        "import { spawn } from 'node:child_process';",
        "import { writeSync } from 'node:fs';",
        `const c = spawn(process.execPath, [${JSON.stringify(child)}, '--user-data-dir=${profile}'], { stdio: 'ignore', detached: true });`,
        'c.unref();',
        'writeSync(1, String(c.pid) + "\\n");',
        "process.kill(process.pid, 'SIGKILL'); // no cleanup: the child is left behind",
      ].join('\n'));
      const run = spawnSync(process.execPath, [parent], { encoding: 'utf8' });
      expect(run.signal).toBe('SIGKILL');
      const childPid = Number.parseInt(run.stdout.trim(), 10);
      expect(childPid).toBeGreaterThan(0);
      try {
        expect(chromeProcessesForProfile(profile)).toContain(childPid);
        // The profile flag is matched exactly: a sibling profile path with the same prefix is not a match.
        expect(chromeProcessesForProfile(`${profile}2`)).not.toContain(childPid);
        expect(chromeProcessesForProfile(path.join(root, 'missing'))).toEqual([]);
      } finally {
        try { process.kill(childPid, 'SIGKILL'); } catch { /* already gone */ }
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe.skipIf(process.platform !== 'win32')('chromeProcessesForProfile on Windows (CIM command-line scan)', () => {
  it('finds the processes of exactly one profile: quoted, unquoted, a file inside it, never a sibling prefix', () => {
    const root = mkdtempSync(path.join(tmpdir(), 'rk-qa-scan-'));
    const script = path.join(root, 'child.mjs');
    writeFileSync(script, 'setInterval(() => {}, 1000);\n');
    const profile = path.join(root, 'profile');
    const sibling = `${profile}2`;
    const spaced = path.join(root, 'pro file');
    // Each child stands in for a Chrome process: its command line carries the profile, like Chrome's children.
    const start = (argument: string) => {
      const child = spawn(process.execPath, [script, argument], { stdio: 'ignore', windowsHide: true });
      expect(child.pid).toBeGreaterThan(0);
      return child;
    };
    const children = [
      start(`--user-data-dir=${profile}`),
      start(`--database=${path.join(profile, 'Crashpad')}`),
      start(`--user-data-dir=${sibling}`),
      start(`--user-data-dir=${spaced}`), // has a space, so Windows quotes the whole argument
    ];
    const [main, inside, other, quoted] = children.map((child) => child.pid!);
    try {
      const found = chromeProcessesForProfile(profile);
      expect(found).toContain(main);
      expect(found).toContain(inside);
      expect(found).not.toContain(other);
      expect(found).not.toContain(quoted);
      // The match ignores case, as Windows paths do.
      expect(chromeProcessesForProfile(profile.toUpperCase())).toContain(main);
      expect(chromeProcessesForProfile(sibling)).toEqual([other]);
      expect(chromeProcessesForProfile(spaced)).toEqual([quoted]);
      expect(chromeProcessesForProfile(path.join(root, 'missing'))).toEqual([]);
    } finally {
      for (const child of children) { try { child.kill(); } catch { /* already gone */ } }
      spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Milliseconds 300'], { windowsHide: true });
      rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    }
  }, 120_000);
});

/** A fake clock and delay: sleeping advances the clock, so bounds are exact and no real time passes. */
function fakeClock() {
  let t = 0;
  return { now: () => t, sleep: async (ms: number) => { t += ms; } };
}
/** A lister that returns the scripted answers in order, then repeats the last one. */
function scripted(answers: number[][]) {
  let call = 0;
  const list = () => answers[Math.min(call++, answers.length - 1)];
  return { list, calls: () => call };
}

describe('settleProfileProcesses (bounded settle, injected lister and clock)', () => {
  it('pids listed on the first scan and gone on the third: exited, nothing left, the exits are observed', async () => {
    const clock = fakeClock();
    const lister = scripted([[7452, 54436], [7452], []]);
    const result = await settleProfileProcesses({ list: lister.list, boundMs: 4_000, pollMs: 250, now: clock.now, sleep: clock.sleep });
    expect(result).toMatchObject({ leftoverPids: [], scans: 3 });
    expect([...result.observedExitingPids].sort((a, b) => a - b)).toEqual([7452, 54436]);
    expect(lister.calls()).toBe(3);
  });

  it('pids that never exit are reported as leftover after the bound, and none are called exiting', async () => {
    const clock = fakeClock();
    const lister = scripted([[7452, 54436]]);
    const result = await settleProfileProcesses({ list: lister.list, boundMs: 1_000, pollMs: 250, now: clock.now, sleep: clock.sleep });
    expect(result.leftoverPids).toEqual([7452, 54436]);
    expect(result.observedExitingPids).toEqual([]);
    expect(clock.now()).toBe(1_000);
    expect(result.scans).toBe(5);
  });

  it('separates a pid that exited from one that stayed: only the stayed one is leftover', async () => {
    const clock = fakeClock();
    const lister = scripted([[10, 20], [20], [20], [20], [20]]);
    const result = await settleProfileProcesses({ list: lister.list, boundMs: 1_000, pollMs: 250, now: clock.now, sleep: clock.sleep });
    expect(result.leftoverPids).toEqual([20]);
    expect(result.observedExitingPids).toEqual([10]);
  });

  it('a clean first scan returns at once without waiting', async () => {
    const clock = fakeClock();
    const lister = scripted([[]]);
    const result = await settleProfileProcesses({ list: lister.list, now: clock.now, sleep: clock.sleep });
    expect(result).toEqual({ leftoverPids: [], observedExitingPids: [], scans: 1 });
    expect(clock.now()).toBe(0);
  });

  it('a zero bound scans once and reports what it saw, without waiting', async () => {
    const clock = fakeClock();
    const lister = scripted([[5]]);
    const result = await settleProfileProcesses({ list: lister.list, boundMs: 0, now: clock.now, sleep: clock.sleep });
    expect(result.leftoverPids).toEqual([5]);
    expect(clock.now()).toBe(0);
  });

  it('a scan that never answers is reported unknown, not as an empty profile', async () => {
    const clock = fakeClock();
    const result = await settleProfileProcesses({ list: () => null, boundMs: 1_000, pollMs: 250, now: clock.now, sleep: clock.sleep });
    expect(result).toMatchObject({ leftoverPids: [], unknown: true });
    expect(clock.now()).toBe(1_000);
  });

  it('a scan that fails once and then answers is retried inside the bound and is not unknown', async () => {
    const clock = fakeClock();
    const result = await settleProfileProcesses({ list: sequence<number[] | null>(null, []), boundMs: 4_000, pollMs: 250, now: clock.now, sleep: clock.sleep });
    expect(result).toEqual({ leftoverPids: [], observedExitingPids: [], scans: 2 });
  });
});

/** A reply source: returns the scripted values in order, then repeats the last one. */
function sequence<T>(...values: T[]): () => T {
  let call = 0;
  return () => values[Math.min(call++, values.length - 1)];
}

/** A stand-in for the launcher's ChildProcess: only the fields the close path reads. */
function fakeLauncher(pid: number, exitCode: number | null = null): ChildProcess {
  return { pid, exitCode, signalCode: null } as unknown as ChildProcess;
}

describe('closeChromeGracefully (injected launcher probe, profile scan and clock; no real process)', () => {
  /** A real, empty temp profile. The close removes it, and each test checks that it did. */
  const profile = () => mkdtempSync(path.join(scratch, 'profile-'));

  it('the reported shape: launcher exited, probe still says alive, profile scan empty → exited at once, no wait', async () => {
    const clock = fakeClock();
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'alive',
        signalTree: (_child, signal) => { signals.push(signal); },
        killPid: () => {},
        scanProfile: () => [],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ step: 'already-exited', exited: true, profileRemoved: true, leftoverPids: [], observedExitingPids: [] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(signals).toEqual([]);
    expect(report.durationMs).toBe(0);
  });

  it('a kill that uses up the whole bound, with the exit recorded on the next loop turn, reads as exited', async () => {
    let t = 0;
    let exitPending = false;
    const child = fakeLauncher(4242);
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child,
      profileDir: profile(),
      killTimeoutMs: 15_000,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'alive',
        // A blocking kill that outlasts the whole bound. Node records the exit only on the next loop turn,
        // so the first read after the kill is stale; the close must take a fresh read before concluding.
        signalTree: (_child, signal) => { signals.push(signal); t += 20_000; exitPending = true; },
        killPid: () => {},
        scanProfile: () => [],
        now: () => t,
        sleep: async (ms) => { if (exitPending) child.exitCode = 1; t += ms; },
      },
    });
    expect(report).toMatchObject({ step: 'sigkill', exited: true, leftoverPids: [] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(signals).toEqual(['SIGKILL']);
    expect(report.durationMs).toBe(20_000);
  });

  it('a liveness probe that throws is unknown, never alive or gone: exited false, livenessUnknown set, no pid claimed', async () => {
    const clock = fakeClock();
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      profileDir: profile(),
      graceMs: 5_000,
      killTimeoutMs: 15_000,
      seams: {
        platform: 'linux',
        launcherLiveness: () => { throw new Error('kill(-pid, 0) failed'); },
        signalTree: () => {},
        killPid: () => {},
        scanProfile: () => [],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
    // The unknown answer is retried through the grace and the kill bound, then reported; nothing is waited beyond them.
    expect(report.durationMs).toBe(20_000);
  });

  it('a liveness probe that gives unknown twice and then answers is retried inside the bound: exited', async () => {
    const clock = fakeClock();
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: sequence<'alive' | 'gone' | 'unknown'>('unknown', 'unknown', 'gone'),
        signalTree: (_child, signal) => { signals.push(signal); },
        killPid: () => {},
        scanProfile: () => [],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ step: 'sigterm', exited: true, leftoverPids: [] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(signals).toEqual(['SIGTERM']);
  });

  it('a profile scan that fails once and then answers is retried inside the bound: exited, not unknown', async () => {
    const clock = fakeClock();
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killPid: () => {},
        scanProfile: sequence<number[] | null>(null, null, []),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ step: 'sigterm', exited: true, leftoverPids: [] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(signals).toEqual(['SIGTERM']);
  });

  it('a profile scan that never answers is unknown, not clean: exited false, livenessUnknown set', async () => {
    const clock = fakeClock();
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killPid: () => {},
        scanProfile: () => null,
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });

  it('a real leftover pid is still reported: exited false, the pid named, and forced on Windows', async () => {
    const clock = fakeClock();
    const signals: string[] = [];
    const killed: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killPid: (pid) => { killed.push(pid); },
        scanProfile: () => [4343],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ step: 'sigkill', exited: false, leftoverPids: [4343] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(killed).toContain(4343);
    expect(signals).toContain('SIGKILL');
  });

  it('on POSIX a real leftover pid is named and the process group is signalled, never the pid alone', async () => {
    const clock = fakeClock();
    const signals: string[] = [];
    const killed: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killPid: (pid) => { killed.push(pid); },
        scanProfile: () => [4343],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [4343] });
    expect(signals).toContain('SIGKILL');
    expect(killed).toEqual([]);
  });

  it('a launcher never seen to exit while the profile scan lists nothing: exited false, livenessUnknown set, no pid claimed', async () => {
    const clock = fakeClock();
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'alive',
        signalTree: () => {},
        killPid: () => {},
        scanProfile: () => [],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });
});
