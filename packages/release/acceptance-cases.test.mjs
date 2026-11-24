// The case-based decision of the twelve lines (acceptance-cases.mjs; GC-A27; rightkit#1514; Adrian, 2026-11-24: "adopt the 12-line evidence table and its 4 named
// limits"). For each line: a clean set of runs passes, and a run broken in the way the evidence table names fails. A judge that passes a broken gate fails here.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { FAIL, LINE_IDS, PASS, decideAll, decideCase, decideRealBuild, groupOf, isClassAProgram, linesOf } from "./acceptance-cases.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const CASES = JSON.parse(fs.readFileSync(path.join(here, "acceptance-cases.json"), "utf8"));
CASES.limitsMs = Object.fromEntries(Object.keys(CASES.limitsMs).map((k) => [k, 600_000])); // the tests pin a limit; the real file's are measured
const COMMIT = "a".repeat(40); const TREE = "b".repeat(40); const SNAP = "D:\\data\\snapshots\\k1"; const T0 = Date.parse("2026-11-24T10:00:00Z");
const VERDICT = "class-a-aaaaaaaaaaaa-full-root.json";
const bc = CASES.brokerCommands;

let seq = 0;
const job = (ws, verb, target, features, at, { crates = [], dur = 5000, extra = [] } = {}) => {
  seq += 1; const feat = features ? features.split(" ") : [];
  const args = [verb, ...crates.flatMap((c) => ["-p", c]), ...feat, "--target", target, "--locked", ...extra];
  return { jobId: `job-${seq}`, verified: true, problems: [], argv: args.filter((a) => a !== "--no-run"), effectiveArgv: args, args, sub: verb, cwd: ws === "." ? SNAP : `${SNAP}\\${ws.replaceAll("/", "\\")}`, gate: { commit: COMMIT, tree: TREE, snapshot: `\\\\?\\${SNAP}`, snapshotUnchanged: true, classA: `\\\\?\\D:\\data\\class-a-verdicts\\${VERDICT}` }, submitted: at, started: at + 50, ended: at + dur, testExecutables: null, status: "SUCCEEDED" };
};
let pids = 100; const ROOT = 7;
const prog = (program, args, at, end, ppid = ROOT) => ({ program, exe: program, args, at, end, exit: 0, argsKnown: true, ambiguous: false, pid: (pids += 1), ppid });
const TEST_EXE = "D:\\data\\ws\\t\\release\\deps\\rk_leaf-0123456789abcdef.exe";
/** A clean run from nothing: Class A programs filling the lanes, a verdict, then every required broker command submitted at once. */
function fullRun() {
  const starts = []; const lanes = 4;
  ["rustfmt", "shellcheck", "actionlint", "oxlint", "tsc", "cargo-deny", "cargo-audit"].forEach((p, i) => starts.push(prog(`C:\\tools\\${p}.exe`, ["--check", `f${i}`], T0 + 1000 + Math.floor(i / lanes) * 900 + i * 10, T0 + 1900 + Math.floor(i / lanes) * 900 + i * 10)));
  for (let i = 0; i < 6; i += 1) starts.push(prog("C:\\tools\\cargo.exe", ["metadata", "--no-deps", "--format-version", "1", "--locked"], T0 + 200 + i, T0 + 400 + i));
  const jobs = []; const sub = T0 + 10_000;
  for (const g of bc.required) for (const v of ["clippy", "test", "doc"]) {
    if (typeof g[v] !== "string" || g[v].startsWith("none")) continue;
    for (const t of g[v] === "each target" ? bc.targets : [bc.hostTarget]) jobs.push(job(g.workspace, v, t, g.features, sub + jobs.length, { extra: v === "test" ? ["--no-run"] : [] }));
  }
  jobs.filter((j) => j.sub === "test").forEach((j) => { j.testExecutables = []; });
  const t = jobs.find((j) => j.sub === "test"); t.testExecutables = [{ path: TEST_EXE, sha256: "e".repeat(64) }];
  starts.push(prog(TEST_EXE, [], t.ended + 500, t.ended + 900));
  return { commit: COMMIT, tree: TREE, exit: 0, signal: null, killed: false, durationMs: 60_000, lanes, log: "", jobs, starts, processGaps: [], cacheWritten: [{ name: "x.json", row: "rustfmt:2021" }], verdicts: { [VERDICT]: sub - 2000 }, decisions: [], locksAfter: [], locksKnown: true, rootAlive: false, rootPid: ROOT, snapshot: SNAP, imageNow: { [TEST_EXE.replaceAll("\\", "/").toLowerCase()]: "e".repeat(64) } };
}
const quiet = (over = {}) => ({ commit: COMMIT, tree: TREE, exit: 0, signal: null, killed: false, durationMs: 9000, lanes: 4, log: "", jobs: [], starts: [], processGaps: [], cacheWritten: [], verdicts: {}, decisions: [], locksAfter: [], locksKnown: true, rootAlive: false, rootPid: ROOT, snapshot: SNAP, imageNow: {}, ...over });
const def = (id) => CASES.cases.find((c) => c.id === id);
const indexName = (id) => `${crypto.createHash("sha256").update(`rk-row-index/1\0${id}`).digest("hex")}.json`;
/** Clean runs for every case. */
function good() {
  const e3 = def(3).expect; const sub = T0 + 5000;
  const leaf = [...bc.targets.map((t, i) => job("rk", "clippy", t, "", sub + i, { crates: ["rk-leaf"] })), { ...job("rk", "test", bc.hostTarget, "", sub + 3, { crates: ["rk-leaf"], extra: ["--no-run"] }), testExecutables: [] }, job("rk", "doc", bc.hostTarget, "", sub + 4, { crates: ["rk-leaf"] })];
  const rows3 = [...e3.classARows, ...e3.compileRows];
  return {
    1: [fullRun()],
    2: [quiet()],
    4: [quiet({ exit: 1, log: "rightkit-gate: BLOCKED: license was expected to be reused and its stored result is missing; nothing ran\n", decisions: [{ commit: COMMIT, forecast: [{ id: "license", expected: "hit" }, { id: "rustfmt:2021", expected: "hit" }] }] })],
    3: [quiet({ jobs: leaf, starts: [prog("C:\\tools\\rustfmt.exe", ["--check", "--edition", "2021", "lib.rs"], T0 + 1000, T0 + 1500)], verdicts: { [VERDICT]: sub - 1000 }, cacheWritten: [...rows3.map((id, i) => ({ name: `r${i}.json`, row: id })), { name: indexName("secret-scan"), row: null }] })],
    5: [quiet({ exit: 1, log: "json-parse failed: ci/acceptance-broken.json\n", cacheWritten: [{ name: "f.json", row: "json-parse" }] }), quiet({ exit: 1, log: "json-parse failed (stored)\n" })],
    6: [quiet({ exit: 1, log: "test fails_on_purpose ... FAILED\n" })],
    7: [quiet({ exit: null, signal: "SIGKILL", killed: true, locksAfter: [`${SNAP}.lock`] }), quiet()],
  };
}
const judge = (runsByCase) => { const results = {}; for (const c of CASES.cases) results[c.id] = decideCase({ ...c, runs: runsByCase[c.id] }, CASES); return decideAll({ cases: CASES, results, realBuild: decideRealBuild(runsByCase[1][0], CASES) }); };
const lineOf = (out, id) => out.lines.find((l) => l.id === id);
/** Breaks one case's run and returns the verdict of one line. */
const broken = (id, caseId, mutate, runIndex = 0) => { const g = good(); mutate(g[caseId][runIndex], g[caseId]); return lineOf(judge(g), id); };
const failsFor = (id, caseId, mutate, re, runIndex = 0) => { const l = broken(id, caseId, mutate, runIndex); assert.equal(l.verdict, FAIL, `${id} must fail: ${l.why}`); assert.match(l.why, re); };

