// QA runs never touch real user data.
//
// A test run may not start an app on the user's real data or on a location that can raise a system
// dialog (macOS file-access prompts for Documents, Desktop, Downloads, iCloud Drive and removable or
// network volumes; the same cloud-sync folders on Windows). `assertQaLaunchSafe` is evaluated by
// `spawnQaApp` before anything starts, with no opt-out: a violation is a typed `UserDataViolation`
// naming the path and the rule, and nothing is launched. Mirrors the Rust crate's `userdata` module.
//
// Rule for an app launched with a workspace:
//  - the workspace home must be a QA root this runner created (directly under a `rightkit-qa`
//    directory inside a system temp root on the internal disk, with the `.rightkit-app.json`
//    binding) and break none of the forbidden-location rules;
//  - every directory handed to the app (the working directory and each absolute path in the
//    environment) must resolve, symlinks followed, inside that home.
//
// Forbidden anywhere: the user's Documents, Desktop, Downloads, Pictures, Music and Movies; iCloud
// Drive (`~/Library/Mobile Documents`, `~/Library/CloudStorage`), OneDrive, Dropbox and Google Drive
// folders; the user's real application-support / AppData / `.config` directory for the app under
// test; any `/Volumes/*` volume; any Windows UNC (network) path.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { appTempDir } from './storage.js';
import { spawnOwnedProcess, type OwnedProcess, type SpawnOwnedProcessOptions } from './process.js';
import type { QaWorkspace } from './workspace.js';

export type UserDataRule =
  | 'user-folder' | 'cloud-storage' | 'external-volume' | 'real-app-data'
  | 'outside-qa-root' | 'outside-temp' | 'not-a-qa-root' | 'unresolvable' | 'unscrubbed-real-data';

export class UserDataViolation extends Error {
  readonly path: string;
  readonly rule: UserDataRule;
  readonly detail: string;
  constructor(violationPath: string, rule: UserDataRule, detail: string) {
    super(`QA refuses to touch ${violationPath} (rule ${rule}): ${detail}; nothing was launched`);
    this.name = 'UserDataViolation';
    this.path = violationPath;
    this.rule = rule;
    this.detail = detail;
  }
}

const USER_FOLDERS = ['documents', 'desktop', 'downloads', 'pictures', 'music', 'movies', 'videos'];

/** Resolve symlinks in the longest existing prefix and append the rest. */
export function resolveForGuard(input: string): string {
  let base = path.resolve(input);
  const tail: string[] = [];
  for (;;) {
    try { return path.join(fs.realpathSync.native(base), ...tail.reverse()); } catch { /* climb */ }
    const parent = path.dirname(base);
    if (parent === base) return path.resolve(input);
    tail.push(path.basename(base));
    base = parent;
  }
}

function comps(p: string): string[] {
  return p.split(/[\\/]+/).filter((c) => c && !/^[A-Za-z]:$/.test(c) && c !== '?').map((c) => c.toLowerCase());
}

const alnum = (s: string): string => s.toLowerCase().replace(/[^\p{L}\p{N}]/gu, '');

function underHome(rest: string[], app?: string): { rule: UserDataRule; detail: string } | undefined {
  if (rest.length === 0) return { rule: 'user-folder', detail: "the user's home directory" };
  if (USER_FOLDERS.includes(rest[0])) return { rule: 'user-folder', detail: `the user's ${rest[0]} folder` };
  if (rest[0] === 'library' && (rest[1] === 'mobile documents' || rest[1] === 'cloudstorage')) return { rule: 'cloud-storage', detail: 'iCloud Drive / CloudStorage' };
  const needle = app ? alnum(app) : '';
  if (!needle) return undefined;
  const hit = (i: number): boolean => rest[i] !== undefined && alnum(rest[i]).includes(needle);
  const inSet = (set: string[]): boolean => rest[1] !== undefined && set.includes(rest[1]);
  let found = false;
  switch (rest[0]) {
    case 'library': found = inSet(['application support', 'containers', 'group containers', 'preferences', 'caches', 'saved application state', 'webkit', 'logs']) && hit(2); break;
    case 'appdata': found = inSet(['roaming', 'local', 'locallow']) && hit(2); break;
    case '.local': found = inSet(['share', 'state']) && hit(2); break;
    case '.config': case '.cache': found = hit(1); break;
    default: break;
  }
  return found ? { rule: 'real-app-data', detail: "the user's real data directory of the app under test" } : undefined;
}

