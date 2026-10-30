import { discoverBrokerSocket, managedControlCandidates, resolveManagedRoots } from './managed-roots.mjs';
import { randomUUID } from "node:crypto";
import { homedir, userInfo } from "node:os";
import { closeSync, existsSync, mkdirSync, openSync, readSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawn as spawnProcess, spawnSync } from "./background-process.mjs";
import { CARRIER_MISSING_CODE, carriedCargoMapOf, runCarriedCargo } from "./carried-cargo.mjs";

/**
 * Linux, and any host that is neither Mac nor Windows, never uses the RustRight broker: WSL2, a VM, a cloud or CI runner, Linux hosted on
 * the Dell (GATE-CONTRACT §1 "One entry"; joint plan step 4). Every managed-cargo helper follows this one policy, whatever control-root,
 * socket or PATH entries the environment carries.
 */
export const directPlatform = (platform = process.platform) => platform !== "darwin" && platform !== "win32";

const MANAGED_AGENT_BIN = /(?:^|\/)(?:\.rightkit-managed|rightkitmanagedagent|rightkit\/build-control)\/agent-bin\/?$/i;
function fileHead(file) {
  let fd = null;
  try { fd = openSync(file, "r"); const b = Buffer.alloc(4096); const n = readSync(fd, b, 0, 4096, 0); return b.subarray(0, n).toString("latin1"); }
  catch { return ""; } finally { if (fd !== null) try { closeSync(fd); } catch { /* closed */ } }
}

/**
 * On a direct (Linux) host: PATH without the directories whose `cargo` is a RustRight shim (the managed agent-bin, a configured
 * RIGHTKIT_MANAGED_AGENT_BIN, or a script that routes through the broker, read 4 KiB at most), and the first real cargo left on it.
 * Plain `cargo` then can only be a real toolchain; with none, a cargo launch fails as missing, never through a shim.
 */
export function directCargoPath({ env = process.env, exists = existsSync, readHead = fileHead } = {}) {
  const configured = env.RIGHTKIT_MANAGED_AGENT_BIN ? path.posix.resolve(env.RIGHTKIT_MANAGED_AGENT_BIN) : null;
  const kept = []; const dropped = []; let cargo = null;
  for (const dir of String(env.PATH ?? "").split(":").filter(Boolean)) {
    const candidate = path.posix.join(dir, "cargo");
    const isShim = exists(candidate) && (MANAGED_AGENT_BIN.test(dir) || (configured && path.posix.resolve(dir) === configured) || /cargo-guard\.mjs|rustright/i.test(readHead(candidate)));
    if (isShim) { dropped.push(dir); continue; }
    kept.push(dir);
    if (!cargo && exists(candidate)) cargo = candidate;
  }
  return { path: kept.join(":"), dropped, cargo };
}

/** `env` for a direct host: PATH without RustRight shim directories. Unchanged on Mac and Windows. */
export function directCargoEnv(env = process.env, platform = process.platform, exists = existsSync) {
  if (!directPlatform(platform)) return env;
  const direct = directCargoPath({ env, exists });
  return direct.dropped.length ? { ...env, PATH: direct.path } : env;
}

export function managedRuntimeClient(env = process.env, platform = process.platform, exists = existsSync) {
  if (directPlatform(platform)) return undefined;
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
    // Broker-managed HOME may point at a build home; the installed native client belongs to the OS account.
    const accountClient = path.posix.join(userInfo().homedir, 'Library', 'Application Support', 'RustRight', 'runtime', 'rustright');
    if (exists(accountClient)) return accountClient;
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

export function managedHost(env, platform = process.platform) {
  if (directPlatform(platform)) return false; // Linux is never broker-managed (directPlatform), whatever the env names
  return Boolean(env.RUSTRIGHT_BUILD_BROKER_SOCKET || env.RIGHTKIT_BUILD_BROKER_SOCKET)
    || /(?:^|[;:])[^;:]*[/\\](?:\.rightkit-managed|rightkitmanagedagent|rightkit[/\\]build-control)[/\\]agent-bin(?:[;:]|$)/i.test(String(env.PATH ?? ""));
}

/**
 * The carried seam (rightkit#1301): on a broker host, a call made by a child of a snapshot door stage, which the gate started with the
 * stage's carrier map in its environment, goes through that carrier (carried-cargo.mjs: one signed, ledgered request per call, from the
 * manifest's directory, output directory from the receipt). A map that cannot be read refuses the call; it is never run uncarried.
 * Linux never has a carrier. A call made with no carrier in its environment keeps the managed client route as before; refusing a
 * carrier-less compile that comes from a snapshot stage is RustRight's side (rightkit#1301 step 3).
 */
function carriedSeam(args, options, { platform, spawn }) {
  if (directPlatform(platform)) return null;
  const carried = carriedCargoMapOf(options.env ?? process.env);
  if (!carried) return null;
  if (carried.error) return { status: 101, signal: null, stdout: "", stderr: `rightkit-gate: ${CARRIER_MISSING_CODE}: ${carried.error}\n`, error: Object.assign(new Error(`${CARRIER_MISSING_CODE}: ${carried.error}`), { code: CARRIER_MISSING_CODE }) };
  return runCarriedCargo(args, options, { map: carried.map, spawn });
}

export function runCargoSync(args, options = {}, { platform = process.platform, exists = existsSync, spawn = spawnSync } = {}) {
  const env = options.env ?? process.env;
  const seam = carriedSeam(args, options, { platform, spawn });
  if (seam) return seam;
  const client = managedRuntimeClient(env, platform, exists);
  if (client) {
    const socket = discoverBrokerSocket(env, { platform, exists });
    if (platform === 'darwin' && (!socket || !exists(socket))) return unavailable('RightKit broker socket is unavailable');
    const childEnv = socket ? { ...env, RUSTRIGHT_BUILD_BROKER_SOCKET: socket } : env;
    const invocation = managedRuntimeCommand(client, ["cargo", ...args]);
    const result = spawn(invocation.command, invocation.args, { ...options, env: childEnv });
    if (platform === 'darwin' && result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) {
      return { ...result, error: unavailable(String(result.stderr).trim()).error };
    }
    return result;
  }
  // The Mac never falls back to direct Cargo, except on GitHub Actions runners, which have no
  // RightKit runtime and use the toolchain's cargo (release-candidate signing jobs).
  const hostedCi = env.GITHUB_ACTIONS === 'true' && !managedHost(env, platform);
  if ((platform === 'darwin' && !hostedCi) || (managedHost(env, platform) && platform === 'win32')) return unavailable('installed RightKit runtime client is unavailable for managed Cargo metadata');
  // A direct (Linux) host runs the real toolchain: RustRight shim directories are taken off PATH first.
  return spawn("cargo", args, directPlatform(platform) ? { ...options, env: directCargoEnv(env, platform, exists) } : options);
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

const carriedSeamActive = (args, options, { platform = process.platform } = {}) => !directPlatform(platform) && Boolean(carriedCargoMapOf(options.env ?? process.env));

/**
 * Async twin of `runCargoSync`: the same client, broker-socket and darwin-never-direct selection (it asks
 * `runCargoSync` to choose, with a spawn that only records the invocation), then runs the chosen invocation
 * without blocking the event loop so independent crates can publish concurrently. The broker still
 * serializes the builds themselves.
 */
export async function runCargoAsync(args, options = {}, deps = {}) {
  const { spawnAsync = spawnCollect, ...syncDeps } = deps;
  // A carried call (a snapshot stage's) is verified in process: it runs whole, then resolves.
  if (carriedSeamActive(args, options, syncDeps)) return runCargoSync(args, options, syncDeps);
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
