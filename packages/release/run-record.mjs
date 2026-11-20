// The run record (GATE-CONTRACT GC-A27, rightkit#1514 step 3): the facts a gate run's twelve-line judge reads, written by the gate as each one
// happens, in order, to one append-only JSON-lines file named by RK_GATE_RUN_RECORD (the launcher, acceptance-launch.mjs, sets it). Off when
// the variable is unset, and never fails a run. The record is an index, not proof: each fact names a file or a value the judge holds against
// a witness the gate does not write (the launcher's own record of the run, the spawn ledger's process journal, the broker's signed receipts).
//
// One line per fact: { kind, seq, mono, at, pid, run, ...fields }. `seq` counts this process's facts; `mono` is process.hrtime.bigint() as a
// decimal string, one monotonic clock for every process of the host, so facts of a run and its nested gates order without wall-clock ties;
// `at` is the wall clock for people. `run` is RK_GATE_RUN_ID. The last fact of each writer is `end` (written once, on exit).
// The variables are RK_GATE_*, which no cache key reads (bin/rightkit-gate.mjs BUILD_ENV_EXCLUDED), so turning the record on changes no result.
import fs from "node:fs";

export const RUN_RECORD_SCHEMA = "rightkit.run-record/1";
/** Every kind of fact the gate writes, with the fields the judge reads. Anything else is not a fact of this schema. */
export const FACT_KINDS = Object.freeze({
  request: ["to", "commit"], // the stages asked for and the commit the gate resolved
  lock: ["token", "reclaimed"], // the snapshot lock this run holds; reclaimed: { from, proof } or null
  snapshot: ["path", "commit", "tree"], // the snapshot synced or verified for this run
  "still-tree": ["stage", "path", "tree"], // the snapshot's tree checked again after a stage (check, compile)
  capacity: ["classAJobs", "classBJobs", "cores", "brokerHost"], // the lanes the scheduler was given, and the host's cores, during the run
  forecast: ["class", "file"], // a forecast frozen (and written) before its stage's lookups
  lookup: ["class", "key"], // one cache lookup, in the order it happened
  decision: ["class", "file", "mismatches"], // the observed decision written after the stage's lookups
  divergence: ["class", "row"], // one lookup that disagreed with the forecast (RK_CACHE_DIVERGENCE), with its row and cause
  lanes: ["class", "rows"], // the scheduler's own ready, start and end times per Class A row
  coverage: ["class", "file"], // a coverage report written
  verdict: ["file", "status"], // the Class A verdict written
  "broker-job": ["requestId", "argv", "cwd", "receipt", "receiptSha256", "status", "testExecutables"], // one broker submission and its receipt
  "receipt-out": ["file"], // the gate's own receipt written
  end: ["exit", "root"], // this writer's last fact
});

let state = null;
const now = () => ({ mono: process.hrtime.bigint().toString(), at: new Date().toISOString() });

/** Opens the record for this process when RK_GATE_RUN_RECORD names a file. Returns whether it is on. Idempotent. */
export function openRunRecord(env = process.env) {
  if (state) return true;
  const file = env.RK_GATE_RUN_RECORD;
  if (typeof file !== "string" || file === "") return false;
  state = { file, run: typeof env.RK_GATE_RUN_ID === "string" ? env.RK_GATE_RUN_ID : null, seq: 0, ended: false, root: String(process.ppid) === String(env.RK_GATE_LAUNCHER_PID ?? "") };
  return true;
}

/** Appends one fact. A no-op when the record is off; a write failure is swallowed (the record never fails a run, and a missing fact is "not measured"). */
export function fact(kind, fields = {}) {
  if (!state || state.ended) return;
  if (!Object.hasOwn(FACT_KINDS, kind)) throw new Error(`run record: unknown fact kind ${kind}`);
  state.seq += 1;
  const line = { kind, seq: state.seq, ...now(), pid: process.pid, run: state.run, ...fields };
  try { fs.appendFileSync(state.file, `${JSON.stringify(line)}\n`); } catch { /* the record never fails a run */ }
}

/** Writes this writer's `end` fact once. `root`: this is the gate process the launcher started (its parent is the launcher). */
export function endRunRecord(exit) {
  if (!state || state.ended) return;
  fact("end", { exit: exit ?? null, root: state.root });
  state.ended = true;
}

/** For tests: forget the open record. */
export function resetRunRecordForTest() { state = null; }

/** Reads a record's text: its facts in order (by `mono`, then writer and `seq`), and every line that is not a fact of this schema. */
export function parseRunRecord(text) {
  const facts = []; const problems = [];
  String(text ?? "").split(/\r?\n/).forEach((raw, i) => {
    if (!raw.trim()) return;
    let f; try { f = JSON.parse(raw); } catch (e) { problems.push(`record line ${i + 1} is not JSON (${e.message})`); return; }
    if (!f || typeof f !== "object" || !Object.hasOwn(FACT_KINDS, f.kind)) { problems.push(`record line ${i + 1} is not a fact of ${RUN_RECORD_SCHEMA}`); return; }
    if (!/^\d+$/.test(String(f.mono ?? "")) || !Number.isInteger(f.seq) || !Number.isInteger(f.pid)) { problems.push(`record line ${i + 1} (${f.kind}) has no clock reading, sequence or writer`); return; }
    facts.push(f);
  });
  facts.sort((a, b) => { const d = BigInt(a.mono) - BigInt(b.mono); return d < 0n ? -1 : d > 0n ? 1 : a.pid - b.pid || a.seq - b.seq; });
  return { facts, problems };
}
