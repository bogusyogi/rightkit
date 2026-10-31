// The one rule for starting the RustRight client (`rustright`), used by every gate and child spawn of it.
//
// On Windows the client is found on PATH as the managed agent shim, `rustright.cmd`. Node refuses to start a .cmd or .bat without
// `shell: true` (the CVE-2024-27980 hardening: EINVAL), and a shell would re-quote the JSON arguments, which breaks the signed
// argvSha256 or lets an argument inject. So on win32 the shim is never spawned: its target executable is read from the shim itself,
// which must be EXACTLY the installed shape below, and that .exe is spawned directly with the argv array, `shell: false`, and the
// environment the shim would have made (RUSTRIGHT_MANAGED_AGENT_SHIM and RIGHTKIT_MANAGED_AGENT_SHIM removed). A shim of any other
// shape is refused with RK_BROKER_CLIENT_UNRESOLVED; there is no shell fallback. POSIX is unchanged: the client is spawned as given.
import fs from "node:fs";
import path from "node:path";
import { spawn as spawnProcess, spawnSync as spawnProcessSync } from "./background-process.mjs";

export const BROKER_CLIENT_UNRESOLVED = "RK_BROKER_CLIENT_UNRESOLVED";
export const AGENT_SHIM_MARKER = "rustright.build-control.agent-shims.install.v1";
/** The marker shims installed before the rename carry; the installer still recognises it as its own (rustright-cli install.rs LEGACY_INSTALL_MARKER), so a Dell agent-bin shim of the old name is the same shim. */
export const LEGACY_AGENT_SHIM_MARKER = "rightkit.build-control.agent-shims.install.v1";
const SHIM_MARKERS = [AGENT_SHIM_MARKER, LEGACY_AGENT_SHIM_MARKER];
/** The variables the shim clears before it starts the runtime. */
export const AGENT_SHIM_ENV = Object.freeze(["RUSTRIGHT_MANAGED_AGENT_SHIM", "RIGHTKIT_MANAGED_AGENT_SHIM"]);

export class BrokerClientError extends Error {
  constructor(message) { super(message); this.name = "BrokerClientError"; this.code = BROKER_CLIENT_UNRESOLVED; }
}

const SHIM_EXE = /^[A-Za-z]:\\[^"%\r\n<>|&^!*?]+\.exe$/i;
const SHIM_LINES = [
  /^@echo off$/i,
  new RegExp(`^rem (?:${SHIM_MARKERS.map((m) => m.replaceAll(".", "\\.")).join("|")})$`),
  /^setlocal$/i,
  /^set "RUSTRIGHT_MANAGED_AGENT_SHIM="$/i,
  /^set "RIGHTKIT_MANAGED_AGENT_SHIM="$/i,
  /^"([^"\r\n]+)" %\*$/,
  /^exit \/b %ERRORLEVEL%$/i,
];

/** The shim's target executable from the shim's text, or a BrokerClientError. The shim must match the installed format exactly. */
export function parseAgentShim(text, shimPath = "the shim") {
  const lines = String(text).split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  if (!lines.some((l) => SHIM_MARKERS.some((m) => l === `rem ${m}`))) throw new BrokerClientError(`${shimPath} is not a RustRight agent shim (no ${AGENT_SHIM_MARKER} marker); it is not started through a shell`);
  if (lines.length !== SHIM_LINES.length || !SHIM_LINES.every((re, i) => re.test(lines[i]))) throw new BrokerClientError(`${shimPath} has a shape other than the installed RustRight agent shim; it is not started through a shell`);
  const exe = SHIM_LINES[5].exec(lines[5])[1];
  if (!SHIM_EXE.test(exe)) throw new BrokerClientError(`${shimPath} names ${JSON.stringify(exe)}, which is not an absolute .exe path without shell metacharacters`);
  return exe;
}

/**
 * The client as it is spawned: `{ file, shim }`. POSIX, and any win32 client that is not a .cmd/.bat, is returned as given (`shim: false`).
 * A win32 .cmd/.bat is resolved to the executable its shim names (`shim: true`), or refused.
 */
