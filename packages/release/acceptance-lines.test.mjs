// Each of the twelve lines is shown to pass a run that meets it and to fail, or to say "not measured", for a run that breaks it or leaves
// no record of it, on small hand-made records of the same shape a real run leaves (log lines, process list, forecast files, coverage
// reports, the Class A verdict, the receipt). A check that cannot fail proves nothing.
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import { CHECK_IDS, CONTRACT_FILE, brokerCompileOf, judgeRun, linkProblems, parseAcceptanceLines, parseLedger, readAcceptanceLines } from "./acceptance-lines.mjs";

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
const C = "a".repeat(40); const T = "b".repeat(40); const LOCK = "7d43ae54-3ea7-46f6-97ed-d9fcfb830c8a"; const SNAP = "D:\\snap\\k1";
const at = (s) => new Date(Date.UTC(2026, 10, 20, 6, 0, 0) + s * 1000).toISOString();
const shim = (sub, rest) => ["/d", "/s", "/c", `""D:\\managed\\agent-bin\\rustright.cmd" cargo ${sub} ${rest}"`];
const entry = (s, cmd, args, cwd = "D:\\repo", ms = 50) => ({ pid: 100, at: at(s), ms, sync: true, cmd, args, cwd, status: 0 });
const compile = (s, sub, rest, cwd = `${SNAP}\\ws`, ms = 2000) => entry(s, "C:\\WINDOWS\\system32\\cmd.exe", shim(sub, rest), cwd, ms);
function records() {
  return {
    commit: C, exit: 0, limitMs: 60_000, live: { exists: () => false, alive: () => false, pids: [100] },
    log: [
      `${at(0)} rightkit-gate: snapshot lock held ${LOCK}`,
      `${at(0)} rightkit-gate: snapshot ${SNAP} is commit ${C} tree ${T} (written=0 removed=0 unchanged=9)`,
      `${at(0)} rightkit-gate: --to compile for ${C.slice(0, 12)}: check -> compile; executor broker`,
      `${at(2)} rightkit-gate: Class A cache decision: decA.json`,
      `${at(2)} rightkit-gate: class A verdict written: v.json`,
      `${at(2)} rightkit-gate: coverage report covA.json: 2 scan(s) in this pass (class A)`,
      `${at(3)} rightkit-gate: snapshot ${SNAP} is still tree ${T} after the check`,
      `${at(9)} rightkit-gate: Class B cache decision: decB.json`,
      `${at(9)} rightkit-gate: coverage report covB.json: 2 scan(s) in this pass (class B)`,
      `${at(10)} rightkit-gate: snapshot ${SNAP} is still tree ${T} after the compile`,
    ],
    files: {
      "decA.json": {
        stage: "class-a", commit: C, forecastFrozenAt: at(1), firstLookupAt: at(1.5),
        fingerprints: [{ id: "rustfmt:2021", inputs: JSON.stringify({ files: [["src/a.rs", "1"], ["src/b.rs", "2"]] }) }],
        forecast: [{ id: "rustfmt:2021", key: "k1", priorKey: "k0", expected: "run", changes: { inputs: ["files.0.1"], inputsChanged: 1 } }, { id: "deny", key: "k2", expected: "hit", status: "pass" }],
        actual: [{ id: "rustfmt:2021", actual: "run", key: "k1" }, { id: "deny", actual: "hit", key: "k2" }],
        mismatches: [], dispatchedRows: ["rustfmt:2021"], phase: "observed",
      },
      "decB.json": {
        stage: "class-b", commit: C, tree: T, forecastFrozenAt: at(5), firstLookupAt: at(5.5),
        forecast: [{ id: "clippy:ws:a", key: "ka", expected: "hit", status: "fail" }, { id: "clippy:ws:b", key: "kb", expected: "run", status: "pass" }],
        actual: [{ id: "clippy:ws:a", actual: "hit", key: "ka", status: "fail" }, { id: "clippy:ws:b", actual: "run", key: "kb", status: "pass" }],
        mismatches: [], dispatchedRows: ["clippy:ws:b"], phase: "observed",
      },
      "v.json": { schema: "rightkit.class-a-verdict/1", commit: C, tree: T, status: "pass" },
      "covA.json": { commit: C, scans: [
        { id: "rustfmt:2021", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1010, endedAt: 1500, lanes: 4 } },
        { id: "deny", class: "A", coverage: "hit", status: "pass" },
        { id: "clippy:ws:a", class: "B", coverage: "skipped", status: "not-in-pass" }, { id: "clippy:ws:b", class: "B", coverage: "skipped", status: "not-in-pass" },
      ] },
      "covB.json": { commit: C, scans: [
        { id: "rustfmt:2021", class: "A", coverage: "skipped", status: "not-in-pass" }, { id: "deny", class: "A", coverage: "skipped", status: "not-in-pass" },
        { id: "clippy:ws:a", class: "B", coverage: "hit", status: "fail" }, { id: "clippy:ws:b", class: "B", coverage: "ran", status: "pass" },
      ] },
    },
    ledger: [
      entry(0, "C:\\Program Files\\Git\\cmd\\git.exe", ["rev-parse", "--verify", `${C}^{commit}`]),
      entry(1.6, "C:\\rust\\bin\\rustfmt.exe", ["--check", "--edition", "2021", "src/a.rs"], SNAP),
      compile(6, "clippy", "--profile release -p b --locked"),
    ],
    end: { end: true, pid: 100, at: at(10), open: 0 },
    extraLines: [],
    receipt: { commit: C, binding: { tree: T }, stages: {
      check: { ok: true, at: at(1), snapshot: { tree: T, intactAfter: true }, snapshotLock: LOCK },
      compile: { ok: true, at: at(6), snapshot: { tree: T, intactAfter: true }, snapshotLock: LOCK },
    } },
  };
}
const ledgerText = (r) => [...r.ledger.map((e) => JSON.stringify(e)), ...r.extraLines, ...(r.end ? [JSON.stringify(r.end)] : [])].join("\n");
const run = (r) => judgeRun({
  lines: LINES, log: r.log.join("\n"), ledger: parseLedger(ledgerText(r)), receipt: r.receipt, commit: r.commit, exit: r.exit, limitMs: r.limitMs, live: r.live,
  readJson: (f) => { if (!(f in r.files)) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); return structuredClone(r.files[f]); },
});
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const decB = (r) => r.files["decB.json"]; const decA = (r) => r.files["decA.json"];
const scan = (r, file, id) => r.files[file].scans.find((s) => s.id === id);

