/**
 * Chrome for Testing for repository QA: one resolver, one launcher, one graceful close.
 *
 * Regular Google Chrome on macOS makes an APFS clone of its app bundle per instance
 * (`<DARWIN_USER_TEMP_DIR>/../X/com.google.Chrome.code_sign_clone/`). A normal shutdown
 * removes it; SIGKILL or a crash leaves it behind. Chrome for Testing does not create the
 * clone, and a graceful close (CDP `Browser.close`, then SIGTERM, then SIGKILL last) lets
 * any browser clean up. Harnesses never fall back to regular Chrome silently.
 *
 * This is for repo-owned web/CDP harnesses only. The native lane (`native.ts`) drives the
 * app's own Tauri WebView and is unaffected.
 */
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import path from 'node:path';
import { chromeForTestingPlatform, managedChromeForTestingRoot, managedExecutable, managedVersions } from './install.js';

export * from './install.js';

/** Environment variable naming an explicit Chrome for Testing executable. */
export const CHROME_FOR_TESTING_ENV = 'RIGHTKIT_CHROME_FOR_TESTING';

export const CHROME_FOR_TESTING_INSTALL_HINT = [
  'Install Chrome for Testing into the RightKit-managed folder:',
  '  pnpm exec rightkit-qa-chrome install',
  `or point ${CHROME_FOR_TESTING_ENV} at its executable, e.g.`,
  `  ${CHROME_FOR_TESTING_ENV}="$HOME/.cache/puppeteer/chrome/mac_arm-<version>/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"`,
  'Regular Google Chrome is never used as a fallback: on macOS each killed instance leaks a code-sign clone.',
].join('\n');

export class ChromeForTestingNotFoundError extends Error {
  readonly searched: string[];

  constructor(message: string, searched: string[]) {
    super(`${message}\nSearched:\n${searched.map((p) => `  ${p}`).join('\n') || '  (nothing)'}\n${CHROME_FOR_TESTING_INSTALL_HINT}`);
    this.name = 'ChromeForTestingNotFoundError';
    this.searched = searched;
  }
}

export interface ResolveChromeForTestingOptions {
  /** Explicit executable; wins over the environment and known locations. */
  executablePath?: string;
  env?: NodeJS.ProcessEnv;
  platform?: NodeJS.Platform;
  arch?: string;
  homedir?: string;
}

const MAC_APP = 'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';

/** Numeric sort key from a versioned cache dir name (`mac_arm-152.0.7977.75`, `chromium-1243`). */
function versionKey(name: string): number[] {
  return (name.match(/\d+/g) ?? []).map(Number);
}

function newestFirst(a: string, b: string): number {
  const ka = versionKey(a);
  const kb = versionKey(b);
  for (let i = 0; i < Math.max(ka.length, kb.length); i++) {
    const d = (kb[i] ?? -1) - (ka[i] ?? -1);
    if (d !== 0) return d;
  }
  return b.localeCompare(a);
}

function versionedChildren(root: string, prefix: RegExp): string[] {
  try {
    return readdirSync(root).filter((name) => prefix.test(name)).sort(newestFirst).map((name) => path.join(root, name));
  } catch {
    return [];
  }
}

/**
 * Known Chrome for Testing install locations in resolution order (after the explicit option
 * and the environment variable): the RightKit-managed folder (newest version first, see
 * `rightkit-qa-chrome install`), then app installs, then Puppeteer's and Playwright's caches
 * as legacy fallbacks.
 */
