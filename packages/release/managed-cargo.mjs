import { discoverBrokerSocket, managedControlCandidates, managedRuntimeClient, resolveManagedRoots } from './managed-roots.mjs';
import { createHash, randomUUID } from "node:crypto";
import { homedir, userInfo } from "node:os";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawn as spawnProcess, spawnSync } from "./background-process.mjs";
import { CARRIER_MISSING_CODE, carriedCargoMapOf, runCarriedCargo } from "./carried-cargo.mjs";

/**
 * Linux, and any host that is neither Mac nor Windows, never uses the RustRight broker: WSL2, a VM, a cloud or CI runner, Linux hosted on
 * the Dell (GATE-CONTRACT §1 "One entry"; joint plan step 4). Every managed-cargo helper follows this one policy, whatever control-root,
 * socket or PATH entries the environment carries.
 */
export const directPlatform = (platform = process.platform) => platform !== "darwin" && platform !== "win32";

/**
 * The optional Linux broker route (Adrian, 2026-10-31: "linux normally not needing broker" is not "broker not allowed on linux"). Linux stays direct
 * unless the host DECLARES a broker, explicitly and only through these variables; a RustRight client on PATH, a socket name in the older broker
 * variables or a shim directory never activates it. A declared route is validated here, once, for every caller (the executor, managed-cargo's
 * carried seam and its client route, the gate entry): an invalid or unreachable declared broker REFUSES, it never falls back to direct cargo.
 *   RIGHTKIT_LINUX_BROKER_CLIENT        absolute path of the installed RustRight client (`rustright`)
 *   RIGHTKIT_LINUX_BROKER_SOCKET        absolute path of the broker's endpoint (the unix socket the client talks to)
 *   RIGHTKIT_LINUX_BROKER_CLIENT_SHA256 REQUIRED: the client's pinned SHA-256 (runtime identity); absent or different refuses
 */
export const LINUX_BROKER_CLIENT_ENV = "RIGHTKIT_LINUX_BROKER_CLIENT";
export const LINUX_BROKER_SOCKET_ENV = "RIGHTKIT_LINUX_BROKER_SOCKET";
export const LINUX_BROKER_CLIENT_SHA256_ENV = "RIGHTKIT_LINUX_BROKER_CLIENT_SHA256";
export const LINUX_BROKER_ROUTE = "linux-broker";
export const LINUX_BROKER_REFUSAL = "RK_LINUX_BROKER_UNAVAILABLE";

/** True when this Linux (direct-platform) host declared a broker route, valid or not. Presence of any declaring variable counts, so a half-written declaration refuses. */
export const linuxBrokerDeclared = (env = process.env, platform = process.platform) =>
  directPlatform(platform) && [LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_SOCKET_ENV, LINUX_BROKER_CLIENT_SHA256_ENV].some((name) => env?.[name] !== undefined);

/** A host that compiles direct: a direct platform that declared no broker route. */
export const directHost = (platform = process.platform, env = process.env) => directPlatform(platform) && !linuxBrokerDeclared(env, platform);

const sha256Cache = new Map();
const LINUX_ROUTE_FS = Object.freeze({
  realpath: (p) => realpathSync.native(p),
  stat: (p) => statSync(p),
  uid: () => (typeof process.getuid === "function" ? process.getuid() : -1),
  digest: (real, meta) => {
    const key = `${real}\0${meta.dev}\0${meta.ino}\0${meta.size}\0${meta.mtimeMs}`;
    if (!sha256Cache.has(key)) sha256Cache.set(key, createHash("sha256").update(readFileSync(real)).digest("hex"));
    return sha256Cache.get(key);
  },
});

/** Why an installed path is not trusted, or null: every directory on it, and the entry itself, belongs to root or to the gate's user and is not group- or world-writable (a root-owned sticky directory such as /tmp excepted). */
function untrustedChain(real, ops, uid, { entryMode = true } = {}) {
  const parts = real.split("/").filter(Boolean);
  let at = "";
  for (let i = -1; i < parts.length; i += 1) {
    if (i >= 0) at = `${at}/${parts[i]}`;
    const here = at || "/";
    const last = i === parts.length - 1;
    let meta;
    try { meta = ops.stat(here); } catch (e) { return `${here} cannot be inspected (${e.code ?? e.message})`; }
    if (meta.uid !== 0 && meta.uid !== uid) return `${here} is owned by uid ${meta.uid}, neither root nor the gate's user`;
    // A shared sticky directory owned by root (/tmp) lets nobody replace another user's entry, so only it may stay writable.
    const stickyShared = !last && meta.uid === 0 && (meta.mode & 0o1000) !== 0;
    if ((!last || entryMode) && (meta.mode & 0o022) !== 0 && !stickyShared) return `${here} is group- or world-writable`;
  }
  return null;
}