test("a run whose records meet every line passes eleven; NN10 stays not measured until the broker's receipt is read", () => {
  const v = run(records());
  for (const id of twelve.filter((x) => x !== "NN10")) assert.equal(verdictOf(v, id), "pass", `${id}: ${whyOf(v, id)}`);
  assert.equal(verdictOf(v, "NN10"), "not measured");
  assert.equal(v.pass, false, "a run with a line not measured is not a pass");
});

// ---- the cases each line must fail, or must not pass -----------------------------------------------------------------------------------------
// Those marked (review) are gatecodex's findings of 2026-11-20 on the step-1 checks; each one passed before the fix.

test("NN01: only the selected commit is built", () => {
  expect(alter((r) => r.log.splice(9, 1, `${at(10)} rightkit-gate: snapshot ${SNAP} is still tree ${"c".repeat(40)} after the compile`)), "NN01", "fail");
  expect(alter((r) => { r.receipt.commit = "d".repeat(40); }), "NN01", "fail");
  // (review) the log can be self-consistent and still describe a commit other than the one selected.
  expect(alter((r) => { r.commit = "e".repeat(40); }), "NN01", "fail", /selected/);
  expect(alter((r) => { r.commit = undefined; }), "NN01", "not measured", /selected/);
  expect(alter((r) => { r.log[2] = `${at(0)} rightkit-gate: --to compile for ${"f".repeat(12)}: check -> compile`; }), "NN01", "fail");
  // (review) the receipt must be this run's and bound to the snapshot's tree.
  expect(alter((r) => { r.receipt = null; }), "NN01", "not measured", /receipt/);
  expect(alter((r) => { r.receipt.binding.tree = "c".repeat(40); }), "NN01", "fail");
  expect(alter((r) => { r.receipt.stages.compile.snapshotLock = "00000000-0000-0000-0000-000000000000"; }), "NN01", "fail", /lock/);
  // A stage the receipt carries from an earlier run of the same commit and tree (recorded before this run began) is no other commit built.
  expect(alter((r) => { Object.assign(r.receipt.stages.compile, { at: at(-600), snapshotLock: "00000000-0000-0000-0000-000000000000" }); }), "NN01", "pass");
  expect(alter((r) => { delete r.receipt.stages.compile.at; }), "NN01", "not measured", /no time/);
  expect(alter((r) => { r.receipt.stages.check.snapshot.intactAfter = false; }), "NN01", "fail");
  expect(alter((r) => { decB(r).commit = "d".repeat(40); }), "NN01", "fail");
});