test("the clean runs pass twelve of twelve, and the real build passes its six lines", () => {
  const out = judge(good());
  for (const l of out.lines) assert.equal(l.verdict, PASS, `${l.id}: ${l.why}`);
  for (const l of out.realBuild) assert.equal(l.verdict, PASS, `real build ${l.id}: ${l.why}`);
  assert.equal(out.passed, 12); assert.equal(out.pass, true);
});
test("every line is decided by at least one case, and no verdict is ever 'not measured'", () => {
  for (const id of LINE_IDS) assert.ok(CASES.cases.some((c) => linesOf(c).includes(id)), `${id} has no case`);
  const out = judge(good()); for (const l of out.lines) for (const c of l.cases) assert.ok([PASS, FAIL].includes(c.verdict));
});
test("a case that was not run fails every line it decides; a missing real build fails the whole", () => {
  const results = {}; const g = good(); for (const c of CASES.cases) if (c.id !== 4) results[c.id] = decideCase({ ...c, runs: g[c.id] }, CASES);
  const out = decideAll({ cases: CASES, results, realBuild: null });
  assert.equal(lineOf(out, "NN04").verdict, FAIL); assert.match(lineOf(out, "NN04").why, /not run/); assert.equal(out.pass, false);
  assert.ok(out.realBuild.every((l) => l.verdict === FAIL));
});