/**
 * Resolve the declared Linux broker route: `{ declared: false }` (the direct default), `{ declared: true, ok: false, code, problem }` (refuse) or
 * `{ declared: true, ok: true, client, socket, clientSha256 }`. Judged: the client is an absolute, regular, executable file reached through a
 * root- or gate-user-owned chain that nobody else can write, and matches the required pinned digest; the endpoint is an absolute unix
 * socket on such a chain (the broker authenticates its peers itself); and no other broker endpoint is named in the environment. Whether the broker
 * behind the socket answers, and what it advertises, is asked of the real client by the gate's capability handshake, which refuses the route.
 * `fsOps` is for tests of the judging only (stat/realpath/uid/digest over a described tree); it is never an alternative broker.
 */
export function resolveLinuxBrokerRoute({ env = process.env, platform = process.platform, fsOps } = {}) {
  if (!linuxBrokerDeclared(env, platform)) return { declared: false };
  const ops = { ...LINUX_ROUTE_FS, ...fsOps };
  const refuse = (problem) => ({ declared: true, ok: false, code: LINUX_BROKER_REFUSAL, problem: `${LINUX_BROKER_REFUSAL}: the declared Linux broker route cannot be used: ${problem}; nothing compiled and cargo never runs direct around a declared broker` });
  const absolute = (name) => {
    const value = env[name];
    if (typeof value !== "string" || !value) return { problem: `${name} is ${value === undefined ? "not set" : "empty"} (a declared route needs both ${LINUX_BROKER_CLIENT_ENV} and ${LINUX_BROKER_SOCKET_ENV})` };
    if (!path.posix.isAbsolute(value) || value.split("/").includes("..") || value.includes("\0")) return { problem: `${name} must be an absolute path with no '..' (got ${JSON.stringify(value.slice(0, 120))})` };
    return { value };
  };
  const uid = ops.uid();
  const c = absolute(LINUX_BROKER_CLIENT_ENV); if (c.problem) return refuse(c.problem);
  const k = absolute(LINUX_BROKER_SOCKET_ENV); if (k.problem) return refuse(k.problem);
  let client; let clientMeta;
  try { client = ops.realpath(c.value); clientMeta = ops.stat(client); } catch (e) { return refuse(`the client ${c.value} cannot be resolved (${e.code ?? e.message})`); }
  if (!clientMeta.isFile?.()) return refuse(`the client ${client} is not a regular file`);
  if ((clientMeta.mode & 0o111) === 0) return refuse(`the client ${client} is not executable`);
  const clientProblem = untrustedChain(client, ops, uid);
  if (clientProblem) return refuse(`the client is not trusted: ${clientProblem}`);
  let socket; let socketMeta;
  try { socket = ops.realpath(k.value); socketMeta = ops.stat(socket); } catch (e) { return refuse(`the broker endpoint ${k.value} does not exist or cannot be inspected (${e.code ?? e.message}); the broker is not running here`); }
  if (!socketMeta.isSocket?.()) return refuse(`the broker endpoint ${socket} is not a unix socket`);
  const socketProblem = untrustedChain(socket, ops, uid, { entryMode: false });
  if (socketProblem) return refuse(`the broker endpoint is not trusted: ${socketProblem}`);
  for (const other of ["RUSTRIGHT_BUILD_BROKER_SOCKET", "RIGHTKIT_BUILD_BROKER_SOCKET"]) {
    if (typeof env[other] === "string" && env[other] !== "" && env[other] !== k.value && env[other] !== socket) return refuse(`${other} names another endpoint (${env[other]}); one route is declared, so the choice is ambiguous`);
  }
  let clientSha256;
  try { clientSha256 = ops.digest(client, clientMeta); } catch (e) { return refuse(`the client ${client} cannot be read for its identity (${e.code ?? e.message})`); }
  // The pin is REQUIRED (root decision, 2026-10-31): a declaration without the client's digest, or with another one, is refused.
  const pinned = env[LINUX_BROKER_CLIENT_SHA256_ENV];
  if (pinned === undefined || pinned === "") return refuse(`${LINUX_BROKER_CLIENT_SHA256_ENV} is not set (the declared client's SHA-256 is required; the client ${client} has ${clientSha256})`);
  if (!/^[0-9a-f]{64}$/i.test(String(pinned))) return refuse(`${LINUX_BROKER_CLIENT_SHA256_ENV} is not a SHA-256 digest`);
  if (String(pinned).toLowerCase() !== clientSha256) return refuse(`the client ${client} has digest ${clientSha256}, not the pinned ${String(pinned).toLowerCase()}`);
  return { declared: true, ok: true, client, socket, clientSha256 };
}

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

