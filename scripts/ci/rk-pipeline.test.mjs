import assert from "node:assert/strict";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
import test from "node:test";
import { packageRows } from "./rk-npm-preflight.mjs";
import { openClaim, needsReconcile, acquireLock, releaseLock, closureOf, dispatchRefusal, holdReason, satisfiesAny, readiness, allowsetsFor, cmdCanary, cmdDispatch, cmdLoop, cmdPreflight, cmdWatch, matchingUploadRuns, uploadRunsQuery, parseArgs, readQualification, registryVersion, satisfies, stateKey, waves, hostedRunsInFlight } from "./rk-pipeline.mjs";
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
    check: async () => ({ code: 0, out: "" }), head: () => SHA, readiness, readState: () => structuredClone(state), writeState: (s) => { state = structuredClone(s); },
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
    check: async () => ({ code: 0, out: "" }), head: () => SHA, readiness: async () => ({ unpublished: new Set(["unit"]), waves: [["unit"]] }),
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
    check: async () => ({ code: 0, out: "" }), head: () => SHA,
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
    check: async () => ({ code: 0, out: "" }), head: () => "a".repeat(40),
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
  assert.deepEqual(paths, ["crates/rightkit-a", "crates/rightkit-b", "ci/consumers/rightkit-a", "ci/consumer-recipes.json"], "the closure (each crate dir holds its own Cargo.toml and Cargo.lock), the recipe and the inventory are the change set");
  const npmCrates = new Map([["@rightkit/x", { name: "@rightkit/x", version: "1.0.0", dir: "packages/x", publish: true, deps: [] }]]);
  const npmState = { dispatches: { "npm:old:@rightkit/x": { unit: "@rightkit/x", sha: "old", status: "failed", runId: 9, dispatchedAt: "2026-10-23T00:00:00Z" } } };
  const npmPaths = [];
  holdReason("@rightkit/x", "new", npmState, npmCrates, { pipeline: {}, changed: (f, t, p) => { npmPaths.push(...p); return false; }, show });
  assert.deepEqual(npmPaths, ["packages/x", "ci/consumers/npm/x", "ci/consumer-recipes.json"], "an npm unit's recipe lives under ci/consumers/npm/<short name>");
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
    check: async () => ({ code: 0, out: "" }), head: () => SHA, readiness, readState: () => structuredClone(state), writeState: (s) => { state = structuredClone(s); },
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


test("an npm receipt is admitted through the real receiptRefusal with the real packageRows plan and the package directory the pinned dispatcher derives; an unknown package or a moved directory refuses", () => {
  const sha = "f".repeat(40);
  const pkg = { name: "@rightkit/legal", dir: "packages/legal", version: "0.6.2", scripts: { test: "vitest run" } };
  const packages = new Map([[pkg.name, pkg]]);
  const pipeline = { tools: { pnpm: { [process.platform]: { storeDir: "/Volumes/D/.pnpm-store/v11" } } } };
  const rows = packageRows(pkg.name, pkg, { storeDir: pipeline.tools.pnpm[process.platform].storeDir });
  const request = "11111111-1111-4111-8111-111111111111";
  const row = (r) => ({ id: r.id, kind: r.kind, cwd: ".", command: `rustright run rightkit-pnpm -- ${r.args.join(" ")}`, exit: 0, timedOut: false, seconds: 1, ok: true, brokerRequests: [request], attestation: { verified: true, signatureVerified: true, requestId: request, nonce: "a".repeat(64) } });
  const receipt = { schema: "rk-preflight.v2", mode: "local-test", sha, registry: "npm", unit: pkg.name, version: pkg.version, dir: pkg.dir, adapter: "rightkit-pnpm", route: "local", ok: true, finishedAt: "2026-10-23T02:00:00Z", scans: { ok: true, exit: 0, timedOut: false }, planChecks: { ok: true, exit: 0, timedOut: false }, requiredRows: rows.map((r) => r.id), rows: rows.map(row) };
  // Real package reader over a real tree: the driver must read the helper's record (with scripts), not the planner's.
  const fs = require("node:fs"), os = require("node:os"), pathMod = require("node:path");
  const root = fs.mkdtempSync(pathMod.join(os.tmpdir(), "rk-npm-driver-"));
  fs.mkdirSync(pathMod.join(root, "packages", "legal"), { recursive: true });
  fs.writeFileSync(pathMod.join(root, "packages", "legal", "package.json"), JSON.stringify({ name: pkg.name, version: pkg.version, scripts: pkg.scripts }));
  assert.equal(dispatchRefusal(receipt, sha, pkg.name, "npm", {}, pkg.version, { root, pipeline }), null, "the real helper reader supplies the scripts packageRows needs");
  const deps = { readPackages: () => packages, pipeline };
  assert.equal(dispatchRefusal(receipt, sha, pkg.name, "npm", {}, pkg.version, deps), null, "the driver passes what the pinned dispatcher derives");
  assert.match(dispatchRefusal({ ...receipt, dir: "packages/other" }, sha, pkg.name, "npm", {}, pkg.version, deps), /package directory mismatch/);
  assert.match(dispatchRefusal({ ...receipt, requiredRows: ["test"] }, sha, pkg.name, "npm", {}, pkg.version, deps), /row order differs/);
  assert.equal(dispatchRefusal(receipt, sha, "@rightkit/nope", "npm", {}, "1.0.0", deps), "unknown npm package");
  assert.match(dispatchRefusal(receipt, sha, pkg.name, "npm", {}, pkg.version, { ...deps, pipeline: {} }), /store pin/, "no store pin, no plan, no dispatch");
});

