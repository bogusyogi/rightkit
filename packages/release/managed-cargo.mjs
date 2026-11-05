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

/**
 * A gate run outside the broker by Adrian's recorded exception (broker-exception.mjs, `rightkit-gate --no-broker <id>`): the gate hands its
 * children this variable set to the exception id. Inside that live door run (`RK_GATE_RUN` set) every `rustright cargo|rustc|rustdoc` runs the real
 * toolchain directly, RustRight shims off PATH, as on a direct Linux host; the installed client is never asked. Outside a door run it means nothing.
 */
export const NO_BROKER_ENV = "RIGHTKIT_GATE_NO_BROKER";
export const gateNoBroker = (env = process.env) => Boolean(env?.[NO_BROKER_ENV] && env?.RK_GATE_RUN);

/** A host that compiles direct: a direct platform that declared no broker route, or any host inside a gate run outside the broker by exception. */
export const directHost = (platform = process.platform, env = process.env) => (directPlatform(platform) && !linuxBrokerDeclared(env, platform)) || gateNoBroker(env);

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

const MANAGED_AGENT_BIN = /(?:^|\/)(?:\.rightkit-managed|\.rustright|rightkitmanagedagent|rightkit\/build-control|rustright\/build-control)\/agent-bin\/?$/i;
function fileHead(file) {
  let fd = null;
  try { fd = openSync(file, "r"); const b = Buffer.alloc(4096); const n = readSync(fd, b, 0, 4096, 0); return b.subarray(0, n).toString("latin1"); }
  catch { return ""; } finally { if (fd !== null) try { closeSync(fd); } catch { /* closed */ } }
}

/** True when `file` is a gate's own runtime cargo wrapper (cargo-target-shim.mjs installCargoShim writes it), not a toolchain cargo. */
export function isGateCargoWrapper(file, readHead = fileHead) {
  const head = readHead(file);
  return head.startsWith("#!/bin/sh\n") && head.includes("cargo-target-shim.mjs") && head.includes(" --map ");
}

/**
 * The absolute cargo a gate wraps: the first of `candidates` (a PATH lookup, in order) that is not another gate's runtime wrapper, or the
 * toolchain cargo such a wrapper's map records (a lookup that returns only the first match finds only the wrapper).
 * A gate started inside a scan of an outer gate finds that gate's wrapper first on PATH; wrapping it would send every call through
 * the outer map, which binds none of this gate's workspaces (0.6.18 Linux qualify: RK_CARGO_UNBOUND_WORKSPACE "resolved (none)").
 */
export function realCargoOf(candidates, readHead = fileHead, readMap = (file) => JSON.parse(readFileSync(file, "utf8"))) {
  for (const c of candidates) {
    if (!path.isAbsolute(c)) continue;
    if (!isGateCargoWrapper(c, readHead)) return c;
    // A PATH lookup (`which`) names only the first match: the wrapper's own map records the toolchain cargo it wraps.
    try {
      const at = /--map '([^']+)'/.exec(readHead(c));
      const wrapped = at ? readMap(at[1]).realCargo : null;
      if (typeof wrapped === "string" && path.isAbsolute(wrapped) && !isGateCargoWrapper(wrapped, readHead)) return wrapped;
    } catch { /* an unreadable map names nothing: keep looking */ }
  }
  return undefined;
}

/**
 * On a direct (Linux) host: PATH without the directories whose `cargo` is a RustRight shim (the managed agent-bin, a configured
 * RIGHTKIT_MANAGED_AGENT_BIN, or a script that routes through the broker, read 4 KiB at most), and the first real cargo left on it.
 * Plain `cargo` then can only be a real toolchain; with none, a cargo launch fails as missing, never through a shim.
 */
/**
 * The broker's own registered toolchain on a broker host (Mac, Dell): `{ cargo, dirs }` from its config's `tools` (cargo's toolchain bin first,
 * then the registered cargo plugins cargo-deny, cargo-audit, ...), or null when no broker config is readable (Linux, CI). The gate's direct,
 * non-compiling cargo (GATE-CONTRACT GC-A11) runs exactly the broker's pinned toolchain and plugins, never whatever PATH or rustup's
 * default holds (the Dell has no cargo on PATH; rustup's default there is another toolchain). The config is the live one at the managed
 * control root, else the data root's (a host whose control-root move has not happened yet).
 */
