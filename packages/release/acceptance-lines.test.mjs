// Each of the twelve lines is shown to pass a run that meets it and to fail, or to say "not measured", for a run that breaks it or leaves
// no evidence of it, on small hand-made evidence of the same shape a real run leaves outside the gate (the operating system's process record,
// the broker's signed jobs, git, the history copied before the run, the cache store, the launcher). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import os from "node:os";
import path from "node:path";
import { CHECK_IDS, CONTRACT_FILE, judgeRun, linkProblems, parseAcceptanceLines, parseCommandLine, processesFromEslogger, processesFromWindowsEvents, readAcceptanceLines, readRunDir } from "./acceptance-lines.mjs";
import { execFileSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { exportWindowsProcesses, fingerprintOf, kindOf, launch, parseWevtutilEvents, sameFingerprint, takeLaunchLock } from "./acceptance-launch.mjs";
import * as LINESMOD from "./acceptance-lines.mjs";

// The lines under test are the contract's own: docs/GATE-CONTRACT.md [GC-A27], read here exactly as the command line reads them.
const CONTRACT = fs.readFileSync(CONTRACT_FILE, "utf8");
const LINES = readAcceptanceLines(CONTRACT);
const twelve = Array.from({ length: 12 }, (_, i) => `NN${String(i + 1).padStart(2, "0")}`);
const withoutRow = (id) => CONTRACT.split(/\r?\n/).filter((l) => !l.startsWith(`| \`${id}\` |`)).join("\n");

test("the contract's GC-A27 table is the test's only list of lines: thirteen, NN01 to NN13 (NN13 added 2026-11-25, rightkit#1695), each with a check and no check without a line", () => {
  assert.deepEqual(LINES.map((l) => l.id), [...twelve, "NN13"]);
  assert.deepEqual([...CHECK_IDS], [...twelve, "NN13"]);
  assert.deepEqual(linkProblems(LINES), []);
  for (const l of LINES) assert.ok(l.text.length > 0 && l.text.endsWith("."), `${l.id} is a whole sentence: ${l.text}`);
  assert.equal(LINES.find((l) => l.id === "NN01").text, CONTRACT.match(/^\| `NN01` \| (.+) \|$/m)[1], "the sentence is the contract's, character for character");
});

test("a line removed from the contract's table is refused: its check then has no line", () => {
  for (const id of twelve) {
    const altered = withoutRow(id);
    assert.notEqual(altered, CONTRACT, `${id}'s row was found and removed`);
    assert.throws(() => readAcceptanceLines(altered), (e) => e.problems.includes(`${id} has a check and is not a line in the contract`), `removing ${id}`);
    assert.throws(() => judgeRun({ lines: parseAcceptanceLines(altered).lines }), /has a check and is not a line in the contract/);
  }
});

test("a line added to the contract with no check is refused, and so is a check with no line", () => {
  const added = CONTRACT.replace("<!-- acceptance-lines:end -->", "| `NN14` | A new line nobody wrote a check for. |\n<!-- acceptance-lines:end -->");
  assert.throws(() => readAcceptanceLines(added), (e) => e.problems.includes("NN14 is a line in the contract and has no check"));
  assert.deepEqual(linkProblems(LINES, [...CHECK_IDS, "NN14"]), ["NN14 has a check and is not a line in the contract"]);
});

test("anything but one clean table is refused: no markers, a repeated id, a row of another shape, an empty sentence, no lines given", () => {
  assert.throws(() => readAcceptanceLines(CONTRACT.replace("<!-- acceptance-lines:begin -->", "")), /has no <!-- acceptance-lines:begin -->/);
  assert.throws(() => readAcceptanceLines(CONTRACT.replace("| `NN02` |", "| `NN01` |")), /NN01 appears more than once/);
  assert.throws(() => readAcceptanceLines(CONTRACT.replace("| `NN03` |", "| NN03 |")), /not `\| \\`NNxx\\` \| sentence \|`/);
  assert.throws(() => readAcceptanceLines(CONTRACT.replace(/^\| `NN04` \| .+ \|$/m, "| `NN04` |  |")), /NN04 has no sentence/);
  assert.throws(() => readAcceptanceLines(`${CONTRACT}\n<!-- acceptance-lines:begin -->\n<!-- acceptance-lines:end -->\n`), /more than one acceptance-lines table/);
  assert.throws(() => judgeRun({}), /no lines were given/);
});

// ---- the records of one good compile run ------------------------------------------------------------------------------------------------------
// Class A: rustfmt:2021 changed (one of its two files changed) and ran; deny is unchanged and reused. Class B: clippy of crate a is unchanged and
// reused, an earlier failure; clippy of crate b changed and ran, as one broker compile. Every record agrees with every other.
// ---- the evidence of one good compile run --------------------------------------------------------------------------------------------------------
// Nothing here is written by the gate (Adrian, 2026-11-21: the test takes nothing from the gate's own reporting): no log, no run record, no row history,
// no cache entry, no forecast, coverage or verdict text. It is what the operating system recorded (programs, parents, times, exits), what the broker signed
// (one clippy job for crate b), what git says (the selected commit's files and the launcher's baseline commit's), the launcher's own record, and the
// previous launched run's own evidence. The row inventory (which rows exist and the files each reads) and when each row became ready are inputs from
// outside the gate; the real sources for them do not exist yet, so on a real run they are null and the lines that need them say so.
// Class A: rustfmt:2021 reads src/a.rs and src/b.rs; src/a.rs changed since the baseline, so rustfmt is owed src/a.rs; deny is unchanged.
// Class B: clippy of crate a is unchanged and failed in the previous run; clippy of crate b changed and was built by one broker job.
const C = "a".repeat(40); const P = "9".repeat(40); const T = "b".repeat(40); const SNAP = "D:\\snap\\k1"; const GATE = 100; const LAUNCHER = 50;
const T0 = Date.UTC(2026, 10, 20, 6, 0, 0);
const at = (s) => new Date(T0 + s * 1000).toISOString();
const ms = (s) => T0 + s * 1000;
let nextPid = 200;
const proc = (s, exe, args, { end = s + 0.05, exit = 0, ppid = GATE, pid = (nextPid += 1), cwd = null } = {}) => ({ pid, ppid, exe, args, cwd, start: ms(s), end: end === null ? null : ms(end), exit });
const GIT = "C:\\Program Files\\Git\\cmd\\git.exe"; const RUSTFMT = "C:\\rust\\bin\\rustfmt.exe"; const RUSTRIGHT = "D:\\managed\\agent-bin\\rustright.exe"; const DENY = "C:\\cargo\\bin\\cargo-deny.exe";
const VERDICT = "D:\\verdicts\\v.json";
const job = (id, argv, { cwd = `${SNAP}\\ws`, start = 6.1, end = 8, waits = { admission: 50, queue: 0, bind: 50 }, gate = { commit: C, tree: T, snapshotUnchanged: true, classA: VERDICT }, extra = {} } = {}) => ({
  file: `R-${id}.json`, verified: { ok: true, problems: [] },
  doc: { provenance: { payload: { jobId: id, request: { requestedArgv: argv, workspace: cwd, adapter: null }, startedAt: start === null ? undefined : at(start), terminalAt: end === null ? undefined : at(end), timingsMs: waits, gate, ...extra } } },
});
const inv = (entries) => new Map(entries.map(([id, stage, files]) => [id, { stage, files }]));
function records() {
  nextPid = 200;
  return {
    launch: { schema: "rightkit.acceptance-launch/3", runId: "f".repeat(64), commit: C, kind: "compile", host: "fixture-host", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, gatePid: GATE, launcherPid: LAUNCHER, fingerprint: { gateCodeTree: "c".repeat(40), node: "v22.0.0", platform: "win32", tools: {} },
      lanes: 4, lanesSource: "this host's cores, the gate's own default; nothing is handed to the gate", limitMs: 60_000, limitsSha256: "c".repeat(64), snapshotsRoot: "D:\\snaps",
      baseline: { commit: P, out: "D:\\prev", runId: "e".repeat(64), exit: 0, stable: true }, previous: { runId: "e".repeat(64), fromLastRun: true, exit: 0 },
      startedAt: at(0), startMono: "0", endedAt: at(10), endMono: String(10_000_000_000n), exit: 0, signal: null, afterRun: { observedAt: at(10), locks: [], locksKnown: true, rootAlive: false } },
    witness: { kind: "windows-security", from: at(-2), to: at(12), newest: at(12), continuous: true, problems: [] },
    procs: [
      { pid: GATE, ppid: LAUNCHER, exe: "C:\\node\\node.exe", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C], cwd: null, start: ms(0), end: ms(10), exit: 0 },
      proc(0, GIT, ["rev-parse", "--verify", `${C}^{commit}`], { cwd: SNAP }),
      proc(1.0, RUSTFMT, ["--check", "--edition", "2021", "src/a.rs"], { end: 1.1, cwd: SNAP }),
      proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "b", "--locked"], { end: 8, cwd: `${SNAP}\\ws` }),
    ],
    receipts: [job("j1", ["clippy", "--profile", "release", "-p", "b", "--locked"])],
    verdictTimes: new Map([[VERDICT, ms(1.2)]]),
    inventory: inv([["rustfmt:2021", "A", ["src/a.rs", "src/b.rs"]], ["deny", "A", ["Cargo.lock"]], ["clippy:ws:a", "B", ["ws/a/src/lib.rs"]], ["clippy:ws:b", "B", ["ws/b/src/lib.rs"]]]),
    blobs: { "src/a.rs": "a1", "src/b.rs": "b1", "Cargo.lock": "l1", "ws/a/src/lib.rs": "x1", "ws/b/src/lib.rs": "y1" },
    priorBlobs: { "src/a.rs": "a0", "src/b.rs": "b1", "Cargo.lock": "l1", "ws/a/src/lib.rs": "x1", "ws/b/src/lib.rs": "y0" },
    prior: { failedRows: new Set() },
    readiness: new Map([["rustfmt", ms(1.0)]]),
    tree: { "ci/shards.json": { solo: [] }, "ws/Cargo.toml": '[workspace]\nmembers = ["a", "b"]\n' }, working: {},
    snapshotStat: { root: "D:\\snaps", dirs: { k1: { "src/a.rs": [10, ms(0.5)], "src/b.rs": [10, ms(0.5)] } } }, cacheStore: { dir: "D:\\store", written: [], removed: [] },
    diskOverride: {}, live: { exists: () => false, alive: () => false, locksIn: () => [] }, successor: null, verifierPinned: true, digests: {},
  };
}
// Gate-written text and values that a case puts beside the evidence, to show the judge never reads them: the judge is handed them as extra inputs and a read of any is recorded.
const decoy = (r, name, value) => { (r.decoys ??= {})[name] = value; };
const run = (r) => {
  const readWorking = () => { r.workingRead = true; return r.working; };
  const watched = (args) => (r.decoys ? new Proxy({ ...args, ...r.decoys }, { get(t, k, rcv) { if (Object.hasOwn(r.decoys, k)) r.decoyRead = k; return Reflect.get(t, k, rcv); } }) : args);
  return judgeRun(watched({
    lines: LINES, launch: r.launch, processes: r.procs, witness: r.witness, receipts: r.receipts, inventory: r.inventory, inventoryUncovered: r.inventoryUncovered ?? [], prior: r.prior, readiness: r.readiness, verdictTimes: r.verdictTimes,
    git: { tree: T, blobOf: (p) => r.blobs[p] ?? null, priorBlobOf: (p) => r.priorBlobs[p] ?? null, readWorking, readTree: (p) => { if (!(p in r.tree)) throw Object.assign(new Error(`ENOENT ${p}`), { code: "ENOENT" }); return structuredClone(r.tree[p]); } },
    live: r.live, successor: r.successor, verifierPinned: r.verifierPinned, digestOf: (f) => r.digests[f] ?? null,
    diskBlobs: (abs, rel) => (Object.hasOwn(r.diskOverride, rel) ? r.diskOverride[rel] : r.blobs[rel] ?? null), cacheStore: r.cacheStore, snapshotStat: r.snapshotStat,
  }));
};
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const byExe = (r, exe) => r.procs.find((p) => p.exe === exe);
// A stand-in for wevtutil: for a fixture object { oldest, newest, continuous, records, events: [{ id, time, data }] } it answers the three queries the exporter makes with the XML wevtutil
// writes (escaped text, one Event per record). Events not listed fill `records` out with other record ids; `continuous: false` leaves a hole in the numbering.
const xmlEsc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
const eventXml = (e, rec) => `<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Microsoft-Windows-Security-Auditing'/><EventID>${e.id}</EventID><EventRecordID>${rec}</EventRecordID><TimeCreated SystemTime='${e.time}'/></System><EventData>${Object.entries(e.data ?? {}).map(([k, v]) => (v === null ? `<Data Name='${k}'/>` : `<Data Name='${k}'>${xmlEsc(v)}</Data>`)).join("")}</EventData></Event>`;
const fake = (raw) => (cmd, args) => {
  const a = args.join(" ");
  if (a.includes("/c:1") && a.includes("/rd:false")) return eventXml({ id: 4624, time: raw.oldest }, 1);
  if (a.includes("/c:1") && a.includes("/rd:true")) return eventXml({ id: 4624, time: raw.newest }, 99999);
  const events = raw.events ?? []; const all = [...events]; while (all.length < (raw.records ?? events.length)) all.push({ id: 4703, time: raw.newest, data: {} });
  return all.map((e, i) => eventXml(e, 100 + i + (raw.continuous === false && i > 0 ? 5 : 0))).join("\r\n");
};

test("a run whose evidence meets every line passes ten; NN04 (not measured until Adrian rewords it) and NN05 (a pass needs evidence of the lookup, which none outside the gate gives) cannot", () => {
  const v = run(records());
  for (const id of twelve.filter((x) => x !== "NN04" && x !== "NN05")) assert.equal(verdictOf(v, id), "pass", `${id}: ${whyOf(v, id)}`);
  assert.equal(verdictOf(v, "NN04"), "not measured"); assert.equal(verdictOf(v, "NN05"), "not measured");
  assert.equal(v.pass, false, "a run with a line not measured is not a pass");
});

