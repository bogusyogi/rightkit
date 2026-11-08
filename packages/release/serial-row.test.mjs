// rightkit#1539 through the real runScans: the serial test row owns no compile while its plain row is in the run. Plain compiled and passed: its executables run again with
// --test-threads=1, one compile in all; plain failed: the serial row fails on that dependency and never compiles; plain was a cache hit: the serial row reuses the stored executables when every digest still matches, else compiles once, alone.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runScans } from "./deterministic-scans.mjs";

const libtestOk = "running 1 test\ntest t ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
const libtestFail = "running 1 test\ntest t ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";

function rig({ compileStatus = 0, plainTestsFail = false } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-serial-")); const target = path.join(root, "target"); fs.mkdirSync(path.join(target, "release"), { recursive: true });
  const exeLink = path.join(target, "release", "a-1"); fs.writeFileSync(exeLink, "#!/bin/sh\n", { mode: 0o755 });
  const exe = fs.realpathSync.native(exeLink); // the gate runs the resolved path (macOS /var is /private/var)
  const calls = [];
  const spawn = async (cmd, args, opts = {}) => {
    calls.push({ cmd, args: [...args], env: opts.env ?? {} });
    if (cmd === exe) return { status: plainTestsFail && !args.includes("--test-threads=1") ? 101 : 0, stdout: plainTestsFail && !args.includes("--test-threads=1") ? libtestFail : libtestOk, stderr: "" };
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    // the compile: cargo's JSON stream naming the one test executable
    const lines = [{ reason: "compiler-artifact", package_id: "path+file:///x/a#a@0.1.0", manifest_path: path.join(root, "Cargo.toml"), target: { name: "a", kind: ["lib"] }, profile: { test: true }, executable: exeLink }, { reason: "build-finished", success: compileStatus === 0 }];
    return { status: compileStatus, stdout: lines.map((l) => JSON.stringify(l)).join("\n") + "\n", stderr: compileStatus ? "error: could not compile\n" : "" };
  };
  const mk = (id, runArgs) => ({ id, class: "B", tool: "rustright", args: ["cargo", "test", "-p", "a", "--locked", "--message-format=json"], command: "rustright cargo test -p a --locked --message-format=json", cwd: undefined, env: { CARGO_TARGET_DIR: target }, runOutside: { args: runArgs }, ...(id.includes("serial") ? { reuseExecutables: "test:rust:a" } : {}) });
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: root, liveRoot: root }, scans: [mk("test:rust:a", []), mk("test:rust-serial:a", ["--test-threads=1"])], notes: [], deferred: [], qualifyOwed: [] };
  const run = (extra = {}) => runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, ...extra });
  return { run, calls, exe, root };
}
const compiles = (calls, exe) => calls.filter((c) => c.cmd !== exe && !c.args.includes("--version"));
const testRuns = (calls, exe) => calls.filter((c) => c.cmd === exe);
const byId = (result, id) => result.scans.find((r) => r.id === id);

test("plain compiled and passed: the serial row compiles nothing and runs the same executable with --test-threads=1", async () => {
  const { run, calls, exe } = rig();
  const result = await run();
  assert.equal(byId(result, "test:rust:a")?.status, "pass", JSON.stringify(byId(result, "test:rust:a")));
  assert.equal(byId(result, "test:rust-serial:a")?.status, "pass", JSON.stringify(byId(result, "test:rust-serial:a")));
  assert.equal(compiles(calls, exe).length, 1, "one compile for the crate and configuration");
  const runs = testRuns(calls, exe);
  assert.equal(runs.length, 2); assert.ok(runs.some((r) => r.args.includes("--test-threads=1")), "the serial run is single-threaded"); assert.ok(runs.some((r) => !r.args.includes("--test-threads=1")));
  assert.equal(byId(result, "test:rust-serial:a").reusedExecutablesOf, "test:rust:a");
});

test("plain failed: the serial row fails on that dependency and never compiles", async () => {
  const { run, calls, exe } = rig({ plainTestsFail: true });
  const result = await run();
  assert.equal(byId(result, "test:rust:a").status, "fail");
  const serial = byId(result, "test:rust-serial:a");
  assert.equal(serial.status, "fail"); assert.equal(serial.code, "TEST_DEPENDENCY_FAILED");
  assert.equal(compiles(calls, exe).length, 1, "the failed plain row's compile is the only one");
  assert.equal(testRuns(calls, exe).filter((r) => r.args.includes("--test-threads=1")).length, 0, "nothing runs serially on a failed plain row");
});

