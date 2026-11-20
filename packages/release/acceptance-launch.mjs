// The launcher of a gate run judged by the twelve lines (GATE-CONTRACT GC-A27, rightkit#1514 step 3). The acceptance test takes nothing from
// the gate's own reporting (Adrian, 2026-11-21), so this launcher, not the gate, records the run: it owns the selected commit, the run id, the
// clock, the exit, the run lock, the Class A lane count and the time limit pinned before the gate starts; it copies the gate's state from
// before the run (the row history, the cache keys present, the broker receipts present); it keeps the gate's output for people; and after the
// gate ends it exports the operating system's record of every program that started and exited in the run's window.
//
//   node acceptance-launch.mjs --out <run dir> --commit <sha> --kind <run kind> --cache-dir <gate cache store> --receipts <broker receipts dir>
//                              [--limits <file>] [--lanes <n>] [--history-root <dir>] [--repo <dir>] [--gate <bin/rightkit-gate.mjs>]
//                              [--lock <file>] -- <gate arguments>
//
// The run folder holds launch.json (this record), before/history.json, before/cache-keys.txt, before/receipts.txt, processes.json and gate.log.
// The run lock (`--lock`, default the run folder's parent's acceptance-launch.lock) holds { runId, pid, out, startedAt }. A live holder refuses
// the run (exit 3, nothing started). A dead holder is the run before this one, killed: this run takes the lock over and names it in
// `previous`, so that run's NN12 is judged on this one.
import { execFileSync, spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gateRunsRoot } from "./managed-roots.mjs";

