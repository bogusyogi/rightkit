// Host-wide build ownership (docs/GATE-CONTRACT.md §1 "Commit-only build", "Broker lease"; joint plan step 4, G1).
//
// The gate acquires the host's build ownership BEFORE it syncs a snapshot and holds it until the stage completes, so no other
// request can replace a snapshot while anything reads it. One owner per host (`<managed root>/build-ownership/owner.json`, next
// to `snapshots/`): builds are one per host, and the snapshot's own sync lock only covers the sync itself.
//
// The owner record is written whole (create-exclusive) and names a fresh handle (the `ownership.handle` a broker request carries),
// the gate's pid & start identity, the stage, snapshot & commit, how the gate's children are contained, and whether a broker
// request may have been submitted under this handle. The handle exists before any sync or submit.
//
// A dead gate PID is NOT enough to take ownership over: compilers and Class A children can outlive a crashed gate and still read
// the snapshot. A record is reclaimed only on positive proof that everything it covers is gone:
//   - the owner process is gone (not running, or its pid now runs a process with another start identity), and
//   - its covered children are gone:
//       Windows: the gate runs inside its own named Job Object (every descendant joins it, detached ones and those Node nests in
//         its own job included; measured on the Dell, D:\scratch\rk-step4\dell-jobexp.log). A holder process outside the gate's
//         jobs keeps the job's only handle and records `drained` once the job has no active process. Proof: the job opens with 0
//         active, or it is gone and its holder recorded `drained` (or `failed` before the gate ever ran inside it). A gate whose
//         launcher's job lets children leave silently (a Node parent) cannot hold its children in the job; a self-check at
//         acquisition records that (`contained: false`), and such a gate's crash is never cleared except by a reboot.
//       POSIX, three nets, all of which must be clear: (1) the gate's process group, counting members started after the
//         ownership whose ancestry leads to the owner or to an orphan (ancestors and peers that share the group are not its
//         children); (2) no process whose environment carries the handle (RIGHTKIT_BUILD_OWNERSHIP=<handle>), which catches a
//         descendant that started its own session (Linux /proc/<pid>/environ; macOS `ps eww`, which hides the environment of
//         Apple platform binaries only); (3) no process whose working directory is inside the snapshot (Linux /proc/<pid>/cwd;
//         macOS `lsof -d cwd`, platform binaries included); (4) every process group a gate under this ownership started
//         detached (spawnUnderOwnership records it before the child runs; class A scans, right-release builds and heavy-command
//         start their children that way) is empty, and no such spawn was left half-recorded. Residual on POSIX: a descendant
//         that itself starts a new session, drops the variable (or is an Apple platform binary on macOS) AND runs outside the
//         snapshot directory while reading it.
//       Any host: a reboot after the record was written ends every process it covered.
//   - if a broker request may have been submitted, `rustright snapshot-lease status` reports REAPED for that snapshot & handle.
// Any other answer (alive, a probe that fails or is unsupported, ACTIVE, UNKNOWN, a record or holder state that cannot be read)
// is busy: never a reclaim. The same rules decide a normal release: the record is removed only when nothing but the gate itself
// is left; otherwise it stays, and the next gate reclaims it once the survivors are gone.
//
// Children inherit the ownership through RIGHTKIT_BUILD_OWNERSHIP=<handle>: a nested gate (the door running `check`) whose
// variable names the current record joins it instead of waiting on itself, and never releases it.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn as spawnDetachedDefault, spawnSync } from "./background-process.mjs";
import { managedRuntimeClient } from "./managed-cargo.mjs";
import { snapshotRoot } from "./snapshot.mjs";

export const OWNERSHIP_SCHEMA = "rightkit.build-ownership/1";
export const HOLDER_SCHEMA = "rightkit.build-ownership-holder/1";
export const OWNERSHIP_ENV = "RIGHTKIT_BUILD_OWNERSHIP";
/** The owner record's path, exported beside the handle so every process under the ownership records detached children there. */
export const OWNERSHIP_FILE_ENV = "RIGHTKIT_BUILD_OWNERSHIP_FILE";
export const LEASE_STATUS_SCHEMA = "rustright.snapshot-lease-status.v1";
const LEASE_STATES = new Set(["ACTIVE", "REAPED", "UNKNOWN"]);
// `ps` prints start times to the second: a member started in the second the gate took ownership counts as the gate's.
const START_SLACK_MS = 1000;
// Two start identities of one pid are the same process when they agree to within this (ps prints seconds; this gate's own start
// is read from its time origin on Windows, a few hundred ms after the OS creation time at most).
const SAME_START_MS = 3000;
const sameStart = (a, b) => Number.isFinite(a) && Number.isFinite(b) && Math.abs(a - b) <= SAME_START_MS;

export class OwnershipBusy extends Error {
  constructor(message, detail = {}) { super(message); this.code = "busy"; this.detail = detail; }
}