test("loop --registry all runs crates and npm units in one coordinator; npm does not wait for crates", async () => {
  assert.equal(parseArgs(["loop", "--registry", "all"]).registry, "all");
  assert.throws(() => parseArgs(["dispatch", "--registry", "all", "x"]), /loop also accepts all/);
  const state = { dispatches: {} };
  const preflights = []; const dispatches = [];
  let release;
  const crateGate = new Promise((r) => { release = r; });
  const ready = { crates: ["rightkit-x"], npm: ["@rightkit/y"] };
  const result = cmdLoop({ registry: "all", units: [], jobs: 1 }, {
    check: async () => ({ code: 0, out: "" }), head: () => "a".repeat(40),
    readiness: async ({ registry }) => ({ unpublished: new Set(ready[registry]), waves: [ready[registry]], crates: [] }),
    readState: () => state, writeState: () => {},
    watch: async () => 0, holdReason: () => null, pause: () => new Promise((r) => setTimeout(r, 5)),
    preflight: async (unit, registry) => { preflights.push(`${registry}:${unit}`); if (registry === "crates") await crateGate; return { code: 0 }; },
    dispatch: async (o) => {
      dispatches.push(`${o.registry}:${o.units[0]}`);
      ready[o.registry] = [];
      state.dispatches[`${o.registry}:${o.units[0]}`] = { unit: o.units[0], registry: o.registry, sha: "a".repeat(40), status: "published" };
      return 0;
    },
  });
  await new Promise((r) => setTimeout(r, 30));
  assert.deepEqual(dispatches, ["npm:@rightkit/y"], "npm dispatched while the crate preflight holds the only crate slot (--jobs 1)");
  release();
  assert.equal(await result, 0);
  assert.deepEqual(preflights.sort(), ["crates:rightkit-x", "npm:@rightkit/y"]);
  assert.deepEqual(dispatches.sort(), ["crates:rightkit-x", "npm:@rightkit/y"]);
});

test("loop --registry all: a crate whose preflight fails does not block the npm unit", async () => {
  const state = { dispatches: {} };
  const dispatches = [];
  const ready = { crates: ["rightkit-bad"], npm: ["@rightkit/y"] };
  const code = await cmdLoop({ registry: "all", units: [], jobs: 2 }, {
    check: async () => ({ code: 0, out: "" }), head: () => "b".repeat(40),
    readiness: async ({ registry }) => ({ unpublished: new Set(ready[registry]), waves: [ready[registry]], crates: [] }),
    readState: () => state, writeState: () => {},
    watch: async () => 0, holdReason: () => null, pause: () => new Promise((r) => setTimeout(r, 5)),
    preflight: async (unit, registry) => (registry === "crates" ? { code: 1, last: "RED" } : { code: 0 }),
    dispatch: async (o) => {
      dispatches.push(`${o.registry}:${o.units[0]}`);
      ready[o.registry] = [];
      state.dispatches[`${o.registry}:${o.units[0]}`] = { unit: o.units[0], registry: o.registry, sha: "b".repeat(40), status: "published" };
      return 0;
    },
  });
  assert.deepEqual(dispatches, ["npm:@rightkit/y"], "npm published although the crate failed its preflight");
  assert.equal(state.dispatches[stateKey("crates", "b".repeat(40), "rightkit-bad")].phase, "preflight");
  assert.equal(code, 1, "the loop still reports the failed crate");
});