// ---- the frozen step-2 batch, each case's original fault in outside evidence (D:\scratch\loop-review\twelve-line-step2\final-batch-cases.md, sha256 70ff356d;
// batch 3: D:\scratch\loop-review\twelve-line-step3\batch-3-cases.md) -------------------------------------------------------------------------------
// Adrian, 2026-11-21: "Restate them". Each case keeps its author's id and expected verdict. Where the original fault lived only in a record the gate writes
// and has no form in evidence the gate does not write, the case is in NO_OUTSIDE_FORM with the reason, not silently dropped and not given a verdict it
// did not have.
const SPLIT_TREE = (r, crates) => { r.tree["ws/Cargo.toml"] = `[workspace]\nmembers = [${crates.map((c) => `"${c}"`).join(", ")}]\n`; };
const split = (r, ...argvs) => {
  r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.receipts = []; const crates = new Set();
  argvs.forEach((a, i) => { const argv = ["clippy", "--locked", ...a.split(" ")]; r.procs.push(proc(6, RUSTRIGHT, ["cargo", ...argv], { end: 8, cwd: `${SNAP}\\ws` })); r.receipts.push(job(`j${i + 1}`, argv)); for (const m of a.matchAll(/-p (\w+)/g)) crates.add(m[1]); });
  r.inventory = inv([...crates].map((c) => [`clippy:ws:${c}`, "B", [`ws/${c}/src/lib.rs`]]));
  for (const c of crates) { r.blobs[`ws/${c}/src/lib.rs`] = "new"; r.priorBlobs[`ws/${c}/src/lib.rs`] = "old"; }
  SPLIT_TREE(r, [...crates]);
};
const policy = (r, solo, working = null) => { if (solo === null) delete r.tree["ci/shards.json"]; else r.tree["ci/shards.json"] = { solo }; if (working) r.working = { "ci/shards.json": { solo: working } }; };
const unchangedB = (r) => { r.priorBlobs["ws/b/src/lib.rs"] = "y1"; };
const noBroker = (r) => { r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.receipts = []; };
const EXE = `${SNAP}\\ws\\target\\debug\\deps\\b-1.exe`;
const testJob = (r, exe, extra) => { r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.procs.push(proc(6, RUSTRIGHT, ["cargo", "test", "-p", "b", "--no-run"], { end: 8, cwd: `${SNAP}\\ws` }), proc(8.5, exe, [], { end: 9, cwd: SNAP })); r.receipts = [job("t1", ["test", "-p", "b", "--no-run"], { extra })]; };
// Both witnesses describe one call: the observed client and the signed job change together.
const same = (r, argv, cwd = `${SNAP}\\ws`) => { const c = r.procs.find((p) => p.exe === RUSTRIGHT); c.args = ["cargo", ...argv]; c.cwd = cwd; const p = r.receipts[0].doc.provenance.payload; p.request.requestedArgv = argv; p.request.workspace = cwd; };
// A second compile of another workspace whose client, signed start and submission all sit at startedAt (no admission, queue or bind wait), alongside the first job (6.0 to 8.0).
const gap9 = (r, startedAt, endedAt = 9.999) => { const argv = ["clippy", "--profile", "release", "-p", "x", "--locked"]; r.procs.push(proc(startedAt, RUSTRIGHT, ["cargo", ...argv], { end: endedAt, cwd: `${SNAP}\\other` })); r.receipts.push(job("j2", argv, { cwd: `${SNAP}\\other`, start: startedAt, end: endedAt, waits: { admission: 0, queue: 0, bind: 0 } })); };
const pair = (r, id, argv, cwd, startedAt, terminalAt) => { r.procs.push(proc(startedAt - 0.1, RUSTRIGHT, ["cargo", ...argv], { end: terminalAt, cwd })); r.receipts.push(job(id, argv, { cwd, start: startedAt, end: terminalAt })); };
const wrapped = (s, line, end = s + 0.1) => proc(s, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", line], { end, cwd: null });
const longRun = (r) => { r.launch.startedAt = at(-30); r.launch.startMono = "0"; r.launch.endMono = String(40_000_000_000n); r.witness.from = at(-32); r.procs[0].start = ms(-30); r.procs[1].start = ms(-30); r.procs[1].end = ms(-29.95); };
const FROZEN = [
  // NN01: only the selected commit is built.
  ["GCX-01-1", "NN01", "not measured", (r) => { r.receipts = []; }], // a broker compile with no signed job
  ["GCX-01-2", "NN01", "not measured", (r) => { r.diskOverride["src/a.rs"] = null; }], // Class B stays fully evidenced; only the bytes rustfmt was handed could not be read back after the run
  ["GCX-01-3", "NN01", "fail", (r) => { r.diskOverride["src/a.rs"] = "e".repeat(40); }], // the file rustfmt was handed holds other bytes than the selected commit's (restored: the original's fault has this outside form)
  ["GCX-01-4", "NN01", "fail", (r) => { r.receipts[0].doc.provenance.payload.gate.tree = "e".repeat(40); }], // signed for another tree
  ["gate 13", "NN01", "not measured", (r) => { delete r.receipts[0].doc.provenance.payload.gate.snapshotUnchanged; }], // nothing says the snapshot held after the compile
  // NN02: Class A finishes before anything goes to the broker; if it fails, nothing goes.
  ["GCX-02-1", "NN02", "fail", (r) => { r.procs.push(wrapped(1.05, '""C:\\Program Files\\RustRight\\rustright.cmd" cargo metadata --format-version 1"')); }], // a quoted wrapper-only send during Class A
  ["GCX-02-2", "NN02", "not measured", (r) => { unchangedB(r); noBroker(r); byExe(r, RUSTFMT).exit = 1; r.procs[0].end = null; r.procs.push({ pid: 999, ppid: 4242, exe: GIT, args: ["status"], cwd: null, start: ms(9), end: ms(10), exit: 0 }); }], // Class A failed, Class B was reused, an unrelated process closed at 10 s, and the gate process has no exit
  ["GCX-02-3", "NN02", "not measured", (r) => { byExe(r, RUSTRIGHT).start = ms(1.1); }], // the broker client starts at the very moment Class A ends
  ["gate 9", "NN02", "pass", (r) => { decoy(r, "gateLog", "Class A failed: cargo-deny exited 1"); decoy(r, "classAVerdict", { passed: false }); }], // a good run with misleading text in the gate's log and a failing verdict file beside it, never read (restored as the author wrote it)
  // NN03: Class A starts ready, independent work whenever an eligible lane is free.
  ["GCX-03-1", "NN03", "not measured", (r) => { r.launch.lanes = 1.5; }],
  ["GCX-03-2", "NN03", "not measured", (r) => { const f = byExe(r, RUSTFMT); f.end = f.start - 1; }],
  ["GCX-03-3", "NN03", "fail", (r) => { r.launch.lanes = 2; const f = byExe(r, RUSTFMT); f.start = ms(2.0); f.end = ms(2.1); }], // one ready row waits a second beside free capacity
  ["gate 4", "NN03", "fail", (r) => { decoy(r, "gateLanes", 1); r.blobs["Cargo.lock"] = "l2"; r.readiness.set("cargo-deny", ms(1.0)); const f = byExe(r, RUSTFMT); f.start = ms(1.0); f.end = ms(3.0); r.procs.push(proc(3.0, DENY, ["check"], { end: 5, cwd: SNAP })); }], // two programs ready together, the second held two seconds with four lanes free; a gate-reported lane count of one is present and never read (restored with its owed deny row)
  ["GCX-03-4", "NN03", "pass", (r) => { r.launch.lanes = 20; }], // the same row ready and started at once on twenty lanes
  // NN05: if lookup disagrees with the record the gate stops before sending work; otherwise the work that ran matches it.
  ["GCX-05-1", "NN05", "not measured", (r) => { r.inventory = inv([["rustfmt:2021", "A", ["src/a.rs", "src/b.rs"]], ["deny", "A", ["Cargo.lock"]]]); }], // Class B work observed with no Class B row known
  ["GCX-05-3", "NN05", "fail", (r) => { same(r, ["clippy", "--profile", "release", "-p", "b", "-p", "z", "--locked"]); }], // the observed call and its signed job both name an extra crate
  ["GCX-05-5", "NN05", "fail", (r) => { byExe(r, RUSTFMT).args.push("src/unplanned.rs"); r.procs.push(proc(1.2, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 1.3, cwd: SNAP })); }], // an unplanned operand and a tool no row plans, beside the valid work
  // NN06: a row whose inputs did not change is never run again, including one that failed before.
  ["GCX-06-1", "NN06", "fail", (r) => { r.blobs["src/a.rs"] = "a0"; }], // rustfmt's inputs are unchanged and rustfmt still started
  ["GCX-06-3", "NN06", "not measured", (r) => { r.prior = { failedRows: new Set(["clippy:ws:a"]) }; byExe(r, RUSTFMT).exit = 1; r.launch.exit = 1; }], // the run is red, but another row's program failed, so the reused failure's own outcome is not shown
  ["gate 1", "NN06", "fail", (r) => { r.procs.push(proc(1.7, DENY, ["check"], { end: 1.9, cwd: SNAP })); }], // deny is unchanged and its tool visibly started
  // NN07: a row whose inputs did change is always run again, and does only the work the change needs.
  ["GCX-07-1", "NN07", "fail", (r) => { r.blobs["src/b.rs"] = "b2"; }], // a.rs and b.rs changed; only a.rs reached rustfmt
  ["GCX-07-2", "NN07", "fail", (r) => { byExe(r, RUSTFMT).args = ["--check", "--edition", "2021"]; }],
  ["GCX-07-3", "NN07", "pass", (r) => { r.blobs["src/b.rs"] = "b2"; byExe(r, RUSTFMT).args.push("src/b.rs"); }],
  ["GCX-07-5", "NN07", "fail", (r) => { same(r, ["clippy", "--profile", "release", "-p", "ws", "--locked"]); }], // the observed compile selected the workspace label, not crate b
  ["GCX-07-6", "NN07", "fail", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; same(r, ["clippy", "--profile", "release", "-p", "b", "--locked"], `${SNAP}\\other`); }], // the observed compile ran in another workspace that also holds b
  ["gate 12", "NN07", "fail", (r) => { const id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.inventory.delete("clippy:ws:b"); r.inventory.set(id, { stage: "B", files: ["ws/b/src/lib.rs"] }); same(r, ["clippy", "--profile", "release", "--target", "x86_64-pc-windows-msvc", "-p", "b", "--locked"]); }], // the observed compile was for another target than the row's
  // NN08: nothing is started twice in one run.
  ["GCX-08-1", "NN08", "pass", (r) => { r.procs.push(proc(7, "/opt/Tool", ["probe"], { cwd: "/src" }), proc(8, "/opt/tool", ["probe"], { cwd: "/src" })); }],
  ["GCX-08-2", "NN08", "pass", (r) => { r.procs.push(proc(7, "/usr/bin/git", ["status"], { cwd: "/src/A" }), proc(8, "/usr/bin/git", ["status"], { cwd: "/src/a" })); }],
  ["GCX-08-3", "NN08", "fail", (r) => { const nested = proc(7, "C:\\node\\node.exe", ["nested-gate.mjs"], { end: 9 }); r.procs.push(nested, proc(8, RUSTFMT, ["--check", "--edition", "2021", "src/a.rs"], { ppid: nested.pid, cwd: SNAP })); }], // the same call by two writers
  ["gate 5", "NN08", "fail", (r) => { const g = r.procs[1]; const w = wrapped(0.5, `"${g.exe}" ${g.args.join(" ")}`, 0.6); w.cwd = g.cwd; r.procs.push(w, proc(0.52, g.exe, [...g.args], { ppid: w.pid, cwd: g.cwd, end: 0.55 })); }], // the direct call run again through cmd /s: the shell's own child, in the same known folder, is the second start
  ["gate 6", "NN08", "fail", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, [...g.args], { cwd: `${g.cwd}\\` })); }], // the same folder spelled with a trailing separator
  ["gate 7", "NN08", "pass", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, ["-c", "core.fsmonitor=false", ...g.args], { cwd: g.cwd })); }],
  // NN09: compatible compiles combined per workspace and stage; every split has a reason the check can verify; independent groups run together.
  ["GCX-09-1", "NN09", "fail", (r) => { split(r, "-p b -p c", "-p c"); policy(r, ["c"]); }],
  ["GCX-09-2", "NN09", "fail", (r) => { split(r, "-p c --target x", "-p c --target x"); policy(r, ["c"]); }], // moved by the EGM ruling on the HeardRight 2026-11-10 finding: one command per target is the split Cargo's feature unification requires, so the failing form is a solo crate sent twice for the same target
  ["GCX-09-3", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, [], ["c"]); }], // the run's tree names no solo crate; a later checkout does
  ["GCX-09-4", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, ["c"], []); }], // the run's tree names c solo; a later checkout withdraws it
  ["GCX-09-5", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null, ["c"]); }], // the run's tree has no policy; a later checkout would justify the split
  ["GCX-09-6", "NN09", "not measured", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; gap9(r, 7.999); }], // the second compile is submitted at 7.999, one ms before the first ends at 8.0: one ms of overlap, no word on readiness or capacity (restored: the submission delay is the original's, with no admission or bind wait to blur it)
  ["GCX-09-7", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json"] = JSON.stringify({ bundle: { externalBin: ["binaries/c-sidecar"] } }); }],
  ["GCX-09-8", "NN09", "not measured", (r) => { decoy(r, "gateLog", "sidecar-bound: c (printed by the gate)"); decoy(r, "sidecarClaim", { c: "sidecar-bound" }); split(r, "-p b", "-p c"); policy(r, null); }], // a competing printed sidecar reason is present and never read; the tree has no policy (restored)
  ["GCX-09-9", "NN09", "fail", (r) => { split(r, "-p b", "-p b"); policy(r, ["b"]); }],
  ["gate 11", "NN09", "fail", (r) => { r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "sub"]\n'; r.tree["ws/sub/Cargo.toml"] = '[package]\nname = "c"\n'; r.procs.push(proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "c", "--locked"], { end: 8, cwd: `${SNAP}\\ws\\sub` })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "c", "--locked"], { cwd: `${SNAP}\\ws\\sub` })); }],
  // NN10: tests are compiled by the broker and run by the gate.
  ["GCX-10-1", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.receipts = []; }], // a test request and the program's launch with no signed job at all
  ["GCX-10-2", "NN10", "not measured", (r) => { decoy(r, "gateReceipt", { testFile: "b-1.exe" }); testJob(r, EXE, {}); }], // the signed job exists and carries no manifest; a file name in the gate's own receipt is present and never read (restored)
  // NN11: the run finishes inside its time limit.
  ["GCX-11-1", "NN11", "not measured", (r) => { r.launch.endMono = null; r.launch.endedAt = null; r.launch.exit = null; }],
  ["GCX-11-2", "NN11", "fail", (r) => { r.launch.limitMs = 9999; }],
  ["GCX-11-3", "NN11", "pass", (r) => { r.launch.limitMs = 10000; }],
  ["gate 10", "NN11", "fail", (r) => { decoy(r, "gateLog", "first stamp 06:00:00"); longRun(r); r.launch.limitMs = 15_000; }], // the launcher began 30 s before the log's first stamp and the run took 40 s against a limit of 15 s (restored on one coherent timeline)
  // NN12: the run ends clean, even if killed: the next ordinary run starts safely by itself.
  ["gate 2", "NN12", "not measured", (r) => { decoy(r, "gateLiveList", []); r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [] }; }], // the operating system's listing is empty while the probe finds the gate alive; a pid list the gate wrote is present and never read (restored)
  ["gate 3", "NN12", "not measured", (r) => { decoy(r, "gateLiveList", [LAUNCHER]); r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: LAUNCHER }, { pid: 4 }] }; }], // the listing omits the gate process the probe finds alive (restored)
  ["GCX-12-1", "NN12", "not measured", (r) => { r.launch.exit = 137; }],
  ["GCX-12-2", "NN12", "fail", (r) => { r.launch.exit = 137; r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: GATE }, { pid: LAUNCHER }] }; }], // the killed run's own gate process (pid 100) is still alive, and the operating system's listing shows it (restored: the original's actor and observation)
];
// The specific reason each case's verdict must give (so it cannot be right for an unrelated one).
const REASONS = {
  "GCX-01-1": /broker client compile start\(s\) and 0 signed compile job/, "GCX-01-2": /ran in a folder the process record does not keep/, "GCX-01-3": /ran outside the selected snapshot/, "GCX-01-4": /built tree eeeeeeeeeeee, not the selected commit/, "gate 13": /does not say whether the snapshot stayed unchanged/,
  "GCX-02-1": /broker client started \(rustright cargo metadata\) before Class A finished/, "GCX-02-2": /has no recorded exit/, "GCX-02-3": /same moment Class A ended/, "GCX-02-4": /admitted on a Class A verdict .* written before this run began/, "gate 9": /Class A passed/,
  "GCX-03-1": /no lane count/, "GCX-03-2": /ending before it started/, "GCX-03-3": /was ready at .* and started 1000 ms later, \d+ ms of it with a lane free of 2/, "GCX-03-4": /none waited while a lane of 20 was free/, "gate 4": /cargo-deny was ready at .* and started 2000 ms later, \d+ ms of it with a lane free of 4/, "gate 10": /^40\.0 s, limit 15\.0 s/,
  "GCX-05-1": /no Class B row is known/, "GCX-05-2": /clippy:ws:b was owed a rerun and no program or signed job shows it/, "GCX-05-3": /also builds z, which no owed row needs/, "GCX-05-5": /rustfmt was handed src\/unplanned\.rs/, "gate 8": /did work although no file it reads changed/,
  "GCX-06-1": /rustfmt:2021 is unchanged and rustfmt started/, "GCX-06-3": /run's exit is not known/, "gate 1": /deny is unchanged and cargo-deny started/,
  "GCX-07-1": /src\/b\.rs changed and was never handed to rustfmt/, "GCX-07-2": /src\/a\.rs changed and was never handed to rustfmt/, "GCX-07-3": /each with only the work its change needs/, "GCX-07-4": /receipt does not verify/, "GCX-07-5": /no signed broker job built it/, "GCX-07-6": /no signed broker job built it/, "gate 12": /@x86_64-unknown-linux-gnu changed .* no signed broker job built it/,
  "GCX-08-1": /no two alike/, "GCX-08-2": /no two alike/, "GCX-08-3": /2x rustfmt/, "gate 5": /2x git rev-parse/, "gate 6": /2x git rev-parse/, "gate 7": /no two alike/,
  "GCX-09-1": /must build alone \(c\) were built together/, "GCX-09-2": /solo crate was sent as several commands/, "GCX-09-3": /sent as separate commands, and only 0 of them/, "GCX-09-4": /c solo per the run's ci\/shards\.json/, "GCX-09-5": /ci\/shards\.json at the run's tree cannot be read/, "GCX-09-6": /submitted \d+ ms after the first while it ran/,
  "GCX-09-7": /c sidecar-bound per ws\/c\/tauri\.conf\.json/, "GCX-09-8": /ci\/shards\.json at the run's tree cannot be read/, "GCX-09-9": /b built more than once/, "gate 11": /sent as separate commands, and only 0 of them/,
  "GCX-10-1": /test request\(s\) went to the broker and 0 signed test job/, "GCX-10-2": /cannot be verified: no broker key is pinned/,
  "GCX-11-1": /never saw the run end/, "GCX-11-2": /limit 10\.0 s \(pinned at launch/, "GCX-11-3": /limit 10\.0 s pinned at launch/,
  "GCX-12-1": /needs that run's record/, "GCX-12-2": /still alive: node 100/, "gate 2": /no operating-system record/, "gate 3": /no start of the gate process/,
};
for (const id of ["gate 2", "gate 3", "gate 4", "gate 8", "gate 9", "GCX-01-3", "GCX-02-4", "GCX-05-2", "GCX-09-8", "GCX-10-2"]) delete REASONS[id];
Object.assign(REASONS, {
  "GCX-01-2": /no broker job names the snapshot folder/, "GCX-02-2": /gate process the launcher started has no recorded exit/, "GCX-02-3": /started at the same moment Class A ended/,
  "GCX-05-3": /also builds z, which no owed row needs/, "GCX-05-5": /^(?=[\s\S]*rustfmt was handed src\/unplanned\.rs)(?=[\s\S]*deny did work although no file it reads changed)/, "GCX-06-3": /another row also failed/,
  "GCX-07-4": /not shown to be this run's/, "GCX-07-5": /clippy:ws:b changed .* no signed broker job built it/, "GCX-07-6": /clippy:ws:b changed .* no signed broker job built it/, "gate 12": /@x86_64-unknown-linux-gnu changed .* no signed broker job built it/,
  "gate 5": /2x git rev-parse/, "GCX-09-6": /submitted \d+ ms after the first while it ran/, "GCX-12-2": /outlived the gate's exit \(first: child \d+\)/,
  "GCX-01-3": /was handed src\/a\.rs, whose bytes on disk are not the selected commit's/, "gate 2": /process listing is empty/, "gate 3": /process listing omits node 100/, "gate 4": /cargo-deny was ready at .* and started 2000 ms later, \d+ ms of it with a lane free of 4/, "gate 10": /^40\.0 s, limit 15\.0 s/,
  "gate 9": /Class A passed/, "GCX-09-8": /ci\/shards\.json at the run's tree cannot be read/, "GCX-10-2": /carries no manifest of test programs/,
});
Object.assign(REASONS, { "GCX-01-2": /was handed src\/a\.rs and its bytes on disk after the run were not read/, "GCX-05-5": /^(?=[\s\S]*rustfmt was handed src\/unplanned\.rs)(?=[\s\S]*shellcheck ran and no known row plans it)/,
  "GCX-09-6": /broker job j2 was submitted 1999 ms after the first while it ran/, "GCX-12-2": /1 program\(s\) the run started are still alive: node 100/ });
delete REASONS["GCX-07-4"]; // superseded below: its original fault was a missing gate-written coverage report that NN07 does not read
const EXTRA_TWINS = [
  ["gate 2 twin", "NN12", "fail", (r) => { r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: GATE }] }; }], // the same live gate with the listing showing it: the gate is still running
  ["gate 3 twin", "NN12", "fail", (r) => { r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: LAUNCHER }, { pid: GATE }] }; }], // the listing that includes the gate process
  ["GCX-01-2 twin", "NN01", "pass", () => {}], // the same run with every file rustfmt was handed read back after the run
  ["GCX-02-2 twin", "NN02", "pass", (r) => { unchangedB(r); noBroker(r); byExe(r, RUSTFMT).exit = 1; r.procs.push({ pid: 999, ppid: 4242, exe: GIT, args: ["status"], cwd: null, start: ms(9), end: ms(10), exit: 0 }); }], // the same, with the gate's exit recorded: Class A failed and nothing went to the broker
  ["GCX-02-3 twin", "NN02", "pass", (r) => { byExe(r, RUSTRIGHT).start = ms(1.6); }], // the same client a half second after Class A ended
  ["GCX-06-3 twin", "NN06", "pass", (r) => { r.prior = { failedRows: new Set(["clippy:ws:a"]) }; r.launch.exit = 1; }], // the same red run with no other row failing: the reused failure kept it red
  ["GCX-07-4 twin", "NN07", "pass", () => {}], // the signed receipt names its command and verifies
  ["GCX-09-6 twin", "NN09", "pass", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; const argv = ["clippy", "--profile", "release", "-p", "x", "--locked"]; r.procs.push(proc(6.0, RUSTRIGHT, ["cargo", ...argv], { end: 9.999, cwd: `${SNAP}\\other` })); r.receipts.push(job("j2", argv, { cwd: `${SNAP}\\other`, start: 8.0, end: 9.999, waits: { admission: 0, queue: 2000, bind: 0 } })); }], // the same second compile, with the broker's own signed queue wait of 2 s: submitted with the first, held by the broker
  ["GCX-07-5 twin", "NN07", "pass", (r) => { same(r, ["clippy", "--profile", "release", "-p", "b", "--locked"]); }], // the observed compile selected crate b
  ["GCX-07-6 twin", "NN07", "pass", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; same(r, ["clippy", "--profile", "release", "-p", "b", "--locked"], `${SNAP}\\ws`); }], // the same compile in the owed workspace
  ["gate 12 twin", "NN07", "pass", (r) => { const id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.inventory.delete("clippy:ws:b"); r.inventory.set(id, { stage: "B", files: ["ws/b/src/lib.rs"] }); same(r, ["clippy", "--profile", "release", "--target", "x86_64-unknown-linux-gnu", "-p", "b", "--locked"]); }], // the compile for the row's own target
  ["gate 10 twin", "NN11", "pass", (r) => { longRun(r); r.launch.limitMs = 60_000; }], // the same 40 s run inside a limit of 60 s
  ["GCX-08-1 twin", "NN08", "fail", (r) => { r.procs.push(proc(7, "/opt/tool", ["probe"], { cwd: "/src" }), proc(8, "/opt/tool", ["probe"], { cwd: "/src" })); }], // the same Unix program twice in one folder
  ["GCX-08-2 twin", "NN08", "fail", (r) => { r.procs.push(proc(7, "/usr/bin/git", ["status"], { cwd: "/src/a" }), proc(8, "/usr/bin/git", ["status"], { cwd: "/src/a" })); }], // the same Unix folder twice
  ["gate 5 control", "NN08", "not measured", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", `"${g.exe}" ${g.args.join(" ")}`], { end: 0.6, cwd: "D:\\snap\\other" })); }], // the same wrapper written in another known folder: not the same call, and no child shows what it ran
  ["gate 9 twin", "NN02", "fail", (r) => { r.procs.push(proc(9, DENY, ["check"], { exit: 1, cwd: SNAP })); }], // the same failing tool, in this run's snapshot
  ["GCX-09-7 twin", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, []); }], // the same split with no sidecar mark in the tree
];
const TWINS = [["GCX-03-4", "GCX-03-3", "fail"], ["GCX-07-3", "GCX-07-1", "fail"], ["GCX-09-4", "GCX-09-3", "fail"], ["GCX-11-3", "GCX-11-2", "fail"], ["GCX-09-7", "GCX-09-7 twin", "fail"], ["GCX-08-1", "GCX-08-1 twin", "fail"], ["GCX-08-2", "GCX-08-2 twin", "fail"], ["gate 7", "gate 6", "fail"], ["GCX-02-2 twin", "GCX-02-2", "not measured"], ["GCX-02-3 twin", "GCX-02-3", "not measured"], ["GCX-06-3 twin", "GCX-06-3", "not measured"], ["GCX-07-4 twin", "GCX-07-4 (receipt not credited)", "not measured"], ["GCX-09-6 twin", "GCX-09-6", "not measured"], ["GCX-12-1", "GCX-12-2", "fail"], ["GCX-07-5 twin", "GCX-07-5", "fail"], ["GCX-07-6 twin", "GCX-07-6", "fail"], ["gate 12 twin", "gate 12", "fail"], ["gate 10 twin", "gate 10", "fail"], ["gate 5 control", "gate 5", "fail"], ["gate 2 twin", "gate 2", "not measured"], ["gate 3 twin", "gate 3", "not measured"], ["GCX-01-2 twin", "GCX-01-2", "not measured"]];
// Cases whose original fault lived only in a record the gate writes (its forecast, its lookup keys, its decision values) and has no form in evidence the gate
// does not write. Each is reported to Adrian and chat gate; none is given a verdict it did not have.
const NO_OUTSIDE_FORM = [
  ["GCX-04-1", "NN04", "fail", "rustfmt ran before the forecast froze: the order of two events inside the gate"],
  ["GCX-04-2", "NN04", "not measured", "a tie between forecast freeze and first lookup: two events inside the gate"],
  ["GCX-04-3", "NN04", "fail", "a Class A forecast that names itself Class B: a field of a gate-written file"],
  ["gate 14", "NN04", "fail", "the Class B forecast frozen after a broker compile ended: the order of two gate events"],
  ["GCX-05-4", "NN05", "fail", "three unknown decision values in the gate's forecast, lookup and coverage records"],
  ["GCX-06-2", "NN06", "not measured", "the lookup record carries no key: a field of a gate-written record"],
  ["gate 15", "NN06", "not measured", "the lookup record carries no key: a field of a gate-written record"],
  ["gate 8", "NN05", "fail", "the gate's stop line naming clippy:ws:bb for clippy:ws:b: only the gate's own words name a row"],
  ["GCX-02-4", "NN02", "fail", "a Class A verdict belonging to another run: the verdict file is written by the gate, and NN02 no longer reads it"],
  ["GCX-05-2", "NN05", "fail", "a claim recorded as dispatched and run in the gate's decision: gate-written"],
  ["GCX-07-4", "NN07", "not measured", "Class B coverage report absent while the compile and its workspace stay: a gate-written record NN07 does not read; with the compile and workspace intact the outside judge passes (EGM, CV-6 F-15 restoration)"],
];
// What replaces a superseded original, under a new id: its former body, kept as its own case with its specific reason.
const SUBSTITUTES = [
  ["gate 2 (record absent)", "NN12", "not measured", (r) => { r.witness = null; }],
  ["gate 3 (gate process absent)", "NN12", "not measured", (r) => { r.procs = r.procs.filter((p) => p.pid !== GATE); }],
  ["gate 8 (unchanged row built)", "NN05", "fail", (r) => { unchangedB(r); }],
  ["GCX-01-3 (other folder)", "NN01", "fail", (r) => { byExe(r, RUSTFMT).cwd = "D:\\snap\\other"; }],
  ["GCX-05-2 (owed, nothing seen)", "NN05", "fail", (r) => { noBroker(r); }],
  ["GCX-10-2 (no key pinned)", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE }] }); r.verifierPinned = false; r.receipts[0].verified = { ok: false, problems: ["no broker key is pinned"] }; }],
  ["GCX-01-2 (windows shape)", "NN01", "not measured", (r) => { byExe(r, RUSTFMT).cwd = null; }],
  ["GCX-07-4 (receipt not credited)", "NN07", "not measured", (r) => { r.receipts[0].verified = { ok: false, problems: ["signature does not match"] }; }], // the command and workspace stay; only the broker's attestation is gone
  ["GCX-07-4 (report absent)", "NN07", "pass", (r) => { decoy(r, "gateCoverageB", undefined); }], // the original's missing coverage report, present as an absent decoy that is never read: it changes nothing
];
test("the frozen step-2 batch: all 63 cases accounted for, each run in outside evidence with its author's id and verdict or printed as superseded", async (t) => {
  assert.equal(FROZEN.length + NO_OUTSIDE_FORM.length, 63);
  t.diagnostic(`executable checks: ${FROZEN.length} originals + ${SUBSTITUTES.length} substitutes under new ids; superseded originals printed: ${NO_OUTSIDE_FORM.length}`);
  assert.equal(new Set([...FROZEN, ...NO_OUTSIDE_FORM].map(([id]) => id)).size, 63, "every id once");
  for (const [id, line, verdict, fn] of FROZEN) {
    await t.test(`${id} ${line}: expected ${verdict}`, (st) => { const r = alter(fn); const v = run(r); st.diagnostic(`${id} ${line}: got ${verdictOf(v, line)}: ${whyOf(v, line).slice(0, 200)}`); assert.equal(verdictOf(v, line), verdict, `${id} ${line}: ${whyOf(v, line)}`); assert.match(whyOf(v, line), REASONS[id], `${id} gives the verdict for another reason than the case names`); assert.equal(r.workingRead, undefined, "the mutable checkout is never read"); assert.equal(r.decoyRead, undefined, `${id}: the judge read a gate-written input (${r.decoyRead}) that must never be read`); if (r.decoys) assert.ok(Object.keys(r.decoys).length > 0, "the case carries its misleading input"); });
  }
  for (const [id, line, verdict, why] of NO_OUTSIDE_FORM) await t.test(`${id} ${line}: superseded: ${why}`, (st) => { st.diagnostic(`${id} ${line} (was expected ${verdict}): superseded: ${why}`); });
  // A passing case is paired with a case that differs from it only in the decisive fact and gets the other verdict.
  for (const [id, twin, expected] of TWINS) await t.test(`${id} twin ${twin}: differs only in the decisive fact`, () => { const find = (x) => FROZEN.find(([y]) => y === x) ?? EXTRA_TWINS.find(([y]) => y === x) ?? SUBSTITUTES.find(([y]) => y === x); const a = find(id); const b = find(twin); const va = run(alter(a[3])); const vb = run(alter(b[3])); assert.notEqual(verdictOf(va, a[1]), verdictOf(vb, b[1]), "the twin must give the other verdict"); assert.equal(verdictOf(va, a[1]), a[2], `${id}: ${whyOf(va, a[1])}`); assert.equal(verdictOf(vb, b[1]), expected, `${twin}: ${whyOf(vb, b[1])}`); });
});