/** Where the host's owner record lives: beside the snapshot root, never in scratch. */
export function ownershipFile({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  return paths.join(paths.dirname(snapshotRoot({ platform, env, home })), "build-ownership", "owner.json");
}

export const jobName = (handle) => `Local\\RightKitBuildOwnership-${handle}`;
/** Where the process groups of children a gate under this ownership started detached (POSIX) are recorded. */
export const childrenDir = (file, handle) => path.join(path.dirname(file), `children-${handle}`);
export const holderFile = (file, handle) => path.join(path.dirname(file), `holder-${handle}.json`);

const run = (spawn, command, args, env = {}) => spawn(command, args, { encoding: "utf8", env: { ...process.env, ...env }, timeout: 60_000, maxBuffer: 1 << 26 });

// One C# type for every Windows probe: create, open, assign and count a named job.
const JOB_TYPE = `Add-Type @"
using System; using System.Runtime.InteropServices;
public static class RkJob {
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern IntPtr CreateJobObjectW(IntPtr a, string name);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern IntPtr OpenJobObjectW(uint access, bool inherit, string name);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr proc);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool QueryInformationJobObject(IntPtr job, int cls, byte[] info, int len, IntPtr ret);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr proc, IntPtr job, out bool result);
  [DllImport("kernel32.dll")] public static extern IntPtr GetCurrentProcess();
  public static int Active(IntPtr job) { var b = new byte[48]; if (!QueryInformationJobObject(job, 1, b, 48, IntPtr.Zero)) return -1; return BitConverter.ToInt32(b, 40); }
}
"@`;
const psString = (s) => `'${String(s).replace(/'/g, "''")}'`;
const encoded = (script) => Buffer.from(script, "utf16le").toString("base64");

/** The holder: creates the gate's job, puts the gate in it, then keeps the only handle until the job is empty. */
export function holderScript({ name, pid, file }) {
  return [
    "$ErrorActionPreference = 'Stop'",
    JOB_TYPE,
    `$name = ${psString(name)}; $gate = [uint32]${Number(pid)}; $file = ${psString(file)}`,
    `function Put($o) { $o.schema = '${HOLDER_SCHEMA}'; $o.holder = $PID; $o.at = (Get-Date).ToUniversalTime().ToString('o'); $tmp = "$file.$PID.tmp"; [IO.File]::WriteAllText($tmp, ($o | ConvertTo-Json -Compress)); Move-Item -Force -LiteralPath $tmp -Destination $file }`,
    "try {",
    "  $h = [RkJob]::CreateJobObjectW([IntPtr]::Zero, $name); $e = [Runtime.InteropServices.Marshal]::GetLastWin32Error()",
    "  if ($h -eq [IntPtr]::Zero) { throw \"CreateJobObject failed ($e)\" }",
    "  if ($e -eq 183) { throw \"job $name already exists\" }",
    "  $p = [RkJob]::OpenProcess(0x0101, $false, $gate); if ($p -eq [IntPtr]::Zero) { throw \"OpenProcess $gate failed ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))\" }",
    "  if (-not [RkJob]::AssignProcessToJobObject($h, $p)) { throw \"AssignProcessToJobObject failed ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))\" }",
    "  [RkJob]::CloseHandle($p) | Out-Null",
    "  Put @{ state = 'holding'; job = $name }",
    "} catch { Put @{ state = 'failed'; why = \"$_\" }; exit 1 }",
    // Reports the live count whenever it changes, drained at zero. A release asks for a fresh count by writing `<file>.query`
    // ({nonce}); the holder counts the job after reading it and answers in `<file>.ack` with that nonce (queryHolder).
    "$last = -1; $query = \"$file.query\"; $ack = \"$file.ack\"",
    "function Answer { try { $q = [IO.File]::ReadAllText($query) | ConvertFrom-Json; $n = [string]$q.nonce; if ($n -notmatch '^[0-9a-f]{32,128}$') { return }; $c = [RkJob]::Active($h); $o = @{ schema = '" + HOLDER_SCHEMA + "'; nonce = $n; active = $c; holder = $PID; at = (Get-Date).ToUniversalTime().ToString('o') }; $tmp = \"$ack.$PID.tmp\"; [IO.File]::WriteAllText($tmp, ($o | ConvertTo-Json -Compress)); Move-Item -Force -LiteralPath $tmp -Destination $ack } catch { } }",
    "while ($true) { if ([IO.File]::Exists($query)) { Answer }; $a = [RkJob]::Active($h); if ($a -eq 0) { Put @{ state = 'drained'; job = $name }; exit 0 }; if ($a -gt 0 -and $a -ne $last) { Put @{ state = 'holding'; job = $name; active = $a }; $last = $a }; Start-Sleep -Milliseconds 50 }",
  ].join("\n");
}

/**
 * A fresh count of the gate's job from its holder (outside the job): writes `<holder file>.query` with a new nonce and waits for
 * `<holder file>.ack` carrying that nonce, which the holder writes after counting the job anew. Returns `{ active, holder, at, nonce }`
 * or throws: no answer within `timeoutMs` (a dead or hung holder) is unknown, never the last count it wrote.
 */
export function queryHolder(file, { timeoutMs = 5000, nonce = crypto.randomBytes(16).toString("hex"), sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms) } = {}) {
  const query = `${file}.query`; const ack = `${file}.ack`;
  fs.rmSync(ack, { force: true });
  const tmp = `${query}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify({ nonce }), { flag: "wx", mode: 0o600 });
  try { fs.renameSync(tmp, query); } finally { fs.rmSync(tmp, { force: true }); }
  try {
    const until = Date.now() + timeoutMs;
    for (;;) {
      let answer = null;
      try { answer = JSON.parse(fs.readFileSync(ack, "utf8").replace(/^\uFEFF/, "")); } catch { answer = null; }
      if (answer?.schema === HOLDER_SCHEMA && answer.nonce === nonce) {
        if (!Number.isInteger(answer.active)) throw new Error(`the holder's answer to ${nonce} carries no count`);
        return { active: answer.active, holder: answer.holder, at: answer.at, nonce };
      }
      if (Date.now() > until) throw new Error(`the job holder did not answer a fresh count within ${timeoutMs} ms`);
      sleep(25);
    }
  } finally { fs.rmSync(query, { force: true }); fs.rmSync(ack, { force: true }); }
}

