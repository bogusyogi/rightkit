// The launcher of a gate run judged by the twelve lines (GATE-CONTRACT GC-A27, rightkit#1514 step 3). The acceptance test takes nothing from
// the gate's own reporting (Adrian, 2026-11-21), so this launcher, not the gate, records the run: it owns the selected commit, the run id, the
// clock, the exit, the run lock, the Class A lane count, the time limit pinned before the gate starts, the run kind (derived from the stage the gate
// is asked for, never a label), the snapshot root, and its own record of the previous launched run (the baseline commit, and a fingerprint of the
// gate code, tools and environment that git cannot show). It keeps the gate's output for people. After the gate ends it exports the operating
// system's record of every program that started and exited in the run's window.
//
//   node acceptance-launch.mjs --out <run dir> --commit <sha> --receipts <broker receipts dir> [--kind <stage>] [--limits <file>] [--lanes <n>]
//                              [--repo <dir>] [--gate <bin/rightkit-gate.mjs>] [--lock <file>] -- <gate arguments>
//
// The run folder holds launch.json (this record), before/receipts.txt, processes.json and gate.log. Beside the run folder: acceptance-launch.lock
// (the live run) and acceptance-launch.last.json (the last launched run, kept after it ends, so the next run has its predecessor and baseline even
// when the launcher's lock was released). A live holder refuses the run (exit 3, nothing started). A dead holder is the run before this one, killed:
// this run takes the lock over (one rename, so exactly one successor wins) and names it in `previous`.
import { execFileSync, spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const LAUNCH_SCHEMA = "rightkit.acceptance-launch/3";
const here = path.dirname(fileURLToPath(import.meta.url));
const mono = () => process.hrtime.bigint().toString();
const alive = (pid) => { if (!Number.isInteger(pid) || pid <= 0) return false; try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");

/**
 * Takes the run lock. Returns { previous } (the dead holder this run took over, or null) or { refused } when a live run holds it or another launcher
 * claimed the same dead holder first. Takeover is one exclusive create (open with wx, which Windows maps to CREATE_NEW) of a claim file named by the dead
 * holder's own identity (its run id, or its lock file's write time when it cannot be read): of any number of launchers that saw the same dead holder
 * exactly one creates the claim, the others get EEXIST and do not start. A launcher that saw the holder after it was replaced sees a live holder, or a
 * different identity, and so claims a different file. Nothing is deleted by a contender but the dead holder's own lock, moved aside.
 */
export function takeLaunchLock(file, holder, { isAlive = alive, fsApi = fs } = {}) {
  fsApi.mkdirSync(path.dirname(file), { recursive: true });
  const create = () => { try { fsApi.writeFileSync(file, `${JSON.stringify(holder)}
`, { flag: "wx" }); return true; } catch (e) { if (e.code !== "EEXIST") throw e; return false; } };
  if (create()) return { previous: null };
  let held = null; try { held = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { held = null; }
  let written = 0; try { written = Math.trunc(fsApi.statSync(file).mtimeMs * 1000); } catch { written = 0; }
  if (held && isAlive(held.pid)) return { refused: `run ${held.runId} (pid ${held.pid}) holds ${file}` };
  const identity = `${held?.runId ?? "unreadable"}.${held?.pid ?? 0}.${written}`.replace(/[^\w.-]/g, "_");
  const claim = `${file}.successor.${identity}`;
  try { fsApi.writeFileSync(claim, `${JSON.stringify({ runId: holder.runId, pid: holder.pid })}
`, { flag: "wx" }); }
  catch (e) { if (e.code === "EEXIST") return { refused: `another launcher already claimed the successor of ${held?.runId ?? "the unreadable holder"} (${path.basename(claim)})` }; throw e; }
  // This launcher alone holds the claim for that dead holder. The holder is read again: a live run at the path now is not the one that was claimed.
  let now = null; try { now = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { now = null; }
  if (now && isAlive(now.pid) && now.runId !== held?.runId) return { refused: `run ${now.runId} (pid ${now.pid}) holds ${file}` };
  const aside = `${file}.dead.${identity}`;
  try { fsApi.renameSync(file, aside); } catch (e) { if (e.code === "ENOENT") return { refused: `${file} was taken over before this launcher moved it` }; throw e; }
  if (!create()) return { refused: `another launcher created ${file} after the dead holder was moved aside` };
  return { previous: held ? { runId: held.runId ?? null, out: held.out ?? null, pid: held.pid ?? null, reclaimed: true } : { runId: null, out: null, pid: null, reclaimed: true, unreadable: true } };
}

/**
 * What git cannot show about a run, recorded before it starts: the gate code (the tree id of packages/release at the selected commit), the node and
 * platform, and the version output of the tools the gate runs. Two runs are the same kind of baseline only when every field is present and equal.
 */
export function fingerprintOf({ repo, commit, run = execFileSync, platform = process.platform }) {
  const probe = (cmd, args) => { try { return String(run(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], windowsHide: true, timeout: 20_000 })).trim().split(/\r?\n/)[0]; } catch { return null; } };
  const gateCodeTree = probe("git", ["-C", repo, "rev-parse", `${commit}:packages/release`]);
  const tools = Object.fromEntries([["rustfmt", ["--version"]], ["cargo", ["--version"]], ["cargo-deny", ["--version"]], ["pnpm", ["--version"]], ["git", ["--version"]]].map(([c, a]) => [c, probe(c, a)]));
  return { gateCodeTree, node: process.version, platform, tools };
}
const complete = (f) => !!f && f.gateCodeTree !== null && Object.values(f.tools ?? {}).every((v) => typeof v === "string") && typeof f.node === "string";
export const sameFingerprint = (a, b) => complete(a) && complete(b) && JSON.stringify(a) === JSON.stringify(b);

/**
 * The operating system's record of the programs that started and exited in [from, to]: Windows Security events 4688 and 4689, read with
 * Get-WinEvent (Event Log Readers is enough). The record says how far the log had advanced (its newest event) and whether it reaches back to the
 * run's start (its oldest), and every event it keeps has a process id and a time.
 */
export function exportWindowsProcesses(from, to, { run = execFileSync } = {}) {
  const ps = `$ErrorActionPreference='Stop'; $from=[datetime]::Parse('${from}').ToLocalTime(); $to=[datetime]::Parse('${to}').ToLocalTime();
$oldest=(Get-WinEvent -LogName Security -MaxEvents 1 -Oldest).TimeCreated.ToUniversalTime().ToString('o');
$newest=(Get-WinEvent -LogName Security -MaxEvents 1).TimeCreated.ToUniversalTime().ToString('o');
$all=@(Get-WinEvent -FilterHashtable @{LogName='Security'; StartTime=$from; EndTime=$to} -ErrorAction SilentlyContinue);
$ids=@($all | ForEach-Object { $_.RecordId } | Sort-Object);
$continuous=($ids.Count -le 1) -or (($ids[-1] - $ids[0] + 1) -eq $ids.Count);
$ev=@($all | Where-Object { $_.Id -eq 4688 -or $_.Id -eq 4689 } | ForEach-Object { $x=[xml]$_.ToXml(); $d=@{}; foreach($n in $x.Event.EventData.Data){ $d[$n.Name]=$n.'#text' }; [pscustomobject]@{ id=$_.Id; time=$_.TimeCreated.ToUniversalTime().ToString('o'); data=$d } });
[pscustomobject]@{ oldest=$oldest; newest=$newest; continuous=$continuous; records=$ids.Count; events=$ev } | ConvertTo-Json -Depth 5 -Compress`;
  const problems = []; let raw;
  try { raw = JSON.parse(run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", ps], { encoding: "utf8", windowsHide: true, maxBuffer: 512 * 1024 * 1024 })); }
  catch (e) { return { kind: "windows-security", from, to, newest: null, continuous: false, events: [], problems: [`the Security log could not be read (${String(e.stderr ?? e.message).trim().split(/\r?\n/)[0]})`] }; }
  if (!(Date.parse(raw?.oldest) <= Date.parse(from))) problems.push(`the Security log's oldest event (${raw?.oldest}) is newer than the run's start, so events may have been overwritten`);
  if (raw?.continuous !== true) problems.push(`the Security log's record ids have a break inside the run's window (${raw?.records ?? "?"} records), so a start or exit may have been lost`);
  if (!(Date.parse(raw?.newest) >= Date.parse(to) - 5000) && !(Date.parse(raw?.newest) >= Date.parse(from))) problems.push(`the Security log's newest event (${raw?.newest}) is older than the run's window`);
  // A value that is not a hex or decimal number is unknown (null), never zero: a missing exit status must not read as success.
  const hex = (v) => (typeof v === "string" && /^0x[0-9a-f]+$/i.test(v) ? Number.parseInt(v, 16) : typeof v === "string" && /^\d+$/.test(v) ? Number(v) : typeof v === "number" ? v : null);
  const events = (Array.isArray(raw?.events) ? raw.events : raw?.events ? [raw.events] : []).map((e) => e.id === 4688
    ? { id: 4688, time: e.time, pid: hex(e.data.NewProcessId), ppid: hex(e.data.ProcessId), exe: e.data.NewProcessName, commandLine: e.data.CommandLine ?? null }
    : { id: 4689, time: e.time, pid: hex(e.data.ProcessId), exe: e.data.ProcessName, exit: hex(e.data.Status) });
  const bad = events.filter((e) => !Number.isInteger(e.pid) || !Number.isFinite(Date.parse(e.time)) || (e.id === 4688 && !Number.isInteger(e.ppid)));
  if (bad.length) problems.push(`${bad.length} event(s) have no usable process id, parent or time`);
  if (events.some((e) => e.id === 4688 && (e.commandLine === null || e.commandLine === undefined))) problems.push("start events carry no command line (ProcessCreationIncludeCmdLine_Enabled is off)");
  return { kind: "windows-security", from, to, newest: raw?.newest ?? null, continuous: raw?.continuous === true, events, problems };
}

/** The stage the launched gate is asked for: its `--to` value (the gate's own default is the check stage). */
export const kindOf = (gateArgs) => { const i = gateArgs.indexOf("--to"); return i < 0 ? "check" : gateArgs[i + 1] ?? null; };

/** Starts one gate run and records it. Resolves with the gate's exit (or 128 + signal number). */
export async function launch({ out, commit, kind = null, receiptsDir, limits = null, lanes = null, repo = process.cwd(), gate = path.join(here, "bin", "rightkit-gate.mjs"), lock = null, gateArgs = [], env = process.env, spawnImpl = spawn, host = os.hostname(), exportProcesses = null, platform = process.platform, fingerprint = null, snapshotsRoot = null }) {
  if (!out || !/^[0-9a-f]{40}$/.test(String(commit ?? "")) || !receiptsDir) throw new Error("launch needs --out, a full --commit sha and --receipts");
  const derived = kindOf(gateArgs);
  if (kind !== null && kind !== derived) throw new Error(`--kind ${kind} does not match the stage the gate is asked for (--to ${derived}); the time limit is chosen by what is run`);
  const before = path.join(out, "before"); fs.mkdirSync(before, { recursive: true });
  const runId = crypto.randomBytes(32).toString("hex");
  const dir = path.dirname(path.resolve(out)); const lockFile = lock ?? path.join(dir, "acceptance-launch.lock"); const lastFile = path.join(dir, "acceptance-launch.last.json");
  const taken = takeLaunchLock(lockFile, { runId, pid: process.pid, out: path.resolve(out), startedAt: new Date().toISOString() });
  if (taken.refused) { process.stderr.write(`acceptance-launch: refused: ${taken.refused}; nothing started\n`); return 3; }
  // The limit and the lanes are pinned now, before the gate runs, never chosen afterwards.
  let limitMs = null; let limitsSha256 = null;
  if (limits) { const body = fs.readFileSync(limits); limitsSha256 = sha256(body); const v = JSON.parse(body.toString("utf8"))[`${host}/${derived}`]; limitMs = Number.isFinite(v) ? v : null; }
  const laneCount = Number.isInteger(lanes) && lanes > 0 ? lanes : (typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length);
  // The previous launched run, from the launcher's own record: its identity (whether or not the lock was released), the baseline commit, and whether
  // this run is the same kind of baseline (gate code, tools, node and platform equal).
  let last = null; try { last = JSON.parse(fs.readFileSync(lastFile, "utf8")); } catch { last = null; }
  const fp = fingerprint ?? fingerprintOf({ repo, commit, platform });
  const baseline = last ? { commit: last.commit ?? null, out: last.out ?? null, runId: last.runId ?? null, exit: last.exit ?? null, signal: last.signal ?? null, stable: sameFingerprint(last.fingerprint, fp) } : null;
  const previous = taken.previous ?? (last ? { runId: last.runId ?? null, out: last.out ?? null, pid: null, reclaimed: false, fromLastRun: true, exit: last.exit ?? null, signal: last.signal ?? null } : null);
  const list = (d) => { try { return fs.readdirSync(d).filter((n) => n.endsWith(".json")); } catch { return []; } };
  fs.writeFileSync(path.join(before, "receipts.txt"), `${list(receiptsDir).join("\n")}\n`);
  const files = { log: path.join(out, "gate.log"), launch: path.join(out, "launch.json") };
  const args = [gate, ...gateArgs, "--commit", commit];
  let snapRoot = snapshotsRoot; if (snapRoot === null) { try { snapRoot = (await import("./snapshot.mjs")).snapshotRoot({ platform, env }); } catch { snapRoot = null; } }
  const record = { schema: LAUNCH_SCHEMA, runId, commit, kind: derived, host, gate: { path: path.resolve(repo, gate), args }, launcherPid: process.pid, gatePid: null, lanes: laneCount, lanesSource: lanes ? "--lanes, handed to the gate as RK_GATE_JOBS" : "this host's cores, handed to the gate as RK_GATE_JOBS", limitMs, limitsSha256, receiptsDir: path.resolve(receiptsDir), snapshotsRoot: snapRoot, fingerprint: fp, baseline, startedAt: null, startMono: null, endedAt: null, endMono: null, exit: null, signal: null, previous };
  const save = () => fs.writeFileSync(files.launch, `${JSON.stringify(record, null, 2)}\n`);
  const logFd = fs.openSync(files.log, "a");
  record.startedAt = new Date().toISOString(); record.startMono = mono();
  const child = spawnImpl(process.execPath, args, { cwd: repo, windowsHide: true, stdio: ["ignore", logFd, logFd], env: { ...env, RK_GATE_JOBS: String(laneCount) } });
  record.gatePid = Number.isInteger(child.pid) ? child.pid : null;
  save();
  const forward = (sig) => { try { child.kill(sig); } catch { /* already gone */ } };
  process.on("SIGINT", forward); process.on("SIGTERM", forward);
  const [code, signal] = await new Promise((resolve) => { child.once("error", () => resolve([null, null])); child.once("exit", (c, s) => resolve([c, s])); });
  process.off("SIGINT", forward); process.off("SIGTERM", forward);
  record.endedAt = new Date().toISOString(); record.endMono = mono(); record.exit = code; record.signal = signal ?? null;
  const locksNow = (d) => { try { return fs.readdirSync(d).filter((n) => n.endsWith(".lock")).map((n) => path.join(d, n)); } catch { return null; } };
  // One read of the snapshot root decides both what was found and whether it could be read.
  const seenLocks = snapRoot ? locksNow(snapRoot) : null;
  record.afterRun = { observedAt: new Date().toISOString(), locks: seenLocks ?? [], locksKnown: seenLocks !== null, rootAlive: alive(record.gatePid) };
  save();
  try { fs.closeSync(logFd); } catch { /* closed */ }
  // The operating system's record of the run's programs, a little either side of the gate's own span.
  const from = new Date(Date.parse(record.startedAt) - 2000).toISOString(); const to = new Date(Date.now() + 2000).toISOString();
  const exported = exportProcesses ? exportProcesses(from, to) : platform === "win32" ? exportWindowsProcesses(from, to) : { kind: null, from, to, newest: null, continuous: false, events: [], problems: [`no process record is collected on ${platform} yet (eslogger on the Mac)`] };
  fs.writeFileSync(path.join(out, "processes.json"), `${JSON.stringify(exported)}\n`);
  // This run is now the last launched run, kept apart from the lock so the next run has its identity whether or not anything was released.
  const tmp = `${lastFile}.${process.pid}.tmp`; fs.writeFileSync(tmp, `${JSON.stringify({ runId, out: path.resolve(out), commit, fingerprint: fp, host, endedAt: record.endedAt, exit: code, signal: signal ?? null })}\n`); fs.renameSync(tmp, lastFile);
  try { const held = JSON.parse(fs.readFileSync(lockFile, "utf8")); if (held.runId === runId) fs.rmSync(lockFile, { force: true }); } catch { /* not ours any more */ }
  return code ?? (signal ? 128 + (os.constants.signals[signal] ?? 0) : 1);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2); const cut = argv.indexOf("--");
  const own = cut < 0 ? argv : argv.slice(0, cut); const gateArgs = cut < 0 ? [] : argv.slice(cut + 1);
  const opt = (name) => { const i = own.indexOf(name); return i < 0 ? null : own[i + 1]; };
  if (gateArgs.includes("--commit")) { console.error("acceptance-launch: the launcher owns --commit; give it before --"); process.exit(2); }
  launch({ out: opt("--out"), commit: opt("--commit"), kind: opt("--kind"), receiptsDir: opt("--receipts"), limits: opt("--limits"), lanes: opt("--lanes") ? Number(opt("--lanes")) : null, repo: opt("--repo") ?? process.cwd(), gate: opt("--gate") ?? undefined, lock: opt("--lock"), gateArgs })
    .then((code) => process.exit(code), (e) => { console.error(`acceptance-launch: ${e.message}`); process.exit(2); });
}
