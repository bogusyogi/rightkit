// Host-wide build ownership (docs/GATE-CONTRACT.md §1 "Commit-only build", "Broker lease"; joint plan step 4, G1).
//
// The gate acquires the host's build ownership BEFORE it syncs a snapshot and holds it until the stage completes, so no other
// request can replace a snapshot while anything reads it. One owner per host, at ONE fixed location per host (`ownershipFile`:
// `<managed root>/build-ownership/owner.json` on Mac & Dell, `~/.local/state/rightkit/build-ownership/owner.json` on Linux), never
// derived from the snapshot root or from an environment variable: builds are one per host, and the snapshot's own sync lock only
// covers the sync itself. The directory is mode 0700 and refused when group- or other-accessible (`ensureOwnershipDir`).
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
//       POSIX, four nets, all of which must be clear: (1) the gate's process group, counting members started after the
//         ownership whose ancestry leads to the owner, to an orphan (re-parented to pid 1) or to ANY other process that predates
//         the ownership (an orphan adopted by a subreaper such as systemd --user or WSL's init reads that way; only the owner's
//         recorded parent, or a process that shares the member's group, is a peer that is not the gate's child); (2) no process
//         whose environment carries the handle (RIGHTKIT_BUILD_OWNERSHIP=<handle>), which catches a descendant that started its
//         own session (Linux /proc/<pid>/environ; macOS `ps eww`, which hides the environment of Apple platform binaries only);
//         (3) no process that uses the snapshot directory in any way that survives a new session, a scrubbed environment and a
//         working directory elsewhere: its working or root directory, executable, an open file descriptor or a mapped file
//         inside it (Linux /proc/<pid>/{cwd,root,exe,fd,maps}; macOS `lsof`, platform binaries included), or an argument naming
//         it (/proc/<pid>/cmdline; macOS `ps -ww`); (4) every process group a gate under this ownership started detached
//         (spawnUnderOwnership records it before the child runs; class A scans, right-release builds and heavy-command start
//         their children that way) is empty, and no such spawn was left half-recorded.
//         Unknown is held: on Linux a /proc entry of this user's that cannot be read (EACCES/EPERM, e.g. a non-dumpable process)
//         never counts as "not a reader" by default. It is skipped only when it vanished (ENOENT/ESRCH), belongs to another user
//         (/proc/<pid>/status, world-readable), or the INVARIANT below holds: such a process is out of this ownership's reach only
//         when it started before this ownership's baseline (/proc/<pid>/stat field 22 against the boot time, less START_SLACK_MS
//         of reading error): the later of the acquisition of the previous owner whose release proved zero readers (a durable marker,
//         `last-clean-release.json` beside the record, written only by a clean release; a record carries it as `cleanBefore`) and
//         the snapshot root's birth in this boot, capped at acquisition; with neither, this record's own acquisition. A process
//         that predates THIS ownership but started after the previous acquisition may be the previous owner's leftover, so it
//         stays held when a baseline from a clean release or a birth exists (it is only dropped when it predates that baseline). An
//         unreadable stat: the process is held. Residual on POSIX, stated plainly: a descendant that started a
//         new session, dropped the variable, runs outside the snapshot directory, names it in no argument AND holds no file in it
//         open or mapped at the instant the nets sample it (it reads between samples) is invisible; a sampled net cannot prove
//         such a process will not read next. (On macOS a hidden-environment process is attributed by no start time: it is reported in
//         the output and never held; rightkit#1313.) Another user's unreadable processes are
//         skipped on the premise that they cannot read this user's managed root.
//       Any host: a reboot after the record was written ends every process it covered.
//   - if a carried (snapshot-bound) request was sent under the handle (`broker.submitted`, set only just before such a send; an uncarried
//     probe or an installed carrier map never sets it), `rustright snapshot-lease status` reports REAPED for that snapshot & handle.
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
import { resolveManagedRoots } from "./managed-roots.mjs";
import { requireManagedRoot } from "./managed-root-guard.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, OwnershipBusy, holds, markBrokerSubmit, markBrokerUnsent, markCarriedSend, markLockFile } from "./ownership-mark.mjs";

// The mark itself lives in a leaf module (ownership-mark.mjs): snapshot-broker.mjs records carried sends through it, and importing this
// file from there would close a cycle (this file -> managed-cargo -> cargo-target-shim -> snapshot-broker).
export { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, OwnershipBusy, markCarriedSend, markLockFile };

export const OWNERSHIP_SCHEMA = "rightkit.build-ownership/1";
export const HOLDER_SCHEMA = "rightkit.build-ownership-holder/1";
/** The owner record's path, exported beside the handle so every process under the ownership records detached children there. */
/** GB-85: how long a successful stage's release waits for every submission under its handle to be durably complete (the broker's own bounded `--wait-ms`, never a poll of the build). */
export const RELEASE_LEASE_WAIT_MS = 15_000;
export const LEASE_STATUS_SCHEMA = "rustright.snapshot-lease-status.v1";
const LEASE_STATES = new Set(["ACTIVE", "REAPED", "UNKNOWN"]);
// A start time read from `ps` (or /proc/<pid>/stat) is never late, only early: ps prints whole seconds (up to 1 s early), and on
// Linux the figure is btime (whole seconds, so the boot is up to 1 s early) plus the process's ticks: measured on WSL2, a process
// 98 ms old read as 788 ms old. So a start can read up to about 2 s before the process really began; this slack (with margin) is
// how much earlier than an instant a reading may be and still be "not before" it. A member started right after the gate took
// ownership counts as the gate's however its start reads.
const START_SLACK_MS = 3000;
// Two start identities of one pid are the same process when they agree to within this (ps prints seconds; this gate's own start
// is read from its time origin on Windows, a few hundred ms after the OS creation time at most).
const SAME_START_MS = 3000;
const sameStart = (a, b) => Number.isFinite(a) && Number.isFinite(b) && Math.abs(a - b) <= SAME_START_MS;


/** The directory beside the host's snapshots (or beside any injected root) that holds the owner record. */
export const OWNERSHIP_DIRNAME = "build-ownership";

/**
 * Where the host's one owner record lives (GC:24 "the host's build ownership"; GC:52 "host-wide build ownership"; item 5 row 5.17):
 * ONE fixed location per host, independent of the snapshot root and of every environment variable. Two gates whose environments
 * choose different snapshot roots (XDG_STATE_HOME, a configured root) must still contend for one record, so nothing here reads
 * `process.env`, and nothing derives from where snapshots live.
 *   Mac & Dell: `<managed root>/build-ownership/owner.json` (the host's fixed managed root, `/Volumes/D/.rightkit-managed`,
 *     `D:\.rightkit-managed`).
 *   Linux (WSL2 included): `<account home>/.local/state/rightkit/build-ownership/owner.json`, the account home taken from the
 *     passwd database (`os.userInfo()`), never from $HOME.
 * `home` replaces the Linux account home and `beside` replaces the whole location (`<dirname(beside)>/build-ownership/owner.json`);
 * both are injection points for fixtures and tests only. A production caller passes neither: the root a caller derived from the
 * environment must never be passed as `beside`, because that is the defect this row removes.
 */
export function ownershipFile({ platform = process.platform, home, beside } = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  if (beside !== undefined) {
    if (typeof beside !== "string" || beside === "") throw new TypeError(`ownershipFile: beside must be a path (${JSON.stringify(beside)})`);
    return paths.join(paths.dirname(beside), OWNERSHIP_DIRNAME, "owner.json");
  }
  if (platform === "darwin" || platform === "win32") return paths.join(resolveManagedRoots({ env: {}, platform, home: "" }).dataRoot, OWNERSHIP_DIRNAME, "owner.json");
  let base = home;
  if (base === undefined) {
    try { base = os.userInfo().homedir; } catch (e) { throw new OwnershipBusy(`build ownership: this account's home directory cannot be read (${e.message}), so the host's owner record has no location`); }
  }
  return paths.join(base, ".local", "state", "rightkit", OWNERSHIP_DIRNAME, "owner.json");
}

/**
 * The managed-root guard, FIRST in `acquireBuildOwnership` (decision D4, rows 5.15 and 5.17): on Mac & Dell, when the record is at
 * the host's own location (inside the host's managed root, or inside the `managedRoot` a fixture injects), the managed root must
 * already exist, be a plain directory and, on the Mac, live on another device than `/`. Taking ownership creates `build-ownership/`,
 * and must never be what brings a missing managed root (or a boot-volume substitute for an unmounted one) into being. Throws the
 * guard's named refusal (`code: "refused"`, `reason`), before anything is read or written. Linux has no managed root.
 */
function guardManagedRoot(file, { platform, managedRoot, device }) {
  if (platform !== "darwin" && platform !== "win32") return;
  const root = managedRoot ?? resolveManagedRoots({ env: {}, platform, home: "" }).dataRoot;
  if (managedRoot === undefined) {
    const paths = platform === "win32" ? path.win32 : path.posix;
    const fold = (x) => paths.resolve(x).toLowerCase();   // both volumes are case-insensitive by default
    const rel = paths.relative(fold(root), fold(file));
    if (rel === "" || rel === ".." || rel.startsWith(`..${paths.sep}`) || paths.isAbsolute(rel)) return; // a record elsewhere (a fixture's): not the host's
  }
  requireManagedRoot(root, { platform, device });
}

