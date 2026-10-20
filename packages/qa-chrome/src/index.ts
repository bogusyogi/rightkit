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
import { MANAGED_CURRENT_DIR, MANAGED_PREVIOUS_DIR, chromeForTestingPlatform, managedChromeForTestingRoot, managedExecutable, managedHeadlessShellExecutable, managedVersions } from './install.js';
import { REMOVER_STOP_WAIT_MS, REMOVE_ABORTED_CODE, removeDirInChild } from './remove-dir.js';
import { sandboxAccessProblem } from './sandbox-access.js';

export { REMOVE_ABORTED_CODE, removeDirInChild };

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

/** Environment variable naming an explicit `chrome-headless-shell` executable (the same name rightkit-browser reads). */
export const CHROME_HEADLESS_SHELL_ENV = 'RIGHTKIT_CHROME_HEADLESS_SHELL';

/**
 * `chrome-headless-shell` locations in resolution order: the managed `current`, then `current.previous`,
 * then legacy per-version folders (newest first). Only the managed fixed path has a headless shell installed.
 */
export function chromeHeadlessShellCandidates(options: ResolveChromeForTestingOptions = {}): string[] {
  const env = options.env ?? process.env;
  const platform = options.platform ?? process.platform;
  const home = options.homedir ?? homedir();
  const key = chromeForTestingPlatform(platform, options.arch ?? process.arch);
  const managedRoot = managedChromeForTestingRoot({ env, platform, homedir: home });
  return [
    managedHeadlessShellExecutable(path.join(managedRoot, MANAGED_CURRENT_DIR), key),
    managedHeadlessShellExecutable(path.join(managedRoot, MANAGED_PREVIOUS_DIR), key),
    ...managedVersions({ env, platform, homedir: home }).map((v) => managedHeadlessShellExecutable(path.join(managedRoot, v), key)),
  ];
}

/**
 * Resolve the `chrome-headless-shell` executable installed by `rightkit-qa-chrome install` (or rightkit-browser):
 * `executablePath`, then `RIGHTKIT_CHROME_HEADLESS_SHELL`, then {@link chromeHeadlessShellCandidates}.
 * Throws {@link ChromeForTestingNotFoundError} when none exists.
 */
