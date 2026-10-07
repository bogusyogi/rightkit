/**
 * RightKit-managed Chrome for Testing: downloaded straight from Google's Chrome for Testing
 * release list into one suite-wide folder, so every app and agent resolves the same current
 * build without Puppeteer's or Playwright's caches.
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createWriteStream, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

/** Environment variable overriding the managed Chrome for Testing folder. */
export const CHROME_FOR_TESTING_ROOT_ENV = 'RIGHTKIT_CHROME_FOR_TESTING_ROOT';

/** Google's published Chrome for Testing channel list. */
export const CHROME_FOR_TESTING_RELEASES =
  'https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json';


export interface ManagedRootOptions {
  env?: NodeJS.ProcessEnv;
  platform?: NodeJS.Platform;
  homedir?: string;
}

/**
 * The suite-wide folder (`<app data>/Orthic Labs/shared/chrome-for-testing`), or
 * `RIGHTKIT_CHROME_FOR_TESTING_ROOT`. Versions live in `<root>/<version>/<platform dir>/`.
 */
export function managedChromeForTestingRoot(options: ManagedRootOptions = {}): string {
  const env = options.env ?? process.env;
  if (env[CHROME_FOR_TESTING_ROOT_ENV]) return env[CHROME_FOR_TESTING_ROOT_ENV]!;
  const platform = options.platform ?? process.platform;
  const home = options.homedir ?? homedir();
  const appData =
    platform === 'darwin' ? path.join(home, 'Library', 'Application Support')
    : platform === 'win32' ? (env.LOCALAPPDATA || path.join(home, 'AppData', 'Local'))
    : (env.XDG_DATA_HOME || path.join(home, '.local', 'share'));
  return path.join(appData, 'Orthic Labs', 'shared', 'chrome-for-testing');
}

/** Google's platform key for this host. */
export function chromeForTestingPlatform(platform: NodeJS.Platform = process.platform, arch: string = process.arch): string {
  if (platform === 'darwin') return arch === 'arm64' ? 'mac-arm64' : 'mac-x64';
  if (platform === 'win32') return arch === 'ia32' ? 'win32' : 'win64';
  return 'linux64';
}

/** Executable path inside one extracted version folder. */
export function managedExecutable(versionDir: string, platformKey: string): string {
  const dir = path.join(versionDir, `chrome-${platformKey}`);
  if (platformKey.startsWith('mac')) {
    return path.join(dir, 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing');
  }
  return path.join(dir, platformKey.startsWith('win') ? 'chrome.exe' : 'chrome');
}

export interface InstallChromeForTestingOptions extends ManagedRootOptions {
  /** Google channel: Stable (default), Beta, Dev or Canary. */
  channel?: 'Stable' | 'Beta' | 'Dev' | 'Canary';
  arch?: string;
  log?: (line: string) => void;
}

export interface InstalledChromeForTesting {
  version: string;
  executablePath: string;
  /** False when that version was already installed. */
  downloaded: boolean;
}

/**
 * Install the channel's current Chrome for Testing into the managed folder (idempotent),
 * downloading only from Google's Chrome for Testing bucket; `install.json` records the archive digest.
 */
export async function installChromeForTesting(options: InstallChromeForTestingOptions = {}): Promise<InstalledChromeForTesting> {
  const channel = options.channel ?? 'Stable';
  const platform = options.platform ?? process.platform;
  const key = chromeForTestingPlatform(platform, options.arch ?? process.arch);
  const log = options.log ?? (() => {});
  const res = await fetch(CHROME_FOR_TESTING_RELEASES);
  if (!res.ok) throw new Error(`Chrome for Testing release list: HTTP ${res.status}`);
  const list = (await res.json()) as {
    channels: Record<string, { version: string; downloads: { chrome?: { platform: string; url: string }[] } }>;
  };
  const entry = list.channels[channel];
  const download = entry?.downloads.chrome?.find((d) => d.platform === key);
  if (!entry || !download) throw new Error(`no Chrome for Testing ${channel} download for ${key}`);
  if (!download.url.startsWith('https://storage.googleapis.com/chrome-for-testing-public/')) {
    throw new Error(`unexpected Chrome for Testing download host: ${download.url}`);
  }
  const root = managedChromeForTestingRoot(options);
  const versionDir = path.join(root, entry.version);
  const exe = managedExecutable(versionDir, key);
  if (existsSync(exe)) {
    log(`Chrome for Testing ${entry.version} already installed: ${exe}`);
    return { version: entry.version, executablePath: exe, downloaded: false };
  }
  mkdirSync(root, { recursive: true });
  const staging = mkdtempSync(path.join(root, `.staging-${entry.version}-`));
  try {
    const zip = path.join(staging, 'chrome.zip');
    log(`downloading Chrome for Testing ${entry.version} (${key}) from ${download.url}`);
    const body = await fetch(download.url);
    if (!body.ok || !body.body) throw new Error(`download failed: HTTP ${body.status}`);
    await pipeline(Readable.fromWeb(body.body as never), createWriteStream(zip));
    const out = path.join(staging, 'out');
    mkdirSync(out);
    // macOS: ditto keeps the app bundle's symlinks and signature intact (Info-ZIP unzip does not).
    const unzip = platform === 'win32'
      ? spawnSync(windowsTar(), ['-xf', zip, '-C', out], { stdio: 'inherit', windowsHide: true })
      : platform === 'darwin'
        ? spawnSync('ditto', ['-x', '-k', zip, out], { stdio: 'inherit' })
        : spawnSync('unzip', ['-q', zip, '-d', out], { stdio: 'inherit' });
    if (unzip.status !== 0) throw new Error('extracting Chrome for Testing failed');
    const staged = managedExecutable(out, key);
    if (!existsSync(staged)) throw new Error(`archive has no executable at ${staged}`);
    // Chrome for Testing is not Developer-ID signed (no team, no sealed resources), so
    // integrity rests on the pinned Google HTTPS host; record what was installed.
    const sha256 = createHash('sha256').update(readFileSync(zip)).digest('hex');
    writeFileSync(path.join(out, 'install.json'), `${JSON.stringify({ version: entry.version, platform: key, channel, url: download.url, sha256, installedAt: new Date().toISOString() }, null, 2)}\n`);
    rmSync(versionDir, { recursive: true, force: true });
    renameSync(out, versionDir);
    log(`installed Chrome for Testing ${entry.version}: ${exe}`);
    return { version: entry.version, executablePath: exe, downloaded: true };
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

/** Installed managed versions, newest first. */
export function managedVersions(options: ManagedRootOptions = {}): string[] {
  try {
    return readdirSync(managedChromeForTestingRoot(options))
      .filter((n) => /^\d+(\.\d+)+$/.test(n))
      .sort((a, b) => {
        const ka = a.split('.').map(Number), kb = b.split('.').map(Number);
        for (let i = 0; i < Math.max(ka.length, kb.length); i++) if ((kb[i] ?? 0) !== (ka[i] ?? 0)) return (kb[i] ?? 0) - (ka[i] ?? 0);
        return 0;
      });
  } catch {
    return [];
  }
}

/** System bsdtar: a bare `tar` under Git Bash resolves to GNU tar, which reads `C:\…` as a remote host. */
function windowsTar(): string {
  return path.join(process.env.SystemRoot ?? 'C:\Windows', 'System32', 'tar.exe');
}