// ---- restart at a new SHA: durable (registry, unit, version) claim ----
import { spawnSync as spawnChild } from "node:child_process";
import { mkdtempSync, writeFileSync as writeFileNow, readFileSync as readFileNow } from "node:fs";
import { tmpdir } from "node:os";
import { join as joinPath, resolve as resolvePath } from "node:path";

const MODULE_URL = new URL("./rk-pipeline.mjs", import.meta.url).href;
/** Run cmdLoop in a FRESH Node process at SHA B against a state file on disk written by the "previous" process at SHA A.
 * The SHA A run stays pending for the first watch ticks and only completes on a later tick. */
function freshProcessRestart({ registry, recorded, planned, status = "dispatched" }) {
  const dir = mkdtempSync(joinPath(tmpdir(), "rk-restart-"));
  const A = "a".repeat(40), B = "b".repeat(40), unit = registry === "npm" ? "@rightkit/x" : "rightkit-x";
  const stateFile = joinPath(dir, "state.json");
  writeFileNow(stateFile, JSON.stringify({ dispatches: { [`${registry}:${A}:${unit}`]: { unit, registry, sha: A, version: recorded, status, runId: 101 } } }));
  const child = `
    import fs from "node:fs";
    const { cmdLoop } = await import(${JSON.stringify(MODULE_URL)});
    const file = ${JSON.stringify(stateFile)}; const calls = []; let ticks = 0;
    const read = () => JSON.parse(fs.readFileSync(file, "utf8")); const write = (s) => fs.writeFileSync(file, JSON.stringify(s));
    const keyA = ${JSON.stringify(`${registry}:${A}:${unit}`)};
    const published = () => { const d = read().dispatches; return d[keyA]?.status === "published" && d[keyA].version === ${JSON.stringify(planned)} || calls.includes("dispatch"); };
    const code = await cmdLoop({ registry: ${JSON.stringify(registry)}, units: [], jobs: 2 }, {
      check: async () => ({ code: 0, out: "" }), head: () => ${JSON.stringify(B)},
      readiness: async () => ({ unpublished: new Set(published() ? [] : [${JSON.stringify(unit)}]), waves: [published() ? [] : [${JSON.stringify(unit)}]], crates: new Map([[${JSON.stringify(unit)}, { version: ${JSON.stringify(planned)} }]]) }),
      readState: read, writeState: write,
      watch: async () => { ticks += 1; const s = read(); const d = s.dispatches[keyA]; if (ticks >= 3 && d && !["published", "failed"].includes(d.status)) { d.status = "published"; write(s); } return 0; },
      holdReason: () => null, pause: () => new Promise((r) => setTimeout(r, 5)),
      preflight: async () => { calls.push("preflight"); return { code: 0 }; },
      dispatch: async () => { calls.push("dispatch"); return 0; },
    });
    process.stdout.write("\\nRESULT " + JSON.stringify({ code, calls, ticks, final: read().dispatches[keyA].status }) + "\\n");
  `;
  const r = spawnChild(process.execPath, ["--input-type=module", "-e", child], { encoding: "utf8", timeout: 60_000 });
  assert.equal(r.status, 0, r.stderr);
  const line = r.stdout.split("\n").find((l) => l.startsWith("RESULT "));
  assert.ok(line, r.stdout);
  return JSON.parse(line.slice(7));
}