/** `env` for a direct host: PATH without RustRight shim directories. Unchanged on Mac, Windows and a declared Linux broker route. */
export function directCargoEnv(env = process.env, platform = process.platform, exists = existsSync) {
  if (!directHost(platform, env)) return env; // Mac, Windows, and a Linux host that declared a broker route (no direct cargo there)
  const direct = directCargoPath({ env, exists });
  return direct.dropped.length ? { ...env, PATH: direct.path } : env;
}

// The installed-runtime client lookup lives in managed-roots.mjs (a leaf module, so broker-client.mjs can use it without an import cycle).
export { managedRuntimeClient };

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
  // Linux is broker-managed only through the explicit declared route (linuxBrokerDeclared), whatever else the env names.
  if (directPlatform(platform)) return linuxBrokerDeclared(env, platform);
  return Boolean(env.RUSTRIGHT_BUILD_BROKER_SOCKET || env.RIGHTKIT_BUILD_BROKER_SOCKET)
    || /(?:^|[;:])[^;:]*[/\\](?:\.rightkit-managed|rightkitmanagedagent|rightkit[/\\]build-control)[/\\]agent-bin(?:[;:]|$)/i.test(String(env.PATH ?? ""));
}

/**
 * The carried seam (rightkit#1301): on a broker host, a call made by a child of a snapshot door stage, which the gate started with the
 * stage's carrier map in its environment, goes through that carrier (carried-cargo.mjs: one signed, ledgered request per call, from the
 * manifest's directory, output directory from the receipt). A map that cannot be read refuses the call; it is never run uncarried.
 * A direct Linux host never has a carrier; a Linux host that declared a broker route has the same one. A call made with no carrier in its environment keeps the managed client route as before; refusing a
 * carrier-less compile that comes from a snapshot stage is RustRight's side (rightkit#1301 step 3).
 */
function carriedSeam(args, options, { platform, spawn }) {
  if (directHost(platform, options.env ?? process.env)) return null;
  const carried = carriedCargoMapOf(options.env ?? process.env);
  if (!carried) return null;
  if (carried.error) return { status: 101, signal: null, stdout: "", stderr: `rightkit-gate: ${CARRIER_MISSING_CODE}: ${carried.error}\n`, error: Object.assign(new Error(`${CARRIER_MISSING_CODE}: ${carried.error}`), { code: CARRIER_MISSING_CODE }) };
  return runCarriedCargo(args, options, { map: carried.map, spawn });
}

export function runCargoSync(args, options = {}, { platform = process.platform, exists = existsSync, spawn = spawnSync, fsOps } = {}) {
  const env = options.env ?? process.env;
  const seam = carriedSeam(args, options, { platform, spawn });
  if (seam) return seam;
  // A Linux host that declared a broker route: every call goes to the validated client at the declared endpoint, or is refused. Never direct.
  const linux = resolveLinuxBrokerRoute({ env, platform, fsOps });
  if (linux.declared) {
    if (!linux.ok) return unavailable(linux.problem);
    const result = spawn(linux.client, ["cargo", ...args], { ...options, env: { ...env, RUSTRIGHT_BUILD_BROKER_SOCKET: linux.socket } });
    if (result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) return { ...result, error: unavailable(String(result.stderr).trim()).error };
    return result;
  }
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

const carriedSeamActive = (args, options, { platform = process.platform } = {}) => !directHost(platform, options.env ?? process.env) && Boolean(carriedCargoMapOf(options.env ?? process.env));

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
  if ((platform === 'darwin' || linuxBrokerDeclared(options.env ?? process.env, platform)) && result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) {
    return { ...result, error: unavailable(String(result.stderr).trim()).error };
  }
  return result;
}