// ---- batch 3: controls that differ from a case only in its decisive fact ---------------------------------------------------------------------------
test("batch 3 controls: each fails or passes for the stated reason, and its twin does the opposite", () => {
  // NN03: the same single row, started at once, passes; delayed, it fails with that reason.
  expect(alter((r) => { r.launch.lanes = 2; const f = byExe(r, RUSTFMT); f.start = ms(2.0); f.end = ms(2.1); }), "NN03", "fail", /ready at .* and started 1000 ms later/);
  // NN05: b unchanged and built -> fail; the same run with b built only when it changed passes (base).
  expect(alter((r) => { unchangedB(r); }), "NN05", "fail", /did work although no file it reads changed/);
  // NN09: a policy only in the later checkout never justifies the split; the run's tree decides, and a later checkout is never read.
  const r = alter((x) => { split(x, "-p b", "-p c"); policy(x, [], ["c"]); }); expect(r, "NN09", "fail", /sent as separate commands/); assert.equal(r.workingRead, undefined);
  // NN02: the same wrapper-only quoted send, after Class A ended, is a send that went after Class A (no failure of this line)
  { const v = run(alter((x) => { x.procs.push(wrapped(5, '""C:\\Program Files\\RustRight\\rustright.cmd" cargo metadata --format-version 1"')); })); assert.notEqual(verdictOf(v, "NN02"), "fail", "a send after Class A ended is not a send before it"); assert.match(whyOf(v, "NN02"), /no receipt cannot be placed/); }
  // NN11: the same gate starting at the launcher's own time stays inside the limit
  expect(alter((x) => { x.launch.limitMs = 15_000; }), "NN11", "pass");
  // NN06: a reused failure keeps the run red: an unchanged row that failed before and a run that exited 0 fail; exiting 1 passes
  expect(alter((x) => { x.prior = { failedRows: new Set(["clippy:ws:a"]) }; }), "NN06", "fail", /failed in the previous launched run and is unchanged, and the run exited 0/);
  expect(alter((x) => { x.prior = { failedRows: new Set(["clippy:ws:a"]) }; x.launch.exit = 1; }), "NN06", "pass");
  // NN12: a surviving owned program fails, none passes
  expect(alter((x) => { x.live = { exists: () => false, alive: (pid) => pid === byExe(x, RUSTFMT).pid, locksIn: () => [] }; }), "NN12", "fail", /still alive/);
  // F-9: a reused failure counts as a Class A failure, and nothing may go to the broker after it
  expect(alter((x) => { x.prior = { failedRows: new Set(["clippy:ws:a", "deny"]) }; }), "NN02", "fail", /Class A failed/);
  expect(alter((x) => { x.prior = null; }), "NN06", "not measured", /previous launched run's evidence is not available/);
});

// ---- the evidence readers ---------------------------------------------------------------------------------------------------------------------------
test("a Windows command line is read as its program reads it", () => {
  assert.deepEqual(parseCommandLine('"C:\\Program Files\\Git\\cmd\\git.exe" rev-parse --verify a^{commit}'), ["C:\\Program Files\\Git\\cmd\\git.exe", "rev-parse", "--verify", "a^{commit}"]);
  assert.deepEqual(parseCommandLine('x "a b" c\\\\"d e" "f\\"g"'), ["x", "a b", 'c\\d e', 'f"g']);
  assert.deepEqual(parseCommandLine('x ""'), ["x", ""]);
});
test("Windows start and exit events become programs, an exit joined to the latest start of its pid", () => {
  const ps = processesFromWindowsEvents([
    { id: 4688, time: at(0), pid: 10, ppid: 1, exe: "C:\\a.exe", commandLine: '"C:\\a.exe" x' }, { id: 4689, time: at(1), pid: 10, exit: 0 },
    { id: 4688, time: at(2), pid: 10, ppid: 1, exe: "C:\\b.exe", commandLine: "b y" }, { id: 4689, time: at(3), pid: 10, exit: 3 },
  ]);
  assert.deepEqual(ps.map((p) => [p.exe, p.args, p.end - p.start, p.exit, p.cwd]), [["C:\\a.exe", ["x"], 1000, 0, null], ["C:\\b.exe", ["y"], 1000, 3, null]]);
});
test("the process record is whole or the lines say it is not: the log's newest event, the gate's own exit, the tree from the launcher", () => {
  expect(alter((r) => { r.witness.newest = at(5); }), "NN08", "not measured", /newest event is older/);
  expect(alter((r) => { delete r.witness.newest; }), "NN08", "not measured", /how far the Security log had advanced/);
  expect(alter((r) => { r.procs[0].ppid = 77; }), "NN08", "not measured", /no start of the gate process/);
  expect(alter((r) => { r.procs.push({ ...proc(5, GIT, ["status"]), ppid: 4242 }, proc(5.5, GIT, ["status"])); }), "NN08", "pass");
  expect(alter((r) => { byExe(r, RUSTFMT).end = null; }), "NN08", "not measured", /no recorded exit/);
  expect(alter((r) => { r.procs.push({ pid: null, ppid: GATE, exe: GIT, args: [], cwd: null, start: ms(3), end: ms(3.1), exit: 0 }); }), "NN08", "not measured", /no usable process id/);
});
test("a receipt that does not verify is never credited: its job is a gap, and a verified twin still counts", () => {
  expect(alter((r) => { r.receipts[0].verified = { ok: false, problems: ["bad signature"] }; }), "NN05", "not measured", /does not verify/);
  expect(alter((r) => { r.verifierPinned = false; r.receipts[0].verified = { ok: false, problems: ["no broker key is pinned"] }; }), "NN09", "not measured", /cannot be verified/);
});
test("no gate-written input is read: a history, a cache entry or the gate's output changes no verdict", () => {
  const src = fs.readFileSync(new URL("./acceptance-lines.mjs", import.meta.url), "utf8");
  for (const word of ["gateOutput", "cacheWrites", "cacheBefore", "history.json", "gate.log", "RK_CACHE_DIVERGENCE"]) assert.equal(src.includes(word), false, `${word} is a gate-written input and the judge must not read it`);
});
test("the launcher's lock: a live holder refuses, a dead one is taken over by exactly one successor and named", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-launch-"));
  const lock = path.join(dir, "l");
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "r", pid: process.pid })}\n`);
  assert.match(takeLaunchLock(lock, { runId: "s", pid: 1 }).refused, /holds/);
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "k", pid: 999999, out: "o" })}\n`);
  const first = takeLaunchLock(lock, { runId: "s", pid: process.pid });
  assert.deepEqual(first.previous, { runId: "k", out: "o", pid: 999999, reclaimed: true });
  // The same dead holder cannot be taken twice: the lock is now live (ours), so a second launcher is refused.
  assert.match(takeLaunchLock(lock, { runId: "t", pid: 1 }, { isAlive: () => true }).refused, /holds/);
  // A takeover racing the first: the lock was already moved aside, so a rename of a missing lock is refused, not taken.
  const ghost = path.join(dir, "ghost"); fs.writeFileSync(ghost, `${JSON.stringify({ runId: "d", pid: 999998 })}\n`);
  const fsRace = { ...fs, renameSync: (a, b) => { fs.rmSync(a); return fs.renameSync(a, b); } };
  assert.match(takeLaunchLock(ghost, { runId: "u", pid: process.pid }, { fsApi: fsRace }).refused, /taken over before this launcher moved it/);
});
test("the launcher derives the run kind from the invocation, and a label that disagrees is refused before anything starts", async () => {
  assert.equal(kindOf(["--to", "compile"]), "compile"); assert.equal(kindOf([]), "check");
  await assert.rejects(() => launch({ out: path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-kind-")), "run"), commit: "a".repeat(40), kind: "check", receiptsDir: os.tmpdir(), gateArgs: ["--to", "compile"] }), /does not match the stage/);
  expect(alter((r) => { r.launch.kind = "check"; }), "NN11", "not measured", /does not match the stage/);
});
test("the fingerprint of what git cannot show: equal only when every field is present and equal", () => {
  const a = { gateCodeTree: "t1", node: "v1", platform: "win32", tools: { rustfmt: "r", cargo: "c", "cargo-deny": "d", pnpm: "p", git: "g" } };
  assert.equal(sameFingerprint(a, structuredClone(a)), true);
  assert.equal(sameFingerprint(a, { ...a, tools: { ...a.tools, cargo: "c2" } }), false);
  assert.equal(sameFingerprint(a, { ...a, tools: { ...a.tools, pnpm: null } }), false);
  expect(alter((r) => { r.launch.baseline.stable = false; }), "NN06", "not measured", /baseline's/);
  expect(alter((r) => { r.launch.baseline = null; }), "NN07", "not measured", /no earlier launched run/);
});
test("the Security log export: its events become the process record, and a log that does not reach the run is said", () => {
  const raw = { oldest: at(-100), newest: at(3), continuous: true, records: 3, events: [{ id: 4688, time: at(0), data: { NewProcessId: "0x64", ProcessId: "0x32", NewProcessName: "C:\\node.exe", CommandLine: "node gate.mjs" } }, { id: 4689, time: at(1), data: { ProcessId: "0x64", ProcessName: "C:\\node.exe", Status: "0x0" } }] };
  const x = exportWindowsProcesses(at(-1), at(2), { run: fake(raw) });
  assert.deepEqual(x.problems, []); assert.equal(x.newest, at(3)); assert.equal(x.continuous, true);
  // A break in the record ids of the window (a lost start or exit) is a problem, and the record is then not continuous.
  { const broken = exportWindowsProcesses(at(-1), at(2), { run: fake({ ...raw, continuous: false, records: 7 }) }); assert.equal(broken.continuous, false); assert.match(broken.problems.join(), /record ids have a break/); }
  assert.deepEqual(x.events.map((e) => [e.id, e.pid, e.ppid ?? null]), [[4688, 100, 50], [4689, 100, null]]);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: fake({ ...raw, oldest: at(1) }) }).problems[0], /oldest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: fake({ ...raw, newest: at(-50) }) }).problems.join(), /newest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => { throw Object.assign(new Error("x"), { stderr: "Access is denied." }); } }).problems[0], /Access is denied/);
  // 2026-11-25: a process the kernel makes with no image file (vmmemWSL) never carries a command line; it is not a sign the setting is off. A program
  // started from a file with no command line is.
  const start = (pid, name, line) => ({ id: 4688, time: at(0), data: { NewProcessId: pid, ProcessId: "0x32", NewProcessName: name, ...(line ? { CommandLine: line } : {}) } });
  assert.deepEqual(exportWindowsProcesses(at(-1), at(2), { run: fake({ ...raw, events: [...raw.events, start("0x99", "vmmemWSL", null)] }) }).problems, []);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: fake({ ...raw, events: [...raw.events, start("0x98", "C:\\Windows\\System32\\cmd.exe", null)] }) }).problems.join(), /1 start event\(s\) of programs carry no command line .*cmd\.exe/);
});
test("NN10: a verified compile-only test build whose program the gate ran, as the broker built it, passes", () => {
  const ok = (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.digests[EXE] = { sha256: "h1", mtimeMs: ms(8.2) }; };
  expect(alter(ok), "NN10", "not measured", /image of b-1\.exe at the moment it ran is not shown/);
  expect(alter((r) => { ok(r); r.digests[EXE] = { sha256: "h2", mtimeMs: ms(8.2) }; }), "NN10", "fail", /not the program the broker built/);
  // F-19: the launch is after the build, credited to one receipt, and the image unchanged since before it ran
  expect(alter((r) => { ok(r); byExe(r, EXE).start = ms(7.5); byExe(r, EXE).end = ms(7.6); }), "NN10", "fail", /started before the broker finished building it/);
  expect(alter((r) => { ok(r); r.procs.push(proc(6, RUSTRIGHT, ["cargo", "test", "-p", "c", "--no-run"], { end: 8, cwd: `${SNAP}\\ws` })); r.receipts.push(job("t2", ["test", "-p", "c", "--no-run"], { extra: { testExecutables: [{ path: EXE, sha256: "h1" }] } })); }), "NN10", "fail", /also listed by broker job t1, and its launch is credited to that receipt only/);
  expect(alter((r) => { ok(r); r.digests[EXE] = { sha256: "h1", mtimeMs: ms(9) }; }), "NN10", "fail", /written after the gate started it/);
  expect(alter((r) => { ok(r); r.digests[EXE] = { sha256: "h1" }; }), "NN10", "not measured", /when .* was last written is not shown/);
  expect(alter((r) => { ok(r); r.procs = r.procs.filter((p) => p.exe !== EXE); }), "NN10", "fail", /never ran it/);
  expect(alter((r) => { ok(r); delete r.receipts[0].doc.provenance.payload.testExecutables; }), "NN10", "not measured", /carries no manifest of test programs/);
  expect(alter((r) => { ok(r); r.receipts[0].doc.provenance.payload.testExecutables = []; }), "NN10", "not measured", /no test program/);
  expect(alter((r) => { r.procs.push(proc(8, "C:\\cargo\\bin\\cargo.exe", ["test", "-p", "b"])); }), "NN10", "fail", /outside the broker/);
});
test("NN12: a killed run passes only on the next ordinary run that names it, started after it, and ended 0; the lock is looked for in the launcher's snapshot root", () => {
  const killed = (r) => { r.launch.exit = 137; };
  const next = (r, over = {}) => ({ launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), fingerprint: r.launch.fingerprint, snapshotsRoot: "D:\\snaps", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, ...over } });
  expect(alter((r) => { killed(r); r.successor = next(r); }), "NN12", "pass", /next ordinary run/);
  expect(alter((r) => { killed(r); r.successor = next(r, { exit: 1 }); }), "NN12", "not measured");
  expect(alter((r) => { killed(r); r.successor = next(r, { startedAt: at(5) }); }), "NN12", "not measured");
  expect(alter((r) => { r.live = { exists: () => false, alive: () => false, locksIn: () => ["D:\\snaps\\repo.lock"] }; }), "NN12", "fail", /repo\.lock/);
  expect(alter((r) => { r.launch.snapshotsRoot = null; r.receipts = []; }), "NN12", "not measured", /cannot be looked for|neither/);
});
test("the test's own row enumerator reads a tree and says which kinds of row only gate code decides", async () => {
  const { enumerateRows } = await import("./acceptance-rows.mjs");
  const files = new Map([["Cargo.toml", "w"], ["crates/a/Cargo.toml", "a"], ["crates/a/src/lib.rs", "a1"], ["crates/b/Cargo.toml", "b"], ["crates/b/src/lib.rs", "b1"], ["Cargo.lock", "l"]]);
  const text = { "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nedition = "2021"\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nedition.workspace = true\n', "crates/b/Cargo.toml": '[package]\nname = "b"\nedition = "2024"\n' };
  const e = enumerateRows({ files, read: (p) => text[p] });
  assert.deepEqual([...e.rows.keys()].sort(), ["check:.:a", "check:.:b", "clippy:.:a", "clippy:.:b", "rustfmt:2021", "rustfmt:2024"]);
  assert.deepEqual(e.rows.get("rustfmt:2024").files, ["crates/b/src/lib.rs"]);
  assert.ok(e.rows.get("clippy:.:a").files.includes("Cargo.lock") && e.rows.get("clippy:.:a").files.includes("crates/a/src/lib.rs"));
  assert.ok(e.uncovered.length > 10, "the kinds only gate code decides are listed");
  // A judge given only this enumeration says the rows it does not decide are not measured, on every line that needs the inventory.
  for (const id of ["NN05", "NN06", "NN07"]) expect(alter((r) => { r.inventoryUncovered = e.uncovered; }), id, "not measured", /exist only in gate code/);
});

test("NN09 (F-16): the workspace is read from the selected tree, spellings and output formatting do not split a group, and a real difference does", () => {
  // The same two crates, spelled --package b and --package=c: one group, so two commands with no reason fail.
  expect(alter((r) => { split(r, "--package b", "--package=c"); policy(r, []); }), "NN09", "fail", /sent as separate commands/);
  // The same, differing only in how diagnostics are printed: still one group.
  expect(alter((r) => { split(r, "-p b --message-format json", "-p c"); policy(r, []); }), "NN09", "fail", /sent as separate commands/);
  expect(alter((r) => { split(r, "-p b --color never", "-p c --color always"); policy(r, []); }), "NN09", "fail", /sent as separate commands/);
  // A real difference (the feature set) is two groups, each of one command: nothing is combinable, so nothing needs a reason.
  expect(alter((r) => { split(r, "-p b --features x", "-p c"); policy(r, []); }), "NN09", "pass");
  // A crate folder inside a workspace, with no workspace of its own, belongs to that workspace (read from the tree); a separate workspace does not.
  expect(alter((r) => { r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "sub"]\n'; r.tree["ws/sub/Cargo.toml"] = '[package]\nname = "c"\n'; r.procs.push(proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "c", "--locked"], { end: 8, cwd: `${SNAP}\\ws\\sub` })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "c", "--locked"], { cwd: `${SNAP}\\ws\\sub` })); }), "NN09", "fail", /sent as separate commands/);
  expect(alter((r) => { r.tree["ws/sub/Cargo.toml"] = '[workspace]\nmembers = ["c"]\n'; r.procs.push(proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "c", "--locked"], { end: 8, cwd: `${SNAP}\\ws\\sub` })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "c", "--locked"], { cwd: `${SNAP}\\ws\\sub` })); }), "NN09", "pass");
});
test("batch 4: the successor must be an ordinary run of the same store; the baseline must have ended cleanly; the launcher's takeover is exclusive; the sidecar is read as the tree states it", () => {
  const killed = (r) => { r.launch.exit = 137; };
  const next = (r, over = {}) => ({ launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), fingerprint: r.launch.fingerprint, snapshotsRoot: "D:\\snaps", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, ...over } });
  expect(alter((r) => { killed(r); r.successor = next(r, { snapshotsRoot: "D:\\other-store" }); }), "NN12", "not measured");
  expect(alter((r) => { killed(r); r.successor = next(r, { gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C, "--no-cache"] } }); }), "NN12", "not measured");
  // What the launcher saw the moment the run ended is what counts, not a later look.
  expect(alter((r) => { r.launch.afterRun = { observedAt: at(10), locks: ["D:\\snaps\\repo.lock"], locksKnown: true, rootAlive: false }; }), "NN12", "fail", /still there when the run ended/);
  expect(alter((r) => { r.launch.afterRun = { observedAt: at(10), locks: [], locksKnown: true, rootAlive: true }; }), "NN12", "fail", /still alive when the launcher saw the run end/);
  expect(alter((r) => { delete r.launch.afterRun; }), "NN12", "not measured", /no observation .* at the moment the run ended/);
  // An interrupted predecessor is no baseline.
  expect(alter((r) => { r.launch.baseline.exit = 137; }), "NN07", "not measured", /did not end cleanly/);
  // Log continuity: a break in the record ids leaves the process record unproven.
  expect(alter((r) => { r.witness.continuous = false; }), "NN08", "not measured", /ran without a break/);
  // NN03: a program with no readiness of its own leaves a wait unshown; NN05 cannot pass without evidence of the lookup.
  expect(alter((r) => { r.readiness = new Map(); }), "NN03", "not measured", /last discovery probe's exit/);
  expect(alter((r) => { r.readiness = new Map(); r.procs = r.procs.filter((p) => p.exe !== GIT); }), "NN03", "not measured", /for every program that started/);
  // NN08: an unknown folder on one side leaves two otherwise identical calls possibly the same call, never distinct.
  expect(alter((r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, [...g.args], { cwd: null })); }), "NN08", "not measured", /possibly the same call/); // one folder unknown: not a proven repeat (batch 9, table 3)
  // NN09: a sidecar declared only in a comment justifies nothing; an active declaration does.
  const commented = (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json5"] = '{ bundle: { /* externalBin: ["binaries/c"], */ } }'; };
  expect(alter(commented), "NN09", "fail", /sent as separate commands/);
  expect(alter((r) => { commented(r); r.tree["ws/c/tauri.conf.json5"] = '{ bundle: { externalBin: ["binaries/c"] } }'; }), "NN09", "pass");
  // NN09: serial execution with no queue wait signed is not shown to be capacity.
  expect(alter((r) => { split(r, "-p b", "-p c"); policy(r, ["c"]); const p = r.receipts[1].doc.provenance.payload; p.startedAt = at(8.0); p.terminalAt = at(9); p.timingsMs = { admission: 50, queue: 0, bind: 50 }; }), "NN09", "not measured", /no queue wait signed/);
  // NN10: a reason string alone is not proof of a compile; a launch at the very moment of the build's end is not shown to be after it.
  expect(alter((r) => { testJob(r, EXE, {}); r.receipts[0].doc.provenance.payload.testExecutablesReason = "no test programs"; }), "NN10", "not measured", /reason string and no program list/);
  expect(alter((r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.digests[EXE] = { sha256: "h1", mtimeMs: ms(7) }; byExe(r, EXE).start = ms(8); byExe(r, EXE).end = ms(8.1); }), "NN10", "not measured", /very moment the build ended/);
  // NN11: the gate process's own observed exit bounds the run when the launcher's clock disagrees.
  expect(alter((r) => { r.launch.limitMs = 15_000; r.procs[0].end = ms(40); }), "NN11", "fail", /40\.0/);
  // A receipt no send of this run's own tree matches is not credited; a twin whose send matches is.
  expect(alter((r) => { r.receipts.push(job("foreign", ["clippy", "-p", "zz"])); }), "NN09", "not measured", /not shown to be this run's/);
});
test("the substitutes: each superseded original's former body, under its new id, still gives its verdict for its specific reason", async (t) => {
  const SUB_REASONS = {
    "gate 2 (record absent)": /no operating-system record/, "gate 3 (gate process absent)": /no start of the gate process/,
    "gate 8 (unchanged row built)": /did work although no file it reads changed/, "GCX-01-3 (other folder)": /ran outside the selected snapshot/,
    "GCX-05-2 (owed, nothing seen)": /clippy:ws:b was owed a rerun and no program or signed job shows it/,
    "GCX-10-2 (no key pinned)": /cannot be verified: no broker key is pinned/, "GCX-01-2 (windows shape)": /ran in a folder the process record does not keep/,
    "GCX-07-4 (receipt not credited)": /broker job j1 is not credited: its receipt does not verify \(signature does not match\)/, "GCX-07-4 (report absent)": /each with only the work its change needs/,
  };
  assert.equal(SUBSTITUTES.length, Object.keys(SUB_REASONS).length);
  { const tw = EXTRA_TWINS.find(([x]) => x === "gate 9 twin"); const v = run(alter(tw[3])); assert.equal(verdictOf(v, "NN02"), "fail", "the same failing tool in this run's snapshot is Class A failing"); }
  for (const [id, line, verdict, fn] of SUBSTITUTES) await t.test(`${id} ${line}: ${verdict}`, () => { const r = alter(fn); const v = run(r); assert.equal(verdictOf(v, line), verdict, `${id}: ${whyOf(v, line)}`); assert.match(whyOf(v, line), SUB_REASONS[id]); assert.equal(r.decoyRead, undefined, `${id}: a gate-written input was read (${r.decoyRead})`); });
});