const BROKER_PLUGIN_TOOLS = Object.freeze(["cargo-deny", "cargo-audit", "cargo-about", "cargo-outdated", "cargo-machete", "cargo-upgrade", "cargo-set-version"]);
export function brokerToolchain({ env = process.env, platform = process.platform, readFile = (f) => readFileSync(f, "utf8"), exists = existsSync } = {}) {
  let roots; try { roots = resolveManagedRoots({ env, platform }); } catch { return null; }
  const paths = platform === "win32" ? path.win32 : path.posix;
  for (const file of [roots.configPath, roots.dataRoot ? paths.join(roots.dataRoot, "config.json") : null].filter(Boolean)) {
    let config; try { if (!exists(file)) continue; config = JSON.parse(readFile(file)); } catch { continue; }
    const tools = Array.isArray(config?.tools) ? config.tools : [];
    const byId = new Map(tools.filter((t) => t && typeof t.id === "string" && typeof t.path === "string").map((t) => [t.id, t]));
    const cargo = byId.get("cargo");
    if (!cargo || !exists(cargo.path)) continue;
    const dirOf = (t) => (typeof t.root === "string" && t.root ? t.root : paths.dirname(t.path));
    const dirs = [dirOf(cargo), ...BROKER_PLUGIN_TOOLS.map((id) => byId.get(id)).filter((t) => t && exists(t.path)).map(dirOf)];
    return { cargo: cargo.path, dirs: [...new Set(dirs)], config: file };
  }
  return null;
}

export function directCargoPath({ env = process.env, exists = existsSync, readHead = fileHead, platform = process.platform, gateWrappers = false, broker = brokerToolchain({ env, platform, exists }) } = {}) {
  // `platform` only matters for a forced direct run on Windows (a recorded broker exception): ";" separators, backslash paths, cargo.exe.
  const win = platform === "win32";
  const norm = (d) => (win ? String(d).replaceAll("\\", "/") : d);
  const configured = env.RIGHTKIT_MANAGED_AGENT_BIN ? path.posix.resolve(norm(env.RIGHTKIT_MANAGED_AGENT_BIN)) : null;
  const kept = []; const dropped = []; let cargo = null;
  for (const dir of String(env.PATH ?? "").split(win ? ";" : ":").filter(Boolean)) {
    const candidates = win ? ["cargo.exe", "cargo.cmd", "cargo.bat", "cargo"].map((n) => path.win32.join(dir, n)) : [path.posix.join(dir, "cargo")];
    const found = candidates.find((c) => exists(c));
    // `gateWrappers`: a gate resolving its OWN executor also skips another gate's runtime cargo wrapper (a gate started inside an outer gate's scan,
    // 0.6.18/0.6.19 Linux qualify). A gate's children never pass it: inside a stage the wrapper first on PATH is the binding they must go through. The
    // one other caller is test-support/toolchain-cargo.mjs, for a test's throwaway fixtures under the temp directory, never anything in the snapshot.
    const isShim = Boolean(found) && (MANAGED_AGENT_BIN.test(norm(dir)) || (configured && path.posix.resolve(norm(dir)) === configured) || /cargo-guard\.mjs|rustright/i.test(readHead(found)) || (gateWrappers && isGateCargoWrapper(found, readHead)));
    if (isShim) { dropped.push(dir); continue; }
    kept.push(dir);
    if (!cargo && found) cargo = found;
  }
  // A broker host: its registered toolchain and plugins come first, and its cargo is the one used.
  if (broker) return { path: [...broker.dirs, ...kept.filter((d) => !broker.dirs.includes(d))].join(win ? ";" : ":"), dropped, cargo: broker.cargo, broker: broker.config };
  return { path: kept.join(win ? ";" : ":"), dropped, cargo };
}

/**
 * GC-A11 (Adrian, 2026-11-02: "Class a never in the broker. Any cargo things that are cheap and non compiling are in class a, can run outside the
 * broker"): the environment for a NON-COMPILING cargo call (metadata, locate-project, tree, package --no-verify) on any host: PATH with the broker's
 * own toolchain first and every RustRight shim dropped, so `cargo` is the real binary. The broker refuses such a request from inside a snapshot.
 */