export function resolveChromeHeadlessShell(options: ResolveChromeForTestingOptions = {}): string {
  const env = options.env ?? process.env;
  if (options.executablePath) {
    if (existsSync(options.executablePath)) return options.executablePath;
    throw new ChromeForTestingNotFoundError(`explicit chrome-headless-shell executable does not exist: ${options.executablePath}`, [options.executablePath]);
  }
  const fromEnv = env[CHROME_HEADLESS_SHELL_ENV];
  if (fromEnv) {
    if (existsSync(fromEnv)) return fromEnv;
    throw new ChromeForTestingNotFoundError(`${CHROME_HEADLESS_SHELL_ENV} points at a missing file: ${fromEnv}`, [fromEnv]);
  }
  const candidates = chromeHeadlessShellCandidates(options);
  const found = candidates.find((candidate) => existsSync(candidate));
  if (found) return found;
  throw new ChromeForTestingNotFoundError('chrome-headless-shell is not installed', candidates);
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

export type ProfileRemoval = 'removed' | 'not-attempted' | 'failed' | 'timed-out-terminated' | 'timed-out-running';

export type ChromeCloseStep = 'cdp' | 'sigterm' | 'sigkill' | 'already-exited' | 'not-attempted';

export interface ChromeCloseReport {
  /**
   * Last shutdown phase that issued something (`cdp`, `sigterm`, `sigkill`); otherwise `already-exited` when the
   * launcher was gone before any step, or `not-attempted`. A phase that had nothing to issue (POSIX with the
   * launcher gone and no member of its group left, a scan that failed) does not change it.
   */
  step: ChromeCloseStep;
  exited: boolean;
  /** True only when the profile directory was verified gone (or there was no harness profile). */
  profileRemoved: boolean;
  /**
   * What happened to the profile removal, exactly: `removed` (verified gone), `not-attempted` (no time was left
   * for it, `profileRemoveMs: 0`, or no harness profile), `failed` (the removal ended in an error or ran out of
   * retries, the directory remains), `timed-out-terminated` (the bound was spent, the remover was stopped and
   * its exit confirmed, the directory may be partly removed), `timed-out-running` (the bound was spent and the
   * remover's exit could not be confirmed inside the budget: it may still be running).
   */
  profileRemoval: ProfileRemoval;
  /**
   * Pids of processes still carrying this profile's `--user-data-dir` after the close, from a
   * process-list scan (Windows, macOS and Linux). Empty when none remain. Before the report is built, Windows
   * forces them by native identity (pid plus creation time). POSIX signals only while the launcher remains owned;
   * processes surviving an exited launcher remain diagnostic leftovers.
   * A Windows row whose identity could not be verified is not listed; it sets `livenessUnknown` instead.
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
   * query or the final profile scan failed or timed out, the final scan listed a process whose identity could
   * not be verified (Windows), or the launcher's exit was never seen while the profile scan listed nothing.
   * `exited` is false whenever this is set.
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
  /**
   * Total time `close()` may take, measured from its start. Every step is bounded by `min(its own bound,
   * what remains)`; once spent, close goes straight to a final fresh observation and returns. The per-step
   * options above stay as upper bounds. Default 60 s.
   */
  budgetMs?: number;
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
  // signalTree does nothing once Node has recorded the launcher's exit (its saved pid or group may be reused).
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
 * group exists, any other error is unknown. Windows: `kill(pid, 0)` on the launcher itself, which libuv's open
 * handle pins to our process until Node records the exit; ESRCH is gone. Node's exit bookkeeping is consulted
 * first either way (see `observeLauncher`).
 */
function launcherLiveness(child: ChildProcess): LauncherLiveness {
  const pid = child.pid;
  if (!pid) return 'gone';
  // Windows: Node can miss a force-killed Chrome launcher's exit (no 'exit' event, exitCode stays null) while
  // the OS has it terminated (rightkit#811), so ask the OS. libuv still holds the launcher's process handle
  // until Node records the exit, so the pid cannot be reused and this probe names our launcher.
  try {
    process.kill(process.platform === 'win32' ? pid : -pid, 0);
    return 'alive';
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === 'ESRCH') return 'gone';
    if (code === 'EPERM') return 'alive';
    return 'unknown';
  }
}

/** Monotonic milliseconds for every close deadline: wall-clock changes cannot stretch or cut a bound. */
const monotonicNow = (): number => performance.now();

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Caps for one external call. Inside a close every call is also bounded by the remaining close budget
 * (`min(cap, remaining)`), so these only apply when a call has no budget of its own.
 */
const TASKKILL_CAP_MS = 10_000;
const SCAN_CAP_MS = 15_000;

/**
 * A process the profile scan listed. On Windows `creation` is the process's full 100 ns creation FILETIME
 * (decimal string), read through a native handle that stayed open while the scan refreshed CIM ownership
 * metadata; the kill reopens the pid and kills only if the same FILETIME is read from the handle it will terminate,
 * so a pid reused by another process is never killed. A Windows row whose identity could not be established
 * (access denied, a CIM row that vanished or changed owner mid-check) carries `unknown: true` and no creation
 * time: it is never killed, never read as clean, and makes the close report `livenessUnknown`. POSIX entries
 * carry no creation time and never authorize post-launcher-exit signaling.
 */
export interface ProfileProcess {
  pid: number;
  creation?: string;
  /** Windows only, present only when true: listed by CIM for this profile, but its identity could not be verified. */
  unknown?: true;
}

/**
 * Declare native entry points in memory: no Add-Type compiler process or temporary assembly is needed.
 * Profile discovery opens a minimal-rights handle before refreshing CIM ownership metadata. Keeping that
 * handle open prevents PID reuse during the query; GetProcessTimes supplies the exact native birth token.
 * Probe compares caller-supplied tokens exactly. Kill reopens the pid, compares that token again and
 * terminates through that same handle only. PID lifetime: https://devblogs.microsoft.com/oldnewthing/20110107-00/?p=11803/
 */
const WINDOWS_PROCESS_PRELUDE = `$ErrorActionPreference = 'Stop';
$assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly([Reflection.AssemblyName]::new('RkNative'), [Reflection.Emit.AssemblyBuilderAccess]::Run);
$module = $assembly.DefineDynamicModule('RkNative');
$builder = $module.DefineType('RkNative', [Reflection.TypeAttributes]'Public, Sealed, Abstract');
function RkImport($name, [Type]$result, [Type[]]$parameters) {
  $method = $builder.DefineMethod($name, [Reflection.MethodAttributes]'Public, Static, PinvokeImpl', $result, $parameters);
  $ctor = [Runtime.InteropServices.DllImportAttribute].GetConstructor([Type[]]@([string]));
  $fields = [Reflection.FieldInfo[]]@([Runtime.InteropServices.DllImportAttribute].GetField('SetLastError'));
  $attribute = [Reflection.Emit.CustomAttributeBuilder]::new($ctor, [object[]]@('kernel32.dll'), $fields, [object[]]@($true));
  $method.SetCustomAttribute($attribute);
  $method.SetImplementationFlags([Reflection.MethodImplAttributes]::PreserveSig);
}
RkImport 'OpenProcess' ([IntPtr]) ([Type[]]@([uint32], [bool], [uint32]));
RkImport 'GetProcessTimes' ([bool]) ([Type[]]@([IntPtr], [long].MakeByRefType(), [long].MakeByRefType(), [long].MakeByRefType(), [long].MakeByRefType()));
RkImport 'WaitForSingleObject' ([uint32]) ([Type[]]@([IntPtr], [uint32]));
RkImport 'TerminateProcess' ([bool]) ([Type[]]@([IntPtr], [uint32]));
RkImport 'CloseHandle' ([bool]) ([Type[]]@([IntPtr]));
$native = $builder.CreateType();
function RkProbe([uint32]$id, [long]$creation) {
  $h = $native::OpenProcess(0x101000, $false, $id);
  if ($h -eq [IntPtr]::Zero) { if ([Runtime.InteropServices.Marshal]::GetLastWin32Error() -eq 87) { return 'G' }; return 'U' };
  try {
    $w = $native::WaitForSingleObject($h, 0);
    if ($w -eq 0) { return 'G' }; if ($w -ne 258) { return 'U' };
    [long]$c = 0; [long]$e = 0; [long]$k = 0; [long]$u = 0;
    if (-not $native::GetProcessTimes($h, [ref]$c, [ref]$e, [ref]$k, [ref]$u)) { return 'U' };
    if ($creation -lt 0 -or $c -ne $creation) { return 'U' };
    return 'L' + [string]$c;
  } finally { [void]$native::CloseHandle($h) }
}
function RkProfile([uint32]$id, $rx) {
  $h = $native::OpenProcess(0x101000, $false, $id);
  if ($h -eq [IntPtr]::Zero) { if ([Runtime.InteropServices.Marshal]::GetLastWin32Error() -eq 87) { return 'G' }; return 'U' };
  try {
    $w = $native::WaitForSingleObject($h, 0);
    if ($w -eq 0) { return 'G' }; if ($w -ne 258) { return 'U' };
    [long]$birth = 0; [long]$e = 0; [long]$k = 0; [long]$u = 0;
    if (-not $native::GetProcessTimes($h, [ref]$birth, [ref]$e, [ref]$k, [ref]$u)) { return 'U' };
    $row = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + [string]$id);
    $w = $native::WaitForSingleObject($h, 0);
    if ($w -eq 0) { return 'G' }; if ($w -ne 258) { return 'U' };
    [long]$again = 0;
    if (-not $native::GetProcessTimes($h, [ref]$again, [ref]$e, [ref]$k, [ref]$u) -or $again -ne $birth) { return 'U' };
    if ($null -eq $row -or [string]::IsNullOrEmpty($row.CommandLine)) { return 'U' };
    if (-not $rx.IsMatch($row.CommandLine)) { return 'G' };
    return 'L' + [string]$birth;
  } finally { [void]$native::CloseHandle($h) }
}
function RkKill([uint32]$id, [long]$creation) {
  $h = $native::OpenProcess(0x1001, $false, $id);
  if ($h -eq [IntPtr]::Zero) { return };
  try {
    [long]$c = 0; [long]$e = 0; [long]$k = 0; [long]$u = 0;
    if ($native::GetProcessTimes($h, [ref]$c, [ref]$e, [ref]$k, [ref]$u) -and $c -eq $creation) { [void]$native::TerminateProcess($h, 1) };
  } finally { [void]$native::CloseHandle($h) }
}
function RkEmit($id, $ft) {
  try { $r = RkProbe ([uint32]$id) ([long]$ft) } catch { $r = 'U' };
  if ($r.StartsWith('L')) { [string]$id + '|' + $r.Substring(1) } elseif ($r -ne 'G') { 'UNKNOWN|' + [string]$id }
}; `;

/**
 * Whole milliseconds an external call may take, or 0 when it must not be started. `spawnSync` treats a
 * timeout of 0 as no timeout at all, so a call whose floored time is not positive is never spawned.
 */
function spawnTimeout(ms: number, cap: number): number {
  const t = Math.floor(Math.min(ms, cap));
  return t > 0 ? t : 0;
}

/** Runs one PowerShell script with in-memory native declarations. `null` when it could not start, timed out or failed. */
function runWindowsPowerShell(body: string, env: Record<string, string>, timeoutMs: number): string | null {
  if (!(timeoutMs > 0)) return null;
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', WINDOWS_PROCESS_PRELUDE + body], {
    encoding: 'utf8',
    windowsHide: true,
    timeout: timeoutMs,
    env: { ...process.env, ...env },
  });
  if (result.status !== 0 || typeof result.stdout !== 'string') return null;
  return result.stdout;
}

