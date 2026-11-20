// Each of the twelve lines is shown to pass a run that meets it and to fail, or to say "not measured", for a run that breaks it or leaves
// no evidence of it, on small hand-made evidence of the same shape a real run leaves outside the gate (the operating system's process record,
// the broker's signed jobs, git, the history copied before the run, the cache store, the launcher). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import os from "node:os";
import path from "node:path";
import { CHECK_IDS, CONTRACT_FILE, judgeRun, linkProblems, parseAcceptanceLines, parseCommandLine, processesFromWindowsEvents, readAcceptanceLines, readRunDir } from "./acceptance-lines.mjs";
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
    diskBlobs: (abs, rel) => (Object.hasOwn(r.diskOverride, rel) ? r.diskOverride[rel] : r.blobs[rel] ?? null),
  }));
};
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const byExe = (r, exe) => r.procs.find((p) => p.exe === exe);

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
const pair = (r, id, argv, cwd, startedAt, terminalAt) => { r.procs.push(proc(startedAt - 0.1, RUSTRIGHT, ["cargo", ...argv], { end: terminalAt, cwd })); r.receipts.push(job(id, argv, { cwd, start: startedAt, end: terminalAt })); };
const wrapped = (s, line, end = s + 0.1) => proc(s, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", line], { end, cwd: null });
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
  ["gate 9", "NN02", "pass", (r) => { decoy(r, "gateLog", "Class A failed: cargo-deny exited 1"); decoy(r, "classAVerdict", { passed: false }); r.procs.push(proc(9, DENY, ["check"], { exit: 1, cwd: "D:\\other-unit" })); }], // a failing tool in another unit's folder after the broker work, with misleading text in the gate's log present and never read (restored)
  // NN03: Class A starts ready, independent work whenever an eligible lane is free.
  ["GCX-03-1", "NN03", "not measured", (r) => { r.launch.lanes = 1.5; }],
  ["GCX-03-2", "NN03", "not measured", (r) => { const f = byExe(r, RUSTFMT); f.end = f.start - 1; }],
  ["GCX-03-3", "NN03", "fail", (r) => { r.launch.lanes = 2; const f = byExe(r, RUSTFMT); f.start = ms(2.0); f.end = ms(2.1); }], // one ready row waits a second beside free capacity
  ["gate 4", "NN03", "fail", (r) => { decoy(r, "gateLanes", 1); r.readiness.set("shellcheck", ms(1.0)); const f = byExe(r, RUSTFMT); f.start = ms(1.0); f.end = ms(3.0); r.procs.push(proc(3.0, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 5, cwd: SNAP })); }], // a gate-reported lane count of one is present and never read: the launcher's four lanes decide (restored)
  ["GCX-03-4", "NN03", "pass", (r) => { r.launch.lanes = 20; }], // the same row ready and started at once on twenty lanes
  // NN05: if lookup disagrees with the record the gate stops before sending work; otherwise the work that ran matches it.
  ["GCX-05-1", "NN05", "not measured", (r) => { r.inventory = inv([["rustfmt:2021", "A", ["src/a.rs", "src/b.rs"]], ["deny", "A", ["Cargo.lock"]]]); }], // Class B work observed with no Class B row known
  ["GCX-05-3", "NN05", "fail", (r) => { same(r, ["clippy", "--profile", "release", "-p", "b", "-p", "z", "--locked"]); }], // the observed call and its signed job both name an extra crate
  ["GCX-05-5", "NN05", "fail", (r) => { r.procs.push(proc(1.2, "C:\\tools\\shellcheck.exe", ["x.sh"], { end: 1.3, cwd: SNAP })); }], // a tool no row plans starts beside the valid work (the original's independent secret-scan dispatch)
  // NN06: a row whose inputs did not change is never run again, including one that failed before.
  ["GCX-06-1", "NN06", "fail", (r) => { r.blobs["src/a.rs"] = "a0"; }], // rustfmt's inputs are unchanged and rustfmt still started
  ["GCX-06-3", "NN06", "not measured", (r) => { r.prior = { failedRows: new Set(["clippy:ws:a"]) }; byExe(r, RUSTFMT).exit = 1; r.launch.exit = 1; }], // the run is red, but another row's program failed, so the reused failure's own outcome is not shown
  ["gate 1", "NN06", "fail", (r) => { r.procs.push(proc(1.7, DENY, ["check"], { end: 1.9, cwd: SNAP })); }], // deny is unchanged and its tool visibly started
  // NN07: a row whose inputs did change is always run again, and does only the work the change needs.
  ["GCX-07-1", "NN07", "fail", (r) => { r.blobs["src/b.rs"] = "b2"; }], // a.rs and b.rs changed; only a.rs reached rustfmt
  ["GCX-07-2", "NN07", "fail", (r) => { byExe(r, RUSTFMT).args = ["--check", "--edition", "2021"]; }],
  ["GCX-07-3", "NN07", "pass", (r) => { r.blobs["src/b.rs"] = "b2"; byExe(r, RUSTFMT).args.push("src/b.rs"); }],
  ["GCX-07-4", "NN07", "not measured", (r) => { delete r.receipts[0].doc.provenance.payload.request.workspace; }], // the signed job names no workspace: the compile activity stays, but which row's crate it built is not shown
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
  ["GCX-09-2", "NN09", "fail", (r) => { split(r, "-p c --target x", "-p c --target y"); policy(r, ["c"]); }],
  ["GCX-09-3", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, [], ["c"]); }], // the run's tree names no solo crate; a later checkout does
  ["GCX-09-4", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, ["c"], []); }], // the run's tree names c solo; a later checkout withdraws it
  ["GCX-09-5", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null, ["c"]); }], // the run's tree has no policy; a later checkout would justify the split
  ["GCX-09-6", "NN09", "not measured", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; pair(r, "j2", ["clippy", "--profile", "release", "-p", "x", "--locked"], `${SNAP}\\other`, 7.999, 9.999); }], // the second compile is submitted one ms before the first ends: one ms of overlap, no word on readiness or capacity
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
  // NN12: the run ends clean, even if killed: the next ordinary run starts safely by itself.
  ["gate 2", "NN12", "not measured", (r) => { decoy(r, "gateLiveList", []); r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [] }; }], // the operating system's listing is empty while the probe finds the gate alive; a pid list the gate wrote is present and never read (restored)
  ["gate 3", "NN12", "not measured", (r) => { decoy(r, "gateLiveList", [LAUNCHER]); r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: LAUNCHER }, { pid: 4 }] }; }], // the listing omits the gate process the probe finds alive (restored)
  ["GCX-12-1", "NN12", "not measured", (r) => { r.launch.exit = 137; }],
  ["GCX-12-2", "NN12", "fail", (r) => { r.launch.exit = 137; const d = proc(5, "C:\\tools\\child.exe", ["x"], { end: null, exit: null, cwd: SNAP }); r.procs.push(d); r.live = { ...r.live, alive: (pid) => pid === d.pid }; }], // a program of the run is still going when the killed gate has long gone (the surviving process)
];
// The specific reason each case's verdict must give (so it cannot be right for an unrelated one).
const REASONS = {
  "GCX-01-1": /broker client compile start\(s\) and 0 signed job/, "GCX-01-2": /ran in a folder the process record does not keep/, "GCX-01-3": /ran outside the selected snapshot/, "GCX-01-4": /built tree eeeeeeeeeeee, not the selected commit/, "gate 13": /does not say whether the snapshot stayed unchanged/,
  "GCX-02-1": /broker client started \(rustright cargo metadata\) before Class A finished/, "GCX-02-2": /has no recorded exit/, "GCX-02-3": /same moment Class A ended/, "GCX-02-4": /admitted on a Class A verdict .* written before this run began/, "gate 9": /Class A passed/,
  "GCX-03-1": /no lane count/, "GCX-03-2": /ending before it started/, "GCX-03-3": /was ready at .* and started 1000 ms later, \d+ ms of it with a lane free of 2/, "GCX-03-4": /none waited while a lane of 20 was free/, "gate 4": /shellcheck was ready at .* and started 2000 ms later, \d+ ms of it with a lane free of 4/,
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
  "GCX-01-3": /was handed src\/a\.rs, whose bytes on disk are not the selected commit's/, "gate 2": /process listing is empty/, "gate 3": /process listing omits node 100/, "gate 4": /shellcheck was ready at .* and started 2000 ms later, \d+ ms of it with a lane free of 4/,
  "gate 9": /Class A passed/, "GCX-09-8": /ci\/shards\.json at the run's tree cannot be read/, "GCX-10-2": /carries no manifest of test programs/,
});
Object.assign(REASONS, { "GCX-01-2": /was handed src\/a\.rs and its bytes on disk after the run were not read/, "GCX-05-5": /shellcheck ran and no known row plans it/, "GCX-07-4": /workspace/ });
const EXTRA_TWINS = [
  ["gate 2 twin", "NN12", "fail", (r) => { r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: GATE }] }; }], // the same live gate with the listing showing it: the gate is still running
  ["gate 3 twin", "NN12", "fail", (r) => { r.live = { ...r.live, alive: (pid) => pid === GATE, inventory: () => [{ pid: LAUNCHER }, { pid: GATE }] }; }], // the listing that includes the gate process
  ["GCX-01-2 twin", "NN01", "pass", () => {}], // the same run with every file rustfmt was handed read back after the run
  ["GCX-02-2 twin", "NN02", "pass", (r) => { unchangedB(r); noBroker(r); byExe(r, RUSTFMT).exit = 1; r.procs.push({ pid: 999, ppid: 4242, exe: GIT, args: ["status"], cwd: null, start: ms(9), end: ms(10), exit: 0 }); }], // the same, with the gate's exit recorded: Class A failed and nothing went to the broker
  ["GCX-02-3 twin", "NN02", "pass", (r) => { byExe(r, RUSTRIGHT).start = ms(1.6); }], // the same client a half second after Class A ended
  ["GCX-06-3 twin", "NN06", "pass", (r) => { r.prior = { failedRows: new Set(["clippy:ws:a"]) }; r.launch.exit = 1; }], // the same red run with no other row failing: the reused failure kept it red
  ["GCX-07-4 twin", "NN07", "pass", () => {}], // the signed receipt names its command
  ["GCX-07-5 twin", "NN07", "pass", (r) => { same(r, ["clippy", "--profile", "release", "-p", "b", "--locked"]); }], // the observed compile selected crate b
  ["GCX-07-6 twin", "NN07", "pass", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; same(r, ["clippy", "--profile", "release", "-p", "b", "--locked"], `${SNAP}\\ws`); }], // the same compile in the owed workspace
  ["gate 12 twin", "NN07", "pass", (r) => { const id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.inventory.delete("clippy:ws:b"); r.inventory.set(id, { stage: "B", files: ["ws/b/src/lib.rs"] }); same(r, ["clippy", "--profile", "release", "--target", "x86_64-unknown-linux-gnu", "-p", "b", "--locked"]); }], // the compile for the row's own target
  ["gate 10 twin", "NN11", "pass", (r) => { r.launch.limitMs = 10_500; }], // the gate starting with the launcher
  ["gate 10 substitute", "NN11", "fail", (r) => { r.launch.limitMs = 10_500; r.procs[0].start = ms(-1); }], // the gate began one second before the launcher's own start (the earliest the record allows)
  ["GCX-08-1 twin", "NN08", "fail", (r) => { r.procs.push(proc(7, "/opt/tool", ["probe"], { cwd: "/src" }), proc(8, "/opt/tool", ["probe"], { cwd: "/src" })); }], // the same Unix program twice in one folder
  ["GCX-08-2 twin", "NN08", "fail", (r) => { r.procs.push(proc(7, "/usr/bin/git", ["status"], { cwd: "/src/a" }), proc(8, "/usr/bin/git", ["status"], { cwd: "/src/a" })); }], // the same Unix folder twice
  ["gate 5 control", "NN08", "not measured", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", `"${g.exe}" ${g.args.join(" ")}`], { end: 0.6, cwd: "D:\\snap\\other" })); }], // the same wrapper written in another known folder: not the same call, and no child shows what it ran
  ["gate 9 twin", "NN02", "fail", (r) => { r.procs.push(proc(9, DENY, ["check"], { exit: 1, cwd: SNAP })); }], // the same failing tool, in this run's snapshot
  ["GCX-09-7 twin", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, []); }], // the same split with no sidecar mark in the tree
];
const TWINS = [["GCX-03-4", "GCX-03-3", "fail"], ["GCX-07-3", "GCX-07-1", "fail"], ["GCX-09-4", "GCX-09-3", "fail"], ["GCX-11-3", "GCX-11-2", "fail"], ["GCX-09-7", "GCX-09-7 twin", "fail"], ["GCX-08-1", "GCX-08-1 twin", "fail"], ["GCX-08-2", "GCX-08-2 twin", "fail"], ["gate 7", "gate 6", "fail"], ["GCX-02-2 twin", "GCX-02-2", "not measured"], ["GCX-02-3 twin", "GCX-02-3", "not measured"], ["GCX-06-3 twin", "GCX-06-3", "not measured"], ["GCX-07-4 twin", "GCX-07-4", "not measured"], ["GCX-07-5 twin", "GCX-07-5", "fail"], ["GCX-07-6 twin", "GCX-07-6", "fail"], ["gate 12 twin", "gate 12", "fail"], ["gate 10 twin", "gate 10 substitute", "fail"], ["gate 5 control", "gate 5", "fail"], ["gate 2 twin", "gate 2", "not measured"], ["gate 3 twin", "gate 3", "not measured"], ["GCX-01-2 twin", "GCX-01-2", "not measured"]];
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
  ["gate 10", "NN11", "fail", "an owned process 30 s before the launcher's start: a child cannot start before its parent, so no record outside the gate can show it consistently (its nearest outside form is the case 'gate 10 substitute': the gate itself starting before the launcher)"],
  ["gate 8", "NN05", "fail", "the gate's stop line naming clippy:ws:bb for clippy:ws:b: only the gate's own words name a row"],
  ["GCX-02-4", "NN02", "fail", "a Class A verdict belonging to another run: the verdict file is written by the gate, and NN02 no longer reads it"],
  ["GCX-05-2", "NN05", "fail", "a claim recorded as dispatched and run in the gate's decision: gate-written"],
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
  for (const [id, twin, expected] of TWINS) await t.test(`${id} twin ${twin}: differs only in the decisive fact`, () => { const find = (x) => FROZEN.find(([y]) => y === x) ?? EXTRA_TWINS.find(([y]) => y === x); const a = find(id); const b = find(twin); const va = run(alter(a[3])); const vb = run(alter(b[3])); assert.notEqual(verdictOf(va, a[1]), verdictOf(vb, b[1]), "the twin must give the other verdict"); assert.equal(verdictOf(va, a[1]), a[2], `${id}: ${whyOf(va, a[1])}`); assert.equal(verdictOf(vb, b[1]), expected, `${twin}: ${whyOf(vb, b[1])}`); });
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
  const x = exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify(raw) });
  assert.deepEqual(x.problems, []); assert.equal(x.newest, at(3)); assert.equal(x.continuous, true);
  // A break in the record ids of the window (a lost start or exit) is a problem, and the record is then not continuous.
  { const broken = exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, continuous: false, records: 7 }) }); assert.equal(broken.continuous, false); assert.match(broken.problems.join(), /record ids have a break/); }
  assert.deepEqual(x.events.map((e) => [e.id, e.pid, e.ppid ?? null]), [[4688, 100, 50], [4689, 100, null]]);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, oldest: at(1) }) }).problems[0], /oldest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, newest: at(-50) }) }).problems.join(), /newest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => { throw Object.assign(new Error("x"), { stderr: "Access is denied." }); } }).problems[0], /Access is denied/);
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
  const next = (r, over = {}) => ({ launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), snapshotsRoot: "D:\\snaps", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, ...over } });
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
  const next = (r, over = {}) => ({ launch: { previous: { runId: r.launch.runId, fromLastRun: true }, exit: 0, host: "fixture-host", startedAt: at(20), snapshotsRoot: "D:\\snaps", gate: { path: "bin\\rightkit-gate.mjs", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C] }, ...over } });
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
  expect(alter((r) => { r.readiness = new Map(); }), "NN03", "not measured", /for every program that started/);
  // NN08: an unknown folder on one side leaves two otherwise identical calls possibly the same call, never distinct.
  expect(alter((r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, [...g.args], { cwd: null })); }), "NN08", "fail", /2x git rev-parse/);
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
  };
  assert.equal(SUBSTITUTES.length, Object.keys(SUB_REASONS).length);
  { const tw = EXTRA_TWINS.find(([x]) => x === "gate 9 twin"); const v = run(alter(tw[3])); assert.equal(verdictOf(v, "NN02"), "fail", "the same failing tool in this run's snapshot is Class A failing"); }
  for (const [id, line, verdict, fn] of SUBSTITUTES) await t.test(`${id} ${line}: ${verdict}`, () => { const v = run(alter(fn)); assert.equal(verdictOf(v, line), verdict, `${id}: ${whyOf(v, line)}`); assert.match(whyOf(v, line), SUB_REASONS[id]); });
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
  assert.ok(opened.every((n) => ["launch.json", "processes.json", "receipts.txt"].includes(n)), `unexpected opens: ${opened.join(", ")}`);
  assert.equal(withLegacy.processes.length, 1); assert.deepEqual(withLegacy.processes, without.processes); assert.deepEqual(withLegacy.launch, without.launch);
});

