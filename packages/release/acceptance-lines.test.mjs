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
    commit: C, runPid: 100, exit: 0, limitMs: 60_000, live: { exists: () => false, alive: () => false, pids: [100] },
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
      `${at(10)} rightkit-gate: 3 scan(s) for 1 changed file(s); class A scans 4 at a time`,
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
        stage: "class-b", commit: C, tree: T, forecastFrozenAt: at(0.9), firstLookupAt: at(5.5),
        forecast: [{ id: "clippy:ws:a", key: "ka", expected: "hit", status: "fail" }, { id: "clippy:ws:b", key: "kb", expected: "run", status: "pass" }],
        actual: [{ id: "clippy:ws:a", actual: "hit", key: "ka", status: "fail" }, { id: "clippy:ws:b", actual: "run", key: "kb", status: "pass" }],
        mismatches: [], dispatchedRows: ["clippy:ws:b"], phase: "observed",
      },
      "v.json": { schema: "rightkit.class-a-verdict/1", commit: C, tree: T, status: "pass" },
      "covA.json": { commit: C, scans: [
        { id: "rustfmt:2021", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1000, endedAt: 1500, lanes: 4 } },
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
    end: { end: true, pid: 100, at: at(10), open: 0, root: true, lock: LOCK },
    tree: { "ci/shards.json": { solo: [] } },
    extraLines: [],
    receipt: { commit: C, binding: { tree: T }, stages: {
      check: { ok: true, at: at(1), snapshot: { tree: T, intactAfter: true }, snapshotLock: LOCK },
      compile: { ok: true, at: at(6), snapshot: { tree: T, intactAfter: true }, snapshotLock: LOCK },
    } },
  };
}
const ledgerText = (r) => [...r.ledger.map((e) => JSON.stringify(e)), ...r.extraLines, ...(r.end ? [JSON.stringify(r.end)] : [])].join("\n");
const run = (r) => judgeRun({
  lines: LINES, log: r.log.join("\n"), ledger: parseLedger(ledgerText(r)), receipt: r.receipt, commit: r.commit, runPid: r.runPid, exit: r.exit, limitMs: r.limitMs, live: r.live,
  readTree: (p) => { if (!(p in r.tree)) throw Object.assign(new Error(`ENOENT ${p}`), { code: "ENOENT" }); return structuredClone(r.tree[p]); },
  readJson: (f) => { if (!(f in r.files)) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); return structuredClone(r.files[f]); },
});
const alter = (fn) => { const r = records(); fn(r); return r; };
const verdictOf = (v, id) => v.lines.find((l) => l.id === id).verdict;
const whyOf = (v, id) => v.lines.find((l) => l.id === id).why;
const expect = (r, id, verdict, why) => { const v = run(r); assert.equal(verdictOf(v, id), verdict, `${id}: ${whyOf(v, id)}`); if (why) assert.match(whyOf(v, id), why); };
const decB = (r) => r.files["decB.json"]; const decA = (r) => r.files["decA.json"];
const scan = (r, file, id) => r.files[file].scans.find((s) => s.id === id);
/** Class B wholly reused: b unchanged too, nothing dispatched, nothing compiled. */
const wholeReuseB = (r) => { const d = decB(r); d.forecast[1].expected = "hit"; d.actual[1].actual = "hit"; d.dispatchedRows = []; scan(r, "covB.json", "clippy:ws:b").coverage = "hit"; r.ledger.splice(2, 1); };

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
  // (review 2) an earlier receipt stage under another lock does not cover a stage this run did work in: this run compiles row b.
  expect(alter((r) => { Object.assign(r.receipt.stages.compile, { at: at(-600), snapshotLock: "00000000-0000-0000-0000-000000000000" }); }), "NN01", "fail", /did work/);
  // A stage carried from an earlier run of the same commit and tree is accepted only when every record shows this run did none of its work.
  expect(alter((r) => { wholeReuseB(r); Object.assign(r.receipt.stages.compile, { at: at(-600), snapshotLock: "00000000-0000-0000-0000-000000000000" }); }), "NN01", "pass");
  expect(alter((r) => { wholeReuseB(r); r.end = null; Object.assign(r.receipt.stages.compile, { at: at(-600), snapshotLock: "00000000-0000-0000-0000-000000000000" }); }), "NN01", "not measured", /carried/);
  expect(alter((r) => { delete r.receipt.stages.compile.at; }), "NN01", "not measured", /no time/);
  expect(alter((r) => { r.receipt.stages.check.snapshot.intactAfter = false; }), "NN01", "fail");
  expect(alter((r) => { decB(r).commit = "d".repeat(40); }), "NN01", "fail");
});

