// Windows class-1 platform. Evidence children run through `rightkit-sandbox-exec`, which starts the
// command inside an AppContainer whose only filesystem grants are the protected build root (read-only)
// and explicit writable roots. Network is denied (no internetClient capability).
//
// Implements the same platform surface as createMacosClass1Platform: createProtectedBuildRoot,
// protectedInvocation and spawnProtectedChild. Never spawns anything except through spawnProtectedChild.
import { spawn as nodeSpawn } from 'node:child_process';
import { stat, rm } from 'node:fs/promises';
import { win32 } from 'node:path';
import { digestInputClosure } from './input-closure.mjs';
import { createProtectedBuildRoot as createWindowsBuildRoot } from './windows-build-root.mjs';

function classError(code, message) { return Object.assign(new Error(`${code}: ${message}`), { code }); }

/** Lexical containment with Windows semantics; equal paths count as inside. */
function isInside(parent, child) {
  const rel = win32.relative(parent, child);
  return rel === '' || (rel !== '..' && !rel.startsWith(`..${win32.sep}`) && !win32.isAbsolute(rel));
}

function sameWindowsPath(left, right) { return win32.normalize(left).toLowerCase() === win32.normalize(right).toLowerCase(); }

async function requireSandboxExec(sandboxExec) {
  if (typeof sandboxExec !== 'string' || !win32.isAbsolute(sandboxExec)) {
    throw classError('RK_CLASS1_SANDBOX_MISSING', 'sandboxExec must be an absolute path to rightkit-sandbox-exec');
  }
  const info = await stat(sandboxExec).catch(() => null);
  if (!info?.isFile()) throw classError('RK_CLASS1_SANDBOX_MISSING', `${sandboxExec} is not an existing file`);
}

/** Closure entries are relative, forward-slash paths (the same shape the macOS platform accepts). */
function validateClosureFiles(files) {
  if (!Array.isArray(files) || files.length === 0) throw classError('RK_CLASS1_INPUT', 'files must be a nonempty array');
  for (const file of files) {
    if (typeof file !== 'string' || !file || file.startsWith('/') || file.includes('\\') || file.split('/').some((part) => !part || part === '.' || part === '..')) {
      throw classError('RK_CLASS1_INPUT', `invalid closure path: ${JSON.stringify(file)}`);
    }
  }
  return files;
}

/** Child environment, passed to rightkit-sandbox-exec as `--env KEY=VALUE`. Keys are identifiers; values are NUL-free. */
function validateEnv(env) {
  if (env === null || typeof env !== 'object' || Array.isArray(env)) throw classError('RK_CLASS1_ENV_INVALID', 'env must be a plain object');
  return Object.entries(env).map(([key, value]) => {
    if (!/^[A-Za-z_][A-Za-z0-9_()]*$/.test(key) || typeof value !== 'string' || value.includes('\0')) {
      throw classError('RK_CLASS1_ENV_INVALID', `invalid environment entry: ${JSON.stringify(key)}`);
    }
    return `${key}=${value}`;
  });
}

/**
 * Builds the Windows class-1 platform. `sandboxExec` is the absolute path to rightkit-sandbox-exec.
 * `serviceSid`, `agentSid` and `copyCostApproval` are trusted broker configuration forwarded to the
 * Windows snapshot helper; without them that helper fails closed.
 */