export function nonCompilingCargoEnv(env = process.env, platform = process.platform) {
  const pathKey = platform === "win32" ? (Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
  const direct = directCargoPath({ env: { ...env, PATH: env[pathKey] }, platform, gateWrappers: true });
  return { ...env, [pathKey]: direct.path };
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
  const runs = path.join(resolveManagedRoots({ env, platform }).buildControl, "global", "runs");
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
    || /(?:^|[;:])[^;:]*[/\\](?:\.rightkit-managed|\.rustright|rightkitmanagedagent|rightkit[/\\]build-control|rustright[/\\]build-control)[/\\]agent-bin(?:[;:]|$)/i.test(String(env.PATH ?? ""));
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

/**
 * The ONE route a call takes to the broker's client on a broker host (runCargoSync and cargo-guard both use it): the declared Linux route when the host declared one (only when it
 * validates; an invalid one is `{ refusal }`, never a fallback to another client), else the installed runtime client with its discovered socket (on the Mac a missing socket is
 * `{ refusal: "RightKit broker socket is unavailable" }`), else null. `{ client, env, mapUnavailable }` runs as `<client> cargo <args>`; `mapUnavailable` says a RK_BROKER_UNAVAILABLE
 * answer of the client is to be reported as an unavailable broker (the declared route, and the Mac).
 */
export function brokerClientRoute({ env = process.env, platform = process.platform, exists = existsSync, fsOps, linuxRoute = resolveLinuxBrokerRoute, installedClient = managedRuntimeClient, socketOf = discoverBrokerSocket } = {}) {
  const linux = linuxRoute({ env, platform, fsOps });
  if (linux.declared) return linux.ok ? { client: linux.client, env: { ...env, RUSTRIGHT_BUILD_BROKER_SOCKET: linux.socket }, mapUnavailable: true } : { refusal: linux.problem };
  const client = installedClient(env, platform, exists);
  if (!client) return null;
  const socket = socketOf(env, { platform, exists });
  if (platform === 'darwin' && (!socket || !exists(socket))) return { refusal: 'RightKit broker socket is unavailable' };
  return { client, env: socket ? { ...env, RUSTRIGHT_BUILD_BROKER_SOCKET: socket } : env, mapUnavailable: platform === 'darwin' };
}

/** Cargo commands that compile nothing: Class A, run direct with the pinned toolchain and never carried by the broker (GC-A11, BROKER-CONTRACT interface item 14). */
const NON_COMPILING_CARGO = new Set(['metadata', 'fetch', 'tree', 'locate-project', 'pkgid', 'verify-project', 'read-manifest']);

export function runCargoSync(args, options = {}, { platform = process.platform, exists = existsSync, spawn = spawnSync, fsOps } = {}) {
  const env = options.env ?? process.env;
  // The broker refuses every carried request that compiles nothing (RK_CHECK_ERROR), so right-release's `cargo metadata` of the compile and
  // package stages runs here, directly, exactly as the gate runs its Class A cargo calls.
  if (NON_COMPILING_CARGO.has(args[0])) return spawn("cargo", args, { ...options, env: directCargoEnv(env, platform, exists) });
  const seam = carriedSeam(args, options, { platform, spawn });
  if (seam) return seam;
  // A gate run outside the broker by Adrian's recorded exception: the real toolchain, shims off PATH, on every platform (NO_BROKER_ENV).
  if (gateNoBroker(env)) return spawn("cargo", args, { ...options, env: directCargoEnv(env, platform, exists) });
  // A Linux host that declared a broker route: every call goes to the validated client at the declared endpoint, or is refused. Never direct. Otherwise the managed client (brokerClientRoute).
  const route = brokerClientRoute({ env, platform, exists, fsOps });
  if (route?.refusal) return unavailable(route.refusal);
  if (route) {
    const invocation = managedRuntimeCommand(route.client, ["cargo", ...args]);
    const result = spawn(invocation.command, invocation.args, { ...options, env: route.env });
    if (route.mapUnavailable && result.status !== 0 && /RK_BROKER_UNAVAILABLE|RightKit broker (?:unavailable|endpoint is not configured)/i.test(String(result.stderr ?? ''))) {
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
