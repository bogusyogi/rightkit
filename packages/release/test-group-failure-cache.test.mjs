// NN41 through the real runScans, the way the Dell runs it (astra62 review, 2026-11-18): Rust test rows compile with --no-run and run their executables
// outside the compile (runOutside, GC-A08), and two of them form one batched group (classBBatching). A group whose COMPILE fails with compiler
// diagnostics is stored under the group's failure key, so the same commit run again compiles nothing and both members report the cached failure.
// A group that compiled and whose test failed is never stored: it runs again.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileScanCache, runScans } from "./deterministic-scans.mjs";
import { openClassBHistory } from "./class-a-cache-history.mjs";

const libtestFail = "running 1 test\ntest t ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
const compileError = "error: gate-acceptance NN41: a deterministic compile failure\n --> rk/rk-leaf/src/lib.rs:9:1\nerror: could not compile `rk-leaf` (lib) due to 1 previous error\n";

function groupRig({ compiles, withHistory = false }) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-nn41-grp-")); const target = path.join(root, "target"); fs.mkdirSync(path.join(target, "release"), { recursive: true });
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\nmembers = []\n");
  const exes = {}; for (const c of ["a", "b"]) { const l = path.join(target, "release", `${c}-1`); fs.writeFileSync(l, "#!/bin/sh\n", { mode: 0o755 }); exes[c] = { link: l, real: fs.realpathSync.native(l) }; }
  const calls = [];
  const spawn = async (cmd, args) => {
    calls.push({ cmd, args: [...args] });
    if (Object.values(exes).some((e) => e.real === cmd)) return { status: 101, stdout: libtestFail, stderr: "" };
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    if (args.includes("tree")) return { status: 0, stdout: "", stderr: "" };
    const crates = args.flatMap((a, i) => (a === "-p" ? [args[i + 1]] : []));
    if (!compiles) return { status: 101, stdout: `${JSON.stringify({ reason: "build-finished", success: false })}\n`, stderr: compileError };
    const lines = crates.map((c) => ({ reason: "compiler-artifact", package_id: `path+file:///x/${c}#${c}@0.1.0`, manifest_path: path.join(root, "Cargo.toml"), target: { name: c, kind: ["lib"] }, profile: { test: true }, executable: exes[c].link }));
    lines.push({ reason: "build-finished", success: true });
    return { status: 0, stdout: lines.map((l) => JSON.stringify(l)).join("\n") + "\n", stderr: "" };
  };
  const plain = (crate) => ({ class: "B", tool: "cargo", env: { CARGO_TARGET_DIR: target }, id: `test:rust:${crate}`, args: ["test", "-p", crate, "--locked", "--message-format=json"], command: `cargo test -p ${crate} --locked --message-format=json`, runOutside: { args: [] }, batchable: { key: "k", at: 1, crate, unit: crate } });
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: root, liveRoot: root }, scans: [plain("a"), plain("b")], notes: [], deferred: [], qualifyOwed: [] };
  const store = new Map();
  const digest = (scan) => `d-${scan.id}`;
  // Real-history variant: the same rig behind the Nov-18 admission (history forecast before lookup, record only on a mismatch-free decision).
  const historyOptions = withHistory ? { historyRoot: path.join(root, "history"), storeDir: path.join(root, "cache") } : null;
  if (historyOptions) openClassBHistory(historyOptions);
  const cache = historyOptions ? { ...fileScanCache(historyOptions.storeDir), digest } : { read: (k) => store.get(k) ?? null, write: (k, row) => { store.set(k, { key: k, at: new Date().toISOString(), row }); }, digest };
  const runPlain = (failureIdentity = "broker-1") => runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, cache, classBBatching: true, failureIdentity });
  const runAdmitted = async (failureIdentity = "broker-1") => {
    const history = openClassBHistory(historyOptions);
    const result = await runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, cache, classBBatching: true, failureIdentity, classBCacheForecast: history.forecast });
    if (!result.classBCacheDecision.mismatches.length) history.record(result.classBCacheDecision);
    return result;
  };
  const run = historyOptions ? runAdmitted : runPlain;
  const compileCalls = () => calls.filter((c) => c.cmd === "cargo" && c.args[0] === "test");
  return { run, calls, compileCalls, store, storeDir: historyOptions?.storeDir };
}
const byId = (result, id) => result.scans.find((r) => r.id === id);