export interface GuardOptions {
  /** The app under test; its real data directories become forbidden. */
  app?: string;
  /** Override the user homes that are protected (default: os.homedir(), HOME, USERPROFILE). Tests use a fake home. */
  userHomes?: string[];
  /** Override the allowed temp roots (default: the app temp dir, os.tmpdir(), RUNNER_TEMP). */
  tempRoots?: string[];
}

export class QaLaunchGuard {
  readonly app?: string;
  readonly qaRoot?: string;
  private readonly userHomes: string[];
  private readonly tempRoots: string[];

  private constructor(app: string | undefined, qaRoot: string | undefined, options: GuardOptions) {
    this.app = app;
    this.qaRoot = qaRoot;
    const homes = options.userHomes ?? [os.homedir(), process.env.HOME, process.env.USERPROFILE].filter((h): h is string => !!h);
    this.userHomes = [...new Set(homes.map(resolveForGuard))];
    const temps = options.tempRoots ?? [appTempDir(), os.tmpdir(), process.env.RUNNER_TEMP].filter((t): t is string => !!t);
    this.tempRoots = [...new Set(temps.map(resolveForGuard))];
  }

  /** Guard for a launch without a workspace: paths must sit in a system temp root and break no forbidden rule. */
  static forTempRoots(options: GuardOptions = {}): QaLaunchGuard { return new QaLaunchGuard(options.app, undefined, options); }

  /** Guard for an app launched in `workspace`; throws when its home is not a QA root this runner created or is forbidden. */
  static forWorkspace(workspace: Pick<QaWorkspace, 'home'> & Partial<Pick<QaWorkspace, 'appKey'>>, options: GuardOptions = {}): QaLaunchGuard {
    const home = resolveForGuard(workspace.home);
    let binding: { app?: unknown } | undefined;
    try { binding = JSON.parse(fs.readFileSync(path.join(home, '.rightkit-app.json'), 'utf8')); } catch { /* not ours */ }
    if (!binding) throw new UserDataViolation(home, 'not-a-qa-root', 'no .rightkit-app.json binding: not a QA home created by @rightkit/qa');
    if (path.basename(path.dirname(home)) !== 'rightkit-qa') throw new UserDataViolation(home, 'not-a-qa-root', 'the home is not directly under a rightkit-qa directory');
    const app = options.app ?? (typeof binding.app === 'string' ? binding.app : workspace.appKey);
    const guard = new QaLaunchGuard(app, home, { ...options, app });
    guard.judgeOutsideWorkspace(home);
    return guard;
  }