test("NN02: Class A finishes before anything goes to the broker; if it fails, nothing goes", () => {
  expect(alter((r) => { r.ledger[2].at = at(1); }), "NN02", "fail", /before/);
  // (review) a process-list line that is not a whole record is not dropped silently: it leaves the list incomplete.
  expect(alter((r) => { r.extraLines.push("{not json"); }), "NN02", "not measured", /not JSON/);
  expect(alter((r) => { delete r.ledger[2].at; }), "NN02", "not measured", /has no at/);
  // (review) a "verdict written" line is not a pass: the verdict must say pass, for this commit and tree.
  expect(alter((r) => { r.files["v.json"].status = "fail"; }), "NN02", "fail");
  expect(alter((r) => { r.files["v.json"].commit = "d".repeat(40); }), "NN02", "fail");
  // (review) "nothing was sent" after a red Class A is shown only from a complete process list.
  const red = (r) => { r.files["v.json"].status = "fail"; r.ledger.splice(2, 1); r.log.splice(7, 2); r.receipt.stages.compile = undefined; };
  expect(alter(red), "NN02", "pass", /nothing was sent/);
  expect(alter((r) => { red(r); r.end = null; }), "NN02", "not measured", /end record/);
  expect(alter((r) => { red(r); r.end.open = 1; }), "NN02", "not measured", /still open/);
  expect(alter((r) => { red(r); r.ledger.push(compile(6, "test", "--profile release -p b --locked")); }), "NN02", "fail");
});

test("NN03: Class A starts ready, independent work whenever an eligible lane is free", () => {
  const lanes = (rows) => (r) => { r.files["covA.json"].scans = [...rows, { id: "deny", class: "A", coverage: "hit", status: "pass" }]; };
  const row = (id, readyAt, startedAt, endedAt, n = 2) => ({ id, class: "A", coverage: "ran", status: "pass", lane: { readyAt, startedAt, endedAt, lanes: n } });
  expect(alter(lanes([row("x", 0, 10, 500), row("y", 0, 20, 900)])), "NN03", "pass");
  expect(alter(lanes([row("x", 0, 10, 500), row("y", 0, 400, 900, 2)])), "NN03", "fail", /waited/);
  // (review) every lane busy when the row became ready, then one freed while it still waited: a single look at the ready time missed it.
  expect(alter(lanes([row("x", 0, 0, 500), row("y", 0, 0, 1000), row("z", 0, 900, 1200)])), "NN03", "fail", /z waited/);
  // (review) a row that ran with no lane times leaves the line not measured, it is not skipped.
  expect(alter(lanes([row("x", 0, 10, 500), { id: "y", class: "A", coverage: "ran", status: "pass" }])), "NN03", "not measured", /y/);
});

test("NN04: before the first cache lookup, the gate records the expected rows and which will be reused and which will run", () => {
  expect(alter((r) => { decA(r).forecast = []; }), "NN04", "fail");
  // (review) a forecast that does not say when it was frozen does not show it came before the first lookup.
  expect(alter((r) => { delete decB(r).forecastFrozenAt; }), "NN04", "not measured", /frozen/);
  expect(alter((r) => { decB(r).forecastFrozenAt = at(6); }), "NN04", "fail", /after the first lookup/);
  // (review) incomplete, repeated or another run's rows, and a stage that worked without a forecast.
  expect(alter((r) => { decB(r).forecast.push({ ...decB(r).forecast[0] }); }), "NN04", "fail", /more than once/);
  expect(alter((r) => { decB(r).commit = "d".repeat(40); }), "NN04", "fail", /commit/);
  expect(alter((r) => { r.log.splice(7, 1); }), "NN04", "fail", /Class B/);
  expect(alter((r) => { r.files["covB.json"].scans.push({ id: "clippy:ws:c", class: "B", coverage: "hit", status: "pass" }); }), "NN04", "fail", /clippy:ws:c/);
});

