import assert from "node:assert/strict";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
import test from "node:test";
import { acquireLock, releaseLock, closureOf, holdReason, satisfiesAny, readiness, allowsetsFor, cmdLoop, cmdWatch, matchingUploadRuns, parseArgs, registryVersion, satisfies, stateKey, waves } from "./rk-pipeline.mjs";
const SHA = "f".repeat(40);
const index = (...versions) => new Map(versions.map((v) => [v, { yanked: false, cksum: "0".repeat(64) }]));

test("argv normalizes unit lists; npm is a registry of its own, anything else is refused", () => {
  assert.deepEqual(parseArgs(["dispatch", "rightkit-daemon rightkit-crash", "rightkit-daemon"]).units, ["rightkit-daemon", "rightkit-crash"]);
  assert.equal(parseArgs(["loop", "--registry", "npm"]).registry, "npm");
  assert.throws(() => parseArgs(["loop", "--registry", "pypi"]), /crates or npm/);
});

test("readiness uses all non-yanked versions, actual requirement semantics & prospective satisfaction", () => {
  assert.equal(satisfies("0.0.3", "0.0.4"), false);
  assert.equal(satisfies("^0.5.0", "0.5.4"), true);
  const crates = new Map([
    ["child", { name: "child", version: "0.5.5", publish: true, deps: [] }],
    ["daemon", { name: "daemon", version: "0.4.0", publish: true, deps: [{ name: "child", req: "^0.5.0" }] }],
    ["next", { name: "next", version: "1.0.0", publish: true, deps: [{ name: "daemon", req: "0.4.0" }] }],
  ]);
  const versions = new Map([["child", index("0.5.4", "0.6.0")], ["daemon", index("0.3.0")], ["next", index()]]);
  assert.deepEqual(waves(crates, versions).waves, [["child", "daemon"], ["next"]]);
  versions.get("child").set("0.5.5", { yanked: false });
  assert.ok(!waves(crates, versions).unpublished.has("child"), "older exact release remains published even when max version differs");
  versions.get("child").get("0.5.4").yanked = true;
  versions.get("child").get("0.5.5").yanked = true;
  assert.deepEqual(waves(crates, versions).waves, [{ stuck: ["daemon", "next"] }]);
});

test("registry failure is not a missing version", async () => {
  await assert.rejects(registryVersion("rightkit-test", "crates", async () => ({ status: 403, ok: false })), /registry read failed/);
});

test("planner refusal cannot manufacture a qualification exemption", async () => {
  let calls = 0;
  const planFn = async (req) => { calls++; assert.equal(req.allowMissingAssetsText, ""); throw new Error("rightkit-audio: name it in allow_missing_assets"); };
  const policy = { config: {}, pipeline: {}, crates: new Map([["rightkit-audio", { version: "1.0.0" }]]) };
  const result = await allowsetsFor("crates", ["rightkit-audio"], planFn, policy);
  assert.equal(calls, 1);
  assert.equal(result.errors.length, 1);
  assert.deepEqual(result.allow.allow_missing_assets, []);
  policy.pipeline = { dispatch: { exceptions: { "rightkit-audio": { version: "1.0.0", reason: "recorded scope", allow: ["allow_missing_assets"] } } } };
  const allowed = await allowsetsFor("crates", ["rightkit-audio"], async (req) => { assert.equal(req.allowMissingAssetsText, "rightkit-audio"); return { units: [] }; }, policy);
  assert.deepEqual(allowed.errors, []);
});

test("same-SHA uploads are joined by parent run & attempt, never timestamp", () => {
  const run = (id, title) => ({ id, event: "workflow_run", path: ".github/workflows/publish-direct.yml", display_title: title });
  const rows = [run(100, "publish-direct 10 1"), run(101, "publish-direct 11 1"), run(102, "publish-direct 10 2")];
  assert.deepEqual(matchingUploadRuns(rows, { runId: 10, runAttempt: 1 }).map((r) => r.id), [100]);
});