export function createWindowsClass1Platform({
  sandboxExec,
  spawn = nodeSpawn,
  digest = digestInputClosure,
  serviceSid,
  agentSid,
  copyCostApproval = { windowsLead: false, macLead: false },
  buildRootFactory = createWindowsBuildRoot,
  removeRoot = (root) => rm(root, { recursive: true, force: true }),
} = {}) {
  async function createProtectedBuildRoot({ sourceRoot, files, buildRootParent, id }) {
    await requireSandboxExec(sandboxExec);
    if (!/^[A-Za-z0-9._-]{1,128}$/.test(id ?? '')) throw classError('RK_CLASS1_INVALID_ID', 'build root id is malformed');
    if (typeof sourceRoot !== 'string' || !win32.isAbsolute(sourceRoot)) throw classError('RK_CLASS1_INVALID_ROOT', 'sourceRoot must be absolute');
    const closure = validateClosureFiles(files);
    const inputs = closure.map((file) => win32.join(sourceRoot, ...file.split('/')));
    // The snapshot helper names the root itself; `id` is validated for parity with the macOS platform.
    const snapshot = await buildRootFactory({ inputs, root: buildRootParent, repositoryRoot: sourceRoot, serviceSid, agentSid, includeGit: false, copyCostApproval });
    const root = snapshot.buildRoot;
    const cloned = snapshot.manifest.map((entry) => entry.path);
    let released = false;
    const release = async () => {
      if (released) return;
      released = true;
      await removeRoot(root);
    };
    try {
      const verdict = await digest(root, cloned);
      return Object.freeze({ root, digest: verdict.digest, files: Object.freeze(cloned), method: snapshot.method, release });
    } catch (error) {
      // A root that cannot be digested is never evidence; do not leave it behind.
      await release().catch(() => {});
      throw error;
    }
  }

  /** Returns { command, args, cwd } for the confined child. Never spawns. */
  async function protectedInvocation({ buildRoot, clone, command, args = [], cwd = buildRoot, env = {}, writableRoots = [], temporaryRoots = [] }) {
    await requireSandboxExec(sandboxExec);
    if (typeof buildRoot !== 'string' || !win32.isAbsolute(buildRoot)) throw classError('RK_CLASS1_INVALID_ROOT', 'buildRoot must be absolute');
    if (typeof command !== 'string' || !win32.isAbsolute(command)) throw classError('RK_CLASS1_COMMAND_INVALID', 'protected child command must be absolute');
    if (!Array.isArray(args) || args.some((arg) => typeof arg !== 'string' || arg.includes('\0'))) throw classError('RK_CLASS1_COMMAND_INVALID', 'arguments must be NUL-free strings');
    if (typeof cwd !== 'string' || !win32.isAbsolute(cwd) || !isInside(buildRoot, cwd)) throw classError('RK_CLASS1_CWD_OUTSIDE', 'protected child cwd must be absolute and lie inside the build root');
    const envFlags = validateEnv(env).flatMap((entry) => ['--env', entry]);
    if (!Array.isArray(writableRoots) || !Array.isArray(temporaryRoots)) throw classError('RK_CLASS1_PROFILE_INVALID', 'writable roots must be arrays');
    for (const dir of [...writableRoots, ...temporaryRoots]) {
      if (typeof dir !== 'string' || !win32.isAbsolute(dir)) throw classError('RK_CLASS1_PROFILE_INVALID', `writable root must be absolute: ${JSON.stringify(dir)}`);
      // A writable grant on the build root or any ancestor would let the child rewrite the evidence tree.
      if (isInside(buildRoot, dir)) throw classError('RK_CLASS1_PROFILE_INVALID', 'writable roots may not lie inside the build root');
      if (isInside(dir, buildRoot)) throw classError('RK_CLASS1_PROFILE_INVALID', 'writable roots may not contain the build root');
    }
    if (clone) {
      if (!sameWindowsPath(clone.root, buildRoot)) throw classError('RK_CLASS1_CLONE_MUTATED', 'clone does not match the build root');
      const current = await digest(clone.root, clone.files);
      if (current.digest !== clone.digest) throw classError('RK_CLASS1_CLONE_MUTATED', 'build root changed after admission');
    }
    const writes = [...writableRoots, ...temporaryRoots].flatMap((dir) => ['--writable', dir]);
    return Object.freeze({
      command: sandboxExec,
      args: Object.freeze(['--read-only', buildRoot, ...writes, '--cwd', cwd, ...envFlags, '--', command, ...args]),
      cwd,
    });
  }

  async function spawnProtectedChild({ env = {}, stdio = ['ignore', 'pipe', 'pipe'], ...input }) {
    const invocation = await protectedInvocation({ ...input, env });
    // The launcher itself gets only what it needs to start; the child's environment travels as --env flags.
    const launcherEnv = process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {};
    // Run the launcher from its own directory so the build root is never held open as the process cwd.
    return spawn(invocation.command, invocation.args, { cwd: win32.dirname(invocation.command), env: launcherEnv, stdio, windowsHide: true });
  }

  return Object.freeze({
    platform: 'win32',
    protection: Object.freeze({ mechanism: 'appcontainer', network: 'denied', sandbox: sandboxExec }),
    createProtectedBuildRoot,
    protectedInvocation,
    spawnProtectedChild,
  });
}