test("5.1 the launcher's takeover is one exclusive claim per dead holder: of two contenders that saw the same dead holder exactly one starts, in either interleaving", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-claim-"));
  const lock = path.join(dir, "l");
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "dead", pid: 999999 })}\n`);
  // B inspects the dead holder; before B acts, A completes its whole takeover. B then finds the claim taken and does not start.
  let a = null;
  const isAlive = (pid) => { if (a === null) { a = takeLaunchLock(lock, { runId: "A", pid: process.pid }, { isAlive: (p) => p === process.pid }); } return pid === process.pid; };
  const b = takeLaunchLock(lock, { runId: "B", pid: 4242 }, { isAlive });
  assert.equal(a.previous?.runId, "dead"); assert.ok(b.refused, "the second contender must be refused");
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).runId, "A");
  // Another dead holder is claimed once by whoever comes first, and the claim files are one per dead holder with none left by a loser.
  fs.rmSync(lock); fs.writeFileSync(lock, `${JSON.stringify({ runId: "dead2", pid: 999998 })}\n`);
  const first = takeLaunchLock(lock, { runId: "C", pid: process.pid }, { isAlive: (p) => p === process.pid });
  assert.equal(first.previous.runId, "dead2");
  assert.equal(fs.readdirSync(dir).filter((n) => n.includes(".successor.")).length, 2);
  // Two different dead holders whose ids and pids have the same length are two claims, not one: the claim name keeps the characters of the identity.
  fs.rmSync(lock); fs.writeFileSync(lock, `${JSON.stringify({ runId: "dead3", pid: 999997 })}\n`);
  const third = takeLaunchLock(lock, { runId: "D", pid: process.pid }, { isAlive: (p) => p === process.pid });
  assert.equal(third.previous?.runId, "dead3", `a later dead holder of the same shape must be takeable: ${third.refused ?? ""}`);
  const claims = fs.readdirSync(dir).filter((n) => n.includes(".successor."));
  assert.equal(claims.length, 3); assert.ok(claims.some((n) => n.includes("dead3")) && claims.some((n) => n.includes("dead2")), `claim names keep the holder's identity: ${claims.join(", ")}`);
});

