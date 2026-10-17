import { discoverBrokerSocket, managedControlCandidates, resolveManagedRoots } from './managed-roots.mjs';
import { randomUUID } from "node:crypto";
import { homedir } from "node:os";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export function managedRuntimeClient(env = process.env, platform = process.platform, exists = existsSync) {
  const roots = managedControlCandidates({ env, platform });
  const paths = platform === 'win32' ? path.win32 : path.posix;
  // runtime-current ships the native `rustright` client.
  for (const root of roots) {
    const client = paths.join(root, "runtime-current", platform === 'win32' ? "rustright.exe" : "rustright");
    if (exists(client)) return client;
  }
  // RustRight's own install (2026-10-17 cutover): the Mac agent-bin shims exec this client directly.
  if (platform === 'darwin') {
    const client = path.posix.join(env.HOME || homedir(), 'Library', 'Application Support', 'RustRight', 'runtime', 'rustright');
    if (exists(client)) return client;
  }
  return undefined;
}

/** Command line that runs the managed client: the native rustright client runs directly. */
export function managedRuntimeCommand(client, args) {
  return { command: client, args };
}

/** Hold a client-run lease that blocks broker runtime cutover until release() (see
 * proveDevelopmentRunsDrained). No managed runtime → no lease; lease I/O failure warns and proceeds. */
export function acquireManagedRunLease({ command, env = process.env, platform = process.platform, exists = existsSync, pid = process.pid } = {}) {
  const client = managedRuntimeClient(env, platform, exists);
  if (!client) return { path: null, release() {} };
  const runs = path.join(resolveManagedRoots({ env, platform }).dataRoot, "rightkit-build-control", "global", "runs");
  const file = path.join(runs, `${pid}-${randomUUID()}.json`);
  try {
    mkdirSync(runs, { recursive: true });
    writeFileSync(file, `${JSON.stringify({ pid, command, startedAt: new Date().toISOString() })}\n`, { flag: "wx" });
  } catch (error) {
    console.warn(`right-release: could not record managed run lease (${error.message}); runtime cutover is not blocked for this run`);
    return { path: null, release() {} };
  }
  return { path: file, release() { rmSync(file, { force: true }); } };
}

export function managedHost(env) {
  return Boolean(env.RIGHTKIT_BUILD_BROKER_SOCKET)
    || /(?:^|[;:])[^;:]*[/\\](?:\.rightkit-managed|rightkitmanagedagent|rightkit[/\\]build-control)[/\\]agent-bin(?:[;:]|$)/i.test(String(env.PATH ?? ""));
}

export function runCargoSync(args, options = {}, { platform = process.platform, exists = existsSync, spawn = spawnSync } = {}) {
  const env = options.env ?? process.env;
  const client = managedRuntimeClient(env, platform, exists);
  if (client) {
    const socket = discoverBrokerSocket(env, { platform, exists });
    if (platform === 'darwin' && (!socket || !exists(socket))) return unavailable('RightKit broker socket is unavailable');
    const childEnv = socket ? { ...env, RIGHTKIT_BUILD_BROKER_SOCKET: socket } : env;
    const invocation = managedRuntimeCommand(client, ["cargo", ...args]);
    const result = spawn(invocation.command, invocation.args, { ...options, env: childEnv });
    if (platform === 'darwin' && result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) {
      return { ...result, error: unavailable(String(result.stderr).trim()).error };
    }
    return result;
  }
  // The Mac never falls back to direct Cargo, except on GitHub Actions runners, which have no
  // RightKit runtime and use the toolchain's cargo (release-candidate signing jobs).
  const hostedCi = env.GITHUB_ACTIONS === 'true' && !managedHost(env);
  if ((platform === 'darwin' && !hostedCi) || (managedHost(env) && platform === 'win32')) return unavailable('installed RightKit runtime client is unavailable for managed Cargo metadata');
  return spawn("cargo", args, options);
}

function unavailable(message) {
  return { status: null, stdout: "", stderr: message, error: Object.assign(new Error(message), { code: 'RK_BROKER_UNAVAILABLE' }) };
}