test("fast green dispatches while slow local peer is still running; failed peer is not retried", async () => {
  let state = { dispatches: {} }, releaseSlow;
  const slow = new Promise((resolve) => { releaseSlow = resolve; });
  const published = new Set(), calls = [];
  const readiness = async () => ({ unpublished: new Set(["fast", "slow"].filter((u) => !published.has(u))), waves: [["fast", "slow"].filter((u) => !published.has(u))] });
  const result = await cmdLoop({ registry: "crates", units: [], jobs: 2 }, {
    head: () => SHA, readiness, readState: () => structuredClone(state), writeState: (s) => { state = structuredClone(s); },
    preflight: async (unit) => { calls.push(unit); if (unit === "slow") return slow; return { code: 0 }; },
    dispatch: async ({ units }) => {
      assert.deepEqual(units, ["fast"]); assert.deepEqual(calls, ["fast", "slow"]);
      published.add("fast"); releaseSlow({ code: 1, last: "scans red" }); return 0;
    },
    watch: async () => 0,
    pause: () => new Promise((resolve) => setTimeout(resolve, 1)),
  });
  assert.equal(result, 1);
  assert.deepEqual(calls, ["fast", "slow"]);
  assert.equal(state.dispatches[stateKey("crates", SHA, "slow")].phase, "preflight");
});

test("resume watches existing dispatch & reports blocked work instead of false completion", async () => {
  let watched = 0;
  const d = { unit: "unit", sha: SHA, registry: "crates", status: "dispatched" };
  const result = await cmdLoop({ registry: "crates", units: [], jobs: 2 }, {
    head: () => SHA, readiness: async () => ({ unpublished: new Set(["unit"]), waves: [["unit"]] }),
    readState: () => ({ dispatches: { [stateKey("crates", SHA, "unit")]: { ...d } } }),
    watch: async () => { watched++; d.status = "failed"; return 1; },
    preflight: async () => { throw new Error("must not restart existing attempt"); },
  });
  assert.equal(watched, 1); assert.equal(result, 1);
});


test("watch stores dispatched version & preserves unrelated concurrent state; upload failure returns nonzero", async () => {
  const key = stateKey("crates", SHA, "unit");
  const dispatch = { unit: "unit", registry: "crates", sha: SHA, version: "1.0.0", runId: 1, runAttempt: 1, requestId: "req-1", dispatchedAt: new Date().toISOString(), status: "dispatched" };
  let state = { dispatches: { [key]: dispatch } };
  const deps = {
    readState: () => structuredClone(state), writeState: (s) => { state = s; },
    runState: () => ({ status: "completed", conclusion: "success", head_sha: SHA, run_attempt: 1, path: ".github/workflows/publish.yml", event: "workflow_dispatch", display_title: "publish req-1" }),
    uploadRun: () => [{ id: 20, status: "completed", conclusion: "success" }],
    registryVersion: async () => { state.dispatches.concurrent = { status: "dispatched" }; return index("1.0.0", "2.0.0"); },
  };
  assert.equal(await cmdWatch({ wait: false }, deps), 0);
  assert.equal(state.dispatches[key].published, "1.0.0");
  assert.ok(state.dispatches.concurrent);
  state = { dispatches: { [key]: { ...dispatch, status: "dispatched" } } };
  deps.uploadRun = () => [{ id: 20, status: "completed", conclusion: "failure" }];
  assert.equal(await cmdWatch({ wait: false }, deps), 1);
  assert.equal(state.dispatches[key].status, "failed");
});

test("dependent enters once its registry prerequisite is live while unrelated slow preflight remains running", async () => {
  let releaseSlow;
  const slow = new Promise((r) => { releaseSlow = r; });
  const done = new Set(), entered = [];
  const result = await cmdLoop({ registry: "crates", units: [], jobs: 2 }, {
    head: () => SHA,
    readiness: async () => ({ unpublished: new Set(["parent", "slow", "child"].filter((u) => !done.has(u))), waves: [[...(done.has("parent") ? [] : ["parent"]), "slow", ...(done.has("parent") && !done.has("child") ? ["child"] : [])]] }),
    readState: () => ({ dispatches: {} }),
    preflight: async (unit) => { entered.push(unit); return unit === "slow" ? slow : { code: 0 }; },
    dispatch: async ({ units: [unit] }) => {
      done.add(unit);
      if (unit === "child") { assert.ok(!done.has("slow")); releaseSlow({ code: 0 }); }
      return 0;
    },
    watch: async () => 0, pause: () => new Promise((r) => setTimeout(r, 1)),
  });
  assert.equal(result, 0);
  assert.deepEqual(entered, ["parent", "slow", "child"]);
});


