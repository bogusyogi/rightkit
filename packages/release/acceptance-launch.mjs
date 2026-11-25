// The launcher of a gate run judged by the twelve lines (GATE-CONTRACT GC-A27, rightkit#1514 step 3). The acceptance test takes nothing from
// the gate's own reporting (Adrian, 2026-11-21), so this launcher, not the gate, records the run: it owns the selected commit, the run id, the
// clock, the exit, the run lock, the Class A lane count, the time limit pinned before the gate starts, the run kind (derived from the stage the gate
// is asked for, never a label), the snapshot root, and its own record of the previous launched run (the baseline commit, and a fingerprint of the
// gate code, tools and environment that git cannot show). It keeps the gate's output for people. After the gate ends it exports the operating
// system's record of every program that started and exited in the run's window.
//
//   node acceptance-launch.mjs --out <run dir> --commit <sha> --receipts <broker receipts dir> [--kind <stage>] [--limits <file>]
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
  const tools = Object.fromEntries([["rustc", ["-vV"]], ["rustfmt", ["--version"]], ["cargo", ["--version"]], ["cargo-deny", ["--version"]], ["pnpm", ["--version"]], ["git", ["--version"]]].map(([c, a]) => [c, probe(c, a)]));
  return { gateCodeTree, node: process.version, platform, tools };
}
const complete = (f) => !!f && f.gateCodeTree !== null && Object.values(f.tools ?? {}).every((v) => typeof v === "string") && typeof f.node === "string";
export const sameFingerprint = (a, b) => complete(a) && complete(b) && JSON.stringify(a) === JSON.stringify(b);

/**
 * The operating system's record of the programs that started and exited in [from, to]: Windows Security events 4688 and 4689, read with
 * Get-WinEvent (Event Log Readers is enough). The record says how far the log had advanced (its newest event) and whether it reaches back to the
 * run's start (its oldest), and every event it keeps has a process id and a time.
 */
/**
 * The Security log's events as the system tool wevtutil writes them, parsed here: no PowerShell object in between. One query gives every event of the run's window (the record
 * ids prove none was lost), one the oldest event and one the newest. Windows PowerShell 5.1's ConvertTo-Json left a quote unescaped inside a command line that was itself JSON
 * and broke a whole run's record (run7: a 6.4 MB document failed to parse at 3.29 MB), and Get-WinEvent with ToXml took over 60 ms an event (a 150 s window took 359 s);
 * wevtutil takes seconds, and XML escaping keeps every character of a command line exact.
 */