test("NN05: a lookup that disagrees stops the gate before work is sent and names the row and cause; otherwise the work that ran matches", () => {
  const diverged = (r) => { const a = decB(r).actual.find((x) => x.id === "clippy:ws:a"); a.actual = "run"; decB(r).mismatches = [{ id: "clippy:ws:a", reason: "expected entry absent; removal cause unknown" }]; decB(r).dispatchedRows = []; scan(r, "covB.json", "clippy:ws:b").coverage = "hit"; r.ledger.splice(2, 1); r.log.push(`${at(9)} RK_CACHE_DIVERGENCE {"id":"clippy:ws:a"}`); r.exit = 1; };
  expect(alter(diverged), "NN05", "pass", /stopped/);
  expect(alter((r) => { diverged(r); r.exit = 0; }), "NN05", "fail", /exited 0/);
  expect(alter((r) => { diverged(r); r.log.pop(); }), "NN05", "fail", /name/);
  expect(alter((r) => { diverged(r); r.ledger.push(compile(9.5, "clippy", "--profile release -p b --locked")); }), "NN05", "fail", /sent/);
  // (review) mismatches[] can be empty while lookup and forecast disagree: the disagreement is found by joining the rows, not read from it.
  expect(alter((r) => { decB(r).actual.find((x) => x.id === "clippy:ws:a").actual = "run"; }), "NN05", "fail");
  // (review) a missing dispatch list or exit code does not pass a divergence.
  expect(alter((r) => { diverged(r); delete decB(r).dispatchedRows; }), "NN05", "not measured", /dispatch/);
  expect(alter((r) => { diverged(r); r.exit = null; }), "NN05", "not measured", /exit/);
  expect(alter((r) => { diverged(r); r.end = null; }), "NN05", "not measured", /end record/);
  // (review) agreement still needs the work that ran to be exactly the forecast's: no row missing, none extra, in every record.
  expect(alter((r) => { decB(r).dispatchedRows.push("clippy:ws:a"); }), "NN05", "fail", /clippy:ws:a/);
  expect(alter((r) => { scan(r, "covB.json", "clippy:ws:a").coverage = "ran"; }), "NN05", "fail", /clippy:ws:a/);
  expect(alter((r) => { r.ledger.push(compile(7, "doc", "--profile release -p b --locked")); }), "NN05", "fail", /doc/);
});

test("NN06: a row whose inputs did not change is never run again, including one that failed before", () => {
  expect(alter((r) => { decB(r).actual.find((x) => x.id === "clippy:ws:a").actual = "run"; }), "NN06", "fail");
  // (review) a forecast hit with no recorded lookup result is not a reuse.
  expect(alter((r) => { decB(r).actual = decB(r).actual.filter((x) => x.id !== "clippy:ws:a"); }), "NN06", "not measured", /clippy:ws:a/);
  // (review) a reused failure must still be a failure: a cached failure that turns into success was not reused.
  expect(alter((r) => { decB(r).actual.find((x) => x.id === "clippy:ws:a").status = "pass"; }), "NN06", "fail", /failure/);
  expect(alter((r) => { scan(r, "covB.json", "clippy:ws:a").status = "pass"; }), "NN06", "fail", /failure/);
  // (review) a hit under another key (a changed toolchain changes the key) is not this row's reuse.
  expect(alter((r) => { decB(r).actual.find((x) => x.id === "clippy:ws:a").key = "other"; }), "NN06", "fail", /key/);
  // (review) the process list and the coverage report are checked too: a reused row must not have run.
  expect(alter((r) => { r.ledger.push(compile(7, "clippy", "--profile release -p a --locked")); }), "NN06", "fail", /clippy:ws:a/);
  expect(alter((r) => { scan(r, "covB.json", "clippy:ws:a").coverage = "ran"; }), "NN06", "fail", /clippy:ws:a/);
  expect(alter((r) => { scan(r, "covA.json", "deny").coverage = "ran"; }), "NN06", "fail", /deny/);
});

test("NN07: a row whose inputs did change is always run again, and does only the work the change needs", () => {
  expect(alter((r) => { decB(r).actual.find((x) => x.id === "clippy:ws:b").actual = "hit"; }), "NN07", "fail");
  // (review) rustfmt handed a file the change did not touch (the rk-core recording): it ran, but did more than the change needs.
  expect(alter((r) => { r.ledger[1].args.push("src/b.rs"); }), "NN07", "fail", /src\/b\.rs/);
  // (review) a changed row that the forecast and the gate say ran, with no compile in a complete process list naming it.
  expect(alter((r) => { r.ledger.splice(2, 1); }), "NN07", "fail", /clippy:ws:b/);
  expect(alter((r) => { r.ledger.splice(2, 1, compile(6, "clippy", "--profile release -p a -p b --locked")); }), "NN07", "fail", /\ba\b/);
  // A forecast that lists only some of its changed inputs cannot show the scope, and says so.
  expect(alter((r) => { decA(r).forecast[0].changes.inputsChanged = 7; }), "NN07", "not measured", /changed inputs/);
});

