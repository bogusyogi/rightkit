// GB-07 (BROKER-CONTRACT section 16): unit publication dispatch is a control-plane operation. `releaseUnit` takes the SOURCE root (the snapshot: the carried build and pack steps stay snapshot-bound)
// apart from the CONTROL-PLANE checkout root (the primary checkout, outside the managed snapshots root, where the broker admits uncarried root requests). Dispatch and preflight run the TRUSTED,
// INSTALLED coordinator (the gate's own rk-pipeline, found from the running gate's own location), never a scripts/ci/rk-pipeline.mjs under the snapshot or the checkout; the candidate's commit,
// artifact digest and qualification are explicit inputs of the call, never read from files under the checkout; a retry reconciles by the same commit, tree and digest identity.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { controlPlaneProblem, loadInGatePipeline, releaseUnit, runUnitGate, trustedCoordinator } from "./unit-gate.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const commit = "a".repeat(40), tree = "b".repeat(40);
const unit = { kind: "npm", name: "@rightkit/demo", version: "1.0.0" };
const artifact = { artifactSha256: "1".repeat(64) };
const INSTALLED = path.resolve(HERE, "..", "..", "scripts", "ci", "rk-pipeline.mjs");

test("[GB-07] the trusted coordinator is the running gate's own installed rk-pipeline, found from this module's location with its digest, never from a root or checkout argument", () => {
  const found = trustedCoordinator();
  assert.equal(found.file, INSTALLED);
  assert.equal(found.sha256, createHash("sha256").update(fs.readFileSync(INSTALLED)).digest("hex"));
  assert.throws(() => trustedCoordinator({ exists: () => false }), /installed coordinator/);
  // A module somewhere else resolves ITS own coordinator: the location of the gate is the identity, not the repository being released.
  const elsewhere = trustedCoordinator({ moduleUrl: "file:///opt/rightkit/current/packages/release/unit-gate.mjs", exists: () => true, readFile: () => Buffer.from("x") });
  assert.equal(elsewhere.file, path.resolve("/opt/rightkit/current/scripts/ci/rk-pipeline.mjs"));
});

test("[GB-07] source root and control-plane root are apart: a control-plane checkout inside the source snapshot, inside the managed snapshots root, or a source snapshot used as its own control plane is refused naming RK_CHECK_ERROR; a primary checkout outside both is accepted", () => {
  const snapshots = "/Volumes/D/.rightkit-managed/snapshots";
  const ok = controlPlaneProblem({ root: `${snapshots}/k1`, checkout: "/Users/dev/rightkit", snapshotsRoot: snapshots });
  assert.equal(ok, null);
  assert.match(controlPlaneProblem({ root: `${snapshots}/k1`, checkout: `${snapshots}/k1`, snapshotsRoot: snapshots }), /inside the managed snapshots root.*RK_CHECK_ERROR/s);
  assert.match(controlPlaneProblem({ root: `${snapshots}/k1`, checkout: `${snapshots}/k2`, snapshotsRoot: snapshots }), /inside the managed snapshots root/);
  assert.match(controlPlaneProblem({ root: "/src/snap", checkout: "/src/snap/sub", snapshotsRoot: null }), /inside the source root/);
  assert.match(controlPlaneProblem({ root: "/src/snap", checkout: "relative/path", snapshotsRoot: null }), /absolute/);
  // A direct, unsnapshotted source (root equal to checkout, not under the snapshots root) keeps its single root.
  assert.equal(controlPlaneProblem({ root: "/Users/dev/rightkit", checkout: "/Users/dev/rightkit", snapshotsRoot: snapshots }), null);
  // Windows spellings are judged with the platform's path rules.
  const win = "D:\\.rightkit-managed\\snapshots";
  assert.equal(controlPlaneProblem({ root: `${win}\\k1`, checkout: "D:\\work\\rightkit", snapshotsRoot: win, pathApi: path.win32 }), null);
  assert.match(controlPlaneProblem({ root: `${win}\\k1`, checkout: `${win}\\k1`, snapshotsRoot: win, pathApi: path.win32 }), /inside the managed snapshots root/);
  assert.match(controlPlaneProblem({ root: `${win}\\k1`, checkout: `d:\\.RIGHTKIT-MANAGED\\snapshots\\k9`, snapshotsRoot: win, pathApi: path.win32 }), /inside the managed snapshots root/, "drive and name case do not hide it");
});

