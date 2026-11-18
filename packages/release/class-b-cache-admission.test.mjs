import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileScanCache, runScans } from "./deterministic-scans.mjs";
import { openClassBHistory } from "./class-a-cache-history.mjs";

const cold = (rows) => rows.map(({ id }) => ({ id, expected: "run" }));
const warm = (decision, rows = decision.fingerprints) => rows.map(({ id, key, failureKey }) => {
  const prior = decision.retained.find((row) => row.id === id && (row.key === key || row.key === failureKey));
  return prior ? { ...prior, expected: "hit" } : { id, expected: "run" };
});

// Node subprocesses exercise real producer/cache admission without a compiler or broker.
function fixture(t, { grouped = false, mixed = false, withHistory = false } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-class-b-admission-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, "package.json"), '{"license":"MIT"}');
  const marker = path.join(root, "dispatches.jsonl");
  const program = `require('node:fs').appendFileSync(${JSON.stringify(marker)}, 'ran\\n')`;
  const scans = ["alpha", "beta"].map((name) => {
    const args = ["--eval", program, "--", "-p", "one", "--message-format=short"];
    return { id: `check:${name}`, class: "B", root: "content", cwd: ".", tool: "node", args, command: `${process.execPath} ${args.join(" ")}`, ...(grouped ? { batchable: { key: "same-configuration", at: 3, crate: "one", unit: name } } : {}) };
  });
  if (mixed) scans.unshift({ id: "license", class: "A", command: "internal:license", internal: "license", files: ["package.json"] });
  const inputs = new Map(scans.map(({ id }) => [id, "original"]));
  const historyOptions = { historyRoot: path.join(root, "history"), storeDir: path.join(root, "cache") };
  if (withHistory) openClassBHistory(historyOptions);
  const disk = fileScanCache(historyOptions.storeDir);
  const cache = { ...disk, digest: (scan) => scan.batch ? JSON.stringify(scan.batch.members.map((id) => inputs.get(id))) : inputs.get(scan.id) };
  const plan = { roots: { contentRoot: root, liveRoot: root }, scans };
  const run = (classBCacheForecast, extra = {}) => runScans(plan, { classes: mixed ? ["A", "B"] : ["B"], classAEarlier: !mixed, classBBatching: grouped, cache, classBCacheForecast, ...extra });
  const dispatches = () => fs.existsSync(marker) ? fs.readFileSync(marker, "utf8").split("\n").filter(Boolean).length : 0;
  return { root, plan, inputs, cache, run, dispatches, openHistory: () => openClassBHistory(historyOptions) };
}

test("B reconciles cold, warm & changed logical rows before normal dispatch", async (t) => {
  const f = fixture(t);
  const first = await f.run(cold);
  assert.equal(first.ok, true);
  assert.equal(f.dispatches(), 2);
  assert.deepEqual(first.classBCacheDecision.dispatchedRows, ["check:alpha", "check:beta"]);
  const second = await f.run((rows) => warm(first.classBCacheDecision, rows));
  assert.equal(second.ok, true);
  assert.equal(f.dispatches(), 2);
  assert.deepEqual(second.classBCacheDecision.dispatchedRows, []);
  f.inputs.set("check:beta", "edited");
  const third = await f.run(() => [warm(first.classBCacheDecision)[0], { id: "check:beta", expected: "run" }]);
  assert.equal(third.ok, true);
  assert.equal(f.dispatches(), 3);
  assert.deepEqual(third.classBCacheDecision.dispatchedRows, ["check:beta"]);
});

test("B rejects swapped row identities with equal hit/run totals before dispatch", async (t) => {
  const f = fixture(t);
  await f.run(cold);
  f.inputs.set("check:beta", "edited");
  const result = await f.run(() => [{ id: "check:alpha", expected: "run" }, { id: "check:beta", expected: "hit" }]);
  assert.equal(result.ok, false);
  assert.deepEqual(result.classBCacheDecision.mismatches.filter(({ phase }) => phase !== "members").map(({ id, expected, actual }) => [id, expected, actual]), [["check:alpha", "run", "hit"], ["check:beta", "hit", "run"]]);
  assert.deepEqual(result.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.dispatches(), 2);
});

test("B refuses late deletion rather than turning an admitted hit into work", async (t) => {
  const f = fixture(t);
  const first = await f.run(cold);
  const key = first.classBCacheDecision.retained[0].key;
  const originalRead = f.cache.readOutcome;
  let removed = false;
  f.cache.readOutcome = (asked) => {
    const result = originalRead(asked);
    if (asked === key && !removed) {
      removed = true;
      fs.unlinkSync(path.join(f.root, "cache", `${key}.json`));
    }
    return result;
  };
  const result = await f.run((rows) => warm(first.classBCacheDecision, rows));
  assert.equal(result.ok, false);
  assert.match(result.classBCacheDecision.mismatches[0].reason, /changed between lookup and dispatch/);
  assert.deepEqual(result.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.dispatches(), 2);
});

test("B forecasts actual grouped invocation keys & reuses stored group outcomes", async (t) => {
  const f = fixture(t, { grouped: true });
  const first = await f.run((rows, { phase }) => {
    if (phase === "members") return cold(rows);
    assert.equal(rows.length, 2);
    assert.match(rows[0].invocationId, /^check:batch:/);
    assert.equal(rows[0].invocationId, rows[1].invocationId);
    assert.equal(rows[0].key, rows[1].key);
    assert.equal(rows[0].coveredBy, rows[0].invocationId);
    return cold(rows);
  });
  assert.equal(first.ok, true);
  assert.equal(f.dispatches(), 1);
  const second = await f.run((rows) => warm(first.classBCacheDecision, rows));
  assert.equal(second.ok, true);
  assert.equal(f.dispatches(), 1);
  assert.deepEqual(second.classBCacheDecision.dispatchedRows, []);
  assert.equal(second.scans.every(({ cached }) => cached === true), true);
});

