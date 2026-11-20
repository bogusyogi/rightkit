// Each of the twelve lines is shown to pass a run that meets it and to fail, or to say "not measured", for a run that breaks it or leaves
// no evidence of it, on small hand-made evidence of the same shape a real run leaves outside the gate (the operating system's process record,
// the broker's signed jobs, git, the history copied before the run, the cache store, the launcher). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import os from "node:os";
import path from "node:path";
import { CHECK_IDS, CONTRACT_FILE, judgeRun, linkProblems, parseAcceptanceLines, parseCommandLine, processesFromWindowsEvents, readAcceptanceLines } from "./acceptance-lines.mjs";
import { exportWindowsProcesses, kindOf, launch, sameFingerprint, takeLaunchLock } from "./acceptance-launch.mjs";

// The lines under test are the contract's own: docs/GATE-CONTRACT.md [GC-A27], read here exactly as the command line reads them.
const CONTRACT = fs.readFileSync(CONTRACT_FILE, "utf8");
const LINES = readAcceptanceLines(CONTRACT);
const twelve = Array.from({ length: 12 }, (_, i) => `NN${String(i + 1).padStart(2, "0")}`);
const withoutRow = (id) => CONTRACT.split(/\r?\n/).filter((l) => !l.startsWith(`| \`${id}\` |`)).join("\n");

test("the contract's GC-A27 table is the test's only list of lines: twelve, NN01 to NN12, each with a check and no check without a line", () => {
  assert.deepEqual(LINES.map((l) => l.id), twelve);
  assert.deepEqual([...CHECK_IDS], twelve);
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
  const added = CONTRACT.replace("<!-- acceptance-lines:end -->", "| `NN13` | A new line nobody wrote a check for. |\n<!-- acceptance-lines:end -->");
  assert.throws(() => readAcceptanceLines(added), (e) => e.problems.includes("NN13 is a line in the contract and has no check"));
  assert.deepEqual(linkProblems(LINES, [...CHECK_IDS, "NN13"]), ["NN13 has a check and is not a line in the contract"]);
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
    launch: { schema: "rightkit.acceptance-launch/3", runId: "f".repeat(64), commit: C, kind: "compile", host: "fixture-host", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, gatePid: GATE, launcherPid: LAUNCHER,
      lanes: 4, lanesSource: "this host's cores, handed to the gate as RK_GATE_JOBS", limitMs: 60_000, limitsSha256: "c".repeat(64), snapshotsRoot: "D:\\snaps",
      baseline: { commit: P, out: "D:\\prev", runId: "e".repeat(64), stable: true }, previous: { runId: "e".repeat(64), fromLastRun: true, exit: 0 },
      startedAt: at(0), startMono: "0", endedAt: at(10), endMono: String(10_000_000_000n), exit: 0, signal: null },
    witness: { kind: "windows-security", from: at(-2), to: at(12), newest: at(12), problems: [] },
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
    live: { exists: () => false, alive: () => false, locksIn: () => [] }, successor: null, verifierPinned: true, digests: {},
  };
}
const run = (r) => {
  const readWorking = () => { r.workingRead = true; return r.working; };
  return judgeRun({
    lines: LINES, launch: r.launch, processes: r.procs, witness: r.witness, receipts: r.receipts, inventory: r.inventory, inventoryUncovered: r.inventoryUncovered ?? [], prior: r.prior, readiness: r.readiness, verdictTimes: r.verdictTimes,
    git: { tree: T, blobOf: (p) => r.blobs[p] ?? null, priorBlobOf: (p) => r.priorBlobs[p] ?? null, readWorking, readTree: (p) => { if (!(p in r.tree)) throw Object.assign(new Error(`ENOENT ${p}`), { code: "ENOENT" }); return structuredClone(r.tree[p]); } },
    live: r.live, successor: r.successor, verifierPinned: r.verifierPinned, digestOf: (f) => r.digests[f] ?? null,
  });
};
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const byExe = (r, exe) => r.procs.find((p) => p.exe === exe);