function harness({ states, checkoutFiles = {}, env = {} } = {}) {
  const checkout = tempRoot("gb07-checkout-"); const root = tempRoot("gb07-source-");
  for (const [rel, text] of Object.entries(checkoutFiles)) { fs.mkdirSync(path.dirname(path.join(checkout, rel)), { recursive: true }); fs.writeFileSync(path.join(checkout, rel), text); }
  const qualification = path.join(tempRoot("gb07-qualification-"), "qualification.json"); fs.writeFileSync(qualification, JSON.stringify({ commit, tree, artifactSha256: artifact.artifactSha256 }));
  const calls = [];
  const row = { unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", sha256: "2".repeat(64), sha1: "s1" };
  const meta = { schema: 1, registry: "npm", dryRun: false, commit, repository: "bogusyogi/rightkit", runId: "20", runAttempt: "1", candidate: { id: "qualified-10-1", source: "qualified", sourceSha: commit, runId: "10", runAttempt: "1" } };
  let reads = 0;
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args, cwd: options.cwd, env: options.env });
    if (cmd === "git") return { status: 0, stdout: args[0] === "rev-parse" ? `${commit}\n` : "", stderr: "" };
    const base = path.basename(cmd);
    if (base === "gh") {
      if (args[0] === "api") return { status: 0, stdout: JSON.stringify({ artifacts: [{ name: "publication-receipt-10-1-1" }] }) };
      const dir = args[args.indexOf("--dir") + 1]; fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "publication-receipt.json"), JSON.stringify({ ...meta, receipts: [row] })); return { status: 0, stdout: "" };
    }
    return { status: 0, stdout: "", stderr: args[1] === "canary" ? "" : "" };
  };
  const readJson = () => ({ dispatches: { k: states[Math.min(reads++, states.length - 1)] } });
  const base = { unit, root, checkout, commit, tree, executor: { kind: "broker" }, env: { RK_GATE_RUN: "x", ...env }, artifact, qualification, platform: "darwin", out: () => {}, scratch: tempRoot("gb07-scratch-"), run, readJson, registryBytes: async () => ({ shasum: "s1", sha256: "2".repeat(64) }) };
  return { base, calls, checkout, root, qualification };
}
const dispatched = { unit: unit.name, sha: commit, registry: "npm", runId: 10, runAttempt: 1, requestId: "req", tree, artifactSha256: artifact.artifactSha256 };
const published = { ...dispatched, status: "published", version: unit.version, uploadRunId: 20 };
const coordinatorCalls = (calls) => calls.filter((c) => c.cmd === process.execPath);

test("[GB-07] dispatch and preflight execute the gate's installed coordinator from the control-plane checkout; a modified or planted scripts/ci/rk-pipeline.mjs under the checkout is never run, and the commit and qualification ride the call as explicit inputs", async () => {
  const h = harness({ states: [dispatched, published], checkoutFiles: { "scripts/ci/rk-pipeline.mjs": "throw new Error('a planted coordinator ran');\n" } });
  const done = await releaseUnit(h.base);
  assert.equal(done.registryVerified, true);
  const ran = coordinatorCalls(h.calls);
  assert.deepEqual(ran.map((c) => c.args[1]), ["canary", "preflight", "dispatch", "watch"]);
  for (const c of ran) {
    assert.equal(c.args[0], INSTALLED, "the installed coordinator, whatever the checkout holds");
    assert.notEqual(c.args[0], path.join(h.checkout, "scripts", "ci", "rk-pipeline.mjs"));
    assert.equal(c.cwd, h.checkout, "the control-plane checkout is the working directory");
  }
  for (const c of ran.filter((x) => ["canary", "preflight", "dispatch"].includes(x.args[1]))) { const at = c.args.indexOf("--sha"); assert.ok(at > 1 && c.args[at + 1] === commit, `${c.args[1]} carries the explicit commit`); }
  const dispatch = ran.find((c) => c.args[1] === "dispatch");
  assert.ok(dispatch.args.includes("--qualification") && dispatch.args[dispatch.args.indexOf("--qualification") + 1] === h.qualification, "the qualification receipt is an explicit input");
  assert.equal(done.coordinator.file, INSTALLED); assert.match(done.coordinator.sha256, /^[0-9a-f]{64}$/);
});

test("[GB-07] negative: a control-plane checkout inside the managed snapshots root or the source snapshot starts nothing; HEAD at another commit starts nothing", async () => {
  const dataRoot = tempRoot("gb07-data-"); const snapshots = path.join(dataRoot, "snapshots"); fs.mkdirSync(snapshots, { recursive: true });
  const inSnapshots = harness({ states: [dispatched, published], env: { RIGHTKIT_DATA_ROOT: dataRoot } });
  const inner = path.join(snapshots, "k1"); fs.mkdirSync(inner, { recursive: true });
  await assert.rejects(releaseUnit({ ...inSnapshots.base, root: inner, checkout: inner }), /inside the managed snapshots root/);
  assert.deepEqual(inSnapshots.calls, [], "nothing ran");
  const nested = harness({ states: [dispatched, published] });
  const sub = path.join(nested.root, "sub"); fs.mkdirSync(sub);
  await assert.rejects(releaseUnit({ ...nested.base, checkout: sub }), /inside the source root/);
  assert.deepEqual(nested.calls, []);
  const moved = harness({ states: [dispatched, published] });
  const run = (cmd, args, options) => (cmd === "git" && args[0] === "rev-parse" ? { status: 0, stdout: `${"c".repeat(40)}\n` } : moved.base.run(cmd, args, options));
  await assert.rejects(releaseUnit({ ...moved.base, run }), /not at the qualified commit/);
  assert.deepEqual(coordinatorCalls(moved.calls), []);
});

