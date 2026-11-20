// Each of the twelve lines is shown to pass a run that meets it and to fail, or to say "not measured", for a run that breaks it or leaves
// no evidence of it, on small hand-made evidence of the same shape a real run leaves outside the gate (the operating system's process record,
// the broker's signed jobs, git, the history copied before the run, the cache store, the launcher). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import os from "node:os";
import path from "node:path";
import { CHECK_IDS, CONTRACT_FILE, judgeRun, linkProblems, parseAcceptanceLines, parseCommandLine, processesFromWindowsEvents, readAcceptanceLines } from "./acceptance-lines.mjs";
import { exportWindowsProcesses, historyFileFor, takeLaunchLock } from "./acceptance-launch.mjs";

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
// Nothing here is written by the gate (Adrian, 2026-11-21: the test takes nothing from the gate's own reporting). It is what the operating
// system recorded (programs, parents, times, exits), what the broker signed (one clippy job for crate b), what git says the selected commit is,
// the row history the launcher copied before the gate started, the cache entries written during the run, and the launcher's own record.
// Class A: rustfmt:2021 last read src/a.rs and src/b.rs; src/a.rs changed, so rustfmt is owed src/a.rs; deny is unchanged. Class B: clippy of
// crate a is unchanged; clippy of crate b changed and was built by one broker job.
const C = "a".repeat(40); const T = "b".repeat(40); const SNAP = "D:\\snap\\k1"; const GATE = 100; const LAUNCHER = 50;
const T0 = Date.UTC(2026, 10, 20, 6, 0, 0);
const at = (s) => new Date(T0 + s * 1000).toISOString();
const ms = (s) => T0 + s * 1000;
let nextPid = 200;
const proc = (s, exe, args, { end = s + 0.05, exit = 0, ppid = GATE, pid = (nextPid += 1), cwd = null } = {}) => ({ pid, ppid, exe, args, cwd, start: ms(s), end: end === null ? null : ms(end), exit });
const GIT = "C:\\Program Files\\Git\\cmd\\git.exe"; const RUSTFMT = "C:\\rust\\bin\\rustfmt.exe"; const RUSTRIGHT = "D:\\managed\\agent-bin\\rustright.exe"; const DENY = "C:\\cargo\\bin\\cargo-deny.exe";
const job = (id, argv, { cwd = `${SNAP}\\ws`, start = 6.1, end = 8, waits = { admission: 50, queue: 0, bind: 50 }, gate = { commit: C, tree: T, snapshotUnchanged: true }, extra = {} } = {}) => ({
  file: `R-${id}.json`, verified: { ok: true, problems: [] },
  doc: { provenance: { payload: { jobId: id, request: { requestedArgv: argv, workspace: cwd, adapter: null }, startedAt: start === null ? undefined : at(start), terminalAt: end === null ? undefined : at(end), timingsMs: waits, gate, ...extra } } },
});
const hist = (files, key, { failed = false } = {}) => ({ last: { key, inputs: JSON.stringify({ files }) }, passes: failed ? [] : [key], failures: failed ? [key] : [] });
function records() {
  nextPid = 200;
  return {
    launch: { schema: "rightkit.acceptance-launch/2", runId: "f".repeat(64), commit: C, kind: "compile", host: "fixture-host", gatePid: GATE, launcherPid: LAUNCHER, lanes: 4, lanesSource: "this host's cores, handed to the gate as RK_GATE_JOBS",
      limitMs: 60_000, limitsSha256: "c".repeat(64), cacheDir: "D:\\repo\\.git\\rightkit-gate-cache", historyFile: null, historyCopiedAt: at(-1), startedAt: at(0), startMono: "0", endedAt: at(10), endMono: String(10_000_000_000n), exit: 0, signal: null, previous: null },
    witness: { kind: "windows-security", from: at(-2), to: at(12), problems: [] },
    procs: [
      { pid: GATE, ppid: LAUNCHER, exe: "C:\\node\\node.exe", args: ["bin\\rightkit-gate.mjs", "--to", "compile", "--commit", C], cwd: null, start: ms(0), end: ms(10), exit: 0 },
      proc(0, GIT, ["rev-parse", "--verify", `${C}^{commit}`]),
      proc(1.6, RUSTFMT, ["--check", "--edition", "2021", "src/a.rs"], { end: 1.7 }),
      proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "b", "--locked"], { end: 8 }),
    ],
    receipts: [job("j1", ["clippy", "--profile", "release", "-p", "b", "--locked"])],
    history: { copiedAt: at(-1), A: { "rustfmt:2021": hist([["src/a.rs", "a0"], ["src/b.rs", "b1"]], "k0"), deny: hist([["Cargo.lock", "l1"]], "k2") },
      B: { "clippy:ws:a": hist([["ws/a/src/lib.rs", "x1"]], "ka"), "clippy:ws:b": hist([["ws/b/src/lib.rs", "y0"]], "kb0") } },
    cacheBefore: ["k0", "k2", "ka", "kb0"],
    cacheWrites: [{ key: "k1", at: at(1.7), row: { id: "rustfmt:2021", class: "A", status: "pass" } }, { key: "kb", at: at(8), row: { id: "clippy:ws:b", class: "B", status: "pass" } }],
    blobs: { "src/a.rs": "a1", "src/b.rs": "b1", "Cargo.lock": "l1", "ws/a/src/lib.rs": "x1", "ws/b/src/lib.rs": "y1" },
    tree: { "ci/shards.json": { solo: [] }, "ws/Cargo.toml": '[workspace]\nmembers = ["a", "b"]\n' },
    gateOutput: "", live: { exists: () => false, alive: () => false }, successor: null, verifierPinned: true, digests: {},
  };
}
const run = (r) => judgeRun({
  lines: LINES, launch: r.launch, processes: r.procs, witness: r.witness, receipts: r.receipts, history: r.history, cacheBefore: r.cacheBefore, cacheWrites: r.cacheWrites,
  git: { tree: T, blobOf: (p) => r.blobs[p] ?? null, readTree: (p) => { if (!(p in r.tree)) throw Object.assign(new Error(`ENOENT ${p}`), { code: "ENOENT" }); return structuredClone(r.tree[p]); } },
  gateOutput: r.gateOutput, live: r.live, successor: r.successor, verifierPinned: r.verifierPinned, digestOf: (f) => r.digests[f] ?? null,
});
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const byExe = (r, exe) => r.procs.find((p) => p.exe === exe);