export function resolveBrokerClient(client, { platform = process.platform, readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  if (platform !== "win32" || !/\.(cmd|bat)$/i.test(String(client))) return { file: client, shim: false };
  let text;
  try { text = readFile(client); } catch (e) { throw new BrokerClientError(`${client} cannot be read to find the RustRight client (${e.message})`); }
  return { file: parseAgentShim(text, client), shim: true };
}

/** `env` without the shim's own variables (case-insensitive: Windows environments are). */
export function withoutAgentShimEnv(env) {
  const out = { ...env };
  for (const key of Object.keys(out)) if (AGENT_SHIM_ENV.includes(key.toUpperCase())) delete out[key];
  return out;
}

/**
 * Spawn the client: `spawn(file, args, options)` with the resolved file. A shim target is started with `shell: false` and the shim's
 * environment; anything else keeps the caller's options untouched. A refused client returns a spawn-shaped `{ error }` (code
 * RK_BROKER_CLIENT_UNRESOLVED) and nothing is started.
 */
export function spawnBrokerClient(spawn, client, args, options = {}, { platform = process.platform, readFile } = {}) {
  let resolved;
  try { resolved = resolveBrokerClient(client, { platform, readFile }); } catch (error) { return { error, status: null, signal: null, stdout: "", stderr: "" }; }
  if (!resolved.shim) return spawn(resolved.file, args, options);
  return spawn(resolved.file, args, { ...options, shell: false, env: withoutAgentShimEnv(options.env ?? process.env) });
}

/**
 * The client as PATH names it. POSIX: the bare name `rustright` (the shim is an executable file). win32: the first absolute path `where`
 * lists for it (the managed `rustright.cmd` shim or an .exe), so resolveBrokerClient can read the shim; none found leaves the bare name,
 * which the caller's spawn then fails on, as it did. `probe` is a synchronous spawn `(file, args, options)`; it is called on win32 only.
 */
export function locateBrokerClient({ platform = process.platform, env = process.env, probe } = {}) {
  if (platform !== "win32") return "rustright";
  if (typeof probe !== "function") throw new BrokerClientError("locating the RustRight client on win32 needs a synchronous spawn (`probe`)");
  const r = probe("where", ["rustright"], { encoding: "utf8", env, windowsHide: true, shell: false, timeout: 20_000 });
  const found = String(r?.stdout ?? "").split(/\r?\n/).map((l) => l.trim()).filter((l) => path.win32.isAbsolute(l));
  return found.find((l) => /\.(cmd|bat)$/i.test(l)) ?? found.find((l) => /\.exe$/i.test(l)) ?? "rustright";
}

/**
 * `{ command, args, options }` to hand to ANY spawn (synchronous or not) for `command`: anything but the bare `rustright` is returned
 * as given; `rustright` is located (locateBrokerClient) and resolved (resolveBrokerClient), so a win32 `.cmd` shim becomes its .exe
 * target with `shell: false` and the shim's environment. Throws BrokerClientError for a shim that is not the installed shape.
 */
export function brokerClientInvocation(command, args, options = {}, { platform = process.platform, probe, readFile } = {}) {
  if (command !== "rustright") return { command, args, options };
  const client = locateBrokerClient({ platform, env: options.env, probe });
  const resolved = resolveBrokerClient(client, { platform, readFile });
  if (!resolved.shim) return { command: resolved.file, args, options };
  return { command: resolved.file, args, options: { ...options, shell: false, env: withoutAgentShimEnv(options.env ?? process.env) } };
}

/**
 * How long the gate waits for a SHORT client query (`status`, `snapshot-lease status`, `doctor`, `cancel`). The Windows client retries
 * ERROR_PIPE_BUSY inside itself for up to 120 s (rustright-cli client.rs PIPE_BUSY_WAIT, 8 pipe instances): a gate that killed it sooner would
 * report "not ready" or UNKNOWN (and hold the ownership) for a client that was about to connect. 135 s is that wait plus margin.
 */
export const BROKER_CLIENT_QUERY_TIMEOUT_MS = 135_000;
/** How long ONE carried call (a compile or adapter step) may run before the gate kills its client and cancels the request (the broker's class deadline is off by default). */
export const CARRIED_CALL_TIMEOUT_MS = 65 * 60_000;
/** The client's reconnect budget variable (rustright-cli client.rs ENV_RECONNECT_WAIT_MS; default 15 minutes per outage). */
export const RECONNECT_WAIT_ENV = "RUSTRIGHT_RECONNECT_WAIT_MS";
/** The reconnect budget a carried call gets once an earlier call of the same ownership found the broker unreachable. */
export const OUTAGE_RECONNECT_WAIT_MS = 10_000;
export const BROKER_UNAVAILABLE = "RK_BROKER_UNAVAILABLE";

/**
 * The environment of a client whose broker was found unreachable by an earlier call: its reconnect budget is cut to at most
 * OUTAGE_RECONNECT_WAIT_MS (a caller's smaller figure stands), so N serial calls against a dead broker cost N short waits, never N x 15 minutes.
 */
export function withOutageReconnectBound(env) {
  const out = { ...env };
  let current = null;
  for (const key of Object.keys(out)) {
    if (key.toUpperCase() !== RECONNECT_WAIT_ENV) continue;
    const n = Number(String(out[key]).trim());
    if (Number.isSafeInteger(n) && n >= 0) current = current === null ? n : Math.min(current, n);
    delete out[key];
  }
  out[RECONNECT_WAIT_ENV] = String(current === null ? OUTAGE_RECONNECT_WAIT_MS : Math.min(current, OUTAGE_RECONNECT_WAIT_MS));
  return out;
}

/**
 * One health question before a stage's carried calls (`rustright doctor --json --no-scan`: exit 0 only when the broker answered READY, the
 * client's own contract), so a broker that is down fails the stage once, here, instead of once per carried call. Returns `{ ok: true }` or
 * `{ ok: false, code: RK_BROKER_UNAVAILABLE, message }`. Bounded by BROKER_CLIENT_QUERY_TIMEOUT_MS (a busy Windows pipe is waited for).
 */
export function brokerStagePreflight({ client, env = process.env, spawn, platform = process.platform, readFile, timeoutMs = BROKER_CLIENT_QUERY_TIMEOUT_MS }) {
  const run = spawn ?? spawnProcessSync;
  const r = spawnBrokerClient(run, client, ["doctor", "--json", "--no-scan"], { env: withOutageReconnectBound(env), encoding: "utf8", timeout: timeoutMs, windowsHide: true }, { platform, readFile });
  if (r?.error) return { ok: false, code: r.error.code === BROKER_CLIENT_UNRESOLVED ? BROKER_CLIENT_UNRESOLVED : BROKER_UNAVAILABLE, message: `\`rustright doctor\` did not run to an answer (${r.error.message}); the broker is not known to be up, so the stage does not start` };
  if (r?.status !== 0) return { ok: false, code: BROKER_UNAVAILABLE, message: `\`rustright doctor\` reports the broker is not READY (exit ${r?.status ?? r?.signal ?? "unknown"}); the stage does not start` };
  return { ok: true };
}

/**
 * The client as an ASYNC child, so the gate keeps running its own signal handlers while a carried call is in flight (a blocking spawnSync cannot
 * react to SIGTERM, so a killed gate never cancelled its broker job). Resolves, never rejects, with spawnSync's shape plus `timedOut` / `aborted`:
 * `{ status, signal, error, pid, stdout, stderr, timedOut, aborted }`. `timeout` and `signal` (an AbortSignal) kill the client's tree: POSIX sends
 * `killSignal` (the client turns SIGINT/SIGTERM into one CANCEL, rustright-cli signals.rs) and SIGKILL after `killGraceMs`; Windows ends the tree with
 * taskkill /T /F (no signal reaches a Windows client, so the caller sends the cancel itself).
 */
export function spawnClientProcess(file, args, options = {}) {
  const { timeout = 0, signal, killSignal = "SIGTERM", killGraceMs = 5000, maxBuffer = 1 << 28, encoding, stdio, ...rest } = options;
  const piped = stdio !== "inherit";
  return new Promise((resolve) => {
    let child;
    const result = { status: null, signal: null, error: undefined, pid: 0, stdout: "", stderr: "", timedOut: false, aborted: false };
    let settled = false; let timer = null; let grace = null; let size = 0;
    const finish = () => { if (settled) return; settled = true; clearTimeout(timer); clearTimeout(grace); signal?.removeEventListener?.("abort", onAbort); resolve(result); };
    const kill = () => {
      if (!child || child.exitCode !== null || child.signalCode !== null) return;
      if (process.platform === "win32") { try { spawnProcessSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore", timeout: 30_000 }); } catch { /* the client is gone or cannot be reached */ } return; }
      try { child.kill(killSignal); } catch { /* already gone */ }
      grace = setTimeout(() => { try { child.kill("SIGKILL"); } catch { /* already gone */ } }, killGraceMs);
      grace.unref?.();
    };
    function onAbort() { result.aborted = true; kill(); }
    try { child = spawnProcess(file, args, { ...rest, stdio: piped ? ["ignore", "pipe", "pipe"] : "inherit" }); }
    catch (error) { result.error = error; finish(); return; }
    result.pid = child.pid ?? 0;
    if (piped) {
      for (const [name, stream] of [["stdout", child.stdout], ["stderr", child.stderr]]) {
        stream.on("data", (chunk) => {
          size += chunk.length;
          if (size > maxBuffer) { if (!result.error) result.error = Object.assign(new Error(`${name} exceeded maxBuffer`), { code: "ENOBUFS" }); kill(); return; }
          result[name] += chunk.toString("utf8");
        });
      }
    }
    child.on("error", (error) => { result.error = result.error ?? error; if (!child.pid) finish(); });
    child.on("close", (code, sig) => {
      result.status = code; result.signal = sig;
      if (result.timedOut && !result.error) result.error = Object.assign(new Error(`the client did not finish within ${timeout} ms and was killed`), { code: "ETIMEDOUT" });
      finish();
    });
    if (timeout > 0) { timer = setTimeout(() => { result.timedOut = true; kill(); }, timeout); timer.unref?.(); }
    if (signal) { if (signal.aborted) onAbort(); else signal.addEventListener("abort", onAbort, { once: true }); }
  });
}

/** `spawnBrokerClient`, asynchronously: the client is resolved and started exactly as the synchronous one starts it, through `spawnAsync` (default spawnClientProcess). */
export async function spawnBrokerClientAsync(spawnAsync, client, args, options = {}, { platform = process.platform, readFile } = {}) {
  let resolved;
  try { resolved = resolveBrokerClient(client, { platform, readFile }); } catch (error) { return { error, status: null, signal: null, stdout: "", stderr: "" }; }
  const run = spawnAsync ?? spawnClientProcess;
  if (!resolved.shim) return run(resolved.file, args, options);
  return run(resolved.file, args, { ...options, shell: false, env: withoutAgentShimEnv(options.env ?? process.env) });
}