for (const registry of ["crates", "npm"]) {
  test(`fresh process at SHA B (${registry}): same version in flight at SHA A gets zero pre-flight and zero second dispatch; A completes`, () => {
    const out = freshProcessRestart({ registry, recorded: "1.0.0", planned: "1.0.0" });
    assert.deepEqual(out.calls, [], "no pre-flight or dispatch while SHA A owns the version");
    assert.ok(out.ticks >= 3, "SHA A was still pending on the first watch ticks");
    assert.equal(out.final, "published");
    assert.equal(out.code, 0);
  });
  test(`fresh process at SHA B (${registry}): a changed version stays eligible`, () => {
    const out = freshProcessRestart({ registry, recorded: "0.9.0", planned: "1.0.0" });
    assert.deepEqual(out.calls, ["preflight", "dispatch"]);
  });
  test(`fresh process at SHA B (${registry}): an unknown/ambiguous status is a claim (fail closed)`, () => {
    const out = freshProcessRestart({ registry, recorded: "1.0.0", planned: "1.0.0", status: "unknown" });
    assert.deepEqual(out.calls, []);
  });
}

test("openClaim: same version at another SHA claims; settled, changed version, other registry and same SHA do not", () => {
  const s = { dispatches: { k: { unit: "u", registry: "npm", sha: "A", version: "1.0.0", status: "dispatching" } } };
  assert.ok(openClaim(s, { registry: "npm", unit: "u", version: "1.0.0", sha: "B" }));
  assert.equal(openClaim(s, { registry: "npm", unit: "u", version: "1.1.0", sha: "B" }), null);
  assert.equal(openClaim(s, { registry: "crates", unit: "u", version: "1.0.0", sha: "B" }), null);
  assert.equal(openClaim(s, { registry: "npm", unit: "u", version: "1.0.0", sha: "A" }), null);
  for (const status of ["published", "failed"]) assert.equal(openClaim({ dispatches: { k: { ...s.dispatches.k, status } } }, { registry: "npm", unit: "u", version: "1.0.0", sha: "B" }), null, status);
});

// ---- real cmdWatch: ambiguous intent / deadline must keep the claim (no stub watch) ----
async function restartWithRealWatch(record) {
  const A = "e".repeat(40), B = "f".repeat(40), unit = "rightkit-x";
  const state = { dispatches: { [stateKey("crates", A, unit)]: { unit, registry: "crates", sha: A, version: "1.0.0", ...record } } };
  const calls = [];
  const code = await cmdLoop({ registry: "crates", units: [], jobs: 2 }, {
    check: async () => ({ code: 0, out: "" }), head: () => B,
    readiness: async () => ({ unpublished: new Set([unit]), waves: [[unit]], crates: new Map([[unit, { version: "1.0.0" }]]) }),
    readState: () => state, writeState: () => {},
    // The REAL watcher; only the GitHub/registry reads are fakes (run not readable, no uploads, nothing published).
    watch: (o) => cmdWatch(o, { readState: () => state, writeState: () => {}, runState: () => null, uploadRun: () => [], registryVersion: async () => new Map() }),
    holdReason: () => null, pause: () => new Promise((r) => setTimeout(r, 5)),
    preflight: async () => { calls.push("preflight"); return { code: 0 }; },
    dispatch: async () => { calls.push("dispatch"); return 0; },
  });
  return { code, calls, record: state.dispatches[stateKey("crates", A, unit)] };
}

test("real cmdWatch: an ambiguous dispatch intent becomes failed+reconcile and still blocks a restart dispatch", async () => {
  const out = await restartWithRealWatch({ status: "unknown" });
  assert.equal(out.record.status, "failed");
  assert.equal(out.record.reconcile, true);
  assert.deepEqual(out.calls, [], "no pre-flight or dispatch while a run may exist");
  assert.equal(out.code, 1, "reported unfinished: reconcile required");
});

test("real cmdWatch: a watch deadline becomes failed+reconcile and still blocks a restart dispatch", async () => {
  const out = await restartWithRealWatch({ status: "dispatched", runId: 7, runAttempt: 1, dispatchedAt: "2026-01-01T00:00:00.000Z" });
  assert.match(out.record.conclusion, /deadline/);
  assert.equal(out.record.reconcile, true);
  assert.deepEqual(out.calls, []);
});

