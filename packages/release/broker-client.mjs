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
import { managedRuntimeClient } from "./managed-roots.mjs";
import { currentInterfaceEnv, legacyInterfaceNamesIn } from "./rustright-names.mjs";

export const BROKER_CLIENT_UNRESOLVED = "RK_BROKER_CLIENT_UNRESOLVED";
export const AGENT_SHIM_MARKER = "rustright.build-control.agent-shims.install.v1";
/** The variables the shim clears before it starts the runtime. */
export const AGENT_SHIM_ENV = Object.freeze(["RUSTRIGHT_MANAGED_AGENT_SHIM", "RIGHTKIT_MANAGED_AGENT_SHIM"]);

export class BrokerClientError extends Error {
  constructor(message) { super(message); this.name = "BrokerClientError"; this.code = BROKER_CLIENT_UNRESOLVED; }
}

const SHIM_EXE = /^[A-Za-z]:\\[^"%\r\n<>|&^!*?]+\.exe$/i;
const SHIM_LINES = [
  /^@echo off$/i,
  new RegExp(`^rem ${AGENT_SHIM_MARKER.replaceAll(".", "\\.")}$`),
  /^setlocal$/i,
  /^set "RUSTRIGHT_MANAGED_AGENT_SHIM="$/i,
  /^set "RIGHTKIT_MANAGED_AGENT_SHIM="$/i,
  /^"([^"\r\n]+)" %\*$/,
  /^exit \/b %ERRORLEVEL%$/i,
];

/** The shim's target executable from the shim's text, or a BrokerClientError. The shim must match the installed format exactly. */
export function parseAgentShim(text, shimPath = "the shim") {
  const lines = String(text).split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  if (!lines.some((l) => l === `rem ${AGENT_SHIM_MARKER}`)) throw new BrokerClientError(`${shimPath} is not a RustRight agent shim (no ${AGENT_SHIM_MARKER} marker); it is not started through a shell`);
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
  // GB-09: every outbound client call carries the current RUSTRIGHT_* spelling of the interface names only (a legacy RIGHTKIT_* one is translated, never sent).
  const baseEnv = options.env ?? process.env;
  const env = legacyInterfaceNamesIn(baseEnv).length ? currentInterfaceEnv(baseEnv) : options.env;
  if (!resolved.shim) return spawn(resolved.file, args, env === options.env ? options : { ...options, env });
  return spawn(resolved.file, args, { ...options, shell: false, env: withoutAgentShimEnv(env ?? process.env) });
}

/**
 * The client as PATH names it. POSIX: the bare name `rustright` (the shim is an executable file). win32: the first absolute path `where`
 * lists for it (the managed `rustright.cmd` shim or an .exe), so resolveBrokerClient can read the shim; none found leaves the bare name,
 * which the caller's spawn then fails on, as it did. `probe` is a synchronous spawn `(file, args, options)`; it is called on win32 only.
 */
export function locateBrokerClient({ platform = process.platform, env = process.env, probe, installed = managedRuntimeClient } = {}) {
  if (platform !== "win32") return "rustright";
  // GB-08 (BR11/B31): the installed runtime's own .exe is the client wherever it exists, so a PATH that exposes the `rustright.cmd` shim first
  // (or a stale one) never decides what starts; PATH is consulted only when no installed runtime is found.
  let runtime;
  try { runtime = typeof installed === "function" ? installed(env, platform) : undefined; } catch { runtime = undefined; }
  if (typeof runtime === "string" && /\.exe$/i.test(runtime) && path.win32.isAbsolute(runtime)) return runtime;
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
export function brokerClientInvocation(command, args, options = {}, { platform = process.platform, probe, readFile, installed } = {}) {
  if (command !== "rustright") return { command, args, options };
  const client = locateBrokerClient({ platform, env: options.env, probe, ...(installed ? { installed } : {}) });
  const resolved = resolveBrokerClient(client, { platform, readFile });
  if (!resolved.shim) return { command: resolved.file, args, options };
  return { command: resolved.file, args, options: { ...options, shell: false, env: withoutAgentShimEnv(options.env ?? process.env) } };
}