const XML_ENTITIES = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" };
const unxml = (s) => String(s).replace(/&#x([0-9a-f]+);/gi, (_, h) => String.fromCodePoint(Number.parseInt(h, 16))).replace(/&#(\d+);/g, (_, d) => String.fromCodePoint(Number(d))).replace(/&(amp|lt|gt|quot|apos);/g, (_, n) => XML_ENTITIES[n]);
/** wevtutil's XML text into [{ id, recordId, time, data }], `data` holding each named Data value (null when it has no text). */
export function parseWevtutilEvents(text) {
  const out = [];
  for (const m of String(text).matchAll(/<Event[ >][\s\S]*?<\/Event>/g)) {
    const x = m[0];
    const id = /<EventID[^>]*>(\d+)<\/EventID>/.exec(x); const rec = /<EventRecordID>(\d+)<\/EventRecordID>/.exec(x); const time = /<TimeCreated[^>]*SystemTime=['"]([^'"]+)['"]/.exec(x);
    const data = {};
    for (const d of x.matchAll(/<Data Name=['"]([^'"]+)['"]\s*(?:\/>|>([\s\S]*?)<\/Data>)/g)) data[d[1]] = d[2] === undefined || d[2] === "" ? null : unxml(d[2]);
    out.push({ id: id ? Number(id[1]) : null, recordId: rec ? Number(rec[1]) : null, time: time ? time[1] : null, data });
  }
  return out;
}
export function exportWindowsProcesses(from, to, { run = execFileSync } = {}) {
  const problems = []; let oldest; let newest; let all;
  const wev = (args) => run("wevtutil.exe", ["qe", "Security", ...args, "/f:xml"], { encoding: "utf8", windowsHide: true, maxBuffer: 1024 * 1024 * 1024 });
  try {
    oldest = parseWevtutilEvents(wev(["/c:1", "/rd:false"]))[0]?.time ?? null;
    newest = parseWevtutilEvents(wev(["/c:1", "/rd:true"]))[0]?.time ?? null;
    all = parseWevtutilEvents(wev([`/q:*[System[TimeCreated[@SystemTime>='${from}' and @SystemTime<='${to}']]]`]));
  }
  catch (e) { return { kind: "windows-security", from, to, newest: null, continuous: false, events: [], problems: [`the Security log could not be read (${String(e.stderr || e.message).trim().split(/\r?\n/)[0]})`] }; }
  const ids = all.map((e) => e.recordId).filter(Number.isInteger).sort((a, b) => a - b);
  const continuous = all.every((e) => Number.isInteger(e.recordId)) && (ids.length <= 1 || ids[ids.length - 1] - ids[0] + 1 === ids.length);
  const raw = { oldest, newest, continuous, records: ids.length, events: all.filter((e) => e.id === 4688 || e.id === 4689) };
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
  // A program started from a file always carries its command line when ProcessCreationIncludeCmdLine_Enabled is on. A process the kernel makes with no image
  // file (vmmemWSL, vmmem, Registry, MemCompression: an image name with no path) never carries one, whoever started it: one such start on the host
  // (WSL's memory process, started by another session) made the 0.6.78 real build's whole record "incomplete" (2026-11-25).
  const noLine = events.filter((e) => e.id === 4688 && (e.commandLine === null || e.commandLine === undefined) && /[\\/]/.test(String(e.exe ?? "")));
  if (noLine.length) problems.push(`${noLine.length} start event(s) of programs carry no command line (ProcessCreationIncludeCmdLine_Enabled is off?; first: ${noLine[0].exe})`);
  return { kind: "windows-security", from, to, newest: raw?.newest ?? null, continuous: raw?.continuous === true, events, problems };
}

/** The stage the launched gate is asked for: its `--to` value (the gate's own default is the check stage). */
export const kindOf = (gateArgs) => { const i = gateArgs.indexOf("--to"); return i < 0 ? "check" : gateArgs[i + 1] ?? null; };

/**
 * A recursive listing of a folder as the file system shows it: relative path (forward slashes) -> [size, mtime in ms]. A folder that does not exist is an empty,
 * known listing; one that cannot be read, or that holds more files than `cap`, is null (unknown). Nothing in it is read.
 */
export function listFolder(dir, { cap = 200_000 } = {}) {
  if (!dir) return null;
  try { if (!fs.existsSync(dir)) return {}; } catch { return null; }
  const out = {}; let n = 0;
  const walk = (d, rel) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const r = rel ? `${rel}/${e.name}` : e.name; const f = path.join(d, e.name);
      if (e.isDirectory()) walk(f, r);
      else if (e.isFile()) { n += 1; if (n > cap) throw new Error("too many files"); const st = fs.statSync(f); out[r] = [st.size, Math.trunc(st.mtimeMs)]; }
    }
  };
  try { walk(dir, ""); } catch { return null; }
  return out;
}
/** The gate's scan-result store: the folder rightkit-gate-cache in the repository's git folder, its one fixed place (no variable chooses it), or null when the git folder cannot be found. */
export function cacheStoreDir(repo, _env = null, run = execFileSync) {
  try { const common = String(run("git", ["-C", repo, "rev-parse", "--git-common-dir"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], windowsHide: true })).trim(); return common ? path.join(path.resolve(repo, common), "rightkit-gate-cache") : null; } catch { return null; }
}
/** The folders of the snapshot root that came into being or changed since `sinceMs`, each listed (the stat record of the snapshots at the gate's exit). */
export function statSnapshots(root, sinceMs, { cap = 200_000, entries = null } = {}) {
  if (!root) return { root: null, dirs: {}, known: false };
  let names; try { names = (entries ?? fs.readdirSync(root, { withFileTypes: true })).filter((e) => e.isDirectory()); } catch { return { root, dirs: {}, known: false }; }
  const dirs = {}; let total = 0; let known = true;
  for (const e of names) {
    let st; try { st = fs.statSync(path.join(root, e.name)); } catch { known = false; continue; }
    const files = listFolder(path.join(root, e.name), { cap: cap - total });
    if (files === null) { dirs[e.name] = null; known = false; continue; }
    total += Object.keys(files).length; dirs[e.name] = files;
  }
  return { root, dirs, known };
}