  /** The first forbidden-location rule `resolved` breaks, ignoring any QA root. */
  forbidden(resolved: string): { rule: UserDataRule; detail: string } | undefined {
    if (resolved.startsWith('\\\\') && !/^\\\\\?\\[A-Za-z]:/.test(resolved)) return { rule: 'external-volume', detail: 'a network (UNC) path' };
    const c = comps(resolved);
    if (!/^[A-Za-z]:/.test(resolved) && resolved.startsWith('/') && c[0] === 'volumes') return { rule: 'external-volume', detail: 'an external or network volume under /Volumes' };
    for (let i = 0; i < c.length; i += 1) {
      const name = c[i];
      if (name.startsWith('onedrive') || name.startsWith('dropbox') || name.startsWith('google drive') || name === 'googledrive' || name === 'my drive'
        || name === 'icloud drive' || name === 'iclouddrive' || name === 'mobile documents' || (name === 'cloudstorage' && c[i - 1] === 'library')) {
        return { rule: 'cloud-storage', detail: `a cloud-synced folder (${name})` };
      }
    }
    // `/Users/<name>/...` and `C:\Users\<name>\...` for any account, not only this process's.
    if ((c[0] === 'users' || c[0] === 'home') && c.length > 2) {
      const hit = underHome(c.slice(2), this.app);
      if (hit) return hit;
    }
    for (const home of this.userHomes) {
      const rel = path.relative(home, resolved);
      if (rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel))) {
        const hit = underHome(comps(rel), this.app);
        if (hit) return hit;
      }
    }
    return undefined;
  }

  private judgeOutsideWorkspace(resolved: string): void {
    const hit = this.forbidden(resolved);
    if (hit) throw new UserDataViolation(resolved, hit.rule, hit.detail);
    if (!this.tempRoots.some((t) => isUnder(t, resolved))) throw new UserDataViolation(resolved, 'outside-temp', 'not inside a system temp directory on the internal disk');
  }

  /** Judge one directory the runner would hand to the app. Returns its resolved path. */
  checkDir(input: string): string {
    if (!path.isAbsolute(input) || input.split(/[\\/]+/).includes('..')) throw new UserDataViolation(input, 'unresolvable', 'the path must be absolute and contain no `..`');
    const resolved = resolveForGuard(input);
    if (this.qaRoot) {
      if (isUnder(this.qaRoot, resolved)) return resolved;
      const hit = this.forbidden(resolved);
      if (hit) throw new UserDataViolation(resolved, hit.rule, hit.detail);
      throw new UserDataViolation(resolved, 'outside-qa-root', `not inside the QA root ${this.qaRoot}`);
    }
    this.judgeOutsideWorkspace(resolved);
    return resolved;
  }

  /** Judge every absolute-path value of a launch environment. */
  checkEnv(env: Record<string, string | undefined>): void {
    for (const value of Object.values(env)) if (typeof value === 'string' && path.isAbsolute(value)) this.checkDir(value);
  }
}

function isUnder(root: string, candidate: string): boolean {
  const rel = path.relative(root, candidate);
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}

export interface AssertQaLaunchOptions extends GuardOptions {
  /** Working directory the app will start in. Default: the workspace home. */
  cwd?: string;
  /** Extra environment for the launch (merged over the workspace environment). */
  env?: Record<string, string | undefined>;
  /** Further directories handed to the app (for example `--data-dir` arguments). */
  dirs?: string[];
}

/** Evaluate the guard for a launch in `workspace`. Throws {@link UserDataViolation}; returns the guard when safe. */
export function assertQaLaunchSafe(workspace: QaWorkspace, options: AssertQaLaunchOptions = {}): QaLaunchGuard {
  const guard = QaLaunchGuard.forWorkspace(workspace, options);
  guard.checkDir(workspace.home);
  guard.checkDir(options.cwd ?? workspace.home);
  guard.checkEnv({ ...workspace.env, ...(options.env ?? {}) });
  for (const dir of options.dirs ?? []) guard.checkDir(dir);
  return guard;
}

export interface SpawnQaAppOptions extends Omit<SpawnOwnedProcessOptions, 'cwd' | 'env'>, GuardOptions {
  cwd?: string;
  /** Merged over the workspace environment, which is always applied. */
  env?: Record<string, string | undefined>;
  dirs?: string[];
}

/**
 * Launch an app in a QA workspace after the guard passes. The workspace environment always applies;
 * the parent process environment is passed through except HOME-like keys, which the workspace owns.
 */
export function spawnQaApp(workspace: QaWorkspace, command: string, args: string[] = [], options: SpawnQaAppOptions = {}): OwnedProcess {
  const { cwd, env, dirs, app, userHomes, tempRoots, ...rest } = options;
  assertQaLaunchSafe(workspace, { cwd, env, dirs, app, userHomes, tempRoots });
  return spawnOwnedProcess(command, args, { ...rest, cwd: cwd ?? workspace.home, env: { ...process.env, ...workspace.env, ...(env ?? {}) } as NodeJS.ProcessEnv });
}