test("mixed A+B mode completes A then blocks whole B set on forecast divergence", async (t) => {
  const f = fixture(t, { mixed: true });
  const result = await f.run((rows) => rows.map(({ id }) => ({ id, expected: "hit" })));
  assert.equal(result.ok, false);
  assert.equal(result.scans.find(({ id }) => id === "license").status, "pass");
  assert.equal(result.scans.filter(({ class: cls }) => cls === "B").every(({ code }) => code === "CLASS_B_CACHE_DISAGREEMENT"), true);
  assert.equal(f.dispatches(), 0);
});


test("a removed solo hit cannot hide behind a changed group with equal aggregate totals", async (t) => {
  const f = fixture(t, { grouped: true });
  // Seed alpha alone, then select both rows. Independent prediction keeps alpha solo-hit.
  const beta = f.plan.scans.pop();
  const seed = await f.run(cold);
  f.plan.scans.push(beta);
  const alpha = seed.classBCacheDecision.retained[0];
  fs.unlinkSync(path.join(f.root, "cache", `${alpha.key}.json`));
  const result = await f.run((rows) => rows.map(({ id }) => id === alpha.id ? { ...alpha, expected: "hit" } : { id, expected: "run" }));
  assert.equal(result.ok, false);
  assert.equal(result.classBCacheDecision.mismatches.some(({ id, expected, actual, phase }) => id === alpha.id && expected === "hit" && actual === "run" && phase === "members"), true);
  assert.equal(result.classBCacheDecision.fingerprints.find(({ id }) => id === alpha.id).invocationId, alpha.id);
  assert.deepEqual(result.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.dispatches(), 1);
});


test("grouped member & invocation forecasts both precede every result lookup", async (t) => {
  const f = fixture(t, { grouped: true });
  const reads = [];
  const read = f.cache.readOutcome;
  f.cache.readOutcome = (key) => { reads.push(key); return read(key); };
  const phases = [];
  const result = await f.run((rows, { phase }) => {
    phases.push(phase);
    assert.deepEqual(reads, []);
    return cold(rows);
  });
  assert.equal(result.ok, true);
  assert.deepEqual(phases, ["members", "invocations"]);
  assert.ok(reads.length > 0);
  assert.equal(f.dispatches(), 1);
});

test("held quiet invocation receives effective lease environment at dispatch", async (t) => {
  for (const brokerHost of [false, true]) {
    const f = fixture(t);
    f.plan.scans.splice(1);
    const scan = f.plan.scans[0];
    scan.id = "test:npm:quiet";
    f.inputs.set(scan.id, "quiet-test");
    scan.quiet = true;
    scan.kind = "test";
    const child = path.join(f.root, "quiet-child.cjs");
    const proof = path.join(f.root, "quiet-proof.json");
    fs.writeFileSync(child, `const {test}=require('node:test'); const assert=require('node:assert/strict'); const fs=require('node:fs'); test('receives held lease',()=>{ assert.equal(process.env.RUSTRIGHT_QUIET_LEASE,'held'); fs.writeFileSync(${JSON.stringify(proof)},JSON.stringify({lease:process.env.RUSTRIGHT_QUIET_LEASE})); });`);
    scan.args = ["--test", "--test-reporter=tap", child];
    scan.command = `${process.execPath} ${scan.args.join(" ")}`;
    let released = false;
    const env = { ...process.env }; delete env.NODE_TEST_CONTEXT;
    const result = await f.run(cold, { env, brokerHost, quietLane: { acquire: async () => ({ id: "held", env: { RUSTRIGHT_QUIET_LEASE: "held" } }), release: async () => { released = true; } } });
    assert.equal(result.ok, true, JSON.stringify(result.scans));
    assert.deepEqual(JSON.parse(fs.readFileSync(proof, "utf8")), { lease: "held" });
    assert.equal(result.scans[0].results.pass, 1);
    assert.equal(released, true);
    assert.deepEqual(result.classBCacheDecision.dispatchedRows, [scan.id]);
  }
});

test("real independent history & producer cache compose across cold/warm group & removal", async (t) => {
  const f = fixture(t, { grouped: true, withHistory: true });
  const run = async () => {
    const history = f.openHistory();
    const result = await f.run(history.forecast);
    if (!result.classBCacheDecision.mismatches.length) history.record(result.classBCacheDecision);
    return result;
  };
  const first = await run();
  assert.equal(first.ok, true, JSON.stringify(first.classBCacheDecision));
  assert.equal(f.dispatches(), 1);
  const second = await run();
  assert.equal(second.ok, true, JSON.stringify(second.classBCacheDecision));
  assert.deepEqual(second.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.dispatches(), 1);
  fs.unlinkSync(path.join(f.root, "cache", `${first.classBCacheDecision.retained[0].key}.json`));
  const third = await run();
  assert.equal(third.ok, false);
  assert.ok(third.classBCacheDecision.mismatches.some(({ expected, actual }) => expected === "hit" && actual === "run"));
  assert.deepEqual(third.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.dispatches(), 1);
});
