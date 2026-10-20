import { ChildProcess as ChildProcessClass, spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';
import {
  CHROME_FOR_TESTING_ENV,
  ChromeForTestingNotFoundError,
  chromeForTestingCandidates,
  chromeProcessesForProfile,
  closeChromeGracefully,
  codeSignCloneEntries,
  findChromeForTesting,
  killVerifiedProcesses,
  launchChromeForTesting,
  parseProfileScan,
  probeProcessIdentities,
  probeProfileProcesses,
  removeDirInChild,
  signalTree,
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
    // The managed fixed-layout folders (current, current.previous) always lead; app installs come next.
    const candidates = chromeForTestingCandidates(base).filter((c) => !c.includes(path.join('Orthic Labs', 'shared', 'chrome-for-testing')));
    expect(candidates.slice(0, 2)).toEqual([path.join('/Applications', macExe), path.join(home, 'Applications', macExe)]);
    expect(candidates.indexOf(newest)).toBeLessThan(candidates.findIndex((c) => c.includes('ms-playwright')));
    if (!existsSync(candidates[0])) expect(resolveChromeForTesting(base)).toBe(newest);
  });

  it('the RightKit-managed folder wins, newest version first, over app installs and caches', () => {
    const managed = path.join(home, 'Library', 'Application Support', 'Orthic Labs', 'shared', 'chrome-for-testing');
    const older = touch(path.join(managed, '153.0.1.1', 'chrome-mac-arm64', macExe));
    const current = touch(path.join(managed, '155.0.8059.39', 'chrome-mac-arm64', macExe));
    const candidates = chromeForTestingCandidates(base);
    // Fixed layout first (current, current.previous), then legacy version folders newest first.
    expect(candidates.slice(0, 4)).toEqual([
      path.join(managed, 'current', 'chrome-mac-arm64', macExe), path.join(managed, 'current.previous', 'chrome-mac-arm64', macExe), current, older,
    ]);
    expect(resolveChromeForTesting(base)).toBe(current);
    const custom = path.join(scratch, 'custom-root');
    const pinned = touch(path.join(custom, '154.0.1.2', 'chrome-mac-arm64', macExe));
    expect(chromeForTestingCandidates({ ...base, env: { RIGHTKIT_CHROME_FOR_TESTING_ROOT: custom } })[2]).toBe(pinned);
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
        killProcesses: () => {},
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

  it('Windows: a profile held with EBUSY/EPERM right after the kill is removed on a later attempt, with the leftover pid re-forced', async () => {
    const clock = fakeClock();
    const dir = profile();
    let attempts = 0;
    const killed: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: (procs) => { killed.push(...procs.map((p) => p.pid)); },
        scanProfile: () => (attempts >= 1 && attempts < 3 ? [777] : []),
        removeDir: (target) => {
          attempts += 1;
          if (attempts === 1) throw Object.assign(new Error('busy'), { code: 'EBUSY' });
          if (attempts === 2) throw Object.assign(new Error('perm'), { code: 'EPERM' });
          rmSync(target, { recursive: true, force: true });
        },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(attempts).toBe(3);
    expect(killed).toContain(777);
    expect(existsSync(dir)).toBe(false);
  });

  it('a profile that stays held reports profileRemoved: false after the bound, and never throws', async () => {
    const clock = fakeClock();
    const dir = profile();
    let attempts = 0;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      profileRemoveMs: 1_000,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: () => {},
        scanProfile: () => [],
        removeDir: () => { attempts += 1; throw Object.assign(new Error('busy'), { code: 'EBUSY' }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(attempts).toBeGreaterThan(1);
    expect(report.profileRemoved).toBe(false);
    rmSync(dir, { recursive: true, force: true });
  });

  it('a non-retryable removal error stops at once', async () => {
    const clock = fakeClock();
    const dir = profile();
    let attempts = 0;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: () => {},
        scanProfile: () => [],
        removeDir: () => { attempts += 1; throw Object.assign(new Error('nope'), { code: 'EINVAL' }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(attempts).toBe(1);
    expect(report.profileRemoved).toBe(false);
    rmSync(dir, { recursive: true, force: true });
  });

  describe('profile removal is bounded by its own phase and the global budget', () => {
    const closeSeams = (clock: { now: () => number; sleep: (ms: number) => Promise<void> }) => ({
      platform: 'linux' as const,
      launcherLiveness: () => 'gone' as const,
      signalTree: () => {},
      killProcesses: () => {},
      scanProfile: () => [],
      now: clock.now,
      sleep: clock.sleep,
    });
    /** Removal timers on a manual clock: the first (the bound) fires at once and advances time; later ones (the stop wait) fire at once only when `graceFires`. */
    const timers = (clock: { advance: (ms: number) => void }, graceFires: boolean) => {
      let calls = 0;
      return (ms: number): Promise<void> => {
        calls += 1;
        if (calls === 1 || graceFires) {
          clock.advance(ms);
          return Promise.resolve();
        }
        return new Promise<void>(() => {});
      };
    };
    const clockAt = () => {
      let t = 0;
      return { now: () => t, sleep: async (ms: number) => { t += ms; }, advance: (ms: number) => { t += ms; } };
    };

    it('a zero removal phase attempts no removal: profileRemoval not-attempted, profileRemoved false while the directory exists', async () => {
      const clock = fakeClock();
      const dir = profile();
      let calls = 0;
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 0,
        seams: { ...closeSeams(clock), removeDir: () => { calls += 1; } },
      });
      expect(calls).toBe(0);
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'not-attempted', durationMs: 0 });
      expect(existsSync(dir)).toBe(true);
      rmSync(dir, { recursive: true, force: true });
    });

    it('a removal that ignores its abort is abandoned at the phase bound and reported timed-out-running: close returns, profileRemoved is false', async () => {
      const clock = clockAt();
      const dir = profile();
      const asked: Array<{ timeoutMs: number; signal: AbortSignal }> = [];
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 1_000,
        budgetMs: 60_000,
        seams: {
          ...closeSeams(clock),
          removeDir: (_dir, timeoutMs, signal) => { asked.push({ timeoutMs, signal }); return new Promise<void>(() => {}); },
          removeTimeout: timers(clock, true),
        },
      });
      expect(asked).toHaveLength(1);
      expect(asked[0].timeoutMs).toBe(750);
      expect(asked[0].signal.aborted).toBe(true);
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-running' });
      expect(existsSync(dir)).toBe(true);
      rmSync(dir, { recursive: true, force: true });
    });

    it('the stop wait comes out of the removal phase, not on top of it: a small phase, a large global budget and a stuck remover end inside the phase', async () => {
      const clock = clockAt();
      const dir = profile();
      const asked: number[] = [];
      const timerMs: number[] = [];
      const firing = timers(clock, true);
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 1_000,
        budgetMs: 60_000,
        seams: {
          ...closeSeams(clock),
          removeDir: (_dir, timeoutMs) => { asked.push(timeoutMs); return new Promise<void>(() => {}); },
          removeTimeout: (ms) => { timerMs.push(ms); return firing(ms); },
        },
      });
      // 750 ms for the removal, then the 250 ms reserved inside the same 1 s phase to confirm the stop; the
      // 59 s the global budget still has are not used for the stop wait.
      expect(asked).toEqual([750]);
      expect(timerMs).toEqual([750, 250]);
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-running' });
      expect(report.durationMs).toBeLessThanOrEqual(1_000);
      expect(clock.now()).toBeLessThanOrEqual(1_000);
      rmSync(dir, { recursive: true, force: true });
    });

    it('a removal that stops when told is reported timed-out-terminated, and nothing keeps running', async () => {
      const clock = clockAt();
      const dir = profile();
      let stopped = false;
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 1_000,
        seams: {
          ...closeSeams(clock),
          removeDir: (_dir, _timeoutMs, signal) =>
            new Promise<void>((_resolve, reject) => {
              signal.addEventListener('abort', () => {
                stopped = true;
                reject(Object.assign(new Error('stopped'), { code: 'RK_REMOVE_ABORTED' }));
              });
            }),
          removeTimeout: timers(clock, false),
        },
      });
      expect(stopped).toBe(true);
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-terminated' });
      expect(report.durationMs).toBeLessThanOrEqual(1_000);
      rmSync(dir, { recursive: true, force: true });
    });

    it('a shorter global budget bounds a stalled removal, including its final-observation reserve', async () => {
      const clock = clockAt();
      const dir = profile();
      const asked: number[] = [];
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        budgetMs: 2_000,
        seams: {
          ...closeSeams(clock),
          removeDir: (_dir, timeoutMs) => { asked.push(timeoutMs); return new Promise<void>(() => {}); },
          removeTimeout: timers(clock, true),
        },
      });
      expect(asked).toEqual([1_125]);
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-running' });
      expect(report.durationMs).toBeLessThanOrEqual(2_000);
      rmSync(dir, { recursive: true, force: true });
    });

    it('a normal removal inside the bound reports removed and leaves nothing to abort', async () => {
      const clock = fakeClock();
      const dir = profile();
      let seen: { timeoutMs: number; signal: AbortSignal } | undefined;
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 5_000,
        seams: {
          ...closeSeams(clock),
          removeDir: async (target, timeoutMs, signal) => { seen = { timeoutMs, signal }; rmSync(target, { recursive: true, force: true }); },
        },
      });
      expect(seen?.timeoutMs).toBe(4_000);
      expect(seen?.signal.aborted).toBe(false);
      expect(report).toMatchObject({ exited: true, profileRemoved: true, profileRemoval: 'removed', durationMs: 0 });
      expect(existsSync(dir)).toBe(false);
    });

    it('the default remover (a separate process) removes a real directory', async () => {
      const dir = profile();
      writeFileSync(path.join(dir, 'f'), 'x');
      await removeDirInChild(dir, 10_000, new AbortController().signal);
      expect(existsSync(dir)).toBe(false);
    });

    it('the default remover is terminated on abort: it settles only after its process has exited, and a pre-aborted signal starts nothing', async () => {
      const hang = 'setInterval(() => {}, 1000)';
      const dir = profile();
      const stop = new AbortController();
      const running = removeDirInChild(dir, 10_000, stop.signal, hang);
      running.catch(() => {});
      // Give the process time to start, then spend the bound.
      await new Promise((resolve) => setTimeout(resolve, 300));
      stop.abort();
      const error = (await running.then(() => undefined, (e: unknown) => e)) as (Error & { code?: string; pid?: number }) | undefined;
      expect(error?.code).toBe('RK_REMOVE_ABORTED');
      expect(typeof error?.pid).toBe('number');
      // The remover has exited: its pid is no longer a live process.
      expect(() => process.kill(error!.pid!, 0)).toThrow();
      expect(existsSync(dir)).toBe(true);
      const early = new AbortController();
      early.abort();
      const refused = (await removeDirInChild(dir, 10_000, early.signal, hang).then(() => undefined, (e: unknown) => e)) as (Error & { code?: string; pid?: number }) | undefined;
      expect(refused?.code).toBe('RK_REMOVE_ABORTED');
      expect(refused?.pid).toBeUndefined();
      rmSync(dir, { recursive: true, force: true });
    });

    it('end to end: a close whose default remover hangs terminates it at the bound and reports timed-out-terminated', async () => {
      const dir = profile();
      let removerPid: number | undefined;
      const started = Date.now();
      const report = await closeChromeGracefully({
        child: fakeLauncher(4242, 1),
        profileDir: dir,
        profileRemoveMs: 1_000,
        budgetMs: 20_000,
        seams: {
          platform: 'linux',
          launcherLiveness: () => 'gone',
          signalTree: () => {},
          killProcesses: () => {},
          scanProfile: () => [],
          removeDir: (target, timeoutMs, signal) => {
            const pending = removeDirInChild(target, timeoutMs, signal, 'setInterval(() => {}, 1000)');
            return pending.catch((e: { pid?: number }) => { removerPid = e.pid; throw e; });
          },
        },
      });
      expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-terminated' });
      expect(Date.now() - started).toBeLessThan(5_000);
      expect(removerPid).toBeTypeOf('number');
      expect(() => process.kill(removerPid!, 0)).toThrow();
      rmSync(dir, { recursive: true, force: true });
    });

    it('a kill that fails while the remover is still alive is not an exit: the close reports timed-out-running, never timed-out-terminated', async () => {
      const dir = profile();
      const realKill = ChildProcessClass.prototype.kill;
      let removerPid: number | undefined;
      // The kill fails the way Node reports it: an 'error' event on the child, which stays alive.
      ChildProcessClass.prototype.kill = function (this: ChildProcess) {
        removerPid = this.pid;
        this.emit('error', new Error('kill failed'));
        return false;
      };
      try {
        const report = await closeChromeGracefully({
          child: fakeLauncher(4242, 1),
          profileDir: dir,
          profileRemoveMs: 1_000,
          budgetMs: 20_000,
          seams: {
            platform: 'linux',
            launcherLiveness: () => 'gone',
            signalTree: () => {},
            killProcesses: () => {},
            scanProfile: () => [],
            removeDir: (target, timeoutMs, signal) => removeDirInChild(target, timeoutMs, signal, 'setInterval(() => {}, 1000)'),
          },
        });
        expect(report).toMatchObject({ profileRemoved: false, profileRemoval: 'timed-out-running' });
        expect(removerPid).toBeTypeOf('number');
        // The remover really is still running.
        expect(() => process.kill(removerPid!, 0)).not.toThrow();
      } finally {
        ChildProcessClass.prototype.kill = realKill;
        // The test's own orphan: end it now.
        if (removerPid !== undefined) {
          try { process.kill(removerPid, 'SIGKILL'); } catch { /* already gone */ }
        }
        rmSync(dir, { recursive: true, force: true });
      }
    });

    it('when the remover cannot be confirmed stopped the harness process itself still exits: its handles are released, and the call is not reported settled', async () => {
      const dir = profile();
      const source = pathToFileURL(fileURLToPath(new URL('./remove-dir.ts', import.meta.url))).href;
      const fixture = path.join(scratch, 'abandon-remover.mjs');
      // A harness process whose remover cannot be stopped: kill is a no-op, so the remover (a 20 s timer) lives
      // on and its exit is never recorded. After the abort the harness waits past the stop wait and then has
      // nothing of its own left to do, so it must exit on its own.
      writeFileSync(
        fixture,
        [
          "import { ChildProcess } from 'node:child_process';",
          `import { removeDirInChild } from ${JSON.stringify(source)};`,
          "ChildProcess.prototype.kill = function () { console.log('REMOVER_PID', this.pid); return true; };",
          'const stop = new AbortController();',
          "const running = removeDirInChild(process.argv[2], 10000, stop.signal, 'setTimeout(() => {}, 20000)');",
          'let settled = false;',
          'running.then(() => { settled = true; }, () => { settled = true; });',
          'await new Promise((resolve) => setTimeout(resolve, 300));',
          'stop.abort();',
          'await new Promise((resolve) => setTimeout(resolve, 1500));',
          "console.log('SETTLED', settled);",
        ].join('\n'),
      );
      const harness = spawn(process.execPath, ['--experimental-strip-types', fixture, dir], { stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true });
      let output = '';
      harness.stdout!.setEncoding('utf8');
      harness.stdout!.on('data', (chunk: string) => { output += chunk; });
      const outcome = await new Promise<'exited' | 'hung'>((resolve) => {
        const timer = setTimeout(() => resolve('hung'), 8_000);
        harness.once('close', () => {
          clearTimeout(timer);
          resolve('exited');
        });
      });
      if (outcome === 'hung') harness.kill('SIGKILL');
      // The unstoppable remover is the test's own orphan: end it now rather than wait for its timer.
      const removerPid = Number(/REMOVER_PID (\d+)/.exec(output)?.[1]);
      if (removerPid > 0) {
        try { process.kill(removerPid, 'SIGKILL'); } catch { /* already gone */ }
      }
      rmSync(dir, { recursive: true, force: true });
      expect(outcome).toBe('exited');
      expect(removerPid).toBeGreaterThan(0);
      expect(output).toContain('SETTLED false');
    }, 30_000);
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
        killProcesses: () => {},
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
        killProcesses: () => {},
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
        killProcesses: () => {},
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
        killProcesses: () => {},
        scanProfile: sequence<number[] | null>(null, null, []),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    // The launcher was gone from the first observation, so nothing is signalled; the scan is simply retried.
    expect(report).toMatchObject({ step: 'already-exited', exited: true, leftoverPids: [] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(signals).toEqual([]);
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
        killProcesses: () => {},
        scanProfile: () => null,
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });

  it('a real leftover pid is still reported: exited false, the pid named, and only that descendant forced on Windows', async () => {
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
        killProcesses: (procs) => { killed.push(...procs.map((p) => p.pid)); },
        scanProfile: () => [4343],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ step: 'sigkill', exited: false, leftoverPids: [4343] });
    expect(report.livenessUnknown).toBeUndefined();
    expect(killed).toContain(4343);
    // The launcher was observed gone, so it is not signalled again; only the listed descendant is forced.
    expect(signals).toEqual([]);
  });

  it('on POSIX, with the group root gone and members left, no group signal is sent and the leftover is a reported cleanup failure', async () => {
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
        killProcesses: (procs) => { killed.push(...procs.map((p) => p.pid)); },
        scanProfile: () => [4343],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [4343] });
    expect(signals).toEqual([]);
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
        killProcesses: () => {},
        scanProfile: () => [],
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });
});