/**
 * Force the descendants a fresh scan listed.
 *
 * Windows: by native identity. One PowerShell call for the whole batch: each pid is opened with a native
 * handle, its creation FILETIME is re-read on that same handle, and only an exact match with the scanned
 * FILETIME is terminated through that handle. A mismatch (another process now owns the pid), a gone pid, or
 * any error skips the process. Entries without a creation time (`unknown` rows included) are never killed.
 * Bounded by `timeoutMs` (whole ms, capped); with no positive time left nothing is spawned.
 * POSIX scans authorize no signals: snapshots cannot reserve a PID or process group against reuse.
 */
export function killVerifiedProcesses(procs: readonly ProfileProcess[], timeoutMs: number): void {
  if (process.platform !== 'win32') return;
  const timeout = spawnTimeout(timeoutMs, TASKKILL_CAP_MS);
  if (timeout <= 0) return;
  const batch = procs
    .filter((p) => Number.isInteger(p.pid) && p.pid > 0 && typeof p.creation === 'string' && /^\d+$/.test(p.creation))
    .map((p) => `${p.pid}|${p.creation}`);
  if (batch.length === 0) return;
  const body = `foreach ($e in $env:RK_QA_KILL.Split(';')) { $f = $e.Split('|'); try { RkKill ([uint32]$f[0]) ([long]$f[1]) } catch { } }`;
  runWindowsPowerShell(body, { RK_QA_KILL: batch.join(';') }, timeout);
}

/** True once Node has recorded the launcher's exit. Node records it on an event-loop turn, never in the middle of synchronous code. */
function launcherExitRecorded(child: ChildProcess): boolean {
  return typeof child.exitCode === 'number' || typeof child.signalCode === 'string';
}

/**
 * Signal the launcher's tree. Does nothing once Node has recorded the launcher's exit: after the group root
 * is gone its saved pid or process group may belong to someone else. Windows: SIGKILL goes through the
 * ChildProcess only (Node holds the launcher's handle); descendants are killed by identity with
 * {@link killVerifiedProcesses}. Windows SIGTERM (host exit only) is `taskkill /T`. POSIX: the process group.
 */