// Real cmdWatch with one dispatch that has a green qualification run; only the upload/registry read is faked.
async function watchWithFailingRead(read) {
  const key = stateKey("crates", SHA, "unit");
  const dispatch = { unit: "unit", registry: "crates", sha: SHA, version: "1.0.0", runId: 1, runAttempt: 1, requestId: "req-1", dispatchedAt: new Date().toISOString(), status: "dispatched" };
  let state = { dispatches: { [key]: dispatch } };
  const lines = [];
  const write = process.stdout.write;
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  try {
    const code = await cmdWatch({ wait: false }, {
      readState: () => structuredClone(state), writeState: (s) => { state = s; },
      runState: () => ({ status: "completed", conclusion: "success", head_sha: SHA, run_attempt: 1, path: ".github/workflows/publish.yml", event: "workflow_dispatch", display_title: "publish req-1" }),
      ...read,
    });
    return { code, lines, record: state.dispatches[key] };
  } finally {
    process.stdout.write = write;
  }
}

test("real cmdWatch: an unreadable upload-run listing is logged and the dispatch stays pending, not published or failed", async () => {
  const out = await watchWithFailingRead({
    uploadRun: () => { throw new Error("upload runs are unreadable: ETIMEDOUT"); },
    registryVersion: async () => { throw new Error("registry must not be read before the upload succeeds"); },
  });
  assert.equal(out.code, 1, "reported unfinished");
  assert.equal(out.record.status, "dispatched");
  assert.equal(out.record.published, undefined);
  assert.ok(out.lines.some((l) => l.includes("unit: upload runs unreadable: upload runs are unreadable: ETIMEDOUT")), out.lines.join(""));
});

test("real cmdWatch: a registry read that throws is logged and the dispatch stays pending, not published or failed", async () => {
  const out = await watchWithFailingRead({
    uploadRun: () => [{ id: 20, status: "completed", conclusion: "success" }],
    registryVersion: async () => { throw new Error("HTTP 503 from index"); },
  });
  assert.equal(out.code, 1, "reported unfinished");
  assert.equal(out.record.status, "dispatched");
  assert.equal(out.record.published, undefined);
  assert.ok(out.lines.some((l) => l.includes("unit: registry read failed: HTTP 503 from index")), out.lines.join(""));
});

test("uploadRunsQuery: created filter is %3E%3D-encoded and starts 15 minutes before dispatchedAt", () => {
  const dispatchedAt = "2026-10-23T10:00:00.000Z";
  const query = uploadRunsQuery({ dispatchedAt });
  assert.equal(query, "repos/bogusyogi/rightkit/actions/workflows/publish-direct.yml/runs?event=workflow_run&per_page=100&created=%3E%3D2026-10-23T09:45:00.000Z");
});

test("needsReconcile: plain run/upload failures do not claim; ambiguous ones do (flag or legacy conclusion)", () => {
  assert.equal(needsReconcile({ status: "failed", conclusion: "failure" }), false);
  assert.equal(needsReconcile({ status: "failed", conclusion: "upload failure" }), false);
  assert.equal(needsReconcile({ status: "failed", reconcile: true }), true);
  assert.equal(needsReconcile({ status: "failed", conclusion: "watch deadline exceeded; reconcile existing run before retry" }), true);
  assert.equal(needsReconcile({ status: "published" }), false);
});

// ---- @rightkit/release pipeline canary in the loop ----
import { CANARY_INCOMPLETE, CANARY_REGRESSION, needsCanary } from "./rk-pipeline.mjs";

function canaryLoopHarness({ sha = "c".repeat(40), units = ["@rightkit/release", "@rightkit/qa"], canaryCodes = {} } = {}) {
  let state = { dispatches: {} };
  const events = [];
  const pending = new Set(units);
  const run = (code) => cmdLoop({ registry: "npm", units: [], jobs: 2 }, {
    check: async () => ({ code: 0, out: "" }), head: () => sha,
    readiness: async () => ({ unpublished: new Set(pending), waves: [[...pending]], crates: [] }),
    readState: () => structuredClone(state), writeState: (s) => { state = structuredClone(s); },
    watch: async () => 0, holdReason: () => null, pause: () => new Promise((r) => setTimeout(r, 1)),
    canary: async (unit) => { events.push(`canary:${unit}`); const c = canaryCodes[unit]?.shift() ?? 0; return { code: c, last: `canary exit ${c}` }; },
    preflight: async (unit) => { events.push(`preflight:${unit}`); return { code: 0 }; },
    dispatch: async (o) => {
      events.push(`dispatch:${o.units[0]}`); pending.delete(o.units[0]);
      state.dispatches[stateKey("npm", sha, o.units[0])] = { unit: o.units[0], registry: "npm", sha, status: "published" };
      return 0;
    },
  });
  return { run, events, get state() { return state; }, sha };
}

