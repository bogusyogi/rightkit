// Class 1 (evidence builds: enforce admission, release, seal, publish, protected
// acceptance). The build reads only from a broker-owned clone of the supported
// input closure, and children run under a kernel write denial on that clone.
//
// Platform interface (every platform implements exactly these two functions):
//   createProtectedBuildRoot({ sourceRoot, files, buildRootParent, id }) -> { root, digest, release() }
//   spawnProtectedChild({ buildRoot, clone, command, args, cwd, env, writableRoots, temporaryRoots, stdio }) -> ChildProcess
//   Children write only to writableRoots (target/OUT_DIR, cargo home, git db, sccache) and temporaryRoots.
//   The caller owns clone.release(); call it only after every child has exited.
// Other platforms receive the typed unsupported default and fail closed.
//
// Closure decision: `.git` is never part of the class-1 input closure. Inputs are
// the git-listed files only, so repository metadata cannot alter a build.
import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { chmod, lstat, mkdir, readlink, realpath, rm, stat, symlink } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { promisify } from 'node:util';
import { digestInputClosure, INPUTS_MUTATED, INPUTS_VERIFIED, normalizeClosurePath, recordInputClosure } from './input-closure.mjs';

const execFile = promisify(nodeExecFile);
export const CLASS1_CLONE_UNSUPPORTED = 'RK_CLASS1_CLONE_UNSUPPORTED';
export const CLASS1_HELPER_UNTRUSTED = 'RK_CLASS1_HELPER_UNTRUSTED';
export const CLASS1_PROTECTION = Object.freeze({ mechanism: 'apfs-clonefile+seatbelt', clone: 'clonefile-nofollow', fallbackCopy: false });

export const CLASS1_UNSUPPORTED = 'RK_CLASS1_UNSUPPORTED_ON_HOST';
export const CLASS1_MESSAGE = 'class1-unsupported-on-this-host';
export const SANDBOX_EXEC = '/usr/bin/sandbox-exec';

export function class1Unsupported(platform) {
  return Object.assign(new Error(`${CLASS1_MESSAGE} (platform ${platform})`), { code: CLASS1_UNSUPPORTED, platform });
}

function classError(code, message) { return Object.assign(new Error(message), { code }); }

/** Validates the closure, rejects symlinks that escape sourceRoot and special files. Returns normalized paths. */
export async function validateClosure({ sourceRoot, files, lstatFn = lstat, realpathFn = realpath }) {
  if (!isAbsolute(sourceRoot)) throw classError('RK_CLASS1_INVALID_ROOT', 'sourceRoot must be absolute');
  const rootReal = await realpathFn(sourceRoot);
  const normalized = [...new Set(files.map((path) => normalizeClosurePath(path)))].sort();
  for (const path of normalized) {
    const absolute = join(rootReal, path);
    const info = await lstatFn(absolute).catch((error) => { if (error.code === 'ENOENT') throw classError('RK_CLASS1_INPUT_MISSING', `closure input is missing: ${path}`); throw error; });
    if (info.isSymbolicLink()) {
      const target = resolve(dirname(absolute), await readlink(absolute));
      const targetReal = await realpathFn(target).catch(() => null);
      if (!targetReal || !isInside(rootReal, targetReal)) throw classError('RK_CLASS1_SYMLINK_ESCAPE', `closure symlink escapes the source root: ${path}`);
    } else if (!info.isFile()) {
      throw classError('RK_CLASS1_UNSUPPORTED_INPUT', `closure input is not a regular file or in-root symlink: ${path}`);
    }
  }
  return normalized;
}