test("plain compile failed: the serial row fails on that dependency and never compiles a second time", async () => {
  const { run, calls, exe } = rig({ compileStatus: 101 });
  const result = await run();
  assert.equal(byId(result, "test:rust:a").status, "fail");
  assert.equal(byId(result, "test:rust-serial:a").status, "fail"); assert.equal(byId(result, "test:rust-serial:a").code, "TEST_DEPENDENCY_FAILED");
  assert.equal(compiles(calls, exe).length, 1);
});

test("plain was a cache hit and its executables are unchanged: the serial row re-checks the stored proof and compiles nothing", async () => {
  const { run, calls, exe } = rig();
  const store = new Map();
  const cache = { read: (k) => store.get(k) ?? null, write: (k, row) => { store.set(k, { key: k, at: new Date().toISOString(), row }); }, digest: (scan) => `d-${scan.id}` };
  // first run stores both passes; then only the serial row's entry is dropped, so the plain row hits and the serial row misses
  const first = await run({ cache });
  assert.equal(byId(first, "test:rust:a").status, "pass"); assert.equal(byId(first, "test:rust-serial:a").status, "pass");
  const stored = [...store.values()].find((v) => v.row.id === "test:rust:a" && v.row.status === "pass")?.row;
  assert.ok(stored?.executableProof?.binaries?.length === 1, "the plain pass stores the digest-bound proof"); assert.equal(stored.acceptedBinaries, undefined, "never the run-time objects");
  for (const [k, v] of [...store]) if (v.row.id === "test:rust-serial:a") store.delete(k);
  calls.length = 0;
  const second = await run({ cache });
  assert.equal(byId(second, "test:rust:a").cached, true, "the plain row is a hit");
  const serial = byId(second, "test:rust-serial:a");
  assert.equal(serial.status, "pass", JSON.stringify(serial)); assert.notEqual(serial.cached, true); assert.equal(serial.reusedCachedProof, true);
  assert.equal(compiles(calls, exe).length, 0, "no compile: the proven executable runs again");
  assert.equal(testRuns(calls, exe).length, 1); assert.ok(testRuns(calls, exe)[0].args.includes("--test-threads=1"));
});

test("plain was a cache hit but its executable changed: the serial row compiles once, on its own", async () => {
  const { run, calls, exe } = rig();
  const store = new Map();
  const cache = { read: (k) => store.get(k) ?? null, write: (k, row) => { store.set(k, { key: k, at: new Date().toISOString(), row }); }, digest: (scan) => `d-${scan.id}` };
  await run({ cache });
  for (const [k, v] of [...store]) if (v.row.id === "test:rust-serial:a") store.delete(k);
  fs.writeFileSync(exe, "#!/bin/sh\n# rebuilt\n", { mode: 0o755 }); // the proof's digest no longer matches
  calls.length = 0;
  const second = await run({ cache });
  const serial = byId(second, "test:rust-serial:a");
  assert.equal(serial.status, "pass", JSON.stringify(serial)); assert.notEqual(serial.reusedCachedProof, true); assert.match(serial.reuseFallback ?? "", /changed since its plain row passed/);
  assert.equal(compiles(calls, exe).length, 1, "one compile, by the serial row alone");
  assert.equal(testRuns(calls, exe).length, 1); assert.ok(testRuns(calls, exe)[0].args.includes("--test-threads=1"));
});