export function chromeForTestingCandidates(options: ResolveChromeForTestingOptions = {}): string[] {
  const env = options.env ?? process.env;
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const home = options.homedir ?? homedir();
  const puppeteerRoot = path.join(env.PUPPETEER_CACHE_DIR || path.join(home, '.cache', 'puppeteer'), 'chrome');
  const out: string[] = [];
  const key = chromeForTestingPlatform(platform, arch);
  for (const v of managedVersions({ env, platform, homedir: home })) {
    out.push(managedExecutable(path.join(managedChromeForTestingRoot({ env, platform, homedir: home }), v), key));
  }

  if (platform === 'darwin') {
    out.push(path.join('/Applications', MAC_APP), path.join(home, 'Applications', MAC_APP));
    const macDirs = arch === 'arm64' ? ['chrome-mac-arm64', 'chrome-mac-x64'] : ['chrome-mac-x64', 'chrome-mac-arm64'];
    for (const dir of versionedChildren(puppeteerRoot, /^mac(_arm)?-/)) {
      for (const sub of macDirs) out.push(path.join(dir, sub, MAC_APP));
    }
    const playwrightRoot = env.PLAYWRIGHT_BROWSERS_PATH || path.join(home, 'Library', 'Caches', 'ms-playwright');
    for (const dir of versionedChildren(playwrightRoot, /^chromium-\d/)) {
      for (const sub of macDirs) out.push(path.join(dir, sub, MAC_APP));
    }
  } else if (platform === 'win32') {
    const local = env.LOCALAPPDATA || path.join(home, 'AppData', 'Local');
    for (const dir of versionedChildren(puppeteerRoot, /^win(32|64)-/)) {
      out.push(path.join(dir, 'chrome-win64', 'chrome.exe'), path.join(dir, 'chrome-win32', 'chrome.exe'));
    }
    const playwrightRoot = env.PLAYWRIGHT_BROWSERS_PATH || path.join(local, 'ms-playwright');
    for (const dir of versionedChildren(playwrightRoot, /^chromium-\d/)) {
      out.push(path.join(dir, 'chrome-win64', 'chrome.exe'));
    }
  } else {
    for (const dir of versionedChildren(puppeteerRoot, /^linux(_arm)?-/)) {
      out.push(path.join(dir, 'chrome-linux64', 'chrome'), path.join(dir, 'chrome-linux-arm64', 'chrome'));
    }
    const playwrightRoot = env.PLAYWRIGHT_BROWSERS_PATH || path.join(home, '.cache', 'ms-playwright');
    for (const dir of versionedChildren(playwrightRoot, /^chromium-\d/)) {
      out.push(path.join(dir, 'chrome-linux64', 'chrome'));
    }
  }
  return out;
}

/**
 * Resolve the Chrome for Testing executable: `executablePath`, then `RIGHTKIT_CHROME_FOR_TESTING`,
 * then the known install locations. Throws {@link ChromeForTestingNotFoundError} with install
 * instructions when none exists; never falls back to regular Chrome.
 */
export function resolveChromeForTesting(options: ResolveChromeForTestingOptions = {}): string {
  const env = options.env ?? process.env;
  if (options.executablePath) {
    if (existsSync(options.executablePath)) return options.executablePath;
    throw new ChromeForTestingNotFoundError(`explicit Chrome for Testing executable does not exist: ${options.executablePath}`, [options.executablePath]);
  }
  const fromEnv = env[CHROME_FOR_TESTING_ENV];
  if (fromEnv) {
    if (existsSync(fromEnv)) return fromEnv;
    throw new ChromeForTestingNotFoundError(`${CHROME_FOR_TESTING_ENV} points at a missing file: ${fromEnv}`, [fromEnv]);
  }
  const candidates = chromeForTestingCandidates(options);
  const found = candidates.find((candidate) => existsSync(candidate));
  if (found) return found;
  throw new ChromeForTestingNotFoundError('Chrome for Testing is not installed', candidates);
}

/** Non-throwing form for test skips: `{ path }` or `{ reason }`. */
export function findChromeForTesting(options: ResolveChromeForTestingOptions = {}): { path?: string; reason?: string } {
  try {
    return { path: resolveChromeForTesting(options) };
  } catch (error) {
    return { reason: (error as Error).message };
  }
}