test("NN02: Class A finishes before anything goes to the broker; if it fails, nothing goes", () => {
  expect(records(), "NN02", "pass", /Class A passed .* first, then 1 compile\(s\)/);
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
  // The rows given replace Class A's changed row in every record: forecast, lookup, dispatch and coverage.
  const lanes = (rows) => (r) => {
    const d = decA(r); const deny = (xs) => xs.filter((x) => x.id === "deny");
    d.forecast = [...rows.map((x) => ({ id: x.id, key: `k-${x.id}`, expected: "run" })), ...deny(d.forecast)];
    d.actual = [...rows.map((x) => ({ id: x.id, actual: "run", key: `k-${x.id}` })), ...deny(d.actual)];
    d.dispatchedRows = rows.filter((x) => x.coverage === "ran").map((x) => x.id);
    r.files["covA.json"].scans = [...rows, { id: "deny", class: "A", coverage: "hit", status: "pass" }];
  };
  const row = (id, readyAt, startedAt, endedAt, n = 2) => ({ id, class: "A", coverage: "ran", status: "pass", lane: { readyAt, startedAt, endedAt, lanes: n } });
  expect(alter(lanes([row("x", 0, 0, 500), row("y", 0, 0, 900)])), "NN03", "pass");
  // Council CV-1 F-8: no blanket allowance; a wait of 20 ms beside a free lane is a wait, unless the lane record states that resolution.
  expect(alter(lanes([row("x", 0, 0, 500), row("y", 0, 20, 900)])), "NN03", "fail", /y waited 20 ms/);
  expect(alter(lanes([row("x", 0, 0, 500), { ...row("y", 0, 20, 900), lane: { readyAt: 0, startedAt: 20, endedAt: 900, lanes: 2, resolutionMs: 50 } }])), "NN03", "pass");
  // The lane count is the log's announcement, not the row report's word: with none announced, a wait cannot be judged.
  expect(alter((r) => { lanes([row("x", 0, 0, 500), row("y", 0, 400, 900, 1)])(r); r.log = r.log.filter((l) => !/at a time/.test(l)); }), "NN03", "not measured", /not announced/);
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
  expect(alter((r) => { decB(r).forecastFrozenAt = at(6); }), "NN04", "fail", /after the run's first lookup/);
  // Council CV-1 F-10: the line is about the run's first lookup, so a Class B forecast frozen after Class A's first lookup fails too.
  expect(alter((r) => { decB(r).forecastFrozenAt = at(2); }), "NN04", "fail", /after the run's first lookup/);
  // (review) incomplete, repeated or another run's rows, and a stage that worked without a forecast.
  expect(alter((r) => { decB(r).forecast.push({ ...decB(r).forecast[0] }); }), "NN04", "fail", /more than once/);
  expect(alter((r) => { decB(r).commit = "d".repeat(40); }), "NN04", "fail", /commit/);
  expect(alter((r) => { r.log.splice(7, 1); }), "NN04", "fail", /Class B/);
  expect(alter((r) => { r.files["covB.json"].scans.push({ id: "clippy:ws:c", class: "B", coverage: "hit", status: "pass" }); }), "NN04", "fail", /clippy:ws:c/);
});

test("NN05: a lookup that disagrees stops the gate before work is sent and names the row and cause; otherwise the work that ran matches", () => {
  const diverged = (r) => { const a = decB(r).actual.find((x) => x.id === "clippy:ws:a"); a.actual = "run"; decB(r).mismatches = [{ id: "clippy:ws:a", reason: "expected entry absent; removal cause unknown" }]; decB(r).dispatchedRows = []; scan(r, "covB.json", "clippy:ws:b").coverage = "hit"; r.ledger.splice(2, 1); r.log.push(`${at(10)} RK_CACHE_DIVERGENCE {"id":"clippy:ws:a","reason":"expected entry absent; removal cause unknown"}`); r.exit = 1; };
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
  // Passes: both unchanged rows reused under their keys, the earlier failure still a failure, and no process or coverage row ran either.
  expect(records(), "NN06", "pass", /2 unchanged row\(s\) reused, 1 of them an earlier failure that stayed one/);
  expect(alter((r) => { decB(r).forecast[0].status = "pass"; decB(r).actual[0].status = "pass"; scan(r, "covB.json", "clippy:ws:a").status = "pass"; }), "NN06", "pass", /2 unchanged row\(s\) reused$/);
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
  expect(alter((r) => { r.ledger.push(compile(6, "clippy", "--profile release -p b --features z --locked")); }), "NN09", "pass");
  // Council CV-1 F-27: commands that did not start together cannot pass on overlap alone, without a record of capacity and readiness.
  expect(alter((r) => { r.ledger.push(compile(6.5, "clippy", "--profile release -p b --features z --locked")); }), "NN09", "not measured", /different times/);
  // (review) "nothing was sent" is shown only from a complete process list.
  expect(alter((r) => { r.ledger.splice(2, 1); r.end = null; }), "NN09", "not measured", /end record/);
  // (review) independent groups that ran one after the other cannot pass without the record of the free capacity they waited beside.
  expect(alter((r) => { r.ledger.push(compile(9, "clippy", "--profile release -p c --locked", `${SNAP}\\other`)); }), "NN09", "not measured", /different times/);
});

test("NN11 is judged only against a limit, over times that run forwards; NN12 only right after the run", () => {
  // Passes: ten seconds of forward log against a sixty-second limit; right after the run, no lock and no process of the run alive.
  expect(records(), "NN11", "pass", /10\.0 s from its first to its last recorded event, limit 60\.0 s/);
  expect(records(), "NN12", "pass", /no lock and no process left behind/);
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

// gatecodex's second review (2026-11-20, on 19d714e2d): eight inputs the checks passed falsely, its four rulings, and a justified split.
test("review 2: the eight false passes, and the rulings on reuse, uncacheable rows and splits", () => {
  const rr = (sub, rest) => entry(1, "C:\\WINDOWS\\system32\\cmd.exe", shim(sub, rest), `${SNAP}\\ws`);
  // NN02: anything that goes to the broker counts, not only compiles: a broker metadata call before Class A passed.
  expect(alter((r) => { r.ledger.push(rr("metadata", "--format-version 1")); }), "NN02", "fail", /before the Class A verdict/);
  expect(alter((r) => { r.ledger.push(entry(0.5, "C:\\WINDOWS\\system32\\where.exe", ["rustright"])); }), "NN02", "pass");
  // NN03: a row dispatched and run that the coverage report leaves out has no lane record; a report of zero lanes is no lane record.
  expect(alter((r) => { decA(r).forecast.push({ id: "secret-scan", key: "ks", expected: "run" }); decA(r).actual.push({ id: "secret-scan", actual: "run", key: "ks" }); decA(r).dispatchedRows.push("secret-scan"); }), "NN03", "not measured", /secret-scan/);
  expect(alter((r) => { Object.assign(scan(r, "covA.json", "rustfmt:2021").lane, { readyAt: 0, startedAt: 5000, endedAt: 6000, lanes: 0 }); }), "NN03", "not measured", /lanes/);
  // Ruling 1: "no row ran" passes only for a proven, complete, wholly reused Class A with nothing owed or dispatched.
  const reuseA = (r) => { decA(r).forecast[0].expected = "hit"; decA(r).actual[0].actual = "hit"; decA(r).dispatchedRows = []; scan(r, "covA.json", "rustfmt:2021").coverage = "hit"; delete scan(r, "covA.json", "rustfmt:2021").lane; };
  expect(alter(reuseA), "NN03", "pass", /wholly reused/);
  expect(alter((r) => { reuseA(r); decA(r).dispatchedRows = ["rustfmt:2021"]; }), "NN03", "not measured");
  expect(alter((r) => { reuseA(r); decA(r).forecast[0].expected = "run"; }), "NN03", "not measured");
  // NN04: a broker compile that starts before the Class B forecast is frozen.
  expect(alter((r) => { r.ledger[2].at = at(0.5); }), "NN04", "fail", /before the Class B forecast was frozen/);
  // NN05: two contradictory lookup results for one row, the right one last.
  expect(alter((r) => { decB(r).actual.unshift({ id: "clippy:ws:a", actual: "run", key: "ka", status: "fail" }); }), "NN05", "fail", /clippy:ws:a .*more than once/);
  // NN06: no Class B coverage and a cut process list cannot show that a reused row did not run.
  expect(alter((r) => { r.log.splice(8, 1); r.end = null; }), "NN06", "not measured", /coverage|process list/);
  // Ruling 2: an uncacheable row whose own record shows its inputs unchanged, and which ran, did unchanged work again.
  expect(alter((r) => { decA(r).forecast.push({ id: "shellcheck", key: "kx", priorKey: "kx", expected: "uncacheable" }); decA(r).actual.push({ id: "shellcheck", actual: "uncacheable" }); decA(r).dispatchedRows.push("shellcheck"); r.files["covA.json"].scans.push({ id: "shellcheck", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1020, endedAt: 1100, lanes: 4 } }); }), "NN06", "fail", /shellcheck/);
  // NN09: the same settings in another flag order are one group; a split for a crate the commit's ci/shards.json names solo is justified.
  const two = (second) => (r) => { decB(r).forecast.push({ id: "clippy:ws:c", key: "kc", expected: "run" }); decB(r).actual.push({ id: "clippy:ws:c", actual: "run", key: "kc" }); decB(r).dispatchedRows.push("clippy:ws:c"); r.files["covB.json"].scans.push({ id: "clippy:ws:c", class: "B", coverage: "ran", status: "pass" }); r.ledger.push(compile(6, "clippy", second)); };
  expect(alter(two("--locked -p c --profile release")), "NN09", "fail", /2x cargo clippy/);
  const solo = (r) => { r.tree["ci/shards.json"] = { solo: ["c"] }; };
  expect(alter((r) => { two("--profile release -p c --locked")(r); solo(r); }), "NN09", "pass", /solo/);
  expect(alter((r) => { two("--profile release -p c --locked")(r); r.tree["ci/shards.json"] = { solo: ["x"] }; }), "NN09", "fail", /2x cargo clippy/);
  expect(alter((r) => { two("--profile release -p c --locked")(r); delete r.tree["ci/shards.json"]; }), "NN09", "not measured", /shards\.json/);
});

// ---- the frozen step-2 batch (D:\scratch\loop-review\twelve-line-step2\final-batch-cases.md, sha256 70ff356d) ---------------------------------------
// All 63 cases, with their authors' ids and expected verdicts unchanged: gatecodex's 48 (GCX-*) and chat "gate"'s 15 (gate 1 to gate 15, 12 and
// 14 being the two it found already right). Each is a mutation of these records. Their fixture* inputs reach the checks through the readers a
// real run uses: files of the run's own Git tree (r.tree) and the mutable snapshot folder (r.files). One table, one loop, one result per id.
const lane = (r) => scan(r, "covA.json", "rustfmt:2021").lane;
const twoChanged = (r) => { decA(r).forecast[0].changes = { inputs: ["files.0.1", "files.1.1"], inputsChanged: 2 }; };
const split = (r, left, right) => {
  const args = [left, right];
  r.ledger.splice(2, 1, ...args.map((a) => compile(6, "clippy", `--locked ${a}`)));
  const ids = [...new Set(args.flatMap((a) => Array.from(a.matchAll(/-p (\w+)/g), (m) => `clippy:ws:${m[1]}`)))];
  const d = decB(r);
  d.forecast = ids.map((id) => ({ id, key: id, expected: "run" }));
  d.actual = ids.map((id) => ({ id, key: id, actual: "run", status: "pass" }));
  d.dispatchedRows = ids;
  r.files["covB.json"].scans = ids.map((id) => ({ id, class: "B", coverage: "ran", status: "pass" }));
};
// gatecodex's fixturePolicy: the policy at the run's tree (null: that tree's policy cannot be read); atReview: the mutable snapshot's copy.
const policy = (r, atRun, atReview) => {
  if (atRun === null) delete r.tree["ci/shards.json"]; else r.tree["ci/shards.json"] = { solo: atRun };
  r.files[`${SNAP}\\ci\\shards.json`] = { solo: atReview };
};
function gate11(r) { r.ledger.push(compile(6, "clippy", "--profile release -p c --locked", `${SNAP}\\ws\\sub`)); const d = decB(r); d.forecast.push({ id: "clippy:ws:c", key: "kc", expected: "run", status: "pass" }); d.actual.push({ id: "clippy:ws:c", actual: "run", key: "kc", status: "pass" }); d.dispatchedRows.push("clippy:ws:c"); r.files["covB.json"].scans.push({ id: "clippy:ws:c", class: "B", coverage: "ran", status: "pass" }); r.files["covA.json"].scans.push({ id: "clippy:ws:c", class: "B", coverage: "skipped", status: "not-in-pass" }); }
const FROZEN = [
  ["GCX-01-1", "NN01", "not measured", (r) => { delete r.receipt.stages.compile; }],
  ["GCX-01-2", "NN01", "not measured", (r) => { delete r.receipt.stages.check; }],
  ["GCX-01-3", "NN01", "fail", (r) => { r.files["covA.json"].commit = "e".repeat(40); }],
  ["GCX-01-4", "NN01", "fail", (r) => { r.receipt.stages.compile.snapshot.tree = "e".repeat(40); }],
  ["GCX-02-1", "NN02", "fail", (r) => { r.ledger.push(entry(1, "C:\\Windows\\System32\\cmd.exe", ["/d", "/s", "/c", '""C:\\Program Files\\RustRight\\rustright.cmd" cargo metadata --format-version 1"'])); }],
  ["GCX-02-2", "NN02", "not measured", (r) => { wholeReuseB(r); r.ledger = []; r.end = { ...r.end, pid: 999 }; r.files["v.json"].status = "fail"; }],
  ["GCX-02-3", "NN02", "not measured", (r) => { r.ledger[2].at = at(2); }],
  ["GCX-03-1", "NN03", "not measured", (r) => { lane(r).lanes = 1.5; }],
  ["GCX-03-2", "NN03", "not measured", (r) => { lane(r).endedAt = lane(r).startedAt - 1; }],
  ["GCX-03-3", "NN03", "fail", (r) => { Object.assign(lane(r), { readyAt: 1000, startedAt: 2000, endedAt: 2100, lanes: 2 }); }],
  ["GCX-03-4", "NN03", "pass", (r) => { Object.assign(lane(r), { readyAt: 1000, startedAt: 1000, endedAt: 2100, lanes: 20 }); }],
  ["GCX-04-1", "NN04", "fail", (r) => { r.ledger[1].at = at(0.5); }],
  ["GCX-04-2", "NN04", "not measured", (r) => { decA(r).firstLookupAt = decA(r).forecastFrozenAt; }],
  ["GCX-04-3", "NN04", "fail", (r) => { decA(r).stage = "class-b"; }],
  ["GCX-05-1", "NN05", "not measured", (r) => { delete r.files["decB.json"]; }],
  ["GCX-05-2", "NN05", "fail", (r) => { r.ledger.splice(2, 1); }],
  ["GCX-05-3", "NN05", "fail", (r) => { r.ledger[2] = compile(6, "clippy", "--profile release -p b -p z --locked"); }],
  ["GCX-05-4", "NN05", "fail", (r) => { decA(r).forecast[1].expected = "bogus"; decA(r).actual[1].actual = "bogus"; scan(r, "covA.json", "deny").coverage = "bogus"; }],
  ["GCX-06-1", "NN06", "fail", (r) => { decA(r).forecast[0].priorKey = decA(r).forecast[0].key; decA(r).forecast[0].changes = { inputs: [], inputsChanged: 0 }; }],
  ["GCX-06-2", "NN06", "not measured", (r) => { delete decB(r).actual[0].key; }],
  ["GCX-06-3", "NN06", "not measured", (r) => { delete decB(r).actual[0].status; delete scan(r, "covB.json", "clippy:ws:a").status; }],
  ["GCX-07-1", "NN07", "fail", (r) => { twoChanged(r); }],
  ["GCX-07-2", "NN07", "fail", (r) => { r.ledger[1].args = ["--check", "--edition", "2021"]; }],
  ["GCX-07-3", "NN07", "pass", (r) => { twoChanged(r); r.ledger[1].args.push("src/b.rs"); }],
  ["GCX-07-4", "NN07", "not measured", (r) => { delete r.files["covB.json"]; }],
  ["GCX-07-5", "NN07", "fail", (r) => { r.ledger[2] = compile(6, "clippy", "--profile release -p ws --locked"); }],
  ["GCX-08-1", "NN08", "pass", (r) => { r.ledger = Array.of(entry(7, "/opt/Tool", ["probe"], "/src"), entry(8, "/opt/tool", ["probe"], "/src")); }],
  ["GCX-08-2", "NN08", "pass", (r) => { r.ledger = Array.of(entry(7, "/usr/bin/git", ["status"], "/src/A"), entry(8, "/usr/bin/git", ["status"], "/src/a")); }],
  ["GCX-08-3", "NN08", "fail", (r) => { r.ledger.push({ ...structuredClone(r.ledger[1]), at: at(8), pid: 101 }); r.extraLines.push(JSON.stringify({ ...r.end, pid: 101 })); }],
  ["GCX-09-1", "NN09", "fail", (r) => { split(r, "-p b -p c", "-p c"); policy(r, ["c"], ["c"]); }],
  ["GCX-09-2", "NN09", "fail", (r) => { split(r, "-p c --target x", "-p c --target y"); policy(r, ["c"], ["c"]); }],
  ["GCX-09-3", "NN09", "fail", (r) => { split(r, "-p b", "-p c"); policy(r, [], ["c"]); }],
  ["GCX-09-4", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, ["c"], []); }],
  ["GCX-09-5", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null, ["c"]); }],
  ["GCX-09-6", "NN09", "not measured", (r) => { r.ledger.push(compile(7.999, "clippy", "--profile release -p x --locked", `${SNAP}\\other`)); }],
  ["GCX-10-1", "NN10", "not measured", (r) => { r.ledger[2] = compile(6, "test", "-p b --no-run"); r.ledger.push(entry(8, `${SNAP}\\b-test.exe`, [], SNAP)); }],
  ["GCX-10-2", "NN10", "not measured", (r) => { r.ledger[2] = compile(6, "test", "-p b --no-run"); r.receipt.stages.compile.testExecutables = ["b-test.exe"]; }],
  ["GCX-11-1", "NN11", "not measured", (r) => { r.log = r.log.slice(0, 6); r.exit = null; }],
  ["GCX-11-2", "NN11", "fail", (r) => { r.limitMs = 9999; }],
  ["GCX-11-3", "NN11", "pass", (r) => { r.limitMs = 10000; }],
  ["GCX-12-1", "NN12", "not measured", (r) => { r.exit = 137; r.log = r.log.slice(0, 6); r.live = { exists: () => false, alive: () => false, pids: [100] }; }],
  ["GCX-12-2", "NN12", "fail", (r) => { r.exit = 137; r.live = { exists: () => false, alive: (p) => p === 100, pids: [100] }; }],
  ["GCX-02-4", "NN02", "fail", (r) => { r.files["v.json"].snapshotLock = "other-run"; r.files["v.json"].commit = C; r.files["v.json"].tree = T; }],
  ["GCX-05-5", "NN05", "fail", (r) => { r.ledger[1].args.push("src/unplanned.rs"); decA(r).dispatchedRows.push("secret-scan"); }],
  // fixtureWorkspaces {row: ws, invocation: other}: two workspaces of the run's tree, both with member b.
  ["GCX-07-6", "NN07", "fail", (r) => { r.ledger[2] = compile(6, "clippy", "--profile release -p b --locked", `${SNAP}\\other`); r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; r.tree["other/Cargo.toml"] = '[workspace]\nmembers = ["b"]\n'; }],
  // fixtureTreeEvidence {tree T, workspace ws, stage clippy, sidecarBound [c], batchable [b]}: the producer's own input at T, c's Tauri externalBin.
  ["GCX-09-7", "NN09", "pass", (r) => { split(r, "-p b", "-p c"); policy(r, [], []); r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "c"]\n'; r.tree["ws/c/Cargo.toml"] = '[package]\nname = "c"\n'; r.tree["ws/c/tauri.conf.json"] = JSON.stringify({ bundle: { externalBin: ["binaries/c-sidecar"] } }); }],
  // fixtureGateClaim: only the gate's printed reason; no tree evidence.
  ["GCX-09-8", "NN09", "not measured", (r) => { split(r, "-p b", "-p c"); policy(r, null, []); r.log.push(`${at(9)} rightkit-gate: c is sidecar-bound`); }],
  ["GCX-09-9", "NN09", "fail", (r) => { split(r, "-p b", "-p b"); policy(r, ["b"], ["b"]); }],
  ["gate 1", "NN06", "fail", (r) => { r.ledger.push(entry(1.7, "C:\\cargo\\bin\\cargo-deny.exe", ["check"], SNAP)); }],
  ["gate 2", "NN12", "not measured", (r) => { r.live = { exists: () => false, alive: () => true, pids: [] }; }],
  ["gate 3", "NN12", "not measured", (r) => { r.live = { exists: () => false, alive: (p) => p === 100, pids: [999] }; }],
  ["gate 4", "NN03", "fail", (r) => { const c = r.files["covA.json"]; c.scans[0].lane = { readyAt: 1000, startedAt: 1000, endedAt: 3000, lanes: 1 }; c.scans[1] = { id: "deny", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 3000, endedAt: 5000, lanes: 1 } }; const d = decA(r); d.forecast[1] = { id: "deny", key: "k2", priorKey: "k9", expected: "run", changes: { inputs: ["files.0.1"], inputsChanged: 1 } }; d.actual[1] = { id: "deny", actual: "run", key: "k2" }; d.dispatchedRows = ["rustfmt:2021", "deny"]; }],
  ["gate 5", "NN08", "fail", (r) => { const g = r.ledger[0]; r.ledger.push(entry(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", `"${g.cmd}" ${g.args.join(" ")}`], g.cwd)); }],
  ["gate 6", "NN08", "fail", (r) => { const g = r.ledger[0]; r.ledger.push(entry(0.5, g.cmd, [...g.args], `${g.cwd}\\`)); }],
  ["gate 7", "NN08", "pass", (r) => { const g = r.ledger[0]; r.ledger.push(entry(0.5, g.cmd, ["-c", "core.fsmonitor=false", ...g.args], g.cwd)); }],
  ["gate 8", "NN05", "fail", (r) => { const d = decB(r); d.actual[1].actual = "hit"; d.mismatches = [{ id: "clippy:ws:b", reason: "expected run, found a stored result" }]; d.dispatchedRows = []; scan(r, "covB.json", "clippy:ws:b").coverage = "hit"; r.ledger.splice(2, 1); r.exit = 1; r.log.push(`${at(9)} RK_CACHE_DIVERGENCE {"id":"clippy:ws:bb"}`); }],
  ["gate 9", "NN02", "pass", (r) => { r.log.push(`${at(9)} rightkit-gate: unit cargo:other stopped: class A check (compile tier) failed`); }],
  ["gate 10", "NN11", "fail", (r) => { r.limitMs = 15_000; r.ledger.unshift(entry(-30, "C:\\Program Files\\Git\\cmd\\git.exe", ["status"], "D:\\repo")); }],
  // gatecodex's ruling: manifests ws/Cargo.toml with members b, sub; ws/sub/Cargo.toml declares package c.
  ["gate 11", "NN09", "fail", (r) => { gate11(r); r.tree["ws/Cargo.toml"] = '[workspace]\nmembers = ["b", "sub"]\n'; r.tree["ws/sub/Cargo.toml"] = '[package]\nname = "c"\n'; }],
  ["gate 12", "NN07", "fail", (r) => { const d = decB(r); for (const k of ["forecast", "actual"]) d[k][1].id = "clippy:ws:b@x86_64-unknown-linux-gnu"; d.dispatchedRows = ["clippy:ws:b@x86_64-unknown-linux-gnu"]; for (const f of ["covA.json", "covB.json"]) scan(r, f, "clippy:ws:b").id = "clippy:ws:b@x86_64-unknown-linux-gnu"; r.ledger[2] = compile(6, "clippy", "--profile release --target x86_64-pc-windows-msvc -p b --locked"); }],
  ["gate 13", "NN01", "not measured", (r) => { r.log = r.log.filter((l) => !/after the compile/.test(l)); }],
  // The case's own scenario: "the Class B forecast ... frozen at 5 s" (these records freeze it at 0.9 s, so the case states it).
  ["gate 14", "NN04", "fail", (r) => { decB(r).forecastFrozenAt = at(5); r.ledger[2] = compile(3, "clippy", "--profile release -p b --locked", `${SNAP}\\ws`, 1000); }],
  ["gate 15", "NN06", "not measured", (r) => { delete decB(r).actual[0].key; }],
];
test("the frozen step-2 batch: all 63 cases, each with its author's id and expected verdict", async (t) => {
  assert.equal(FROZEN.length, 63);
  assert.equal(new Set(FROZEN.map(([id]) => id)).size, 63, "every id once");
  for (const [id, line, verdict, fn] of FROZEN) {
    await t.test(`${id} ${line}: ${verdict}`, () => { const v = run(alter(fn)); assert.equal(verdictOf(v, line), verdict, `${id} ${line}: ${whyOf(v, line)}`); });
  }
});

// Cases of mine beside the batch (not part of the 63): gate 5 quoted the way cmd /s reads it, the same without /s, and gate 11 with no manifest.
test("gate 5 and gate 11 twins: a correctly quoted cmd /c repeat, and a split whose workspace the run's tree cannot show", () => {
  expect(alter((r) => { r.ledger.push(entry(0.4, "C:\\Program Files\\Git\\cmd\\git.exe", ["status"]), entry(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/s", "/c", '""C:\\Program Files\\Git\\cmd\\git.exe" status"'])); }), "NN08", "fail");
  expect(alter((r) => { r.ledger.push(entry(0.4, "C:\\Program Files\\Git\\cmd\\git.exe", ["status"]), entry(0.5, "C:\\WINDOWS\\system32\\cmd.exe", ["/d", "/c", '"C:\\Program Files\\Git\\cmd\\git.exe" status'])); }), "NN08", "fail");
  expect(alter((r) => { gate11(r); }), "NN09", "not measured");
});

// Council CV-1's verified critical findings that the frozen batch does not exercise on its own, one case each.
test("Council CV-1: the critical findings the frozen batch leaves uncovered", () => {
  // F-1: a compile that builds outside the snapshot, by its folder or by the manifest it names.
  expect(alter((r) => { r.ledger[2] = compile(6, "clippy", "--profile release -p b --locked", "D:\\other-checkout\\ws"); }), "NN01", "fail", /outside the snapshot/);
  expect(alter((r) => { r.ledger[2] = compile(6, "clippy", "--profile release -p b --manifest-path ..\\..\\..\\elsewhere\\Cargo.toml --locked"); }), "NN01", "fail", /manifest/);
  // F-3: a process list closed by another run's gate process cannot show this run's absence of work.
  expect(alter((r) => { r.end.lock = "00000000-0000-0000-0000-000000000000"; }), "NN08", "not measured", /belongs to snapshot lock/);
  expect(alter((r) => { r.extraLines.push(JSON.stringify({ end: true, pid: 101, at: at(9), open: 0 })); r.ledger.push({ ...entry(7, "node", ["x.mjs"]), pid: 101 }); }), "NN08", "not measured", /not linked to the run's root/);
  expect(alter((r) => { r.extraLines.push(JSON.stringify({ end: true, pid: 101, at: at(9), open: 0, parent: 100 })); r.ledger.push({ ...entry(7, "node", ["x.mjs"]), pid: 101 }); }), "NN08", "pass");
  // F-5: a forecast that names another run is that run's.
  expect(alter((r) => { decB(r).snapshotLock = "other-run"; }), "NN04", "fail", /another run/);
  // F-6: a send while a Class A tool was still running is before Class A finished, whatever the verdict line says.
  expect(alter((r) => { r.ledger[1] = { ...r.ledger[1], at: at(1.6), ms: 5000 }; }), "NN02", "fail", /still running/);
  // F-9: a row looked up that the frozen forecast never named.
  expect(alter((r) => { decB(r).actual.push({ id: "clippy:ws:z", actual: "hit", key: "kz" }); }), "NN04", "fail", /clippy:ws:z/);
  // F-14: an exit code that is not a number is not an unsuccessful exit.
  const diverged = (r) => { const a = decB(r).actual.find((x) => x.id === "clippy:ws:a"); a.actual = "run"; decB(r).mismatches = [{ id: "clippy:ws:a", reason: "gone" }]; decB(r).dispatchedRows = []; scan(r, "covB.json", "clippy:ws:b").coverage = "hit"; r.ledger.splice(2, 1); r.log.push(`${at(10)} RK_CACHE_DIVERGENCE {"id":"clippy:ws:a","reason":"gone"}`); r.exit = 1; };
  expect(alter((r) => { diverged(r); r.exit = Number.NaN; }), "NN05", "not measured", /exit code/);
  expect(alter((r) => { diverged(r); r.log[r.log.length - 1] = `${at(10)} RK_CACHE_DIVERGENCE {"id":"clippy:ws:a","reason":"another cause"}`; }), "NN05", "fail", /another cause/);
  // F-15: work of any kind that starts after the stop notice.
  expect(alter((r) => { diverged(r); r.ledger.push(entry(10.5, "C:\\rust\\bin\\rustfmt.exe", ["--check", "--edition", "2021", "src/a.rs"], SNAP)); }), "NN05", "fail", /after the stop notice/);
  // F-16: a row whose own keys show it changed, forecast for reuse, must still run.
  expect(alter((r) => { Object.assign(decB(r).forecast[0], { priorKey: "k-old" }); }), "NN07", "fail", /changed .* forecast for reuse/);
  // F-24: `--release` and `--profile release` are the same settings, so these two commands are one workspace and stage split in two.
  expect(alter((r) => { r.ledger.splice(2, 1, compile(6, "clippy", "--release -p b --locked"), compile(6, "clippy", "--profile release -p a --locked")); }), "NN09", "fail", /2x cargo clippy/);
  // F-30: a killed run passes NN12 only on the record of the next ordinary run reclaiming it.
  expect(alter((r) => { r.exit = 137; r.live = { exists: () => false, alive: () => false, pids: [100], successor: { ok: true, reclaimed: true } }; }), "NN12", "pass", /next ordinary run/);
  // F-17's other side: a version query of a reused row's tool (actionlint spells it -version) is a probe, not the row's work.
  expect(alter((r) => { r.ledger.push(entry(1.7, "C:\\cargo\\bin\\cargo-deny.exe", ["--version"], SNAP), entry(1.75, "D:\\bin\\actionlint.exe", ["-version"], "D:\\t\\rk-probe-x")); }), "NN06", "pass");
  // F-33: an uncacheable row proven changed by its own record is no rerun of unchanged work.
  expect(alter((r) => { decA(r).forecast.push({ id: "shellcheck", key: "kx", priorKey: "kw", expected: "uncacheable" }); decA(r).actual.push({ id: "shellcheck", actual: "uncacheable" }); decA(r).dispatchedRows.push("shellcheck"); r.files["covA.json"].scans.push({ id: "shellcheck", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1000, endedAt: 1100, lanes: 4 } }); }), "NN06", "pass");
});

// Shapes found in the recorded runs of full run 4 (D:\scratch\loop-review\muse-fulldev-4) that the first fixed checks misread.
test("record shapes real runs write: uncacheable rows, both change-list spellings, gate-run test suites, feature-set variants", () => {
  // An uncacheable row is forecast, looked up and covered as such and runs every time: it agrees with itself, and NN06 cannot pass while it exists.
  const uncacheable = (r) => { decA(r).forecast.push({ id: "shellcheck", expected: "uncacheable" }); decA(r).actual.push({ id: "shellcheck", actual: "uncacheable" }); decA(r).dispatchedRows.push("shellcheck"); r.files["covA.json"].scans.push({ id: "shellcheck", class: "A", coverage: "ran", status: "pass", lane: { readyAt: 1000, startedAt: 1000, endedAt: 1100, lanes: 4 } }); r.ledger.splice(2, 0, entry(1.7, "C:\\tools\\shellcheck.exe", ["scripts/a.sh"], SNAP)); };
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