/** Run as a child of the gate (as its work runs): `IN` when this process is in job `name`, `OUT` when not, `ERR:<n>` otherwise. */
export function selfCheckScript(name) {
  return [
    "$ErrorActionPreference = 'Stop'",
    JOB_TYPE,
    `$j = [RkJob]::OpenJobObjectW(0x0004, $false, ${psString(name)}); if ($j -eq [IntPtr]::Zero) { 'ERR:' + [Runtime.InteropServices.Marshal]::GetLastWin32Error(); exit 0 }`,
    "$in = $false; if (-not [RkJob]::IsProcessInJob([RkJob]::GetCurrentProcess(), $j, [ref]$in)) { 'ERR:' + [Runtime.InteropServices.Marshal]::GetLastWin32Error(); exit 0 }",
    "if ($in) { 'IN' } else { 'OUT' }",
  ].join("\n");
}

/** Probe of a named job: `JOB:<active>` (opened), `GONE` (no such object), `ERR:<n>` otherwise; plus `BOOT:<ms>`. */
function jobProbeScript(name) {
  return [
    "$ErrorActionPreference = 'Stop'",
    JOB_TYPE,
    `$j = [RkJob]::OpenJobObjectW(0x0004, $false, ${psString(name)}); $e = [Runtime.InteropServices.Marshal]::GetLastWin32Error()`,
    "if ($j -ne [IntPtr]::Zero) { 'JOB:' + [RkJob]::Active($j); [RkJob]::CloseHandle($j) | Out-Null } elseif ($e -eq 2) { 'GONE' } else { 'ERR:' + $e }",
  ].join("\n");
}

/**
 * The host's process facts the ownership rules rely on. Each answer is a fact or a throw; a throw means "unknown" and the
 * caller treats it as busy.
 */