export interface LaunchChromeOptions extends ResolveChromeForTestingOptions {
  /** Default true (`--headless=new`). */
  headless?: boolean;
  /** Default true. */
  muteAudio?: boolean;
  /** Extra flags. The launcher owns `--user-data-dir` and `--remote-debugging-port`. */
  args?: string[];
  /** Temp profile directory prefix. */
  profilePrefix?: string;
  /** Initial URL. Default `about:blank`. */
  url?: string;
  /** DevTools endpoint wait. Default 20s. */
  startTimeoutMs?: number;
}

export type ChromeCloseStep = 'cdp' | 'sigterm' | 'sigkill' | 'already-exited';

export interface ChromeCloseReport {
  /** The step after which the whole browser process tree was gone. */
  step: ChromeCloseStep;
  exited: boolean;
  profileRemoved: boolean;
  /**
   * Pids of processes still carrying this profile's `--user-data-dir` after the close, from a
   * process-list scan (Windows, macOS and Linux). Empty when none remain. Reported only: close()
   * does not signal them on POSIX; on Windows they are forced before the report is built.
   */
  leftoverPids: number[];
  /**
   * Pids the profile scan listed right after the forced phase but not after the settle: they were
   * exiting. Reported so a caller can tell slow exit from a leak; not counted as leftover.
   */
  observedExitingPids: number[];
  durationMs: number;
}

export interface CloseChromeOptions {
  /** Upper bound for the post-force settle that waits for profile processes to exit. Default 4 s. */
  settleMs?: number;
  /** Per-step grace before escalating. Default 5000 ms. */
  graceMs?: number;
}

export interface ChromeForTestingBrowser {
  executablePath: string;
  process: ChildProcess;
  pid: number;
  /** Browser-level DevTools WebSocket URL. */
  wsEndpoint: string;
  port: number;
  /** Harness-owned temp profile, removed by {@link ChromeForTestingBrowser.close}. */
  profileDir: string;
  /** Graceful close; idempotent. */
  close(options?: CloseChromeOptions): Promise<ChromeCloseReport>;
}

const MANAGED_FLAGS = /^--(user-data-dir|remote-debugging-port|remote-debugging-pipe)(=|$)/;

/** Launch Chrome for Testing with a fresh temp profile and a DevTools endpoint. */
export async function launchChromeForTesting(options: LaunchChromeOptions = {}): Promise<ChromeForTestingBrowser> {
  const executablePath = resolveChromeForTesting(options);
  const extra = options.args ?? [];
  const managed = extra.find((arg) => MANAGED_FLAGS.test(arg));
  if (managed) throw new Error(`${managed} is managed by launchChromeForTesting`);
  const profileDir = mkdtempSync(path.join(tmpdir(), options.profilePrefix ?? 'rk-qa-chrome-'));
  const args = [
    ...(options.headless === false ? [] : ['--headless=new']),
    ...(options.muteAudio === false ? [] : ['--mute-audio']),
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-breakpad',
    '--password-store=basic',
    '--use-mock-keychain',
    ...extra,
    `--user-data-dir=${profileDir}`,
    '--remote-debugging-port=0',
    options.url ?? 'about:blank',
  ];
  // Own process group on Unix so the whole tree can be signalled together.
  const child = spawn(executablePath, args, {
    stdio: ['ignore', 'ignore', 'pipe'],
    detached: process.platform !== 'win32',
    windowsHide: true,
  });
  const pid = child.pid;
  if (!pid) {
    rmSync(profileDir, { recursive: true, force: true });
    throw new Error(`failed to start Chrome for Testing: ${executablePath}`);
  }
  let closing: Promise<ChromeCloseReport> | undefined;
  // A harness that exits without closing still asks Chrome to shut down gracefully.
  const onHostExit = () => signalTree(child, 'SIGTERM');
  process.once('exit', onHostExit);
  const close = (closeOptions: CloseChromeOptions = {}) => {
    process.removeListener('exit', onHostExit);
    closing ??= closeChromeGracefully({ child, wsEndpoint, profileDir, ...closeOptions });
    return closing;
  };
  let wsEndpoint: string | undefined;
  try {
    wsEndpoint = await new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Chrome for Testing did not expose a DevTools endpoint')), options.startTimeoutMs ?? 20_000);
      let buf = '';
      child.stderr!.setEncoding('utf8');
      // Keep draining after the endpoint line so Chrome never blocks on a full pipe.
      child.stderr!.on('data', (chunk: string) => {
        if (wsEndpoint) return;
        buf += chunk;
        const match = buf.match(/DevTools listening on (ws:\/\/\S+)/);
        if (match) {
          clearTimeout(timer);
          resolve(match[1]);
        }
      });
      child.once('exit', (code, signal) => {
        clearTimeout(timer);
        reject(new Error(`Chrome for Testing exited early (code ${code}, signal ${signal})\n${buf.slice(-2000)}`));
      });
      child.once('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
    });
  } catch (error) {
    await close();
    throw error;
  }
  return {
    executablePath,
    process: child,
    pid,
    wsEndpoint,
    port: Number(new URL(wsEndpoint).port),
    profileDir,
    close,
  };
}