test("5.12 the ledger: all 63 originals, each with its disposition and the case that stands in for it", (t) => {
  const SUBSTITUTE_FOR = { "gate 8": "gate 8 (unchanged row built)", "GCX-05-2": "GCX-05-2 (owed, nothing seen)", "gate 10": "gate 10 substitute" };
  const RESTORED = ["gate 2", "gate 3", "gate 4", "gate 9", "GCX-09-8", "GCX-10-2", "GCX-01-3"];
  const RESTATED = ["gate 5", "GCX-01-2", "GCX-05-5", "GCX-07-4", "GCX-09-6", "GCX-12-2"];
  const where = (id) => SUBSTITUTES.find(([x]) => x === id) ?? EXTRA_TWINS.find(([x]) => x === id);
  const rows = [];
  for (const [id, line, verdict] of FROZEN) rows.push([id, line, verdict, RESTORED.includes(id) ? "executable: restored under its original id, misleading input present and unread" : RESTATED.includes(id) ? "executable: restated" : "executable", "-"]);
  for (const [id, line, verdict, why] of NO_OUTSIDE_FORM) rows.push([id, line, verdict, `superseded: ${why}`, SUBSTITUTE_FOR[id] ?? "-"]);
  assert.equal(rows.length, 63);
  for (const id of RESTORED) assert.ok(FROZEN.some(([x]) => x === id) && !NO_OUTSIDE_FORM.some(([x]) => x === id), `${id} is executable under its own id`);
  for (const [orig, sub] of Object.entries(SUBSTITUTE_FOR)) { assert.ok(where(sub), `${orig}'s substitute ${sub} exists`); assert.ok(NO_OUTSIDE_FORM.some(([x]) => x === orig), `${orig} is superseded`); }
  for (const row of rows) t.diagnostic(`${row[0]} | ${row[1]} | was ${row[2]} | ${row[3]} | substitute: ${row[4]}`);
  { const g10 = where("gate 10 substitute"); const v = run(alter(g10[3])); assert.equal(verdictOf(v, "NN11"), "fail"); assert.match(whyOf(v, "NN11"), /^11\.0 s, limit 10\.5 s/); }
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