export function processProbe({ platform = process.platform, spawn = spawnSync, spawnHolder = spawnDetachedDefault, kill = process.kill.bind(process), readFile = fs.readFileSync, readdir = fs.readdirSync, uid = process.getuid?.() } = {}) {
  const exists = (pid) => {
    try { kill(pid, 0); return true; } catch (e) { if (e.code === "ESRCH") return false; if (e.code === "EPERM") return true; throw e; }
  };
  if (platform === "win32") {
    const ps = (script) => {
      const r = run(spawn, "powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded(script)]);
      if (r.error || r.status !== 0) throw new Error(`powershell: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}`);
      return String(r.stdout ?? "").trim();
    };
    return {
      platform,
      /** `{ alive, startedMs }`. This gate's own start comes from its time origin (no process launch). */
      identity(pid) {
        if (pid === process.pid) return { alive: true, startedMs: Math.round(performance.timeOrigin), group: null };
        if (!exists(pid)) return { alive: false, startedMs: null, group: null };
        const out = ps(`$p = Get-Process -Id ${Number(pid)} -ErrorAction SilentlyContinue; if ($p) { 'START:' + [int64](($p.StartTime.ToUniversalTime()) - [datetime]'1970-01-01Z').TotalMilliseconds } else { 'GONE' }`);
        if (out === "GONE") return { alive: false, startedMs: null, group: null };
        const m = /START:(\d+)/.exec(out);
        if (!m) throw new Error(`cannot read the start time of pid ${pid}`);
        return { alive: true, startedMs: Number(m[1]), group: null };
      },
      bootTime() {
        const m = /BOOT:(\d+)/.exec(ps("'BOOT:' + [int64](((Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime()) - [datetime]'1970-01-01Z').TotalMilliseconds"));
        if (!m) throw new Error("cannot read the boot time");
        return Number(m[1]);
      },
      /** A fresh count from the job's holder (see queryHolder): asked at release, when a probe this gate spawned would sit in the job. */
      queryHolder: (file, options) => queryHolder(file, options),
      /** `{ open: true, active }`, `{ open: false }` (no such job), or a throw. */
      jobState(name) {
        const out = ps(jobProbeScript(name));
        const job = /JOB:(-?\d+)/.exec(out);
        if (job) { const active = Number(job[1]); if (active < 0) throw new Error(`cannot count the processes of ${name}`); return { open: true, active }; }
        if (/^GONE$/m.test(out)) return { open: false };
        throw new Error(`cannot open ${name}: ${out}`);
      },
      /**
       * Starts the holder that puts `pid` in job `name`; returns once it reports holding (or throws). The holder must outlive
       * this gate: it is started by `cmd /c start /b`, so it is cmd's child, not Node's, and silently leaves the kill-on-close
       * job Node keeps for its own children (libuv sets SILENT_BREAKAWAY_OK); cmd exits at once. A DETACHED_PROCESS PowerShell
       * never starts in a console-less session (measured on the Dell over ssh, 2026-10-29), so `detached` is not used.
       */
      contain({ name, pid, file, timeoutMs = 60_000, sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms) }) {
        const child = spawnHolder("cmd.exe", ["/d", "/c", "start", "\"\"", "/b", "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded(holderScript({ name, pid, file }))], { stdio: "ignore" });
        child.unref?.();
        const until = Date.now() + timeoutMs;
        let holding = null;
        for (;;) {
          const state = readHolder(file);
          if (state?.state === "holding") { holding = state; break; }
          if (state?.state === "failed") throw new Error(`the job holder could not contain the gate: ${state.why}`);
          if (Date.now() > until) throw new Error(`the job holder did not report within ${timeoutMs} ms`);
          sleep(50);
        }
        // Self-check: a child of this gate, started as its work is started, asks Windows whether it is in the job. Under a launcher
        // whose own job lets children leave silently (a Node parent: libuv's job sets SILENT_BREAKAWAY_OK, and a job nested under
        // it loses them too) it is not, and nothing then proves this gate's work gone after a crash: the record says so and a dead
        // owner stays busy. (Counting the job's processes cannot tell: a console child's host process lands in it either way.)
        const answer = ps(selfCheckScript(name));
        if (!/^(IN|OUT)$/m.test(answer)) throw new Error(`the containment self-check gave no answer (${answer.slice(0, 120)})`);
        return { ...holding, contained: /^IN$/m.test(answer) };
      },
    };
  }
  // Every process with its parent, group and start, plus the pids that are this gate's own (itself, its ancestors and the `ps`
  // that listed them), which are never another owner's children even when they share its group.
  const psList = () => {
    const r = run(spawn, "ps", ["-A", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "lstart="], { LC_ALL: "C", LANG: "C" });
    if (r.error || r.status !== 0) throw new Error(`ps: ${r.error?.message ?? (String(r.stderr ?? "").trim() || `exit ${r.status}`)}`);
    const rows = String(r.stdout ?? "").split(/\r?\n/).map((l) => /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+?)\s*$/.exec(l)).filter(Boolean)
      .map((m) => ({ pid: Number(m[1]), ppid: Number(m[2]), group: Number(m[3]), startedMs: Date.parse(m[4].replace(/\s+/g, " ")) }));
    const byPid = new Map(rows.map((row) => [row.pid, row]));
    const own = new Set(Number.isInteger(r.pid) ? [r.pid] : []);
    for (let pid = process.pid; pid > 1 && !own.has(pid); pid = byPid.get(pid)?.ppid ?? 0) own.add(pid);
    return { rows, own };
  };
  return {
    platform,
    /** `{ alive, startedMs, group }` from `ps` (C locale). */
    identity(pid) {
      if (!exists(pid)) return { alive: false, startedMs: null, group: null };
      const row = psList().rows.find((p) => p.pid === pid);
      if (!row) return { alive: false, startedMs: null, group: null };
      if (!Number.isFinite(row.startedMs)) throw new Error(`cannot read the start time of pid ${pid}`);
      return { alive: true, startedMs: row.startedMs, group: row.group };
    },
    bootTime() {
      if (platform === "linux") {
        const m = /^btime (\d+)$/m.exec(String(readFile("/proc/stat", "utf8")));
        if (!m) throw new Error("no btime in /proc/stat");
        return Number(m[1]) * 1000;
      }
      const r = run(spawn, "sysctl", ["-n", "kern.boottime"]);
      const m = /sec = (\d+)/.exec(String(r.stdout ?? ""));
      if (r.error || r.status !== 0 || !m) throw new Error("cannot read kern.boottime");
      return Number(m[1]) * 1000;
    },
    /**
     * Process group `group` now: `members` (this gate, its ancestors and its own `ps` left out), every process by pid (for
     * ancestry) and the set of this gate's own pids.
     */
    groupSnapshot(group) {
      if (!Number.isInteger(group) || group <= 1) throw new Error(`no usable process group (${group})`);
      const { rows, own } = psList();
      const members = rows.filter((p) => p.group === group && !own.has(p.pid));
      for (const p of members) if (!Number.isFinite(p.startedMs)) throw new Error(`cannot read the start time of pid ${p.pid}`);
      return { members, byPid: new Map(rows.map((row) => [row.pid, row])), own };
    },
    /**
     * Pids of processes whose environment carries `<OWNERSHIP_ENV>=<handle>` (this gate's own chain left out). Linux reads
     * /proc/<pid>/environ; macOS reads `ps eww`, which shows the environment of every process the user may inspect except Apple
     * platform binaries (/bin/*, /usr/bin/*), which the working-directory net covers. null where neither exists.
     */
    tokenHolders(handle) {
      const token = `${OWNERSHIP_ENV}=${handle}`;
      if (fs.existsSync("/proc/self/environ")) {
        const needle = Buffer.from(`${token}\0`);
        const found = [];
        for (const name of readdir("/proc")) {
          if (!/^\d+$/.test(name) || Number(name) === process.pid) continue;
          let environ;
          try { environ = readFile(`/proc/${name}/environ`); } catch (e) { if (["EACCES", "EPERM", "ENOENT", "ESRCH"].includes(e.code)) continue; throw e; }
          if (Buffer.concat([environ, Buffer.from("\0")]).includes(needle)) found.push(Number(name));
        }
        return found;
      }
      if (platform !== "darwin") return null;
      const { own } = psList();
      const r = run(spawn, "ps", ["eww", "-A", "-o", "pid=", "-o", "command="], { LC_ALL: "C", LANG: "C" });
      if (r.error || r.status !== 0) throw new Error(`ps eww: ${r.error?.message ?? (String(r.stderr ?? "").trim() || `exit ${r.status}`)}`);
      const pattern = new RegExp(`(^|\\s)${token.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&")}(\\s|$)`);
      return String(r.stdout ?? "").split(/\r?\n/).map((l) => /^\s*(\d+)\s(.*)$/.exec(l)).filter((m) => m && !own.has(Number(m[1])) && Number(m[1]) !== r.pid && pattern.test(m[2])).map((m) => Number(m[1]));
    },
    /**
     * Pids of processes whose working directory is `dir` or inside it (this gate's own chain left out): Linux /proc/<pid>/cwd,
     * macOS `lsof -d cwd` for this user. A reader of the snapshot that left both the group and its environment still runs
     * there. null where neither exists.
     */
    cwdHolders(dir) {
      const inside = (p) => { const rel = path.relative(dir, p); return rel === "" || (!!rel && !rel.startsWith("..") && !path.isAbsolute(rel)); };
      if (fs.existsSync("/proc/self/cwd")) {
        const found = [];
        for (const name of readdir("/proc")) {
          if (!/^\d+$/.test(name) || Number(name) === process.pid) continue;
          let cwd;
          try { cwd = fs.readlinkSync(`/proc/${name}/cwd`); } catch (e) { if (["EACCES", "EPERM", "ENOENT", "ESRCH"].includes(e.code)) continue; throw e; }
          if (inside(cwd)) found.push(Number(name));
        }
        return found;
      }
      if (platform !== "darwin") return null;
      const { own } = psList();
      const r = run(spawn, "lsof", ["-a", "-d", "cwd", "-Fpn", "-u", String(uid)]);
      // Only a clean exit is a complete listing: any other status (even with output) is unknown, so the caller stays busy.
      if (r.error || r.status !== 0) throw new Error(`lsof: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}`);
      const found = []; let pid = null;
      for (const line of String(r.stdout ?? "").split(/\r?\n/)) {
        if (line.startsWith("p")) pid = Number(line.slice(1));
        else if (line.startsWith("n") && pid && !own.has(pid) && inside(line.slice(1))) found.push(pid);
      }
      return found;
    },
  };
}

/**
 * Spawns through `spawn`. A call that detaches the child (POSIX: a new session and process group, which the gate's own group no
 * longer covers) while a build ownership is held records it first: an intent file before the spawn, then the child's group (its
 * pid) once it exists, so a crashed gate's record is kept until that group is empty; a crash between the two leaves the intent,
 * which keeps the record (unknown group). Windows children stay in the gate's job, so nothing is recorded there. Every detached
 * launch on a gate path goes through here (scans, right-release builds, heavy-command).
 */
export function spawnUnderOwnership(spawn, command, args, options = {}, { env = process.env, platform = process.platform } = {}) {
  const handle = env[OWNERSHIP_ENV] ?? options?.env?.[OWNERSHIP_ENV];
  if (!options?.detached || platform === "win32" || !handle) return spawn(command, args, options);
  const merged = { ...(options.env ?? {}), ...env };
  const dir = childrenDir(merged[OWNERSHIP_FILE_ENV] || ownershipFile({ platform, env: merged }), handle);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const intent = path.join(dir, `intent-${process.pid}-${crypto.randomUUID()}.json`);
  // The child's start lower bound is taken BEFORE it is created, so however long this process pauses before recording the
  // group, every member of it started at or after the bound (a later bound would read a live group as pid reuse).
  const notBefore = Date.now() - START_SLACK_MS;
  fs.writeFileSync(intent, JSON.stringify({ by: process.pid, at: new Date().toISOString(), notBefore, command: String(command) }), { flag: "wx", mode: 0o600 });
  let child;
  try { child = spawn(command, args, options); } catch (e) { fs.rmSync(intent, { force: true }); throw e; }
  if (child?.pid) fs.writeFileSync(path.join(dir, `group-${child.pid}.json`), JSON.stringify({ group: child.pid, startedMs: notBefore, command: String(command) }), { mode: 0o600 });
  fs.rmSync(intent, { force: true });
  return child;
}

/** The holder's last report, or null when absent or unreadable. */
export function readHolder(file) {
  try { const h = JSON.parse(fs.readFileSync(file, "utf8")); return h?.schema === HOLDER_SCHEMA ? h : null; } catch { return null; }
}

/**
 * RustRight's lease state for a snapshot & ownership handle (`rustright snapshot-lease status`). Anything but a well-formed
 * answer is UNKNOWN: a missing client, a failed or unknown command, a schema or state the gate does not know.
 */
export function brokerLeaseState({ snapshot, handle, env = process.env, platform = process.platform, spawn = spawnSync, client = managedRuntimeClient(env, platform) }) {
  if (!client) return { state: "UNKNOWN", why: "no RustRight client on this host" };
  const r = run(spawn, client, ["snapshot-lease", "status", "--snapshot", snapshot, "--ownership", handle], env);
  if (r.error || r.status !== 0) return { state: "UNKNOWN", why: `rustright snapshot-lease status: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}` };
  let answer = null;
  try { answer = JSON.parse(String(r.stdout ?? "").trim().split(/\r?\n/).pop()); } catch { answer = null; }
  if (answer?.schema !== LEASE_STATUS_SCHEMA || !LEASE_STATES.has(answer.state)) return { state: "UNKNOWN", why: `unrecognised lease status ${JSON.stringify(String(r.stdout ?? "").trim().slice(0, 200))}` };
  return { state: answer.state, why: `rustright snapshot-lease status: ${answer.state}` };
}

function publish(target, text) {
  const tmp = `${target}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, text, { flag: "wx", mode: 0o600 });
  try { fs.linkSync(tmp, target); return true; } catch (e) { if (e.code === "EEXIST") return false; throw e; } finally { fs.rmSync(tmp, { force: true }); }
}
const readText = (file) => { try { return fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

/**
 * Whether group member `m` is work the owner covers. Ancestors and peers share a group without being the gate's children (the
 * shell that started it, `| tee`, a later gate started by the same script), so a member counts only when it started after the
 * ownership began AND its ancestry leads to the owner or to an orphan (re-parented to pid 1 when its parent died), never to a
 * process that existed before the ownership or to the asking gate's own chain. A member whose ancestry cannot be followed counts.
 * On Linux an orphan adopted by a subreaper (systemd --user) reads as rooted elsewhere; the environment net covers it.
 */
function coveredMember(m, owner, { byPid, own }) {
  const since = Date.parse(owner.acquiredAt) - START_SLACK_MS;
  const before = (p) => !Number.isFinite(p.startedMs) || p.startedMs < since;
  if (own.has(m.pid) || before(m)) return false;
  const seen = new Set([m.pid]);
  for (let p = byPid.get(m.ppid); p; p = byPid.get(p.ppid)) {
    if (p.pid === owner.pid && sameStart(p.startedMs, owner.startedMs)) return true;
    if (p.pid <= 1) return true;
    if (own.has(p.pid) || before(p) || seen.has(p.pid)) return false;
    seen.add(p.pid);
  }
  return true;
}

/**
 * Whether the work `owner` covers, other than the process `self` (the live gate at release, null at reclaim), is proven gone.
 * Returns the proofs, or throws a reason that is busy.
 */
function coveredWorkGone(owner, { probe, lease, file, selfPid = null }) {
  const proofs = [];
  let rebooted = false;
  // A reboot is only a question for a dead owner; a live gate releasing its own record has not outlived one.
  if (selfPid === null) { try { rebooted = probe.bootTime() > Date.parse(owner.acquiredAt); } catch { rebooted = false; } }
  if (rebooted) proofs.push("the host rebooted after the record was written");
  const c = owner.containment ?? {};
  if (!rebooted && c.kind === "job" && c.contained === false && selfPid === null) {
    throw new Error(`job ${c.name} never held its gate's children (the gate's launcher lets children leave its job silently, so they left this one too), so their exit cannot be proven; it stays held until the host reboots`);
  }
  if (!rebooted && c.kind === "job" && selfPid !== null) {
    // A live release is fenced exactly like a crash: work that was never proven inside the job cannot be proven gone by it.
    if (c.contained !== true) throw new Error(`job ${c.name} was never proven to hold this gate's children (contained: ${c.contained}), so their exit cannot be proven; it stays held until the host reboots`);
    // Anything this gate spawned to look at the job would sit in the job itself, so the count comes from the holder (outside the
    // job), asked afresh now (a nonce it must echo after counting), never its last report. Children that just exited get a moment.
    const until = Date.now() + 3000;
    let answer;
    for (;;) {
      try { answer = probe.queryHolder(holderFile(file, owner.handle)); } catch (e) { throw new Error(`job ${c.name}'s holder gave no fresh count (${e.message})`); }
      if (!Number.isInteger(answer?.active) || answer.active < 1) throw new Error(`job ${c.name}'s holder counted ${answer?.active} process(es), but this live gate is one of them`);
      if (answer.active === 1 || Date.now() >= until) break;
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100);
    }
    if (answer.active > 1) throw new Error(`job ${c.name} still holds ${answer.active} process(es) (this gate is one)`);
    proofs.push(`job ${c.name} holds only this gate (holder ${answer.holder} counted it at ${answer.at} for query ${answer.nonce})`);
  } else if (!rebooted && c.kind === "job") {
    let state;
    try { state = probe.jobState(c.name); } catch (e) { throw new Error(`job ${c.name} cannot be read (${e.message})`); }
    if (state.open) {
      const allowed = selfPid === null ? 0 : 1;
      if (state.active > allowed) throw new Error(`job ${c.name} still holds ${state.active} process(es)${selfPid === null ? "" : " (this gate is one)"}`);
      proofs.push(`job ${c.name} holds ${state.active === 0 ? "no process" : "only this gate"}`);
    } else {
      const holder = readHolder(holderFile(file, owner.handle));
      if (holder?.state === "drained") proofs.push(`job ${c.name} drained (holder pid ${holder.holder} at ${holder.at})`);
      else if (holder?.state === "failed") proofs.push(`the gate never ran inside job ${c.name} (holder failed: ${holder.why})`);
      else throw new Error(`job ${c.name} is gone but its holder recorded no drain (${holder?.state ?? "no holder record"})`);
    }
  } else if (!rebooted && c.kind === "group") {
    let snap;
    try { snap = probe.groupSnapshot(c.group); } catch (e) { throw new Error(`process group ${c.group} cannot be listed (${e.message})`); }
    const children = snap.members.filter((m) => m.pid !== selfPid && coveredMember(m, owner, snap)).map((m) => m.pid);
    if (children.length) throw new Error(`process group ${c.group} still has ${children.length} member(s) started under this ownership: ${children.join(", ")}`);
    proofs.push(`process group ${c.group} has no member started under this ownership`);
    // Children the gate started detached lead groups of their own: each recorded group must be empty too.
    const registry = childrenDir(file, owner.handle);
    let entries = [];
    try { entries = fs.readdirSync(registry); } catch (e) { if (e.code !== "ENOENT") throw new Error(`the detached-child record ${registry} cannot be read (${e.message})`); }
    const intents = entries.filter((n) => n.startsWith("intent-"));
    if (intents.length) throw new Error(`a detached spawn was in flight when its gate stopped (${intents[0]}), so its process group is unknown`);
    const groups = entries.filter((n) => /^group-\d+\.json$/.test(n));
    for (const name of groups) {
      let entry;
      try { entry = JSON.parse(fs.readFileSync(path.join(registry, name), "utf8")); } catch { throw new Error(`the detached-child entry ${name} cannot be read`); }
      if (!Number.isInteger(entry?.group) || !Number.isFinite(entry?.startedMs)) throw new Error(`the detached-child entry ${name} cannot be read`);
      let snapshot;
      try { snapshot = probe.groupSnapshot(entry.group); } catch (e) { throw new Error(`detached process group ${entry.group} cannot be listed (${e.message})`); }
      const live = snapshot.members.filter((m) => m.pid !== selfPid && m.startedMs >= entry.startedMs - START_SLACK_MS).map((m) => m.pid);
      if (live.length) throw new Error(`detached process group ${entry.group} (${entry.command}) still has ${live.length} member(s): ${live.join(", ")}`);
    }
    if (groups.length) proofs.push(`the ${groups.length} detached process group(s) it started are empty`);
    let carriers;
    try { carriers = probe.tokenHolders(owner.handle); } catch (e) { throw new Error(`processes carrying ${OWNERSHIP_ENV} cannot be listed (${e.message})`); }
    if (carriers) {
      const others = carriers.filter((pid) => pid !== selfPid);
      if (others.length) throw new Error(`process(es) ${others.join(", ")} still carry ${OWNERSHIP_ENV}=${owner.handle}`);
      proofs.push(`no process carries ${OWNERSHIP_ENV}=${owner.handle}`);
    }
    if (owner.snapshot) {
      let inside;
      try { inside = probe.cwdHolders(owner.snapshot); } catch (e) { throw new Error(`working directories cannot be listed (${e.message})`); }
      if (inside) {
        const others = inside.filter((pid) => pid !== selfPid);
        if (others.length) throw new Error(`process(es) ${others.join(", ")} still run inside the snapshot ${owner.snapshot}`);
        proofs.push(`no process runs inside ${owner.snapshot}`);
      }
    }
  } else if (!rebooted) {
    throw new Error(`the record names no containment (${JSON.stringify(c)})`);
  }
  if (owner.broker?.submitted) {
    const status = lease({ snapshot: owner.snapshot, handle: owner.handle });
    if (status.state !== "REAPED") throw new Error(`broker work may exist under this handle and its lease is ${status.state} (${status.why})`);
    proofs.push(`broker lease for ${owner.handle} is REAPED`);
  }
  return proofs;
}

