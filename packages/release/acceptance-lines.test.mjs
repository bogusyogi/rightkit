// Each of the twelve lines is shown to pass a run that meets it and to fail a run that breaks it, on small hand-made records of the same
// shape a real run leaves (log lines, process list, forecast files). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import { CHECK_IDS, CONTRACT_FILE, brokerCompileOf, callKey, judgeRun, linkProblems, parseAcceptanceLines, parseLedger, readAcceptanceLines } from "./acceptance-lines.mjs";

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

const C = "a".repeat(40); const T = "b".repeat(40);
const at = (s) => `2026-11-20T06:00:${String(s).padStart(2, "0")}.000Z`;
const goodLog = [
  `${at(0)} rightkit-gate: snapshot D:\\snap\\k1 is commit ${C} tree ${T} (written=0 removed=0 unchanged=9)`,
  `${at(1)} rightkit-gate: Class A cache decision: decA.json`,
  `${at(2)} rightkit-gate: class A verdict written: v.json`,
  `${at(3)} rightkit-gate: Class B cache decision: decB.json`,
  `${at(8)} rightkit-gate: snapshot D:\\snap\\k1 is still tree ${T} after the check`,
].join("\n");
const decision = (rows, more = {}) => ({ forecast: rows.map(([id, expected, status = "pass"]) => ({ id, expected, status })), actual: rows.map(([id, , , actual]) => ({ id, actual })), mismatches: [], dispatchedRows: [], ...more });
const files = (a, b) => (f) => ({ "decA.json": a, "decB.json": b })[f] ?? (() => { throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); })();
const good = () => files(decision([["fmt", "hit", "pass", "hit"], ["deny", "run", "pass", "ran"]]), decision([["clippy:a", "hit", "fail", "hit"], ["clippy:b", "run", "pass", "ran"]]));
const compile = (sub, rest, second, cwd = "D:\\snap\\k1\\ws") => ({ cmd: "C:\\WINDOWS\\system32\\cmd.exe", args: ["/d", "/s", "/c", `""D:\\managed\\agent-bin\\rustright.cmd" cargo ${sub} ${rest}"`], cwd, at: at(second) });
const goodLedger = [{ cmd: "git", args: ["rev-parse", "HEAD"], cwd: "D:\\repo", at: at(0) }, compile("clippy", "--profile release -p a -p b --locked", 4), compile("test", "--profile release -p a -p b --locked", 5)];
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const judge = (over = {}) => judgeRun({ lines: LINES, log: goodLog, ledger: goodLedger, readJson: good(), limitMs: 60_000, exit: 0, live: { exists: () => false, alive: () => false, pids: [1] }, ...over });

test("there are twelve lines, and a run that meets the measurable ones passes them", () => {
  assert.equal(LINES.length, 12);
  const v = judge();
  for (const id of ["NN01", "NN02", "NN04", "NN05", "NN06", "NN07", "NN08", "NN09", "NN11", "NN12"]) assert.equal(verdictOf(v, id), "pass", `${id}: ${v.lines.find((l) => l.id === id).why}`);
  // No lane times and no broker receipts in these records: said so, never passed.
  assert.equal(verdictOf(v, "NN03"), "not measured"); assert.equal(verdictOf(v, "NN10"), "not measured");
  assert.equal(v.pass, false, "a run with a line not measured is not a pass");
});

test("NN01 fails when the snapshot's tree is another one after the work", () => {
  assert.equal(verdictOf(judge({ log: goodLog.replace(`still tree ${T}`, `still tree ${"c".repeat(40)}`) }), "NN01"), "fail");
  assert.equal(verdictOf(judge({ receipt: { commit: "d".repeat(40) } }), "NN01"), "fail");
});

test("NN02 fails when a compile reaches the broker before the Class A verdict, or after Class A failed", () => {
  assert.equal(verdictOf(judge({ ledger: [compile("clippy", "-p a", 1)] }), "NN02"), "fail");
  const failedA = goodLog.replace("class A verdict written: v.json", "unit cargo:a stopped: class A check (compile tier) failed");
  assert.equal(verdictOf(judge({ log: failedA, ledger: [compile("clippy", "-p a", 4)] }), "NN02"), "fail");
  assert.equal(verdictOf(judge({ log: failedA, ledger: [] }), "NN02"), "pass");
});

test("NN03 is judged from lane times: a row that waited while a lane was free fails", () => {
  const rows = (wait) => ({ scans: [{ id: "x", class: "A", lane: { readyAt: 0, startedAt: 10, endedAt: 500, lanes: 2 } }, { id: "y", class: "A", lane: { readyAt: 0, startedAt: wait, endedAt: 900, lanes: 2 } }] });
  const withRows = (wait) => judgeRun({ lines: LINES, log: `${goodLog}\n${at(9)} rightkit-gate: coverage report cov.json: 2 scan(s)`, ledger: goodLedger, readJson: (f) => (f === "cov.json" ? rows(wait) : good()(f)) });
  assert.equal(verdictOf(withRows(20), "NN03"), "pass");
  assert.equal(verdictOf(withRows(400), "NN03"), "fail");
});

