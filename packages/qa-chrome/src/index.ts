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
import { MANAGED_CURRENT_DIR, MANAGED_PREVIOUS_DIR, chromeForTestingPlatform, managedChromeForTestingRoot, managedExecutable, managedVersions } from './install.js';
import { sandboxAccessProblem } from './sandbox-access.js';

export * from './install.js';
export * from './sandbox-access.js';

/** Environment variable naming an explicit Chrome for Testing executable. */
export const CHROME_FOR_TESTING_ENV = 'RIGHTKIT_CHROME_FOR_TESTING';

export const CHROME_FOR_TESTING_INSTALL_HINT = [
  'Install the pinned Chrome for Testing at the RightKit-managed fixed path (<shared>/chrome-for-testing/current):',
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
 * and the environment variable): the RightKit-managed `current` folder, then `current.previous`,
 * then legacy per-version folders (newest first; see `rightkit-qa-chrome install`), then app installs, then Puppeteer's and Playwright's caches
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
  const managedRoot = managedChromeForTestingRoot({ env, platform, homedir: home });
  out.push(
    managedExecutable(path.join(managedRoot, MANAGED_CURRENT_DIR), key),
    managedExecutable(path.join(managedRoot, MANAGED_PREVIOUS_DIR), key),
  );
  // Legacy per-version folders from before the fixed layout.
  for (const v of managedVersions({ env, platform, homedir: home })) {
    out.push(managedExecutable(path.join(managedRoot, v), key));
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
  /**
   * Optional, present only when true. The close could not establish that the browser is gone: a liveness
   * query or the final profile scan failed or timed out, or the launcher's exit was never seen while the
   * profile scan listed nothing. `exited` is false whenever this is set.
   */
  livenessUnknown?: boolean;
}