test("loop: the canary runs before pre-flight for @rightkit/release only", async () => {
  assert.equal(needsCanary("@rightkit/release", "npm"), true);
  assert.equal(needsCanary("@rightkit/qa", "npm"), false);
  assert.equal(needsCanary("@rightkit/release", "crates"), false);
  const h = canaryLoopHarness();
  assert.equal(await h.run(), 0);
  assert.ok(h.events.indexOf("canary:@rightkit/release") < h.events.indexOf("preflight:@rightkit/release"));
  assert.ok(h.events.indexOf("preflight:@rightkit/release") < h.events.indexOf("dispatch:@rightkit/release"));
  assert.ok(!h.events.includes("canary:@rightkit/qa"), "other units get no canary");
  assert.ok(h.events.includes("preflight:@rightkit/qa") && h.events.includes("dispatch:@rightkit/qa"));
});

test("loop: a canary regression records phase canary, skips pre-flight and dispatch, and is not retried at the same SHA", async () => {
  const h = canaryLoopHarness({ units: ["@rightkit/release"], canaryCodes: { "@rightkit/release": [CANARY_REGRESSION, 0] } });
  assert.equal(await h.run(), 1);
  assert.deepEqual(h.events, ["canary:@rightkit/release"]);
  const record = h.state.dispatches[stateKey("npm", h.sha, "@rightkit/release")];
  assert.equal(record.status, "failed");
  assert.equal(record.phase, "canary");
  // A fresh loop at the same SHA (the state survived): the unit is not attempted again, even though the next canary would be green.
  assert.equal(await h.run(), 1);
  assert.deepEqual(h.events, ["canary:@rightkit/release"], "no second canary, no pre-flight");
});

test("loop: an incomplete canary records canary-incomplete, skips pre-flight, and IS retried by the next loop at the same SHA", async () => {
  const h = canaryLoopHarness({ units: ["@rightkit/release"], canaryCodes: { "@rightkit/release": [CANARY_INCOMPLETE, 0] } });
  assert.equal(await h.run(), 1);
  assert.deepEqual(h.events, ["canary:@rightkit/release"]);
  assert.equal(h.state.dispatches[stateKey("npm", h.sha, "@rightkit/release")].phase, "canary-incomplete");
  assert.equal(await h.run(), 0);
  assert.deepEqual(h.events, ["canary:@rightkit/release", "canary:@rightkit/release", "preflight:@rightkit/release", "dispatch:@rightkit/release"]);
});

test("loop: a crashing canary process (any other exit) is treated as incomplete, never as green", async () => {
  const h = canaryLoopHarness({ units: ["@rightkit/release"], canaryCodes: { "@rightkit/release": [2] } });
  assert.equal(await h.run(), 1);
  assert.equal(h.state.dispatches[stateKey("npm", h.sha, "@rightkit/release")].phase, "canary-incomplete");
  assert.ok(!h.events.includes("preflight:@rightkit/release"));
});

test("the canary command is parsed and refuses units without a canary", () => {
  assert.equal(parseArgs(["canary", "@rightkit/release"]).command, "canary");
  assert.deepEqual(parseArgs(["canary", "@rightkit/release"]).units, ["@rightkit/release"]);
});

// ---- P1 of docs/PIPELINE-ARCHITECTURE.md: the files-only check runs before anything is canaried, compiled or dispatched ----
const RED_CHECK = async () => ({ code: 1, out: "FAIL consumer-locks (G08): ci/consumers/wv2/Cargo.lock lists rightkit-a 0.6.0\npreflight: 1 problem(s)\n" });

