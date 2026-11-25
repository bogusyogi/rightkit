import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { defaultSpawnAsync, fileScanCache, runScans } from "./deterministic-scans.mjs";
import { openClassBHistory } from "./class-a-cache-history.mjs";
import { workspaceManifestDigest } from "./feature-partition.mjs";

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

// These regressions use distinct packages. Feature partitioning reads a real,
// dependency-free Cargo workspace; executable work remains Node subprocesses.
async function independentGroupFixture(t) {
  const f = fixture(t, { grouped: true, withHistory: true });
  const names = ["core", "edited"];
  fs.writeFileSync(path.join(f.root, "Cargo.toml"), '[workspace]\nmembers = ["core", "edited"]\nresolver = "2"\n');
  fs.writeFileSync(path.join(f.root, "Cargo.lock"), 'version = 3\n\n[[package]]\nname = "core"\nversion = "0.1.0"\n\n[[package]]\nname = "edited"\nversion = "0.1.0"\n');
  for (const name of names) {
    fs.mkdirSync(path.join(f.root, name, "src"), { recursive: true });
    fs.writeFileSync(path.join(f.root, name, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n\n[features]\ndefault = ["enabled"]\nenabled = []\n`);
    fs.writeFileSync(path.join(f.root, name, "src/lib.rs"), 'pub fn value() -> u8 { 1 }\n');
  }
  const marker = path.join(f.root, "physical-argv.jsonl");
  const program = `require('node:fs').appendFileSync(${JSON.stringify(marker)}, JSON.stringify(process.argv.slice(1)) + '\\n')`;
  f.plan.scans.forEach((scan, i) => {
    const name = names[i];
    scan.id = `check:${name}`;
    scan.args = ["--eval", program, "--", "-p", name, "--message-format=short"];
    scan.command = `${process.execPath} ${scan.args.join(" ")}`;
    scan.batchable = { key: "distinct-equivalent-packages", at: 3, crate: name, unit: name };
    f.inputs.set(scan.id, fs.readFileSync(path.join(f.root, name, "src/lib.rs"), "utf8"));
  });
  const native = await defaultSpawnAsync("rustup", ["which", "cargo"], { cwd: fileURLToPath(new URL("../../", import.meta.url)), env: process.env });
  assert.equal(native.status, 0, native.stderr);
  const nativeCargo = native.stdout.trim();
  assert.ok(path.isAbsolute(nativeCargo) && fs.existsSync(nativeCargo), "rustup resolves installed native Cargo");
  const digest = f.cache.digest;
  f.cache.digest = (scan) => scan.batch ? JSON.stringify({ members: digest(scan), manifests: workspaceManifestDigest(f.root) }) : digest(scan);
  // Native Cargo asks `rustc` on PATH for target information; inside a gate run that is the managed shim, which refuses this temporary
  // workspace outside the repository and managed roots (macOS, 0.6.80). The toolchain's own rustc beside native Cargo answers instead.
  const nativeRustc = path.join(path.dirname(nativeCargo), `rustc${process.platform === "win32" ? ".exe" : ""}`);
  assert.ok(fs.existsSync(nativeRustc), "native rustc sits beside native Cargo");
  const resolutions = [];
  const spawn = async (tool, args, options) => {
    if (args[0] !== "tree") return defaultSpawnAsync(tool, args, options);
    resolutions.push(args);
    return defaultSpawnAsync(nativeCargo, [...args, "--offline"], { ...options, env: { ...(options?.env ?? process.env), RUSTC: nativeRustc } });
  };
  const run = async () => {
    const history = f.openHistory();
    const result = await f.run(history.forecast, { spawn });
    if (!result.classBCacheDecision.mismatches.length) history.record(result.classBCacheDecision);
    return result;
  };
  const argv = () => fs.existsSync(marker) ? fs.readFileSync(marker, "utf8").trim().split("\n").map(JSON.parse) : [];
  const edit = (value) => {
    const source = path.join(f.root, "edited/src/lib.rs");
    fs.writeFileSync(source, `pub fn value() -> u8 { ${value} }\n`);
    f.inputs.set("check:edited", fs.readFileSync(source, "utf8"));
  };
  return { ...f, run, argv, resolutions, edit };
}

test("editing one grouped member reuses original invocation evidence without dispatching unchanged core", async (t) => {
  const f = await independentGroupFixture(t);
  const first = await f.run();
  assert.equal(first.ok, true, JSON.stringify(first));
  assert.deepEqual(f.argv(), [["-p", "core", "-p", "edited", "--message-format=short"]]);
  assert.equal(f.resolutions.length, 3, "Cargo resolves group & both distinct members");
  const original = first.classBCacheDecision.fingerprints.find(({ id }) => id === "check:core");
  assert.match(original.invocationId, /^check:batch:/);
  assert.equal(original.coveredBy, original.invocationId);
  const soloKey = first.classBCacheDecision.memberFingerprints.find(({ id }) => id === "check:core").key;
  assert.equal(f.cache.readOutcome(soloKey).entry, null, "group result must never project into solo cache");

  const warm = await f.run();
  assert.equal(warm.ok, true, JSON.stringify(warm));
  assert.equal(f.argv().length, 1);
  f.edit(2);
  const edited = await f.run();
  assert.deepEqual(f.argv()[1], ["-p", "edited", "--message-format=short"], "physical invocation must exclude unchanged core");
  assert.equal(edited.ok, true, JSON.stringify(edited));
  assert.deepEqual(edited.classBCacheDecision.dispatchedRows, ["check:edited"]);
  const core = edited.classBCacheDecision.fingerprints.find(({ id }) => id === "check:core");
  assert.equal(core.invocationId, original.invocationId);
  assert.equal(core.coveredBy, original.invocationId);
  assert.equal(core.key, original.key);
  assert.equal(edited.scans.find(({ id }) => id === "check:core").batchedWith.id, original.invocationId);
  assert.equal(f.cache.readOutcome(soloKey).entry, null);

  const warmed = await f.run();
  assert.equal(warmed.ok, true, JSON.stringify(warmed));
  assert.deepEqual(warmed.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.argv().length, 2);
  f.edit(3);
  const editedAgain = await f.run();
  assert.equal(editedAgain.ok, true, JSON.stringify(editedAgain));
  assert.deepEqual(editedAgain.classBCacheDecision.dispatchedRows, ["check:edited"]);
  assert.deepEqual(f.argv()[2], ["-p", "edited", "--message-format=short"]);
  assert.equal(editedAgain.classBCacheDecision.fingerprints.find(({ id }) => id === "check:core").invocationId, original.invocationId);
  const warmedAgain = await f.run();
  assert.equal(warmedAgain.ok, true, JSON.stringify(warmedAgain));
  assert.deepEqual(warmedAgain.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.argv().length, 3);
  fs.unlinkSync(path.join(f.root, "cache", `${original.key}.json`));
  // Changed work is ready, but missing retained core evidence blocks all dispatch.
  f.edit(4);
  const missing = await f.run();
  assert.equal(missing.ok, false);
  assert.ok(missing.classBCacheDecision.mismatches.some(({ id, expected, actual }) => id === "check:core" && expected === "hit" && actual === "run"));
  assert.deepEqual(missing.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.argv().length, 3);
  assert.equal(f.cache.readOutcome(soloKey).entry, null);
});

test("publisher consumer transition discards ordinary group source & admits independent work", async (t) => {
  for (const binding of ["provesPublisher", "coverageKey"]) await t.test(binding, async (t) => {
    const f = await independentGroupFixture(t);
    const first = await f.run();
    assert.equal(first.ok, true, JSON.stringify(first));
    const core = f.plan.scans.find(({ id }) => id === "check:core");
    core[binding] = binding === "coverageKey" ? "publisher-core-requirement" : true;
    core.batchable.key = "publisher-bound-core";
    f.edit(2);
    const result = await f.run();
    assert.equal(result.ok, true, JSON.stringify(result));
    assert.deepEqual(result.classBCacheDecision.dispatchedRows, ["check:core", "check:edited"]);
    assert.deepEqual(f.argv().slice(1), [["-p", "core", "--message-format=short"], ["-p", "edited", "--message-format=short"]]);
    const prediction = result.classBCacheDecision.memberForecast.find(({ id }) => id === "check:core");
    assert.equal(prediction.expected, "run");
    assert.equal(prediction.source, undefined);
    assert.equal(result.classBCacheDecision.retained.find(({ id }) => id === "check:core").source, undefined);
  });
});

test("changed Cargo feature inputs replace old group eligibility; same-context missing proof refuses", async (t) => {
  const f = await independentGroupFixture(t);
  const first = await f.run();
  assert.equal(first.ok, true, JSON.stringify(first));
  const original = first.classBCacheDecision.fingerprints.find(({ id }) => id === "check:core");
  const unchanged = fs.readFileSync(path.join(f.root, "core/src/lib.rs"), "utf8");
  const manifest = path.join(f.root, "edited/Cargo.toml");
  fs.writeFileSync(manifest, fs.readFileSync(manifest, "utf8").replace('default = ["enabled"]', 'default = []'));
  const result = await f.run();
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(fs.readFileSync(path.join(f.root, "core/src/lib.rs"), "utf8"), unchanged);
  assert.deepEqual(result.classBCacheDecision.dispatchedRows, ["check:core", "check:edited"]);
  assert.deepEqual(f.argv()[1], ["-p", "core", "-p", "edited", "--message-format=short"]);
  assert.equal(f.resolutions.length, 6, "changed manifests require new real feature partition");
  assert.equal(result.classBCacheDecision.memberForecast.every(({ source, expected }) => source === undefined && expected === "run"), true);
  const replacement = result.classBCacheDecision.fingerprints.find(({ id }) => id === "check:core");
  assert.notEqual(replacement.key, original.key);
  assert.equal(f.cache.readOutcome(original.key).entry.row.status, "pass", "old evidence still exists but is ineligible");
  fs.unlinkSync(path.join(f.root, "cache", `${replacement.key}.json`));
  f.edit(2);
  const missing = await f.run();
  assert.equal(missing.ok, false);
  assert.equal(missing.classBCacheDecision.memberForecast.find(({ id }) => id === "check:core").expected, "hit");
  assert.ok(missing.classBCacheDecision.mismatches.some(({ id, expected, actual }) => id === "check:core" && expected === "hit" && actual === "run"));
  assert.deepEqual(missing.classBCacheDecision.dispatchedRows, []);
  assert.equal(f.argv().length, 2);
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