export interface CloseChromeOptions {
  /** Upper bound for the post-force settle that waits for profile processes to exit. Default 4 s. */
  settleMs?: number;
  /** Per-step grace before escalating. Default 5000 ms. */
  graceMs?: number;
  /** Upper bound for retrying profile removal on EBUSY/EPERM (Windows). Default 10 s. */
  profileRemoveMs?: number;
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
  // Windows: a sandbox that cannot read its own browser crashes the network service at startup.
  // Refuse that launch with the folder and the fix instead of a browser whose navigations abort.
  if (!extra.includes('--no-sandbox')) {
    const unreadable = sandboxAccessProblem(executablePath);
    if (unreadable) throw unreadable;
  }
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

/** Liveness of the launched process from one OS query. `unknown` means the query failed or timed out. */
export type LauncherLiveness = 'alive' | 'gone' | 'unknown';

/**
 * OS probe for the launcher. POSIX: `kill(-pid, 0)` on its process group; ESRCH is gone, EPERM means the
 * group exists, any other error is unknown. Windows has no separate query: Node's exit bookkeeping decides
 * (see `observeLauncher`), so this reports `alive` until that records an exit.
 */
function launcherLiveness(child: ChildProcess): LauncherLiveness {
  const pid = child.pid;
  if (!pid) return 'gone';
  if (process.platform === 'win32') return 'alive';
  try {
    process.kill(-pid, 0);
    return 'alive';
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === 'ESRCH') return 'gone';
    if (code === 'EPERM') return 'alive';
    return 'unknown';
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Windows: force one profile leftover pid and its children. */
function killPidForced(pid: number): void {
  spawnSync('taskkill.exe', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
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

/** Last line of a Windows profile scan that ran to completion. Without it the output is a failed query. */
const SCAN_COMPLETE = 'RK_QA_SCAN_COMPLETE';

/**
 * Pids of every process whose command line carries `profileDir`: the Chrome browser process and all its
 * descendants, including orphans of an exited launcher. The profile directory is unique per launch, so the
 * match cannot hit unrelated browsers. Windows scans with PowerShell; macOS and Linux with `ps`.
 *
 * On Windows the path is matched as a whole path, in any case: it must be followed by a quote, white
 * space, a path separator or the end of the command line. A sibling profile `<profileDir>2` does not match,
 * while a child that names a file inside the profile (`--database=<profileDir>\Crashpad`) does.
 *
 * A scan that fails or times out returns `[]`, which means "unknown", not "none". `closeChromeGracefully`
 * never reads that as clean; it reports `livenessUnknown` instead.
 */
export function chromeProcessesForProfile(profileDir: string): number[] {
  return probeProfileProcesses(profileDir) ?? [];
}

/** The same scan with failure kept distinct: `null` when the query failed, exited non-zero, or timed out. */
function probeProfileProcesses(profileDir: string): number[] | null {
  if (!profileDir) return [];
  if (process.platform !== 'win32') return posixProcessesForProfile(profileDir);
  const script = `$ErrorActionPreference = 'Stop'; $p = $env:RK_QA_PROFILE; $q = [string][char]34; $r = [regex]::new([regex]::Escape($p) + '(?![^' + $q + '\\s\\\\/])', 'IgnoreCase'); Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.ProcessId -ne $PID -and $r.IsMatch($_.CommandLine) } | ForEach-Object { $_.ProcessId }; '${SCAN_COMPLETE}'`;
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    encoding: 'utf8',
    windowsHide: true,
    timeout: 15_000,
    env: { ...process.env, RK_QA_PROFILE: profileDir },
  });
  if (result.status !== 0 || typeof result.stdout !== 'string') return null;
  const lines = result.stdout.split(/\r?\n/).map((line) => line.trim());
  if (!lines.includes(SCAN_COMPLETE)) return null;
  return lines
    .map((line) => Number.parseInt(line, 10))
    .filter((pid) => Number.isInteger(pid) && pid > 0 && pid !== process.pid);
}

/**
 * POSIX scan: every process whose argv carries `--user-data-dir=<profileDir>` as a whole argument (a sibling
 * profile `<profileDir>2` does not match). `ps -o args=` works on macOS and Linux. `null` when `ps` failed.
 */
function posixProcessesForProfile(profileDir: string): number[] | null {
  const flag = `--user-data-dir=${profileDir}`;
  const result = spawnSync('ps', ['-axww', '-o', 'pid=', '-o', 'args='], { encoding: 'utf8', timeout: 15_000 });
  if (result.status !== 0 || typeof result.stdout !== 'string') return null;
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
 * Test seams for {@link closeChromeGracefully}. Every field defaults to the real implementation, so
 * production callers never set this.
 */
export interface CloseSeams {
  /** Platform. Default `process.platform`. */
  platform?: NodeJS.Platform;
  /** OS probe for the launcher. Asked only while Node has not recorded the launcher's exit. */
  launcherLiveness?: (child: ChildProcess) => LauncherLiveness;
  /** Signals the browser tree: `taskkill /T` on Windows, the process group on POSIX. */
  signalTree?: (child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL') => void;
  /** Force-kills one profile leftover pid (Windows). */
  killPid?: (pid: number) => void;
  /** Profile scan. `null` when the query failed or timed out. */
  scanProfile?: (profileDir: string) => number[] | null;
  /** Removes the profile directory; may throw like `rmSync`. */
  removeDir?: (dir: string) => void;
  /** Whether a path exists. Default `existsSync`. */
  exists?: (dir: string) => boolean;
  /** Clock. Default `Date.now`. */
  now?: () => number;
  /** Delay. Default `setTimeout`. */
  sleep?: (ms: number) => Promise<void>;
}

interface CloseDriver {
  platform: NodeJS.Platform;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
  launcher: (child: ChildProcess) => LauncherLiveness;
  signal: (child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL') => void;
  killPid: (pid: number) => void;
  scan: (profileDir: string) => number[] | null;
  removeDir: (dir: string) => void;
  exists: (dir: string) => boolean;
}

function closeDriver(seams: CloseSeams = {}): CloseDriver {
  return {
    platform: seams.platform ?? process.platform,
    now: seams.now ?? Date.now,
    sleep: seams.sleep ?? delay,
    launcher: seams.launcherLiveness ?? launcherLiveness,
    signal: seams.signalTree ?? signalTree,
    killPid: seams.killPid ?? killPidForced,
    scan: seams.scanProfile ?? probeProfileProcesses,
    removeDir: seams.removeDir ?? ((dir) => rmSync(dir, { recursive: true, force: true })),
    exists: seams.exists ?? existsSync,
  };
}

/** Error codes that mean "a handle is still open, try again" (Windows holds a profile briefly after exit). */
const RETRYABLE_REMOVE_CODES = new Set(['EBUSY', 'EPERM', 'EACCES', 'ENOTEMPTY', 'EMFILE']);

/**
 * Remove the harness-owned profile, retrying EBUSY/EPERM-class failures until `boundMs` passes. Between
 * attempts on Windows, any process still carrying the profile is forced again. A non-retryable error stops
 * the loop. Never throws: the result is whether the directory is gone.
 */
async function removeProfileDir(d: CloseDriver, profileDir: string, boundMs: number): Promise<boolean> {
  const deadline = d.now() + boundMs;
  for (;;) {
    try {
      d.removeDir(profileDir);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | undefined)?.code;
      if (code === undefined || !RETRYABLE_REMOVE_CODES.has(code)) return !d.exists(profileDir);
    }
    if (!d.exists(profileDir)) return true;
    if (d.now() >= deadline) return false;
    if (d.platform === 'win32') for (const pid of scanOf(d, profileDir) ?? []) d.killPid(pid);
    await d.sleep(Math.min(250, Math.max(1, deadline - d.now())));
  }
}

/** A scan that throws is a failed query. */
function scanOf(d: CloseDriver, profileDir: string): number[] | null {
  try {
    return d.scan(profileDir);
  } catch {
    return null;
  }
}

/**
 * One fresh observation of the launcher. The loop turn comes first: Node records a child's exit only when
 * the event loop delivers it, so a read made before a turn can be stale after a blocking `spawnSync` kill.
 * Node's exit bookkeeping decides when it has recorded an exit; otherwise the OS probe is asked, and a probe
 * that throws is `unknown`.
 */
async function observeLauncher(d: CloseDriver, child: ChildProcess): Promise<LauncherLiveness> {
  await d.sleep(0);
  if (typeof child.exitCode === 'number' || typeof child.signalCode === 'string') return 'gone';
  try {
    return d.launcher(child);
  } catch {
    return 'unknown';
  }
}

/** The launcher is gone and the profile scan is known to list nothing. */
async function isSettled(d: CloseDriver, child: ChildProcess, profileDir: string | undefined): Promise<boolean> {
  if ((await observeLauncher(d, child)) !== 'gone') return false;
  if (!profileDir) return true;
  return scanOf(d, profileDir)?.length === 0;
}

/** Waits up to `ms` for {@link isSettled}. The first check is already fresh, so `ms = 0` still observes once. */
async function waitSettled(d: CloseDriver, child: ChildProcess, profileDir: string | undefined, ms: number): Promise<boolean> {
  const deadline = d.now() + ms;
  for (;;) {
    if (await isSettled(d, child, profileDir)) return true;
    const left = deadline - d.now();
    if (left <= 0) return false;
    await d.sleep(Math.min(100, left));
  }
}

/**
 * SIGKILL the tree, and on Windows each listed profile pid, re-issuing every 2 s until settled or `killBound`
 * passes. Returns at once when the launcher is gone and the profile scan is clean.
 */
async function forceSettled(d: CloseDriver, child: ChildProcess, profileDir: string | undefined, killBound: number): Promise<boolean> {
  const deadline = d.now() + killBound;
  for (;;) {
    const launcher = await observeLauncher(d, child);
    const scan = profileDir ? scanOf(d, profileDir) : [];
    if (launcher === 'gone' && scan?.length === 0) return true;
    d.signal(child, 'SIGKILL');
    if (d.platform === 'win32') for (const pid of scan ?? []) d.killPid(pid);
    const left = Math.min(2_000, Math.max(0, deadline - d.now()));
    if (await waitSettled(d, child, profileDir, left)) return true;
    if (d.now() >= deadline) return false;
  }
}

/**
 * Close a Chrome process tree gracefully: CDP `Browser.close`, wait; SIGTERM the process group, wait; SIGKILL
 * only last. Then remove the harness-owned profile directory and settle the profile's process list.
 *
 * `exited` is true only when Node has observed the launcher's exit (or the OS probe reports it gone) and the
 * profile scan lists nothing. A failed scan or probe is retried inside the bound and otherwise reported as
 * `livenessUnknown`; it is never read as clean or as alive.
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
  /** Upper bound for retrying profile removal on EBUSY/EPERM. Default 10 s. */
  profileRemoveMs?: number;
  /** Test seams. Every field defaults to the real implementation. */
  seams?: CloseSeams;
}): Promise<ChromeCloseReport> {
  const d = closeDriver(options.seams);
  const { child, profileDir } = options;
  const started = d.now();
  const grace = options.graceMs ?? 5_000;
  const killBound = options.killTimeoutMs ?? 15_000;
  let step: ChromeCloseStep = 'already-exited';
  let settled = await isSettled(d, child, profileDir);
  if (!settled && options.wsEndpoint) {
    step = 'cdp';
    await sendBrowserClose(options.wsEndpoint, Math.min(grace, 2_000));
    settled = await waitSettled(d, child, profileDir, grace);
  }
  // Windows has no graceful signal for a windowless headless browser (`taskkill /T` without
  // `/F` is a no-op there), so that step is skipped rather than burning a grace period.
  if (!settled && d.platform !== 'win32') {
    step = 'sigterm';
    d.signal(child, 'SIGTERM');
    settled = await waitSettled(d, child, profileDir, grace);
  }
  if (!settled) {
    step = 'sigkill';
    // Windows: Chrome's children outlive a dead launcher and `taskkill /T` cannot find them, so each listed
    // profile pid is forced too. Under load one round can leave a hung renderer, so this keeps re-issuing.
    await forceSettled(d, child, profileDir, killBound);
  }
  // Settle: pids still exiting get a bounded wait, then the last scan decides. A pid still listed after the
  // bound is a leftover. A scan that never answers is unknown, not an empty profile.
  let leftoverPids: number[] = [];
  let observedExitingPids: number[] = [];
  let scanUnknown = false;
  if (profileDir) {
    const settle = await settleProfileProcesses({
      list: () => scanOf(d, profileDir),
      boundMs: options.settleMs ?? 4_000,
      now: d.now,
      sleep: d.sleep,
    });
    leftoverPids = settle.leftoverPids;
    observedExitingPids = settle.observedExitingPids;
    scanUnknown = settle.unknown === true;
  }
  // The profile is removed after the settle, not before: on Windows a dying Chrome tree keeps handles open
  // in the profile for a while, and one early delete attempt fails with EBUSY/EPERM. Removal retries inside
  // its own bound and re-forces any pid still listed between attempts.
  let profileRemoved = true;
  if (profileDir) {
    profileRemoved = await removeProfileDir(d, profileDir, options.profileRemoveMs ?? 10_000);
  }
  // One more fresh look at the launcher decides the verdict.
  const launcher = await observeLauncher(d, child);
  const exited = launcher === 'gone' && leftoverPids.length === 0 && !scanUnknown;
  const report: ChromeCloseReport = { step, exited, profileRemoved, leftoverPids, observedExitingPids, durationMs: d.now() - started };
  if (!exited && (leftoverPids.length === 0 || scanUnknown)) report.livenessUnknown = true;
  return report;
}

export interface ProfileSettleOptions {
  /** Lists the pids of the profile's processes now. `null` when the query failed or timed out. */
  list: () => number[] | null;
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
  /** Present only when true: the last scan failed, so the profile may still hold processes unseen. */
  unknown?: boolean;
}

/**
 * Bounded settle for a profile's process list: scan, and while anything is listed (or a scan failed) and the
 * bound has not passed, wait and scan again. Reports what is still listed (leftovers) apart from what was seen
 * exiting, so slow exit and a real leak are distinguishable. A failed scan is retried inside the bound; if the
 * last one still fails, the result is `unknown`. Pure apart from the injected list, clock and delay.
 */
export async function settleProfileProcesses(options: ProfileSettleOptions): Promise<ProfileSettleResult> {
  const boundMs = options.boundMs ?? 4_000;
  const pollMs = options.pollMs ?? 250;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const deadline = now() + boundMs;
  let current: number[] | null = options.list();
  let lastKnown: number[] = current ?? [];
  let scans = 1;
  const first = [...lastKnown];
  const exiting = new Set<number>();
  while ((current === null || current.length > 0) && now() < deadline) {
    await sleep(Math.min(pollMs, Math.max(1, deadline - now())));
    current = options.list();
    scans++;
    if (current === null) continue;
    for (const pid of first) if (!current.includes(pid)) exiting.add(pid);
    lastKnown = current;
  }
  const result: ProfileSettleResult = { leftoverPids: lastKnown, observedExitingPids: [...exiting], scans };
  if (current === null) result.unknown = true;
  return result;
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