function treeAlive(child: ChildProcess): boolean {
  const pid = child.pid;
  if (!pid) return false;
  if (process.platform === 'win32') return child.exitCode === null && child.signalCode === null;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}

async function waitGone(child: ChildProcess, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (treeAlive(child)) {
    if (Date.now() >= deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return true;
}

/** SIGKILL the tree, re-issuing the kill every 2 s until it is gone or `timeoutMs` passes. */
async function forceTreeGone(child: ChildProcess, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    signalTree(child, 'SIGKILL');
    if (await waitGone(child, Math.min(2_000, Math.max(0, deadline - Date.now())))) return true;
    if (Date.now() >= deadline) return false;
  }
}

function signalTree(child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL'): void {
  const pid = child.pid;
  if (!pid) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill.exe', ['/PID', String(pid), '/T', ...(signal === 'SIGKILL' ? ['/F'] : [])], { stdio: 'ignore', windowsHide: true });
    return;
  }
  try {
    process.kill(-pid, signal);
  } catch {
    try { child.kill(signal); } catch { /* gone */ }
  }
}

/**
 * Windows only: pids of every process whose command line carries `profileDir` (the Chrome
 * browser process and all its descendants, including orphans of an exited launcher). `[]` on
 * other platforms. The profile directory is unique per launch, so the match cannot hit
 * unrelated browsers.
 */
export function chromeProcessesForProfile(profileDir: string): number[] {
  if (process.platform !== 'win32') return posixProcessesForProfile(profileDir);
  const script = '$p = $env:RK_QA_PROFILE; Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.ProcessId -ne $PID -and $_.CommandLine.IndexOf($p, [StringComparison]::OrdinalIgnoreCase) -ge 0 } | ForEach-Object { $_.ProcessId }';
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    encoding: 'utf8',
    windowsHide: true,
    timeout: 15_000,
    env: { ...process.env, RK_QA_PROFILE: profileDir },
  });
  if (result.status !== 0) return [];
  return result.stdout
    .split(/\r?\n/)
    .map((line) => Number.parseInt(line.trim(), 10))
    .filter((pid) => Number.isInteger(pid) && pid > 0 && pid !== process.pid);
}

/**
 * POSIX scan: every process whose argv carries `--user-data-dir=<profileDir>` as a whole argument
 * (a sibling profile `<profileDir>2` does not match). `ps -o args=` works on macOS and Linux.
 */
