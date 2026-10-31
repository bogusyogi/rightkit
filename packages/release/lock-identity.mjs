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

// ---- the gate's identity as RustRight reads it (BROKER-CONTRACT section 16, GB-76) ----
// The carrier owner record names the gate by `{ pid, startTimeMs }`, and the broker judges it with `rustright_process::identity_state`: the recorded start time must EQUAL what RustRight reads
// for that pid (Same), or the pid is `Reused`. The lock identity above tolerates a few seconds; this does not, so it computes RustRight's own figure on each platform:
//   Linux    `/proc/<pid>/stat` field 22 (start ticks since boot) and `/proc/stat` btime: `btime * 1000 + ticks * 1000 / CLK_TCK` (integer division)
//   macOS    the process's kernel start timeval (kinfo_proc's first field, `sysctl -b kern.proc.pid.<pid>`: int64 seconds, int32 microseconds), `(sec * 1e6 + usec) / 1000` floored
//   Windows  GetProcessTimes' creation time as UNIX milliseconds (`Process.StartTime`, floored to a millisecond)
// `null` when the figure cannot be read: the owner record then carries null and the broker keeps the carrier for ever (unverifiable), never retires it.

/** Linux: the start time in epoch ms from a `/proc/<pid>/stat` text, `/proc/stat` text and CLK_TCK, or null. The command name may hold spaces and parentheses: fields count from the LAST `)`. */
export function linuxStartTimeMs(stat, procStat, hz) {
  const close = String(stat).lastIndexOf(")");
  if (close < 0) return null;
  const ticks = Number(String(stat).slice(close + 1).trim().split(/\s+/)[19]);
  const boot = Number(/^btime\s+(\d+)/m.exec(String(procStat))?.[1]);
  if (!Number.isInteger(ticks) || ticks < 0 || !Number.isInteger(boot) || !Number.isFinite(hz) || hz <= 0) return null;
  return boot * 1000 + Math.floor((ticks * 1000) / hz);
}

/** macOS: the start time in epoch ms from the bytes `sysctl -b kern.proc.pid.<pid>` prints (the start timeval leads kinfo_proc), or null. */
export function darwinStartTimeMs(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length < 12) return null;
  const sec = bytes.readBigInt64LE(0); const usec = bytes.readInt32LE(8);
  if (sec <= 0n || usec < 0 || usec >= 1_000_000) return null;
  return Number((sec * 1_000_000n + BigInt(usec)) / 1000n);
}

/** `pid`'s start time as RustRight's `start_time_ms` reads it, or null when it cannot be read (gone, denied, an unknown platform). `readFile`, `spawn` and `hz` are seams for a simulated host. */
export function processStartTimeMs(pid, { platform = process.platform, readFile = fs.readFileSync, spawn = spawnSync, hz = null } = {}) {
  try {
    if (!Number.isInteger(pid) || pid <= 0) return null;
    if (platform === "linux") {
      let clock = hz;
      if (clock === null) { const r = run(spawn, "getconf", ["CLK_TCK"]); const n = Number(String(r.stdout ?? "").trim()); clock = !r.error && r.status === 0 && Number.isInteger(n) && n > 0 ? n : 100; }
      return linuxStartTimeMs(readFile(`/proc/${pid}/stat`, "utf8"), readFile("/proc/stat", "utf8"), clock);
    }
    if (platform === "darwin") {
      const r = spawn(SYSCTL(), ["-b", `kern.proc.pid.${pid}`], { env: { ...process.env, LC_ALL: "C" }, timeout: 30_000, maxBuffer: 1 << 20 });
      return r.error || r.status !== 0 ? null : darwinStartTimeMs(Buffer.isBuffer(r.stdout) ? r.stdout : Buffer.from(String(r.stdout ?? ""), "latin1"));
    }
    if (platform === "win32") {
      const script = `$p = Get-Process -Id ${Number(pid)} -ErrorAction SilentlyContinue; if ($p) { 'START:' + ([DateTimeOffset]$p.StartTime).ToUnixTimeMilliseconds() } else { 'GONE' }`;
      const r = run(spawn, "powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")]);
      if (r.error || r.status !== 0) return null;
      const m = /START:(\d+)/.exec(String(r.stdout ?? ""));
      return m ? Number(m[1]) : null;
    }
  } catch { /* unreadable: null */ }
  return null;
}

/** This process's own start time as RustRight reads it. Cached: a process's start never changes. A null (unreadable) is retried on the next call. */
let cachedSelfStart = null;
export function selfStartTimeMs(options = {}) {
  if (options.fresh !== true && cachedSelfStart !== null) return cachedSelfStart;
  const ms = processStartTimeMs(process.pid, options);
  if (options.fresh !== true && ms !== null) cachedSelfStart = ms;
  return ms;
}