/**
 * Why the record `observed` may be reclaimed (`{ owner, proofs }`), or an OwnershipBusy throw naming what is still live or
 * unknown. Pure apart from the probes.
 */
export function judgeOwner(observed, { file, probe, lease, host = os.hostname() }) {
  let owner = null;
  try { owner = JSON.parse(observed); } catch { owner = null; }
  const busy = (why, detail = {}) => new OwnershipBusy(`build ownership ${file}: ${why}`, { owner, ...detail });
  if (owner?.schema !== OWNERSHIP_SCHEMA || typeof owner.handle !== "string" || !Number.isInteger(owner.pid) || owner.pid <= 0 || owner.pid > 0x7fffffff || !Number.isFinite(Date.parse(owner.acquiredAt))) {
    throw busy("the record cannot be read as an owner record; remove it only when no build is running on this host");
  }
  if (owner.host !== host) throw busy(`held by pid ${owner.pid} on ${owner.host}`);
  let self;
  try { self = probe.identity(owner.pid); } catch (e) { throw busy(`owner pid ${owner.pid} cannot be proven gone (${e.message})`); }
  if (self.alive && (!Number.isFinite(owner.startedMs) || !Number.isFinite(self.startedMs) || sameStart(self.startedMs, owner.startedMs))) {
    throw busy(`held by pid ${owner.pid} (${owner.stage ?? "stage"} of ${owner.commit ?? "a commit"}) since ${owner.acquiredAt}`);
  }
  const proofs = [self.alive ? `owner pid ${owner.pid} now runs another process (started ${new Date(self.startedMs).toISOString()}, owner started ${new Date(owner.startedMs).toISOString()})` : `owner pid ${owner.pid} is gone`];
  try { proofs.push(...coveredWorkGone(owner, { probe, lease, file })); } catch (e) { throw busy(`owner pid ${owner.pid} is gone but ${e.message}`); }
  return { owner, proofs };
}