test("P1: a red check stops the loop before any canary, pre-flight or dispatch", async () => {
  const events = [];
  const code = await cmdLoop({ registry: "npm", units: [], jobs: 2 }, {
    check: async (...a) => { events.push("check"); return RED_CHECK(...a); },
    head: () => "c".repeat(40),
    readiness: async () => { events.push("readiness"); return { unpublished: new Set(["@rightkit/release"]), waves: [["@rightkit/release"]], crates: [] }; },
    readState: () => ({ dispatches: {} }), writeState: () => { events.push("write"); },
    watch: async () => { events.push("watch"); return 0; }, holdReason: () => null, pause: () => Promise.resolve(),
    canary: async () => { events.push("canary"); return { code: 0 }; },
    preflight: async () => { events.push("preflight"); return { code: 0 }; },
    dispatch: async () => { events.push("dispatch"); return 0; },
  });
  assert.equal(code, 1);
  assert.deepEqual(events, ["check"], "nothing else ran, and no state was written");
});

test("P1: pre-flight runs the check first and compiles nothing when it is red", async () => {
  const compiled = [];
  const many = async (units) => { compiled.push(...units); return new Map(units.map((u) => [u, { code: 0, last: "GREEN" }])); };
  assert.equal(await cmdPreflight({ registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: () => "c".repeat(40), check: RED_CHECK, preflightMany: many }), 1);
  assert.deepEqual(compiled, []);
  assert.equal(await cmdPreflight({ registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: () => "c".repeat(40), check: async () => ({ code: 0 }), preflightMany: many }), 0);
  assert.deepEqual(compiled, ["rightkit-a"]);
});

test("P1: a direct canary runs the check first and starts no canary when it is red", async () => {
  const canaried = [];
  const canary = async (unit) => { canaried.push(unit); return { code: 0, last: "GREEN" }; };
  assert.equal(await cmdCanary({ units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: RED_CHECK, canary }), 1);
  assert.deepEqual(canaried, []);
  assert.equal(await cmdCanary({ units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: async () => ({ code: 0 }), canary }), 0);
  assert.deepEqual(canaried, ["@rightkit/release"]);
});

test("runner-aware dispatch: at maxInFlightRuns hosted runs the dispatcher holds (exit 3) and sends nothing; a count gh cannot give refuses (exit 2)", async () => {
  let planned = false;
  const planFn = async () => { planned = true; return { units: [] }; };
  const green = async () => ({ code: 0, out: "" });
  const pipeline = { dispatch: { maxInFlightRuns: 2 } };
  assert.equal(await cmdDispatch({ registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: green, planFn, pipeline, inFlight: async () => 2 }), 3);
  assert.equal(planned, false, "a hold reaches no plan or dispatch");
  assert.equal(await cmdDispatch({ registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: green, planFn, pipeline, inFlight: async () => null }), 2);
  assert.equal(planned, false);
  // Below the limit the dispatcher goes on to the tree checks (git state, origin/main), which this unit test does not own.
});

test("hostedRunsInFlight counts queued, waiting and running publish and publish-app runs, and is null when gh fails", async () => {
  const calls = [];
  const gh = (n) => async (cmd, args) => { calls.push(args.join(" ")); return { code: 0, out: JSON.stringify(Array.from({ length: n }, (_, i) => ({ databaseId: i }))) }; };
  assert.equal(await hostedRunsInFlight(gh(1)), 6, "one run per workflow per status");
  assert.ok(calls.every((c) => /^run list -R bogusyogi\/rightkit --workflow publish(-app)?\.yml --status (queued|in_progress|waiting) /.test(c)), calls.join("\n"));
  assert.equal(await hostedRunsInFlight(async () => ({ code: 1, out: "" })), null);
});

test("P1: a direct dispatch runs the check first and dispatches nothing when it is red", async () => {
  let planned = false;
  const planFn = async () => { planned = true; return { units: [] }; };
  assert.equal(await cmdDispatch({ registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: RED_CHECK, planFn }), 1);
  assert.equal(planned, false, "a red check reaches no receipt, plan or dispatch");
});

// Astra's second review of #945: check A, another chat commits B, the canary packs B. One SHA binds every stage.
const A = "a".repeat(40), B = "b".repeat(40);
const movingHead = () => { let moved = false; return { head: () => (moved ? B : A), move: () => { moved = true; } }; };