test("NN08: nothing is started twice in one run", () => {
  const plus = (e) => (r) => { r.ledger.push(e); };
  // The same program spelled by name and by path, the same arguments and folder: one call made twice.
  expect(alter(plus(entry(8, "git", ["rev-parse", "--verify", `${C}^{commit}`]))), "NN08", "fail", /2x/);
  expect(alter(plus(entry(8, "git", ["rev-parse", "--verify", `${C}^{commit}`], "D:\\other"))), "NN08", "pass");
  // (review) two different programs of one name are two calls; so are arguments that differ only in letter case or in an id.
  expect(alter(plus(entry(8, "D:\\portable\\git\\bin\\git.exe", ["rev-parse", "--verify", `${C}^{commit}`]))), "NN08", "pass");
  expect(alter((r) => { r.ledger.push(entry(8, "node", ["x.mjs", "Src/A.rs"]), entry(9, "node", ["x.mjs", "src/a.rs"])); }), "NN08", "pass");
  expect(alter((r) => { r.ledger.push(entry(8, "node", ["cancel", "0f8fad5b-d9cb-469f-a165-70867728950e"]), entry(9, "node", ["cancel", "7c9e6679-7425-40de-944b-e07fc1f90ae7"])); }), "NN08", "pass");
  // (review) a documented per-run part (a probe's temp folder) does not make a repeat into two calls.
  expect(alter((r) => { r.ledger.push(entry(8, "cargo", ["--version"], "D:\\t\\rk-probe-AAAA"), entry(9, "cargo", ["--version"], "D:\\t\\rk-probe-BBBB")); }), "NN08", "fail", /2x cargo/);
  // (review) "no repeat" is shown only from a complete process list whose entries can all be compared.
  expect(alter((r) => { r.end = null; }), "NN08", "not measured", /end record/);
  expect(alter(plus(entry(8, "node", ["big.mjs", "a", "... 52 args"]))), "NN08", "not measured", /cut short/);
});

test("NN09: compatible compiles are combined per workspace and stage; every split has a reason; independent groups run together", () => {
  expect(alter((r) => { r.ledger.splice(2, 1, compile(6, "clippy", "--profile release -p b --locked"), compile(6.5, "clippy", "--profile release -p a --locked")); }), "NN09", "fail", /2x cargo clippy/);
  expect(alter((r) => { r.ledger.splice(2, 1, compile(6, "clippy", "--profile release -p b --target x --locked"), compile(6.1, "clippy", "--profile release -p b --target y --locked")); }), "NN09", "fail");
  // Another feature set is another stage, and overlapping groups ran together.
  expect(alter((r) => { r.ledger.push(compile(6.5, "clippy", "--profile release -p b --features z --locked")); }), "NN09", "pass");
  // (review) "nothing was sent" is shown only from a complete process list.
  expect(alter((r) => { r.ledger.splice(2, 1); r.end = null; }), "NN09", "not measured", /end record/);
  // (review) independent groups that ran one after the other cannot pass without the record of the free capacity they waited beside.
  expect(alter((r) => { r.ledger.push(compile(9, "clippy", "--profile release -p c --locked", `${SNAP}\\other`)); }), "NN09", "not measured", /one after the other/);
});

test("NN11 is judged only against a limit, over times that run forwards; NN12 only right after the run", () => {
  expect(alter((r) => { r.limitMs = 1_000; }), "NN11", "fail");
  expect(alter((r) => { r.limitMs = null; }), "NN11", "not measured");
  // (review) a log whose last time is earlier than its first gives no interval to judge.
  expect(alter((r) => { r.log.push(`${at(-60)} rightkit-gate: late line`); }), "NN11", "not measured", /backwards/);
  expect(alter((r) => { r.live = null; }), "NN12", "not measured");
  expect(alter((r) => { r.live = { exists: (f) => f.endsWith(".lock"), alive: () => false, pids: [] }; }), "NN12", "fail");
  expect(alter((r) => { r.live = { exists: () => false, alive: () => true, pids: [7] }; }), "NN12", "fail");
  // (review) an unknown process list is not an empty one.
  expect(alter((r) => { r.live = { exists: () => false, alive: () => false }; }), "NN12", "not measured", /process/);
});