/**
 * Creates the ownership directory mode 0700 (one level on Mac & Dell) and refuses an existing POSIX one that grants group or other access (item 5 row 5.22):
 * another user's unreadable process is skipped by the nets on the premise that it cannot read this user's managed root, and a
 * directory other users could write would let them forge or remove the record. Windows has no mode bits; its directory ACL is the
 * same trust the record itself has.
 */
export function ensureOwnershipDir(dir, { platform = process.platform } = {}) {
  // Mac & Dell: exactly ONE level, `build-ownership/` inside a managed root that already exists (decision D4: the gate never
  // creates the managed root, and a recursive creation here would). Linux: the durable state directory under the account home is
  // created whole (GC:63).
  if (platform === "linux") fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  else { try { fs.mkdirSync(dir, { mode: 0o700 }); } catch (e) { if (e.code !== "EEXIST") throw e; } }
  if (platform === "win32" || process.platform === "win32") return;
  const st = fs.statSync(dir);
  if (!st.isDirectory()) throw new OwnershipBusy(`build ownership directory ${dir} is not a directory`);
  if ((st.mode & 0o077) !== 0) throw new OwnershipBusy(`build ownership directory ${dir} is mode ${(st.mode & 0o777).toString(8).padStart(3, "0")}: group or other users can reach it, so the premise that another user's processes cannot read this user's managed root does not hold; chmod 700 ${dir} (nothing ran)`, { dir, mode: st.mode & 0o777 });
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

/** Where the holder's script is written for PowerShell -File (beside the holder record). */
export const holderScriptFile = (file) => `${file}.ps1`;
/** cmd.exe's longest command line; the holder's launch must stay under it. */
export const CMD_LINE_LIMIT = 8191;
/** The holder's launch: `cmd /d /c start "" /b powershell -File <script>` (see contain). */
export const holderLaunchArgs = (script) => ["/d", "/c", "start", "\"\"", "/b", "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script];

/** The holder: creates the gate's job, puts the gate in it, then keeps the only handle until the job is empty. */
export function holderScript({ name, pid, file, selfDelete = false }) {
  return [
    "$ErrorActionPreference = 'Stop'",
    ...(selfDelete ? ["try { Remove-Item -LiteralPath $PSCommandPath -Force } catch { }"] : []),
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
    "$last = -1; $query = \"$file.query\"; $ack = \"$file.ack\"; $answered = ''",
    // Each nonce is answered once (rewriting the ack would contend with the reader removing it).
    "function Answer { try { $q = [IO.File]::ReadAllText($query) | ConvertFrom-Json; $n = [string]$q.nonce; if ($n -notmatch '^[0-9a-f]{32,128}$' -or $n -eq $script:answered) { return }; $c = [RkJob]::Active($h); $o = @{ schema = '" + HOLDER_SCHEMA + "'; nonce = $n; active = $c; holder = $PID; at = (Get-Date).ToUniversalTime().ToString('o') }; $tmp = \"$ack.$PID.tmp\"; [IO.File]::WriteAllText($tmp, ($o | ConvertTo-Json -Compress)); Move-Item -Force -LiteralPath $tmp -Destination $ack; $script:answered = $n } catch { } }",
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
  } finally {
    // The holder may hold either file open for a moment (Windows refuses the removal then); a leftover is harmless: the next
    // query replaces the query and removes the ack before it waits, and only an answer with its own nonce counts.
    for (const f of [query, ack]) { try { fs.rmSync(f, { force: true }); } catch { /* left for the next query */ } }
  }
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

const PATH_CHAR = /[A-Za-z0-9._@+~-]/;
/** Whether `text` (an argument list) names the directory `dir`: `dir` itself or a path under it, not a longer sibling name. */
export function namesDir(text, dir) {
  const d = dir.length > 1 ? dir.replace(/\/+$/, "") : dir;
  if (!d) return false;
  for (let i = text.indexOf(d); i !== -1; i = text.indexOf(d, i + 1)) {
    const before = i === 0 ? "" : text[i - 1]; const after = text[i + d.length] ?? "";
    if ((before === "" || !PATH_CHAR.test(before)) && (after === "" || after === "/" || !PATH_CHAR.test(after))) return true;
  }
  return false;
}

/** The most parent links `classifyHiddenProcesses` follows from one process before it holds it (a cycle or an absurdly deep chain is unknown). */
export const HIDDEN_WALK_LIMIT = 1024;
/** What `launchctl blame <pid>` prints (exit 3) for a process launchd does not manage, or that no longer exists. */
export const NOT_MANAGED_LINE = "Process is not managed by launchd or does not exist.";
/** `launchctl blame` answer -> `managed` (a launchd job: exit 0 with a reason), `unmanaged` (exit 3, the line above) or `unknown` (anything else). */
export function blameKind(answer) {
  const line = String(answer?.line ?? "").trim();
  if (answer?.rc === 3 && line === NOT_MANAGED_LINE) return "unmanaged";
  if (answer?.rc === 0 && line !== "" && line !== NOT_MANAGED_LINE) return "managed";
  return "unknown";
}

/**
 * macOS (item 5 row 5.24a, rule 5.24a' of signed/item5-sage-decisions-2.md D1 with D2; item186-decision.md:47-71 with the gate's
 * anchor in place of the broker's root start). `ps eww` prints no environment for an Apple platform binary (/bin/*, /usr/bin/*,
 * system daemons), so the token net cannot tell such a process from one that never carried the handle, and the snapshot-use net
 * samples only the instant it runs. A gate descendant that is a platform binary, started its own session and touches nothing in the
 * snapshot at that instant would escape both. So: hidden means held after the anchor, cleared only by lineage proof.
 *
 * `procs` are the live processes as `{ pid, ppid, group, startedMs, uid, state, hidden, uninspected, command }` (`startedMs` has
 * `ps lstart`'s one-second resolution; `state` is `ps stat`, whose `X` means traced; `hidden`: `ps eww` shows no environment, an
 * empty environment included; `uninspected`: `lsof` listed nothing for it). `anchorMs` is the baseline (the later of the last clean
 * release and the snapshot root's birth in this boot, capped at acquisition); with none, the anchor is the record's own acquisition
 * (`owner.acquiredAt`), never "no anchor": a process that started before this ownership began is no unproven work of any owner,
 * because every record is published on a free host after a release that proved zero readers, or by a reclaim that proved the
 * previous work gone. `owner` is the record; `recorded` the detached children the gate recorded, `[{ group, command }]` (a child
 * leads the group named by its own pid). `blame(pid) -> { rc, line }` is `launchctl blame`; `reread(pids) -> Map(pid -> { ppid,
 * startedMs })` re-lists pids (the binding against pid reuse).
 *
 * A candidate is a same-user, live (not a zombie) process the gate did not itself start for this probe, that is hidden or that
 * `lsof` could not inspect. Each is judged in this order:
 *   (t) it, or a process on its walk, is traced (a ptrace attach reparents to the tracer) -> HELD.
 *   (a) it started before the anchor second -> drop. The anchor second itself counts as after: `lstart` is whole seconds.
 *   (b) it is in a recorded detached group -> HELD. It is in the owner's group -> HELD unless its parent is one of the owner's
 *       recorded ancestors, matched by pid and start identity (`gate | tee`: that ancestor's other child), when it goes to (c).
 *       The owner's own recorded ancestors are never its children and are not candidates.
 *   (c) walk up from it; for each parent, in order:
 *       the parent is the owner or a recorded child; the parent cannot be read; the parent started after its child (pid reuse);
 *       or the walk passes HIDDEN_WALK_LIMIT steps or loops -> HELD.
 *       The parent is launchd (pid 1): the child X on the walk started at or after the anchor; `launchctl blame X` decides. A
 *       launchd job (exit 0 and a reason) means the chain was born under launchd and never passed through the gate -> drop,
 *       once the re-read after the blame calls confirms X (still a child of pid 1, same start). Not managed (exit 3) is a reparented
 *       orphan, or a vanished one -> HELD. Anything else (another exit code, no output, a spawn error, a timeout) -> HELD.
 *       The parent provably started before the anchor, and is not launchd -> drop (a terminal opened after the anchor under a
 *       Terminal that predates it).
 *       Otherwise move to the parent and repeat. A recorded ancestor is walked like any other process: the exemption in (b) is never
 *       an immediate drop.
 * Residual R1 (trusted per-user model, AR:273-276): a gate descendant can ask launchd to spawn a process (`launchctl submit`, `open`, an
 * XPC service, a LaunchAgent). That process is launchd-born, so it is dropped. macOS has no subreaper, so nothing else reparents a
 * descendant to a process that predates the anchor.
 * Returns `[{ pid, why }]`, every process held, each reason naming the pid.
 */
export function classifyHiddenProcesses({ procs, own = new Set(), uid, owner, recorded = [], anchorMs, alive = () => true, blame = null, reread = null }) {
  const byPid = new Map(procs.map((p) => [p.pid, p]));
  const anchor = Number.isFinite(anchorMs) ? anchorMs : Date.parse(owner?.acquiredAt ?? "");
  const anchorSecond = Number.isFinite(anchor) ? Math.floor(anchor / 1000) * 1000 : NaN;
  const ownerGroup = Number.isInteger(owner?.containment?.group) ? owner.containment.group : null;
  const recordedGroups = new Set(recorded.map((r) => r.group));
  const recordedChild = (p) => recordedGroups.has(p.pid);
  const isOwner = (p) => p.pid === owner?.pid && sameStart(p.startedMs, owner?.startedMs);
  const ancestors = Array.isArray(owner?.ancestors) ? owner.ancestors : [];
  const isAncestor = (p) => ancestors.some((a) => a?.pid === p.pid && sameStart(a.startedMs, p.startedMs));
  const traced = (p) => /X/.test(String(p.state));
  const before = (p) => p.startedMs < anchorSecond;
  const held = [];
  const cleared = [];                                   // [{ p, top, hold }]: dropped by a launchd-job top, pending the re-read
  const blamed = new Map();                             // one answer per pid within this sample
  const blameOf = (pid) => {
    if (!blamed.has(pid)) { let a; try { a = typeof blame === "function" ? blame(pid) : null; } catch { a = null; } blamed.set(pid, a); }
    return blamed.get(pid);
  };
  const name = (p) => `pid ${p.pid} (${String(p.command ?? "?").slice(0, 80)}), started ${Number.isFinite(p.startedMs) ? new Date(p.startedMs).toISOString() : "at a time that cannot be read"}`;
  for (const p of procs) {
    if (own.has(p.pid) || p.uid !== uid || /^Z/.test(String(p.state)) || !(p.hidden || p.uninspected)) continue;
    const what = p.hidden ? "its environment is hidden (`ps eww` shows none)" : "`lsof` cannot inspect it";
    const hold = (why) => { if (alive(p.pid)) held.push({ pid: p.pid, why: `${name(p)}: ${what}, and ${why}` }); };
    if (isOwner(p) || isAncestor(p)) continue; // the owner, or one of its parents: never its child
    if (!Number.isFinite(anchorSecond)) { hold("this ownership has no anchor and its record has no readable acquisition time, so nothing hidden can be cleared"); continue; }
    if (traced(p)) { hold("it is traced (a ptrace attach reparents it to the tracer), so its lineage proves nothing"); continue; } // (t)
    if (!Number.isFinite(p.startedMs)) { hold("its start time cannot be read"); continue; }
    if (before(p)) continue; // (a)
    const parentRow = byPid.get(p.ppid);
    const peer = !!parentRow && isAncestor(parentRow) && !traced(parentRow);
    if (recordedGroups.has(p.group)) { hold(`it is in process group ${p.group}, a detached group this gate recorded`); continue; } // (b)
    if (p.group === ownerGroup && !peer) { hold(`it is in process group ${p.group}, the owner's group`); continue; }
    // (c)
    let x = p; let verdict = null; let top = null; const seen = new Set([p.pid]);
    for (let steps = 0; verdict === null; steps++) {
      if (steps >= HIDDEN_WALK_LIMIT) { verdict = `the ancestry walk passed ${HIDDEN_WALK_LIMIT} steps`; break; }
      const parent = byPid.get(x.ppid);
      if (!parent) { verdict = `the parent of pid ${x.pid} (pid ${x.ppid}) cannot be read`; break; }
      if (isOwner(parent)) { verdict = `its ancestry reaches the owner (pid ${parent.pid}) through pid ${x.pid}`; break; }
      if (recordedChild(parent)) { verdict = `its ancestry reaches a child this gate recorded (pid ${parent.pid}) through pid ${x.pid}`; break; }
      if (traced(parent)) { verdict = `pid ${parent.pid} on its walk is traced, so its lineage proves nothing`; break; }
      if (parent.pid === 1) {
        const answer = blameOf(x.pid);
        const kind = blameKind(answer);
        if (kind === "managed") { verdict = false; top = x; break; }
        verdict = kind === "unmanaged" ? `pid ${x.pid} is an orphan: its parent is launchd, it started at or after the anchor, and launchctl blame says it is not managed by launchd (a reparented orphan, or one that vanished)`
          : `pid ${x.pid}'s parent is launchd and launchctl blame gave no usable answer (${answer ? `exit ${answer.rc}, ${JSON.stringify(String(answer.line ?? "").slice(0, 80))}` : "it could not run"}), so it is unknown`;
        break;
      }
      if (!Number.isFinite(parent.startedMs) || parent.startedMs > x.startedMs) { verdict = `pid ${parent.pid} started after its child pid ${x.pid} (pid reuse), so the lineage cannot be trusted`; break; }
      if (before(parent)) { verdict = false; break; } // a parent that provably predates the anchor, and is not launchd
      if (seen.has(parent.pid)) { verdict = `the ancestry of pid ${p.pid} loops at pid ${parent.pid}`; break; }
      seen.add(parent.pid); x = parent;
    }
    if (verdict) hold(verdict);
    else if (top) cleared.push({ p, top, hold });
  }
  // Binding against pid reuse: a top cleared as a launchd job is re-listed after the blame calls; one that is no longer a child of
  // launchd, started at another time, or is gone, was another process when it was judged.
  if (cleared.length) {
    const tops = [...new Set(cleared.map((c) => c.top.pid))];
    let fresh = null; try { fresh = typeof reread === "function" ? reread(tops) : null; } catch { fresh = null; }
    for (const c of cleared) {
      const f = fresh?.get(c.top.pid);
      if (!f) c.hold(fresh ? `pid ${c.top.pid}, the launchd job it descends from, is gone on the re-read, so its lineage changed during the sample` : `pid ${c.top.pid}, the launchd job it descends from, could not be re-read, so the answer cannot be bound to it`);
      else if (f.ppid !== 1 || f.startedMs !== c.top.startedMs) c.hold(`pid ${c.top.pid}, the launchd job it descends from, is not the same process on the re-read (parent ${f.ppid}, started ${Number.isFinite(f.startedMs) ? new Date(f.startedMs).toISOString() : "?"}; it was parent 1, started ${new Date(c.top.startedMs).toISOString()}): pid reuse`);
    }
  }
  return held;
}

/**
 * The host's process facts the ownership rules rely on. Each answer is a fact or a throw; a throw means "unknown" and the
 * caller treats it as busy.
 */
export function processProbe({ platform = process.platform, spawn = spawnSync, spawnHolder = spawnDetachedDefault, kill = process.kill.bind(process), readFile = fs.readFileSync, readdir = fs.readdirSync, readlink = fs.readlinkSync, exists: procExists = fs.existsSync, proc = "/proc", uid = process.getuid?.() } = {}) {
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
      /**
       * `{ alive, startedMs }`. This gate's own start comes from its time origin (no process launch). Another process's start is
       * a Unix time in ms from `[DateTimeOffset]`: `[datetime]'1970-01-01Z'` parses as LOCAL time, so subtracting it was off by the
       * zone's offset (5.5 h on the Dell, 2026-10-29), which made a live owner read as another process and, west of UTC, a reboot
       * appear to have happened.
       */
      identity(pid) {
        if (pid === process.pid) return { alive: true, startedMs: Math.round(performance.timeOrigin), group: null };
        if (!exists(pid)) return { alive: false, startedMs: null, group: null };
        const out = ps(`$p = Get-Process -Id ${Number(pid)} -ErrorAction SilentlyContinue; if ($p) { 'START:' + ([DateTimeOffset]$p.StartTime).ToUnixTimeMilliseconds() } else { 'GONE' }`);
        if (out === "GONE") return { alive: false, startedMs: null, group: null };
        const m = /START:(\d+)/.exec(out);
        if (!m) throw new Error(`cannot read the start time of pid ${pid}`);
        return { alive: true, startedMs: Number(m[1]), group: null };
      },
      bootTime() {
        const m = /BOOT:(\d+)/.exec(ps("'BOOT:' + ([DateTimeOffset](Get-CimInstance Win32_OperatingSystem).LastBootUpTime).ToUnixTimeMilliseconds()"));
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
        // The script goes in a file beside the holder record (mode 0600 means nothing on NTFS: the file has the ownership directory's
        // ACL, the same trust the record itself has), never on the command line: cmd.exe cuts a command line at
        // 8191 characters, and the encoded holder passed that once it answered fresh queries (the holder never started; Dell
        // 2026-10-29, D:\scratch\rk-step4\dell-ev7-build-ownership.log). The holder deletes the file once PowerShell has read it.
        const script = holderScriptFile(file);
        fs.rmSync(script, { force: true });
        fs.writeFileSync(script, holderScript({ name, pid, file, selfDelete: true }), { flag: "wx", mode: 0o600 });
        const holding = (() => {
          try {
            const child = spawnHolder("cmd.exe", holderLaunchArgs(script), { stdio: "ignore" });
            child.unref?.();
            const until = Date.now() + timeoutMs;
            for (;;) {
              const state = readHolder(file);
              if (state?.state === "holding") return state;
              if (state?.state === "failed") throw new Error(`the job holder could not contain the gate: ${state.why}`);
              if (Date.now() > until) throw new Error(`the job holder did not report within ${timeoutMs} ms`);
              sleep(50);
            }
          } finally { fs.rmSync(script, { force: true }); }
        })();
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
    return { rows, own, byPid };
  };
  // Linux /proc. An entry this gate cannot read is a fact to decide, never a silent skip: gone (ENOENT/ESRCH) is not a reader;
  // another user's process is skipped (it cannot read this user's managed root; its uid comes from /proc/<pid>/status, readable
  // by everyone); anything else, including a process of this user (EACCES/EPERM: non-dumpable, hidepid) is UNKNOWN and throws, so
  // the caller stays busy.
  const GONE = new Set(["ENOENT", "ESRCH"]);
  // A real Linux host always has /proc, so its absence there is unknown (throws below); a Linux platform simulated on another host
  // (gate-contract's linux cases on a Mac) has nothing real to read and falls through to "no net".
  const procAvailable = () => platform === "linux" && (process.platform === "linux" || procExists(proc));
  const procPids = () => {
    if (!procExists(proc)) throw new Error(`no ${proc} on this linux host`);
    return readdir(proc).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid).map(Number);
  };
  const bootMs = () => {
    const m = /^btime (\d+)$/m.exec(String(readFile(`${proc}/stat`, "utf8")));
    if (!m) throw new Error(`no btime in ${proc}/stat`);
    return Number(m[1]) * 1000;
  };
  // When pid started, from /proc/<pid>/stat field 22 (clock ticks since boot, USER_HZ = 100 on Linux) and the boot time. stat and
  // status stay readable for non-dumpable processes. btime has one-second resolution, so the figure can be up to a second early.
  const TICKS_PER_SECOND = 100;
  const startedAt = (pid, boot) => {
    const text = String(readFile(`${proc}/${pid}/stat`, "utf8"));
    const fields = text.slice(text.lastIndexOf(")") + 2).split(" "); // from field 3 (state) on; comm may hold spaces and parens
    const ticks = Number(fields[19]);
    if (!Number.isInteger(ticks)) throw new Error(`cannot read the start time in ${proc}/${pid}/stat`);
    return boot + Math.round((ticks * 1000) / TICKS_PER_SECOND);
  };
  // THE INVARIANT behind skipping an unreadable (EACCES/EPERM) process of this user, which could be reading the snapshot unseen:
  // it is skipped only if it started before `skipBefore` (less START_SLACK_MS of reading error), the BASELINE of this ownership,
  // which is the later of
  //   (1) the acquisition of the previous owner whose release proved zero readers (the record's `cleanBefore`, from the durable
  //       marker of that release): the process predates that owner too, so it is no leftover of any owner whose work is unresolved;
  //   (2) the birth of the snapshot root in this boot (the root's birth record), capped at this ownership's acquisition: a process
  //       that started before the root existed cannot have held it open from before, and one that started before this ownership
  //       is not the gate's own.
  // With neither (a previous release that ended held, unknown or reclaimed leaves no marker; a missing, malformed or other-boot
  // birth record) the baseline is this ownership's own acquisition (decision D1, 5.23 amended): nothing that started before this
  // record was published is the unproven work of any owner, because every record is published on a free host after a release that
  // proved zero readers, or by a reclaim that proved the previous work gone. A process that started at or after that, when its
  // start time cannot be read, or when its stat is unreadable, stays held.
  const unreadable = (pid, e, skipBefore) => {
    if (GONE.has(e.code)) return;
    if (!["EACCES", "EPERM"].includes(e.code)) throw e;
    let status;
    try { status = String(readFile(`${proc}/${pid}/status`, "utf8")); } catch (e2) { if (GONE.has(e2.code)) return; throw new Error(`${proc}/${pid} is unreadable (${e.code}) and so is its owner (${e2.code ?? e2.message}), so what it reads is unknown`); }
    // A zombie (Z) or dead (X) process has no memory, descriptors or working directory, and its /proc entries refuse reads: it reads nothing.
    if (/^State:\s*[ZXx]/m.test(status)) return;
    const m = /^Uid:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)/m.exec(status);
    if (!m) throw new Error(`${proc}/${pid} is unreadable (${e.code}) and its owner cannot be read, so what it reads is unknown`);
    if (Number.isInteger(uid) && ![1, 2, 3, 4].some((i) => Number(m[i]) === uid)) return;
    const what = `${/^Name:\s*(.*)$/m.exec(status)?.[1] ?? "?"}, ${/^State:\s*(\S+)/m.exec(status)?.[1] ?? "?"}`;
    const unknown = (why) => new Error(`${proc}/${pid} (a process of this user: ${what}) is unreadable (${e.code}) and ${why}, so what it reads is unknown`);
    if (!Number.isFinite(skipBefore)) throw unknown("there is no baseline for this ownership (no clean previous release is recorded, and no birth record of the snapshot root in this boot)");
    let startMs;
    try { startMs = startedAt(pid, bootMs()); } catch (e3) { if (GONE.has(e3.code)) return; throw unknown(`its start time cannot be read (${e3.code ?? e3.message})`); }
    if (startMs + START_SLACK_MS < skipBefore) return;
    throw unknown(`it started at ${new Date(startMs).toISOString()}, not before the baseline ${new Date(skipBefore).toISOString()} (the later of the last clean release's acquisition and the snapshot root's birth, else this ownership's own acquisition)`);
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
    /** This boot's identity for a snapshot root's birth record (null when unknown). */
    bootId() { return platform === "linux" ? hostBootId({ platform, readFile, bootIdFile: `${proc}/sys/kernel/random/boot_id` }) : hostBootId({ platform, spawn }); },
    bootTime() {
      if (platform === "linux") {
        return bootMs();
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
     * /proc/<pid>/environ (an unreadable entry of this user's throws: unknown); macOS reads `ps eww`, which shows the environment
     * of every process the user may inspect except Apple platform binaries (/bin/*, /usr/bin/*), which `hiddenHolders` judges by
     * lineage and the snapshot-use net covers. null where neither exists.
     */
    tokenHolders(handle, skipBefore) {
      const token = `${OWNERSHIP_ENV}=${handle}`;
      if (procAvailable()) {
        const needle = Buffer.from(`${token}\0`);
        const found = [];
        for (const pid of procPids()) {
          let environ;
          try { environ = readFile(`${proc}/${pid}/environ`); } catch (e) { unreadable(pid, e, skipBefore); continue; }
          if (Buffer.concat([environ, Buffer.from("\0")]).includes(needle)) found.push(pid);
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
     * macOS only (null elsewhere: Linux reads /proc/<pid>/environ under the unreadable rule, Windows uses job containment). The
     * live same-user processes the token net cannot judge because `ps eww` shows no environment for them (hidden: an Apple
     * platform binary, or a truly empty environment), plus any `lsof` cannot inspect, classified by `classifyHiddenProcesses`
     * with this gate's anchor. `{ owner, anchorMs, recorded }` as there. Returns `[{ pid, why }]` (held), or throws: a listing
     * that did not finish cleanly is unknown, and the caller stays busy.
     */
    hiddenHolders({ owner, anchorMs, recorded = [] } = {}) {
      if (platform !== "darwin") return null;
      // Without this process's own user id nothing could be told from another user's (which is skipped): unknown, never "none".
      if (!Number.isInteger(uid)) throw new Error("this process's user id is unknown");
      const C = { LC_ALL: "C", LANG: "C" };
      const own = new Set();
      const listing = (what, r) => { if (Number.isInteger(r.pid)) own.add(r.pid); if (r.error || r.status !== 0) throw new Error(`${what}: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}`); return String(r.stdout ?? ""); };
      const table = listing("ps", run(spawn, "ps", ["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "uid=", "-o", "stat=", "-o", "lstart=", "-o", "command="], C));
      const withEnv = listing("ps eww", run(spawn, "ps", ["eww", "-A", "-o", "pid=", "-o", "command="], C));
      const lsofOut = listing("lsof", run(spawn, "lsof", ["-n", "-P", "-a", "-u", String(uid), "-Fp"]));
      const procs = [];
      for (const line of table.split(/\r?\n/)) {
        const m = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(\w{3} \w{3}\s+\d+ \d\d:\d\d:\d\d \d{4})\s+(.*)$/.exec(line);
        if (m) procs.push({ pid: Number(m[1]), ppid: Number(m[2]), group: Number(m[3]), uid: Number(m[4]), state: m[5], startedMs: Date.parse(m[6].replace(/\s+/g, " ")), command: m[7].trim() });
      }
      if (!procs.length) throw new Error("ps listed no process");
      // This gate's own chain (itself and its ancestors) and the listings it just ran are never another owner's children.
      const byPid = new Map(procs.map((p) => [p.pid, p]));
      for (let pid = process.pid; pid > 1 && !own.has(pid); pid = byPid.get(pid)?.ppid ?? 0) own.add(pid);
      const env = new Map();
      for (const line of withEnv.split(/\r?\n/)) { const m = /^\s*(\d+)\s+(.*)$/.exec(line); if (m) env.set(Number(m[1]), m[2].trim()); }
      const inspected = new Set(lsofOut.split(/\r?\n/).filter((l) => l.startsWith("p")).map((l) => Number(l.slice(1))));
      for (const p of procs) {
        // A process shows its environment when its `ps eww` line is its command line followed by more; anything else (no line, the
        // same line, another command line) is hidden. An empty environment prints no more than the command, so it is hidden too.
        const shown = env.get(p.pid);
        p.hidden = !(shown !== undefined && shown.length > p.command.length && shown.startsWith(`${p.command} `));
        p.uninspected = !inspected.has(p.pid);
      }
      // `launchctl blame <pid>` (LC_ALL=C, 5 s): exit 0 and a reason is a launchd job; exit 3 and the "not managed" line is a process
      // launchd does not manage (a reparented orphan) or one that is gone; anything else is unknown (rc null: it could not run).
      const blame = (pid) => {
        const r = spawn("launchctl", ["blame", String(pid)], { encoding: "utf8", env: { ...process.env, LC_ALL: "C", LANG: "C" }, timeout: 5000, maxBuffer: 1 << 16 });
        if (r.error) return { rc: null, line: String(r.error.message ?? r.error) };
        const text = `${String(r.stdout ?? "")}${String(r.stderr ?? "")}`.split(/\r?\n/).find((l) => l.trim() !== "") ?? "";
        return { rc: r.status, line: text.trim() };
      };
      // Re-lists pids after the blame calls (the binding against pid reuse): `Map(pid -> { ppid, startedMs })`; a pid that is gone is absent.
      const reread = (pids) => {
        const r = run(spawn, "ps", ["-o", "pid=", "-o", "ppid=", "-o", "lstart=", "-p", pids.join(",")], C);
        if (r.error || (r.status !== 0 && String(r.stdout ?? "").trim() !== "")) throw new Error(`ps -p: ${r.error?.message ?? `exit ${r.status}`}`);
        const out = new Map();
        for (const line of String(r.stdout ?? "").split(/\r?\n/)) {
          const m = /^\s*(\d+)\s+(\d+)\s+(\w{3} \w{3}\s+\d+ \d\d:\d\d:\d\d \d{4})\s*$/.exec(line);
          if (m) out.set(Number(m[1]), { ppid: Number(m[2]), startedMs: Date.parse(m[3].replace(/\s+/g, " ")) });
        }
        return out;
      };
      return classifyHiddenProcesses({ procs, own, uid, owner, recorded, anchorMs, alive: exists, blame, reread });
    },
    /**
     * Processes using the snapshot `dir` in a way a new session, a scrubbed environment and a working directory elsewhere do not
     * hide, as `[{ pid, how }]` (this gate's own chain left out): its working or root directory, its executable, an open file
     * descriptor or a mapped file inside `dir`, or an argument naming `dir`. Linux: /proc/<pid>/{cwd,root,exe,fd,maps,cmdline}
     * (an unreadable entry of this user's throws: unknown). macOS: `lsof` for this user (cwd, txt, mem and every descriptor) and
     * `ps -ww` arguments. null where neither exists.
     */
    snapshotUsers(dir, skipBefore) {
      // Only POSIX hosts have these nets; the paths they see are POSIX paths on every host (injected tables run on Windows too).
      const posix = path.posix;
      const roots = [posix.resolve(dir)];
      try { const real = fs.realpathSync(dir); if (!roots.includes(real)) roots.push(real); } catch { /* the directory is gone: only its recorded spelling can be named */ }
      // Outside is exactly `..`, `../x` or an absolute relative path: a directory named `..x` inside the snapshot is inside it.
      const inside = (p) => posix.isAbsolute(p) && roots.some((root) => { const rel = posix.relative(root, p); return !(rel === ".." || rel.startsWith("../") || posix.isAbsolute(rel)); });
      const named = (text) => roots.some((root) => namesDir(text, root));
      if (!procAvailable() && platform !== "darwin") return null;
      const { own } = psList();
      const found = []; const seen = new Set();
      const add = (pid, how) => { if (own.has(pid) || seen.has(pid)) return; seen.add(pid); found.push({ pid, how }); };
      if (procAvailable()) {
        // /proc/<pid>/fd and maps list the files a process holds open or mapped right now; cmdline survives its other changes.
        const targetOf = (file) => { try { return readlink(file); } catch (e) { if (GONE.has(e.code)) return null; throw e; } };
        for (const pid of procPids()) {
          if (own.has(pid)) continue;
          try {
            for (const [link, how] of [["cwd", "working directory"], ["root", "root directory"], ["exe", "executable"]]) {
              const target = targetOf(`${proc}/${pid}/${link}`);
              if (target !== null && inside(target)) { add(pid, how); break; }
            }
            if (!seen.has(pid)) {
              let fds = []; try { fds = readdir(`${proc}/${pid}/fd`); } catch (e) { if (!GONE.has(e.code)) throw e; }
              for (const fd of fds) { const target = targetOf(`${proc}/${pid}/fd/${fd}`); if (target !== null && inside(target)) { add(pid, `open file ${target}`); break; } }
            }
            if (!seen.has(pid)) {
              let maps = ""; try { maps = String(readFile(`${proc}/${pid}/maps`, "utf8")); } catch (e) { if (!GONE.has(e.code)) throw e; }
              for (const line of maps.split("\n")) { const m = /^\S+\s+\S+\s+\S+\s+\S+\s+\d+\s+(\/.*?)(?: \(deleted\))?$/.exec(line); if (m && inside(m[1])) { add(pid, `mapped file ${m[1]}`); break; } }
            }
            if (!seen.has(pid)) {
              let cmdline = ""; try { cmdline = String(readFile(`${proc}/${pid}/cmdline`, "utf8")); } catch (e) { if (!GONE.has(e.code)) throw e; }
              if (named(cmdline)) add(pid, "argument");
            }
          } catch (e) { unreadable(pid, e, skipBefore); }
        }
        return found;
      }
      const r = run(spawn, "lsof", ["-n", "-P", "-a", "-u", String(uid), "-Fpfn"]);
      // Only a clean exit is a complete listing: any other status (even with output) is unknown, so the caller stays busy.
      if (r.error || r.status !== 0) throw new Error(`lsof: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}`);
      if (Number.isInteger(r.pid)) own.add(r.pid);
      let pid = null; let fd = "";
      for (const line of String(r.stdout ?? "").split(/\r?\n/)) {
        if (line.startsWith("p")) { pid = Number(line.slice(1)); fd = ""; }
        else if (line.startsWith("f")) fd = line.slice(1);
        else if (line.startsWith("n") && pid && inside(line.slice(1))) add(pid, fd === "cwd" ? "working directory" : fd === "rtd" ? "root directory" : fd === "txt" ? "executable or text file" : fd === "mem" ? `mapped file ${line.slice(1)}` : `open file ${line.slice(1)}`);
      }
      const a = run(spawn, "ps", ["-A", "-ww", "-o", "pid=", "-o", "command="], { LC_ALL: "C", LANG: "C" });
      if (a.error || a.status !== 0) throw new Error(`ps arguments: ${a.error?.message ?? (String(a.stderr ?? "").trim() || `exit ${a.status}`)}`);
      if (Number.isInteger(a.pid)) own.add(a.pid);
      for (const line of String(a.stdout ?? "").split(/\r?\n/)) { const m = /^\s*(\d+)\s(.*)$/.exec(line); if (m && named(m[2])) add(Number(m[1]), "argument"); }
      return found;
    },
    /** This gate's ancestors, nearest first, as `[{ pid, startedMs }]` (pid 1 and the gate itself left out). */
    ancestors() {
      const { byPid } = psList();
      const chain = [];
      for (let row = byPid.get(process.pid); row && row.ppid > 1 && chain.length < 64; ) {
        row = byPid.get(row.ppid);
        if (row) chain.push({ pid: row.pid, startedMs: Number.isFinite(row.startedMs) ? row.startedMs : null });
      }
      return chain;
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
  const dir = childrenDir(merged[OWNERSHIP_FILE_ENV] || ownershipFile({ platform }), handle);
  try { fs.mkdirSync(dir, { mode: 0o700 }); } catch (e) { if (e.code !== "EEXIST") throw e; } // the ownership directory exists while this gate holds it
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

export const CLEAN_SCHEMA = "rightkit.build-ownership-clean/1";
/** Beside the owner record: the acquisition time of the last owner whose release proved zero readers. */
export const cleanMarkerFile = (file) => path.join(path.dirname(file), "last-clean-release.json");
/** The marker's acquisition time (ISO), or null when absent, unreadable or malformed (no clean release is then known). */
export function readCleanMarker(file) {
  try {
    const m = JSON.parse(fs.readFileSync(cleanMarkerFile(file), "utf8"));
    return m?.schema === CLEAN_SCHEMA && Number.isFinite(Date.parse(m.acquiredAt)) ? m.acquiredAt : null;
  } catch { return null; }
}
function writeCleanMarker(file, owner) {
  const target = cleanMarkerFile(file); const tmp = `${target}.${crypto.randomUUID()}.tmp`;
  try {
    fs.writeFileSync(tmp, JSON.stringify({ schema: CLEAN_SCHEMA, acquiredAt: owner.acquiredAt, releasedAt: new Date().toISOString(), handle: owner.handle }), { flag: "wx", mode: 0o600 });
    fs.renameSync(tmp, target);
  } catch { /* no marker: nothing is skipped, which only holds more */ } finally { fs.rmSync(tmp, { force: true }); }
}

export const ROOT_BIRTH_SCHEMA = "rightkit.snapshot-root-birth/1";
/** Inside a snapshot root: when this root came into being, and in which boot of the host. */
export const rootBirthFile = (root) => path.join(root, ".rightkit-root-birth.json");
/** This boot's identity: Linux boot_id, macOS kern.bootsessionuuid; null where it cannot be read (Windows, a failure): no baseline then. */
export function hostBootId({ platform = process.platform, readFile = fs.readFileSync, spawn = spawnSync, bootIdFile = "/proc/sys/kernel/random/boot_id" } = {}) {
  try {
    if (platform === "linux") { const id = String(readFile(bootIdFile, "utf8")).trim(); return /^[0-9a-f-]{16,}$/i.test(id) ? id : null; }
    if (platform === "darwin") {
      // kern.bootsessionuuid: one UUID per boot, stable for the whole boot (kern.boottime moves when the wall clock is stepped).
      const r = run(spawn, "sysctl", ["-n", "kern.bootsessionuuid"]);
      const id = String(r.stdout ?? "").trim();
      return !r.error && r.status === 0 && /^[0-9a-f-]{16,}$/i.test(id) ? id : null;
    }
  } catch { /* unknown */ }
  return null;
}
/**
 * Writes the birth record of a snapshot root that was just created (never over an existing one). Best effort: without it the
 * root has no baseline and build ownership applies its strict rule. Returns whether it wrote.
 */
export function recordRootBirth(root, { now = Date.now, bootId = hostBootId() } = {}) {
  try {
    fs.writeFileSync(rootBirthFile(root), JSON.stringify({ schema: ROOT_BIRTH_SCHEMA, createdAt: new Date(now()).toISOString(), bootId }), { flag: "wx", mode: 0o600 });
    return true;
  } catch { return false; }
}
/** When `root` was born (epoch ms), or null when there is no readable birth record of THIS boot (missing, malformed, another boot). */
export function readRootBirth(root, bootId) {
  if (typeof bootId !== "string" || !bootId) return null;
  try {
    const b = JSON.parse(fs.readFileSync(rootBirthFile(root), "utf8"));
    const at = Date.parse(b?.createdAt);
    return b?.schema === ROOT_BIRTH_SCHEMA && b.bootId === bootId && Number.isFinite(at) ? at : null;
  } catch { return null; }
}

/**
 * Migration for a snapshot root with no usable baseline: it predates birth records (an older gate made it), or its birth record
 * names another boot (a reboot happened since). On Linux, one non-dumpable agent of this user (ssh-agent) would otherwise hold
 * every release. A root is a cache, so under THIS process's own ownership, taken on a free host (no owner record existed; never a
 * reclaim, never a nested gate), before this sync's first write, its snapshots are deleted and the root is born anew, with this
 * boot's identity. Why that is sound: after a reboot no process of the old boot is alive, so the old root has no live readers; a
 * free host proves there is no current owner; and the fresh birth makes the strict rule apply only to processes that started after
 * it. Refused (the root stays as it was, strict) when: this process does not hold the only record, or the record is not a free-host
 * acquisition; this boot's identity cannot be read; the root is missing or is a link (a missing root is born by the sync); its birth
 * record is unreadable, or names THIS boot; or anything in the root is not a snapshot artifact (`<16 hex>`, `<16 hex>.lock`,
 * `.state.json`, `.staging`, or the birth record), because a root that is some other directory is never emptied.
 * Returns `{ migrated, why }`.
 */
export function migrateBirthlessRoot(root, { env = process.env, file = env[OWNERSHIP_FILE_ENV], bootId = hostBootId() } = {}) {
  const no = (why) => ({ migrated: false, why });
  const handle = env[OWNERSHIP_ENV];
  if (!handle || !file) return no("this process holds no build ownership");
  let record = null; try { record = JSON.parse(fs.readFileSync(file, "utf8")); } catch { record = null; }
  if (record?.handle !== handle || record.pid !== process.pid) return no("this process does not hold the owner record");
  if (record.freeHost !== true) return no("the ownership was not taken on a free host");
  if (typeof bootId !== "string" || !bootId) return no("this boot's identity cannot be read");
  let st = null; try { st = fs.lstatSync(root); } catch { st = null; }
  if (!st?.isDirectory()) return no("the root is missing or is not a plain directory");
  let reason = "the root had no birth record";
  if (fs.existsSync(rootBirthFile(root))) {
    let birth = null; try { birth = JSON.parse(fs.readFileSync(rootBirthFile(root), "utf8")); } catch { birth = null; }
    if (birth?.schema !== ROOT_BIRTH_SCHEMA) return no("the root's birth record cannot be read");
    if (birth.bootId === bootId) return no("the root already has a birth record of this boot");
    reason = `the root's birth record names another boot (${JSON.stringify(birth.bootId)}, not ${JSON.stringify(bootId)})`;
  }
  let entries; try { entries = fs.readdirSync(root); } catch (e) { return no(`the root cannot be listed (${e.code ?? e.message})`); }
  const birthName = path.basename(rootBirthFile(root));
  const stranger = entries.find((n) => n !== birthName && !/^[0-9a-f]{16}(\.[A-Za-z0-9._-]+)?$/.test(n));
  if (stranger !== undefined) return no(`${JSON.stringify(stranger)} in the root is not a snapshot artifact`);
  try { for (const n of entries) fs.rmSync(path.join(root, n), { recursive: true, force: true }); }
  catch (e) { return no(`the root could not be emptied (${e.code ?? e.message}); it stays without a baseline`); }
  return recordRootBirth(root, { bootId }) ? { migrated: true, why: `${reason}; its snapshots were deleted and it was born anew in this boot` } : no("the new birth could not be recorded");
}

/** The holder's last report, or null when absent or unreadable. */
export function readHolder(file) {
  try { const h = JSON.parse(fs.readFileSync(file, "utf8")); return h?.schema === HOLDER_SCHEMA ? h : null; } catch { return null; }
}

/**
 * RustRight's lease state for a snapshot & ownership handle (`rustright snapshot-lease status`). Anything but a well-formed
 * answer is UNKNOWN: a missing client, a failed or unknown command, a schema or state the gate does not know.
 */
export function brokerLeaseState({ snapshot, handle, env = process.env, platform = process.platform, spawn = spawnSync, client = managedRuntimeClient(env, platform), waitMs = 0 }) {
  if (!client) return { state: "UNKNOWN", why: "no RustRight client on this host" };
  // GB-85: `--wait-ms N` makes the broker answer as soon as every submission under the handle is durably complete (REAPED), or at the bound with the state it has
  // (waitTimedOut: true). A client that predates the option answers with a usage error: the plain query is then made, never a guess.
  const ask = (wait) => run(spawn, client, ["snapshot-lease", "status", "--snapshot", snapshot, "--ownership", handle, ...(wait > 0 ? ["--wait-ms", String(Math.min(Math.trunc(wait), 30_000))] : [])], env);
  let r = ask(waitMs);
  if (waitMs > 0 && r.status !== 0 && !r.error && /wait-ms|unknown|unexpected|unrecognized|usage/i.test(String(r.stderr ?? ""))) r = ask(0);
  if (r.error || r.status !== 0) return { state: "UNKNOWN", why: `rustright snapshot-lease status: ${r.error?.message ?? (String(r.stderr ?? "").trim().split(/\r?\n/).pop() || `exit ${r.status}`)}` };
  let answer = null;
  try { answer = JSON.parse(String(r.stdout ?? "").trim().split(/\r?\n/).pop()); } catch { answer = null; }
  if (answer?.schema !== LEASE_STATUS_SCHEMA || !LEASE_STATES.has(answer.state)) return { state: "UNKNOWN", why: `unrecognised lease status ${JSON.stringify(String(r.stdout ?? "").trim().slice(0, 200))}` };
  // The answer must echo what was asked (RustRight echoes `snapshot` as received and `ownership` as the handle): another pair's answer,
  // or no echo at all, binds nothing to this owner, so it is UNKNOWN and the record is held.
  if (answer.snapshot !== snapshot || answer.ownership !== handle) return { state: "UNKNOWN", why: `the lease status does not echo the queried snapshot and ownership handle (${JSON.stringify(answer.snapshot ?? null)}, ${JSON.stringify(answer.ownership ?? null)})` };
  // GB-23: `members` is one row per submission the broker knows under the handle ({handle, state, spawned, refused?, job?}); the answer's own state is only the aggregate.
  let members;
  if (answer.members !== undefined) {
    const covers = (h) => h === handle || String(h).startsWith(`${handle}#`);
    if (!Array.isArray(answer.members) || answer.members.some((m) => !m || typeof m.handle !== "string" || !covers(m.handle) || !LEASE_STATES.has(m.state))) return { state: "UNKNOWN", why: `the lease status carries members that are not submissions under ${handle}` };
    members = answer.members;
  }
  if (answer.waitTimedOut === true) return { state: "UNKNOWN", why: `rustright snapshot-lease status: still ${answer.state} after waiting ${answer.waitedMs ?? waitMs} ms (waitTimedOut); ownership stays UNKNOWN`, members, waitTimedOut: true };
  return { state: answer.state, why: `rustright snapshot-lease status: ${answer.state}`, ...(members ? { members } : {}) };
}

function publish(target, text) {
  if (process.platform === "win32") {
    // Windows: no second name. Linking a temp file and deleting its name left owner.json a file that could not be replaced for
    // seconds (measured on the Dell, rename EPERM with no open handle on it; a plain exclusive create has no such state). The
    // record is created exclusively and written at once; a reader that catches it half-written cannot parse it and stays busy
    // (fail closed), and a failed write removes it.
    let fd;
    try { fd = fs.openSync(target, "wx", 0o600); } catch (e) { if (e.code === "EEXIST") return false; throw e; }
    try { fs.writeSync(fd, text); } catch (e) { try { fs.closeSync(fd); } catch { /* closed below */ } fs.rmSync(target, { force: true }); throw e; }
    fs.closeSync(fd);
    return true;
  }
  const tmp = `${target}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, text, { flag: "wx", mode: 0o600 });
  try { fs.linkSync(tmp, target); return true; } catch (e) { if (e.code === "EEXIST") return false; throw e; } finally { fs.rmSync(tmp, { force: true }); }
}
const readText = (file) => { try { return fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

/**
 * Whether group member `m` is work the owner covers. Ancestors and peers share a group without being the gate's children (the
 * shell that started it, `| tee`, a later gate started by the same script), so a member counts only when it started after the
 * ownership began AND its ancestry does not end at a proven peer. The walk covers the member at the owner, at pid 1 (an orphan),
 * at an unknown parent, and at ANY process that predates the ownership except a peer parent: an orphan adopted by a subreaper
 * (systemd --user, WSL's init) has that subreaper as its parent now, and a subreaper is an ancestor of the gate as often as not,
 * so "a pre-existing parent" proves nothing about being the gate's work. A peer parent is the owner's recorded nearest
 * ancestor (the shell that ran `gate | tee`) or a process that shares the member's own process group (a script shell without job
 * control); a descendant of the asking gate is its own. Everything else is covered: a peer under another parent is held only
 * until it exits, never cleared by guesswork.
 */
function coveredMember(m, owner, { byPid, own }) {
  const since = Date.parse(owner.acquiredAt) - START_SLACK_MS;
  const before = (p) => !Number.isFinite(p.startedMs) || p.startedMs < since;
  if (own.has(m.pid) || before(m)) return false;
  // The owner's own ancestors (recorded at acquisition) are never its children, however recently they started.
  if (Array.isArray(owner.ancestors) && owner.ancestors.some((a) => a?.pid === m.pid && sameStart(a.startedMs, m.startedMs))) return false;
  const parent = Array.isArray(owner.ancestors) ? owner.ancestors[0] : null;
  const peerParent = (p) => (Number.isInteger(p.group) && p.group === m.group) || (!!parent && p.pid === parent.pid && sameStart(p.startedMs, parent.startedMs));
  const seen = new Set([m.pid]);
  for (let p = byPid.get(m.ppid); p; p = byPid.get(p.ppid)) {
    if (p.pid === owner.pid && sameStart(p.startedMs, owner.startedMs)) return true;
    if (p.pid <= 1 || seen.has(p.pid)) return true;
    seen.add(p.pid);
    if (p.pid === process.pid) return false;
    if (before(p) || own.has(p.pid)) return !peerParent(p);
  }
  return true;
}

/**
 * Whether the work `owner` covers, other than the process `self` (the live gate at release, null at reclaim), is proven gone.
 * Returns the proofs, or throws a reason that is busy.
 */
function coveredWorkGone(owner, { probe, lease, file, selfPid = null, waitMs = 0 }) {
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
    const recorded = [];
    for (const name of groups) {
      let entry;
      try { entry = JSON.parse(fs.readFileSync(path.join(registry, name), "utf8")); } catch { throw new Error(`the detached-child entry ${name} cannot be read`); }
      if (!Number.isInteger(entry?.group) || !Number.isFinite(entry?.startedMs)) throw new Error(`the detached-child entry ${name} cannot be read`);
      recorded.push({ group: entry.group, command: entry.command });
      let snapshot;
      try { snapshot = probe.groupSnapshot(entry.group); } catch (e) { throw new Error(`detached process group ${entry.group} cannot be listed (${e.message})`); }
      const live = snapshot.members.filter((m) => m.pid !== selfPid && m.startedMs >= entry.startedMs - START_SLACK_MS).map((m) => m.pid);
      if (live.length) throw new Error(`detached process group ${entry.group} (${entry.command}) still has ${live.length} member(s): ${live.join(", ")}`);
    }
    if (groups.length) proofs.push(`the ${groups.length} detached process group(s) it started are empty`);
    // See `unreadable` in processProbe. The baseline is the later of (1) the acquisition of the last owner whose release proved zero
    // readers and (2) the snapshot root's birth in this boot, capped at this ownership's own acquisition (a process the gate
    // may have started is never baseline). NaN (neither) skips nothing.
    let birth = NaN;
    if (owner.snapshot) {
      try { const b = readRootBirth(path.dirname(owner.snapshot), probe.bootId?.()); if (b !== null) birth = Math.min(b, Date.parse(owner.acquiredAt)); } catch { birth = NaN; }
    }
    const baselines = [Date.parse(owner.cleanBefore ?? ""), birth].filter(Number.isFinite);
    // With no baseline (no clean previous release, no birth of the root in this boot, an unreadable boot id) the anchor is this record's
    // own acquisition: nothing that started before this ownership began is the unproven work of any owner (every record is published
    // on a free host after a release that proved zero readers, or by a reclaim that proved the previous work gone). Never "no anchor".
    const skipBefore = baselines.length ? Math.max(...baselines) : Date.parse(owner.acquiredAt);
    let carriers;
    try { carriers = probe.tokenHolders(owner.handle, skipBefore); } catch (e) { throw new Error(`processes carrying ${OWNERSHIP_ENV} cannot be listed (${e.message})`); }
    if (carriers) {
      const others = carriers.filter((pid) => pid !== selfPid);
      if (others.length) throw new Error(`process(es) ${others.join(", ")} still carry ${OWNERSHIP_ENV}=${owner.handle}`);
      proofs.push(`no process carries ${OWNERSHIP_ENV}=${owner.handle}`);
    }
    if (owner.snapshot) {
      let users;
      try { users = probe.snapshotUsers(owner.snapshot, skipBefore); } catch (e) { throw new Error(`processes using the snapshot cannot be listed (${e.message})`); }
      if (users) {
        const others = users.filter((u) => u.pid !== selfPid);
        if (others.length) throw new Error(`process(es) ${others.map((u) => `${u.pid} (${u.how})`).join(", ")} still use the snapshot ${owner.snapshot}`);
        proofs.push(`no process has a working directory, executable, open or mapped file or argument inside or naming ${owner.snapshot}`);
      }
    }
    // macOS: a process whose environment `ps eww` does not show cannot be attributed to this ownership by the token net. Start time plus a
    // hidden environment is no attribution (rightkit#1313: an unrelated same-user process started during the run held every later gate), so
    // such a process is REPORTED here and never held. Only lineage the gate recorded (tracked child groups and intents above), the exact
    // ownership token, an actual snapshot user, and the broker lease hold a release.
    if (typeof probe.hiddenHolders === "function") {
      let hidden;
      try { hidden = probe.hiddenHolders({ owner, anchorMs: skipBefore, recorded }); } catch { hidden = null; }
      const others = (hidden ?? []).filter((h) => h.pid !== selfPid);
      if (others.length) {
        const shown = others.slice(0, 8);
        proofs.push(`reported, not held: ${others.length} unattributed process(es) of this user with a hidden environment started after the anchor (no recorded lineage, token or snapshot use): ${shown.map((h) => `${h.pid}: ${h.why}`).join("; ")}${others.length > shown.length ? `; and ${others.length - shown.length} more (pids ${others.slice(shown.length).map((h) => h.pid).join(", ")})` : ""}`);
      }
    }
  } else if (!rebooted) {
    throw new Error(`the record names no containment (${JSON.stringify(c)})`);
  }
  if (owner.broker?.submitted) {
    // Every submission under this handle must be accounted for by NAME, never by the bare handle (GB-21, GB-23, GB-85). The bare handle answers REAPED when at least one
    // member completed and none is held (rustright snapshot.rs `status`), so it proves only that no lease is held now: a completed sibling never stands in for an
    // unknown submission. Each recorded sub-handle (broker.handles, recorded before its send) is asked by its exact name and must be one of
    //   - REAPED, as a member of that answer (a durable completion; `spawned: false` for a refusal that ran nothing), or
    //   - UNSENT: the gate itself proved, under the record's lock, that its client never started (broker.unsent), AND the broker holds nothing under it (UNKNOWN, no members).
    // An UNKNOWN or ACTIVE one, an ambiguous send, a start that may have run, keeps the record; nothing is counted complete on the absence of a completion.
    const handles = [...new Set(Array.isArray(owner.broker.handles) ? owner.broker.handles : [])];
    const unsent = owner.broker.unsent && typeof owner.broker.unsent === "object" ? owner.broker.unsent : {};
    const waitOptions = waitMs > 0 ? { waitMs } : {};
    const settled = (sub) => unsent[sub] && typeof unsent[sub].reason === "string";
    const everyUnsent = handles.length > 0 && handles.every(settled);
    const status = lease({ snapshot: owner.snapshot, handle: owner.handle, ...waitOptions });
    if (status.state === "ACTIVE") throw new Error(`broker work is still active under this handle (${status.why})`);
    if (status.state !== "REAPED" && !everyUnsent) throw new Error(`broker work may exist under this handle and its lease is ${status.state} (${status.why})`);
    if (status.state === "REAPED") proofs.push(`broker lease for ${owner.handle} is REAPED`);
    for (const sub of handles) {
      if (typeof sub !== "string" || !sub.startsWith(`${owner.handle}#`) || sub.length <= owner.handle.length + 1) throw new Error(`the record lists ${JSON.stringify(sub)}, which is not a sub-handle of ${owner.handle}`);
      const one = lease({ snapshot: owner.snapshot, handle: sub, ...waitOptions });
      const member = Array.isArray(one.members) ? one.members.find((m) => m.handle === sub) : null;
      if (one.state === "REAPED") {
        if (Array.isArray(one.members) && !(member && member.state === "REAPED")) throw new Error(`the broker answers REAPED for ${sub} but lists no completed submission under that name`);
        continue;
      }
      if (settled(sub) && one.state === "UNKNOWN" && Array.isArray(one.members) && one.members.length === 0) { proofs.push(`${sub} never started (${unsent[sub].reason}) and the broker holds nothing under it`); continue; }
      throw new Error(`broker work may exist under sub-handle ${sub} and its lease is ${one.state} (${one.why})`);
    }
    if (handles.length) proofs.push(`every one of ${handles.length} sub-handle(s) is accounted for: REAPED, or proven never started`);
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
 * `{ handle, file, joined, reclaimed, owner, markBrokerSubmit(callHandle), held(), release() }`. Throws OwnershipBusy while another build
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
  managedRoot,
  device,
} = {}) {
  // First, before the environment, the record or any write: the managed root exists (decision D4). `managedRoot` and `device` are
  // fixtures' seams; a production caller passes neither.
  guardManagedRoot(file, { platform, managedRoot, device });
  ensureOwnershipDir(path.dirname(file), { platform });
  const inherited = env[OWNERSHIP_ENV];
  if (inherited) {
    const current = readText(file);
    let owner = null; try { owner = JSON.parse(current ?? ""); } catch { owner = null; }
    if (owner?.handle === inherited) {
      // A nested gate joins the LIVE owner's record (row 5.20): the exact handle of a well-formed record of this host whose owner is
      // still running as the process that wrote it. A handle left in the environment of a survivor of a dead owner names a record
      // nobody holds any more; joining it would run a build under an ownership no live gate answers for.
      if (owner.schema !== OWNERSHIP_SCHEMA || !Number.isInteger(owner.pid) || owner.pid <= 0 || owner.host !== os.hostname()) {
        throw new OwnershipBusy(`build ownership ${file}: ${OWNERSHIP_ENV}=${inherited} names a record that is not a well-formed owner record of this host; a nested gate joins only a live owner's record`);
      }
      let live;
      try { live = probe.identity(owner.pid); } catch (e) { throw new OwnershipBusy(`build ownership ${file}: owner pid ${owner.pid} cannot be proven alive (${e.message}), so a nested gate cannot join it`); }
      if (!live.alive || (Number.isFinite(owner.startedMs) && Number.isFinite(live.startedMs) && !sameStart(live.startedMs, owner.startedMs))) {
        throw new OwnershipBusy(`build ownership ${file}: ${OWNERSHIP_ENV}=${inherited} names a record whose owner pid ${owner.pid} is no longer the process that took it; a nested gate joins only a live owner's record`);
      }
      return { handle: inherited, file, joined: true, reclaimed: null, owner, markBrokerSubmit: (callHandle) => markBrokerSubmit(file, inherited, callHandle), markBrokerUnsent: (callHandle, reason) => markBrokerUnsent(file, inherited, callHandle, reason), held: () => holds(file, inherited), release: () => ({ released: false, joined: true }) };
    }
    throw new OwnershipBusy(`build ownership ${file}: ${OWNERSHIP_ENV}=${inherited} is not the current owner; a nested gate never takes ownership of its own`);
  }
  let me;
  try { me = probe.identity(process.pid); } catch (e) { throw new OwnershipBusy(`build ownership ${file}: this gate's own start identity cannot be read (${e.message})`); }
  if (!me.alive || !Number.isFinite(me.startedMs)) throw new OwnershipBusy(`build ownership ${file}: this gate's own start identity cannot be read`);
  if (platform !== "win32" && !(Number.isInteger(me.group) && me.group > 1)) throw new OwnershipBusy(`build ownership ${file}: this gate's process group cannot be read`);
  // The gate's ancestors, so a later gate can tell a peer of the owner (the shell it ran under) from an orphan a subreaper adopted.
  let ancestors = null;
  try { ancestors = probe.ancestors?.() ?? null; } catch { ancestors = null; }
  const handle = crypto.randomUUID();
  const record = {
    schema: OWNERSHIP_SCHEMA,
    handle,
    host: os.hostname(),
    platform,
    pid: process.pid,
    startedMs: me.startedMs,
    ancestors,
    containment: platform === "win32" ? { kind: "job", name: jobName(handle) } : { kind: "group", group: me.group },
    stage: stage ?? null,
    snapshot: snapshot ?? null,
    commit: commit ?? null,
    acquiredAt: new Date().toISOString(),
    // Set only for an acquisition on a free host after a release that proved zero readers (a reclaim never has it).
    cleanBefore: readCleanMarker(file),
    // True when this ownership was published on a free host (no owner record anywhere); a reclaim sets it false.
    freeHost: true,
    broker: { submitted: false },
  };
  const body = JSON.stringify(record);
  const release = () => {
    if (!holds(file, handle)) return { released: false, why: "no longer the owner" };
    let proofs;
    // The record as it is now (a broker submit may have been marked since it was written).
    try { proofs = coveredWorkGone(JSON.parse(readText(file)), { probe, lease, file, selfPid: process.pid, waitMs: RELEASE_LEASE_WAIT_MS }); }
    catch (e) { return { released: false, why: `left for the next gate to reclaim: ${e.message}` }; }
    finally { if (exportEnv && process.env[OWNERSHIP_ENV] === handle) { delete process.env[OWNERSHIP_ENV]; delete process.env[OWNERSHIP_FILE_ENV]; } }
    // Only a release that proved zero readers leaves the marker; a held one leaves none (and keeps the record).
    writeCleanMarker(file, record);
    fs.rmSync(file, { force: true });
    fs.rmSync(markLockFile(file), { force: true });
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
      // Fail closed: a record this gate published but cannot finish is removed, never left held by a gate that is not running.
      try { rewriteRecord(file, handle, (r) => ({ ...r, containment: record.containment })); }
      catch (e) { if (holds(file, handle)) fs.rmSync(file, { force: true }); throw new OwnershipBusy(`build ownership ${file}: the containment could not be recorded (${e.code ?? e.message}: ${e.message})`); }
    }
    if (exportEnv) { process.env[OWNERSHIP_ENV] = handle; process.env[OWNERSHIP_FILE_ENV] = file; }
    return { handle, file, joined: false, reclaimed, owner: record, contained: platform === "win32" ? record.containment.contained === true : true, markBrokerSubmit: (callHandle) => markBrokerSubmit(file, handle, callHandle), markBrokerUnsent: (callHandle, reason) => markBrokerUnsent(file, handle, callHandle, reason), held: () => holds(file, handle), release };
  };
  if (publish(file, body)) return done(null);
  const observed = readText(file);
  if (observed === null) { if (publish(file, body)) return done(null); throw new OwnershipBusy(`build ownership ${file} was taken by another process`); }
  const judged = judgeOwner(observed, { file, probe, lease });
  hooks.afterJudge?.();
  // A reclaim means the previous release was not clean (a crash, or a release that ended held): nothing unreadable is skipped.
  record.cleanBefore = null;
  record.freeHost = false;
  const reclaimBody = JSON.stringify(record);
  const reclaim = `${file}.reclaim`;
  if (!publish(reclaim, reclaimBody)) throw new OwnershipBusy(`build ownership ${file} is being reclaimed by another process (remove ${reclaim} only if no gate is running)`);
  try {
    if (readText(file) !== observed) throw new OwnershipBusy(`build ownership ${file} changed before it could be reclaimed`);
    fs.rmSync(file, { force: true });
    fs.rmSync(markLockFile(file), { force: true });
    fs.rmSync(holderFile(file, judged.owner.handle), { force: true });
    fs.rmSync(holderScriptFile(holderFile(file, judged.owner.handle)), { force: true });
    fs.rmSync(childrenDir(file, judged.owner.handle), { recursive: true, force: true });
    if (!publish(file, reclaimBody)) throw new OwnershipBusy(`build ownership ${file} was taken by another process`);
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