export interface ScrubDeclaration {
  /** The scenario states that its transform removes all user data. */
  declaredScrubbed: boolean;
  /** Called on the seeded copy before launch; must return true (for example "zero user rows"). */
  verify: (dest: string) => boolean | Promise<boolean>;
}

export interface SeedFromOptions extends GuardOptions {
  /** Destination relative to the workspace data directory. */
  destRel: string;
  /** Caller-supplied copy/transform from `source` to `dest`. */
  transform: (source: string, dest: string) => void | Promise<void>;
  scrub?: ScrubDeclaration;
}

/**
 * Seed `<dataDir>/<destRel>` from `source` through the caller's `transform`. Generated fixtures are the
 * default and need no scrub. A `source` that is the real per-user data of the app (or in any forbidden
 * location) is refused unless `scrub` declares the copy scrubbed and its callback returns true on the
 * result; when it returns false the copy is deleted and the seed throws, so nothing launches on it.
 */
export async function seedFrom(workspace: QaWorkspace, source: string, options: SeedFromOptions): Promise<string> {
  if (path.isAbsolute(options.destRel) || options.destRel.split(/[\\/]+/).includes('..')) throw new Error('seed destination must be relative to the QA data directory');
  const guard = QaLaunchGuard.forWorkspace(workspace, options);
  const dest = guard.checkDir(path.join(workspace.dataDir, options.destRel));
  const resolvedSource = resolveForGuard(source);
  const real = guard.forbidden(resolvedSource);
  const scrub = options.scrub;
  if (real) {
    if (!scrub) throw new UserDataViolation(resolvedSource, 'unscrubbed-real-data', `${real.detail}; use a generated fixture, or declare the copy scrubbed and supply a verification callback`);
    if (!scrub.declaredScrubbed) throw new UserDataViolation(resolvedSource, 'unscrubbed-real-data', `${real.detail}; the scenario did not declare the copy scrubbed`);
  }
  await options.transform(source, dest);
  if (scrub && (real || scrub.declaredScrubbed)) {
    if (!(await scrub.verify(dest))) {
      // The copy lives inside the QA data directory the guard just verified.
      fs.rmSync(dest, { recursive: true, force: true });
      throw new UserDataViolation(dest, 'unscrubbed-real-data', 'the verification callback rejected the seeded copy; it was deleted and nothing launches');
    }
  }
  return dest;
}

const SYSTEM_DIALOG_OWNERS = ['usernotificationcenter', 'coreservicesuiagent', 'universalaccessauthwarn', 'securityagent', 'tccd'];

export function isSystemDialogOwner(name: string): boolean { return SYSTEM_DIALOG_OWNERS.includes(name.toLowerCase()); }

/**
 * Positive evidence only: the first sampled foreground application name that belongs to a macOS system
 * permission-dialog agent. An empty answer proves nothing, because these agents rarely become frontmost.
 */
export function permissionDialogIn(frontmostNames: readonly string[]): string | undefined { return frontmostNames.find(isSystemDialogOwner); }

export type DialogCheck = { status: 'clear' } | { status: 'dialog'; owner: string } | { status: 'unsupported'; reason: string };

/**
 * TODO: observe live TCC prompts. This package launches no native app and has no window-by-owner
 * enumeration, so no reliable detector exists here. The hook reports `unsupported` instead of guessing;
 * the Rust crate's `Control` is the surface that can grow a real detector.
 */
export function livePermissionDialogCheck(_appPid: number): DialogCheck {
  return { status: 'unsupported', reason: 'no window-by-owner enumeration; permission dialogs are not observable from Node' };
}

/** Fail the scenario when a system permission dialog is known to be showing; a no-op while the check is unsupported. */
export function assertNoPermissionDialog(appPid: number, frontmostNames: readonly string[] = []): void {
  const seen = permissionDialogIn(frontmostNames);
  if (seen) throw new Error(`a system permission dialog (${seen}) appeared for the app under test`);
  const live = livePermissionDialogCheck(appPid);
  if (live.status === 'dialog') throw new Error(`a system permission dialog (${live.owner}) appeared for the app under test`);
}