// A test group of crates a and b compiled as one invocation (classBBatching), with a's serial row. `cargo tree` answers alike for every set, so the group stays whole.
function groupRig({ executables = true, e2e = false } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-serial-grp-")); const target = path.join(root, "target"); fs.mkdirSync(path.join(target, "release"), { recursive: true });
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\nmembers = []\n");
  const exes = {}; for (const c of ["a", "b"]) { const l = path.join(target, "release", `${c}-1`); fs.writeFileSync(l, "#!/bin/sh\n", { mode: 0o755 }); exes[c] = { link: l, real: fs.realpathSync.native(l) }; }
  const calls = [];
  const spawn = async (cmd, args) => {
    calls.push({ cmd, args: [...args] });
    if (Object.values(exes).some((e) => e.real === cmd)) return { status: 0, stdout: libtestOk, stderr: "" };
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    if (args.includes("tree")) return { status: 0, stdout: "", stderr: "" };
    const crates = args.flatMap((a, i) => (a === "-p" ? [args[i + 1]] : []));
    const lines = crates.map((c) => ({ reason: "compiler-artifact", package_id: `path+file:///x/${c}#${c}@0.1.0`, manifest_path: path.join(root, "Cargo.toml"), target: { name: c, kind: ["lib"] }, profile: { test: true }, ...(executables ? { executable: exes[c].link } : {}) }));
    lines.push({ reason: "build-finished", success: true });
    return { status: 0, stdout: lines.map((l) => JSON.stringify(l)).join("\n") + "\n", stderr: "" };
  };
  const base = (crate) => ({ class: "B", tool: "cargo", env: { CARGO_TARGET_DIR: target } });
  const plain = (crate) => ({ ...base(crate), id: `test:rust:${crate}`, args: ["test", "-p", crate, "--locked", "--message-format=json"], command: `cargo test -p ${crate} --locked --message-format=json`, runOutside: { args: [] }, batchable: { key: "k", at: 1, crate, unit: crate } });
  const serial = e2e
    ? { ...base("a"), id: "test:rust-serial:a", e2e: true, reuseExecutables: "test:rust:a", args: ["test", "-p", "a", "--locked", "--no-run"], command: "cargo test -p a --locked --no-run" }
    : { ...base("a"), id: "test:rust-serial:a", args: ["test", "-p", "a", "--locked", "--message-format=json"], command: "cargo test -p a --locked --message-format=json", runOutside: { args: ["--test-threads=1"] }, reuseExecutables: "test:rust:a" };
  const e2ePlain = { ...base("a"), id: "test:rust:a", e2e: true, args: ["test", "-p", "a", "--locked", "--no-run"], command: "cargo test -p a --locked --no-run" };
  const scans = e2e ? [e2ePlain, serial] : [plain("a"), plain("b"), serial];
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: root, liveRoot: root }, scans, notes: [], deferred: [], qualifyOwed: [] };
  const store = new Map();
  const cache = { read: (k) => store.get(k) ?? null, write: (k, row) => { store.set(k, { key: k, at: new Date().toISOString(), row }); }, digest: (scan) => `d-${scan.id}` };
  const run = () => runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, cache, classBBatching: true });
  const dropSerial = () => { for (const [k, v] of [...store]) if (v.row.id === "test:rust-serial:a") store.delete(k); };
  const compileCalls = () => calls.filter((c) => c.cmd === "cargo" && c.args[0] === "test");
  return { run, calls, dropSerial, compileCalls, exes };
}

test("D3 regression: the plain row is a member of a cached test group: the serial row re-checks the group's stored proof and compiles nothing", async () => {
  const { run, calls, dropSerial, compileCalls, exes } = groupRig();
  const first = await run();
  assert.ok(byId(first, "test:rust:a").batchedWith, "a ran in a group"); assert.equal(byId(first, "test:rust-serial:a").status, "pass");
  dropSerial(); calls.length = 0;
  const second = await run();
  assert.equal(byId(second, "test:rust:a").cached, true, "the group member says it is a cache hit");
  const serial = byId(second, "test:rust-serial:a");
  assert.equal(serial.status, "pass", JSON.stringify(serial)); assert.equal(serial.reusedCachedProof, true);
  assert.equal(compileCalls().length, 0, "no compile");
  assert.deepEqual(calls.filter((c) => c.cmd === exes.a.real).map((c) => c.args.includes("--test-threads=1")), [true]);
});

test("a test group with no executable: every member passes with nothing to run, and the serial member passes without compiling", async () => {
  const { run, compileCalls } = groupRig({ executables: false });
  const r = await run();
  for (const id of ["test:rust:a", "test:rust:b", "test:rust-serial:a"]) assert.equal(byId(r, id).status, "pass", JSON.stringify(byId(r, id)));
  assert.equal(compileCalls().length, 1, "the group's one compile");
});

test("end-to-end crate: the serial row is the plain row's --no-run compile, so a cache hit of the plain row passes it without compiling", async () => {
  const { run, calls, dropSerial, compileCalls } = groupRig({ e2e: true });
  const first = await run();
  assert.equal(byId(first, "test:rust-serial:a").status, "pass"); assert.equal(compileCalls().length, 1, "one compile in the cold run");
  dropSerial(); calls.length = 0;
  const second = await run();
  assert.equal(byId(second, "test:rust:a").cached, true);
  const serial = byId(second, "test:rust-serial:a");
  assert.equal(serial.status, "pass", JSON.stringify(serial)); assert.equal(serial.reusedExecutablesOf, "test:rust:a");
  assert.equal(compileCalls().length, 0, "no compile on the warm run");
});