export const LAUNCH_SCHEMA = "rightkit.acceptance-launch/2";
const here = path.dirname(fileURLToPath(import.meta.url));
const mono = () => process.hrtime.bigint().toString();
const alive = (pid) => { if (!Number.isInteger(pid) || pid <= 0) return false; try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");

/** Takes the run lock. Returns { previous } (the dead holder this run took over, or null) or { refused } when a live run holds it. */
export function takeLaunchLock(file, holder, { isAlive = alive, fsApi = fs } = {}) {
  fsApi.mkdirSync(path.dirname(file), { recursive: true });
  try { fsApi.writeFileSync(file, `${JSON.stringify(holder)}\n`, { flag: "wx" }); return { previous: null }; }
  catch (e) { if (e.code !== "EEXIST") throw e; }
  let held = null; try { held = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { held = null; }
  if (held && isAlive(held.pid)) return { refused: `run ${held.runId} (pid ${held.pid}) holds ${file}` };
  // A dead holder: take it over by renaming a fresh lock onto it, then read it back; another launcher racing here sees its own or ours.
  const tmp = `${file}.${process.pid}.${crypto.randomBytes(4).toString("hex")}`;
  fsApi.writeFileSync(tmp, `${JSON.stringify(holder)}\n`); fsApi.renameSync(tmp, file);
  let now = null; try { now = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { now = null; }
  if (now?.runId !== holder.runId) return { refused: `another launcher took over ${file}` };
  return { previous: held ? { runId: held.runId ?? null, out: held.out ?? null, pid: held.pid ?? null, reclaimed: true } : { runId: null, out: null, pid: null, reclaimed: true, unreadable: true } };
}

/** The history file the gate keeps for a cache store (class-a-cache-history.mjs: the sha256 of the store's resolved path, lowercased on Windows). */
export const historyFileFor = (historyRoot, cacheDir, platform = process.platform) => { const store = path.resolve(cacheDir); return path.join(historyRoot, `${sha256(platform === "win32" ? store.toLowerCase() : store)}.json`); };

/**
 * The operating system's record of the programs that started and exited in [from, to]: Windows Security events 4688 and 4689, read with
 * Get-WinEvent (Event Log Readers is enough). The record covers the window only when the log's oldest event is older than its start.
 */
export function exportWindowsProcesses(from, to, { run = execFileSync } = {}) {
  const ps = `$ErrorActionPreference='Stop'; $from=[datetime]::Parse('${from}').ToLocalTime(); $to=[datetime]::Parse('${to}').ToLocalTime();
$oldest=(Get-WinEvent -LogName Security -MaxEvents 1 -Oldest).TimeCreated.ToUniversalTime().ToString('o');
$ev=@(Get-WinEvent -FilterHashtable @{LogName='Security'; Id=4688,4689; StartTime=$from; EndTime=$to} -ErrorAction SilentlyContinue | ForEach-Object { $x=[xml]$_.ToXml(); $d=@{}; foreach($n in $x.Event.EventData.Data){ $d[$n.Name]=$n.'#text' }; [pscustomobject]@{ id=$_.Id; time=$_.TimeCreated.ToUniversalTime().ToString('o'); data=$d } });
[pscustomobject]@{ oldest=$oldest; events=$ev } | ConvertTo-Json -Depth 5 -Compress`;
  const problems = []; let raw;
  try { raw = JSON.parse(run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", ps], { encoding: "utf8", windowsHide: true, maxBuffer: 512 * 1024 * 1024 })); }
  catch (e) { return { kind: "windows-security", from, to, events: [], problems: [`the Security log could not be read (${String(e.stderr ?? e.message).trim().split(/\r?\n/)[0]})`] }; }
  if (!(Date.parse(raw?.oldest) <= Date.parse(from))) problems.push(`the Security log's oldest event (${raw?.oldest}) is newer than the run's start, so events may have been overwritten`);
  const hex = (v) => (typeof v === "string" && /^0x[0-9a-f]+$/i.test(v) ? Number.parseInt(v, 16) : Number(v));
  const events = (Array.isArray(raw?.events) ? raw.events : raw?.events ? [raw.events] : []).map((e) => e.id === 4688
    ? { id: 4688, time: e.time, pid: hex(e.data.NewProcessId), ppid: hex(e.data.ProcessId), exe: e.data.NewProcessName, commandLine: e.data.CommandLine ?? null }
    : { id: 4689, time: e.time, pid: hex(e.data.ProcessId), exe: e.data.ProcessName, exit: hex(e.data.Status) });
  if (events.some((e) => e.id === 4688 && (e.commandLine === null || e.commandLine === undefined))) problems.push("start events carry no command line (ProcessCreationIncludeCmdLine_Enabled is off)");
  return { kind: "windows-security", from, to, events, problems };
}

/** Starts one gate run and records it. Resolves with the gate's exit (or 128 + signal number). */
export async function launch({ out, commit, kind, cacheDir, receiptsDir, limits = null, lanes = null, historyRoot = null, repo = process.cwd(), gate = path.join(here, "bin", "rightkit-gate.mjs"), lock = null, gateArgs = [], env = process.env, spawnImpl = spawn, host = os.hostname(), exportProcesses = null, platform = process.platform }) {
  if (!out || !/^[0-9a-f]{40}$/.test(String(commit ?? "")) || !kind || !cacheDir || !receiptsDir) throw new Error("launch needs --out, a full --commit sha, --kind, --cache-dir and --receipts");
  const before = path.join(out, "before"); fs.mkdirSync(before, { recursive: true });
  const runId = crypto.randomBytes(32).toString("hex");
  const lockFile = lock ?? path.join(path.dirname(path.resolve(out)), "acceptance-launch.lock");
  const taken = takeLaunchLock(lockFile, { runId, pid: process.pid, out: path.resolve(out), startedAt: new Date().toISOString() });
  if (taken.refused) { process.stderr.write(`acceptance-launch: refused: ${taken.refused}; nothing started\n`); return 3; }
  // The limit and the lanes are pinned now, before the gate runs, never chosen afterwards.
  let limitMs = null; let limitsSha256 = null;
  if (limits) { const body = fs.readFileSync(limits); limitsSha256 = sha256(body); const v = JSON.parse(body.toString("utf8"))[`${host}/${kind}`]; limitMs = Number.isFinite(v) ? v : null; }
  const laneCount = Number.isInteger(lanes) && lanes > 0 ? lanes : (typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length);
  // The gate's state before the run, copied before it starts.
  const histRoot = historyRoot ?? path.join(gateRunsRoot({ env, platform }), "rightkit-class-a-history");
  const histFile = historyFileFor(histRoot, cacheDir, platform);
  let historyCopiedAt = null; try { fs.copyFileSync(histFile, path.join(before, "history.json")); historyCopiedAt = new Date().toISOString(); } catch { historyCopiedAt = null; }
  const list = (d) => { try { return fs.readdirSync(d).filter((n) => n.endsWith(".json")); } catch { return []; } };
  fs.writeFileSync(path.join(before, "cache-keys.txt"), `${list(cacheDir).map((n) => n.slice(0, -5)).join("\n")}\n`);
  fs.writeFileSync(path.join(before, "receipts.txt"), `${list(receiptsDir).join("\n")}\n`);
  const files = { log: path.join(out, "gate.log"), launch: path.join(out, "launch.json") };
  const args = [gate, ...gateArgs, "--commit", commit];
  const record = { schema: LAUNCH_SCHEMA, runId, commit, kind, host, gate: { path: gate, args }, launcherPid: process.pid, gatePid: null, lanes: laneCount, lanesSource: lanes ? "--lanes, handed to the gate as RK_GATE_JOBS" : "this host's cores, handed to the gate as RK_GATE_JOBS", limitMs, limitsSha256, historyFile: histFile, historyCopiedAt, cacheDir: path.resolve(cacheDir), receiptsDir: path.resolve(receiptsDir), startedAt: null, startMono: null, endedAt: null, endMono: null, exit: null, signal: null, previous: taken.previous };
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
  save();
  try { fs.closeSync(logFd); } catch { /* closed */ }
  // The operating system's record of the run's programs, a little either side of the gate's own span.
  const from = new Date(Date.parse(record.startedAt) - 2000).toISOString(); const to = new Date(Date.now() + 2000).toISOString();
  const exported = exportProcesses ? exportProcesses(from, to) : platform === "win32" ? exportWindowsProcesses(from, to) : { kind: null, from, to, events: [], problems: [`no process record is collected on ${platform} yet (eslogger on the Mac)`] };
  fs.writeFileSync(path.join(out, "processes.json"), `${JSON.stringify(exported)}\n`);
  try { const held = JSON.parse(fs.readFileSync(lockFile, "utf8")); if (held.runId === runId) fs.rmSync(lockFile, { force: true }); } catch { /* not ours any more */ }
  return code ?? (signal ? 128 + (os.constants.signals[signal] ?? 0) : 1);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2); const cut = argv.indexOf("--");
  const own = cut < 0 ? argv : argv.slice(0, cut); const gateArgs = cut < 0 ? [] : argv.slice(cut + 1);
  const opt = (name) => { const i = own.indexOf(name); return i < 0 ? null : own[i + 1]; };
  if (gateArgs.includes("--commit")) { console.error("acceptance-launch: the launcher owns --commit; give it before --"); process.exit(2); }
  launch({ out: opt("--out"), commit: opt("--commit"), kind: opt("--kind"), cacheDir: opt("--cache-dir"), receiptsDir: opt("--receipts"), limits: opt("--limits"), lanes: opt("--lanes") ? Number(opt("--lanes")) : null, historyRoot: opt("--history-root"), repo: opt("--repo") ?? process.cwd(), gate: opt("--gate") ?? undefined, lock: opt("--lock"), gateArgs })
    .then((code) => process.exit(code), (e) => { console.error(`acceptance-launch: ${e.message}`); process.exit(2); });
}