test("5.12 the production reader sets aside the run folder's legacy gate artifacts: with them present it opens only the launcher's record, the process record and the receipts list, and returns the same evidence", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-legacy-"));
  const events = [{ id: 4688, pid: 100, ppid: 50, exe: "C:\\node\\node.exe", commandLine: "node gate.mjs", time: at(0) }, { id: 4689, pid: 100, exit: 0, time: at(1) }];
  fs.writeFileSync(path.join(dir, "launch.json"), JSON.stringify({ schema: "rightkit.acceptance-launch/3", runId: "f".repeat(64), commit: C, startedAt: at(0), endedAt: at(10), exit: 0 }));
  fs.writeFileSync(path.join(dir, "processes.json"), JSON.stringify({ kind: "windows-security", events }));
  const without = readRunDir(dir);
  const legacy = ["gate.log", "verdict.json", "forecast.json", "lookup.json", "coverage.json", "decision.json", "run-record.json", "classA-verdict.json", "sidecar-reasons.txt"];
  for (const n of legacy) fs.writeFileSync(path.join(dir, n), n.endsWith(".json") ? JSON.stringify({ passed: false, classA: "failed", lanes: 1, sidecarBound: ["c"], note: "misleading" }) : "Class A failed; sidecar-bound: c; lanes 1\n");
  const opened = []; const real = fs.readFileSync; const realDir = fs.readdirSync;
  fs.readFileSync = (p, ...a) => { opened.push(path.basename(String(p))); return real(p, ...a); };
  fs.readdirSync = (p, ...a) => { opened.push(`dir:${path.basename(String(p))}`); return realDir(p, ...a); };
  let withLegacy; try { withLegacy = readRunDir(dir); } finally { fs.readFileSync = real; fs.readdirSync = realDir; }
  for (const n of legacy) assert.ok(!opened.includes(n), `${n} was opened by the reader`);
  assert.ok(opened.every((n) => ["launch.json", "processes.json", "receipts.txt", "cache-store.json", "cache-store-after.json", "snapshot-stat.json"].includes(n)), `unexpected opens: ${opened.join(", ")}`);
  assert.equal(withLegacy.processes.length, 1); assert.deepEqual(withLegacy.processes, without.processes); assert.deepEqual(withLegacy.launch, without.launch);
});

test("5.12 the ledger: all 63 originals, each with its disposition and the case that stands in for it", (t) => {
  const SUBSTITUTE_FOR = { "gate 8": "gate 8 (unchanged row built)", "GCX-05-2": "GCX-05-2 (owed, nothing seen)", "GCX-07-4": "GCX-07-4 (receipt not credited)" };
  const RESTORED = ["gate 2", "gate 3", "gate 4", "gate 9", "GCX-09-8", "GCX-10-2", "GCX-01-3", "GCX-09-6", "GCX-12-2"];
  const RESTATED = ["gate 5", "GCX-01-2", "GCX-05-5"];
  const where = (id) => SUBSTITUTES.find(([x]) => x === id) ?? EXTRA_TWINS.find(([x]) => x === id);
  const rows = [];
  for (const [id, line, verdict] of FROZEN) rows.push([id, line, verdict, RESTORED.includes(id) ? "executable: restored under its original id, misleading input present and unread" : RESTATED.includes(id) ? "executable: restated" : "executable", "-"]);
  for (const [id, line, verdict, why] of NO_OUTSIDE_FORM) rows.push([id, line, verdict, `superseded: ${why}`, SUBSTITUTE_FOR[id] ?? "-"]);
  assert.equal(rows.length, 63);
  for (const id of RESTORED) assert.ok(FROZEN.some(([x]) => x === id) && !NO_OUTSIDE_FORM.some(([x]) => x === id), `${id} is executable under its own id`);
  for (const [orig, sub] of Object.entries(SUBSTITUTE_FOR)) { assert.ok(where(sub), `${orig}'s substitute ${sub} exists`); assert.ok(NO_OUTSIDE_FORM.some(([x]) => x === orig), `${orig} is superseded`); }
  for (const row of rows) t.diagnostic(`${row[0]} | ${row[1]} | was ${row[2]} | ${row[3]} | substitute: ${row[4]}`);
});

test("5.5 the sidecar exception is read from the bundle declaration of each format: JSON, JSON5 and TOML, active only, never a comment or another table", () => {
  const setup = (name, body) => (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree[`ws/c/${name}`] = body; };
  const pass = /* active declarations */ [
    ["tauri.conf.json", JSON.stringify({ bundle: { externalBin: ["binaries/c"] } })],
    ["tauri.conf.json5", '{ bundle: { externalBin: ["binaries/c"] } }'],
    ["Tauri.toml", '[bundle]\nexternalBin = ["binaries/c"]\n'],
  ];
  const fail = /* inactive or in the wrong place */ [
    ["tauri.conf.json5", '{ build: { externalBin: ["x"] }, bundle: { } }'],
    ["tauri.conf.json5", '{ bundle: { resources: { externalBin: ["x"] } } }'],
    ["Tauri.toml", '[build]\nexternalBin = ["x"]\n[bundle]\nidentifier = "c"\n'],
    ["Tauri.toml", '[bundle]\n# externalBin = ["binaries/c"]\n'],
    ["tauri.conf.json", JSON.stringify({ bundle: { externalBin: [] } })],
  ];
  for (const [name, body] of pass) expect(alter(setup(name, body)), "NN09", "pass");
  for (const [name, body] of fail) expect(alter(setup(name, body)), "NN09", "fail", /sent as separate commands/);
});

// ---- batch 6: claims two or more Council CV-6 seats raised, each reproduced by a test first (chat gate, 2026-11-21) -----------------------------------------
const CARGO = "C:\\cargo\\bin\\cargo.exe";
test("6.1 cargo's short names and other test verbs, and a verb behind global options on the broker path, are not missed (Opus R-1, Sonnet F6, Fable UO-20, Astra SO-07)", () => {
  const direct = (args) => (r) => { r.procs.push(proc(2, CARGO, args, { end: 3, cwd: SNAP })); };
  for (const args of [["t", "-p", "b"], ["test", "-p", "b"], ["--config", "k=v", "t"], ["bench"], ["nextest", "run"], ["+nightly", "miri", "test"]]) expect(alter(direct(args)), "NN10", "fail", /cargo test start\(s\) ran outside the broker/);
  expect(alter(direct(["frobnicate", "x"])), "NN10", "not measured", /not a verb known to build no tests/);
  expect(alter(direct(["metadata", "--format-version", "1"])), "NN10", "pass", /no test/);
  // the broker path: a short name and a global option before the verb are still a test job, so its manifest is demanded
  expect(alter((r) => { testJob(r, EXE, {}); same(r, ["--config", "k=v", "test", "-p", "b", "--no-run"]); }), "NN10", "not measured", /carries no manifest of test programs/);
  expect(alter((r) => { testJob(r, EXE, {}); same(r, ["t", "-p", "b", "--no-run"]); }), "NN10", "not measured", /carries no manifest of test programs/);
});

test("6.2 a start whose command line the record does not hold is an unknown call, not an empty one, for every line that reads arguments (Opus R-8, Sonnet F3, Astra SO-07, Sol IP-11, Fable UO-3)", () => {
  const blind = (exe) => (r) => { const p = proc(2, exe, [], { end: 3, cwd: SNAP }); p.argsKnown = false; r.procs.push(p); };
  expect(alter(blind(CARGO)), "NN10", "not measured", /no command line in the process record/);
  expect(alter(blind(RUSTRIGHT)), "NN01", "not measured", /no command line in the process record/);
  expect(alter((r) => { const f = byExe(r, RUSTFMT); f.args = []; f.argsKnown = false; }), "NN07", "not measured", /no command line in the process record/);
  expect(alter((r) => { const f = byExe(r, RUSTFMT); f.args = []; f.argsKnown = false; }), "NN01", "not measured", /no command line in the process record/);
  expect(alter((r) => { const f = byExe(r, RUSTFMT); f.args = []; f.argsKnown = false; }), "NN06", "not measured", /no command line in the process record/);
  // the same start with its command line known is judged as before
  expect(alter(() => {}), "NN10", "pass");
});

test("6.3 a tool that serves several rows is not credited to every row of it by one start (Opus R-9, Sonnet F4, Astra SO-06, Sol IP-06, Fable UO-5)", () => {
  const rows = (r) => { r.inventory.set("deny:a", { stage: "A", files: ["a/Cargo.lock"] }); r.inventory.set("deny:b", { stage: "A", files: ["b/Cargo.lock"] }); for (const x of ["a", "b"]) { r.blobs[`${x}/Cargo.lock`] = "n"; r.priorBlobs[`${x}/Cargo.lock`] = "o"; } };
  const one = (cwd) => (r) => { rows(r); r.procs.push(proc(1.3, DENY, ["check"], { end: 1.4, cwd })); };
  expect(alter(one(SNAP)), "NN07", "not measured", /cannot be told apart by row/);
  // a start inside one row's workspace serves that row; the other row has no start at all
  expect(alter((r) => { rows(r); r.procs.push(proc(1.3, DENY, ["check"], { end: 1.4, cwd: `${SNAP}\\a` })); }), "NN07", "fail", /deny:b changed .* did no work/);
  // placed by folder, each row has its own start
  expect(alter((r) => { rows(r); r.procs.push(proc(1.3, DENY, ["check"], { end: 1.4, cwd: `${SNAP}\\a` }), proc(1.5, DENY, ["check"], { end: 1.6, cwd: `${SNAP}\\b` })); }), "NN07", "pass");
});

test("6.4 a Class A operand outside the selected tree, or one that is not a file of it, is never skipped (Opus R-6, Sonnet F1, Astra SO-05, Sol IP-03, Fable UO-2)", () => {
  expect(alter((r) => { byExe(r, RUSTFMT).args.push("..\\x.rs"); }), "NN01", "fail", /outside the selected snapshot/);
  expect(alter((r) => { byExe(r, RUSTFMT).args.push("src/../../x.rs"); }), "NN01", "fail", /outside the selected snapshot/);
  expect(alter((r) => { byExe(r, RUSTFMT).args.push("generated/y.rs"); }), "NN01", "not measured", /not a file of the selected tree/);
  expect(alter(() => {}), "NN01", "pass");
});

