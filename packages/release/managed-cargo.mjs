import { discoverBrokerSocket, managedControlCandidates, resolveManagedRoots } from './managed-roots.mjs';
import { randomUUID } from "node:crypto";
import { homedir } from "node:os";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawn as spawnProcess, spawnSync } from "./background-process.mjs";

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
  // RustRight's own install on Windows (2026-10-18 cutover): the runtime moved out of the control root to
  // <drive>\.rustright\runtime, beside the control root's drive, and runtime-current was retired.
  if (platform === 'win32') {
    for (const root of roots) {
      const client = path.win32.join(path.win32.parse(root).root, '.rustright', 'runtime', 'rustright.exe');
      if (exists(client)) return client;
    }
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

/**
 * Async `spawnSync` look-alike for the batch publisher: runs the command through the release spawn boundary
 * (background-process.mjs), collects stdout and stderr, and resolves `{ status, signal, stdout, stderr, error }`.
 * It never rejects: a spawn failure, a timeout or an exceeded `maxBuffer` resolves with `error` set, which is
 * exactly how callers already read a failed `spawnSync`.
 */
export function spawnCollect(command, args, options = {}, { spawnAsync = spawnProcess } = {}) {
  const { timeout, maxBuffer = 512 * 1024 * 1024, encoding: _encoding, stdio: _stdio, ...rest } = options;
  return new Promise((resolve) => {
    let child;
    try { child = spawnAsync(command, args, { ...rest, stdio: ["ignore", "pipe", "pipe"] }); } catch (error) { resolve({ status: null, signal: null, stdout: "", stderr: "", error }); return; }
    const out = []; const err = [];
    let size = 0; let settled = false; let timer; let failure;
    const finish = (result) => { if (settled) return; settled = true; clearTimeout(timer); resolve(result); };
    const take = (chunks) => (chunk) => {
      size += chunk.length;
      if (size > maxBuffer) { failure ??= new Error(`output exceeded maxBuffer (${maxBuffer} bytes)`); child.kill("SIGKILL"); return; }
      chunks.push(chunk);
    };
    child.stdout.on("data", take(out));
    child.stderr.on("data", take(err));
    child.on("error", (error) => finish({ status: null, signal: null, stdout: "", stderr: "", error }));
    child.on("close", (status, signal) => finish({
      status, signal, stdout: Buffer.concat(out).toString("utf8"), stderr: Buffer.concat(err).toString("utf8"), error: failure,
    }));
    if (timeout) timer = setTimeout(() => { failure ??= new Error(`timed out after ${timeout}ms`); child.kill("SIGKILL"); }, timeout);
  });
}

/**
 * Async twin of `runCargoSync`: the same client, broker-socket and darwin-never-direct selection (it asks
 * `runCargoSync` to choose, with a spawn that only records the invocation), then runs the chosen invocation
 * without blocking the event loop so independent crates can publish concurrently. The broker still
 * serializes the builds themselves.
 */
export async function runCargoAsync(args, options = {}, deps = {}) {
  const { spawnAsync = spawnCollect, ...syncDeps } = deps;
  let chosen;
  const probe = runCargoSync(args, options, { ...syncDeps, spawn: (command, commandArgs, commandOptions) => { chosen = { command, args: commandArgs, options: commandOptions }; return { status: 0, stdout: "", stderr: "" }; } });
  if (!chosen) return probe;
  const result = await spawnAsync(chosen.command, chosen.args, chosen.options);
  const platform = syncDeps.platform ?? process.platform;
  if (platform === 'darwin' && result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) {
    return { ...result, error: unavailable(String(result.stderr).trim()).error };
  }
  return result;
}