/**
 * Acquires the host's build ownership (or joins it, when RIGHTKIT_BUILD_OWNERSHIP names the current record). Returns
 * `{ handle, file, joined, reclaimed, owner, markBrokerSubmit(), held(), release() }`. Throws OwnershipBusy while another build
 * owns the host, its liveness is unknown, or this gate cannot be contained.
 */
export function acquireBuildOwnership({
  file = ownershipFile(),
  stage,
  snapshot,
  commit,
  platform = process.platform,
  env = process.env,
  probe = processProbe({ platform }),
  lease = (q) => brokerLeaseState({ ...q, env, platform }),
  exportEnv = true,
  hooks = {},
} = {}) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const inherited = env[OWNERSHIP_ENV];
  if (inherited) {
    const current = readText(file);
    let owner = null; try { owner = JSON.parse(current ?? ""); } catch { owner = null; }
    if (owner?.handle === inherited) {
      return { handle: inherited, file, joined: true, reclaimed: null, owner, markBrokerSubmit: () => markBrokerSubmit(file, inherited), held: () => holds(file, inherited), release: () => ({ released: false, joined: true }) };
    }
    throw new OwnershipBusy(`build ownership ${file}: ${OWNERSHIP_ENV}=${inherited} is not the current owner; a nested gate never takes ownership of its own`);
  }
  let me;
  try { me = probe.identity(process.pid); } catch (e) { throw new OwnershipBusy(`build ownership ${file}: this gate's own start identity cannot be read (${e.message})`); }
  if (!me.alive || !Number.isFinite(me.startedMs)) throw new OwnershipBusy(`build ownership ${file}: this gate's own start identity cannot be read`);
  if (platform !== "win32" && !(Number.isInteger(me.group) && me.group > 1)) throw new OwnershipBusy(`build ownership ${file}: this gate's process group cannot be read`);
  const handle = crypto.randomUUID();
  const record = {
    schema: OWNERSHIP_SCHEMA,
    handle,
    host: os.hostname(),
    platform,
    pid: process.pid,
    startedMs: me.startedMs,
    containment: platform === "win32" ? { kind: "job", name: jobName(handle) } : { kind: "group", group: me.group },
    stage: stage ?? null,
    snapshot: snapshot ?? null,
    commit: commit ?? null,
    acquiredAt: new Date().toISOString(),
    broker: { submitted: false },
  };
  const body = JSON.stringify(record);
  const release = () => {
    if (!holds(file, handle)) return { released: false, why: "no longer the owner" };
    let proofs;
    // The record as it is now (a broker submit may have been marked since it was written).
    try { proofs = coveredWorkGone(JSON.parse(readText(file)), { probe, lease, file, selfPid: process.pid }); }
    catch (e) { return { released: false, why: `left for the next gate to reclaim: ${e.message}` }; }
    finally { if (exportEnv && process.env[OWNERSHIP_ENV] === handle) { delete process.env[OWNERSHIP_ENV]; delete process.env[OWNERSHIP_FILE_ENV]; } }
    fs.rmSync(file, { force: true });
    fs.rmSync(childrenDir(file, handle), { recursive: true, force: true });
    return { released: true, proofs };
  };
  const done = (reclaimed) => {
    // Windows: the gate enters its job before it returns, so every child it starts from here on is contained.
    if (platform === "win32") {
      let held;
      try { held = probe.contain({ name: record.containment.name, pid: process.pid, file: holderFile(file, handle) }); }
      catch (e) { if (holds(file, handle)) fs.rmSync(file, { force: true }); throw new OwnershipBusy(`build ownership ${file}: ${e.message}`); }
      // A gate the self-check did not find in its job would start work nothing can later prove gone, whether it exits or crashes.
      // No work has started yet (the self-check child has exited), so it refuses now and leaves no record, instead of running and
      // leaving the host held until a reboot.
      if (held?.contained !== true) {
        if (holds(file, handle)) fs.rmSync(file, { force: true });
        throw new OwnershipBusy(`build ownership ${file}: this gate's children would not stay in job ${record.containment.name} (its launcher lets children leave their job silently, e.g. a Node parent's job), so their exit could never be proven; start the gate from a shell or PowerShell, not under such a launcher`);
      }
      record.containment = { ...record.containment, contained: true };
      rewriteRecord(file, handle, (r) => ({ ...r, containment: record.containment }));
    }
    if (exportEnv) { process.env[OWNERSHIP_ENV] = handle; process.env[OWNERSHIP_FILE_ENV] = file; }
    return { handle, file, joined: false, reclaimed, owner: record, contained: platform === "win32" ? record.containment.contained === true : true, markBrokerSubmit: () => markBrokerSubmit(file, handle), held: () => holds(file, handle), release };
  };
  if (publish(file, body)) return done(null);
  const observed = readText(file);
  if (observed === null) { if (publish(file, body)) return done(null); throw new OwnershipBusy(`build ownership ${file} was taken by another process`); }
  const judged = judgeOwner(observed, { file, probe, lease });
  hooks.afterJudge?.();
  const reclaim = `${file}.reclaim`;
  if (!publish(reclaim, body)) throw new OwnershipBusy(`build ownership ${file} is being reclaimed by another process (remove ${reclaim} only if no gate is running)`);
  try {
    if (readText(file) !== observed) throw new OwnershipBusy(`build ownership ${file} changed before it could be reclaimed`);
    fs.rmSync(file, { force: true });
    fs.rmSync(holderFile(file, judged.owner.handle), { force: true });
    fs.rmSync(childrenDir(file, judged.owner.handle), { recursive: true, force: true });
    if (!publish(file, body)) throw new OwnershipBusy(`build ownership ${file} was taken by another process`);
  } finally {
    fs.rmSync(reclaim, { force: true });
  }
  return done({ from: judged.owner.handle, pid: judged.owner.pid, proofs: judged.proofs });
}

