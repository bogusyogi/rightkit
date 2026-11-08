// Process liveness and identity for a lock a coordinator keeps (scripts/ci/rk-pipeline.mjs acquireLock): pid + start time + boot time. A LEAF module (node builtins and
// background-process.mjs only). A holder is dead only when its pid is gone, or the pid now runs a process that started at another time, or the host rebooted. This is the
// minimum any such lock needs; the gate's snapshot lock names its holder by the birth identity of gate-seam.mjs `processIdentity` instead, which the broker compares.
import fs from "node:fs";
import { spawnSync } from "./background-process.mjs";

/** An absolute path for a system tool (a child of the gate may hold no PATH): the first candidate that exists, else the bare name. */
const systemTool = (name, candidates) => candidates.find((c) => { try { return fs.statSync(c).isFile(); } catch { return false; } }) ?? name;
const PS = () => systemTool("ps", ["/bin/ps", "/usr/bin/ps"]);
const SYSCTL = () => systemTool("sysctl", ["/usr/sbin/sysctl", "/sbin/sysctl"]);
const run = (spawn, command, args, env = {}) => spawn(command, args, { encoding: "utf8", env: { ...process.env, ...env }, timeout: 30_000, maxBuffer: 1 << 20 });

/** This host's boot time in epoch ms (Linux btime, macOS kern.boottime, Windows LastBootUpTime); throws when it cannot be read, so a caller never treats a guess as a boot. */
export function lockBootTimeMs({ platform = process.platform, readFile = fs.readFileSync, spawn = spawnSync } = {}) {
  if (platform === "linux") {
    const m = /^btime (\d+)$/m.exec(String(readFile("/proc/stat", "utf8")));
    if (!m) throw new Error("no btime in /proc/stat");
    return Number(m[1]) * 1000;
  }
  if (platform === "darwin") {
    const r = run(spawn, SYSCTL(), ["-n", "kern.boottime"]);
    const m = /sec = (\d+)/.exec(String(r.stdout ?? ""));
    if (r.error || r.status !== 0 || !m) throw new Error("cannot read kern.boottime");
    return Number(m[1]) * 1000;
  }
  if (platform === "win32") {
    const script = "'BOOT:' + ([DateTimeOffset](Get-CimInstance Win32_OperatingSystem).LastBootUpTime).ToUnixTimeMilliseconds()";
    const r = run(spawn, "powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")]);
    const m = /BOOT:(\d+)/.exec(String(r.stdout ?? ""));
    if (r.error || r.status !== 0 || !m) throw new Error("cannot read the boot time");
    return Number(m[1]);
  }
  throw new Error(`no boot time on ${platform}`);
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