test("readiness: an unrelated crate's registry failure blocks only that crate and its consumers; selected ready units advance", async () => {
  const crates = new Map([
    ["rightkit-a", { name: "rightkit-a", version: "0.2.0", publish: true, deps: [] }],
    ["rightkit-b", { name: "rightkit-b", version: "0.3.0", publish: true, deps: [{ name: "rightkit-a", req: "0.2.0" }] }],
    ["rightkit-z", { name: "rightkit-z", version: "9.0.0", publish: true, deps: [] }],
    ["rightkit-zc", { name: "rightkit-zc", version: "1.0.0", publish: true, deps: [{ name: "rightkit-z", req: "9.0.0" }] }],
  ]);
  const lookup = async (name) => { if (name === "rightkit-z") throw new Error("registry 503"); return new Map(name === "rightkit-a" ? [["0.2.0", { yanked: false }]] : []); };
  const r = await readiness({ registry: "crates", units: [] }, { crates, lookup });
  assert.deepEqual([...r.failures.keys()], ["rightkit-z"]);
  assert.ok(r.waves[0].includes("rightkit-b"), "b is ready: a is on the registry");
  assert.ok(!r.waves.flat().includes("rightkit-z"), "z is never ready while its lookup failed");
  const stuck = r.waves.find((w) => !Array.isArray(w));
  assert.ok(stuck && stuck.stuck.includes("rightkit-z") && stuck.stuck.includes("rightkit-zc"), "z and its consumer wait; nothing else does");
  const scoped = await readiness({ registry: "crates", units: ["rightkit-b"] }, { crates, lookup });
  assert.deepEqual([...scoped.crates.keys()].sort(), ["rightkit-a", "rightkit-b"], "only the requested closure is looked up");
  assert.equal(scoped.failures.size, 0);
  assert.deepEqual([...closureOf(crates, ["rightkit-zc"])].sort(), ["rightkit-z", "rightkit-zc"]);
});

test("coordinator lock: a dead owner's lock is reclaimed, a live owner excludes, release only removes our own", () => {
  const fs = require("node:fs"), os = require("node:os"), path = require("node:path");
  const lock = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-")), "state.json.lock");
  acquireLock(lock, { pid: 111, alive: () => true });
  assert.throws(() => acquireLock(lock, { pid: 222, alive: () => true }), /another coordinator \(pid 111\)/);
  releaseLock(lock, { pid: 222 });
  assert.ok(fs.existsSync(lock), "a foreign pid cannot release the lock");
  acquireLock(lock, { pid: 333, alive: () => false });
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, 333, "a stale lock is reclaimed by the next coordinator");
  releaseLock(lock, { pid: 333 });
  assert.ok(!fs.existsSync(lock));
});


test("loop: a readiness exception drains the live preflight worker and the dispatch tail before it propagates; nothing dispatches afterwards", async () => {
  let release;
  const held = new Promise((r) => { release = r; });
  const dispatched = [];
  let readinessCalls = 0;
  const unitState = { dispatches: {} };
  const deps = {
    head: () => "a".repeat(40),
    readiness: async () => { readinessCalls += 1; if (readinessCalls === 2) throw new Error("registry exploded"); return { unpublished: new Set(["rightkit-x"]), waves: [["rightkit-x"]], failures: new Map() }; },
    readState: () => unitState, writeState: (st) => Object.assign(unitState, st),
    watch: async () => 0,
    preflight: async () => { await held; return { code: 0, last: "GREEN" }; },
    dispatch: async (o) => { dispatched.push(o.units[0]); return 0; },
    pause: async () => {},
  };
  const loop = cmdLoop({ registry: "crates", units: ["rightkit-x"], jobs: 2, wait: false }, deps);
  let settled = false;
  loop.then(() => { settled = true; }, () => { settled = true; });
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(settled, false, "the loop must not finish while a worker is held, even after readiness threw");
  release();
  await assert.rejects(loop, /registry exploded/);
  assert.deepEqual(dispatched, ["rightkit-x"], "the held worker completed its dispatch before the loop ended, not after");
  const after = dispatched.length;
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(dispatched.length, after, "nothing dispatches after the loop has ended");
});


