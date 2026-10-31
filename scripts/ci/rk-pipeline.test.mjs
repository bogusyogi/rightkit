import assert from "node:assert/strict";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { packageRows } from "./rk-npm-preflight.mjs";
import { dispatchIdentity, explicitShaRefusal, openClaim, needsReconcile, acquireLock, releaseLock, closureOf, dispatchRefusal, holdReason, satisfiesAny, readiness, allowsetsFor, cmdCanary, cmdDispatch, cmdLoop, cmdPreflight, cmdWatch, matchingUploadRuns, uploadRunsQuery, parseArgs, readQualification, registryVersion, satisfies, stateKey, waves, hostedRunsInFlight } from "./rk-pipeline.mjs";
// GB-07: the coordinator takes its workspace root as a parameter; these tests judge this repository.
const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
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
  const result = await cmdLoop({ root: REPO_ROOT, registry: "crates", units: [], jobs: 2 }, {
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
  const result = await cmdLoop({ root: REPO_ROOT, registry: "crates", units: [], jobs: 2 }, {
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
  assert.equal(await cmdWatch({ root: REPO_ROOT, wait: false }, deps), 0);
  assert.equal(state.dispatches[key].published, "1.0.0");
  assert.ok(state.dispatches.concurrent);
  state = { dispatches: { [key]: { ...dispatch, status: "dispatched" } } };
  deps.uploadRun = () => [{ id: 20, status: "completed", conclusion: "failure" }];
  assert.equal(await cmdWatch({ root: REPO_ROOT, wait: false }, deps), 1);
  assert.equal(state.dispatches[key].status, "failed");
});

test("dependent enters once its registry prerequisite is live while unrelated slow preflight remains running", async () => {
  let releaseSlow;
  const slow = new Promise((r) => { releaseSlow = r; });
  const done = new Set(), entered = [];
  const result = await cmdLoop({ root: REPO_ROOT, registry: "crates", units: [], jobs: 2 }, {
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
  const loop = cmdLoop({ root: REPO_ROOT, registry: "crates", units: ["rightkit-x"], jobs: 2, wait: false }, deps);
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
  const result = await cmdLoop({ root: REPO_ROOT, registry: "crates", units: [], jobs: 2 }, {
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
  const result = cmdLoop({ root: REPO_ROOT, registry: "all", units: [], jobs: 1 }, {
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
  const code = await cmdLoop({ root: REPO_ROOT, registry: "all", units: [], jobs: 2 }, {
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
    const code = await cmdLoop({ root: ${JSON.stringify(REPO_ROOT)}, registry: ${JSON.stringify(registry)}, units: [], jobs: 2 }, {
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
  const code = await cmdLoop({ root: REPO_ROOT, registry: "crates", units: [], jobs: 2 }, {
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
    const code = await cmdWatch({ root: REPO_ROOT, wait: false }, {
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
  const run = (code) => cmdLoop({ root: REPO_ROOT, registry: "npm", units: [], jobs: 2 }, {
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
  const code = await cmdLoop({ root: REPO_ROOT, registry: "npm", units: [], jobs: 2 }, {
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
  assert.equal(await cmdPreflight({ root: REPO_ROOT, registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: () => "c".repeat(40), check: RED_CHECK, preflightMany: many }), 1);
  assert.deepEqual(compiled, []);
  assert.equal(await cmdPreflight({ root: REPO_ROOT, registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: () => "c".repeat(40), check: async () => ({ code: 0 }), preflightMany: many }), 0);
  assert.deepEqual(compiled, ["rightkit-a"]);
});

test("P1: a direct canary runs the check first and starts no canary when it is red", async () => {
  const canaried = [];
  const canary = async (unit) => { canaried.push(unit); return { code: 0, last: "GREEN" }; };
  assert.equal(await cmdCanary({ root: REPO_ROOT, units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: RED_CHECK, canary }), 1);
  assert.deepEqual(canaried, []);
  assert.equal(await cmdCanary({ root: REPO_ROOT, units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: async () => ({ code: 0 }), canary }), 0);
  assert.deepEqual(canaried, ["@rightkit/release"]);
});

test("runner-aware dispatch: at maxInFlightRuns hosted runs the dispatcher holds (exit 3) and sends nothing; a count gh cannot give refuses (exit 2)", async () => {
  let planned = false;
  const planFn = async () => { planned = true; return { units: [] }; };
  const green = async () => ({ code: 0, out: "" });
  const pipeline = { dispatch: { maxInFlightRuns: 2 } };
  assert.equal(await cmdDispatch({ root: REPO_ROOT, registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: green, planFn, pipeline, inFlight: async () => 2 }), 3);
  assert.equal(planned, false, "a hold reaches no plan or dispatch");
  assert.equal(await cmdDispatch({ root: REPO_ROOT, registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: green, planFn, pipeline, inFlight: async () => null }), 2);
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
  assert.equal(await cmdDispatch({ root: REPO_ROOT, registry: "npm", units: ["@rightkit/release"] }, { head: () => "c".repeat(40), check: RED_CHECK, planFn }), 1);
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
  const code = await cmdCanary({ root: REPO_ROOT, units: ["@rightkit/release"] }, { head: () => A, check: async () => ({ code: 0 }), canary: async (unit, sha) => { seen.push([unit, sha]); return { code: 0, last: "GREEN" }; } });
  assert.equal(code, 0);
  assert.deepEqual(seen, [["@rightkit/release", A]]);
});

test("P1: pre-flight results from a HEAD that moved while they ran do not count", async () => {
  const h = movingHead();
  const many = async (units) => { h.move(); return new Map(units.map((u) => [u, { code: 0, last: "GREEN" }])); };
  assert.equal(await cmdPreflight({ root: REPO_ROOT, registry: "crates", units: ["rightkit-a"], jobs: 1 }, { head: h.head, check: async () => ({ code: 0 }), preflightMany: many }), 1);
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

test("[GB-46] coordinator lock: published complete or not at all; pause between create and write cannot yield two owners; torn/unknown record is never dead; pid reuse is dead; release after a successor deletes nothing; simultaneous stale reclaim keeps exactly one owner", () => {
  const fs = require("node:fs"), os = require("node:os"), path = require("node:path");
  const fresh = () => path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock46-")), "state.json.lock");
  const idle = { wait: () => {}, identity: () => ({ startedMs: null, bootMs: null }) };
  // 1. A creator paused right before it publishes: a second coordinator takes the lock; the first, resumed, finds a LIVE owner and does not clobber it.
  let lock = fresh(); let second;
  assert.throws(() => acquireLock(lock, { ...idle, pid: 1, alive: () => true, hooks: { beforePublish: () => { if (!second) { second = acquireLock(lock, { ...idle, pid: 2, alive: () => true }); } } } }), /another coordinator \(pid 2\)/);
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, 2, "exactly one owner");
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).nonce, second.nonce);
  assert.deepEqual(fs.readdirSync(path.dirname(lock)).filter((n) => n.endsWith(".tmp")), [], "no private temp file survives");
  // 2. The file at the lock path is always a complete record: a reader during acquire never sees an empty or partial one.
  lock = fresh();
  acquireLock(lock, { ...idle, pid: 5, alive: () => true });
  const text = fs.readFileSync(lock, "utf8");
  assert.doesNotThrow(() => JSON.parse(text)); assert.match(text, /"nonce":"[0-9a-f]{32}"/); assert.match(text, /"schema":2/);
  // 3. A crash-torn or empty record is UNKNOWN until proven stale: it is refused and never unlinked, even though the "owner" cannot be judged alive.
  for (const torn of ["", '{"pid":', "not json", "{}"]) {
    lock = fresh(); fs.writeFileSync(lock, torn);
    assert.throws(() => acquireLock(lock, { ...idle, pid: 9, alive: () => false }), /owner cannot be read .*UNKNOWN, not dead/, JSON.stringify(torn));
    assert.equal(fs.readFileSync(lock, "utf8"), torn, "the torn record was not deleted");
  }
  // 4. PID reuse: the recorded pid is alive but it started at another time, so the lock is the dead owner's and is reclaimed.
  lock = fresh();
  fs.writeFileSync(lock, JSON.stringify({ schema: 2, pid: 111, nonce: "a".repeat(32), startedMs: 1_000_000, bootMs: null }));
  const reused = acquireLock(lock, { pid: 222, alive: () => true, wait: () => {}, identity: (p) => (p === 111 ? { startedMs: 9_000_000, bootMs: null } : { startedMs: 5, bootMs: null }) });
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, 222);
  assert.ok(reused.nonce);
  // ...and the same pid with the same start is a live owner that excludes.
  lock = fresh();
  fs.writeFileSync(lock, JSON.stringify({ schema: 2, pid: 111, nonce: "b".repeat(32), startedMs: 1_000_000, bootMs: null }));
  assert.throws(() => acquireLock(lock, { pid: 222, alive: () => true, wait: () => {}, identity: () => ({ startedMs: 1_000_500, bootMs: null }) }), /another coordinator \(pid 111\)/);
  // 5. Release after a successor: A's stale release (its nonce) deletes nothing of B's lock.
  lock = fresh();
  const a = acquireLock(lock, { ...idle, pid: 10, alive: () => true });
  const b = acquireLock(lock, { ...idle, pid: 11, alive: (p) => p !== 10 }); // A is dead; B reclaimed
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, 11);
  releaseLock(lock, { pid: 10, nonce: a.nonce });
  assert.ok(fs.existsSync(lock), "a release by a replaced owner never deletes the successor's lock");
  releaseLock(lock, { pid: 11, nonce: "0".repeat(32) });
  assert.ok(fs.existsSync(lock), "a wrong nonce releases nothing");
  releaseLock(lock, { pid: 11, nonce: b.nonce });
  assert.ok(!fs.existsSync(lock));
  // 6. Two coordinators reclaim the same dead owner at once: the second starts while the first is between its judgment and its unlink; one owner results.
  lock = fresh();
  fs.writeFileSync(lock, JSON.stringify({ schema: 2, pid: 700, nonce: "c".repeat(32), startedMs: null, bootMs: null }));
  let late; let firstError = null;
  try { acquireLock(lock, { ...idle, pid: 701, alive: (p) => p !== 700, hooks: { afterJudgedDead: () => { if (!late) { late = "tried"; try { acquireLock(lock, { ...idle, pid: 702, alive: (p) => p !== 700, maxWaitMs: 50 }); late = "acquired"; } catch (error) { late = error.message; } } } } }); } catch (error) { firstError = error.message; }
  assert.equal(late, "acquired", "the second coordinator reclaimed the dead owner while the first was between judgment and unlink");
  assert.match(firstError ?? "", /another coordinator \(pid 702\)/, "the resumed first coordinator found the successor's LIVE lock and neither deleted nor overwrote it");
  const owners = [701, 702].filter((p) => JSON.parse(fs.readFileSync(lock, "utf8")).pid === p);
  assert.equal(owners.length, 1, `exactly one owner (late: ${late})`);
  assert.deepEqual(fs.readdirSync(path.dirname(lock)).filter((n) => /\.(tmp|reclaim)$/.test(n)), [], "no reservation or temp file is left behind");
  // 7. A schema-1 record (an earlier coordinator's) is a complete record judged by its pid.
  lock = fresh(); fs.writeFileSync(lock, JSON.stringify({ pid: 31, at: "2026-01-01T00:00:00Z" }));
  assert.throws(() => acquireLock(lock, { ...idle, pid: 32, alive: () => true }), /another coordinator \(pid 31\)/);
  acquireLock(lock, { ...idle, pid: 32, alive: () => false });
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, 32);
});

test("[GB-07] --sha is the candidate's commit as an explicit input: it parses, HEAD must be exactly it for canary, preflight and dispatch (nothing starts otherwise), and no --sha keeps the old behaviour", async () => {
  const SHA40 = "c".repeat(40), OTHER = "d".repeat(40);
  assert.equal(parseArgs(["dispatch", "u", "--sha", SHA40]).sha, SHA40);
  assert.equal(parseArgs(["dispatch", "u"]).sha, undefined);
  assert.throws(() => parseArgs(["dispatch", "u", "--sha", "abc"]), /full lowercase commit id/);
  assert.equal(explicitShaRefusal({ sha: SHA40 }, SHA40), null);
  assert.match(explicitShaRefusal({ sha: SHA40 }, OTHER), /not the explicit candidate commit/);
  assert.equal(explicitShaRefusal({}, OTHER), null);
  let started = false;
  const check = async () => { started = true; return { code: 0, out: "" }; };
  const head = () => OTHER;
  for (const [cmd, extra] of [[cmdCanary, { canary: async () => { started = true; return { code: 0 }; } }], [cmdPreflight, { preflightMany: async () => { started = true; return new Map(); } }], [cmdDispatch, { planFn: async () => { started = true; return { units: [] }; }, inFlight: async () => { started = true; return 0; } }]]) {
    assert.equal(await cmd({ registry: "npm", units: ["@rightkit/release"], sha: SHA40 }, { head, check, ...extra }), 2);
    assert.equal(started, false, "another HEAD than the explicit commit starts nothing, not even the check");
  }
});

test("[GB-07] a dispatch record keeps the candidate's tree and artifact digest from the qualification receipt it was dispatched with", () => {
  const q = { commit: "c".repeat(40), tree: "e".repeat(40), artifactSha256: "9".repeat(64), unit: { kind: "npm", name: "@rightkit/release", version: "1.2.3" } };
  assert.deepEqual(dispatchIdentity(q), { tree: "e".repeat(40), artifactSha256: "9".repeat(64) });
  assert.deepEqual(dispatchIdentity({}), { tree: undefined, artifactSha256: undefined });
});

// ---------------------------------------------------------------------------------------------------------------------
// GB-07 (BROKER-CONTRACT section 16): the coordinator takes its root and explicit inputs as parameters, and has an in-gate-process entry.
// Test: `gb07_an_uncarried_control_plane_adapter_runs_from_the_primary_and_never_from_the_managed_snapshot` (the wire RightKit follows).
// ---------------------------------------------------------------------------------------------------------------------
import fs from "node:fs";
import os from "node:os";
import { spawnSync } from "node:child_process";
import { inGateDeps, installedToolsRoot, main as pipelineMain, readCrates, readPackages, readUnits, rootOf, runPipeline, stateFileFor } from "./rk-pipeline.mjs";

const scratch = (prefix) => { const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
function gitRepo() {
  const dir = scratch("gb07-pipeline-checkout-");
  const git = (...args) => spawnSync("git", ["-c", "user.email=t@example.test", "-c", "user.name=t", ...args], { cwd: dir, encoding: "utf8" });
  git("init", "-q"); fs.writeFileSync(path.join(dir, "f.txt"), "x"); git("add", "."); git("commit", "-q", "-m", "c");
  return { dir, head: git("rev-parse", "HEAD").stdout.trim() };
}

test("[GB-07] no function of the coordinator derives a root from where the coordinator lives: every reader and command refuses a call without an explicit absolute root", async () => {
  for (const bad of [undefined, "", "relative/dir", null, 7]) assert.throws(() => rootOf(bad), /workspace root as an absolute-path parameter/);
  assert.equal(rootOf(REPO_ROOT), REPO_ROOT);
  assert.equal(rootOf("D:\\work\\rightkit"), "D:\\work\\rightkit", "a Windows absolute root is accepted on any host");
  for (const read of [() => readCrates(), () => readPackages(), () => readUnits("crates"), () => readUnits("npm", undefined)]) assert.throws(read, /workspace root as an absolute-path parameter/);
  await assert.rejects(cmdPreflight({ registry: "crates", units: ["rightkit-a"], jobs: 1 }, { check: async () => ({ code: 0 }), preflightMany: async () => new Map() }), /workspace root as an absolute-path parameter/, "HEAD is read from the root, so no root reads no HEAD");
  assert.equal(await pipelineMain(["preflight", "rightkit-a"], { root: undefined }), 2, "a main without a root exits 2 and starts nothing");
  // The one place a root comes from a location is the command line wrapper; the modules exported to a gate hold no ROOT constant at all.
  const source = fs.readFileSync(path.join(REPO_ROOT, "scripts", "ci", "rk-pipeline.mjs"), "utf8");
  assert.doesNotMatch(source.replace(/\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, ""), /\bROOT\b/, "no ROOT constant (the root of THIS checkout) survives in the coordinator");
});

test("[GB-07] the root is read where the caller points it: HEAD, the explicit --sha and the check all follow the root parameter, not the installed coordinator's checkout", async () => {
  const checkout = gitRepo();
  const seen = [];
  const deps = { check: async (sha, opts) => { seen.push(["check", sha, opts.root]); return { code: 0, out: "" }; }, preflightMany: async (units, opts) => { seen.push(["preflight", units, opts.root]); return new Map(units.map((u) => [u, { code: 0, last: "" }])); } };
  const ok = await runPipeline({ argv: ["preflight", "rightkit-a", "--sha", checkout.head], root: checkout.dir, stateFile: path.join(scratch("gb07-state-"), "state.json"), deps });
  assert.equal(ok.status, 0, ok.stderr);
  assert.deepEqual(seen, [["check", checkout.head, checkout.dir], ["preflight", ["rightkit-a"], checkout.dir]]);
  // HEAD of the installed repository is not the explicit candidate commit of the (temporary) control-plane checkout, and the converse refuses too.
  seen.length = 0;
  const other = await runPipeline({ argv: ["preflight", "rightkit-a", "--sha", "9".repeat(40)], root: checkout.dir, deps });
  assert.equal(other.status, 2);
  assert.match(other.stderr, /not the explicit candidate commit/);
  assert.deepEqual(seen, [], "another HEAD than the explicit commit starts nothing");
});

test("[GB-07] runPipeline runs in the calling process and returns what a child would have: status, stdout (log) and stderr (refusals), per call", async () => {
  const checkout = gitRepo();
  const deps = { check: async () => ({ code: 1, out: "FAIL some-rule\n     fix: do it\n" }) };
  const red = await runPipeline({ argv: ["canary", "@rightkit/release", "--sha", checkout.head], root: checkout.dir, deps });
  assert.equal(red.status, 1);
  assert.match(red.stdout, /FAIL some-rule/);
  assert.match(red.stdout, /check RED at/);
  const none = await runPipeline({ argv: ["canary", "@rightkit/other", "--sha", checkout.head], root: checkout.dir, deps: { check: async () => ({ code: 0, out: "" }) } });
  assert.equal(none.status, 2);
  assert.match(none.stderr, /no pipeline canary for this unit/, "the gate's `release` step recognises exactly this sentence");
  const usage = await runPipeline({ argv: ["frobnicate"], root: checkout.dir, deps });
  assert.equal(usage.status, 2);
  assert.match(usage.stderr, /command must be/);
  // Two overlapping runs keep their own output (the sinks are per run, not module state).
  const [a, b] = await Promise.all([
    runPipeline({ argv: ["canary", "@rightkit/release", "--sha", checkout.head], root: checkout.dir, deps: { check: async () => { await new Promise((r) => setTimeout(r, 20)); return { code: 1, out: "FAIL a-only\n" }; } } }),
    runPipeline({ argv: ["canary", "@rightkit/release", "--sha", checkout.head], root: checkout.dir, deps: { check: async () => ({ code: 1, out: "FAIL b-only\n" }) } }),
  ]);
  assert.match(a.stdout, /a-only/); assert.doesNotMatch(a.stdout, /b-only/);
  assert.match(b.stdout, /b-only/); assert.doesNotMatch(b.stdout, /a-only/);
});

test("[GB-07] the in-gate stages are the INSTALLED gate's modules judging the root as data: the check gets --repo <root>, the pre-flight gets root plus the installed tools root and the run's environment, the canary gets the root and the explicit commit, and the stages are in-process calls, not child processes", async () => {
  const checkout = gitRepo();
  const toolsRoot = scratch("gb07-installed-");
  const calls = [];
  const loaded = [];
  const load = async (rel) => {
    loaded.push(rel);
    if (rel === "scripts/preflight.mjs") return { main: async (argv, io) => { calls.push(["check", argv, io.cwd]); io.out("checked"); return 0; } };
    if (rel === "scripts/ci/rk-preflight.mjs") return { main: async (argv, deps) => { calls.push(["preflight-crates", argv, deps.root, deps.toolsRoot, deps.env.RK_GATE_RUN]); deps.stdout("compile rows green"); return 0; } };
    if (rel === "scripts/ci/rk-npm-preflight.mjs") return { main: async (argv, deps) => { calls.push(["preflight-npm", argv, deps.root, deps.toolsRoot, deps.env.RK_GATE_RUN]); deps.stderr("npm rows red"); return 1; } };
    if (rel === "scripts/ci/rk-release-canary.mjs") return { runCanary: async (o) => { calls.push(["canary", o.root, o.toolsRoot, o.unit, o.expectSha]); return { code: 0, lines: ["GREEN canary"] }; } };
    throw new Error(`unexpected module ${rel}`);
  };
  const env = { ...process.env, RK_GATE_RUN: "run-1" };
  const opts = { root: checkout.dir, registry: "npm", jobs: 2 };
  const deps = inGateDeps(opts, { toolsRoot, env, load });
  assert.deepEqual(await deps.check(checkout.head), { code: 0, out: "checked" });
  const npm = await deps.preflightMany(["@rightkit/x"], opts);
  assert.deepEqual(npm.get("@rightkit/x"), { code: 1, last: "npm rows red" });
  const crates = await deps.preflightMany(["rightkit-a"], { ...opts, registry: "crates" });
  assert.deepEqual(crates.get("rightkit-a"), { code: 0, last: "compile rows green" });
  assert.deepEqual(await deps.canary("@rightkit/release", checkout.head), { code: 0, last: "GREEN canary" });
  assert.deepEqual(calls, [
    ["check", ["--commit", checkout.head, "--repo", checkout.dir], checkout.dir],
    ["preflight-npm", ["--registry", "npm", "--unit", "@rightkit/x"], checkout.dir, toolsRoot, "run-1"],
    ["preflight-crates", ["--registry", "crates", "--unit", "rightkit-a"], checkout.dir, toolsRoot, "run-1"],
    ["canary", checkout.dir, toolsRoot, "@rightkit/release", checkout.head],
  ]);
  assert.deepEqual([...new Set(loaded)].sort(), ["scripts/ci/rk-npm-preflight.mjs", "scripts/ci/rk-preflight.mjs", "scripts/ci/rk-release-canary.mjs", "scripts/preflight.mjs"], "only the four installed modules are loaded, by path under the tools root");
  // The default tools root is where THIS module is installed, never the root a call is about.
  assert.equal(installedToolsRoot(), REPO_ROOT);
  // The module URL is built with pathToFileURL: a POSIX-spelled `file:///opt/...` is not a valid file URL on Windows (it needs a drive letter), where fileURLToPath refuses it.
  const installedRoot = path.resolve("/opt/rightkit/current");
  assert.equal(installedToolsRoot(pathToFileURL(path.join(installedRoot, "scripts", "ci", "rk-pipeline.mjs")).href), installedRoot);
});

test("[GB-07] a state-changing in-gate call takes the coordinator's lock like the command line does: a live holder refuses it before anything dispatches, and the lock is released after", async () => {
  const checkout = gitRepo();
  const stateFile = path.join(scratch("gb07-lock-"), "state.json");
  const held = acquireLock(`${stateFile}.lock`);
  const deps = { check: async () => assert.fail("a refused call starts nothing") };
  const busy = await runPipeline({ argv: ["dispatch", "rightkit-a", "--sha", checkout.head], root: checkout.dir, stateFile, deps });
  assert.equal(busy.status, 2);
  assert.match(busy.stderr, /another coordinator/);
  releaseLock(`${stateFile}.lock`, { nonce: held.nonce });
  const free = await runPipeline({ argv: ["watch"], root: checkout.dir, stateFile, deps: {} });
  assert.equal(free.status, 0, "nothing pending, lock free: the call runs");
  assert.equal(fs.existsSync(`${stateFile}.lock`), false, "the lock is released after the call");
  // A read-only command needs no lock.
  const held2 = acquireLock(`${stateFile}.lock`);
  const ready = await runPipeline({ argv: ["preflight", "rightkit-a", "--sha", checkout.head], root: checkout.dir, stateFile, deps: { check: async () => ({ code: 0 }), preflightMany: async () => new Map([["rightkit-a", { code: 0, last: "" }]]) } });
  assert.equal(ready.status, 0);
  releaseLock(`${stateFile}.lock`, { nonce: held2.nonce });
});

test("[GB-07] refusals of the in-gate entry: a root inside the managed snapshots root (Mac and Windows spellings), a relative root and a root that is a snapshot used as its own control plane start nothing and say RK_CHECK_ERROR", async () => {
  const dataRoot = scratch("gb07-data-");
  const inner = path.join(dataRoot, "snapshots", "k1"); fs.mkdirSync(inner, { recursive: true });
  const deps = { check: async () => assert.fail("a refused call starts nothing"), preflightMany: async () => assert.fail("a refused call starts nothing") };
  const refused = await runPipeline({ argv: ["preflight", "rightkit-a"], root: inner, env: { ...process.env, RIGHTKIT_DATA_ROOT: dataRoot }, deps });
  assert.equal(refused.status, 2);
  assert.match(refused.stderr, /inside the managed snapshots root.*RK_CHECK_ERROR/s);
  const relative = await runPipeline({ argv: ["preflight", "rightkit-a"], root: "some/checkout", deps });
  assert.equal(relative.status, 2);
  assert.match(relative.stderr, /absolute-path/);
  // Windows spellings are judged by the Windows path rules: a drive-letter root under D:\.rightkit-managed\snapshots, whatever its case.
  const win = await runPipeline({ argv: ["preflight", "rightkit-a"], root: "d:\\.RIGHTKIT-MANAGED\\snapshots\\k9", platform: "win32", env: { ...process.env, RIGHTKIT_DATA_ROOT: "D:\\.rightkit-managed" }, deps });
  assert.equal(win.status, 2);
  assert.match(win.stderr, /inside the managed snapshots root/);
});

test("[GB-07] the command line keeps today's behavior: the wrapper takes the root from where the script is installed, parses the same arguments and exits the same codes", () => {
  const script = path.join(REPO_ROOT, "scripts", "ci", "rk-pipeline.mjs");
  const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: "utf8", cwd: scratch("gb07-cli-cwd-") });
  const usage = run("frobnicate");
  assert.equal(usage.status, 2); assert.match(usage.stderr, /rk-pipeline: command must be ready, preflight, canary, dispatch, watch or loop/);
  const registry = run("loop", "--registry", "pypi");
  assert.equal(registry.status, 2); assert.match(registry.stderr, /--registry must be crates or npm/);
  const noUnits = run("preflight");
  assert.equal(noUnits.status, 2); assert.match(noUnits.stderr, /name at least one unit/);
  const badSha = run("dispatch", "u", "--sha", "abc");
  assert.equal(badSha.status, 2); assert.match(badSha.stderr, /full lowercase commit id/);
  assert.equal(stateFileFor("win32"), "D:\\scratch\\rk-pipeline\\state.json");
  assert.equal(stateFileFor("darwin"), "/Volumes/D/scratch/rk-pipeline/state.json");
});
