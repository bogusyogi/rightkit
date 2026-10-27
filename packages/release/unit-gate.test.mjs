import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { qualificationNonce } from "./provenance-verify.mjs";
import { readBrokerReceipt, resolveUnit, runUnitGate } from "./unit-gate.mjs";

const commit = "a".repeat(40), tree = "b".repeat(40);
const unit = { kind: "npm", name: "@rightkit/demo", version: "1.0.0" };
function setup() {
  const root = tempRoot("unit-gate-");
  const artifactFile = path.join(root, "artifact.tgz"); fs.writeFileSync(artifactFile, "artifact");
  const artifactSha256 = createHash("sha256").update("artifact").digest("hex");
  const calls = [], reports = [];
  const result = { ...unit, commit, artifactFile, artifactSha256 };
  const owner = {
    compileLocalUnit: async (_kind, _args, { env }) => { assert.ok(env.RK_GATE_RUN); calls.push("compile"); return {}; },
    packageLocalUnit: async (_state, { env }) => { assert.ok(env.RK_GATE_RUN); calls.push("package"); return result; },
    qualifyLocalUnit: async (_state, { env }) => { assert.ok(env.RK_GATE_RUN); calls.push("qualify"); return { ...result, qualifiedOnly: true, evidencePath: "gate.json" }; },
    closeLocalUnit: async () => calls.push("close"),
  };
  const args = { selector: "npm:@rightkit/demo", to: "qualify", root, commonDir: root, commit, tree, executor: { kind: "broker" }, env: {}, platform: "darwin", resolve: () => unit, check: async (tier) => { calls.push(`check:${tier}`); return 0; }, loadOwner: async () => owner, out: () => {}, err: () => {}, writeReport: (r) => reports.push(structuredClone(r)) };
  return { args, calls, reports, owner, artifactFile };
}

test("unit stages run in order, stop at requested stage and recheck every invocation", async () => {
  const s = setup();
  assert.equal(await runUnitGate({ ...s.args, to: "package" }), 0);
  assert.deepEqual(s.calls, ["check:compile", "compile", "package", "close"]);
  s.calls.length = 0;
  assert.equal(await runUnitGate(s.args), 0);
  assert.deepEqual(s.calls, ["check:release", "compile", "package", "qualify", "close"]);
  assert.equal(s.reports.at(-1).unit.name, unit.name);
  assert.equal(s.reports.at(-1).tree, tree);
  assert.match(s.reports.at(-1).runtime, /^[0-9a-f]{64}$/);
});

test("failure, altered artifacts and release never run later stages", async () => {
  const s = setup();
  assert.equal(await runUnitGate({ ...s.args, check: async () => 1 }), 1);
  assert.deepEqual(s.calls, []);
  s.owner.packageLocalUnit = async () => { fs.writeFileSync(s.artifactFile, "changed"); return { ...unit, commit, artifactFile: s.artifactFile, artifactSha256: "0".repeat(64) }; };
  assert.equal(await runUnitGate(s.args), 1);
  assert.ok(!s.calls.includes("qualify"));
  s.calls.length = 0;
  // Release on a direct (unbrokered) host refuses inside the release stage, after the local stages, with no upload started.
  s.owner.packageLocalUnit = async () => { s.calls.push("package"); return { ...unit, commit, artifactFile: s.artifactFile, artifactSha256: createHash("sha256").update("artifact").digest("hex") }; };
  fs.writeFileSync(s.artifactFile, "artifact");
  const errs = [];
  assert.equal(await runUnitGate({ ...s.args, to: "release", executor: { kind: "direct" }, err: (l) => errs.push(l) }), 1);
  assert.deepEqual(s.calls, ["check:release", "compile", "package", "qualify", "close"]);
  assert.match(errs.join("\n"), /broker-managed host/);
  assert.equal(s.reports.at(-1).stages.release.ok, false);
});