test("NN41: a batched test group whose compile failed is a cached failure on the next run with the same inputs; nothing compiles", async () => {
  const { run, calls, compileCalls, store } = groupRig({ compiles: false });
  const first = await run();
  for (const id of ["test:rust:a", "test:rust:b"]) { const r = byId(first, id); assert.equal(r.status, "fail", JSON.stringify(r)); assert.ok(r.batchedWith, "ran as one group"); }
  assert.equal(compileCalls().length, 1, "one group compile");
  assert.ok([...store.values()].some((v) => v.row.status === "fail" && /^test:batch:/.test(v.row.id)), "the group's failure is stored under the group");
  calls.length = 0;
  const second = await run();
  for (const id of ["test:rust:a", "test:rust:b"]) { const r = byId(second, id); assert.equal(r.status, "fail"); assert.equal(r.cached, true, JSON.stringify(r)); }
  assert.equal(compileCalls().length, 0, "nothing compiles the second time");
  // Another broker runtime: the stored failure is not reused (an environment fix must rerun it).
  calls.length = 0;
  const third = await run("broker-2");
  assert.equal(byId(third, "test:rust:a").cached, undefined); assert.equal(compileCalls().length, 1);
});

test("NN41 under Nov-18 admission: grouped compile failure is forecast hit/fail from real Class B history; a deleted failure entry blocks before dispatch", async () => {
  const { run, calls, compileCalls, storeDir } = groupRig({ compiles: false, withHistory: true });
  const first = await run();
  const d1 = first.classBCacheDecision;
  assert.deepEqual(d1.mismatches, [], JSON.stringify(d1.mismatches));
  assert.equal(compileCalls().length, 1, "one physical group compile");
  assert.deepEqual([...d1.dispatchedRows].sort(), ["test:rust:a", "test:rust:b"]);
  const failureKeys = ["test:rust:a", "test:rust:b"].map((id) => d1.fingerprints.find((f) => f.id === id).failureKey);
  assert.ok(failureKeys[0] && failureKeys[0] === failureKeys[1], `one batch failure key: ${failureKeys}`);
  for (const id of ["test:rust:a", "test:rust:b"]) {
    const retained = d1.retained.find((r) => r.id === id);
    assert.equal(retained.status, "fail", id); assert.equal(retained.key, failureKeys[0], `${id} retained under the batch failure key`);
  }
  calls.length = 0;
  const second = await run();
  const d2 = second.classBCacheDecision;
  assert.deepEqual(d2.mismatches, [], JSON.stringify(d2.mismatches));
  assert.deepEqual(d2.dispatchedRows, []);
  assert.equal(compileCalls().length, 0, "nothing compiles the second time");
  for (const id of ["test:rust:a", "test:rust:b"]) {
    // Solo member identities are never stored (no solo projection); the logical invocation rows carry the batch failure key history knows.
    assert.equal(d2.memberForecast.find((f) => f.id === id).expected, "run", `${id} solo member forecast`);
    const predicted = d2.forecast.find((f) => f.id === id);
    assert.equal(predicted.expected, "hit", `${id} logical forecast`); assert.equal(predicted.status, "fail", `${id} logical forecast status`); assert.equal(predicted.key, failureKeys[0], `${id} forecast key`);
    const r = byId(second, id); assert.equal(r.status, "fail"); assert.equal(r.cached, true, JSON.stringify(r));
  }
  // Entry loss after a hit/fail forecast: admission names the divergence and dispatches nothing (Nov-18 stop).
  fs.unlinkSync(path.join(storeDir, `${failureKeys[0]}.json`));
  const third = await run();
  assert.equal(third.ok, false);
  assert.ok(third.classBCacheDecision.mismatches.some(({ expected, actual }) => expected === "hit" && actual === "run"), JSON.stringify(third.classBCacheDecision.mismatches));
  assert.deepEqual(third.classBCacheDecision.dispatchedRows, []);
  assert.equal(compileCalls().length, 0, "blocked before dispatch");
});

test("NN41: a batched test group that compiled and had a failing test is never stored: it runs again", async () => {
  const { run, calls, compileCalls, store } = groupRig({ compiles: true });
  const first = await run();
  assert.equal(byId(first, "test:rust:a").status, "fail");
  assert.equal([...store.values()].filter((v) => v.row.status === "fail").length, 0, "no failure stored");
  calls.length = 0;
  const second = await run();
  assert.equal(byId(second, "test:rust:a").cached, undefined); assert.equal(compileCalls().length, 1, "it compiles and runs again");
});