test("a run whose evidence meets every line passes all twelve", () => {
  const v = run(records());
  for (const id of twelve) assert.equal(verdictOf(v, id), "pass", `${id}: ${whyOf(v, id)}`);
  assert.equal(v.pass, true);
});

// ---- the frozen step-2 batch, restated (D:\scratch\loop-review\twelve-line-step2\final-batch-cases.md, sha256 70ff356d) --------------------------
// Adrian, 2026-11-21: "Restate them". Every case keeps its author's id and expected verdict; each is now the same fault as it shows in evidence
// the gate does not write (the process record, the broker's signed jobs, git, the history copied before the run, the cache store, the launcher).
const twoRows = (r, second, { lanes = 4, a = [1.0, 1.1], b = [2.0, 2.1] } = {}) => { r.launch.lanes = lanes; const f = byExe(r, RUSTFMT); f.start = ms(a[0]); f.end = ms(a[1]); r.procs.push(proc(b[0], second, ["check"], { end: b[1] })); };
const split = (r, ...argvs) => {
  r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.receipts = []; r.cacheWrites = r.cacheWrites.filter((w) => w.row.class !== "B");
  const crates = new Set();
  argvs.forEach((a, i) => { const argv = ["clippy", "--locked", ...a.split(" ")]; r.procs.push(proc(6, RUSTRIGHT, ["cargo", ...argv], { end: 8 })); r.receipts.push(job(`j${i + 1}`, argv)); for (const m of a.matchAll(/-p (\w+)/g)) crates.add(m[1]); });
  r.history.B = Object.fromEntries([...crates].map((c) => [`clippy:ws:${c}`, hist([[`ws/${c}/src/lib.rs`, "old"]], `k${c}0`)]));
  for (const c of crates) r.blobs[`ws/${c}/src/lib.rs`] = "new";
  r.tree["ws/Cargo.toml"] = `[workspace]\nmembers = [${[...crates].map((c) => `"${c}"`).join(", ")}]\n`;
};
const policy = (r, solo) => { if (solo === null) delete r.tree["ci/shards.json"]; else r.tree["ci/shards.json"] = { solo }; };
const unchangedB = (r) => { r.blobs["ws/b/src/lib.rs"] = "y0"; r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.receipts = []; r.cacheWrites = r.cacheWrites.filter((w) => w.row.id !== "clippy:ws:b"); };
const testJob = (r, exe, extra) => { r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.procs.push(proc(6, RUSTRIGHT, ["cargo", "test", "-p", "b", "--no-run"], { end: 8 }), proc(8.5, exe, [], { end: 9 })); r.receipts = [job("t1", ["test", "-p", "b", "--no-run"], { extra })]; r.history.B["test:ws:b"] = hist([["ws/b/src/lib.rs", "y0"]], "kt0"); };
const EXE = `${SNAP}\\ws\\target\\debug\\deps\\b-1.exe`;
const FROZEN = [
  // NN01: only the selected commit is built.
  ["GCX-01-1", "NN01", "not measured", (r) => { r.receipts = []; }], // a broker compile with no signed job
  ["GCX-01-2", "NN01", "not measured", (r) => { r.witness.to = at(5); }], // the process record does not cover the run's work
  ["GCX-01-3", "NN01", "fail", (r) => { r.receipts[0].doc.provenance.payload.gate.commit = "e".repeat(40); }], // signed for another commit
  ["GCX-01-4", "NN01", "fail", (r) => { r.receipts[0].doc.provenance.payload.gate.tree = "e".repeat(40); }], // signed for another tree
  ["gate 13", "NN01", "not measured", (r) => { delete r.receipts[0].doc.provenance.payload.gate.snapshotUnchanged; }], // nothing says the snapshot held after the compile
  // NN02: Class A finishes before anything goes to the broker; if it fails, nothing goes.
  ["GCX-02-1", "NN02", "fail", (r) => { const cmd = proc(1, "C:\\Windows\\System32\\cmd.exe", ["/d", "/s", "/c", '""C:\\Program Files\\RustRight\\rustright.cmd" cargo metadata --format-version 1"'], { end: 1.3 }); r.procs.push(cmd, proc(1.05, "C:\\Program Files\\RustRight\\rustright.exe", ["cargo", "metadata", "--format-version", "1"], { ppid: cmd.pid, end: 1.25 })); }],
  ["GCX-02-2", "NN02", "not measured", (r) => { unchangedB(r); r.cacheWrites.push({ key: "kf", at: at(1.7), row: { id: "deny", class: "A", status: "fail" } }); r.procs[0].pid = 999; }], // the record shows no gate process of this launcher
  ["GCX-02-3", "NN02", "not measured", (r) => { byExe(r, RUSTFMT).end = ms(6.0); }], // the job's signed submission ties with Class A's end
  ["GCX-02-4", "NN02", "fail", (r) => { byExe(r, RUSTFMT).exit = 1; }], // Class A failed, and the broker still got a job
  ["gate 9", "NN02", "pass", (r) => { r.procs.push(proc(9, "C:\\node\\node.exe", ["other-unit.mjs"], { exit: 1 })); }], // a program outside Class A failing is not Class A
  // NN03: Class A starts ready, independent work whenever an eligible lane is free.
  ["GCX-03-1", "NN03", "not measured", (r) => { r.launch.lanes = 1.5; }],
  ["GCX-03-2", "NN03", "not measured", (r) => { const f = byExe(r, RUSTFMT); f.end = f.start - 1; }],
  ["GCX-03-3", "NN03", "fail", (r) => { twoRows(r, "C:\\tools\\shellcheck.exe", { lanes: 2 }); }], // the second program waits a second beside a free lane
  ["GCX-03-4", "NN03", "pass", (r) => { r.launch.lanes = 20; }],
  ["gate 4", "NN03", "fail", (r) => { twoRows(r, "C:\\tools\\shellcheck.exe", { lanes: 4, a: [1.0, 3.0], b: [3.0, 5.0] }); }], // two ready programs run one after the other on four lanes
  // NN04: the expectation is fixed before the first lookup (the test's own, from the history copied before the gate started).
  ["GCX-04-1", "NN04", "fail", (r) => { r.history.copiedAt = at(0.5); }], // copied after the run's first program
  ["GCX-04-2", "NN04", "not measured", (r) => { r.history.copiedAt = at(0); }], // copied at the same moment, no order shown
  ["GCX-04-3", "NN04", "fail", (r) => { r.launch.historyFile = "D:\\runs\\rightkit-class-a-history\\0000.json"; }], // another store's history
  ["gate 14", "NN04", "fail", (r) => { r.history.copiedAt = at(9); }], // copied after a broker job's work
  // NN05: a disagreeing lookup stops the gate before work and is named; otherwise the work that ran is the work owed.
  ["GCX-05-1", "NN05", "not measured", (r) => { r.history = null; }], // no expectation from before the run
  ["GCX-05-2", "NN05", "fail", (r) => { r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.receipts = []; r.cacheWrites = r.cacheWrites.filter((w) => w.row.id !== "clippy:ws:b"); }], // owed and not done
  ["GCX-05-3", "NN05", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.requestedArgv.push("-p", "z"); }], // a crate no owed row needs
  ["GCX-05-4", "NN05", "fail", (r) => { r.cacheWrites.push({ key: "k9", at: at(1.8), row: { id: "deny", class: "A", status: "bogus" } }); }], // a result stored for an unchanged row
  ["GCX-05-5", "NN05", "fail", (r) => { r.procs.push(proc(1.8, DENY, ["check"])); }], // an unchanged row's tool started
  ["gate 8", "NN05", "fail", (r) => { unchangedB(r); r.cacheBefore = r.cacheBefore.filter((k) => k !== "kb0"); r.launch.exit = 1; r.gateOutput = 'rightkit-gate: RK_CACHE_DIVERGENCE {"id":"clippy:ws:bb"}'; }], // the stop names another row
  // NN06: an unchanged row is never run again, including one that failed before.
  ["GCX-06-1", "NN06", "fail", (r) => { r.blobs["src/a.rs"] = "a0"; }], // rustfmt is unchanged, and its result was stored again
  ["GCX-06-2", "NN06", "not measured", (r) => { delete r.history.B["clippy:ws:a"].last.key; }],
  ["GCX-06-3", "NN06", "not measured", (r) => { r.history.B["clippy:ws:a"] = hist([["ws/a/src/lib.rs", "x1"]], "ka", { failed: true }); r.launch.exit = null; r.launch.signal = "SIGKILL"; }],
  ["gate 1", "NN06", "fail", (r) => { r.procs.push(proc(1.7, DENY, ["check"], { end: 1.9 })); }],
  ["gate 15", "NN06", "not measured", (r) => { delete r.history.A.deny.last.key; }],
  // NN07: a changed row always runs again, and does only the work the change needs.
  ["GCX-07-1", "NN07", "fail", (r) => { r.blobs["src/b.rs"] = "b2"; }], // a.rs and b.rs changed; only a.rs reached rustfmt
  ["GCX-07-2", "NN07", "fail", (r) => { byExe(r, RUSTFMT).args = ["--check", "--edition", "2021"]; }],
  ["GCX-07-3", "NN07", "pass", (r) => { r.blobs["src/b.rs"] = "b2"; byExe(r, RUSTFMT).args.push("src/b.rs"); }],
  ["GCX-07-4", "NN07", "not measured", (r) => { r.receipts = []; r.procs = r.procs.filter((p) => p.exe !== RUSTRIGHT); r.witness.problems = ["events were dropped"]; }],
  ["GCX-07-5", "NN07", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.requestedArgv = ["clippy", "--profile", "release", "-p", "ws", "--locked"]; }],
  ["GCX-07-6", "NN07", "fail", (r) => { r.receipts[0].doc.provenance.payload.request.workspace = `${SNAP}\\other`; r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; }],
  ["gate 12", "NN07", "fail", (r) => { const id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.history.B[id] = r.history.B["clippy:ws:b"]; delete r.history.B["clippy:ws:b"]; r.cacheWrites[1].row.id = id; r.receipts[0].doc.provenance.payload.request.requestedArgv = ["clippy", "--profile", "release", "--target", "x86_64-pc-windows-msvc", "-p", "b", "--locked"]; }],
  // NN08: nothing is started twice in one run.
  ["GCX-08-1", "NN08", "pass", (r) => { r.procs.push(proc(7, "/opt/Tool", ["probe"], { cwd: "/src" }), proc(8, "/opt/tool", ["probe"], { cwd: "/src" })); }],
  ["GCX-08-2", "NN08", "pass", (r) => { r.procs.push(proc(7, "/usr/bin/git", ["status"], { cwd: "/src/A" }), proc(8, "/usr/bin/git", ["status"], { cwd: "/src/a" })); }],
  ["GCX-08-3", "NN08", "fail", (r) => { const nested = proc(7, "C:\\node\\node.exe", ["nested-gate.mjs"], { end: 9 }); r.procs.push(nested, proc(8, RUSTFMT, ["--check", "--edition", "2021", "src/a.rs"], { ppid: nested.pid })); }], // two writers, one call
  ["gate 5", "NN08", "fail", (r) => { const g = r.procs[1]; const cmd = proc(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", `"${g.exe}" ${g.args.join(" ")}`], { end: 0.6 }); r.procs.push(cmd, proc(0.52, g.exe, [...g.args], { ppid: cmd.pid, end: 0.58 })); }],
  ["gate 6", "NN08", "fail", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, [...g.args])); }], // the same call (the record keeps no folder)
  ["gate 7", "NN08", "pass", (r) => { const g = r.procs[1]; r.procs.push(proc(0.5, g.exe, ["-c", "core.fsmonitor=false", ...g.args])); }],
  // NN09: compatible compiles combined per workspace and stage; every split has a reason the check can verify; independent groups run together.
  ["GCX-09-1", "NN09", "fail", (r) => { split(r, "-p b -p c", "-p c"); policy(r, ["c"]); }],
  ["GCX-09-2", "NN09", "fail", (r) => { split(r, "-p c --target x", "-p c --target y"); policy(r, ["c"]); }],
  ["GCX-09-3", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, []); }],
  ["GCX-09-4", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, ["c"]); }],
  ["GCX-09-5", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null); }],
  ["GCX-09-6", "NN09", "not measured", (r) => { r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["x"]\n'; r.procs.push(proc(7.9, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "x", "--locked"], { end: 9 })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "x", "--locked"], { cwd: `${SNAP}\\other`, start: null })); }], // no signed time
  ["GCX-09-7", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, []); r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json"] = JSON.stringify({ bundle: { externalBin: ["binaries/c-sidecar"] } }); }],
  ["GCX-09-8", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null); r.gateOutput = "rightkit-gate: c is sidecar-bound"; }],
  ["GCX-09-9", "NN09", "fail", (r) => { split(r, "-p b", "-p b"); policy(r, ["b"]); }],
  ["gate 11", "NN09", "fail", (r) => { r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "sub"]\n'; r.tree["ws/sub/Cargo.toml"] = '[package]\nname = "c"\n'; r.procs.push(proc(6, RUSTRIGHT, ["cargo", "clippy", "--profile", "release", "-p", "c", "--locked"], { end: 8 })); r.receipts.push(job("j2", ["clippy", "--profile", "release", "-p", "c", "--locked"], { cwd: `${SNAP}\\ws\\sub` })); }],
  // NN10: tests are compiled by the broker and run by the gate.
  ["GCX-10-1", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.verifierPinned = false; r.receipts[0].verified = { ok: false, problems: ["no broker key is pinned"] }; }],
  ["GCX-10-2", "NN10", "not measured", (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE }] }); }], // a program named with no signed digest
  // NN11: the run finishes inside its time limit (the launcher's clock, the limit pinned at launch).
  ["GCX-11-1", "NN11", "not measured", (r) => { r.launch.endMono = null; r.launch.endedAt = null; r.launch.exit = null; }],
  ["GCX-11-2", "NN11", "fail", (r) => { r.launch.limitMs = 9999; }],
  ["GCX-11-3", "NN11", "pass", (r) => { r.launch.limitMs = 10000; }],
  ["gate 10", "NN11", "fail", (r) => { r.launch.limitMs = 15_000; r.procs.push(proc(9, GIT, ["status"], { end: 40 })); }], // a program of the run outlives the gate by 30 s
  // NN12: the run ends clean, even if killed: the next ordinary run starts safely by itself.
  ["GCX-12-1", "NN12", "not measured", (r) => { r.launch.exit = 137; }],
  ["GCX-12-2", "NN12", "fail", (r) => { r.launch.exit = 137; r.live = { exists: () => false, alive: (pid) => pid === byExe(r, RUSTFMT).pid }; }],
  ["gate 2", "NN12", "not measured", (r) => { r.witness = null; }], // no record of the run's programs at all
  ["gate 3", "NN12", "not measured", (r) => { r.witness.to = at(5); }], // the record leaves part of the run out
];
test("the frozen step-2 batch, restated: all 63 cases, each with its author's id and expected verdict", async (t) => {
  assert.equal(FROZEN.length, 63);
  assert.equal(new Set(FROZEN.map(([id]) => id)).size, 63, "every id once");
  for (const [id, line, verdict, fn] of FROZEN) {
    await t.test(`${id} ${line}: expected ${verdict}`, (st) => { const v = run(alter(fn)); st.diagnostic(`${id} ${line}: got ${verdictOf(v, line)}`); assert.equal(verdictOf(v, line), verdict, `${id} ${line}: ${whyOf(v, line)}`); });
  }
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
test("the run is the gate the launcher started and everything under it; another launcher's gate is not this run", () => {
  expect(alter((r) => { r.procs[0].ppid = 77; }), "NN08", "not measured", /no start of the gate process/);
  expect(alter((r) => { r.procs.push({ ...proc(5, GIT, ["status"]), ppid: 4242 }, proc(5.5, GIT, ["status"])); }), "NN08", "pass");
  expect(alter((r) => { byExe(r, RUSTFMT).end = null; }), "NN08", "not measured", /no recorded exit/);
});
test("the launcher's lock: a live holder refuses, a dead one is taken over and named, and the history file is the gate's own for its store", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-launch-"));
  const lock = path.join(dir, "l");
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "r", pid: process.pid })}\n`);
  assert.match(takeLaunchLock(lock, { runId: "s", pid: 1 }).refused, /holds/);
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "k", pid: 999999, out: "o" })}\n`);
  assert.deepEqual(takeLaunchLock(lock, { runId: "s", pid: process.pid }).previous, { runId: "k", out: "o", pid: 999999, reclaimed: true });
  assert.match(historyFileFor("D:\\runs\\h", "D:\\Repo\\.git\\rightkit-gate-cache", "win32"), /h[\\/][0-9a-f]{64}\.json$/);
  assert.equal(historyFileFor("D:\\runs\\h", "D:\\Repo\\.git\\rightkit-gate-cache", "win32"), historyFileFor("D:\\runs\\h", "d:\\repo\\.git\\rightkit-gate-cache", "win32"));
});
test("the Security log export: its events become the process record, and a log that does not reach back is said", () => {
  const raw = { oldest: at(-100), events: [{ id: 4688, time: at(0), data: { NewProcessId: "0x64", ProcessId: "0x32", NewProcessName: "C:\\node.exe", CommandLine: "node gate.mjs" } }, { id: 4689, time: at(1), data: { ProcessId: "0x64", ProcessName: "C:\\node.exe", Status: "0x0" } }] };
  const x = exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify(raw) });
  assert.deepEqual(x.problems, []);
  assert.deepEqual(x.events.map((e) => [e.id, e.pid, e.ppid ?? null]), [[4688, 100, 50], [4689, 100, null]]);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => JSON.stringify({ ...raw, oldest: at(1) }) }).problems[0], /oldest event/);
  assert.match(exportWindowsProcesses(at(-1), at(2), { run: () => { throw Object.assign(new Error("x"), { stderr: "Access is denied." }); } }).problems[0], /Access is denied/);
});
test("NN10: a verified compile-only test build whose program the gate ran, as the broker built it, passes", () => {
  const ok = (r) => { testJob(r, EXE, { testExecutables: [{ path: EXE, sha256: "h1" }] }); r.digests[EXE] = "h1"; };
  expect(alter(ok), "NN10", "pass");
  expect(alter((r) => { ok(r); r.digests[EXE] = "h2"; }), "NN10", "fail", /not the program the broker built/);
  expect(alter((r) => { ok(r); r.procs = r.procs.filter((p) => p.exe !== EXE); }), "NN10", "fail", /never ran it/);
  expect(alter((r) => { ok(r); delete r.receipts[0].doc.provenance.payload.testExecutables; }), "NN10", "fail", /compile only/);
  expect(alter((r) => { r.procs.push(proc(8, "C:\\cargo\\bin\\cargo.exe", ["test", "-p", "b"])); }), "NN10", "fail", /outside the broker/);
});
test("NN12: a killed run passes only on the next ordinary run that took its lock over and ended 0", () => {
  const killed = (r) => { r.launch.exit = 137; };
  expect(alter((r) => { killed(r); r.successor = { launch: { previous: { runId: r.launch.runId, reclaimed: true }, exit: 0 } }; }), "NN12", "pass", /next ordinary run/);
  expect(alter((r) => { killed(r); r.successor = { launch: { previous: { runId: r.launch.runId, reclaimed: true }, exit: 1 } }; }), "NN12", "not measured");
  expect(alter((r) => { r.live = { exists: (f) => f.endsWith(".lock"), alive: () => false }; }), "NN12", "fail", /lock/);
});