test("release stage: the gate owns pre-flight, broker dispatch, the wait and the readback; it fails closed at every step", async () => {
  const { releaseUnit } = await import("./unit-gate.mjs");
  const artifact = { artifactSha256: "1".repeat(64) };
  const qualification = path.join(tempRoot("unit-qualification-"), "qualification.json");
  fs.writeFileSync(qualification, "{}");
  const base = { unit, root: "/repo", commit, executor: { kind: "broker" }, env: { RK_GATE_RUN: "x" }, artifact, qualification, platform: "darwin", out: () => {}, scratch: tempRoot("unit-release-") };
  const row = { unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", sha256: "2".repeat(64), sha1: "s1" };
  // As upload.mjs writes it: runId/runAttempt are the upload run's; `candidate` names the qualification run it consumed.
  const meta = { schema: 1, registry: "npm", dryRun: false, commit, repository: "bogusyogi/rightkit", runId: "20", runAttempt: "1", promotionCommit: commit, candidate: { id: "qualified-10-1", source: "qualified", sourceSha: commit, runId: "10", runAttempt: "1" } };
  const make = ({ states, runStatus = {}, receiptRows = [row], served = { shasum: "s1", sha256: "2".repeat(64) }, artifacts = ["publication-receipt-10-1-1"], receiptMeta = meta, canaryError = false } = {}) => {
    const calls = [];
    const dispatches = [];
    const f = { canaryError, dispatches };
    let reads = 0;
    const run = (cmd, args) => {
      const key = args[0] === "/repo/scripts/ci/rk-pipeline.mjs" ? args[1] : path.basename(cmd) === "gh" ? `gh:${args[0]}` : path.basename(cmd);
      calls.push(key === "gh:run" ? `gh:${args.join(" ")}` : key);
      if (key === "dispatch") dispatches.push(args.slice(1));
      if (key === "gh:api") return { status: 0, stdout: JSON.stringify({ artifacts: artifacts.map((name) => ({ name })) }) };
      if (key === "gh:run") { const dir = args[args.indexOf("--dir") + 1]; fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "publication-receipt.json"), JSON.stringify({ ...receiptMeta, receipts: receiptRows })); return { status: 0, stdout: "" }; }
      return { status: runStatus[key] ?? 0, stdout: "", stderr: runStatus[key] === 2 && key === "canary" && !f.canaryError ? `${unit.name}: no pipeline canary for this unit` : `${key} said no` };
    };
    const readJson = () => ({ dispatches: { k: states[Math.min(reads++, states.length - 1)] } });
    return { ...f, calls, run, readJson, registryBytes: async () => served };
  };
  const dispatched = { unit: unit.name, sha: commit, registry: "npm", runId: 10, runAttempt: 1, requestId: "req" };
  const published = { ...dispatched, status: "published", version: unit.version, uploadRunId: 20 };
  // success: order and the reported record
  let f = make({ states: [dispatched, published] });
  const ok = await releaseUnit({ ...base, ...f });
  assert.deepEqual(f.calls.slice(0, 5), ["canary", "preflight", "dispatch", "watch", "gh:api"]);
  assert.deepEqual(f.dispatches[0], ["dispatch", unit.name, "--registry", "npm", "--qualification", qualification], "the dispatch carries the gate's qualification receipt");
  // no qualification receipt: nothing runs, not even the canary
  const g = make({ states: [dispatched, published] });
  await assert.rejects(releaseUnit({ ...base, ...g, qualification: undefined }), /qualification receipt/);
  await assert.rejects(releaseUnit({ ...base, ...g, qualification: path.join(path.dirname(qualification), "missing.json") }), /qualification receipt/);
  assert.deepEqual(g.calls, []);
  assert.match(f.calls[5], /^gh:run download 20 -R bogusyogi\/rightkit --name publication-receipt-10-1-1 --dir /);
  assert.equal(ok.registryVerified, true); assert.equal(ok.hostedSha256, row.sha256); assert.equal(ok.uploadRunId, 20); assert.equal(ok.hostedEqualsLocal, false); assert.equal(ok.localArtifactSha256, artifact.artifactSha256);
  // direct host: refused before anything runs
  f = make({ states: [dispatched, published] });
  await assert.rejects(releaseUnit({ ...base, ...f, executor: { kind: "direct" } }), /broker-managed host/);
  assert.deepEqual(f.calls, []);
  // canary regression (10) or incomplete (11): no pre-flight; a unit without a canary (2) proceeds
  f = make({ states: [dispatched, published], runStatus: { canary: 10 } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /canary.*failed \(exit 10\)/);
  assert.deepEqual(f.calls, ["canary"]);
  f = make({ states: [dispatched, published], runStatus: { canary: 2, preflight: 1 } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /pre-flight.*failed/);
  assert.deepEqual(f.calls, ["canary", "preflight"]);
  // exit 2 from an internal canary error (no "no pipeline canary" answer) is never read as "no canary"
  f = make({ states: [dispatched, published], runStatus: { canary: 2 }, canaryError: true });
  await assert.rejects(releaseUnit({ ...base, ...f }), /canary.*failed \(exit 2\)/);
  assert.deepEqual(f.calls, ["canary"]);
  // pre-flight red: no dispatch
  f = make({ states: [dispatched, published], runStatus: { preflight: 1 } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /pre-flight.*failed/);
  assert.deepEqual(f.calls, ["canary", "preflight"]);
  // dispatch recorded no run: stop, nothing claimed
  f = make({ states: [{ ...dispatched, runId: undefined }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /no dispatch record with a run/);
  assert.deepEqual(f.calls, ["canary", "preflight", "dispatch"]);
  // watch ended without a published record (upload never ran)
  f = make({ states: [dispatched, { ...dispatched, status: "failed", conclusion: "upload failure" }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /without a published record \(failed: upload failure\)/);
  // ambiguous receipts, and a receipt of another qualification run
  f = make({ states: [dispatched, published], artifacts: ["publication-receipt-10-1-1", "publication-receipt-10-1-2"] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /2 publication receipt artifact/);
  f = make({ states: [dispatched, published], artifacts: ["publication-receipt-99-1-1"] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /0 publication receipt artifact/);
  f = make({ states: [dispatched, published], receiptMeta: { ...meta, commit: "c".repeat(40) } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /is not that run's receipt for qualification run 10 attempt 1/);
  f = make({ states: [dispatched, published], receiptMeta: { ...meta, runId: "10" } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /is not that run's receipt/, "a receipt naming the qualification run as its own run is not the upload run's");
  f = make({ states: [dispatched, published], receiptMeta: { ...meta, candidate: { ...meta.candidate, runId: "11" } } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /is not that run's receipt/, "a candidate of another qualification run");
  f = make({ states: [dispatched, published], receiptMeta: { ...meta, candidate: { ...meta.candidate, source: "local" } } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /is not that run's receipt/, "only a qualified candidate");
  f = make({ states: [dispatched, published], receiptRows: [row, row] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /holds 2 row/);
  // receipt row not verified
  f = make({ states: [dispatched, published], receiptRows: [{ ...row, registryVerified: false, outcome: "published-unverified" }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /records no verified publication \(published-unverified\)/);
  // registry serves other bytes (shasum or tarball sha256)
  f = make({ states: [dispatched, published], served: { shasum: "other", sha256: "2".repeat(64) } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /different bytes/);
  f = make({ states: [dispatched, published], served: { shasum: "s1", sha256: "3".repeat(64) } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /different bytes/);
  // two dispatch records for the same unit and commit are ambiguous
  f = make({ states: [dispatched, published] });
  f.readJson = () => ({ dispatches: { a: dispatched, b: dispatched } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /2 dispatch records/);
});

test("unit selection requires grant and canonical matching publishable manifest", () => {
  const root = tempRoot("unit-select-");
  const dir = path.join(root, "packages", "demo"); fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: unit.name, version: unit.version }));
  assert.deepEqual(resolveUnit("npm:@rightkit/demo", root, { granted: () => true }), unit);
  assert.throws(() => resolveUnit("npm:@rightkit/demo", root, { granted: () => false }));
  assert.throws(() => resolveUnit("npm:../demo", root, { granted: () => true }));
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: "@rightkit/other", version: unit.version }));
  assert.throws(() => resolveUnit("npm:@rightkit/demo", root, { granted: () => true }));
});

test("`--unit` after `--` belongs to right-release, not the gate: an app's dev run is not read as a unit run", async (t) => {
  const { main } = await import("./bin/rightkit-gate.mjs");
  const { mkdtempSync, rmSync, writeFileSync } = await import("node:fs");
  const os = await import("node:os");
  const { spawnSync } = await import("./background-process.mjs");
  const repo = mkdtempSync(path.join(os.tmpdir(), "rk-unit-sep-"));
  t.after(() => rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@example.invalid"); git("config", "user.name", "t");
  writeFileSync(path.join(repo, "README.md"), "x\n"); git("add", "-A"); git("commit", "-q", "-m", "fixture");
  const io = (sink) => ({ err: (l) => sink.push(l), out: () => {}, cwd: repo });
  // Before the separator: the gate's flag; a malformed unit run is refused as one.
  const unitErr = [];
  assert.equal(await main(["--to", "dev", "--unit", "x", "--extra"], io(unitErr)), 2);
  assert.ok(unitErr.some((l) => /unit stages take only/.test(l)), unitErr.join("\n"));
  // After the separator: right-release's argument; the ordinary --to path runs and refuses for its own reasons, never as a unit run.
  const devErr = [];
  const code = await main(["--to", "dev", "--", "--unit", "x"], io(devErr));
  assert.ok(!devErr.some((l) => /unit stages take only|--unit requires --to/.test(l)), devErr.join("\n"));
  assert.notEqual(code, 2, devErr.join("\n"));
});

test("writeQualification binds the passed local stages and the one artifact sha256 into <bundle>/qualification.json; a missing stage refuses", async (t) => {
  const { writeQualification, QUALIFICATION_SCHEMA } = await import("./unit-gate.mjs");
  const dir = tempRoot("unit-qualification-write-");
  const artifact = { artifactFile: path.join(dir, "rightkit-release-0.6.9.tgz"), artifactSha256: "a".repeat(64) };
  const ok = { ok: true, artifactFile: artifact.artifactFile, artifactSha256: artifact.artifactSha256 };
  const signed = (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id }, signature: { alg: "ES256", keyId: "k", value: "ab" } } });
  const report = { stages: { check: { ok: true }, compile: { ok: true, receipts: [signed("job-c")] }, package: { ...ok, receipts: [signed("job-p")] }, qualify: ok } };
  const written = [];
  const file = writeQualification({ report, artifact, unit, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: (f, text) => written.push([f, JSON.parse(text)]) });
  assert.equal(file, path.join(dir, "qualification.json"));
  const [, receipt] = written[0];
  assert.equal(receipt.schema, QUALIFICATION_SCHEMA);
  assert.deepEqual(receipt.unit, { kind: unit.kind, name: unit.name, version: unit.version });
  assert.equal(receipt.commit, commit); assert.equal(receipt.tree, "b".repeat(40)); assert.equal(receipt.executor, "broker");
  assert.equal(receipt.artifactSha256, "a".repeat(64));
  assert.deepEqual(Object.keys(receipt.stages), ["check", "compile", "package", "qualify"]);
  assert.equal(receipt.stages.qualify.artifactSha256, "a".repeat(64));
  assert.deepEqual(receipt.stages.compile.receipts.map((r) => r.id), ["job-c"], "the broker's signed receipts ride in the qualification");
  assert.deepEqual(receipt.stages.package.receipts.map((r) => r.id), ["job-p"]);
  const without = (stage, receipts) => ({ stages: { ...report.stages, [stage]: { ...report.stages[stage], receipts } } });
  for (const [stage, receipts, shown] of [["compile", undefined, "none"], ["package", [], "0"], ["compile", Array.from({ length: 65 }, (_, i) => ({ id: `x${i}` })), "65"]]) {
    assert.throws(() => writeQualification({ report: without(stage, receipts), artifact, unit, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: () => {} }), new RegExp(`signed broker receipts of the ${stage} stage \\(found ${shown}\\)`));
  }
  assert.throws(() => writeQualification({ report: without("compile", [{ id: "big", pad: "x".repeat(60_000) }]), artifact, unit, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: () => {} }), /exceeds the workflow dispatch input limit/);
  assert.throws(() => writeQualification({ report: { stages: { ...report.stages, qualify: { ok: false } } }, artifact, unit, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: () => {} }), /stage qualify ok/);
  assert.throws(() => writeQualification({ report, artifact: { artifactFile: artifact.artifactFile }, unit, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: () => {} }), /qualified artifact/);
  void t;
});

test("a release run sends each compile and package broker job a nonce bound to commit, tree, unit and stage, fetches those jobs' signed receipts and embeds them in the qualification", async () => {
  const s = setup();
  const envs = {};
  const nonce = (stage) => qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage });
  const stageCommands = () => ({ compile: [{ kind: "cargo", argvs: [["fmt"]] }, { kind: "cargo", argvs: [["test"]] }], package: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["pack"]] }] });
  const requests = { "c-1": { kind: "cargo", requestedArgv: ["fmt"] }, "c-2": { kind: "cargo", requestedArgv: ["test"] }, "p-1": { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["pack"] }, "x-1": { kind: "cargo", requestedArgv: ["metadata"] } };
  const fetched = [];
  const fetchReceipt = (id) => { fetched.push(id); return { id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, request: requests[id] }, signature: { alg: "ES256", keyId: "k", value: "ab" } } }; };
  let ids = ["c-1", "c-2", "c-1"];
  s.owner.compileLocalUnit = async (_kind, _args, { env }) => { envs.compile = env.RIGHTKIT_ATTEST_NONCE; return {}; };
  s.owner.localUnitCompileRequests = () => ids;
  const packaged = { ...unit, commit, artifactFile: s.artifactFile, artifactSha256: createHash("sha256").update("artifact").digest("hex") };
  s.owner.packageLocalUnit = async (_state, { env }) => { envs.package = env.RIGHTKIT_ATTEST_NONCE; return { ...packaged, brokerRequests: ["p-1"] }; };
  s.owner.qualifyLocalUnit = async (_state, { env }) => { envs.qualify = env.RIGHTKIT_ATTEST_NONCE; return { ...packaged, qualifiedOnly: true, evidencePath: "gate.json" }; };
  fs.writeFileSync(s.artifactFile, "artifact");
  let written;
  const releaser = async ({ qualification }) => { written = JSON.parse(fs.readFileSync(qualification, "utf8")); return { registry: "npm" }; };
  const run = (over = {}) => runUnitGate({ ...s.args, to: "release", fetchReceipt, stageCommands, releaser, ...over });
  assert.equal(await run(), 0);
  assert.equal(envs.compile, nonce("compile")); assert.equal(envs.package, nonce("package"));
  assert.notEqual(envs.compile, envs.package);
  assert.match(envs.compile, /^[0-9a-f]{64}$/);
  assert.deepEqual(fetched, ["c-1", "c-2", "p-1"], "duplicates collapse; one fetch per job");
  assert.deepEqual(written.stages.compile.receipts.map((r) => r.id), ["c-1", "c-2"]);
  assert.deepEqual(written.stages.package.receipts.map((r) => r.id), ["p-1"]);
  assert.equal(written.stages.qualify.receipts, undefined);
  // A job that is none of the stage's commands is not carried.
  ids = ["c-1", "x-1", "c-2"];
  assert.equal(await run(), 0);
  assert.deepEqual(written.stages.compile.receipts.map((r) => r.id), ["c-1", "c-2"]);
  // Every command of the stage owes a receipt: one required job missing refuses before any dispatch, naming the command.
  ids = ["c-1"];
  const errs = [];
  assert.equal(await run({ releaser: async () => assert.fail("not released"), err: (l) => errs.push(l) }), 1);
  assert.match(errs.join("\n"), /compile stage's cargo test has no broker job receipt \(1 required command\(s\) missing\)/);
  // A job the broker cannot report fails the stage: nothing unauthenticated is released.
  ids = ["c-1", "c-2"]; errs.length = 0;
  assert.equal(await run({ fetchReceipt: () => { throw new Error("no signed provenance receipt"); }, releaser: async () => assert.fail("not released"), err: (l) => errs.push(l) }), 1);
  assert.match(errs.join("\n"), /no signed provenance receipt/);
  // A release with no broker job ids at all refuses before dispatch.
  ids = []; errs.length = 0;
  assert.equal(await run({ releaser: async () => assert.fail("not released"), err: (l) => errs.push(l) }), 1);
  assert.match(errs.join("\n"), /compile stage's cargo fmt has no broker job receipt/);
  // Runs that do not head to release set no nonce and fetch nothing: what the stages do is unchanged.
  Object.keys(envs).forEach((k) => delete envs[k]); fetched.length = 0;
  assert.equal(await runUnitGate({ ...s.args, to: "qualify", fetchReceipt, stageCommands }), 0);
  assert.deepEqual([envs.compile, envs.package, envs.qualify], [undefined, undefined, undefined]);
  assert.deepEqual(fetched, []);
});

test("readBrokerReceipt reduces `rustright status <id>` to the id and signed envelope, and refuses anything unsigned or foreign", () => {
  const prov = { schema: "rustright.provenance.v1", payload: { jobId: "j" }, signature: { alg: "ES256", keyId: "k", value: "ab" } };
  const run = (stdout, status = 0) => () => ({ status, stdout });
  assert.deepEqual(readBrokerReceipt("j", { run: run(JSON.stringify({ type: "STATUS", id: "j", receipt: { id: "j", state: "SUCCEEDED", provenance: prov } })) }), { id: "j", provenance: prov });
  assert.throws(() => readBrokerReceipt("j", { run: run("", 1) }), /cannot report job j/);
  assert.throws(() => readBrokerReceipt("j", { run: run("not json") }), /not JSON/);
  assert.throws(() => readBrokerReceipt("j", { run: run(JSON.stringify({ receipt: { id: "other", provenance: prov } })) }), /no signed provenance receipt/);
  assert.throws(() => readBrokerReceipt("j", { run: run(JSON.stringify({ receipt: { id: "j" } })) }), /no signed provenance receipt/);
});