/** Replaces the record atomically while it is still `handle`'s. */
function rewriteRecord(file, handle, change) {
  const text = readText(file);
  let owner = null; try { owner = JSON.parse(text ?? ""); } catch { owner = null; }
  if (owner?.handle !== handle) throw new OwnershipBusy(`build ownership ${file} is no longer held by ${handle}`);
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(change(owner)), { flag: "wx", mode: 0o600 });
  try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
}

function holds(file, handle) {
  try { return JSON.parse(fs.readFileSync(file, "utf8")).handle === handle; } catch { return false; }
}

/**
 * Records, before the first broker submit, that broker work may exist under this handle; from then on the record is reclaimed
 * only on the broker's REAPED. Replaces the record atomically and only while it is still this handle's.
 */
function markBrokerSubmit(file, handle) {
  const text = readText(file);
  let owner = null; try { owner = JSON.parse(text ?? ""); } catch { owner = null; }
  if (owner?.handle !== handle) throw new OwnershipBusy(`build ownership ${file} is no longer held by ${handle}; no broker request may be submitted`);
  if (owner.broker?.submitted) return owner;
  const next = { ...owner, broker: { ...(owner.broker ?? {}), submitted: true, at: new Date().toISOString() } };
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(next), { flag: "wx", mode: 0o600 });
  try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
  if (!holds(file, handle)) throw new OwnershipBusy(`build ownership ${file} changed while recording a broker submit`);
  return next;
}
