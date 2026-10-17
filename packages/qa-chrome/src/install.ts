/**
 * RightKit-managed Chrome for Testing at ONE fixed path, the same layout the Rust installer
 * (`rightkit-browser` feature `install`) uses, so dev, QA and installed apps agree:
 *
 *   <root>/current            the install (`install.json` records version, platform, digests)
 *   <root>/current.previous   the single rollback copy
 *   <root>/current.staging    extraction area; swapped into `current` only when complete
 *
 * No per-version folder and no new id. The archive's SHA-256 must be pinned ({@link PinnedChrome});
 * a pin entry without a digest is refused before anything is downloaded.
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createWriteStream, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { grantSandboxAccess } from './sandbox-access.js';

/** Environment variable overriding the managed Chrome for Testing folder. */
export const CHROME_FOR_TESTING_ROOT_ENV = 'RIGHTKIT_CHROME_FOR_TESTING_ROOT';

/** Folder names inside the managed root (identical to rightkit-browser's `ManagedRoot`). */
export const MANAGED_CURRENT_DIR = 'current';
export const MANAGED_PREVIOUS_DIR = 'current.previous';
export const MANAGED_STAGING_DIR = 'current.staging';

export interface PinnedDownload {
  /** Google's platform key: `mac-arm64`, `mac-x64`, `win64`, `win32` or `linux64`. */
  platform: string;
  /** `https` URL of `chrome-<platform>.zip`. Immutable: it names the version. */
  url: string;
  /** Lowercase hex SHA-256 of the archive. REQUIRED: leave it out only as a recorded TODO; the install then refuses. */
  sha256?: string;
}

export interface PinnedChrome {
  /** Dotted numeric version. */
  version: string;
  downloads: PinnedDownload[];
}

// TODO(#267): Google publishes no digests for Chrome for Testing. They are computed from the real archives by
// the chrome-pins workflow and written here AND into rightkit-browser's PINNED_CHROME_FOR_TESTING by
// `node scripts/chrome-pins.mjs --apply chrome-pins.json`. A platform with no entry here has no digest, and the
// install refuses it. Keep the begin/end markers: the script rewrites the lines between them.
const CHROME_SHA256: Record<string, string | undefined> = {
  // chrome-pins:begin
  // chrome-pins:end
};

const cft = (version: string, platform: string): PinnedDownload => ({
  platform,
  url: `https://storage.googleapis.com/chrome-for-testing-public/${version}/${platform}/chrome-${platform}.zip`,
  sha256: CHROME_SHA256[platform],
});

/** The build this package installs; the same version and URLs as rightkit-browser's pin. */
export const PINNED_CHROME_FOR_TESTING: PinnedChrome = {
  version: '155.0.8059.39',
  downloads: ['mac-arm64', 'mac-x64', 'linux64', 'win64'].map((p) => cft('155.0.8059.39', p)),
};

/** Thrown before any download when the pin has no usable digest for the platform. */
export class ChromeForTestingDigestMissingError extends Error {
  readonly code = 'CHROME_DIGEST_NOT_PINNED';
  constructor(readonly platform: string, readonly version: string) {
    super(`no SHA-256 is pinned for Chrome for Testing ${version} (${platform}); refusing to download without one`);
    this.name = 'ChromeForTestingDigestMissingError';
  }
}

export class ChromeForTestingDigestMismatchError extends Error {
  readonly code = 'CHROME_DIGEST_MISMATCH';
  constructor(readonly url: string, readonly expected: string, readonly actual: string) {
    super(`${url}: archive digest ${actual} does not match the pinned ${expected}`);
    this.name = 'ChromeForTestingDigestMismatchError';
  }
}

export interface ManagedRootOptions {
  env?: NodeJS.ProcessEnv;
  platform?: NodeJS.Platform;
  homedir?: string;
}