export function signalTree(child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL', timeoutMs: number = TASKKILL_CAP_MS): void {
  const pid = child.pid;
  if (!pid || launcherExitRecorded(child)) return;
  if (process.platform === 'win32') {
    if (signal === 'SIGKILL') {
      try { child.kill('SIGKILL'); } catch { /* gone */ }
      return;
    }
    const timeout = spawnTimeout(timeoutMs, TASKKILL_CAP_MS);
    if (timeout > 0) spawnSync('taskkill.exe', ['/PID', String(pid), '/T'], { stdio: 'ignore', windowsHide: true, timeout });
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
/** Prefix of a line naming a pid whose identity could not be established (access denied, stale CIM row, ...). */
const SCAN_UNKNOWN = 'UNKNOWN|';

/**
 * Parse Windows scan output: `pid|creationFiletime` per live, identity-checked process, `UNKNOWN|pid` for a
 * process whose identity could not be established, then the completion marker. `null` (a failed scan) only
 * when the marker is missing. An unknown pid is kept as an `unknown` entry beside the verified ones: the
 * verified processes can still be forced by identity, while the unknown one is never killed and never read as
 * clean (the close reports `livenessUnknown` for it). One unverifiable row no longer hides every verified one.
 */
export function parseProfileScan(stdout: string): ProfileProcess[] | null {
  const lines = stdout.split(/\r?\n/).map((line) => line.trim());
  if (!lines.includes(SCAN_COMPLETE)) return null;
  const procs: ProfileProcess[] = [];
  for (const line of lines) {
    if (line.startsWith(SCAN_UNKNOWN)) {
      const unknownPid = Number(line.slice(SCAN_UNKNOWN.length));
      if (Number.isInteger(unknownPid) && unknownPid > 0 && unknownPid !== process.pid) procs.push({ pid: unknownPid, unknown: true });
      continue;
    }
    const match = /^(\d+)\|(\d+)$/.exec(line);
    if (!match) continue;
    const pid = Number(match[1]);
    if (Number.isInteger(pid) && pid > 0 && pid !== process.pid) procs.push({ pid, creation: match[2] });
  }
  return procs;
}

/**
 * Pids of every process whose command line carries `profileDir`: the Chrome browser process and all its
 * descendants, including orphans of an exited launcher. The profile directory is unique per launch, so the
 * match cannot hit unrelated browsers. Windows scans with PowerShell; macOS and Linux with `ps`.
 *
 * On Windows the path is matched as a whole path, in any case: it must be followed by a quote, white
 * space, a path separator or the end of the command line. A sibling profile `<profileDir>2` does not match,
 * while a child that names a file inside the profile (`--database=<profileDir>\Crashpad`) does.
 *
 * On Windows every matching CIM row is checked against a native process handle (rightkit#811): a process whose
 * handle is signalled has exited and is skipped, even while `Win32_Process` still lists it. A live process
 * is checked again with a fresh CIM query while that handle pins its PID. The full native creation
 * FILETIME is read from that same handle before and after the query, avoiding CIM timestamp precision loss.
 * A failed ownership query or native check makes the scan unknown; changed ownership is skipped.
 *
 * This convenience cannot tell "none" from "unknown": a scan that fails or times out also returns `[]`, and a
 * Windows row whose identity could not be verified is listed by pid like any other. Callers that need the
 * distinction use {@link probeProfileProcesses}, which returns `null` for a failed scan and marks unverified
 * rows `unknown`. `closeChromeGracefully` uses that form and never reads a failed scan as clean.
 */
export function chromeProcessesForProfile(profileDir: string): number[] {
  return (probeProfileProcesses(profileDir) ?? []).map((p) => p.pid);
}

/**
 * Identity check for caller-supplied native tokens. Each row is a pid and its exact native creation
 * FILETIME (100 ns, decimal string); the historical field name `cimFileTime` is retained. Returns the live
 * rows with their creation time; `null` when any row does not match the live process, cannot be opened, or the
 * query failed. Exported so replacement identity can be exercised without requiring actual PID reuse.
 */
export function probeProcessIdentities(rows: ReadonlyArray<{ pid: number; cimFileTime: string }>, timeoutMs: number = SCAN_CAP_MS): ProfileProcess[] | null {
  const timeout = spawnTimeout(timeoutMs, SCAN_CAP_MS);
  if (timeout <= 0 || process.platform !== 'win32') return null;
  const batch = rows
    .filter((r) => Number.isInteger(r.pid) && r.pid > 0 && /^\d+$/.test(r.cimFileTime))
    .map((r) => `${r.pid}|${r.cimFileTime}`);
  if (batch.length === 0) return [];
  const out = runWindowsPowerShell(`foreach ($e in $env:RK_QA_ROWS.Split(';')) { $f = $e.Split('|'); RkEmit $f[0] $f[1] }; '${SCAN_COMPLETE}'`, { RK_QA_ROWS: batch.join(';') }, timeout);
  const verifiedRows = out === null ? null : parseProfileScan(out);
  // This identity check vouches for every caller-supplied row or for none: one unverifiable row fails the batch.
  return verifiedRows === null || verifiedRows.some((p) => p.unknown) ? null : verifiedRows;
}

/**
 * The same scan with failure kept distinct: `null` when the query failed, exited non-zero, timed out, or had no
 * positive time to start. A Windows pid whose identity could not be verified is an `unknown` entry, not a failure.
 */
export function probeProfileProcesses(profileDir: string, timeoutMs: number = SCAN_CAP_MS): ProfileProcess[] | null {
  if (!profileDir) return [];
  const timeout = spawnTimeout(timeoutMs, SCAN_CAP_MS);
  if (timeout <= 0) return null;
  if (process.platform !== 'win32') return posixProcessesForProfile(profileDir, timeout);
  const body = `$p = $env:RK_QA_PROFILE; $q = [string][char]34; $rx = [regex]::new([regex]::Escape($p) + '(?![^' + $q + '\\s\\\\/])', 'IgnoreCase'); Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.ProcessId -ne $PID -and $rx.IsMatch($_.CommandLine) } | ForEach-Object { try { $r = RkProfile ([uint32]$_.ProcessId) $rx } catch { $r = 'U' }; if ($r.StartsWith('L')) { [string]$_.ProcessId + '|' + $r.Substring(1) } elseif ($r -ne 'G') { 'UNKNOWN|' + [string]$_.ProcessId } }; '${SCAN_COMPLETE}'`;
  const out = runWindowsPowerShell(body, { RK_QA_PROFILE: profileDir }, timeout);
  return out === null ? null : parseProfileScan(out);
}

/**
 * POSIX scan: every process whose argv carries `--user-data-dir=<profileDir>` as a whole argument (a sibling
 * profile `<profileDir>2` does not match). `ps -o args=` works on macOS and Linux. `null` when `ps` failed.
 */
function posixProcessesForProfile(profileDir: string, timeoutMs: number): ProfileProcess[] | null {
  const flag = `--user-data-dir=${profileDir}`;
  const result = spawnSync('ps', ['-axww', '-o', 'pid=', '-o', 'args='], { encoding: 'utf8', timeout: timeoutMs });
  if (result.status !== 0 || typeof result.stdout !== 'string') return null;
  const procs: ProfileProcess[] = [];
  for (const line of result.stdout.split('\n')) {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match) continue;
    const pid = Number(match[1]);
    if (!Number.isInteger(pid) || pid <= 0 || pid === process.pid) continue;
    const command = match[2];
    for (let at = command.indexOf(flag); at >= 0; at = command.indexOf(flag, at + 1)) {
      const after = command[at + flag.length];
      if (after === undefined || after === ' ') { procs.push({ pid }); break; }
    }
  }
  return procs;
}

async function sendBrowserClose(wsEndpoint: string, timeoutMs: number): Promise<void> {
  timeoutMs = spawnTimeout(timeoutMs, 2_000);
  if (timeoutMs <= 0) return;
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
  /**
   * Signals the launcher: through its ChildProcess on Windows, the process group on POSIX. `timeoutMs` is the
   * remaining budget, capped. Never called once the launcher has been observed gone (any platform).
   */
  signalTree?: (child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL', timeoutMs: number) => void;
  /**
   * Force-kills descendants listed by a fresh scan by native identity (pid plus creation FILETIME). Windows only.
   */
  killProcesses?: (procs: ProfileProcess[], timeoutMs: number) => void;
  /** Profile scan, bounded by `timeoutMs`. `null` when the query failed, timed out or hit an unknown pid. A bare pid has no creation time. */
  scanProfile?: (profileDir: string, timeoutMs: number) => Array<number | ProfileProcess> | null;
  /** CDP `Browser.close`, bounded by `timeoutMs`. */
  cdpClose?: (wsEndpoint: string, timeoutMs: number) => Promise<void>;
  /**
   * Removes the profile directory; may throw (with an `errno` `code`) like `rmSync`, or return a promise.
   * `timeoutMs` is the whole milliseconds the removal may take; `signal` is aborted when that time is spent,
   * and the implementation must stop and then settle (resolve or reject; the default one terminates its
   * remover process and settles once that process has exited). After the abort close waits a short bounded time
   * for the settle: resolving, or rejecting with `code: REMOVE_ABORTED_CODE` once the exit is observed, means
   * `timed-out-terminated`; anything else (no settle, or another rejection) reports `timed-out-running`.
   */
  removeDir?: (dir: string, timeoutMs: number, signal: AbortSignal) => void | Promise<void>;
  /** Timer for the removal bound: resolves after `ms`. Default `setTimeout`, cancelled when the removal settles. */
  removeTimeout?: (ms: number) => Promise<void>;
  /** Whether a path exists. Default `existsSync`. */
  exists?: (dir: string) => boolean;
  /** Clock. Default: monotonic (`performance.now`). */
  now?: () => number;
  /** Delay. Default `setTimeout`. */
  sleep?: (ms: number) => Promise<void>;
}

interface CloseDriver {
  platform: NodeJS.Platform;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
  launcher: (child: ChildProcess) => LauncherLiveness;
  signal: (child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL', timeoutMs: number) => void;
  kill: (procs: ProfileProcess[], timeoutMs: number) => void;
  scan: (profileDir: string, timeoutMs: number) => ProfileProcess[] | null;
  cdpClose: (wsEndpoint: string, timeoutMs: number) => Promise<void>;
  removeDir: (dir: string, timeoutMs: number, signal: AbortSignal) => void | Promise<void>;
  removeTimer: (ms: number) => { promise: Promise<void>; cancel: () => void };
  exists: (dir: string) => boolean;
}

function closeDriver(seams: CloseSeams = {}): CloseDriver {
  const seamScan = seams.scanProfile;
  return {
    platform: seams.platform ?? process.platform,
    now: seams.now ?? monotonicNow,
    sleep: seams.sleep ?? delay,
    launcher: seams.launcherLiveness ?? launcherLiveness,
    signal: seams.signalTree ?? signalTree,
    kill: seams.killProcesses ?? killVerifiedProcesses,
    scan: seamScan
      ? (profileDir, timeoutMs) => seamScan(profileDir, timeoutMs)?.map((p) => (typeof p === 'number' ? { pid: p } : p)) ?? null
      : probeProfileProcesses,
    cdpClose: seams.cdpClose ?? sendBrowserClose,
    removeDir: seams.removeDir ?? removeDirInChild,
    removeTimer: seams.removeTimeout
      ? (ms) => ({ promise: seams.removeTimeout!(ms), cancel: () => {} })
      : (ms) => {
          let handle: ReturnType<typeof setTimeout> | undefined;
          const promise = new Promise<void>((resolve) => {
            handle = setTimeout(resolve, Math.min(Math.max(0, ms), 2_147_483_647));
          });
          return { promise, cancel: () => clearTimeout(handle) };
        },
    exists: seams.exists ?? existsSync,
  };
}

/**
 * The one monotonic close budget, measured with the driver clock from close start. `left()` is what any step
 * may still spend: the budget minus a reserve kept for the final fresh observation. `finalLeft()` is what the
 * final observation itself may spend (the whole remainder). `phase(ms)` is a nested bound: its `left()` is
 * `min(this budget's left(), what remains of the phase)`, so every scan, kill and removal step inside a phase
 * is bounded by both the global budget and the phase.
 */
interface CloseBudget {
  left(): number;
  finalLeft(): number;
  phase(ms: number): CloseBudget;
  /** The budget as the profile removal sees it: its `left()` includes the removal reserve the other steps may not spend. */
  forRemoval(): CloseBudget;
}

/** Time kept back from the steps so the final scan can still run inside the budget. */
const FINAL_OBSERVE_RESERVE_MS = 5_000;
/**
 * Time kept back from the shutdown steps (grace waits, forced kills, settle) so the profile removal still gets a
 * real bound when the tree would not settle: without it a close that spent its budget forcing a hung renderer
 * reached the removal with nothing left and reported `not-attempted`.
 */
const PROFILE_REMOVE_RESERVE_MS = 10_000;

function phaseBudget(d: CloseDriver, parent: CloseBudget, ms: number): CloseBudget {
  // A non-finite bound means "no phase cap": the phase is bounded by its parent (the global budget) alone.
  const end = Number.isFinite(ms) ? d.now() + Math.max(0, ms) : Number.POSITIVE_INFINITY;
  const phase: CloseBudget = {
    left: () => Math.min(parent.left(), Math.max(0, end - d.now())),
    finalLeft: () => parent.finalLeft(),
    phase: (next) => phaseBudget(d, phase, next),
    forRemoval: () => parent.forRemoval(),
  };
  return phase;
}

/**
 * Both reserves come off the steps' `left()`: `budget - final reserve - removal reserve`, each reserve at most a
 * quarter of the budget. The removal may spend `budget - final reserve`; the final observation may spend it all.
 */
function makeBudget(d: CloseDriver, started: number, budgetMs: number): CloseBudget {
  const deadline = started + budgetMs;
  const quarter = Math.floor(budgetMs / 4);
  const finalReserve = Math.min(FINAL_OBSERVE_RESERVE_MS, quarter);
  const removeReserve = Math.min(PROFILE_REMOVE_RESERVE_MS, quarter);
  const finalLeft = () => Math.max(0, deadline - d.now());
  const removal: CloseBudget = {
    left: () => Math.max(0, deadline - finalReserve - d.now()),
    finalLeft,
    phase: (ms) => phaseBudget(d, removal, ms),
    forRemoval: () => removal,
  };
  const root: CloseBudget = {
    left: () => Math.max(0, deadline - finalReserve - removeReserve - d.now()),
    finalLeft,
    phase: (ms) => phaseBudget(d, root, ms),
    forRemoval: () => removal,
  };
  return root;
}

/** Error codes that mean "a handle is still open, try again" (Windows holds a profile briefly after exit). */
const RETRYABLE_REMOVE_CODES = new Set(['EBUSY', 'EPERM', 'EACCES', 'ENOTEMPTY', 'EMFILE']);

/**
 * A fresh scan bounded by `min(cap, limitMs)`. A scan that cannot start inside the limit (no whole millisecond
 * left), throws, or fails is `null`: unknown, never clean.
 */
function scanWithin(d: CloseDriver, profileDir: string, limitMs: number): ProfileProcess[] | null {
  const timeoutMs = spawnTimeout(limitMs, SCAN_CAP_MS);
  if (timeoutMs <= 0) return null;
  try {
    return d.scan(profileDir, timeoutMs);
  } catch {
    return null;
  }
}

/**
 * Signal the launcher, bounded by the remaining budget. Never signals a launcher whose exit Node has recorded:
 * its saved pid or process group may belong to someone else by now. Returns whether a signal was issued.
 */
function signalLauncher(d: CloseDriver, b: CloseBudget, child: ChildProcess, signal: 'SIGTERM' | 'SIGKILL'): boolean {
  if (launcherExitRecorded(child)) return false;
  const timeoutMs = spawnTimeout(b.left(), TASKKILL_CAP_MS);
  if (timeoutMs <= 0) return false;
  try {
    d.signal(child, signal, timeoutMs);
  } catch { /* gone */ }
  return true;
}

/**
 * Force the descendants a fresh scan just listed, in one call bounded by the remaining budget and phase. The
 * launcher is never among them (it is signalled through its ChildProcess or its group only). Returns whether a
 * kill was issued; a failed scan (`null`) forces nothing.
 *
 * Windows: every verified row (an `unknown` row is skipped), by native identity. POSIX never forces scanned
 * rows: while the launcher remains owned, `signalLauncher` reaches its group; after exit, a snapshot cannot
 * prevent group-id reuse, so surviving processes remain diagnostic leftovers.
 */
function forceDescendants(d: CloseDriver, b: CloseBudget, child: ChildProcess, fresh: readonly ProfileProcess[] | null): boolean {
  if (d.platform !== 'win32' || !fresh || !child.pid) return false;
  const targets = fresh.filter((p) => p.pid !== child.pid && !p.unknown);
  const timeoutMs = spawnTimeout(b.left(), TASKKILL_CAP_MS);
  if (targets.length === 0 || timeoutMs <= 0) return false;
  try {
    d.kill(targets, timeoutMs);
  } catch { /* gone */ }
  return true;
}

type RemoveAttempt = 'done' | 'timed-out-terminated' | 'timed-out-running';

/** Part of a removal attempt's time kept back for the aborted remover to confirm it has stopped: a quarter, at most {@link REMOVER_STOP_WAIT_MS}. */
function stopReserveMs(ms: number): number {
  return Math.floor(Math.min(REMOVER_STOP_WAIT_MS, ms / 4));
}

/**
 * One removal attempt that must end, stop wait included, inside `ms` (the phase's remaining time when it
 * starts). The removal itself is bounded by `ms` minus a reserve (a quarter, at most 1 s). When that bound is
 * spent the attempt is told to stop (its signal aborts) and is then given at most `min(1 s, stopLeft())` to
 * settle, `stopLeft` being the phase's remaining time read after the bound is spent. So the wait is the
 * reserve and never runs past the phase or the global budget. `timed-out-terminated` only when the remover
 * finished or rejected with {@link REMOVE_ABORTED_CODE} (an observed exit), otherwise `timed-out-running`
 * (abandoned, possibly still running; any other rejection, a failed kill included, does not confirm a stop). A rejection that is not a timeout is thrown.
 */
async function attemptRemove(d: CloseDriver, profileDir: string, ms: number, stopLeft: () => number): Promise<RemoveAttempt> {
  const stop = new AbortController();
  const bound = ms - stopReserveMs(ms);
  const timer = d.removeTimer(bound);
  const op = (async () => d.removeDir(profileDir, bound, stop.signal))();
  const settled = op.then(() => 'done' as const, (error: unknown) => ({ error }));
  try {
    const first = await Promise.race([settled, timer.promise.then(() => 'timeout' as const)]);
    if (first === 'done') return 'done';
    if (first !== 'timeout') throw first.error;
  } finally {
    timer.cancel();
  }
  stop.abort();
  const wait = Math.floor(Math.min(REMOVER_STOP_WAIT_MS, stopLeft()));
  if (wait < 1) return 'timed-out-running';
  const grace = d.removeTimer(wait);
  try {
    const next = await Promise.race([settled, grace.promise.then(() => 'timeout' as const)]);
    if (next === 'timeout') return 'timed-out-running';
    // Termination is claimed only for an observed exit: the remover finished, or rejected with the abort code.
    // Any other rejection after the abort (a failed kill, say) proves nothing about the process.
    return next === 'done' || (next.error as { code?: unknown } | undefined)?.code === REMOVE_ABORTED_CODE ? 'timed-out-terminated' : 'timed-out-running';
  } finally {
    grace.cancel();
  }
}

/**
 * Remove the harness-owned profile, retrying EBUSY/EPERM-class failures until `min(boundMs, remaining budget)`
 * passes; the remaining budget here includes the removal reserve the shutdown steps could not spend, so a tree
 * that never settled still leaves the removal a real bound. Every attempt, its stop wait included, ends inside that same time (the stop wait comes out of the
 * phase, not on top of it): the removal runs off this thread (by default in a
 * separate process this close owns and terminates when the bound is spent), so a stalled or huge removal
 * cannot hold the close past its phase or the global budget. Between attempts on Windows the profile is
 * scanned afresh and only the descendants that scan lists are forced; both calls are bounded by the removal
 * phase and the global budget. A non-retryable error stops the loop. Never throws. The result says exactly
 * what happened (see {@link ProfileRemoval}); `removed` only when the directory is verified gone. With no whole
 * millisecond left in the phase or the global budget (including `profileRemoveMs: 0`) nothing is attempted.
 */
async function removeProfileDir(d: CloseDriver, b: CloseBudget, child: ChildProcess, profileDir: string, boundMs: number): Promise<ProfileRemoval> {
  const rb = b.forRemoval().phase(boundMs);
  let attempted = false;
  const end = (): ProfileRemoval => (!d.exists(profileDir) ? 'removed' : attempted ? 'failed' : 'not-attempted');
  for (;;) {
    const ms = Math.floor(rb.left());
    if (ms < 1) return end();
    attempted = true;
    try {
      const outcome = await attemptRemove(d, profileDir, ms, () => rb.left());
      if (outcome !== 'done') return !d.exists(profileDir) ? 'removed' : outcome;
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | undefined)?.code;
      if (code === undefined || !RETRYABLE_REMOVE_CODES.has(code)) return end();
    }
    if (!d.exists(profileDir)) return 'removed';
    if (rb.left() <= 0) return 'failed';
    if (d.platform === 'win32') forceDescendants(d, rb, child, scanWithin(d, profileDir, rb.left()));
    const nap = Math.min(250, rb.left());
    if (nap > 0) await d.sleep(nap);
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
  if (launcherExitRecorded(child)) return 'gone';
  try {
    return d.launcher(child);
  } catch {
    return 'unknown';
  }
}

/** A fresh observation: the launcher's state, and whether it is gone with a scan that listed nothing. */
interface Settling {
  settled: boolean;
  launcher: LauncherLiveness;
  /** Present after the forced phase: whether it issued any signal or kill. */
  forced?: boolean;
}

/** The launcher is gone and the profile scan is known to list nothing. */
async function isSettled(d: CloseDriver, b: CloseBudget, child: ChildProcess, profileDir: string | undefined): Promise<Settling> {
  const launcher = await observeLauncher(d, child);
  if (launcher !== 'gone') return { settled: false, launcher };
  if (!profileDir) return { settled: true, launcher };
  return { settled: scanWithin(d, profileDir, b.left())?.length === 0, launcher };
}

/** Waits up to `min(ms, remaining budget)` for {@link isSettled}. The first check is already fresh. */
async function waitSettled(d: CloseDriver, b: CloseBudget, child: ChildProcess, profileDir: string | undefined, ms: number): Promise<Settling> {
  const pb = b.phase(ms);
  for (;;) {
    const seen = await isSettled(d, pb, child, profileDir);
    if (seen.settled) return seen;
    const left = pb.left();
    if (left <= 0) return seen;
    await d.sleep(Math.min(100, left));
  }
}

/**
 * Force the tree, re-issuing every 2 s until settled or `min(killBound, remaining budget)` passes. Every
 * iteration works from its own fresh observation. The launcher is signalled only while it has not been
 * observed gone, and only through its ChildProcess (Windows) or process group (POSIX). Descendants are forced
 * from that iteration's scan on Windows, every verified row by native identity. POSIX signals only while the
 * launcher remains owned. A failed scan forces nothing. An iteration that can
 * issue nothing (launcher gone and no verified, owned target, or an unknown scan) ends the phase at once:
 * waiting longer here could change nothing, and the settle and the final observation still follow. Returns at
 * once when the launcher is gone and the profile scan is clean. `forced` says whether anything was issued.
 */
async function forceSettled(d: CloseDriver, b: CloseBudget, child: ChildProcess, profileDir: string | undefined, killBound: number): Promise<Settling> {
  const pb = b.phase(killBound);
  let forced = false;
  for (;;) {
    const launcher = await observeLauncher(d, child);
    const scan = profileDir ? scanWithin(d, profileDir, pb.left()) : [];
    if (launcher === 'gone' && scan?.length === 0) return { settled: true, launcher, forced };
    let acted = launcher !== 'gone' && signalLauncher(d, pb, child, 'SIGKILL');
    if (forceDescendants(d, pb, child, scan)) acted = true;
    forced ||= acted;
    if (!acted) return { settled: false, launcher, forced };
    const next = await waitSettled(d, pb, child, profileDir, Math.min(2_000, pb.left()));
    if (next.settled || pb.left() <= 0) return { ...next, forced };
  }
}

/**
 * Close a Chrome process tree gracefully: CDP `Browser.close`, wait; SIGTERM the process group, wait; SIGKILL
 * only last. Then remove the harness-owned profile directory and observe the profile's process list afresh.
 *
 * One monotonic budget (`budgetMs`, default 60 s, measured from close start) bounds the whole call: every
 * step, scan and kill is bounded by `min(global remaining, its phase remaining)`, no step starts a wait longer
 * than what is left, and once the budget is spent the close goes straight to the final fresh observation. A
 * call is never spawned with no whole millisecond left (`spawnSync` would read 0 as no timeout). The per-step
 * options stay as upper bounds.
 *
 * The profile is removed as soon as the launcher is confirmed gone and a fresh scan lists nothing, without
 * waiting for the remaining grace or kill bounds.
 *
 * POSIX never signals the process group once the launcher is observed gone: a saved group id may be reused.
 * Processes still carrying the profile are reported as `leftoverPids` (or unknown if the scan fails), with
 * `exited: false`.
 *
 * `exited` is true only when the launcher is gone (Node's exit, or the OS probe) and the final fresh scan
 * succeeded and listed nothing. A failed or timed-out scan or probe, or a scan that met a process it could
 * not verify, is reported as `livenessUnknown`; it is never read as clean or as alive.
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
  /** Total budget for the whole close, from its start. Every step is also bounded by what remains. Default 60 s. */
  budgetMs?: number;
  /** Test seams. Every field defaults to the real implementation. */
  seams?: CloseSeams;
}): Promise<ChromeCloseReport> {
  const d = closeDriver(options.seams);
  const { child, profileDir } = options;
  const started = d.now();
  // Keep the last successful observation even when a later scan fails. Unknown must not erase a leak.
  let lastKnownProfile: ProfileProcess[] | undefined;
  const scan = d.scan;
  d.scan = (dir, timeoutMs) => {
    const found = scan(dir, timeoutMs);
    if (found !== null) lastKnownProfile = found;
    return found;
  };
  const budgetMs = typeof options.budgetMs === 'number' && Number.isFinite(options.budgetMs) ? Math.max(0, options.budgetMs) : 60_000;
  const b = makeBudget(d, started, budgetMs);
  const grace = options.graceMs ?? 5_000;
  const killBound = options.killTimeoutMs ?? 15_000;
  let seen = await isSettled(d, b, child, profileDir);
  let step: ChromeCloseStep = seen.launcher === 'gone' ? 'already-exited' : 'not-attempted';
  const cdpTimeout = spawnTimeout(Math.min(grace, b.left()), 2_000);
  if (!seen.settled && seen.launcher !== 'gone' && options.wsEndpoint && cdpTimeout > 0) {
    step = 'cdp';
    await d.cdpClose(options.wsEndpoint, cdpTimeout);
    seen = await waitSettled(d, b, child, profileDir, grace);
  }
  // Windows has no graceful signal for a windowless headless browser (`taskkill /T` without
  // `/F` is a no-op there), so that step is skipped rather than burning a grace period. POSIX signals the
  // group only while its root has not been observed gone.
  if (!seen.settled && d.platform !== 'win32' && seen.launcher !== 'gone' && b.left() >= 1) {
    step = 'sigterm';
    signalLauncher(d, b, child, 'SIGTERM');
    seen = await waitSettled(d, b, child, profileDir, grace);
  }
  // Chrome's children can outlive a dead launcher. Windows forces each descendant a fresh scan lists by native
  // identity; POSIX signals only while its launcher remains owned. Under load one round can leave
  // a hung renderer, so this keeps re-issuing while it has something to force. `step` records the phase only
  // when it issued something.
  if (!seen.settled && killBound > 0 && b.left() >= 1) {
    seen = await forceSettled(d, b, child, profileDir, killBound);
    if (seen.forced) step = 'sigkill';
  }
  // Not settled: pids still exiting get a bounded wait, then the last scan decides. A pid still listed after
  // the bound is a leftover. A scan that never answers is unknown, not an empty profile. When settled the
  // launcher is gone and a fresh scan listed nothing, so there is nothing to wait for: remove at once.
  let leftoverPids: number[] = [];
  let observedExitingPids: number[] = [];
  let scanUnknown = false;
  if (profileDir && !seen.settled) {
    const sb = b.phase(options.settleMs ?? 4_000);
    const settle = await settleProfileProcesses({
      list: () => scanWithin(d, profileDir, sb.left())?.map((p) => p.pid) ?? null,
      boundMs: sb.left(),
      now: d.now,
      sleep: d.sleep,
    });
    leftoverPids = settle.leftoverPids;
    observedExitingPids = settle.observedExitingPids;
    scanUnknown = settle.unknown === true;
  }
  let profileRemoval: ProfileRemoval = 'not-attempted';
  if (profileDir) {
    profileRemoval = await removeProfileDir(d, b, child, profileDir, options.profileRemoveMs ?? 10_000);
  }
  const profileRemoved = !profileDir || profileRemoval === 'removed';
  // Fresh final observations decide the verdict: the settle's list is replaced by a new scan (unknown if the
  // scan fails), and the launcher is asked again.
  if (profileDir) {
    const final = scanWithin(d, profileDir, b.finalLeft());
    if (final === null) {
      scanUnknown = true;
      leftoverPids = lastKnownProfile?.filter((p) => !p.unknown).map((p) => p.pid) ?? leftoverPids;
    } else {
      // A verified row is a leftover. An unknown row (Windows: listed for this profile, identity unverifiable) is
      // neither claimed as a leftover nor read as clean: the close is unknown, as after a failed scan.
      leftoverPids = final.filter((p) => !p.unknown).map((p) => p.pid);
      scanUnknown = final.some((p) => p.unknown === true);
    }
  }
  observedExitingPids = observedExitingPids.filter((pid) => !leftoverPids.includes(pid));
  const launcher = await observeLauncher(d, child);
  const exited = launcher === 'gone' && leftoverPids.length === 0 && !scanUnknown;
  const report: ChromeCloseReport = { step, exited, profileRemoved, profileRemoval, leftoverPids, observedExitingPids, durationMs: Math.max(0, Math.floor(d.now() - started)) };
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
  /** Clock, injectable for tests. Default: monotonic (`performance.now`). */
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
  const now = options.now ?? monotonicNow;
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