function posixProcessesForProfile(profileDir: string): number[] {
  if (!profileDir) return [];
  const flag = `--user-data-dir=${profileDir}`;
  const result = spawnSync('ps', ['-axww', '-o', 'pid=', '-o', 'args='], { encoding: 'utf8', timeout: 15_000 });
  if (result.status !== 0 || typeof result.stdout !== 'string') return [];
  const pids: number[] = [];
  for (const line of result.stdout.split('\n')) {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match) continue;
    const pid = Number(match[1]);
    if (!Number.isInteger(pid) || pid <= 0 || pid === process.pid) continue;
    const command = match[2];
    for (let at = command.indexOf(flag); at >= 0; at = command.indexOf(flag, at + 1)) {
      const after = command[at + flag.length];
      if (after === undefined || after === ' ') { pids.push(pid); break; }
    }
  }
  return pids;
}

async function waitProfileProcessesGone(profileDir: string, timeoutMs: number): Promise<number[]> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const left = chromeProcessesForProfile(profileDir);
    if (left.length === 0 || Date.now() >= deadline) return left;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

async function sendBrowserClose(wsEndpoint: string, timeoutMs: number): Promise<void> {
  await new Promise<void>((resolve) => {
    let ws: WebSocket;
    const done = () => {
      clearTimeout(timer);
      try { ws.close(); } catch { /* closed */ }
      resolve();
    };
    const timer = setTimeout(done, timeoutMs);
    try {
      ws = new WebSocket(wsEndpoint);
    } catch {
      clearTimeout(timer);
      resolve();
      return;
    }
    ws.onopen = () => ws.send(JSON.stringify({ id: 1, method: 'Browser.close' }));
    ws.onmessage = done;
    ws.onclose = done;
    ws.onerror = done;
  });
}

/**
 * Close a Chrome process tree gracefully: CDP `Browser.close`, wait; SIGTERM the process
 * group, wait; SIGKILL only last. Then remove the harness-owned profile directory.
 */