test("[GB-07] a retry reconciles by the same commit, tree and artifact digest: a record for the commit that names another tree or digest (or none) is ambiguous, never adopted", async () => {
  for (const [label, record, why] of [["another tree", { ...dispatched, tree: "d".repeat(40) }, /tree/], ["another digest", { ...dispatched, artifactSha256: "9".repeat(64) }, /artifact digest/], ["no identity", (({ tree: _t, artifactSha256: _a, ...rest }) => rest)(dispatched), /identity/]]) {
    const h = harness({ states: [record, { ...record, status: "published", version: unit.version, uploadRunId: 20 }] });
    await assert.rejects(releaseUnit(h.base), why, label);
  }
  const good = harness({ states: [dispatched, published] });
  assert.equal((await releaseUnit(good.base)).uploadRunId, 20);
});

// ---------------------------------------------------------------------------------------------------------------------
// GB-07, the in-gate-process entry (BROKER-CONTRACT section 16: "either in the gate's own process or as a registered pinned adapter").
// ---------------------------------------------------------------------------------------------------------------------
const descriptor = { file: INSTALLED, sha256: createHash("sha256").update(fs.readFileSync(INSTALLED)).digest("hex") };
const inGate = (h, over = {}) => {
  const entries = [];
  const runPipeline = async (call) => {
    entries.push(call);
    return { status: call.argv[0] === "canary" ? 2 : 0, stdout: "", stderr: call.argv[0] === "canary" ? "rk-pipeline: @rightkit/demo: no pipeline canary for this unit\n" : "" };
  };
  return { entries, pipeline: { runPipeline, coordinator: descriptor, ...over } };
};

test("[GB-07] the in-gate entry: dispatch, canary, preflight and watch run as in-process calls over the explicit root, commit, qualification and state; no child process is started from any path, and a planted checkout coordinator is never reached", async () => {
  const h = harness({ states: [dispatched, published], checkoutFiles: { "scripts/ci/rk-pipeline.mjs": "throw new Error('a planted coordinator ran');\n" } });
  const g = inGate(h);
  const done = await releaseUnit({ ...h.base, pipeline: g.pipeline });
  assert.equal(done.registryVerified, true);
  assert.deepEqual(coordinatorCalls(h.calls), [], "no child process runs the coordinator: it is the gate's own process");
  assert.deepEqual(g.entries.map((e) => e.argv[0]), ["canary", "preflight", "dispatch", "watch"]);
  for (const e of g.entries) {
    assert.equal(e.root, h.checkout, "the control-plane checkout is the root the coordinator judges, not the installed coordinator's own checkout and not the snapshot");
    assert.notEqual(e.root, h.root);
    assert.equal(e.platform, "darwin");
    assert.equal(e.stateFile, "/Volumes/D/scratch/rk-pipeline/state.json");
    assert.equal(e.env.RK_GATE_RUN, "x", "the release run's environment rides the call");
  }
  for (const e of g.entries.filter((x) => ["canary", "preflight", "dispatch"].includes(x.argv[0]))) { const at = e.argv.indexOf("--sha"); assert.ok(at > 0 && e.argv[at + 1] === commit, `${e.argv[0]} carries the explicit commit`); }
  const dispatch = g.entries.find((e) => e.argv[0] === "dispatch");
  assert.equal(dispatch.argv[dispatch.argv.indexOf("--qualification") + 1], h.qualification, "the qualification receipt is an explicit input");
  assert.deepEqual(done.coordinator, descriptor);
});