/**
 * The suite-wide folder (`<app data>/Orthic Labs/shared/chrome-for-testing`), or
 * `RIGHTKIT_CHROME_FOR_TESTING_ROOT`. The install is `<root>/current`.
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

/** `<root>/current` and `<root>/current.previous`. */
export function managedCurrentDir(options: ManagedRootOptions = {}): string {
  return path.join(managedChromeForTestingRoot(options), MANAGED_CURRENT_DIR);
}
export function managedPreviousDir(options: ManagedRootOptions = {}): string {
  return path.join(managedChromeForTestingRoot(options), MANAGED_PREVIOUS_DIR);
}

/** Google's platform key for this host. */
export function chromeForTestingPlatform(platform: NodeJS.Platform = process.platform, arch: string = process.arch): string {
  if (platform === 'darwin') return arch === 'arm64' ? 'mac-arm64' : 'mac-x64';
  if (platform === 'win32') return arch === 'ia32' ? 'win32' : 'win64';
  return 'linux64';
}

/** Executable path inside one extracted folder (`current`, `current.previous`, or a legacy version folder). */
export function managedExecutable(dir: string, platformKey: string): string {
  const sub = path.join(dir, `chrome-${platformKey}`);
  if (platformKey.startsWith('mac')) {
    return path.join(sub, 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing');
  }
  return path.join(sub, platformKey.startsWith('win') ? 'chrome.exe' : 'chrome');
}

export interface InstallChromeForTestingOptions extends ManagedRootOptions {
  /** Defaults to {@link PINNED_CHROME_FOR_TESTING}. */
  pin?: PinnedChrome;
  arch?: string;
  log?: (line: string) => void;
  /** Test seam: fetch the archive bytes (default: global `fetch`). */
  download?: (url: string, dest: string) => Promise<void>;
}

export interface InstalledChromeForTesting {
  version: string;
  executablePath: string;
  /** False when the pinned version was already installed. */
  downloaded: boolean;
}

interface Marker {
  version: string;
  platform: string;
  files: { kind: 'chrome'; url: string; sha256: string }[];
}

function readMarker(dir: string): Marker | undefined {
  try {
    return JSON.parse(readFileSync(path.join(dir, 'install.json'), 'utf8')) as Marker;
  } catch {
    return undefined;
  }
}

/** Throws when a listed archive member could escape `chrome-<key>/` (absolute, `..`, backslash, drive colon, foreign top level). */
export function assertSafeArchiveMembers(names: string[], platformKey: string): void {
  const top = `chrome-${platformKey}`;
  if (names.length === 0) throw new Error('archive is empty');
  for (const raw of names) {
    const parts = raw.split('/').filter((p) => p !== '' && p !== '.');
    if (raw.startsWith('/') || raw.includes('\\') || raw.includes('\0') || parts.length === 0 || parts[0] !== top || parts.some((p) => p === '..' || p.includes(':'))) {
      throw new Error(`unsafe archive member ${JSON.stringify(raw)}`);
    }
  }
}

function listArchive(zip: string, platform: NodeJS.Platform): string[] {
  const done = platform === 'win32'
    ? spawnSync(windowsTar(), ['-tf', zip], { encoding: 'utf8', windowsHide: true })
    : spawnSync('unzip', ['-Z1', zip], { encoding: 'utf8' });
  if (done.status !== 0) throw new Error('listing the Chrome for Testing archive failed');
  return done.stdout.split(/\r?\n/).filter((l) => l !== '');
}

async function defaultDownload(url: string, dest: string): Promise<void> {
  const body = await fetch(url);
  if (!body.ok || !body.body) throw new Error(`download failed: HTTP ${body.status}`);
  await pipeline(Readable.fromWeb(body.body as never), createWriteStream(dest));
}

/**
 * Install the pinned Chrome for Testing at `<root>/current` (idempotent): digest checked before
 * extraction, extracted to `current.staging`, swapped in, the old `current` kept as the one
 * `current.previous`. Throws {@link ChromeForTestingDigestMissingError} when no digest is pinned.
 */
export async function installChromeForTesting(options: InstallChromeForTestingOptions = {}): Promise<InstalledChromeForTesting> {
  const pin = options.pin ?? PINNED_CHROME_FOR_TESTING;
  const platform = options.platform ?? process.platform;
  const key = chromeForTestingPlatform(platform, options.arch ?? process.arch);
  const log = options.log ?? (() => {});
  if (!/^\d+(\.\d+)+$/.test(pin.version)) throw new Error(`invalid pin: version ${JSON.stringify(pin.version)}`);
  const download = pin.downloads.find((d) => d.platform === key);
  if (!download) throw new Error(`the pin has no Chrome for Testing download for ${key}`);
  if (!download.url.startsWith('https://')) throw new Error(`invalid pin: ${download.url} is not an https URL`);
  if (download.sha256 === undefined) throw new ChromeForTestingDigestMissingError(key, pin.version);
  if (!/^[0-9a-fA-F]{64}$/.test(download.sha256)) throw new Error(`invalid pin: digest for ${download.url} is not 64 hex characters`);
  const sha256 = download.sha256.toLowerCase();

  const root = managedChromeForTestingRoot(options);
  const current = path.join(root, MANAGED_CURRENT_DIR);
  const previous = path.join(root, MANAGED_PREVIOUS_DIR);
  const staging = path.join(root, MANAGED_STAGING_DIR);
  const scratch = path.join(root, 'current.download');
  const exe = managedExecutable(current, key);
  const marker: Marker = { version: pin.version, platform: key, files: [{ kind: 'chrome', url: download.url, sha256 }] };

  const have = readMarker(current);
  if (have && JSON.stringify(have) === JSON.stringify(marker) && existsSync(exe)) {
    // Windows: an install made before the grant existed is repaired here.
    grantSandboxAccess(current, exe);
    log(`Chrome for Testing ${pin.version} already installed: ${exe}`);
    return { version: pin.version, executablePath: exe, downloaded: false };
  }

  mkdirSync(root, { recursive: true });
  rmSync(staging, { recursive: true, force: true });
  rmSync(scratch, { recursive: true, force: true });
  mkdirSync(scratch);
  mkdirSync(staging);
  try {
    const zip = path.join(scratch, 'chrome.zip');
    log(`downloading Chrome for Testing ${pin.version} (${key}) from ${download.url}`);
    await (options.download ?? defaultDownload)(download.url, zip);
    const actual = createHash('sha256').update(readFileSync(zip)).digest('hex');
    if (actual !== sha256) throw new ChromeForTestingDigestMismatchError(download.url, sha256, actual);
    assertSafeArchiveMembers(listArchive(zip, platform), key);
    // macOS: ditto keeps the app bundle's symlinks and signature intact (Info-ZIP unzip does not).
    const unzip = platform === 'win32'
      ? spawnSync(windowsTar(), ['-xf', zip, '-C', staging], { stdio: 'inherit', windowsHide: true })
      : platform === 'darwin'
        ? spawnSync('ditto', ['-x', '-k', zip, staging], { stdio: 'inherit' })
        : spawnSync('unzip', ['-q', zip, '-d', staging], { stdio: 'inherit' });
    if (unzip.status !== 0) throw new Error('extracting Chrome for Testing failed');
    const staged = managedExecutable(staging, key);
    if (!existsSync(staged)) throw new Error(`archive has no executable at ${staged}`);
    writeFileSync(path.join(staging, 'install.json'), `${JSON.stringify(marker, null, 2)}\n`);
    // Windows: an unpacked Chrome is unreadable to its own sandbox until this grant (see sandbox-access.ts);
    // granting before the swap means `current` never appears without it.
    grantSandboxAccess(staging, staged);
    rmSync(previous, { recursive: true, force: true });
    const hadCurrent = existsSync(current);
    if (hadCurrent) renameSync(current, previous);
    try {
      renameSync(staging, current);
    } catch (error) {
      if (hadCurrent) renameSync(previous, current);
      throw error;
    }
    log(`installed Chrome for Testing ${pin.version}: ${exe}`);
    return { version: pin.version, executablePath: exe, downloaded: true };
  } finally {
    rmSync(staging, { recursive: true, force: true });
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** Legacy per-version folders (installs made before the fixed layout), newest first. `current` is not listed. */
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
  return path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe');
}