export async function closeChromeGracefully(options: {
  child: ChildProcess;
  wsEndpoint?: string;
  profileDir?: string;
  graceMs?: number;
  /** Upper bound for the forced-kill phase (re-issued until the tree is gone). Default 15 s. */
  killTimeoutMs?: number;
  /** Upper bound for the post-force settle. Default 4 s. */
  settleMs?: number;
}): Promise<ChromeCloseReport> {
  const started = Date.now();
  const grace = options.graceMs ?? 5_000;
  const killBound = options.killTimeoutMs ?? 15_000;
  let step: ChromeCloseStep = 'already-exited';
  let exited = !treeAlive(options.child);
  if (!exited && options.wsEndpoint) {
    step = 'cdp';
    await sendBrowserClose(options.wsEndpoint, Math.min(grace, 2_000));
    exited = await waitGone(options.child, grace);
  }
  // Windows has no graceful signal for a windowless headless browser (`taskkill /T` without
  // `/F` is a no-op there), so that step is skipped rather than burning a grace period.
  if (!exited && process.platform !== 'win32') {
    step = 'sigterm';
    signalTree(options.child, 'SIGTERM');
    exited = await waitGone(options.child, grace);
  }
  if (!exited) {
    step = 'sigkill';
    exited = await forceTreeGone(options.child, killBound);
  }
  // Windows: Chrome's children outlive a dead launcher and `taskkill /T` can no longer find
  // them, but they still hold the profile directory. Give them a short grace, then force them.
  if (process.platform === 'win32' && options.profileDir) {
    let left = await waitProfileProcessesGone(options.profileDir, 1_500);
    // Keep forcing until the profile's processes are gone: under load a single
    // `taskkill /F` round can leave a hung renderer behind for several seconds.
    const deadline = Date.now() + killBound;
    while (left.length > 0 && Date.now() < deadline) {
      for (const pid of left) spawnSync('taskkill.exe', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      if (step !== 'sigkill') step = 'sigkill';
      left = await waitProfileProcessesGone(options.profileDir, Math.min(2_000, Math.max(0, deadline - Date.now())));
    }
    // The forced loop only decides when to stop forcing; whether the profile is clean is decided by the settle below.
  }
  let profileRemoved = true;
  if (options.profileDir) {
    try {
      rmSync(options.profileDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    } catch {
      // A held profile is reported through `profileRemoved`, never thrown from close().
    }
    profileRemoved = !existsSync(options.profileDir);
  }
  // Settle: processes that were still exiting at the last forced round get a bounded wait, then one
  // final scan decides. A pid still listed after the bound is a leftover; it never turns into a pass.
  let leftoverPids: number[] = [];
  let observedExitingPids: number[] = [];
  if (options.profileDir) {
    const profileDir = options.profileDir;
    const settled = await settleProfileProcesses({ list: () => chromeProcessesForProfile(profileDir), boundMs: options.settleMs ?? 4_000 });
    leftoverPids = settled.leftoverPids;
    observedExitingPids = settled.observedExitingPids;
  }
  exited = exited && leftoverPids.length === 0;
  return { step, exited, profileRemoved, leftoverPids, observedExitingPids, durationMs: Date.now() - started };
}

export interface ProfileSettleOptions {
  /** Lists the pids of the profile's processes now. */
  list: () => number[];
  /** Upper bound on waiting for listed pids to exit. Default 4 s. */
  boundMs?: number;
  /** Delay between scans. Default 250 ms. */
  pollMs?: number;
  /** Clock, injectable for tests. Default Date.now. */
  now?: () => number;
  /** Delay, injectable for tests. Default setTimeout. */
  sleep?: (ms: number) => Promise<void>;
}
export interface ProfileSettleResult {
  /** Pids still listed when the bound ended. Empty means the profile is clean. */
  leftoverPids: number[];
  /** Pids listed by the first scan that a later scan no longer listed: they exited during the settle. */
  observedExitingPids: number[];
  /** Scans performed, including the first. */
  scans: number;
}

/**
 * Bounded settle for a profile's process list: scan, and while anything is listed and the bound has
 * not passed, wait and scan again. Reports what is still listed (leftovers) apart from what was seen
 * exiting, so slow exit and a real leak are distinguishable. Pure apart from the injected list, clock
 * and delay.
 */
export async function settleProfileProcesses(options: ProfileSettleOptions): Promise<ProfileSettleResult> {
  const boundMs = options.boundMs ?? 4_000;
  const pollMs = options.pollMs ?? 250;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const deadline = now() + boundMs;
  let current = options.list();
  let scans = 1;
  const first = [...current];
  const exiting = new Set<number>();
  while (current.length > 0 && now() < deadline) {
    await sleep(Math.min(pollMs, Math.max(1, deadline - now())));
    current = options.list();
    scans++;
    for (const pid of first) if (!current.includes(pid)) exiting.add(pid);
  }
  return { leftoverPids: current, observedExitingPids: [...exiting], scans };
}

/** macOS per-user `X` directories that hold browser code-sign clones (deduplicated). */
export function codeSignCloneRoots(): string[] {
  if (process.platform !== 'darwin') return [];
  const temps = new Set<string>();
  const conf = spawnSync('getconf', ['DARWIN_USER_TEMP_DIR'], { encoding: 'utf8' });
  if (conf.status === 0 && conf.stdout.trim()) temps.add(path.resolve(conf.stdout.trim()));
  if (process.env.TMPDIR) temps.add(path.resolve(process.env.TMPDIR));
  return [...new Set([...temps].map((t) => path.join(path.dirname(t), 'X')))].filter((x) => {
    try { return statSync(x).isDirectory(); } catch { return false; }
  });
}

/** Every entry under every `*.code_sign_clone` directory (e.g. `com.google.Chrome.code_sign_clone`). */
export function codeSignCloneEntries(): string[] {
  const out: string[] = [];
  for (const root of codeSignCloneRoots()) {
    let names: string[] = [];
    try { names = readdirSync(root); } catch { continue; }
    for (const dir of names.filter((n) => n.endsWith('code_sign_clone'))) {
      try {
        for (const entry of readdirSync(path.join(root, dir))) out.push(path.join(root, dir, entry));
      } catch { /* unreadable */ }
    }
  }
  return out.sort();
}