test("a run whose evidence meets every line passes eleven; NN04 stays not measured until Adrian rewords it", () => {
  const v = run(records());
  for (const id of twelve.filter((x) => x !== "NN04")) assert.equal(verdictOf(v, id), "pass", `${id}: ${whyOf(v, id)}`);
  assert.equal(verdictOf(v, "NN04"), "not measured");
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
const wrapped = (s, line, end = s + 0.1) => proc(s, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", line], { end, cwd: null });
const FROZEN = [
  // NN01: only the selected commit is built.
  ["GCX-01-1", "NN01", "not measured", (r) => { r.receipts = []; }], // a broker compile with no signed job
  ["GCX-01-2", "NN01", "not measured", (r) => { byExe(r, RUSTFMT).cwd = null; }], // fresh Class A work whose snapshot cannot be placed
  ["GCX-01-3", "NN01", "fail", (r) => { byExe(r, RUSTFMT).cwd = "D:\\snap\\other"; }], // Class A work read another tree
  ["GCX-01-4", "NN01", "fail", (r) => { r.receipts[0].doc.provenance.payload.gate.tree = "e".repeat(40); }], // signed for another tree
  ["gate 13", "NN01", "not measured", (r) => { delete r.receipts[0].doc.provenance.payload.gate.snapshotUnchanged; }], // nothing says the snapshot held after the compile
  // NN02: Class A finishes before anything goes to the broker; if it fails, nothing goes.
  ["GCX-02-1", "NN02", "fail", (r) => { r.procs.push(wrapped(1.05, '""C:\\Program Files\\RustRight\\rustright.cmd" cargo metadata --format-version 1"')); }], // a quoted wrapper-only send during Class A
  ["GCX-02-2", "NN02", "not measured", (r) => { noBroker(r); r.procs[0].end = null; }], // the gate process has no recorded exit, and nothing else shows nothing was sent
  ["GCX-02-3", "NN02", "not measured", (r) => { r.receipts[0].doc.provenance.payload.startedAt = at(1.2); r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); }], // the signed submission ties with Class A's end
  ["GCX-02-4", "NN02", "fail", (r) => { r.verdictTimes = new Map([[VERDICT, ms(-100)]]); }], // admitted on a Class A verdict from before this run
  ["gate 9", "NN02", "pass", (r) => { r.procs.push(proc(9, DENY, ["check"], { exit: 1, cwd: "D:\\other-unit" })); }], // a nested unit's failing Class A tool elsewhere is not this run's
  // NN03: Class A starts ready, independent work whenever an eligible lane is free.
  ["GCX-03-1", "NN03", "not measured", (r) => { r.launch.lanes = 1.5; }],
  ["GCX-03-2", "NN03", "not measured", (r) => { const f = byExe(r, RUSTFMT); f.end = f.start - 1; }],
  ["GCX-03-3", "NN03", "fail", (r) => { r.launch.lanes = 2; const f = byExe(r, RUSTFMT); f.start = ms(2.0); f.end = ms(2.1); }], // one ready row waits a second beside free capacity
  ["GCX-03-4", "NN03", "pass", (r) => { r.launch.lanes = 20; }], // the same row ready and started at once on twenty lanes
  ["gate 4", "NN03", "fail", (r) => { r.readiness.set("shellcheck", ms(1.0)); const f = byExe(r, RUSTFMT); f.start = ms(1.0); f.end = ms(3.0); r.procs.push(proc(3.0, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 5, cwd: SNAP })); }], // two ready programs run one after the other on four lanes
  // NN05: if lookup disagrees with the record the gate stops before sending work; otherwise the work that ran matches it.
  ["GCX-05-1", "NN05", "not measured", (r) => { r.inventory = inv([["rustfmt:2021", "A", ["src/a.rs", "src/b.rs"]], ["deny", "A", ["Cargo.lock"]]]); }], // Class B work observed with no Class B row known
  ["GCX-05-2", "NN05", "fail", (r) => { noBroker(r); }], // positive owed work (b changed) with no program or signed job
  ["GCX-05-3", "NN05", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.requestedArgv.push("-p", "z"); }], // a valid crate cannot excuse an extra one in the same call
  ["GCX-05-5", "NN05", "fail", (r) => { byExe(r, RUSTFMT).args.push("src/unplanned.rs"); }], // an unplanned operand beside valid work
  ["gate 8", "NN05", "fail", (r) => { unchangedB(r); }], // b is unchanged and a broker job still built it
  // NN06: a row whose inputs did not change is never run again, including one that failed before.
  ["GCX-06-1", "NN06", "fail", (r) => { r.blobs["src/a.rs"] = "a0"; }], // rustfmt's inputs are unchanged and rustfmt still started
  ["GCX-06-3", "NN06", "not measured", (r) => { r.launch.exit = null; r.launch.signal = "SIGKILL"; r.launch.endedAt = null; r.launch.endMono = null; }], // the run's outcome is not known, so an unchanged failure's is not
  ["gate 1", "NN06", "fail", (r) => { r.procs.push(proc(1.7, DENY, ["check"], { end: 1.9, cwd: SNAP })); }], // deny is unchanged and its tool visibly started
  // NN07: a row whose inputs did change is always run again, and does only the work the change needs.
  ["GCX-07-1", "NN07", "fail", (r) => { r.blobs["src/b.rs"] = "b2"; }], // a.rs and b.rs changed; only a.rs reached rustfmt
  ["GCX-07-2", "NN07", "fail", (r) => { byExe(r, RUSTFMT).args = ["--check", "--edition", "2021"]; }],
  ["GCX-07-3", "NN07", "pass", (r) => { r.blobs["src/b.rs"] = "b2"; byExe(r, RUSTFMT).args.push("src/b.rs"); }],
  ["GCX-07-4", "NN07", "not measured", (r) => { r.receipts[0].verified = { ok: false, problems: ["signature does not verify"] }; }], // the job's evidence is not trusted
  ["GCX-07-5", "NN07", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.requestedArgv = ["clippy", "--profile", "release", "-p", "ws", "--locked"]; }],
  ["GCX-07-6", "NN07", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.workspace = `${SNAP}\\other`; r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; }],
  ["gate 12", "NN07", "fail", (r) => { const id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.inventory.delete("clippy:ws:b"); r.inventory.set(id, { stage: "B", files: ["ws/b/src/lib.rs"] }); r.receipts[0].doc.provenance.payload.request.requestedArgv = ["clippy", "--profile", "release", "--target", "x86_64-pc-windows-msvc", "-p", "b", "--locked"]; }],
  // NN08: nothing is started twice in one run.
  ["GCX-08-1", "NN08", "pass", (r) => { r.procs.push(proc(7, "/opt/Tool", ["probe"], { cwd: "/src" }), proc(8, "/opt/tool", ["probe"], { cwd: "/src" })); }],
  ["GCX-08-2", "NN08", "pass", (r) => { r.procs.push(proc(7, "/usr/bin/git", ["status"], { cwd: "/src/A" }), proc(8, "/usr/bin/git", ["status"], { cwd: "/src/a" })); }],
  ["GCX-08-3", "NN08", "fail", (r) => { const nested = proc(7, "C:\\node\\node.exe", ["nested-gate.mjs"], { end: 9 }); r.procs.push(nested, proc(8, RUSTFMT, ["--check", "--edition", "2021", "src/a.rs"], { ppid: nested.pid, cwd: SNAP })); }], // the same call by two writers
  ["gate 5", "NN08", "fail", (r) => { const g = r.procs[1]; r.procs.push(wrapped(0.5, `"${g.exe}" ${g.args.join(" ")}`)); }], // the direct call written again through cmd /s with no child shown
  ["gate 6", "NN08", "fail", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, [...g.args], { cwd: `${g.cwd}\\` })); }], // the same folder spelled with a trailing separator
  ["gate 7", "NN08", "pass", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, ["-c", "core.fsmonitor=false", ...g.args], { cwd: g.cwd })); }],
  // NN09: compatible compiles combined per workspace and stage; every split has a reason the check can verify; independent groups run together.
  ["GCX-09-1", "NN09", "fail", (r) => { split(r, "-p b -p c", "-p c"); policy(r, ["c"]); }],
  ["GCX-09-2", "NN09", "fail", (r) => { split(r, "-p c --target x", "-p c --target y"); policy(r, ["c"]); }],
  ["GCX-09-3", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, [], ["c"]); }], // the run's tree names no solo crate; a later checkout does
  ["GCX-09-4", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, ["c"], []); }], // the run's tree names c solo; a later checkout withdraws it
  ["GCX-09-5", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null, ["c"]); }], // the run's tree has no policy; a later checkout would justify the split
  ["GCX-09-6", "NN09", "not measured", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; r.procs.push(proc(7.999, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "x", "--locked"], { end: 9, cwd: `${SNAP}\\other` })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "x", "--locked"], { cwd: `${SNAP}\\other`, start: 8.099 })); }], // one ms of overlap, no word on readiness or capacity
  ["GCX-09-7", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json"] = JSON.stringify({ bundle: { externalBin: ["binaries/c-sidecar"] } }); }],
  ["GCX-09-8", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null); }], // only a printed reason existed; the tree shows none
  ["GCX-09-9", "NN09", "fail", (r) => { split(r, "-p b", "-p b"); policy(r, ["b"]); }],
  ["gate 11", "NN09", "fail", (r) => { r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "sub"]\n'; r.tree["ws/sub/Cargo.toml"] = '[package]\nname = "c"\n'; r.procs.push(proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "c", "--locked"], { end: 8, cwd: `${SNAP}\\ws\\sub` })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "c", "--locked"], { cwd: `${SNAP}\\ws\\sub` })); }],
  // NN10: tests are compiled by the broker and run by the gate.
  ["GCX-10-1", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.receipts = []; }], // a test request and the program's launch with no signed manifest
  ["GCX-10-2", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE }] }); r.verifierPinned = false; r.receipts[0].verified = { ok: false, problems: ["no broker key is pinned"] }; }], // a filename and no authentication
  // NN11: the run finishes inside its time limit.
  ["GCX-11-1", "NN11", "not measured", (r) => { r.launch.endMono = null; r.launch.endedAt = null; r.launch.exit = null; }],
  ["GCX-11-2", "NN11", "fail", (r) => { r.launch.limitMs = 9999; }],
  ["GCX-11-3", "NN11", "pass", (r) => { r.launch.limitMs = 10000; }],
  ["gate 10", "NN11", "fail", (r) => { r.launch.limitMs = 15_000; r.launch.endedAt = at(40); r.launch.endMono = String(40_000_000_000n); r.witness.to = at(42); r.witness.newest = at(42); r.procs[0].start = ms(30); r.procs[0].end = ms(40); }], // the gate itself began 30 s after the launcher: a span taken from its own start would hide them
  // NN12: the run ends clean, even if killed: the next ordinary run starts safely by itself.
  ["GCX-12-1", "NN12", "not measured", (r) => { r.launch.exit = 137; }],
  ["GCX-12-2", "NN12", "fail", (r) => { r.launch.exit = 137; r.live = { exists: () => false, alive: (pid) => pid === GATE, locksIn: () => [] }; }], // the gate process itself survived
  ["gate 2", "NN12", "not measured", (r) => { r.witness = null; }], // no record of the run's programs
  ["gate 3", "NN12", "not measured", (r) => { r.procs = r.procs.filter((p) => p.pid !== GATE); }], // the record does not hold the gate process
];
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
];
test("the frozen step-2 batch: all 63 cases accounted for, 56 run in outside evidence with their author's id and verdict, 7 listed with why they have no outside form", async (t) => {
  assert.equal(FROZEN.length + NO_OUTSIDE_FORM.length, 63);
  assert.equal(new Set([...FROZEN, ...NO_OUTSIDE_FORM].map(([id]) => id)).size, 63, "every id once");
  for (const [id, line, verdict, fn] of FROZEN) {
    await t.test(`${id} ${line}: expected ${verdict}`, (st) => { const r = alter(fn); const v = run(r); st.diagnostic(`${id} ${line}: got ${verdictOf(v, line)}: ${whyOf(v, line).slice(0, 200)}`); assert.equal(verdictOf(v, line), verdict, `${id} ${line}: ${whyOf(v, line)}`); assert.equal(r.workingRead, undefined, "the mutable checkout is never read"); });
  }
  for (const [id, line, verdict, why] of NO_OUTSIDE_FORM) await t.test(`${id} ${line}: expected ${verdict}: NO OUTSIDE FORM (${why})`, (st) => { st.diagnostic(`${id} has no form in evidence the gate does not write: ${why}`); });
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
  assert.match(takeLaunchLock(ghost, { runId: "u", pid: process.pid }, { fsApi: fsRace }).refused, /another launcher took over/);
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
  const raw = { oldest: at(-100), newest: at(3), events: [{ id: 4688, time: at(0), data: { NewProcessId: "0x64", ProcessId: "0x32", NewProcessName: "C:\\node.exe", CommandLine: "node gate.mjs" } }, { id: 4689, time: at(1), data: { ProcessId: "0x64", ProcessName: "C:\\node.exe", Status: "0x0" } }] };
  const x = exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify(raw) });
  assert.deepEqual(x.problems, []); assert.equal(x.newest, at(3));
  assert.deepEqual(x.events.map((e) => [e.id, e.pid, e.ppid ?? null]), [[4688, 100, 50], [4689, 100, null]]);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, oldest: at(1) }) }).problems[0], /oldest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, newest: at(-50) }) }).problems.join(), /newest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => { throw Object.assign(new Error("x"), { stderr: "Access is denied." }); } }).problems[0], /Access is denied/);
});
test("NN10: a verified compile-only test build whose program the gate ran, as the broker built it, passes", () => {
  const ok = (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.digests[EXE] = "h1"; };
  expect(alter(ok), "NN10", "pass");
  expect(alter((r) => { ok(r); r.digests[EXE] = "h2"; }), "NN10", "fail", /not the program the broker built/);
  expect(alter((r) => { ok(r); r.procs = r.procs.filter((p) => p.exe !== EXE); }), "NN10", "fail", /never ran it/);
  expect(alter((r) => { ok(r); delete r.receipts[0].doc.provenance.payload.testExecutables; }), "NN10", "fail", /compile only/);
  expect(alter((r) => { ok(r); r.receipts[0].doc.provenance.payload.testExecutables = []; }), "NN10", "not measured", /no test program/);
  expect(alter((r) => { r.procs.push(proc(8, "C:\\cargo\\bin\\cargo.exe", ["test", "-p", "b"])); }), "NN10", "fail", /outside the broker/);
});
test("NN12: a killed run passes only on the next ordinary run that names it, started after it, and ended 0; the lock is looked for in the launcher's snapshot root", () => {
  const killed = (r) => { r.launch.exit = 137; };
  const next = (r, over = {}) => ({ launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), ...over } });
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