/** Starts one gate run and records it. Resolves with the gate's exit (or 128 + signal number). */
export async function launch({ out, commit, kind = null, receiptsDir, limits = null, lanes = null, repo = process.cwd(), gate = path.join(here, "bin", "rightkit-gate.mjs"), lock = null, gateArgs = [], env = process.env, spawnImpl = spawn, host = os.hostname(), exportProcesses = null, platform = process.platform, fingerprint = null, snapshotsRoot = null, onStart = null, managedRoot = null }) {
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
  // The lanes are the gate's own default, this host's cores: the launcher hands the gate no lane count and no cache folder (Adrian, 2026-11-24: no agent chooses either).
  if (lanes !== null) throw new Error("the launcher no longer sets the gate's lanes: they are this host's cores");
  const laneCount = typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length;
  // The previous launched run, from the launcher's own record: its identity (whether or not the lock was released), the baseline commit, and whether
  // this run is the same kind of baseline (gate code, tools, node and platform equal).
  let last = null; try { last = JSON.parse(fs.readFileSync(lastFile, "utf8")); } catch { last = null; }
  const fp = fingerprint ?? fingerprintOf({ repo, commit, platform });
  const baseline = last ? { commit: last.commit ?? null, out: last.out ?? null, runId: last.runId ?? null, exit: last.exit ?? null, signal: last.signal ?? null, stable: sameFingerprint(last.fingerprint, fp) } : null;
  const previous = taken.previous ?? (last ? { runId: last.runId ?? null, out: last.out ?? null, pid: null, reclaimed: false, fromLastRun: true, exit: last.exit ?? null, signal: last.signal ?? null } : null);
  const list = (d) => { try { return fs.readdirSync(d).filter((n) => n.endsWith(".json")); } catch { return []; } };
  fs.writeFileSync(path.join(before, "receipts.txt"), `${list(receiptsDir).join("\n")}\n`);
  // The gate's scan-result store as the file system shows it before the run, the launcher's own listing (nothing in it is read).
  // A run folder reused after a crash must not offer the earlier run's listings.
  for (const stale of ["cache-store-after.json", "snapshot-stat.json"]) fs.rmSync(path.join(out, stale), { force: true });
  const storeDir = cacheStoreDir(repo); const storeBefore = listFolder(storeDir);
  // Two folders of the managed data root, listed before and after by the launcher: the Class A verdict files the broker requires before it admits a compile, and
  // the gate's Class A decision records (its forecast). The listings are the launcher's; what a decision record says is the gate's own statement and is read as that.
  let snapRootEarly = snapshotsRoot; if (snapRootEarly === null) { try { snapRootEarly = (await import("./snapshot.mjs")).snapshotRoot({ platform, env }); } catch { snapRootEarly = null; } }
  const managed = managedRoot ?? (snapRootEarly ? path.dirname(snapRootEarly) : null);
  const verdictDir = managed ? path.join(managed, "class-a-verdicts") : null; const decisionDir = managed ? path.join(managed, "runs", "rightkit-class-a-decisions") : null;
  const sideBefore = { verdicts: listFolder(verdictDir), decisions: listFolder(decisionDir) };
  fs.writeFileSync(path.join(before, "side-folders.json"), `${JSON.stringify({ verdictDir, decisionDir, ...sideBefore })}\n`);
  fs.writeFileSync(path.join(before, "cache-store.json"), `${JSON.stringify({ dir: storeDir, files: storeBefore })}\n`);
  const files = { log: path.join(out, "gate.log"), launch: path.join(out, "launch.json") };
  const args = [gate, ...gateArgs, "--commit", commit];
  const snapRoot = snapRootEarly;
  const record = { schema: LAUNCH_SCHEMA, runId, commit, kind: derived, host, gate: { path: path.resolve(repo, gate), args }, launcherPid: process.pid, gatePid: null, lanes: laneCount, lanesSource: "this host's cores, the gate's own default; nothing is handed to the gate", limitMs, limitsSha256, receiptsDir: path.resolve(receiptsDir), snapshotsRoot: snapRoot, fingerprint: fp, baseline, startedAt: null, startMono: null, endedAt: null, endMono: null, exit: null, signal: null, previous };
  const save = () => fs.writeFileSync(files.launch, `${JSON.stringify(record, null, 2)}\n`);
  const logFd = fs.openSync(files.log, "a");
  record.startedAt = new Date().toISOString(); record.startMono = mono();
  const child = spawnImpl(process.execPath, args, { cwd: repo, windowsHide: true, stdio: ["ignore", logFd, logFd], env: (() => { const e = { ...env }; for (const k of Object.keys(e)) if (/^(RK_GATE_JOBS|RK_GATE_CACHE_DIR)$/i.test(k)) delete e[k]; return e; })() });
  record.gatePid = Number.isInteger(child.pid) ? child.pid : null;
  save();
  if (typeof onStart === "function") { try { onStart({ child, record: { ...record }, out }); } catch { /* the hook is the caller's */ } }
  const forward = (sig) => { try { child.kill(sig); } catch { /* already gone */ } };
  process.on("SIGINT", forward); process.on("SIGTERM", forward);
  const [code, signal] = await new Promise((resolve) => { child.once("error", () => resolve([null, null])); child.once("exit", (c, s) => resolve([c, s])); });
  process.off("SIGINT", forward); process.off("SIGTERM", forward);
  record.endedAt = new Date().toISOString(); record.endMono = mono(); record.exit = code; record.signal = signal ?? null;
  // The snapshot root is read once, with its entry kinds: the locks and the stat record of the snapshots both come from that one read.
  const rootEntries = (() => { try { return snapRoot ? fs.readdirSync(snapRoot, { withFileTypes: true }) : null; } catch { return null; } })();
  const locksNow = (d) => (rootEntries === null ? null : rootEntries.map((e) => e.name).filter((n) => n.endsWith(".lock")).map((n) => path.join(d, n)));
  // One read of the snapshot root decides both what was found and whether it could be read.
  const seenLocks = snapRoot ? locksNow(snapRoot) : null;
  record.afterRun = { observedAt: new Date().toISOString(), locks: seenLocks ?? [], locksKnown: seenLocks !== null, rootAlive: alive(record.gatePid) };
  // At the gate's exit, before anything can change: the store listed again, and the stat record of the snapshots this run made or touched.
  const storeAfter = listFolder(storeDir);
  fs.writeFileSync(path.join(out, "cache-store-after.json"), `${JSON.stringify({ dir: storeDir, files: storeAfter })}\n`);
  // The same two folders after the run, and which store files this run wrote with the row each names (read at the gate's exit, before a later run can change them).
  const sideAfter = { verdicts: listFolder(verdictDir), decisions: listFolder(decisionDir) };
  fs.writeFileSync(path.join(out, "side-folders-after.json"), `${JSON.stringify({ verdictDir, decisionDir, ...sideAfter })}\n`);
  const changed = storeBefore && storeAfter ? Object.keys(storeAfter).filter((k) => !storeBefore[k] || storeBefore[k][0] !== storeAfter[k][0] || storeBefore[k][1] !== storeAfter[k][1]).sort() : null;
  const rowOfFile = (name) => { try { const d = JSON.parse(fs.readFileSync(path.join(storeDir, name), "utf8")); return typeof d?.row?.id === "string" ? d.row.id : typeof d?.id === "string" ? d.id : null; } catch { return null; } };
  // A combined result names its crates in its own record, or in the command it stores (`-p <crate>`).
  const cratesOfFile = (name) => { try { const d = JSON.parse(fs.readFileSync(path.join(storeDir, name), "utf8")); if (Array.isArray(d?.row?.crates)) return d.row.crates.map(String); let units = null; try { const text = String(d?.row?.raw?.inputs ?? "null"); units = JSON.parse(text.slice(0, text.lastIndexOf("}") + 1) || "null")?.units; /* the key text is the JSON, then :<key schema> */ } catch { units = null; } if (Array.isArray(units) && units.length) return [...new Set(units.map((u) => String(Array.isArray(u) ? u[0] : u).split(":").pop()))]; /* a combined result's row-index entry lists its units, <workspace>:<crate> */ const words = String(d?.row?.command ?? "").split(/\s+/); const out = []; words.forEach((w, i) => { if ((w === "-p" || w === "--package") && words[i + 1]) out.push(words[i + 1]); }); return out.length ? [...new Set(out)] : null; } catch { return null; } };
  fs.writeFileSync(path.join(out, "cache-written.json"), `${JSON.stringify(changed === null ? null : changed.map((name) => ({ name, row: rowOfFile(name), crates: cratesOfFile(name) })))}\n`);
  const snaps = rootEntries === null ? { root: snapRoot, dirs: {}, known: false } : statSnapshots(snapRoot, Date.parse(record.startedAt), { entries: rootEntries });
  fs.writeFileSync(path.join(out, "snapshot-stat.json"), `${JSON.stringify(snaps)}\n`);
  record.cacheStore = { dir: storeDir, beforeKnown: storeBefore !== null, afterKnown: storeAfter !== null }; record.snapshotStat = { root: snaps.root, folders: Object.keys(snaps.dirs), known: snaps.known };
  save();
  try { fs.closeSync(logFd); } catch { /* closed */ }
  // The operating system's record of the run's programs, a little either side of the gate's own span. Windows writes an exit event shortly after the
  // process ends; the record is read two seconds after the gate's exit so a late one is in it (case 7 on 0.6.66, 2026-11-24: one git exit was missing).
  if (!exportProcesses && platform === "win32") await new Promise((resolve) => setTimeout(resolve, 2000));
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
  launch({ out: opt("--out"), commit: opt("--commit"), kind: opt("--kind"), receiptsDir: opt("--receipts"), limits: opt("--limits"), repo: opt("--repo") ?? process.cwd(), gate: opt("--gate") ?? undefined, lock: opt("--lock"), gateArgs })
    .then((code) => process.exit(code), (e) => { console.error(`acceptance-launch: ${e.message}`); process.exit(2); });
}