// Shapes found in the recorded runs of full run 4 (D:\scratch\loop-review\muse-fulldev-4) that the first fixed checks misread.
test("record shapes real runs write: uncacheable rows, both change-list spellings, gate-run test suites, feature-set variants", () => {
  // An uncacheable row is forecast, looked up and covered as such and runs every time: it agrees with itself, and NN06 cannot pass while it exists.
  const uncacheable = (r) => { decA(r).forecast.push({ id: "shellcheck", expected: "uncacheable" }); decA(r).actual.push({ id: "shellcheck", actual: "uncacheable" }); decA(r).dispatchedRows.push("shellcheck"); r.files["covA.json"].scans.push({ id: "shellcheck", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1020, endedAt: 1100, lanes: 4 } }); };
  expect(alter(uncacheable), "NN04", "pass"); expect(alter(uncacheable), "NN05", "pass");
  expect(alter(uncacheable), "NN06", "not measured", /shellcheck is uncacheable/);
  // A key's inputs carry the key schema after the JSON, and changed inputs are named by index or by path.
  const pathForm = (r) => { decA(r).fingerprints[0].inputs += ":rightkit.class-a-key/3"; decA(r).forecast[0].changes = { inputs: ["files/src/a.rs"], inputsChanged: 1, contentOnly: true }; };
  expect(alter(pathForm), "NN07", "pass");
  expect(alter((r) => { pathForm(r); r.ledger[1].args.push("src/b.rs"); }), "NN07", "fail", /src\/b\.rs/);
  // A JavaScript test suite (test:npm:*) is run by the gate, not compiled by the broker: no broker compile is owed for it.
  const npm = (r) => { decB(r).forecast.push({ id: "test:npm:app-lib", key: "kn", expected: "run" }); decB(r).actual.push({ id: "test:npm:app-lib", actual: "run", key: "kn" }); decB(r).dispatchedRows.push("test:npm:app-lib"); r.files["covB.json"].scans.push({ id: "test:npm:app-lib", class: "B", coverage: "ran", status: "pass" }); };
  expect(alter(npm), "NN07", "pass"); expect(alter(npm), "NN05", "pass");
  // Two feature sets of one crate (`:0`, `:1`): a compile for the changed one names the crate the unchanged one shares, and which it built is unknown.
  const variants = (r) => { const d = decB(r); d.forecast = [{ id: "clippy:ws:b:0", key: "k0", expected: "hit", status: "pass" }, { id: "clippy:ws:b:1", key: "k1", expected: "run" }]; d.actual = [{ id: "clippy:ws:b:0", actual: "hit", key: "k0", status: "pass" }, { id: "clippy:ws:b:1", actual: "run", key: "k1" }]; d.dispatchedRows = ["clippy:ws:b:1"]; r.files["covB.json"].scans = [{ id: "clippy:ws:b:0", class: "B", coverage: "hit", status: "pass" }, { id: "clippy:ws:b:1", class: "B", coverage: "ran", status: "pass" }]; r.ledger[2] = compile(6, "clippy", "--profile release -p b --features z --locked"); };
  expect(alter(variants), "NN06", "not measured", /feature set/);
});

test("the records are read as a real run writes them", () => {
  const line = '{"pid":1,"at":"2026-11-20T06:24:41.108Z","ms":5,"sync":false,"cmd":"C:\\\\WINDOWS\\\\system32\\\\cmd.exe","args":["/d","/s","/c","\\"\\"D:\\\\.rightkit-managed\\\\agent-bin\\\\rustright.cmd\\" cargo clippy --profile release --target x86_64-pc-windows-msvc -p witness-engine --locked\\""],"cwd":"D:\\\\snap\\\\k\\\\app","status":0}';
  const ledger = parseLedger(`${line}\nnot json\n`);
  assert.equal(ledger.length, 1);
  const c = brokerCompileOf(ledger[0]);
  assert.equal(c.sub, "clippy"); assert.ok(c.args.includes("witness-engine"));
  assert.equal(brokerCompileOf({ cmd: "git", args: ["status"], cwd: "." }), null);
  assert.equal(brokerCompileOf({ cmd: "rustright", args: ["cargo", "metadata"], cwd: "." }), null, "a cargo question that compiles nothing is not a compile");
});