test("P1: the check reads the captured commit, and HEAD moving while it runs starts nothing", async () => {
  for (const command of ["canary", "preflight", "dispatch", "loop"]) {
    const h = movingHead();
    const events = [];
    const check = async (sha) => { events.push(`check ${sha.slice(0, 1)}`); h.move(); return { code: 0, out: "" }; };
    const deps = {
      head: h.head, check,
      canary: async () => { events.push("canary"); return { code: 0 }; },
      preflightMany: async (units) => { events.push("preflight"); return new Map(units.map((u) => [u, { code: 0 }])); },
      preflight: async () => { events.push("preflight"); return { code: 0 }; },
      planFn: async () => { events.push("plan"); return { units: [] }; },
      dispatch: async () => { events.push("dispatch"); return 0; },
      readiness: async () => { events.push("readiness"); return { unpublished: new Set(["@rightkit/release"]), waves: [["@rightkit/release"]], crates: [] }; },
      readState: () => ({ dispatches: {} }), writeState: () => events.push("write"),
      watch: async () => 0, holdReason: () => null, pause: () => Promise.resolve(),
    };
    const fn = { canary: cmdCanary, preflight: cmdPreflight, dispatch: cmdDispatch, loop: cmdLoop }[command];
    const code = await fn({ registry: "npm", units: ["@rightkit/release"], jobs: 1 }, deps);
    assert.equal(code, 1, command);
    assert.deepEqual(events, ["check a"], `${command}: the check saw the captured commit and nothing else started`);
  }
});

test("P1: the canary child is handed the checked commit", async () => {
  const seen = [];
  const code = await cmdCanary({ units: ["@rightkit/release"] }, { head: () => A, check: async () => ({ code: 0 }), canary: async (unit, sha) => { seen.push([unit, sha]); return { code: 0, last: "GREEN" }; } });
  assert.equal(code, 0);
  assert.deepEqual(seen, [["@rightkit/release", A]]);
});

test("P1: pre-flight results from a HEAD that moved while they ran do not count", async () => {
  const h = movingHead();
  const many = async (units) => { h.move(); return new Map(units.map((u) => [u, { code: 0, last: "GREEN" }])); };
  assert.equal(await cmdPreflight({ registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: h.head, check: async () => ({ code: 0 }), preflightMany: many }), 1);
});

test("dispatch reads the gate's qualification receipt for exactly HEAD and the one unit; without it nothing is dispatched", () => {
  const q = { schema: "rightkit.unit-qualification/1", unit: { kind: "npm", name: "@rightkit/release", version: "0.6.9" }, commit: SHA, tree: "c".repeat(40), runtime: "r", executor: "broker",
    stages: { check: { ok: true }, compile: { ok: true }, package: { ok: true, artifactSha256: "d".repeat(64) }, qualify: { ok: true, artifactSha256: "d".repeat(64) } }, artifactSha256: "d".repeat(64) };
  const read = (over = {}, ctx = {}) => readQualification("/bundle/qualification.json", { sha: SHA, units: ["@rightkit/release"], registry: "npm", readFile: () => JSON.stringify({ ...q, ...over }), ...ctx });
  assert.equal(read().artifactSha256, "d".repeat(64));
  assert.equal(read().file, resolvePath("/bundle/qualification.json"), "the receipt path is reported resolved (on Windows it gains the drive)");
  assert.throws(() => readQualification(undefined, { sha: SHA, units: ["@rightkit/release"], registry: "npm" }), /dispatch requires --qualification/);
  assert.throws(() => read({ commit: "e".repeat(40) }), /not HEAD/);
  assert.throws(() => read({}, { units: ["@rightkit/ci"] }), /covers npm:@rightkit\/release; this dispatch names npm @rightkit\/ci/);
  assert.throws(() => read({}, { units: ["@rightkit/release", "@rightkit/ci"] }), /this dispatch names/);
  assert.throws(() => read({}, { registry: "crates" }), /covers npm:/);
  assert.throws(() => read({ stages: { ...q.stages, package: { ok: false } } }), /stage package is not ok/);
  assert.throws(() => read({ stages: { ...q.stages, qualify: { ok: true, artifactSha256: "e".repeat(64) } } }), /one artifact sha256/);
  assert.throws(() => read({ schema: "x" }), /schema/);
});
