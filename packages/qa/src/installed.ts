import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

/**
 * QA never launches an installed app. The only way to run against one is installed-QA mode, which
 * `right-release qa --stage installed` already signals to the runner it starts: it sets
 * `RIGHT_RELEASE_QA_STAGE=installed`. This package reads that variable and defines no second switch.
 */
export const INSTALLED_QA_STAGE_ENV = 'RIGHT_RELEASE_QA_STAGE';
export const INSTALLED_QA_STAGE_VALUE = 'installed';

export type InstalledPlatform = 'darwin' | 'win32' | 'linux';

export class InstalledAppRefused extends Error {
  readonly path: string;
  readonly root: string;
  constructor(appPath: string, root: string, what: string) {
    super(
      `QA refuses ${what} ${appPath}: it is inside the installed-apps location ${root}. QA drives dev or QA builds, never an installed app. `
      + `To run against an installed app on purpose, use installed-QA mode: run \`right-release qa --stage installed\`, which sets ${INSTALLED_QA_STAGE_ENV}=${INSTALLED_QA_STAGE_VALUE}; nothing was launched`,
    );
    this.name = 'InstalledAppRefused';
    this.path = appPath;
    this.root = root;
  }
}

export type EnvLike = Record<string, string | undefined>;

/** True when installed-QA mode is on (the stage signal `right-release qa --stage installed` sets). */
export function isInstalledQaMode(env: EnvLike = process.env): boolean {
  return env[INSTALLED_QA_STAGE_ENV]?.trim().toLowerCase() === INSTALLED_QA_STAGE_VALUE;
}

/**
 * The installed-app locations of one OS, from the environment (as rightkit-fs AppDirs resolves the
 * per-user bases). macOS: `/Applications` and `<home>/Applications`. Windows: `ProgramFiles`,
 * `ProgramFiles(x86)`, `ProgramW6432` and `%LOCALAPPDATA%\Programs`. Unset or relative values are skipped.
 */
export function installedRoots(platform: InstalledPlatform, env: EnvLike, home: string): string[] {
  const roots: string[] = [];
  if (platform === 'darwin') {
    roots.push('/Applications');
    if (home) roots.push(path.posix.join(home, 'Applications'));
  } else if (platform === 'win32') {
    for (const key of ['ProgramFiles', 'ProgramFiles(x86)', 'ProgramW6432']) {
      const value = env[key];
      if (value && path.win32.isAbsolute(value)) roots.push(value);
    }
    const local = env.LOCALAPPDATA;
    if (local && path.win32.isAbsolute(local)) roots.push(path.win32.join(local, 'Programs'));
  }
  return roots;
}

function fold(platform: InstalledPlatform, p: string): string {
  // `.` and `..` are resolved lexically; both file systems are treated as case-insensitive (the default on macOS and Windows).
  const normalised = platform === 'win32'
    ? path.win32.normalize(p.replace(/^\\\\\?\\(?!UNC\\)/i, '')).replaceAll('\\', '/')
    : path.posix.normalize(p);
  return normalised.toLowerCase().replace(/\/+$/, '');
}

/** Pure: the root `candidate` sits under, or undefined. Both are compared as resolved paths. */
export function installedRootOf(candidate: string, roots: readonly string[], platform: InstalledPlatform): string | undefined {
  const c = fold(platform, candidate);
  return roots.find((root) => {
    const r = fold(platform, root);
    return r !== '' && (c === r || c.startsWith(`${r}/`));
  });
}

/** Resolve symlinks in the longest existing prefix and append the rest. */
export function canonicalForInstalledCheck(input: string): string {
  const base = path.resolve(input);
  const tail: string[] = [];
  let cursor = base;
  for (;;) {
    try { return path.join(fs.realpathSync.native(cursor), ...tail.reverse()); } catch { /* climb to the existing prefix */ }
    const parent = path.dirname(cursor);
    if (parent === cursor) return base;
    tail.push(path.basename(cursor));
    cursor = parent;
  }
}

export interface InstalledCheckOptions {
  /** Default: the process environment. */
  env?: EnvLike;
  /** Default: the running platform. */
  platform?: InstalledPlatform;
  /** Default: `os.homedir()`. */
  home?: string;
  /** Injected roots; replaces the environment lookup. */
  roots?: readonly string[];
  /** Injected path resolution (symlinks, `..`); default resolves on the real file system. */
  canonicalize?: (p: string) => string;
  /** Noun for the message. Default `app`. */
  what?: string;
}

/**
 * Throws {@link InstalledAppRefused} when `appPath` (symlinks and `..` resolved) is under an installed-apps
 * location and installed-QA mode is off. Returns the resolved path otherwise.
 */
export function assertNotInstalledApp(appPath: string, options: InstalledCheckOptions = {}): string {
  const env = options.env ?? process.env;
  const platform = options.platform ?? (process.platform as InstalledPlatform);
  const canonicalize = options.canonicalize ?? canonicalForInstalledCheck;
  const resolved = canonicalize(appPath);
  if (isInstalledQaMode(env)) return resolved;
  const roots = options.roots ?? installedRoots(platform, env, options.home ?? os.homedir()).map(canonicalize);
  const hit = installedRootOf(resolved, roots, platform);
  if (hit) throw new InstalledAppRefused(resolved, hit, options.what ?? 'app');
  return resolved;
}