test("6.5 the sidecar declaration is read from the root bundle object (or tauri.bundle) only, never from a nested key or a string (Opus R-5, Sonnet F2, Sol IP-07, Astra SO-11, Fable UO-7)", () => {
  const set = (body) => (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json5"] = body; };
  for (const body of ['{ plugins: { x: { bundle: { externalBin: ["y"] } } }, bundle: {} }', '{ note: "bundle: { externalBin: [\\"y\\"] }", bundle: {} }', '{ build: { bundle: { externalBin: ["y"] } } }', '{ bundle: { resources: { externalBin: ["y"] } } }']) expect(alter(set(body)), "NN09", "fail", /sent as separate commands/);
  for (const body of ['{ "bundle": { "externalBin": ["y"] } }', "{ bundle: { 'externalBin': ['y'] } }", '{ tauri: { bundle: { externalBin: ["y"] } } }', '{ productName: "a #b", bundle: { externalBin: ["y"] } }']) expect(alter(set(body)), "NN09", "pass");
});

test("6.6 equivalent cargo options are one setting, and an option the check cannot interpret is not read as a reason to split (Opus R-4, Sol IP-07, Fable UO-6)", () => {
  for (const [a, b] of [["-p b -Fx", "-p c -F x"], ["-p b -Zunstable-options", "-p c -Z unstable-options"], ["-p b --frozen", "-p c --offline"], ["-p b --manifest-path .\\ws\\Cargo.toml", "-p c --manifest-path=ws/Cargo.toml"]]) { const v = run(alter((r) => { split(r, a, b); policy(r, []); })); assert.equal(verdictOf(v, "NN09"), "fail", `${a} | ${b}: ${whyOf(v, "NN09")}`); assert.match(whyOf(v, "NN09"), /sent as separate commands/); }
  expect(alter((r) => { split(r, "-p b --some-new-flag", "-p c"); policy(r, []); }), "NN09", "not measured", /does not know the meaning of/);
  // a real difference in a known option (another feature set) is a legitimate reason for two commands
  expect(alter((r) => { split(r, "-p b --features x", "-p c --features y"); policy(r, []); }), "NN09", "pass");
});

test("6.7 F-10: two compiles are 'together' only by overlapping signed intervals or a broker-signed queue wait, never by submission proximity alone (Opus R-3 / CV-5 F-10, Astra SO-15, Sol IP-12)", () => {
  const timed = (submit2, start2, end2, queue) => (r) => { split(r, "-p b", "-p c"); policy(r, ["c"]); const p = r.receipts[1].doc.provenance.payload; p.startedAt = at(start2); p.terminalAt = at(end2); p.timingsMs = { admission: 50, queue, bind: 50 }; r.receipts[1].doc.provenance.payload.startedAt = at(start2); };
  // started 100 ms after the first and still running with it: overlap, together
  expect(alter(timed(0, 6.2, 9, 0)), "NN09", "pass");
  // submitted 150 ms after the first but started only after it ended, with no queue wait signed: held back or no capacity, not shown
  expect(alter(timed(0, 8.05, 9, 0)), "NN09", "not measured", /no queue wait signed/);
  // the same with a queue wait the broker signed: the broker's own, not the gate's
  expect(alter(timed(0, 8.05, 9, 1900)), "NN09", "pass");
  // submitted only after the first ended: no record shows it was ready or that a lane was free, so that the groups could have run together is not shown (batch 9, table 6)
  expect(alter(timed(0, 8.1, 9, 0)), "NN09", "not measured", /submitted only after job j1 ended/);
});

test("6.8 a send or job that fits more than one counterpart is credited to none, and an unmatched send or job is carried into every line that reads receipts (Astra SO-01, Sol IP-01, Fable UO-4, Sonnet F9, Opus R-18)", () => {
  // two jobs with one command, one send: which job the send was is not shown
  expect(alter((r) => { r.receipts.push(job("j1b", ["clippy", "--profile", "release", "-p", "b", "--locked"])); }), "NN09", "not measured", /not shown to be this run's/);
  // a compile send with no receipt beside a matched metadata receipt: NN01 is not masked by the count of jobs
  expect(alter((r) => { r.procs.push(proc(6.5, RUSTRIGHT, ["cargo", "build", "-p", "z"], { end: 7, cwd: `${SNAP}\\ws` })); r.procs.push(proc(6.6, RUSTRIGHT, ["cargo", "metadata"], { end: 6.7, cwd: `${SNAP}\\ws` })); r.receipts.push(job("m1", ["metadata"], { start: 6.6, end: 6.7 })); }), "NN01", "not measured", /(signed compile job|no signed job that matches)/);
  // an unmatched test send: NN10 is not a pass
  expect(alter((r) => { r.procs.push(proc(6.5, RUSTRIGHT, ["cargo", "test", "-p", "b", "--no-run"], { end: 7, cwd: `${SNAP}\\ws` })); }), "NN10", "not measured", /(test request|no signed job that matches)/);
});

test("6.9 the Mac process record keeps the arguments it read; the Security export reads a missing exit status as unknown, never as zero (Opus R-15, Sonnet F8, Astra SO-13, Fable UO-12; Astra SO-09, Sol IP-09)", () => {
  const lines = [{ event: "exec", pid: 7, ppid: 1, path: "/usr/bin/git", args: ["git", "status"], cwd: "/src", time: at(0) }, { event: "exit", pid: 7, status: 0, time: at(1) }];
  const mac = processesFromEslogger(lines); assert.equal(mac[0].argsKnown, true); assert.deepEqual(mac[0].args, ["status"]);
  assert.equal(processesFromEslogger([{ ...lines[0], args: undefined }, lines[1]])[0].argsKnown, false);
  for (const status of [null, "", undefined, "n/a"]) {
    const raw = { oldest: at(-100), newest: at(3), continuous: true, records: 2, events: [{ id: 4688, time: at(0), data: { NewProcessId: "0x64", ProcessId: "0x32", NewProcessName: "C:\\n.exe", CommandLine: "n" } }, { id: 4689, time: at(1), data: { ProcessId: "0x64", ProcessName: "C:\\n.exe", ...(status === undefined ? {} : { Status: status }) } }] };
    const x = exportWindowsProcesses(at(-1), at(2), { run: fake(raw) });
    assert.equal(x.events.find((e) => e.id === 4689).exit === 0, false, `a missing exit status (${JSON.stringify(status)}) must not read as 0`);
    assert.equal(processesFromWindowsEvents(x.events)[0].exit, null);
  }
});

test("6.10 NN12: a program that ends after the gate did is not clean even by a moment, and the successor must run the same gate code (Astra SO-03 SO-14, Sol IP-04, Fable UO-11, Opus R-17)", () => {
  expect(alter((r) => { const d = proc(5, "C:\\tools\\child.exe", ["x"], { end: 10.5, cwd: SNAP }); r.procs.push(d); }), "NN12", "fail", /outlived the gate's exit/);
  const killed = (mutate) => alter((r) => { r.launch.exit = 137; r.successor = { launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), fingerprint: r.launch.fingerprint, snapshotsRoot: "D:\\snaps", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] } } }; mutate?.(r); });
  expect(killed(), "NN12", "pass");
  expect(killed((r) => { r.successor.launch.fingerprint = { ...r.launch.fingerprint, gateCodeTree: "d".repeat(40) }; }), "NN12", "not measured", /--successor/);
  expect(killed((r) => { delete r.successor.launch.fingerprint; }), "NN12", "not measured", /--successor/);
});

test("6.11 a handed-in inventory can add rows and files but never remove or narrow the test's own enumeration; the predecessor's Class B outcomes are unknown (Opus R-7, Sonnet F5, Astra SO-02, Sol IP-02, Fable UO-9; Opus R-11, Fable UO-8)", () => {
  const sh = (cwd, ...a) => execFileSync("git", ["-C", cwd, ...a], { encoding: "utf8", windowsHide: true });
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "rk-inv-repo-")); const out = fs.mkdtempSync(path.join(os.tmpdir(), "rk-inv-out-")); const prevOut = fs.mkdtempSync(path.join(os.tmpdir(), "rk-inv-prev-"));
  sh(repo, "init", "-q"); sh(repo, "config", "user.email", "t@t"); sh(repo, "config", "user.name", "t");
  fs.mkdirSync(path.join(repo, "a", "src"), { recursive: true });
  fs.writeFileSync(path.join(repo, "Cargo.toml"), '[workspace]\nmembers = ["a"]\n'); fs.writeFileSync(path.join(repo, "a", "Cargo.toml"), '[package]\nname = "a"\nedition = "2021"\n'); fs.writeFileSync(path.join(repo, "a", "src", "lib.rs"), "fn a() {}\n");
  sh(repo, "add", "-A"); sh(repo, "commit", "-q", "-m", "x"); const commit = sh(repo, "rev-parse", "HEAD").trim(); const tree = sh(repo, "rev-parse", "HEAD^{tree}").trim();
  const events = [{ id: 4688, pid: 100, ppid: 50, exe: "C:\\n.exe", commandLine: "n", time: at(0) }, { id: 4689, pid: 100, exit: 0, time: at(1) }];
  fs.writeFileSync(path.join(prevOut, "launch.json"), JSON.stringify({ runId: "p".repeat(64), endedAt: at(-5) })); fs.writeFileSync(path.join(prevOut, "processes.json"), JSON.stringify({ kind: "windows-security", events, continuous: true, problems: [] }));
  fs.writeFileSync(path.join(out, "launch.json"), JSON.stringify({ runId: "r".repeat(64), commit, startedAt: at(0), baseline: { commit, out: prevOut, runId: "p".repeat(64) } })); fs.writeFileSync(path.join(out, "processes.json"), JSON.stringify({ kind: "windows-security", events }));
  const own = readRunDir(out, { repo });
  assert.ok(own.inventory.has("rustfmt:2021") && own.inventory.has("clippy:.:a"), `the test's own enumeration finds the rows: ${[...own.inventory.keys()].join(", ")}`);
  // a hand-written file that names the right tree and states no rows
  const file = path.join(out, "inv.json"); fs.writeFileSync(file, JSON.stringify({ tree, rows: {}, uncovered: [] }));
  const handed = readRunDir(out, { repo, inventoryFile: file });
  assert.deepEqual([...handed.inventory.keys()].sort(), [...own.inventory.keys()].sort(), "an empty hand-written inventory removed nothing");
  assert.ok(handed.inventoryUncovered.length >= own.inventoryUncovered.length, "it did not claim a kind covered");
  fs.writeFileSync(file, JSON.stringify({ tree, rows: { "x:extra": { stage: "A", files: ["a/src/lib.rs"] }, "rustfmt:2021": { stage: "A", files: ["more.rs"] } } }));
  const added = readRunDir(out, { repo, inventoryFile: file });
  assert.ok(added.inventory.has("x:extra")); assert.deepEqual(added.inventory.get("rustfmt:2021").files.includes("more.rs"), true); assert.ok(added.inventory.get("rustfmt:2021").files.includes("a/src/lib.rs"), "own files stay");
  // the predecessor's broker outcomes are not read, so no Class B row is known to have passed or failed
  assert.ok(own.prior.unknownRows.has("clippy:.:a"), "a Class B row's prior outcome is unknown"); assert.equal(own.prior.failedRows.has("clippy:.:a"), false);
  // a predecessor whose process export failed, or was empty, or broke, is not evidence that nothing failed
  for (const bad of [{ kind: "windows-security", events: [], continuous: false, problems: ["the Security log could not be read"] }, { kind: "windows-security", events, continuous: true, problems: ["x"] }, { kind: "windows-security", events, continuous: false, problems: [] }]) { fs.writeFileSync(path.join(prevOut, "processes.json"), JSON.stringify(bad)); assert.equal(readRunDir(out, { repo }).prior, null, "an unusable predecessor record yields no prior outcomes"); }
  // a predecessor Class A tool with no recorded end is unknown, not passed
  const open = [{ id: 4688, pid: 100, ppid: 50, exe: "C:\\tools\\rustfmt.exe", commandLine: "rustfmt --check --edition 2021 a.rs", time: at(-9) }];
  fs.writeFileSync(path.join(prevOut, "processes.json"), JSON.stringify({ kind: "windows-security", events: open, continuous: true, problems: [] }));
  assert.ok(readRunDir(out, { repo }).prior.unknownRows.has("rustfmt:2021"), "a tool start with no end leaves its row unknown");
});

test("6.13 a Class A program that failed in a folder outside the snapshot is not shown to be another unit's, and a reused row with an unknown earlier outcome is not shown to have passed (Opus R-2, Fable UO-10; Fable UO-8, Astra SO-12, Sol IP-10)", () => {
  expect(alter((r) => { r.procs.push(proc(9, DENY, ["check"], { exit: 1, cwd: "D:\\other-unit" })); }), "NN02", "not measured", /outside the snapshot/);
  expect(alter((r) => { r.procs.push(proc(9, DENY, ["check"], { exit: null, cwd: "D:\\other-unit" })); }), "NN02", "not measured", /no recorded status/);
  expect(alter((r) => { r.prior.unknownRows = new Set(["rustfmt:2021"]); r.expectedReuse = true; r.inventory.set("deny", { stage: "A", files: ["Cargo.lock"] }); r.prior.unknownRows.add("deny"); }), "NN02", "not measured", /is reused and what happened to it/);
  expect(alter((r) => { r.prior = null; }), "NN02", "not measured", /is reused and what happened to it/);
  expect(alter(() => {}), "NN02", "pass");
});

test("6.14 the reader names only the files of the launcher's record, the process record and the receipts list; a source scan fails if it ever names a file the gate writes", () => {
  const names = new Set([...readRunDir.toString().matchAll(/"([\w.-]+\.(?:json|txt|log|ndjson|jsonl))"/g)].map((m) => m[1]));
  // the launcher's own record, the OS process record, the receipts list, and the launcher's own listings (the cache store before and after, the snapshot stat record)
  assert.deepEqual([...names].sort(), ["cache-store-after.json", "cache-store.json", "launch.json", "processes.json", "receipts.txt", "snapshot-stat.json"], "the production reader's file names");
});

// ---- batch 7: the three buildable items of the EGM review of the open decisions (chat gate, 2026-11-21) ----------------------------------------------------
test("7.1 the launcher lists the gate's cache store before and after the run and stats the snapshots at the gate's exit; the reader returns both from the launcher's own files", async () => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rk-b7-")); execFileSync("git", ["init", "-q", work], { windowsHide: true }); const store = path.join(work, ".git", "rightkit-gate-cache"); const snaps = path.join(work, "snaps"); fs.mkdirSync(store); fs.mkdirSync(snaps); // the store has one fixed place: the repository's git folder
  fs.writeFileSync(path.join(store, "old-entry.json"), "{}");
  // a snapshot folder from earlier runs, updated in place, with times long before this run: it is still listed
  fs.mkdirSync(path.join(snaps, "warm"), { recursive: true }); fs.writeFileSync(path.join(snaps, "warm", "w.rs"), "x"); const longAgo = new Date("2020-01-01T00:00:00Z"); fs.utimesSync(path.join(snaps, "warm", "w.rs"), longAgo, longAgo); fs.utimesSync(path.join(snaps, "warm"), longAgo, longAgo);
  const fp = { gateCodeTree: "c".repeat(40), node: "v22", platform: "win32", tools: { rustfmt: "r", cargo: "c", "cargo-deny": "d", pnpm: "p", git: "g" } };
  const child = () => { const c = new EventEmitter(); c.pid = 999991; c.kill = () => {}; setImmediate(() => { fs.mkdirSync(path.join(store, "sub"), { recursive: true }); fs.writeFileSync(path.join(store, "sub", "new-entry.json"), "{\"a\":1}"); fs.writeFileSync(path.join(store, "old-entry.json"), "{\"changed\":true}"); fs.mkdirSync(path.join(snaps, "k9", "src"), { recursive: true }); fs.writeFileSync(path.join(snaps, "k9", "src", "a.rs"), "fn a() {}"); c.emit("exit", 0, null); }); return c; };
  const out = path.join(work, "run"); const code = await launch({ out, commit: C, receiptsDir: work, repo: work, gate: path.join(work, "g.mjs"), gateArgs: ["--to", "compile"], snapshotsRoot: snaps, spawnImpl: child, exportProcesses: (from, to) => ({ kind: "windows-security", from, to, newest: to, continuous: true, events: [], problems: [] }), fingerprint: fp, platform: "win32", host: "h" });
  assert.equal(code, 0);
  const before = JSON.parse(fs.readFileSync(path.join(out, "before", "cache-store.json"), "utf8")); const after = JSON.parse(fs.readFileSync(path.join(out, "cache-store-after.json"), "utf8"));
  assert.deepEqual(Object.keys(before.files), ["old-entry.json"]); assert.deepEqual(Object.keys(after.files).sort(), ["old-entry.json", "sub/new-entry.json"]);
  const stat = JSON.parse(fs.readFileSync(path.join(out, "snapshot-stat.json"), "utf8")); assert.equal(stat.known, true); assert.deepEqual(Object.keys(stat.dirs.k9), ["src/a.rs"]); assert.deepEqual(Object.keys(stat.dirs.warm), ["w.rs"], "a warm snapshot folder is listed whatever its own times");
  // a run folder reused after a crash does not offer the earlier run's listings
  { const reuse = path.join(work, "reuse"); fs.mkdirSync(reuse, { recursive: true }); fs.writeFileSync(path.join(reuse, "snapshot-stat.json"), JSON.stringify({ root: "old", dirs: { stale: { "x.rs": [1, 1] } }, known: true })); fs.writeFileSync(path.join(reuse, "cache-store-after.json"), JSON.stringify({ dir: "old", files: { "stale.json": [1, 1] } }));
    const c2 = await launch({ out: reuse, commit: C, receiptsDir: work, repo: work, gate: path.join(work, "g.mjs"), gateArgs: ["--to", "compile"], snapshotsRoot: path.join(work, "no-such-root"), spawnImpl: () => { const c = new EventEmitter(); c.pid = 999993; c.kill = () => {}; setImmediate(() => c.emit("exit", 0, null)); return c; }, exportProcesses: (from, to) => ({ kind: "windows-security", from, to, newest: to, continuous: true, events: [], problems: [] }), fingerprint: fp, platform: "win32", host: "h" });
    assert.equal(c2, 0); assert.equal(JSON.parse(fs.readFileSync(path.join(reuse, "snapshot-stat.json"), "utf8")).known, false, "the earlier run's stat record is gone"); assert.equal(Object.hasOwn(JSON.parse(fs.readFileSync(path.join(reuse, "cache-store-after.json"), "utf8")).files, "stale.json"), false); }
  const read = readRunDir(out); assert.deepEqual(read.cacheStore.written, ["old-entry.json", "sub/new-entry.json"]); assert.deepEqual(read.cacheStore.removed, []); assert.deepEqual(Object.keys(read.snapshotStat.dirs.k9), ["src/a.rs"]);
  // a store that cannot be listed is unknown, never empty
  fs.rmSync(store, { recursive: true, force: true }); fs.writeFileSync(store, "x"); const bad = await launch({ out: path.join(work, "run2"), commit: C, receiptsDir: work, repo: work, gate: path.join(work, "g.mjs"), gateArgs: ["--to", "compile"], snapshotsRoot: snaps, spawnImpl: () => { const c = new EventEmitter(); c.pid = 999992; c.kill = () => {}; setImmediate(() => c.emit("exit", 0, null)); return c; }, exportProcesses: (from, to) => ({ kind: "windows-security", from, to, newest: to, continuous: true, events: [], problems: [] }), fingerprint: fp, platform: "win32", host: "h" });
  assert.equal(bad, 0); assert.equal(readRunDir(path.join(work, "run2")).cacheStore, null);
});