describe('closeChromeGracefully: one monotonic budget, early removal, identity-safe signalling (injected seams and clock)', () => {
  const profile = () => mkdtempSync(path.join(scratch, 'profile-'));
  /** A clock the test advances by hand: sleeping and any blocking seam both move it. */
  function manualClock() {
    let t = 0;
    return { now: () => t, sleep: async (ms: number) => { t += ms; }, advance: (ms: number) => { t += ms; } };
  }

  it('an unavailable final scan retains the last known leftover pids', async () => {
    const clock = manualClock();
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      killTimeoutMs: 0,
      settleMs: 0,
      profileRemoveMs: 0,
      seams: {
        platform: 'linux',
        scanProfile: sequence<number[] | null>([777], null),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, leftoverPids: [777], livenessUnknown: true });
  });

  it('zero budget never calls an alive launcher already exited', async () => {
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      budgetMs: 0,
      seams: { launcherLiveness: () => 'alive', now: () => 0, sleep: async () => {} },
    });
    expect(report).toMatchObject({ step: 'not-attempted', exited: false, livenessUnknown: true });
  });

  it('durationMs remains a whole number with a fractional monotonic clock', async () => {
    let t = 100.25;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      seams: { now: () => t, sleep: async () => { t += 0.4; } },
    });
    expect(report.exited).toBe(true);
    expect(report.durationMs).toBe(0);
    expect(Number.isInteger(report.durationMs)).toBe(true);
  });

  it('default settle ends after 4 s so profile removal keeps its own time', async () => {
    const clock = manualClock();
    const removedAt: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        scanProfile: () => [777],
        removeDir: (dir) => { removedAt.push(clock.now()); rmSync(dir, { recursive: true, force: true }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(removedAt).toEqual([4_000]);
    expect(report).toMatchObject({ profileRemoved: true, exited: false, leftoverPids: [777], durationMs: 4_000 });
  });

  it('default force & removal phases keep their 15 s & 10 s caps', async () => {
    const clock = manualClock();
    const removedAt: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      profileDir: profile(),
      settleMs: 0,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'alive',
        scanProfile: () => [],
        signalTree: () => {},
        removeDir: () => { removedAt.push(clock.now()); throw Object.assign(new Error('busy'), { code: 'EBUSY' }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(removedAt[0]).toBe(15_000);
    expect(report).toMatchObject({ exited: false, profileRemoval: 'failed', durationMs: 25_000 });
  });

  it('the budget caps the total duration even when every step would take its full bound (launcher never exits)', async () => {
    const clock = manualClock();
    const dir = profile();
    const cdp: number[] = [];
    const scans: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      wsEndpoint: 'ws://127.0.0.1:1/devtools/browser/x',
      profileDir: dir,
      budgetMs: 6_000,
      graceMs: 5_000,
      killTimeoutMs: 15_000,
      settleMs: 4_000,
      profileRemoveMs: 10_000,
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'alive',
        signalTree: () => {},
        killProcesses: () => {},
        // A blocking scan that uses up its whole timeout and never answers.
        scanProfile: (_dir, timeoutMs) => { scans.push(timeoutMs); clock.advance(timeoutMs); return null; },
        cdpClose: async (_ws, timeoutMs) => { cdp.push(timeoutMs); clock.advance(timeoutMs); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report.durationMs).toBeLessThanOrEqual(6_000);
    expect(clock.now()).toBeLessThanOrEqual(6_000);
    expect(report).toMatchObject({ exited: false, livenessUnknown: true });
    expect([...cdp, ...scans].every((ms) => ms > 0 && ms <= 6_000)).toBe(true);
    // The final fresh scan always ran, inside the budget.
    expect(scans.length).toBeGreaterThan(0);
    rmSync(dir, { recursive: true, force: true });
  });

  it('the budget caps the total duration when scans take up to 2 s and every kill takes its whole timeout (Windows)', async () => {
    const clock = manualClock();
    const dir = profile();
    const scans: number[] = [];
    const killTimeouts: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      budgetMs: 40_000,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: (_procs, timeoutMs) => { killTimeouts.push(timeoutMs); clock.advance(timeoutMs); },
        scanProfile: (_dir, timeoutMs) => { scans.push(timeoutMs); clock.advance(Math.min(timeoutMs, 2_000)); return [4343]; },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report.durationMs).toBeLessThanOrEqual(40_000);
    expect(report).toMatchObject({ exited: false, leftoverPids: [4343] });
    expect(killTimeouts.length).toBeGreaterThan(0);
    expect([...scans, ...killTimeouts].every((ms) => ms > 0 && ms <= 40_000)).toBe(true);
    // No fixed timeout survives: scans never ask for more than the 15 s cap, kills never more than the 10 s cap.
    expect(Math.max(...scans)).toBeLessThanOrEqual(15_000);
    expect(Math.max(0, ...killTimeouts)).toBeLessThanOrEqual(10_000);
    rmSync(dir, { recursive: true, force: true });
  });

  it('removal is attempted right after launcher-gone plus an empty scan, without waiting for the remaining grace or kill bounds', async () => {
    const clock = manualClock();
    const dir = profile();
    const removedAt: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      wsEndpoint: 'ws://127.0.0.1:1/devtools/browser/x',
      profileDir: dir,
      graceMs: 5_000,
      killTimeoutMs: 15_000,
      seams: {
        platform: 'linux',
        launcherLiveness: sequence<'alive' | 'gone' | 'unknown'>('alive', 'gone'),
        signalTree: () => {},
        killProcesses: () => {},
        scanProfile: () => [],
        cdpClose: async () => {},
        removeDir: (target) => { removedAt.push(clock.now()); rmSync(target, { recursive: true, force: true }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(removedAt).toEqual([0]);
    expect(report).toMatchObject({ step: 'cdp', exited: true, profileRemoved: true, durationMs: 0 });
    expect(existsSync(dir)).toBe(false);
  });

  it('retryable removal errors are retried until the budget, then profileRemoved is false', async () => {
    const clock = manualClock();
    const dir = profile();
    const attemptsAt: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      budgetMs: 2_000,
      profileRemoveMs: 60_000,
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: () => {},
        scanProfile: () => [],
        removeDir: () => { attemptsAt.push(clock.now()); throw Object.assign(new Error('busy'), { code: 'EBUSY' }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(attemptsAt.length).toBeGreaterThan(2);
    expect(Math.max(...attemptsAt)).toBeLessThanOrEqual(2_000);
    expect(report.profileRemoved).toBe(false);
    expect(report.durationMs).toBeLessThanOrEqual(2_000);
    rmSync(dir, { recursive: true, force: true });
  });

  it('a scan returning null at the end gives livenessUnknown and exited false, even though the first scan was clean', async () => {
    const clock = manualClock();
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: () => {},
        scanProfile: sequence<number[] | null>([], null),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(report).toMatchObject({ exited: false, profileRemoved: true, leftoverPids: [], livenessUnknown: true });
  });

  it('a pid seen in an earlier scan but absent from the fresh scan is never signalled', async () => {
    const clock = manualClock();
    const signals: string[] = [];
    const killed: number[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killProcesses: (procs) => { killed.push(...procs.map((p) => p.pid)); },
        scanProfile: sequence<number[]>([111], []),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(killed).toEqual([]);
    expect(signals).toEqual([]);
    expect(report).toMatchObject({ step: 'sigkill', exited: true });
  });

  it('PID reuse: a pid listed by an old scan and a different pid by the later scan: only the fresh list is forced', async () => {
    const clock = manualClock();
    const killed: Array<{ pid: number; creation?: string }> = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        killProcesses: (procs) => { killed.push(...procs); },
        scanProfile: sequence<Array<{ pid: number; creation: string }>>([{ pid: 500, creation: '111' }], [{ pid: 600, creation: '222' }], []),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(killed).toEqual([{ pid: 600, creation: '222' }]);
    expect(report.exited).toBe(true);
  });

  it('the launcher is not signalled after it is observed gone; the descendants are still forced from each fresh scan (Windows)', async () => {
    const clock = manualClock();
    const signals: string[] = [];
    let batches = 0;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        // Alive for the first two observations, gone from the third on.
        launcherLiveness: sequence<'alive' | 'gone' | 'unknown'>('alive', 'alive', 'gone'),
        signalTree: (_child, signal) => { signals.push(signal); },
        killProcesses: () => { batches += 1; },
        scanProfile: () => (batches >= 2 ? [] : [{ pid: 200, creation: '9' }]),
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(signals).toEqual(['SIGKILL']);
    expect(batches).toBe(2);
    expect(report.exited).toBe(true);
  });

  it('an unknown pid makes the scan unknown and nothing is killed or signalled; the report stays unknown', async () => {
    const clock = manualClock();
    const signals: string[] = [];
    let batches = 0;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: profile(),
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killProcesses: () => { batches += 1; },
        // What parseProfileScan returns when any listed pid is unknown.
        scanProfile: () => null,
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(batches).toBe(0);
    expect(signals).toEqual([]);
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });
});

describe('parseProfileScan (Windows scan output)', () => {
  it('reads pid|creation lines between the scan and its completion marker', () => {
    expect(parseProfileScan('123|638000000000000001\r\n456|638000000000000002\r\nRK_QA_SCAN_COMPLETE\r\n')).toEqual([
      { pid: 123, creation: '638000000000000001' },
      { pid: 456, creation: '638000000000000002' },
    ]);
    expect(parseProfileScan('RK_QA_SCAN_COMPLETE')).toEqual([]);
  });

  it('an UNKNOWN pid makes the whole scan unknown (null), even beside live pids', () => {
    expect(parseProfileScan('123|638000000000000001\nUNKNOWN|456\nRK_QA_SCAN_COMPLETE\n')).toBeNull();
  });

  it('output without the completion marker is a failed scan', () => {
    expect(parseProfileScan('123|638000000000000001\n')).toBeNull();
    expect(parseProfileScan('')).toBeNull();
  });
});

describe('closeChromeGracefully: phase caps, POSIX group root gone (injected seams and clock)', () => {
  const profile = () => mkdtempSync(path.join(scratch, 'profile-'));
  function manualClock() {
    let t = 0;
    return { now: () => t, sleep: async (ms: number) => { t += ms; }, advance: (ms: number) => { t += ms; } };
  }

  it('phase caps hold within the global budget: every scan, kill and removal-kill gets min(global remaining, phase remaining)', async () => {
    const clock = manualClock();
    const dir = profile();
    const scans: number[] = [];
    const kills: number[] = [];
    let attempts = 0;
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 1),
      profileDir: dir,
      budgetMs: 60_000,
      killTimeoutMs: 3_000,
      settleMs: 1_000,
      profileRemoveMs: 1_500,
      seams: {
        platform: 'win32',
        launcherLiveness: () => 'gone',
        signalTree: () => {},
        // Scans answer at once; every kill uses up the whole timeout it was given.
        killProcesses: (_procs, timeoutMs) => { kills.push(timeoutMs); clock.advance(timeoutMs); },
        scanProfile: (_dir, timeoutMs) => { scans.push(timeoutMs); return [4343]; },
        removeDir: () => { attempts += 1; throw Object.assign(new Error('busy'), { code: 'EBUSY' }); },
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    // Kill phase (3 s): its kill gets the 3 s phase. Settle (1 s): four scans, each with what the phase has left.
    // Removal (1.5 s): one attempt, then its scan gets 1.5 s and its kill the 1.5 s the phase still has; that
    // kill spends the phase, so no second attempt is started past the removal bound. The first and final
    // observations are bounded by the global budget and the 15 s scan cap.
    expect(kills).toEqual([3_000, 1_500]);
    expect(scans).toEqual([15_000, 3_000, 1_000, 750, 500, 250, 1_500, 15_000]);
    expect(attempts).toBe(1);
    expect(report).toMatchObject({ exited: false, profileRemoved: false, profileRemoval: 'failed', leftoverPids: [4343] });
    expect(report.durationMs).toBe(5_500);
    rmSync(dir, { recursive: true, force: true });
  });

  it('POSIX: the group root is gone and the group still has members: no group signal is sent, and the report is a cleanup failure', async () => {
    const clock = manualClock();
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      wsEndpoint: 'ws://127.0.0.1:1/devtools/browser/x',
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'alive', // the group probe still answers for the members; Node recorded the root's exit
        signalTree: (_child, signal) => { signals.push(signal); },
        killProcesses: () => {},
        scanProfile: () => [4343, 4344],
        cdpClose: async () => {},
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(signals).toEqual([]);
    expect(report).toMatchObject({ exited: false, leftoverPids: [4343, 4344] });
    expect(report.livenessUnknown).toBeUndefined();
  });

  it('POSIX: the group root is gone and the scan fails: unknown, still no group signal', async () => {
    const clock = manualClock();
    const signals: string[] = [];
    const report = await closeChromeGracefully({
      child: fakeLauncher(4242, 0),
      profileDir: profile(),
      seams: {
        platform: 'linux',
        launcherLiveness: () => 'gone',
        signalTree: (_child, signal) => { signals.push(signal); },
        killProcesses: () => {},
        scanProfile: () => null,
        now: clock.now,
        sleep: clock.sleep,
      },
    });
    expect(signals).toEqual([]);
    expect(report).toMatchObject({ exited: false, leftoverPids: [], livenessUnknown: true });
  });
});

describe.skipIf(process.platform === 'win32')('POSIX group root gone with members left (real processes)', () => {
  it('neither the host-exit signal nor close() signals the saved group once the root has exited; the member is reported', async () => {
    const root = mkdtempSync(path.join(tmpdir(), 'rk-qa-pgid-'));
    const profile = path.join(root, 'profile');
    const member = path.join(root, 'member.mjs');
    const parent = path.join(root, 'parent.mjs');
    writeFileSync(member, 'setInterval(() => {}, 1000);\n');
    // The root starts a member in its own process group, prints the member's pid and exits.
    writeFileSync(parent, [
      "import { spawn } from 'node:child_process';",
      "import { writeSync } from 'node:fs';",
      `const m = spawn(process.execPath, [${JSON.stringify(member)}, '--user-data-dir=${profile}'], { stdio: 'ignore' });`,
      'writeSync(1, String(m.pid) + "\\n");',
      'process.exit(0);',
    ].join('\n'));
    const launcher = spawn(process.execPath, [parent], { stdio: ['ignore', 'pipe', 'ignore'], detached: true });
    let memberPid = 0;
    try {
      memberPid = await new Promise<number>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('no member pid')), 10_000);
        launcher.stdout!.setEncoding('utf8');
        launcher.stdout!.once('data', (chunk: string) => { clearTimeout(timer); resolve(Number.parseInt(chunk.trim(), 10)); });
      });
      if (launcher.exitCode === null && launcher.signalCode === null) {
        await new Promise<void>((resolve) => launcher.once('exit', () => resolve()));
      }
      expect(memberPid).toBeGreaterThan(0);
      expect(() => process.kill(memberPid, 0)).not.toThrow();
      // Host exit: the root's exit is recorded, so the saved group id is not signalled again.
      signalTree(launcher, 'SIGTERM');
      signalTree(launcher, 'SIGKILL');
      await new Promise((resolve) => setTimeout(resolve, 300));
      expect(() => process.kill(memberPid, 0)).not.toThrow();
      // close(): the member is reported as a leftover and is not signalled.
      const report = await closeChromeGracefully({ child: launcher, profileDir: profile, settleMs: 300, profileRemoveMs: 300, budgetMs: 20_000 });
      expect(report.exited).toBe(false);
      expect(report.leftoverPids).toContain(memberPid);
      expect(() => process.kill(memberPid, 0)).not.toThrow();
    } finally {
      if (memberPid > 0) { try { process.kill(memberPid, 'SIGKILL'); } catch { /* already gone */ } }
      rmSync(root, { recursive: true, force: true });
    }
  }, 60_000);
});

describe.skipIf(process.platform !== 'win32')('Windows native process identity (real processes)', () => {
  /** An idle node process whose command line carries the profile, like a Chrome child. */
  function startIdle(root: string, profile: string): ChildProcess {
    const script = path.join(root, 'child.mjs');
    writeFileSync(script, 'setInterval(() => {}, 1000);\n');
    return spawn(process.execPath, [script, `--user-data-dir=${profile}`], { stdio: 'ignore', windowsHide: true });
  }
  const alive = (child: ChildProcess) => child.exitCode === null && child.signalCode === null;
  const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  async function waitExit(child: ChildProcess): Promise<void> {
    if (!alive(child)) return;
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('child did not exit')), 10_000);
      child.once('exit', () => { clearTimeout(timer); resolve(); });
    });
  }

  it('a native birth token that does not match the live pid is unknown, and nothing is killed', async () => {
    const root = mkdtempSync(path.join(tmpdir(), 'rk-qa-ident-'));
    const profile = path.join(root, 'profile');
    const child = startIdle(root, profile);
    try {
      await pause(500);
      const scanned = probeProfileProcesses(profile, 30_000);
      expect(scanned).toHaveLength(1);
      const [live] = scanned!;
      expect(live.pid).toBe(child.pid);
      expect(live.creation).toMatch(/^\d+$/);
      // Every distinct full native birth token is rejected, including a single 100 ns tick.
      expect(probeProcessIdentities([{ pid: live.pid, cimFileTime: String(BigInt(live.creation!) + 1n) }], 30_000)).toBeNull();
      expect(probeProcessIdentities([{ pid: live.pid, cimFileTime: String(BigInt(live.creation!) - 1n) }], 30_000)).toBeNull();
      expect(probeProcessIdentities([{ pid: live.pid, cimFileTime: String(BigInt(live.creation!) + 10n) }], 30_000)).toBeNull();
      expect(probeProcessIdentities([{ pid: live.pid, cimFileTime: String(BigInt(live.creation!) - 10n) }], 30_000)).toBeNull();
      // An exact native token succeeds independently of CIM's lower timestamp precision.
      expect(probeProcessIdentities([{ pid: live.pid, cimFileTime: live.creation! }], 30_000)).toEqual([{ pid: live.pid, creation: live.creation }]);
      // A close fed the stale row reports unknown and kills nothing.
      const report = await closeChromeGracefully({
        child: { pid: 4242, exitCode: 1, signalCode: null } as unknown as ChildProcess,
        profileDir: profile,
        killTimeoutMs: 1_000,
        settleMs: 500,
        budgetMs: 30_000,
        seams: {
          platform: 'win32',
          launcherLiveness: () => 'gone',
          scanProfile: (_dir, timeoutMs) => probeProcessIdentities([{ pid: live.pid, cimFileTime: String(BigInt(live.creation!) + 10n) }], timeoutMs),
        },
      });
      expect(report).toMatchObject({ exited: false, livenessUnknown: true });
      expect(alive(child)).toBe(true);
    } finally {
      try { child.kill(); } catch { /* already gone */ }
      rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    }
  }, 120_000);

  it('scan then kill with a replaced pid (different creation time) is not killed; the scanned creation time kills exactly that process', async () => {
    const root = mkdtempSync(path.join(tmpdir(), 'rk-qa-kill-'));
    const profile = path.join(root, 'profile');
    const first = startIdle(root, profile);
    const second = startIdle(root, profile);
    try {
      await pause(500);
      const scanned = probeProfileProcesses(profile, 30_000)!;
      const a = scanned.find((p) => p.pid === first.pid)!;
      const b = scanned.find((p) => p.pid === second.pid)!;
      expect(a.creation).toBeDefined();
      expect(b.creation).toBeDefined();
      expect(a.creation).not.toBe(b.creation);
      // The pid now belongs to a process with another creation time: it must survive.
      killVerifiedProcesses([{ pid: a.pid, creation: b.creation }], 30_000);
      killVerifiedProcesses([{ pid: a.pid, creation: String(BigInt(a.creation!) + 1n) }], 30_000);
      await pause(300);
      expect(alive(first)).toBe(true);
      expect(alive(second)).toBe(true);
      // A bare pid (no creation time) is never killed either.
      killVerifiedProcesses([{ pid: a.pid }], 30_000);
      await pause(300);
      expect(alive(first)).toBe(true);
      // The scanned creation time kills exactly that process.
      killVerifiedProcesses([a], 30_000);
      await waitExit(first);
      expect(alive(first)).toBe(false);
      expect(alive(second)).toBe(true);
    } finally {
      try { first.kill(); } catch { /* already gone */ }
      try { second.kill(); } catch { /* already gone */ }
      rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    }
  }, 120_000);
});
