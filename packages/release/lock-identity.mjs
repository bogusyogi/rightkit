// Process identity for the build-ownership submit lock (ownership-mark.mjs): pid + start time + boot id. A LEAF module (node builtins and
// background-process.mjs only) because ownership-mark.mjs is imported by snapshot-broker.mjs and build-ownership.mjs, which cannot import
// each other. build-ownership.mjs keeps the full process probe for the ownership record; this is the minimum the lock needs, with the same
// meaning: a holder is dead only when its pid is gone, or the pid now runs a process that started at another time, or the host rebooted.
import fs from "node:fs";
import { spawnSync } from "./background-process.mjs";

/** Two start times of one pid are one process when they agree to within this (ps prints whole seconds). Same figure as build-ownership.mjs. */
export const LOCK_SAME_START_MS = 3000;

/** An absolute path for a system tool (a child of the gate may hold no PATH): the first candidate that exists, else the bare name. */
const systemTool = (name, candidates) => candidates.find((c) => { try { return fs.statSync(c).isFile(); } catch { return false; } }) ?? name;
const PS = () => systemTool("ps", ["/bin/ps", "/usr/bin/ps"]);
const SYSCTL = () => systemTool("sysctl", ["/usr/sbin/sysctl", "/sbin/sysctl"]);
const run = (spawn, command, args, env = {}) => spawn(command, args, { encoding: "utf8", env: { ...process.env, ...env }, timeout: 30_000, maxBuffer: 1 << 20 });

/** This boot's identity (Linux boot_id, macOS kern.bootsessionuuid); null where it cannot be read (Windows, a failure). */
export function lockBootId({ platform = process.platform, readFile = fs.readFileSync, spawn = spawnSync } = {}) {
  try {
    if (platform === "linux") { const id = String(readFile("/proc/sys/kernel/random/boot_id", "utf8")).trim(); return /^[0-9a-f-]{16,}$/i.test(id) ? id : null; }
    if (platform === "darwin") {
      const r = run(spawn, SYSCTL(), ["-n", "kern.bootsessionuuid"]);
      const id = String(r.stdout ?? "").trim();
      return !r.error && r.status === 0 && /^[0-9a-f-]{16,}$/i.test(id) ? id : null;
    }
  } catch { /* unknown */ }
  return null;
}

/**
 * `{ alive, startedMs }` for `pid`; throws when it cannot be established (the caller then treats the holder as live: fail closed).
 * Unix: kill(pid, 0) for existence, `ps -o lstart=` for the start. Windows: this process from its time origin, another from Get-Process.
 */
export function lockProcessIdentity(pid, { platform = process.platform, spawn = spawnSync, kill = process.kill.bind(process) } = {}) {
  if (pid === process.pid && platform === "win32") return { alive: true, startedMs: Math.round(performance.timeOrigin) };
  let exists = true;
  try { kill(pid, 0); } catch (e) { if (e.code === "ESRCH") exists = false; else if (e.code !== "EPERM") throw e; }
  if (!exists) return { alive: false, startedMs: null };
  if (platform === "win32") {
    const script = `$p = Get-Process -Id ${Number(pid)} -ErrorAction SilentlyContinue; if ($p) { 'START:' + ([DateTimeOffset]$p.StartTime).ToUnixTimeMilliseconds() } else { 'GONE' }`;
    const r = run(spawn, "powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")]);
    if (r.error || r.status !== 0) throw new Error(`powershell: ${r.error?.message ?? `exit ${r.status}`}`);
    const out = String(r.stdout ?? "").trim();
    if (out === "GONE") return { alive: false, startedMs: null };
    const m = /START:(\d+)/.exec(out);
    if (!m) throw new Error(`cannot read the start time of pid ${pid}`);
    return { alive: true, startedMs: Number(m[1]) };
  }
  const r = run(spawn, PS(), ["-o", "lstart=", "-p", String(pid)], { LC_ALL: "C", LANG: "C" });
  if (r.error) throw new Error(`ps: ${r.error.message}`);
  const text = String(r.stdout ?? "").trim();
  if (text === "") { if (r.status === 1) return { alive: false, startedMs: null }; throw new Error(`ps gave no start time for pid ${pid}`); }
  const startedMs = Date.parse(text.replace(/\s+/g, " "));
  if (!Number.isFinite(startedMs)) throw new Error(`cannot read the start time of pid ${pid} (${text})`);
  return { alive: true, startedMs };
}

/** This process's identity as a lock holder records it. Cached: a process's start never changes. */
let cachedSelf = null;
export function selfLockIdentity(options = {}) {
  if (options.fresh !== true && cachedSelf) return cachedSelf;
  const id = lockProcessIdentity(process.pid, options);
  const self = { pid: process.pid, startedMs: id.startedMs, bootId: lockBootId(options) };
  if (options.fresh !== true) cachedSelf = self;
  return self;
}