test("NN01: a gate that reads the checkout fails case 1 or passes case 5; a receipt for another commit, tree or a changed snapshot fails", () => {
  failsFor("NN01", 1, (r) => { r.exit = 1; }, /error planted in the checkout/);
  failsFor("NN01", 5, (r) => { r.exit = 0; }, /did not end with a failing exit/);
  failsFor("NN01", 1, (r) => { r.jobs[3].gate.commit = "c".repeat(40); }, /not the selected/);
  failsFor("NN01", 3, (r) => { r.jobs[0].gate.tree = "d".repeat(40); }, /tree/);
  failsFor("NN01", 1, (r) => { r.jobs[0].gate.snapshotUnchanged = false; }, /snapshot was unchanged/);
  failsFor("NN01", 1, (r) => { r.jobs[0].verified = false; }, /does not verify/);
});
test("NN02: a receipt in the failing case, a verdict newer than the first submission, or a Class A program still running at it fails", () => {
  failsFor("NN02", 5, (r) => { r.jobs.push(job("rk", "clippy", bc.hostTarget, "", T0 + 100)); }, /left 1 broker receipt/);
  failsFor("NN02", 5, (r) => { r.exit = 0; }, /failing exit/, 1);
  failsFor("NN02", 1, (r) => { r.verdicts[VERDICT] = T0 + 10_500; }, /before the Class A verdict file it names was written/);
  // one job sent early under a verdict file an older run left, written by this run only later: that job fails, whatever the others did
  failsFor("NN02", 1, (r) => { r.jobs[0].gate.classA = "D:\\data\\class-a-verdicts\\class-a-aaaaaaaaaaaa-full-scope.json"; r.verdicts["class-a-aaaaaaaaaaaa-full-scope.json"] = r.jobs[0].submitted + 40_000; }, /job-\d+ \(cargo clippy\) was submitted/);
  failsFor("NN02", 1, (r) => { delete r.verdicts[VERDICT]; }, /not in the launcher's listing/);
  failsFor("NN02", 1, (r) => { r.starts[0].end = T0 + 11_000; }, /had not ended at the first submission/);
});
test("NN03: Class A programs run one after another, or a row left waiting beside a free lane, fails", () => {
  failsFor("NN03", 1, (r) => { const a = r.starts.filter(isClassAProgram); a.forEach((p, i) => { p.at = T0 + 1000 + i * 3000; p.end = p.at + 2900; }); r.verdicts[VERDICT] = T0 + 60_000; r.jobs.forEach((j) => { j.submitted += 60_000; j.started += 60_000; j.ended += 60_000; }); }, /while a lane was free|at most 1/);
  failsFor("NN03", 1, (r) => { r.lanes = null; }, /no lane count/);
  failsFor("NN03", 1, (r) => { r.starts = r.starts.filter((s) => !isClassAProgram(s)); }, /0 Class A program/);
});
test("NN04: a forecast that expects the deleted row to run came from the cache; no decision record, or a run that did not stop, fails", () => {
  failsFor("NN04", 4, (r) => { r.decisions[0].forecast[0].expected = "run"; }, /came from what is in the cache/);
  failsFor("NN04", 4, (r) => { r.decisions = []; }, /no Class A decision record/);
  failsFor("NN04", 4, (r) => { r.exit = 0; }, /failing exit/);
});
test("NN05: a gate that carries on after the surprise leaves a receipt, a program or an unnamed row; work beyond the expected rows fails case 3", () => {
  failsFor("NN05", 4, (r) => { r.jobs.push(job("rk", "clippy", bc.hostTarget, "", T0 + 100)); }, /broker receipt/);
  failsFor("NN05", 4, (r) => { r.starts.push(prog("C:\\tools\\rustfmt.exe", ["--check", "a.rs"], T0 + 10, T0 + 20)); }, /Class A program/);
  failsFor("NN05", 4, (r) => { r.log = "BLOCKED: a row disagreed\n"; }, /does not name the row license/);
  failsFor("NN05", 4, (r) => { r.log = "license\n"; }, /no cause/);
  failsFor("NN05", 3, (r) => { r.cacheWritten.push({ name: "z.json", row: "clippy:rk:rk-core" }); }, /a row the edit does not touch/);
  failsFor("NN05", 3, (r) => { r.cacheWritten = r.cacheWritten.filter((f) => f.row !== "doc:rk:rk-leaf"); }, /no result was written for doc:rk:rk-leaf/);
});
test("NN06: a rerun that starts a program, sends a job or writes a result fails, for a passing row and for one that failed before", () => {
  failsFor("NN06", 2, (r) => { r.jobs.push(job("rk", "clippy", bc.hostTarget, "", T0 + 100)); }, /broker receipt/);
  failsFor("NN06", 2, (r) => { r.starts.push(prog("C:\\tools\\shellcheck.exe", ["a.sh"], T0 + 10, T0 + 20)); }, /Class A program/);
  failsFor("NN06", 2, (r) => { r.cacheWritten.push({ name: "k.json", row: "license" }); }, /wrote 1 scan store file/);
  failsFor("NN06", 5, (r) => { r.cacheWritten.push({ name: "k.json", row: "json-parse" }); }, /wrote 1 scan store file/, 1);
  failsFor("NN06", 5, (r) => { r.exit = 0; }, /failing exit/, 1);
  failsFor("NN06", 5, (r, runs) => { runs.pop(); }, /not run a second time/);
});
test("NN07: missing work leaves an expected command or program absent; extra work names a crate or a program the edit does not need", () => {
  failsFor("NN07", 3, (r) => { r.jobs.pop(); }, /0 doc command/);
  failsFor("NN07", 3, (r) => { r.jobs[0].args.splice(1, 0, "-p", "rk-core"); }, /names rk-core/);
  failsFor("NN07", 3, (r) => { r.jobs.push(job("tools", "clippy", bc.hostTarget, "", T0 + 6000, { crates: ["witness-tools"] })); }, /workspace tools/);
  failsFor("NN07", 3, (r) => { r.starts = []; }, /rustfmt did not run/);
  failsFor("NN07", 3, (r) => { r.starts.push(prog("C:\\tools\\shellcheck.exe", ["a.sh"], T0 + 10, T0 + 20)); }, /shellcheck ran although/);
});
test("NN08: a program started twice with the same arguments, a pinned repeat over its count, or a broker command sent twice fails", () => {
  failsFor("NN08", 2, (r) => { r.starts.push(prog("C:\\git\\git.exe", ["rev-parse", "HEAD"], T0 + 1, T0 + 2), prog("C:\\git\\git.exe", ["rev-parse", "HEAD"], T0 + 3, T0 + 4)); }, /git rev-parse HEAD started 2 times \(1 allowed\)/);
  failsFor("NN08", 1, (r) => { r.starts.push(prog("C:\\tools\\cargo.exe", ["metadata", "--no-deps", "--format-version", "1", "--locked"], T0 + 300, T0 + 310)); }, /started 7 times \(6 allowed\)/);
  failsFor("NN08", 1, (r) => { r.jobs.push({ ...r.jobs[0], jobId: "again" }); }, /was sent 2 times/);
  // what a tool starts for itself is not the gate's call: cargo's own rustc probes, twice alike, are not a repeat; the same two started by the gate are
  const twice = (ppid) => (r) => { const c = prog("C:\\tools\\cargo.exe", ["tree"], T0 + 1, T0 + 9); r.starts.push(c, prog("C:\\rust\\rustc.exe", ["-vV"], T0 + 2, T0 + 3, ppid ?? c.pid), prog("C:\\rust\\rustc.exe", ["-vV"], T0 + 4, T0 + 5, ppid ?? c.pid)); };
  assert.equal(broken("NN08", 2, twice(null)).verdict, PASS);
  failsFor("NN08", 2, twice(ROOT), /rustc -vV started 2 times/);
  // a shell wrapper of the gate's counts as the program it ran, once
  assert.equal(broken("NN08", 2, (r) => { const w = prog("C:\\Windows\\System32\\cmd.exe", ["/d", "/s", "/c", "pnpm --version"], T0 + 1, T0 + 9); r.starts.push(w, prog("C:\\n\\pnpm.exe", ["--version"], T0 + 2, T0 + 3, w.pid)); }).verdict, PASS);
});
test("NN09: a split of compatible compiles, a missing or unlisted command, two targets in one command, or waiting for one job before sending the next fails", () => {
  failsFor("NN09", 1, (r) => { r.jobs.push(job("rk", "clippy", bc.hostTarget, "", T0 + 10_100, { crates: ["rk-leaf"] })); }, /2 broker commands for rk \| clippy/);
  failsFor("NN09", 1, (r) => { r.jobs = r.jobs.filter((j) => !(j.sub === "doc" && j.cwd.endsWith("tools"))); }, /no broker command for tools \| doc/);
  failsFor("NN09", 1, (r) => { r.jobs.push(job("rk", "clippy", bc.hostTarget, "--features nonsense", T0 + 10_100)); }, /does not list/);
  failsFor("NN09", 1, (r) => { r.jobs[0].args.push("--target", "aarch64-apple-darwin"); }, /more than one --target/);
  failsFor("NN09", 1, (r) => { r.jobs.forEach((j, i) => { j.submitted = T0 + 10_000 + i * 6000; j.ended = j.submitted + 5000; }); }, /submitted only after the first job had ended/);
});
test("NN10: a broker that runs the tests, a gate that skips or repeats a signed program, a changed program, an unsigned one, or a failing test that passes, fails", () => {
  failsFor("NN10", 1, (r) => { const t = r.jobs.find((j) => j.sub === "test"); t.effectiveArgv = t.effectiveArgv.filter((a) => a !== "--no-run"); }, /the broker ran the tests/);
  failsFor("NN10", 1, (r) => { r.starts = r.starts.filter((s) => s.exe !== TEST_EXE); }, /started 0 times/);
  failsFor("NN10", 1, (r) => { r.imageNow = {}; }, /does not have the signed hash/);
  failsFor("NN10", 1, (r) => { r.starts.push(prog("D:\\data\\ws\\t\\release\\deps\\other-fedcba9876543210.exe", [], T0 + 30_000, T0 + 30_100)); }, /no receipt signs/);
  failsFor("NN10", 1, (r) => { r.jobs.filter((j) => j.sub === "test").forEach((j) => { j.testExecutables = null; }); }, /signs no list of test programs/);
  failsFor("NN10", 6, (r) => { r.exit = 0; }, /failing exit/);
});
test("NN11: a run over its limit, or a case with no pinned limit, fails", () => {
  failsFor("NN11", 2, (r) => { r.durationMs = 700_000; }, /took 700 s; its limit is 600 s/);
  const cases = { ...CASES, limitsMs: { ...CASES.limitsMs, case3: null } }; const g = good();
  assert.equal(decideCase({ ...def(3), runs: g[3] }, cases).NN11.verdict, FAIL);
});
test("NN12: a lock or a program left behind fails; a kill that the next ordinary run does not recover from fails", () => {
  failsFor("NN12", 2, (r) => { r.locksAfter = [`${SNAP}.lock`]; }, /lock was left behind/);
  failsFor("NN12", 3, (r) => { r.starts[0].end = null; }, /no recorded exit/);
  failsFor("NN12", 7, (r) => { r.exit = 1; }, /did not pass/, 1);
  failsFor("NN12", 7, (r) => { r.killed = false; r.exit = 0; }, /was not killed midway/);
  failsFor("NN12", 7, (r) => { r.locksAfter = [`${SNAP}.lock`]; }, /lock was left behind/, 1);
  // another repository's lock in the same folder is not this run's
  assert.equal(broken("NN12", 2, (r) => { r.locksAfter = ["D:\\data\\snapshots\\other.lock"]; }).verdict, PASS);
});
test("the group of a broker job is its workspace, verb, one target and feature flags; a Class A program is a row's tool, never a version probe", () => {
  const j = job("app/src-tauri", "clippy", "x86_64-unknown-linux-gnu", "--all-features", T0);
  assert.deepEqual(groupOf(j, bc.hostTarget), { workspace: "app/src-tauri", verb: "clippy", target: "x86_64-unknown-linux-gnu", features: "--all-features", crates: [] });
  assert.equal(isClassAProgram(prog("C:\\t\\rustfmt.exe", ["--version"], 0, 1)), false);
  assert.equal(isClassAProgram(prog("C:\\t\\node.exe", ["--check", "a.js"], 0, 1)), true);
  assert.equal(isClassAProgram(prog("C:\\t\\node.exe", ["--check", "D:\\snap\\app\\dist\\main.js"], 0, 1)), false, "a build output checked after the web build is not a Class A row");
  assert.equal(isClassAProgram(prog("C:\\t\\cargo.exe", ["metadata", "--no-deps"], 0, 1)), false);
  assert.equal(isClassAProgram(prog("C:\\t\\cargo.exe", ["deny", "check"], 0, 1)), true);
});