test("7.2 NN05 names the launcher's listing: entries written during the run are shown, and cannot be mapped to a row from outside the gate's code, so the line stays not measured", () => {
  expect(alter((r) => { r.cacheStore = { dir: "D:\\store", written: ["ab/cd12.json", "ef/0099.json"], removed: [] }; }), "NN05", "not measured", /2 cache entr(y|ies) written during the run .* which row each belongs to is not shown/);
  expect(alter((r) => { r.cacheStore = null; }), "NN05", "not measured", /launcher recorded no listing of the cache store/);
});

test("7.3 NN01 uses the launcher's stat record of the snapshot: an operand written after its program started fails, and no stat record is a gap", () => {
  expect(alter(() => {}), "NN01", "pass");
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10, ms(1.5)]; }), "NN01", "fail", /src\/a\.rs was written after rustfmt started/);
  expect(alter((r) => { r.snapshotStat = null; }), "NN01", "not measured", /stat record of the snapshot/);
  expect(alter((r) => { delete r.snapshotStat.dirs.k1["src/a.rs"]; }), "NN01", "not measured", /no stat record of src\/a\.rs/);
  // a write within the file system's clock tick of the program's start, or at the same moment, is a tie; a malformed entry is no record
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10, ms(0.99)]; }), "NN01", "not measured", /within 50 ms of rustfmt starting/);
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10, ms(1.0)]; }), "NN01", "not measured", /within 50 ms of rustfmt starting/);
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10, "soon"]; }), "NN01", "not measured", /no stat record of src\/a\.rs/);
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10]; }), "NN01", "not measured", /no stat record of src\/a\.rs/);
  expect(alter((r) => { r.snapshotStat.dirs.k1["src/a.rs"] = [10, ms(0.5)]; r.snapshotStat.dirs = Object.create({ k1: { "src/a.rs": [10, ms(0.5)] } }); }), "NN01", "not measured", /stat record/);
});

