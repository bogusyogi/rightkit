import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { resolveUnit, runUnitGate } from "./unit-gate.mjs";

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
  assert.deepEqual(s.calls, ["check:compile", "compile", "package", "check:release", "qualify", "close"]);
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
  assert.deepEqual(s.calls, ["check:compile", "compile", "package", "check:release", "qualify", "close"]);
  assert.match(errs.join("\n"), /broker-managed host/);
  assert.equal(s.reports.at(-1).stages.release.ok, false);
});

test("release stage: the gate owns pre-flight, broker dispatch, the wait and the readback; it fails closed at every step", async () => {
  const { releaseUnit } = await import("./unit-gate.mjs");
  const artifact = { artifactSha256: "1".repeat(64) };
  const base = { unit, root: "/repo", commit, executor: { kind: "broker" }, env: { RK_GATE_RUN: "x" }, artifact, platform: "darwin", out: () => {}, scratch: tempRoot("unit-release-") };
  const row = { unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", sha256: "2".repeat(64), sha1: "s1", integrity: "sha512-abc" };
  const make = ({ states, runStatus = {}, receiptRows = [row], served = { integrity: "sha512-abc", shasum: "s1" }, artifacts = ["publication-receipt-1-1-1"] } = {}) => {
    const calls = [];
    let reads = 0;
    const run = (cmd, args) => {
      const key = args[0] === "/repo/scripts/ci/rk-pipeline.mjs" ? args[1] : path.basename(cmd) === "gh" ? `gh:${args[0]}` : path.basename(cmd);
      calls.push(key === "gh:run" ? `gh:${args.join(" ")}` : key);
      if (key === "gh:api") return { status: 0, stdout: JSON.stringify({ artifacts: artifacts.map((name) => ({ name })) }) };
      if (key === "gh:run") { const dir = args[args.indexOf("--dir") + 1]; fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "publication-receipt.json"), JSON.stringify({ receipts: receiptRows })); return { status: 0, stdout: "" }; }
      return { status: runStatus[key] ?? 0, stdout: "", stderr: `${key} said no` };
    };
    const readJson = () => ({ dispatches: { k: states[Math.min(reads++, states.length - 1)] } });
    return { calls, run, readJson, registryBytes: async () => served };
  };
  const dispatched = { unit: unit.name, sha: commit, registry: "npm", runId: 10, runAttempt: 1, requestId: "req" };
  const published = { ...dispatched, status: "published", version: unit.version, uploadRunId: 20 };
  // success: order and the reported record
  let f = make({ states: [dispatched, published] });
  const ok = await releaseUnit({ ...base, ...f });
  assert.deepEqual(f.calls, ["preflight", "dispatch", "watch", "gh:api", "gh:run download 20 -R bogusyogi/rightkit --name publication-receipt-1-1-1 --dir " + path.join(base.scratch, "rk-unit-receipts", "20")]);
  assert.equal(ok.registryVerified, true); assert.equal(ok.hostedSha256, row.sha256); assert.equal(ok.uploadRunId, 20); assert.equal(ok.hostedEqualsLocal, false); assert.equal(ok.localArtifactSha256, artifact.artifactSha256);
  // direct host: refused before anything runs
  f = make({ states: [dispatched, published] });
  await assert.rejects(releaseUnit({ ...base, ...f, executor: { kind: "direct" } }), /broker-managed host/);
  assert.deepEqual(f.calls, []);
  // pre-flight red: no dispatch
  f = make({ states: [dispatched, published], runStatus: { preflight: 1 } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /pre-flight.*failed/);
  assert.deepEqual(f.calls, ["preflight"]);
  // dispatch recorded no run: stop, nothing claimed
  f = make({ states: [{ ...dispatched, runId: undefined }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /no dispatch record with a run/);
  assert.deepEqual(f.calls, ["preflight", "dispatch"]);
  // watch ended without a published record (upload never ran)
  f = make({ states: [dispatched, { ...dispatched, status: "failed", conclusion: "upload failure" }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /without a published record \(failed: upload failure\)/);
  // ambiguous receipts
  f = make({ states: [dispatched, published], artifacts: ["publication-receipt-1-1-1", "publication-receipt-1-1-2"] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /2 publication receipt artifact/);
  // receipt row not verified
  f = make({ states: [dispatched, published], receiptRows: [{ ...row, registryVerified: false, outcome: "published-unverified" }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /records no verified publication \(published-unverified\)/);
  // registry serves other bytes
  f = make({ states: [dispatched, published], served: { integrity: "sha512-other", shasum: "s1" } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /different bytes/);
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
  const { spawnSync } = await import("node:child_process");
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