test("a unit that failed hosted is held at a new SHA until its closure, recipe or own policy changed, or a reviewed release names the run", () => {
  const crates = new Map([["rightkit-a", { name: "rightkit-a", version: "1.0.0", dir: "crates/rightkit-a", publish: true, deps: [{ name: "rightkit-b" }] }], ["rightkit-b", { name: "rightkit-b", version: "2.0.0", dir: "crates/rightkit-b", publish: true, deps: [] }]]);
  const state = { dispatches: { "crates:old:rightkit-a": { unit: "rightkit-a", sha: "old", status: "failed", runId: 77, dispatchedAt: "2026-10-23T00:00:00Z" } } };
  const same = { "ci/gate.config.json": '{"rust":{"rightkit-a":{"allowIgnored":"x"}}}', "ci/rk-pipeline.json": '{"dispatch":{"exceptions":{}}}' };
  const show = (sha, file) => same[file];
  const unchanged = () => false;
  const paths = [];
  const held = holdReason("rightkit-a", "new", state, crates, { pipeline: {}, changed: (from, to, p) => { paths.push(...p); return false; }, show });
  assert.match(held, /held: unchanged since hosted failure at old \(run 77\)/);
  assert.deepEqual(paths, ["crates/rightkit-a", "crates/rightkit-b", "ci/consumers/rightkit-a", "Cargo.lock"], "the closure, the recipe and the lock are the change set");
  assert.equal(holdReason("rightkit-a", "new", state, crates, { pipeline: {}, changed: () => true, show }), null, "a change in the closure releases");
  const policyChanged = (sha, file) => (sha === "new" && file === "ci/gate.config.json") ? '{"rust":{"rightkit-a":{"allowIgnored":"y"}}}' : same[file];
  assert.equal(holdReason("rightkit-a", "new", state, crates, { pipeline: {}, changed: unchanged, show: policyChanged }), null, "a change to the unit's own policy entry releases");
  const otherUnit = (sha, file) => (sha === "new" && file === "ci/gate.config.json") ? '{"rust":{"rightkit-a":{"allowIgnored":"x"},"rightkit-z":{"allowIgnored":"q"}}}' : same[file];
  assert.match(holdReason("rightkit-a", "new", state, crates, { pipeline: {}, changed: unchanged, show: otherUnit }), /held/, "another unit's policy change does not release");
  const release = { dispatch: { releaseHolds: { "rightkit-a": { run: 77, version: "1.0.0", reason: "runner image fixed" } } } };
  assert.equal(holdReason("rightkit-a", "new", state, crates, { pipeline: release, changed: unchanged, show }), null, "a reviewed release naming the failed run and version releases");
  const stale = { dispatch: { releaseHolds: { "rightkit-a": { run: 70, version: "1.0.0", reason: "older run" } } } };
  assert.match(holdReason("rightkit-a", "new", state, crates, { pipeline: stale, changed: unchanged, show }), /held/, "a release naming another run does not");
  assert.equal(holdReason("rightkit-b", "new", state, crates, { pipeline: {}, changed: unchanged, show }), null, "a unit without a hosted failure is never held");
  assert.equal(holdReason("rightkit-a", "old", state, crates, { pipeline: {}, changed: unchanged, show }), null, "the failed SHA itself is governed by the attempt record, not the hold");
});


test("a unit whose record at this SHA is a preflight-phase failure is attempted again; any other record ends its attempts", async () => {
  const key = (u) => stateKey("crates", SHA, u);
  let state = { dispatches: { [key("retry")]: { unit: "retry", sha: SHA, status: "failed", phase: "preflight", detail: "tree changed" }, [key("done")]: { unit: "done", sha: SHA, status: "failed", runId: 5 } } };
  const calls = [];
  const readiness = async () => ({ unpublished: new Set(["retry", "done"]), waves: [["retry", "done"]], crates: new Map() });
  const result = await cmdLoop({ registry: "crates", units: [], jobs: 2 }, {
    head: () => SHA, readiness, readState: () => structuredClone(state), writeState: (s) => { state = structuredClone(s); },
    preflight: async (unit) => { calls.push(unit); return { code: 1, last: "still red" }; },
    dispatch: async () => { throw new Error("nothing green to dispatch"); },
    watch: async () => 0, holdReason: () => null,
    pause: () => new Promise((resolve) => setTimeout(resolve, 1)),
  });
  assert.equal(result, 1);
  assert.deepEqual(calls, ["retry"], "only the preflight-phase failure is re-attempted");
  assert.equal(state.dispatches[key("done")].runId, 5, "a hosted record is never overwritten by the loop");
});


test("an npm `a || b` requirement is satisfied by either alternative, and by neither otherwise", () => {
  assert.equal(satisfiesAny("^0.4.1 || ^0.5.0", "0.5.0"), true);
  assert.equal(satisfiesAny("^0.4.1 || ^0.5.0", "0.4.2"), true);
  assert.equal(satisfiesAny("^0.4.1 || ^0.5.0", "0.6.0"), false);
  assert.equal(satisfiesAny("=1.2.3", "1.2.3"), true);
});