export function isInside(parent, child) {
  const rel = relative(parent, child);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

/**
 * Seatbelt profile (write allowlist). Reads are allowed everywhere; writes are denied except device nodes and the
 * managed roots (target/OUT_DIR, cargo home, git db, sccache, temp). The build root is denied last, so it stays
 * read-only even if a managed root is an ancestor of it. Last matching rule wins in SBPL.
 */
export function seatbeltProfile({ buildRoot, writableRoots = [], temporaryRoots = [] }) {
  const quote = (path) => {
    if (typeof path !== 'string' || !isAbsolute(path) || /["\\\u0000-\u001f]/.test(path)) throw classError('RK_CLASS1_PROFILE_INVALID', `profile path is unsafe: ${JSON.stringify(path)}`);
    return `(subpath "${path}")`;
  };
  const allowed = [...writableRoots, ...temporaryRoots];
  for (const root of allowed) if (isInside(buildRoot, root)) throw classError('RK_CLASS1_PROFILE_INVALID', 'writable roots may not lie inside the build root');
  return [
    '(version 1)',
    '(allow default)',
    '(deny file-write*)',
    '(allow file-write* (literal "/dev/null") (literal "/dev/zero") (literal "/dev/tty") (regex #"^/dev/fd/"))',
    ...allowed.map((root) => `(allow file-write* ${quote(root)})`),
    `(deny file-write* ${quote(buildRoot)})`,
    '',
  ].join('\n');
}

/**
 * Clone-then-hash. `fs.cloneFile(from, to)` is the only clone primitive: it must create a copy-on-write clone or throw.
 * Refuses cross-volume roots (clonefile needs one APFS volume) and fails closed when no clone primitive is configured.
 */
export async function createCloneRoot({ sourceRoot, files, buildRootParent, id, fs = {} }) {
  const ops = { stat, mkdir, chmod, symlink, lstat, rm, realpath, ...fs };
  if (!/^[A-Za-z0-9._-]{1,128}$/.test(id ?? '')) throw classError('RK_CLASS1_INVALID_ID', 'build root id is malformed');
  const normalized = await validateClosure({ sourceRoot, files, lstatFn: ops.lstat, realpathFn: ops.realpath });
  const sourceReal = await ops.realpath(sourceRoot);
  const [sourceStat, parentStat] = await Promise.all([ops.stat(sourceReal), ops.stat(buildRootParent)]);
  if (sourceStat.dev !== parentStat.dev) throw classError('RK_CLASS1_CROSS_VOLUME', 'build root parent must be on the same volume as the source root (clonefile)');
  if (typeof ops.cloneFile !== 'function') throw classError('RK_CLASS1_CLONE_UNSUPPORTED', 'no clone helper is configured; class-1 clones fail closed');
  const before = await digestInputClosure(sourceReal, normalized);
  const root = join(buildRootParent, id);
  await ops.mkdir(root, { mode: 0o700 });
  let released = false;
  const release = async () => { if (!released) { released = true; await ops.rm(root, { recursive: true, force: true }); } };
  try {
    for (const path of normalized) {
      const from = join(sourceReal, path); const to = join(root, path);
      await ops.mkdir(dirname(to), { recursive: true, mode: 0o700 });
      const info = await ops.lstat(from);
      if (info.isSymbolicLink()) {
        const target = resolve(dirname(from), await readlink(from));
        const targetReal = await ops.realpath(target);
        // Re-point the link inside the clone; the target was already proven to lie inside sourceRoot.
        await ops.symlink(relative(dirname(to), join(root, relative(sourceReal, targetReal))), to);
      } else {
        await ops.cloneFile(from, to);
        await ops.chmod(to, 0o444);
      }
    }
    const after = await digestInputClosure(root, normalized);
    const source = await digestInputClosure(sourceReal, normalized);
    if (source.digest !== before.digest) throw classError('RK_CLASS1_INPUTS_MUTATED', 'source inputs changed while cloning');
    return Object.freeze({ root, digest: after.digest, files: normalized, release });
  } catch (error) {
    await release().catch(() => {});
    if (error.code === 'ENOTSUP' || error.code === 'EXDEV' || error.code === 'ENOSYS') throw classError('RK_CLASS1_CLONE_UNSUPPORTED', `clonefile unavailable on this volume: ${error.message}`);
    throw error;
  }
}

/** Re-digest before spawn; a clone whose digest moved is never built from. */
export async function revalidateClone(clone, { digest = digestInputClosure } = {}) {
  const current = await digest(clone.root, clone.files);
  if (current.digest !== clone.digest) throw classError('RK_CLASS1_CLONE_MUTATED', 'build root changed after admission');
  return true;
}

/**
 * Shared child preparation: validates the request, re-digests the clone (defence in depth) and returns the
 * Seatbelt invocation. Callers spawn the returned command; they never run `command` directly.
 */
async function protectedInvocation({ sandboxExec, exists, digest, buildRoot, clone, command, args = [], cwd = buildRoot, writableRoots = [], temporaryRoots = [] }) {
  if (!isAbsolute(command)) throw classError('RK_CLASS1_COMMAND_INVALID', 'protected child command must be absolute');
  if (!isInside(buildRoot, cwd)) throw classError('RK_CLASS1_CWD_OUTSIDE', 'protected child cwd must lie inside the build root');
  if (!(await exists(sandboxExec))) throw classError('RK_CLASS1_SANDBOX_MISSING', `${sandboxExec} is required for class-1 children`);
  if (clone) await revalidateClone(clone, { digest });
  const profile = seatbeltProfile({ buildRoot, writableRoots, temporaryRoots });
  return Object.freeze({ command: sandboxExec, args: ['-p', profile, '--', command, ...args], cwd });
}

export function createMacosClass1Platform({ spawn = nodeSpawn, sandboxExec = SANDBOX_EXEC, exists = (path) => stat(path).then(() => true, () => false), digest = digestInputClosure, fs = {}, helper = null } = {}) {
  const sandbox = { sandboxExec, exists, digest };
  return Object.freeze({
    platform: 'darwin',
    protection: Object.freeze({ ...CLASS1_PROTECTION, sandbox: sandboxExec, helper }),
    async createProtectedBuildRoot({ sourceRoot, files, buildRootParent, id }) {
      return createCloneRoot({ sourceRoot, files, buildRootParent, id, fs });
    },
    /** Returns { command, args, cwd } for spawning a child inside the protected clone. Never spawns. */
    async protectedInvocation(input) {
      return protectedInvocation({ ...sandbox, ...input });
    },
    async spawnProtectedChild({ env = {}, stdio = ['ignore', 'pipe', 'pipe'], ...input }) {
      const invocation = await protectedInvocation({ ...sandbox, ...input });
      return spawn(invocation.command, invocation.args, { cwd: invocation.cwd, env, stdio, windowsHide: true });
    },
  });
}

/**
 * Clone primitive backed by the `rightkit-clonefile` helper. The helper only ever clones (CLONE_NOFOLLOW);
 * its exit status maps to a typed class-1 code. Any other failure (including a missing binary) fails closed.
 */
export function cloneFileWithHelper(helper, { run = (file, argv) => execFile(file, argv, { maxBuffer: 1024 * 1024 }) } = {}) {
  if (!isAbsolute(helper)) throw classError(CLASS1_CLONE_UNSUPPORTED, 'clone helper path must be absolute');
  return async (from, to) => {
    try {
      await run(helper, [from, to]);
    } catch (error) {
      const detail = String(error?.stderr ?? '').trim().split('\n').at(-1) ?? '';
      if (error?.code === 'ENOENT') throw classError(CLASS1_CLONE_UNSUPPORTED, `clone helper is missing: ${helper}`);
      if (error?.code === 3) throw classError(CLASS1_CLONE_UNSUPPORTED, `clonefile unsupported for ${to}: ${detail}`);
      if (error?.code === 4) throw classError('RK_CLASS1_CROSS_VOLUME', `clone crosses volumes: ${detail}`);
      if (error?.code === 5) throw classError('RK_CLASS1_CLONE_SYMLINK_REFUSED', `clone source is a symlink: ${from}`);
      if (error?.code === 6) throw classError('RK_CLASS1_UNSUPPORTED_INPUT', `clone source is not a regular file: ${from}`);
      throw classError('RK_CLASS1_CLONE_FAILED', `clone helper failed (${error?.code ?? 'unknown'}): ${detail || error?.message}`);
    }
  };
}

/**
 * The helper must live inside the installed protected runtime and be root-owned, with no group or other write bit on
 * it or on any directory from it up to the runtime root. A user-writable or relocated binary is refused.
 */
export async function verifyCloneHelper({ helper, protectedRoot, stat: statFn = stat, realpath: realpathFn = realpath }) {
  if (!isAbsolute(helper ?? '') || !isAbsolute(protectedRoot ?? '')) throw classError(CLASS1_CLONE_UNSUPPORTED, 'clone helper and protected runtime root must be absolute');
  const rootReal = await realpathFn(protectedRoot).catch(() => null);
  const helperReal = await realpathFn(helper).catch(() => null);
  if (!helperReal) throw classError(CLASS1_CLONE_UNSUPPORTED, `clone helper is missing: ${helper}`);
  if (!rootReal || helperReal === rootReal || !isInside(rootReal, helperReal)) throw classError(CLASS1_HELPER_UNTRUSTED, 'clone helper must lie inside the installed protected runtime');
  const helperInfo = await statFn(helperReal);
  if (!helperInfo.isFile()) throw classError(CLASS1_HELPER_UNTRUSTED, 'clone helper is not a regular file');
  const dirs = [];
  for (let dir = dirname(helperReal); ; dir = dirname(dir)) { dirs.push(dir); if (dir === rootReal || dir === dirname(dir)) break; }
  for (const path of [helperReal, ...dirs]) {
    const info = path === helperReal ? helperInfo : await statFn(path);
    if (info.uid !== 0) throw classError(CLASS1_HELPER_UNTRUSTED, `protected runtime path is not root-owned: ${path}`);
    if ((info.mode & 0o022) !== 0) throw classError(CLASS1_HELPER_UNTRUSTED, `protected runtime path is group- or world-writable: ${path}`);
  }
  if ((helperInfo.mode & 0o111) === 0) throw classError(CLASS1_HELPER_UNTRUSTED, 'clone helper is not executable');
  return helperReal;
}

/**
 * Builds the macOS platform from the broker config. Any missing or untrusted helper yields an unsupported platform
 * that records the reason; admission then fails closed with that code instead of silently disabling class 1.
 */
export async function createMacosClass1PlatformFromConfig({ helper, protectedRoot, sandboxExec = SANDBOX_EXEC, stat: statFn, realpath: realpathFn, run, spawn, exists, digest } = {}) {
  try {
    if (!helper || !protectedRoot) throw classError(CLASS1_CLONE_UNSUPPORTED, 'class1.cloneHelper and class1.protectedRoot are required on darwin');
    const verified = await verifyCloneHelper({ helper, protectedRoot, stat: statFn, realpath: realpathFn });
    return createMacosClass1Platform({ spawn, sandboxExec, exists, digest, helper: verified, fs: { cloneFile: cloneFileWithHelper(verified, { run }) } });
  } catch (error) {
    const refuse = async () => { throw error; };
    return Object.freeze({ platform: 'darwin', supported: false, failure: error.code ?? CLASS1_CLONE_UNSUPPORTED, createProtectedBuildRoot: refuse, protectedInvocation: refuse, spawnProtectedChild: refuse });
  }
}

/** Platform dispatcher. Windows (and any unregistered platform) gets the typed unsupported default. */
export function createClass1Platform({ platform = process.platform, registry = null, macos = undefined } = {}) {
  const implementations = { darwin: macos ?? (() => createMacosClass1Platform()), ...(registry ?? {}) };
  const impl = implementations[platform];
  const built = typeof impl === 'function' ? impl() : impl;
  if (built && typeof built.createProtectedBuildRoot === 'function' && typeof built.spawnProtectedChild === 'function' && typeof built.protectedInvocation === 'function') return Object.freeze({ platform, supported: true, ...built });
  const refuse = async () => { throw class1Unsupported(platform); };
  return Object.freeze({ platform, supported: false, createProtectedBuildRoot: refuse, spawnProtectedChild: refuse, protectedInvocation: refuse });
}

/** Admission gate: class-1 work fails closed unless the host provides protection. */
export async function admitClass1({ platform, input, evidenceClass = 'class2' }) {
  if (evidenceClass !== 'class1') return { admitted: false, evidenceClass, reason: 'not an evidence build' };
  if (!platform?.supported) return { admitted: false, evidenceClass, code: platform?.failure ?? CLASS1_UNSUPPORTED, reason: platform?.failure ? `${CLASS1_MESSAGE}: ${platform.failure}` : CLASS1_MESSAGE };
  try {
    const clone = await platform.createProtectedBuildRoot(input);
    return { admitted: true, evidenceClass, clone };
  } catch (error) {
    return { admitted: false, evidenceClass, code: error.code ?? 'RK_CLASS1_FAILED', reason: error.message };
  }
}

/** Platform for the broker config: darwin uses the verified helper, every other host gets the typed unsupported default. */
export async function class1PlatformFromConfig(class1Config, { platform = process.platform, ...deps } = {}) {
  if (platform !== 'darwin') return createClass1Platform({ platform });
  return createMacosClass1PlatformFromConfig({ helper: class1Config?.cloneHelper, protectedRoot: class1Config?.protectedRoot, ...deps });
}

/** Defence in depth: re-digest the clone after every child exits. A moved clone can never be evidence. */
export async function verifyClone(clone, { digest = digestInputClosure } = {}) {
  const after = await digest(clone.root, clone.files);
  const status = after.digest === clone.digest ? INPUTS_VERIFIED : INPUTS_MUTATED;
  return Object.freeze({ status, preDigest: clone.digest, postDigest: after.digest, changed: [], added: [], removed: [], extrasChanged: [] });
}

/** Adds the class-1 evidence fields to a receipt. A moved clone also marks the receipt inputs-mutated (never promotable). */
export function recordClass1Evidence(receipt, { cloneDigest = null, protection = null, verdict = null } = {}) {
  const evidenced = Object.freeze({ ...receipt, evidenceClass: 'class1', cloneDigest, protection });
  return verdict?.status === INPUTS_MUTATED ? recordInputClosure(evidenced, verdict) : evidenced;
}