test("[GB-07] the in-gate entry keeps every control-plane refusal: a checkout inside the managed snapshots root or the source snapshot, and HEAD at another commit, start nothing", async () => {
  const dataRoot = tempRoot("gb07-ig-data-"); const snapshots = path.join(dataRoot, "snapshots"); fs.mkdirSync(snapshots, { recursive: true });
  const a = harness({ states: [dispatched, published], env: { RIGHTKIT_DATA_ROOT: dataRoot } });
  const inner = path.join(snapshots, "k1"); fs.mkdirSync(inner, { recursive: true });
  const ga = inGate(a);
  await assert.rejects(releaseUnit({ ...a.base, root: inner, checkout: inner, pipeline: ga.pipeline }), /inside the managed snapshots root/);
  assert.deepEqual(ga.entries, []);
  const b = harness({ states: [dispatched, published] }); const gb = inGate(b);
  const sub = path.join(b.root, "sub"); fs.mkdirSync(sub);
  await assert.rejects(releaseUnit({ ...b.base, checkout: sub, pipeline: gb.pipeline }), /inside the source root/);
  assert.deepEqual(gb.entries, []);
  const c = harness({ states: [dispatched, published] }); const gc = inGate(c);
  const run = (cmd, args, options) => (cmd === "git" && args[0] === "rev-parse" ? { status: 0, stdout: `${"c".repeat(40)}\n` } : c.base.run(cmd, args, options));
  await assert.rejects(releaseUnit({ ...c.base, run, pipeline: gc.pipeline }), /not at the qualified commit/);
  assert.deepEqual(gc.entries, []);
  // A coordinator that sits inside the managed snapshots root or the source snapshot is the candidate's own bytes, never the trusted installed one.
  const d = harness({ states: [dispatched, published] });
  for (const where of [path.join(snapshots, "k2", "scripts", "ci", "rk-pipeline.mjs"), path.join(d.root, "scripts", "ci", "rk-pipeline.mjs")]) {
    const g = inGate(d, { coordinator: { file: where, sha256: descriptor.sha256 } });
    await assert.rejects(releaseUnit({ ...d.base, env: { ...d.base.env, RIGHTKIT_DATA_ROOT: dataRoot }, pipeline: g.pipeline }), /inside the (managed snapshots root|source snapshot)/, where);
    assert.deepEqual(g.entries, []);
  }
  await assert.rejects(releaseUnit({ ...d.base, pipeline: { coordinator: descriptor } }), /no runPipeline/);
});

test("[GB-07] loadInGatePipeline imports the installed coordinator into the gate process: pinned digest, no coordinator under the managed snapshots root or the source snapshot, bytes that change under the gate refused, a module without the entry refused", async () => {
  const real = await loadInGatePipeline();
  assert.equal(typeof real.runPipeline, "function", "the real installed rk-pipeline exports its in-gate entry");
  assert.deepEqual(real.coordinator, descriptor);
  const seen = [];
  const fake = (extra = {}) => ({ coordinator: descriptor, importModule: async (url) => { seen.push(url); return { runPipeline: async () => ({ status: 0 }) }; }, ...extra });
  const ok = await loadInGatePipeline(fake({ expectedSha256: descriptor.sha256 }));
  assert.equal(seen[0], new URL(`file://${INSTALLED}`).href, "imported by the installed file's own URL");
  assert.equal(typeof ok.runPipeline, "function");
  await assert.rejects(loadInGatePipeline(fake({ expectedSha256: "0".repeat(64) })), /not the pinned 000000000000.*registered/s);
  await assert.rejects(loadInGatePipeline(fake({ coordinator: { file: INSTALLED, sha256: "short" } })), /sha256 of its bytes/);
  await assert.rejects(loadInGatePipeline(fake({ coordinator: { ...descriptor, sha256: "1".repeat(64) } })), /changed after its digest was taken/);
  let reads = 0;
  await assert.rejects(loadInGatePipeline(fake({ readFile: () => Buffer.from(reads++ === 0 ? fs.readFileSync(INSTALLED) : "swapped") })), /changed while it was loaded/);
  await assert.rejects(loadInGatePipeline(fake({ importModule: async () => ({}) })), /no in-gate entry/);
  const snapshots = "/Volumes/D/.rightkit-managed/snapshots";
  await assert.rejects(loadInGatePipeline(fake({ coordinator: { file: `${snapshots}/k1/scripts/ci/rk-pipeline.mjs`, sha256: descriptor.sha256 }, snapshotsRoot: snapshots })), /inside the managed snapshots root/);
  await assert.rejects(loadInGatePipeline(fake({ root: path.dirname(path.dirname(path.dirname(INSTALLED))) })), /inside the source snapshot/);
  // Windows spellings: a coordinator under D:\.rightkit-managed\snapshots is refused whatever its case.
  await assert.rejects(loadInGatePipeline(fake({ coordinator: { file: "d:\\.RIGHTKIT-MANAGED\\snapshots\\k1\\scripts\\ci\\rk-pipeline.mjs", sha256: descriptor.sha256 }, snapshotsRoot: "D:\\.rightkit-managed\\snapshots", pathApi: path.win32 })), /inside the managed snapshots root/);
});
