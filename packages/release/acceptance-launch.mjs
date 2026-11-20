// The launcher of a gate run judged by the twelve lines (GATE-CONTRACT GC-A27, rightkit#1514 step 3). It is the witness the gate does not
// write: it owns the selected commit, the run id, the clock, the exit and the run lock, starts the gate with the run's record and process
// list switched on, and keeps the gate's log for people. Nothing it records is read from the gate's own word.
//
//   node acceptance-launch.mjs --out <run dir> --commit <sha> --kind <run kind> [--repo <dir>] [--gate <bin/rightkit-gate.mjs>]
//                              [--lock <file>] -- <gate arguments>
//
// The run directory holds:
//   launch.json            this file's record: { schema, runId, commit, kind, host, gate, launcherPid, gatePid, startedAt, startMono,
//                          endedAt, endMono, exit, signal, previous }, written when the gate starts and again, once, when it ends
//   record.jsonl           the gate's facts (run-record.mjs, RK_GATE_RUN_RECORD)
//   ledger.jsonl           the gate's process list (spawn-ledger.mjs, RK_GATE_SPAWN_LEDGER), with ledger.jsonl.journal.jsonl beside it
//   gate.log               the gate's own output, stamped by the gate
//
// The run lock (`--lock`, default the run directory's parent's acceptance-launch.lock) holds { runId, pid, out, startedAt }. A lock whose
// holder is alive refuses the run (exit 3, nothing started). A lock whose holder is dead is the run before this one, killed: this run takes
// it over and names it in `previous` { runId, out, pid, reclaimed: true }, so NN12 of that run is judged on this one.
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const LAUNCH_SCHEMA = "rightkit.acceptance-launch/1";
const here = path.dirname(fileURLToPath(import.meta.url));
const mono = () => process.hrtime.bigint().toString();
const alive = (pid) => { if (!Number.isInteger(pid) || pid <= 0) return false; try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };

/** Takes the run lock. Returns { previous } (the dead holder this run took over, or null) or { refused } when a live run holds it. */
export function takeLaunchLock(file, holder, { isAlive = alive, fsApi = fs } = {}) {
  fsApi.mkdirSync(path.dirname(file), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try { fsApi.writeFileSync(file, `${JSON.stringify(holder)}\n`, { flag: "wx" }); return { previous: null }; }
    catch (e) { if (e.code !== "EEXIST") throw e; }
    let held = null; try { held = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { held = null; }
    if (held && isAlive(held.pid)) return { refused: `run ${held.runId} (pid ${held.pid}) holds ${file}` };
    // A dead holder: the run before this one ended without releasing its lock (killed). This run takes it over and names it.
    fsApi.writeFileSync(file, `${JSON.stringify(holder)}\n`);
    return { previous: held ? { runId: held.runId ?? null, out: held.out ?? null, pid: held.pid ?? null, reclaimed: true } : { runId: null, out: null, pid: null, reclaimed: true, unreadable: true } };
  }
  return { refused: `${file} could not be taken` };
}

/** Starts one gate run and records it. Resolves with the gate's exit (or 128 + signal number). */
export async function launch({ out, commit, kind, repo = process.cwd(), gate = path.join(here, "bin", "rightkit-gate.mjs"), lock = null, gateArgs = [], env = process.env, spawnImpl = spawn, host = os.hostname() }) {
  if (!out || !/^[0-9a-f]{40}$/.test(String(commit ?? "")) || !kind) throw new Error("launch needs --out, a full --commit sha and --kind");
  fs.mkdirSync(out, { recursive: true });
  const runId = crypto.randomBytes(32).toString("hex");
  const lockFile = lock ?? path.join(path.dirname(path.resolve(out)), "acceptance-launch.lock");
  const taken = takeLaunchLock(lockFile, { runId, pid: process.pid, out: path.resolve(out), startedAt: new Date().toISOString() });
  if (taken.refused) { process.stderr.write(`acceptance-launch: refused: ${taken.refused}; nothing started\n`); return 3; }
  const files = { record: path.join(out, "record.jsonl"), ledger: path.join(out, "ledger.jsonl"), log: path.join(out, "gate.log"), launch: path.join(out, "launch.json") };
  const args = [gate, ...gateArgs, "--commit", commit];
  const record = { schema: LAUNCH_SCHEMA, runId, commit, kind, host, gate: { path: gate, args }, launcherPid: process.pid, gatePid: null, startedAt: null, startMono: null, endedAt: null, endMono: null, exit: null, signal: null, previous: taken.previous, files: { record: "record.jsonl", ledger: "ledger.jsonl", log: "gate.log" } };
  const save = () => fs.writeFileSync(files.launch, `${JSON.stringify(record, null, 2)}\n`);
  const logFd = fs.openSync(files.log, "a");
  record.startedAt = new Date().toISOString(); record.startMono = mono();
  const child = spawnImpl(process.execPath, args, {
    cwd: repo, windowsHide: true, stdio: ["ignore", logFd, logFd],
    env: { ...env, RK_GATE_RUN_ID: runId, RK_GATE_RUN_RECORD: path.resolve(files.record), RK_GATE_SPAWN_LEDGER: path.resolve(files.ledger), RK_GATE_LAUNCHER_PID: String(process.pid) },
  });
  record.gatePid = Number.isInteger(child.pid) ? child.pid : null;
  save();
  // A stop asked of the launcher goes to the gate; the gate's own exit is what is recorded.
  const forward = (sig) => { try { child.kill(sig); } catch { /* already gone */ } };
  process.on("SIGINT", forward); process.on("SIGTERM", forward);
  const [code, signal] = await new Promise((resolve) => { child.once("error", () => resolve([null, null])); child.once("exit", (c, s) => resolve([c, s])); });
  process.off("SIGINT", forward); process.off("SIGTERM", forward);
  record.endedAt = new Date().toISOString(); record.endMono = mono(); record.exit = code; record.signal = signal ?? null;
  save();
  try { fs.closeSync(logFd); } catch { /* closed */ }
  try { const held = JSON.parse(fs.readFileSync(lockFile, "utf8")); if (held.runId === runId) fs.rmSync(lockFile, { force: true }); } catch { /* not ours any more */ }
  return code ?? (signal ? 128 + (os.constants.signals[signal] ?? 0) : 1);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2); const cut = argv.indexOf("--");
  const own = cut < 0 ? argv : argv.slice(0, cut); const gateArgs = cut < 0 ? [] : argv.slice(cut + 1);
  const opt = (name) => { const i = own.indexOf(name); return i < 0 ? null : own[i + 1]; };
  if (gateArgs.includes("--commit")) { console.error("acceptance-launch: the launcher owns --commit; give it before --"); process.exit(2); }
  launch({ out: opt("--out"), commit: opt("--commit"), kind: opt("--kind"), repo: opt("--repo") ?? process.cwd(), gate: opt("--gate") ?? undefined, lock: opt("--lock"), gateArgs })
    .then((code) => process.exit(code), (e) => { console.error(`acceptance-launch: ${e.message}`); process.exit(2); });
}