test("7.4 NN03 readiness is the last discovery probe's exit, taken from the process record; the gate's own work between it and the first Class A start is a gap, not a pass", () => {
  const derived = (r) => { r.readiness = new Map(); };
  expect(alter(derived), "NN03", "not measured", /between the last discovery probe's exit .* and the first Class A start \(950 ms\)/);
  // a first Class A start at the very moment the last probe exited is a tie, not a measurement: a derived readiness never passes
  expect(alter((r) => { derived(r); const f = byExe(r, RUSTFMT); f.start = ms(0.05); f.end = ms(0.15); }), "NN03", "not measured", /a tie, not a measurement/);
  // a program that waited with a lane free after the first start still fails, with the probe exit as its readiness (the first start is well after it, so this is not the first-start fallback)
  expect(alter((r) => { derived(r); r.launch.lanes = 4; const f = byExe(r, RUSTFMT); f.start = ms(1.0); f.end = ms(3); r.procs.push(proc(3.5, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 5, cwd: SNAP })); }), "NN03", "not measured", /shellcheck was ready at .* \(the last discovery probe's exit\) and started 3450 ms later/);
  // no discovery probe in the record: nothing is derived and the old statement stands
  expect(alter((r) => { derived(r); r.procs = r.procs.filter((p) => p.exe !== GIT); }), "NN03", "not measured", /when each Class A row became ready is not shown/);
  // a probe with no recorded exit, or with its command line unknown, is no readiness
  expect(alter((r) => { derived(r); const g = r.procs.find((p) => p.exe === GIT); g.end = null; g.exit = null; }), "NN03", "not measured", /no recorded exit|when each Class A row became ready is not shown/);
  // only commands that discover (git rev-parse, cargo metadata, rustc -vV) count: a git commit-tree is not a probe
  expect(alter((r) => { derived(r); r.procs.find((p) => p.exe === GIT).args = ["commit-tree", "x"]; }), "NN03", "not measured", /when each Class A row became ready is not shown/);
});

test("8.1 NN03: a readiness keyed by program name cannot be told which of two starts of that program it belongs to, so it is not used for either (CV-6 F-6)", () => {
  // two rustfmt starts (3.0 s and 3.2 s) and one readiness for 'rustfmt' (3.2 s): taken for both, the first start looks early and the wait before it disappears
  const two = (r) => { r.launch.lanes = 4; const f = byExe(r, RUSTFMT); f.start = ms(3.0); f.end = ms(3.1); r.procs.push(proc(3.2, RUSTFMT, ["--check", "--edition", "2021", "src/b.rs"], { end: 3.3, cwd: SNAP })); r.readiness = new Map([["rustfmt", ms(3.2)]]); };
  const v = run(alter(two));
  assert.notEqual(verdictOf(v, "NN03"), "pass", `NN03: ${whyOf(v, "NN03")}`);
  // with no wait to find, the verdict is still not a pass and names the ambiguity
  const one = alter((r) => { two(r); r.procs.find((p) => p.start === ms(3.2)).start = ms(3.0); r.procs.find((p) => p.start === ms(3.0) && p.end === ms(3.3)).end = ms(3.1); });
  assert.notEqual(verdictOf(run(one), "NN03"), "pass");
  // one start of the program with its readiness is unchanged: still a pass
  expect(alter((r) => { r.launch.lanes = 4; }), "NN03", "pass");
});

test("8.2 NN01: a rustc version probe is not a compile outside the broker (it is a discovery probe to NN03); a rustc that compiles a source still is", () => {
  const probe = (r) => { r.procs.push(proc(0.02, "C:\\rust\\bin\\rustc.exe", ["-vV"], { cwd: SNAP })); };
  const v = run(alter(probe));
  assert.notEqual(verdictOf(v, "NN01"), "fail", `NN01: ${whyOf(v, "NN01")}`);
  for (const args of [["--version"], ["-V"], ["--print", "sysroot"], ["--print=cfg"]]) { const w = run(alter((r) => { r.procs.push(proc(0.02, "C:\\rust\\bin\\rustc.exe", args, { cwd: SNAP })); })); assert.notEqual(verdictOf(w, "NN01"), "fail", `${args.join(" ")}: ${whyOf(w, "NN01")}`); }
  expect(alter((r) => { r.procs.push(proc(0.02, "C:\\rust\\bin\\rustc.exe", ["src/lib.rs", "--crate-type", "lib"], { cwd: SNAP })); }), "NN01", "fail", /compile\(s\) ran outside the broker \(first: rustc src\/lib\.rs/);
  expect(alter((r) => { r.procs.push(proc(0.02, "C:\\rust\\bin\\rustc.exe", ["-vV", "src/lib.rs"], { cwd: SNAP })); }), "NN01", "fail", /outside the broker/);
});

// ---- batch 9: one test per row of D:\scratch\loop-review\twelve-line-step5\decision-tables.md ------------------------------------------------------
test("9.1 NN10 (table 1): every test-producing verb is a test build; a request for one needs its own signed job; a verb not known to build no tests is not shown to build none", () => {
  const verbJob = (verb, tail, extra = {}, withJob = true) => (r) => { r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.procs.push(proc(6, RUSTRIGHT, ["cargo", verb, ...tail], { end: 8, cwd: `${SNAP}\\ws` })); r.receipts = withJob ? [job("t1", [verb, ...tail], { extra })] : []; };
  for (const [verb, tail] of [["test", ["-p", "b", "--no-run"]], ["bench", ["-p", "b", "--no-run"]], ["nextest", ["run", "-p", "b"]], ["miri", ["test", "-p", "b"]], ["llvm-cov", ["-p", "b"]], ["tarpaulin", ["-p", "b"]]]) {
    expect(alter(verbJob(verb, tail)), "NN10", "not measured", /carries no manifest of test programs/); // 1b
    expect(alter(verbJob(verb, tail, { testExecutables: [] })), "NN10", "not measured", /lists no test program/); // 1c
    expect(alter(verbJob(verb, tail, {}, false)), "NN10", "not measured", /test request\(s\) went to the broker and 0 signed test job/); // 1d
  }
  expect(alter((r) => { verbJob("bench", ["-p", "b", "--no-run"], { testExecutables: [{ path: EXE, sha256: "h1" }] })(r); r.procs.push(proc(8.5, EXE, [], { end: 9, cwd: SNAP })); r.digests[EXE] = { sha256: "h1", mtimeMs: ms(8.2) }; }), "NN10", "not measured", /image of b-1\.exe at the moment it ran is not shown/); // 1a: a complete bench build still has no image witness
  expect(alter(() => {}), "NN10", "pass", /broker built no test/); // 1e: a clippy job is no test build
  expect(alter(verbJob("frobnicate", ["-p", "b"])), "NN10", "not measured", /signed job.*frobnicate.*not a verb known to build no tests/); // 1f
  expect(alter((r) => { r.procs.push(proc(7, "C:\\cargo\\bin\\cargo.exe", ["bench", "-p", "b"], { cwd: SNAP })); }), "NN10", "fail", /cargo test start\(s\) ran outside the broker/); // 1g
});

test("9.2 NN07 (table 2): a changed row's signed job that also builds a package no owed row needs fails", () => {
  const withZ = (r) => same(r, ["clippy", "--profile", "release", "-p", "b", "-p", "z", "--locked"]);
  expect(alter(() => {}), "NN07", "pass"); // 2a
  expect(alter(withZ), "NN07", "fail", /broker job j1 also builds z, which no owed row needs/); // 2b
  expect(alter((r) => { withZ(r); r.inventory.set("clippy:ws:z", { stage: "B", files: ["ws/z/src/lib.rs"] }); r.blobs["ws/z/src/lib.rs"] = "z1"; r.priorBlobs["ws/z/src/lib.rs"] = "z0"; r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["a", "b", "z"]\n'; }), "NN07", "pass"); // 2e: z is owed too
});

test("9.3 NN08 (table 3): the same program and arguments twice is a proven repeat only where both folders are known and equal", () => {
  const GITU = "/usr/bin/git"; const twice = (a, b) => (r) => { r.procs.push(proc(7, GITU, ["status"], a), proc(8, GITU, ["status"], b)); };
  expect(alter(twice({ cwd: "/src/a" }, { cwd: "/src/a" })), "NN08", "fail", /2x git status/); // 3a
  expect(alter(twice({ cwd: "/src/a" }, { cwd: "/src/b" })), "NN08", "pass"); // 3b
  for (const [a, b] of [[null, null], ["/src/a", null], [null, "/src/a"]]) expect(alter(twice({ cwd: a }, { cwd: b })), "NN08", "not measured", /possibly the same call/); // 3c
  expect(alter((r) => { r.procs.push(proc(7, GITU, ["status"], { cwd: null }), proc(8, GITU, ["log"], { cwd: null })); }), "NN08", "pass"); // 3d
});

test("9.4 NN12 (table 4): a recorded process is alive only when the process at its id started when it did; an unreadable start time is not shown", () => {
  const t0 = ms(0);
  assert.equal(LINESMOD.liveCheck({ pid: 5, start: t0, readStart: () => undefined }), false); // 4a no process
  assert.equal(LINESMOD.liveCheck({ pid: 5, start: t0, readStart: () => t0 + 500 }), true); // 4b same start
  assert.equal(LINESMOD.liveCheck({ pid: 5, start: t0, readStart: () => t0 + 60_000 }), false); // 4c a reused id
  assert.equal(LINESMOD.liveCheck({ pid: 5, start: t0, readStart: () => null }), null); // 4d unreadable
  assert.equal(LINESMOD.liveCheck({ pid: 5, start: Number.NaN, readStart: () => t0 }), null); // no recorded start
  // the judge: unknown liveness is never a pass, with the run ending 0 or not
  for (const exit of [0, 137]) expect(alter((r) => { r.launch.exit = exit; r.live = { ...r.live, alive: (pid) => (pid === GATE ? null : false), inventory: () => [{ pid: GATE }] }; }), "NN12", "not measured", /start time/);
});

test("9.5 NN06 (table 5): the toolchain version is in the launcher's fingerprint, so a toolchain change is not a stable baseline", () => {
  const run = (rustc) => (cmd, args) => { if (cmd === "rustc") { if (rustc === null) throw new Error("no rustc"); return `${rustc}\nbinary: rustc`; } return cmd === "git" && args.includes("rev-parse") ? "t".repeat(40) : `${cmd} 1.0`; };
  const a = fingerprintOf({ repo: "r", commit: C, run: run("rustc 1.80.0 (abc 2026-01-01)"), platform: "win32" });
  assert.equal(a.tools.rustc, "rustc 1.80.0 (abc 2026-01-01)"); // 5a
  assert.equal(sameFingerprint(a, structuredClone(a)), true);
  assert.equal(sameFingerprint(a, fingerprintOf({ repo: "r", commit: C, run: run("rustc 1.81.0 (def 2026-02-01)"), platform: "win32" })), false); // 5b
  const unread = fingerprintOf({ repo: "r", commit: C, run: run(null), platform: "win32" });
  assert.equal(sameFingerprint(unread, unread), false); // 5c
  expect(alter((r) => { unchangedB(r); r.launch.baseline.stable = false; r.procs.push(proc(6.5, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "a", "--locked"], { end: 7, cwd: `${SNAP}\\ws` })); }), "NN06", "not measured", /baseline's/);
});

test("9.6 NN09 (table 6): a job submitted after another ended is not shown to have been held back without a record of readiness and a free lane", () => {
  const tree = (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; };
  expect(alter((r) => { tree(r); gap9(r, 8.0); }), "NN09", "not measured", /submitted only after job j1 ended/); // 6b
  expect(alter((r) => { tree(r); const argv = ["clippy", "--profile", "release", "-p", "x", "--locked"]; r.procs.push(proc(6.0, RUSTRIGHT, ["cargo", ...argv], { end: 9.999, cwd: `${SNAP}\\other` })); r.receipts.push(job("j2", argv, { cwd: `${SNAP}\\other`, start: 8.0, end: 9.999, waits: { admission: 0, queue: 2000, bind: 0 } })); }), "NN09", "pass"); // 6c: the broker held it
});

test("6.15 the launcher reads the snapshot root once at the gate's exit, and records the gate by an absolute path (Astra SO-03 SO-14, Sol IP-04, Fable UO-11)", async () => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rk-launch-")); const fp = { gateCodeTree: "c".repeat(40), node: "v22", platform: "win32", tools: { rustfmt: "r", cargo: "c", "cargo-deny": "d", pnpm: "p", git: "g" } };
  const child = () => { const c = new EventEmitter(); c.pid = 999991; c.kill = () => {}; setImmediate(() => c.emit("exit", 0, null)); return c; };
  const go = async (name, snapshotsRoot, gate) => { const out = path.join(work, name, "run"); const code = await launch({ out, commit: C, receiptsDir: work, repo: work, gate, gateArgs: ["--to", "compile"], snapshotsRoot, spawnImpl: child, exportProcesses: (from, to) => ({ kind: "windows-security", from, to, newest: to, continuous: true, events: [], problems: [] }), fingerprint: fp, platform: "win32", host: "h" }); assert.equal(code, 0); return JSON.parse(fs.readFileSync(path.join(out, "launch.json"), "utf8")); };
  // one read: a spy counts the directory reads of the snapshot root between the gate's exit and the record
  const root = path.join(work, "store"); fs.mkdirSync(root); fs.writeFileSync(path.join(root, "k1.lock"), "x");
  const real = fs.readdirSync; let reads = 0; fs.readdirSync = (p, ...a) => { if (path.resolve(String(p)) === path.resolve(root)) reads += 1; return real(p, ...a); };
  let rec; try { rec = await go("a", root, "rel\\gate.mjs"); } finally { fs.readdirSync = real; }
  assert.equal(reads, 1, "the snapshot root is read once"); assert.equal(rec.afterRun.locksKnown, true); assert.equal(rec.afterRun.locks.length, 1);
  assert.equal(rec.gate.path, path.resolve(work, "rel\\gate.mjs"), "a relative gate path is recorded as the absolute path it resolved to");
  // a root that cannot be read is unknown, with no locks claimed
  const lost = await go("b", path.join(work, "missing-store"), path.join(work, "g.mjs")); assert.equal(lost.afterRun.locksKnown, false); assert.deepEqual(lost.afterRun.locks, []);
});

test("6.12 the 'present and never read' check can fail: a judge that reads a gate-written input is caught by the same spy", () => {
  const r = records(); decoy(r, "gateLog", "Class A failed"); decoy(r, "classAVerdict", { passed: false });
  // the spy is the one run() uses; a judge that touched the decoy would set decoyRead
  const args = {}; const watched = new Proxy({ ...args, ...r.decoys }, { get(t, k, rcv) { if (Object.hasOwn(r.decoys, k)) r.decoyRead = k; return Reflect.get(t, k, rcv); } });
  assert.equal(r.decoyRead, undefined); void watched.gateLog; assert.equal(r.decoyRead, "gateLog", "a read of the decoy is recorded");
  { const clean = alter((x) => { decoy(x, "gateLog", "Class A failed"); }); run(clean); assert.equal(clean.decoyRead, undefined, "the real judge reads nothing it was not given a name for"); }
  // the production reader, with legacy artifacts present, opens only the files it names (see 5.12); a reader that opened one would be caught by that test's assertion
});

test("10.2 (table 8): cargo's own rustc target-info probe is not a compile outside the broker; a rustc that compiles still is", () => {
  const RUSTC = "C:\\rust\\bin\\rustc.exe";
  const probe = ["-", "--crate-name", "___", "--print=file-names", "--crate-type", "bin", "--crate-type", "rlib", "--crate-type", "proc-macro", "--print=sysroot", "--print=split-debuginfo", "-"];
  expect(alter((r) => { r.procs.push(proc(0.02, RUSTC, probe, { cwd: SNAP })); }), "NN01", "pass"); // 8a: the form cargo runs for any cargo call
  // the exact command lines of run8 (cargo 1.99.1): more prints, a lint flag, and with --target for a target build
  const real = ["-", "--crate-name", "___", "--print=file-names", "--crate-type", "bin", "--crate-type", "rlib", "--crate-type", "dylib", "--crate-type", "cdylib", "--crate-type", "staticlib", "--crate-type", "proc-macro", "--print=sysroot", "--print=split-debuginfo", "--print=crate-name", "--print=cfg", "-Wwarnings"];
  const realTarget = [...real.slice(0, 4), "--target", "x86_64-pc-windows-msvc", ...real.slice(4)];
  expect(alter((r) => { r.procs.push(proc(0.02, RUSTC, real, { cwd: SNAP }), proc(0.03, RUSTC, realTarget, { cwd: SNAP })); }), "NN01", "pass");
  expect(alter((r) => { r.procs.push(proc(0.02, RUSTC, [...real, "src/lib.rs"], { cwd: SNAP })); }), "NN01", "fail", /compile\(s\) ran outside the broker/); // a source with the same options is a compile
  expect(alter((r) => { r.procs.push(proc(0.02, RUSTC, ["-", "--crate-name", "x", "--crate-type", "bin"], { cwd: SNAP })); }), "NN01", "fail", /compile\(s\) ran outside the broker/); // 8b: reads a crate from stdin and compiles it
  expect(alter((r) => { r.procs.push(proc(0.02, RUSTC, [...probe, "--emit=link"], { cwd: SNAP })); }), "NN01", "fail", /compile\(s\) ran outside the broker/); // 8c: an --emit is a compile
});

test("10.3 (table 9): a readiness taken from the last discovery probe is never positive evidence, so a wait on it is not shown, never a fail", () => {
  const waited = (r) => { r.readiness = new Map(); r.launch.lanes = 4; const f = byExe(r, RUSTFMT); f.start = ms(1.0); f.end = ms(3); r.procs.push(proc(3.5, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 5, cwd: SNAP })); };
  expect(alter(waited), "NN03", "not measured", /last discovery probe's exit/); // 9a: shellcheck started 3.45 s after the probe's exit with lanes free
  expect(alter((r) => { r.launch.lanes = 2; const f = byExe(r, RUSTFMT); f.start = ms(2.0); f.end = ms(2.1); }), "NN03", "fail", /ready at .* and started 1000 ms later/); // 9b: a readiness supplied from outside (GCX-03-3) still fails
});

test("10.6 the Security log export carries any command line exactly: quotes, backslashes, newlines, tabs, non-ASCII, markup and a whole JSON document (run7: ConvertTo-Json left a quote unescaped and a 6.4 MB record failed to parse)", () => {
  // The kind of command line that broke run7: another tool's argument that is itself a JSON document holding quoted text, with markup-like text added.
  const tricky = ['codex exec --json "{\\"input-messages\\":[],\\"last-assistant-message\\":\\"Scope reviewed:\\\\n- User requested: \\"Pin those packages and delete the old code\\".\\"}"', "second line", "tab\there", "caf\u00e9 \u2014 \u6f22\u5b57", "100% <done></Data> & 'quoted' &amp; &#65;"].join("\n");
  const raw = { oldest: at(-100), newest: at(3), continuous: true, records: 3, events: [{ id: 4688, time: at(0), data: { NewProcessId: "0x64", ProcessId: "0x32", NewProcessName: "C:\\node.exe", CommandLine: tricky } }, { id: 4689, time: at(1), data: { ProcessId: "0x64", ProcessName: "C:\\node.exe", Status: "0x0" } }] };
  const x = exportWindowsProcesses(at(-1), at(2), { run: fake(raw) });
  assert.equal(x.events[0].commandLine, tricky); assert.deepEqual(x.problems, []); assert.equal(x.continuous, true);
  // an element with no text is no value (null), never an empty command line
  assert.equal(parseWevtutilEvents("<Event><System><EventID>4688</EventID><EventRecordID>5</EventRecordID><TimeCreated SystemTime='" + at(0) + "'/></System><EventData><Data Name='CommandLine'/><Data Name='NewProcessName'></Data></EventData></Event>")[0].data.CommandLine, null);
  // a failed read is a problem, never an empty record
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => { throw Object.assign(new Error("x"), { stderr: "Access is denied." }); } }).problems[0], /Access is denied/);
  // the real tool: wevtutil itself answers a query the way the parser reads (skipped where this account cannot read the Security log)
  if (process.platform === "win32") {
    const now = new Date(); const live = exportWindowsProcesses(new Date(now - 5000).toISOString(), now.toISOString());
    if (!/could not be read/.test(live.problems.join())) { assert.ok(Number.isFinite(Date.parse(live.newest)), "the newest event has a time"); assert.ok(live.events.every((e) => Number.isInteger(e.pid)), "every event has a process id"); }
  }
});

test("10.7 the receipt verifier gets everything the installed rustright verify-receipt asks for: pinned key, key id, host and broker digest, and the receipt's own job id, nonce and request digest (run8: 7 receipts 'did not verify' for want of arguments)", () => {
  const body = JSON.stringify({ provenance: { payload: { jobId: "job-1", nonce: "ab".repeat(32), requestDigest: "cd".repeat(32) }, schema: "rustright.provenance.v1", signature: {} } });
  const argv = LINESMOD.verifierArgv({ file: "R.json", body, key: "04aa", keyId: "k1", host: "BOGUS-DELL", brokerDigest: "bd" });
  assert.deepEqual(argv, ["verify-receipt", "--receipt", "R.json", "--public-key", "04aa", "--key-id", "k1", "--host", "BOGUS-DELL", "--broker-digest", "bd", "--nonce", "ab".repeat(32), "--request-digest", "cd".repeat(32), "--job-id", "job-1"]);
  // a pin missing, or a receipt that names no nonce, request digest or job id, is not verified, never verified on less
  assert.equal(LINESMOD.verifierArgv({ file: "R.json", body, key: "04aa", keyId: "k1", host: "H", brokerDigest: "" }), null);
  for (const drop of ["jobId", "nonce", "requestDigest"]) { const p = JSON.parse(body); delete p.provenance.payload[drop]; assert.equal(LINESMOD.verifierArgv({ file: "R.json", body: JSON.stringify(p), key: "04aa", keyId: "k1", host: "H", brokerDigest: "bd" }), null, drop); }
  assert.equal(LINESMOD.verifierArgv({ file: "R.json", body: "not json", key: "04aa", keyId: "k1", host: "H", brokerDigest: "bd" }), null);
});

test("10.8 (table 12): NN05 fails a broker job no known row explains only when the inventory covers every kind of row; otherwise a row the test cannot see may own it (run8: 7 real jobs 'do no known row's work')", () => {
  // a signed job for a crate and verb no known row plans (a test build; the inventory lists clippy rows only)
  const stray = (r) => { const argv = ["test", "--profile", "release", "-p", "q", "--locked"]; r.procs.push(proc(6.2, RUSTRIGHT, ["cargo", ...argv], { end: 7.9, cwd: `${SNAP}\\ws` })); r.receipts.push(job("j9", argv, { start: 6.2, end: 7.9, waits: { admission: 0, queue: 0, bind: 0 } })); };
  expect(alter((r) => { stray(r); r.inventoryUncovered = ["test:rust"]; }), "NN05", "not measured", /do no known .*row's work/); // 12a: the inventory does not cover test rows
  expect(alter((r) => { stray(r); r.inventoryUncovered = []; }), "NN05", "fail", /broker job\(s\) do no known row's work/); // 12b: every kind covered, still no row owns it
});

test("10.9 (table 13): a rustright capability probe sends no work, so it is not an unknown client start (run8: 'rustright trampoline-capability' not shown to have sent work); another subcommand still is", () => {
  const client = (args) => (r) => { r.procs.push(proc(0.3, RUSTRIGHT, args, { cwd: SNAP })); };
  const gapsOf = (r) => whyOf(run(r), "NN02");
  assert.doesNotMatch(gapsOf(alter(client(["trampoline-capability"]))), /not a send or a known probe/); // 13a
  assert.match(gapsOf(alter(client(["frobnicate", "x"]))), /not a send or a known probe \(first: rustright frobnicate x\)/); // 13b
});

test("11.1 NN09 (table 14; EGM ruling on the HeardRight 2026-11-10 finding): commands that name one distinct target each are a verified split, because Cargo unifies features across the targets of one command; every other split keeps the existing rules", () => {
  const c = (argvs, solo, verdict, why) => expect(alter((r) => { split(r, ...argvs); policy(r, solo); }), "NN09", verdict, why);
  // 14a: one command per target (run8/run9: three clippy jobs for rightkit-time, one per triple)
  c(["-p b --target x", "-p b --target y", "-p b --target z"], [], "pass", /split by target \(x, y, z\)/);
  // 14b: the same target twice is a real duplicate
  c(["-p b --target x", "-p b --target x"], [], "fail", /b@x built more than once/);
  // 14c: the same target, different crates: not a split by target, the existing rule (no reason) applies
  c(["-p b --target x", "-p c --target x"], [], "fail", /sent as separate commands, and only 0 of them/);
  // 14d: settings differ by a known option: separate groups, as before
  c(["-p b --target x --features f", "-p b --target y"], [], "pass");
  // 14e: settings differ by an option the check does not know: not shown
  c(["-p b --target x --some-new-flag", "-p b --target y"], [], "not measured", /does not know the meaning of/);
  // 14f: a command naming two targets beside one naming one gets no split reason
  c(["-p c --target x --target y", "-p c --target z"], [], "fail", /sent as separate commands/);
  c(["-p c --target x --target y", "-p c --target x"], [], "fail", /c@x built more than once/);
  // 14g: a solo crate, one command per target, is what the gate does for it
  c(["-p c --target x", "-p c --target y"], ["c"], "pass", /c solo per the run's ci\/shards\.json/);
  // 14h: a solo crate built together with another crate in each target command still fails
  c(["-p b -p c --target x", "-p b -p c --target y"], ["c"], "fail", /must build alone \(c\) were built together/);
  // 14i: the crate set may differ per target (a crate is never asked for a target it cannot build)
  c(["-p b -p c --target x", "-p b --target y"], [], "pass", /split by target/);
  // no --target at all: nothing changed (GCX-09-1, 09-3, 09-9 and gate 11 keep their verdicts)
  c(["-p b", "-p c"], [], "fail", /sent as separate commands/);
});

test("10.5 (table 11): NN03 never fails for want of evidence: an owed Class A row with no program in the record, from an unreadable record or a whole one, is not shown", () => {
  expect(alter((r) => { r.witness = null; }), "NN03", "not measured", /no operating-system record/); // 11a: no process record at all (run7: Security log unreadable, NN03 FAIL)
  expect(alter((r) => { r.witness.problems = ["the Security log could not be read (Unexpected token)"]; r.witness.continuous = false; r.procs = r.procs.filter((p) => p.exe !== RUSTFMT); }), "NN03", "not measured", /could not be read/); // 11b: an incomplete record
  expect(alter((r) => { r.procs = r.procs.filter((p) => p.exe !== RUSTFMT); }), "NN03", "not measured", /owed a run .* no Class A program started/); // 11c: a whole record and the owed row never started: nothing shows it waited
});

test("10.4 (table 10): the snapshot lock's job holder exits when the gate's job drains, so it ends just after the gate; any other late program still fails", () => {
  const HOLDER = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
  const holderArgs = ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "D:\\snaps\\226c20f30c489153.lock.children\\job-holder-0ec397aa6842abd2.json.ps1"];
  expect(alter((r) => { r.procs.push(proc(0.5, HOLDER, holderArgs, { end: 10.138, cwd: null })); }), "NN12", "pass"); // 10a: ends 138 ms after the gate (10.0 s)
  expect(alter((r) => { r.procs.push(proc(0.5, HOLDER, holderArgs, { end: 14, cwd: null })); }), "NN12", "fail", /outlived the gate's exit/); // 10b: still there 4 s after the gate is not draining
  expect(alter((r) => { r.procs.push(proc(0.5, HOLDER, holderArgs, { end: null, cwd: null })); }), "NN12", "not measured", /no recorded exit/); // 10b: no exit is not shown gone
  expect(alter((r) => { r.procs.push(proc(0.5, HOLDER, ["-NoProfile", "-File", "D:\\work\\other.ps1"], { end: 10.138, cwd: null })); }), "NN12", "fail", /outlived the gate's exit/); // 10c: any other powershell
  expect(alter((r) => { r.procs.push(proc(0.5, HOLDER, holderArgs.map((a) => a.replace("job-holder-0ec397aa6842abd2", "job-holder-zz")), { end: 10.138, cwd: null })); }), "NN12", "fail", /outlived the gate's exit/); // 10c: a name that is not the holder's form
});

test("10.1 (table 7): the Windows console host is not a program of the run: it exits after its parent and starts once per console child", () => {
  const CONHOST = "C:\\Windows\\System32\\conhost.exe";
  const host = (s, end) => proc(s, CONHOST, ["0xffffffff", "-ForceV1"], { end, cwd: null }); // 7a: the form Windows gives a console child
  expect(alter((r) => { r.procs.push(host(1.0, 10.3)); }), "NN12", "pass"); // outlives the gate's exit at 10.0 s, and is not held against the run
  expect(alter((r) => { r.procs.push(host(2.0, 2.5), host(3.0, 3.5)); }), "NN08", "pass"); // one per console child, identical command lines, not a repeat
  expect(alter((r) => { r.procs.push(host(1.0, null)); }), "NN12", "pass"); // no recorded exit is not an open program of the run
  expect(alter((r) => { r.procs.push(proc(1.0, "D:\\tools\\conhost.exe", ["0xffffffff", "-ForceV1"], { end: 10.3, cwd: null })); }), "NN12", "fail", /outlived the gate's exit/); // 7b: a conhost elsewhere is a program
  expect(alter((r) => { r.procs.push(proc(1.0, CONHOST, ["--serve"], { end: 10.3, cwd: null })); }), "NN12", "fail", /outlived the gate's exit/); // 7c
});
