import assert from "node:assert/strict";
import test from "node:test";
import { allowsetsFor, cmdLoop, cmdWatch, matchingUploadRuns, parseArgs, registryVersion, satisfies, stateKey, waves } from "./rk-pipeline.mjs";
const SHA = "f".repeat(40);
const index = (...versions) => new Map(versions.map((v) => [v, { yanked: false, cksum: "0".repeat(64) }]));

test("argv normalizes unit lists & refuses unsupported npm", () => {
  assert.deepEqual(parseArgs(["dispatch", "rightkit-daemon rightkit-crash", "rightkit-daemon"]).units, ["rightkit-daemon", "rightkit-crash"]);
  assert.throws(() => parseArgs(["loop", "--registry", "npm"]), /broker-admitted/);
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