test("NN04 fails on an empty or unreadable forecast", () => {
  assert.equal(verdictOf(judge({ readJson: files({ forecast: [], actual: [], mismatches: [] }, decision([["clippy:a", "hit", "pass", "hit"]])) }), "NN04"), "fail");
  assert.equal(verdictOf(judge({ readJson: () => { throw new Error("gone"); } }), "NN04"), "fail");
});

test("NN05: a wrong forecast passes only when nothing was sent, the row and cause are named and the run failed", () => {
  const wrong = (dispatched) => files(decision([["fmt", "hit", "pass", "ran"]], { mismatches: [{ id: "fmt", reason: "expected entry absent; removal cause unknown" }], dispatchedRows: dispatched }), decision([["clippy:a", "hit", "pass", "hit"]]));
  const stopLog = `${goodLog}\n${at(9)} RK_CACHE_DIVERGENCE {"id":"fmt"}`;
  assert.equal(verdictOf(judge({ readJson: wrong([]), log: stopLog, exit: 1 }), "NN05"), "pass");
  assert.equal(verdictOf(judge({ readJson: wrong(["fmt"]), log: stopLog, exit: 1 }), "NN05"), "fail");
  assert.equal(verdictOf(judge({ readJson: wrong([]), log: goodLog, exit: 1 }), "NN05"), "fail");
  assert.equal(verdictOf(judge({ readJson: wrong([]), log: stopLog, exit: 0 }), "NN05"), "fail");
});

test("NN06 fails when an unchanged row runs again; NN07 fails when a changed row does not run", () => {
  assert.equal(verdictOf(judge({ readJson: files(decision([["fmt", "hit", "pass", "ran"]]), decision([["clippy:a", "hit", "pass", "hit"]])) }), "NN06"), "fail");
  assert.equal(verdictOf(judge({ readJson: files(decision([["fmt", "run", "pass", "hit"]]), decision([["clippy:a", "hit", "pass", "hit"]])) }), "NN07"), "fail");
});

test("NN08 fails on the same call twice, however the program is spelled; a different folder or argument is another call", () => {
  const twice = [...goodLedger, { cmd: "C:\\Program Files\\Git\\cmd\\git.exe", args: ["rev-parse", "HEAD"], cwd: "D:\\repo", at: at(6) }];
  assert.equal(verdictOf(judge({ ledger: twice }), "NN08"), "fail");
  assert.notEqual(callKey({ cmd: "git", args: ["rev-parse", "HEAD"], cwd: "D:\\repo" }), callKey({ cmd: "git", args: ["rev-parse", "HEAD"], cwd: "D:\\other" }));
  assert.equal(callKey({ cmd: "node", args: ["x", "0f8fad5b-d9cb-469f-a165-70867728950e"], cwd: "." }), callKey({ cmd: "node", args: ["x", "7c9e6679-7425-40de-944b-e07fc1f90ae7"], cwd: "." }), "a per-run id is not a different call");
});

test("NN09 fails when one workspace and stage is sent as two commands; another feature set or workspace is its own command", () => {
  assert.equal(verdictOf(judge({ ledger: [compile("clippy", "--profile release -p a --locked", 4), compile("clippy", "--profile release -p b --locked", 5)] }), "NN09"), "fail");
  assert.equal(verdictOf(judge({ ledger: [compile("clippy", "--profile release -p a --locked", 4), compile("clippy", "--profile release -p a --features x --locked", 5), compile("clippy", "--profile release -p c --locked", 6, "D:\\snap\\k1\\other")] }), "NN09"), "pass");
});

test("NN11 is judged only against a limit; NN12 only right after the run", () => {
  assert.equal(verdictOf(judge({ limitMs: 1_000 }), "NN11"), "fail");
  assert.equal(verdictOf(judge({ limitMs: null }), "NN11"), "not measured");
  assert.equal(verdictOf(judge({ live: null }), "NN12"), "not measured");
  assert.equal(verdictOf(judge({ live: { exists: (f) => f.endsWith(".lock"), alive: () => false, pids: [] } }), "NN12"), "fail");
  assert.equal(verdictOf(judge({ live: { exists: () => false, alive: () => true, pids: [7] } }), "NN12"), "fail");
});

test("the records are read as a real run writes them", () => {
  const line = '{"pid":1,"at":"2026-11-20T06:24:41.108Z","sync":false,"cmd":"C:\\\\WINDOWS\\\\system32\\\\cmd.exe","args":["/d","/s","/c","\\"\\"D:\\\\.rightkit-managed\\\\agent-bin\\\\rustright.cmd\\" cargo clippy --profile release --target x86_64-pc-windows-msvc -p witness-engine --locked\\""],"cwd":"D:\\\\snap\\\\k\\\\app"}';
  const [entry] = parseLedger(`${line}\nnot json\n`);
  const c = brokerCompileOf(entry);
  assert.equal(c.sub, "clippy"); assert.ok(c.args.includes("witness-engine"));
  assert.equal(brokerCompileOf({ cmd: "git", args: ["status"], cwd: "." }), null);
  assert.equal(brokerCompileOf({ cmd: "rustright", args: ["cargo", "metadata"], cwd: "." }), null, "a cargo question that compiles nothing is not a compile");
});
