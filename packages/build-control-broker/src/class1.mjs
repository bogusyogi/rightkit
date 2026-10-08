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
import { spawn as nodeSpawn } from 'node:child_process';
import { constants as fsConstants } from 'node:fs';
import { copyFile, chmod, lstat, mkdir, readlink, realpath, rm, stat, symlink } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { digestInputClosure, normalizeClosurePath } from './input-closure.mjs';

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

/** Clone-then-hash. Refuses cross-volume roots (clonefile needs one APFS volume) and fails closed when cloning is unsupported. */
export async function createCloneRoot({ sourceRoot, files, buildRootParent, id, fs = {} }) {
  const ops = { stat, mkdir, copyFile, chmod, symlink, lstat, rm, realpath, ...fs };
  if (!/^[A-Za-z0-9._-]{1,128}$/.test(id ?? '')) throw classError('RK_CLASS1_INVALID_ID', 'build root id is malformed');
  const normalized = await validateClosure({ sourceRoot, files, lstatFn: ops.lstat, realpathFn: ops.realpath });
  const sourceReal = await ops.realpath(sourceRoot);
  const [sourceStat, parentStat] = await Promise.all([ops.stat(sourceReal), ops.stat(buildRootParent)]);
  if (sourceStat.dev !== parentStat.dev) throw classError('RK_CLASS1_CROSS_VOLUME', 'build root parent must be on the same volume as the source root (clonefile)');
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
        await ops.copyFile(from, to, fsConstants.COPYFILE_FICLONE_FORCE);
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

export function createMacosClass1Platform({ spawn = nodeSpawn, sandboxExec = SANDBOX_EXEC, exists = (path) => stat(path).then(() => true, () => false), digest = digestInputClosure, fs = {} } = {}) {
  return Object.freeze({
    platform: 'darwin',
    async createProtectedBuildRoot({ sourceRoot, files, buildRootParent, id }) {
      return createCloneRoot({ sourceRoot, files, buildRootParent, id, fs });
    },
    async spawnProtectedChild({ buildRoot, clone, command, args = [], cwd = buildRoot, env = {}, writableRoots = [], temporaryRoots = [], stdio = ['ignore', 'pipe', 'pipe'] }) {
      if (!isAbsolute(command)) throw classError('RK_CLASS1_COMMAND_INVALID', 'protected child command must be absolute');
      if (!isInside(buildRoot, cwd)) throw classError('RK_CLASS1_CWD_OUTSIDE', 'protected child cwd must lie inside the build root');
      if (!(await exists(sandboxExec))) throw classError('RK_CLASS1_SANDBOX_MISSING', `${sandboxExec} is required for class-1 children`);
      if (clone) await revalidateClone(clone, { digest });
      const profile = seatbeltProfile({ buildRoot, writableRoots, temporaryRoots });
      return spawn(sandboxExec, ['-p', profile, '--', command, ...args], { cwd, env, stdio, windowsHide: true });
    },
  });
}

/** Platform dispatcher. Windows (and any unregistered platform) gets the typed unsupported default. */
export function createClass1Platform({ platform = process.platform, registry = null, macos = undefined } = {}) {
  const implementations = { darwin: macos ?? (() => createMacosClass1Platform()), ...(registry ?? {}) };
  const impl = implementations[platform];
  const built = typeof impl === 'function' ? impl() : impl;
  if (built && typeof built.createProtectedBuildRoot === 'function' && typeof built.spawnProtectedChild === 'function') return Object.freeze({ platform, supported: true, ...built });
  const refuse = async () => { throw class1Unsupported(platform); };
  return Object.freeze({ platform, supported: false, createProtectedBuildRoot: refuse, spawnProtectedChild: refuse });
}

/** Admission gate: class-1 work fails closed unless the host provides protection. */
export async function admitClass1({ platform, input, evidenceClass = 'class2' }) {
  if (evidenceClass !== 'class1') return { admitted: false, evidenceClass, reason: 'not an evidence build' };
  if (!platform?.supported) return { admitted: false, evidenceClass, code: CLASS1_UNSUPPORTED, reason: CLASS1_MESSAGE };
  try {
    const clone = await platform.createProtectedBuildRoot(input);
    return { admitted: true, evidenceClass, clone };
  } catch (error) {
    return { admitted: false, evidenceClass, code: error.code ?? 'RK_CLASS1_FAILED', reason: error.message };
  }
}
