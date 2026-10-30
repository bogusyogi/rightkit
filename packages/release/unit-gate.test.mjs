import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { qualificationNonce } from "./provenance-verify.mjs";
import { AGENT_SHIM_MARKER } from "./broker-client.mjs";
import { readBrokerReceipt, releaseUnit, resolveUnit, runUnitGate } from "./unit-gate.mjs";

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
  const args = { selector: "npm:@rightkit/demo", to: "qualify", root, commonDir: root, commit, tree, executor: { kind: "direct" }, env: {}, platform: "darwin", resolve: () => unit, check: async (tier) => { calls.push(`check:${tier}`); return 0; }, loadOwner: async () => owner, out: () => {}, err: () => {}, writeReport: (r) => reports.push(structuredClone(r)) };
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
  // GB-45: the uploader's verified reconciliation of an earlier attempt's IDENTICAL bytes is a successful terminal result, not a failed publication.
  const reconciled = { ...row, outcome: "published-by-earlier-attempt", publishedBy: "earlier-attempt", earlierAttempt: 1, earlierOutcome: "pending" };
  f = make({ states: [dispatched, published], receiptRows: [reconciled] });
  const reusedOk = await releaseUnit({ ...base, ...f });
  assert.equal(reusedOk.registryVerified, true); assert.equal(reusedOk.hostedSha256, "2".repeat(64), "bound to the receipt's hash, which the registry readback confirmed");
  // ...but only with the same bytes, a verified row, a named provenance and the one row: everything else still refuses.
  f = make({ states: [dispatched, published], receiptRows: [reconciled], served: { shasum: "s1", sha256: "3".repeat(64) } });
  await assert.rejects(releaseUnit({ ...base, ...f }), /different bytes/, "DIFFERENT bytes on the registry");
  f = make({ states: [dispatched, published], receiptRows: [{ ...reconciled, registryVerified: false }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /records no verified publication \(published-by-earlier-attempt\): it is not registry-verified/, "unverified");
  f = make({ states: [dispatched, published], receiptRows: [{ ...reconciled, publishedBy: undefined }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /provenance is ambiguous/, "ambiguous provenance");
  f = make({ states: [dispatched, published], receiptRows: [{ ...reconciled, outcome: "already-exists" }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /outcome is "already-exists"/, "a generic already-exists answer is never success");
  f = make({ states: [dispatched, published], receiptRows: [] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /holds 0 row/, "missing row");
  f = make({ states: [dispatched, published], receiptRows: [{ ...reconciled, sha256: undefined }] });
  await assert.rejects(releaseUnit({ ...base, ...f }), /carries no sha256/);
  // Producer and consumer share one definition.
  const { SUCCESSFUL_PUBLICATION_OUTCOMES, PUBLISHED_BY_EARLIER_ATTEMPT } = await import("./publication-outcomes.mjs");
  assert.deepEqual([...SUCCESSFUL_PUBLICATION_OUTCOMES], ["published", "published-by-earlier-attempt"]);
  const uploader = fs.readFileSync(new URL("../../scripts/ci/upload.mjs", import.meta.url), "utf8");
  assert.match(uploader, /outcome: PUBLISHED_BY_EARLIER_ATTEMPT, publishedBy: "earlier-attempt"/, "the uploader writes the shared name");
  assert.equal(PUBLISHED_BY_EARLIER_ATTEMPT, "published-by-earlier-attempt");
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
  const artifact = { artifactFile: path.join(dir, "rightkit-release-0.6.9.tgz"), artifactSha256: "a".repeat(64), contentSha256: "c".repeat(64) };
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
  assert.equal(receipt.contentSha256, "c".repeat(64), "an npm unit is bound hosted by its content digest");
  assert.equal(receipt.contentExcluded, undefined, "and no exclusion is recorded for a unit that has none");
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
  // An npm unit owes its content digest; the swap-helper package also owes that its archive held the helper and its signing receipt.
  const write = (a, u = unit) => writeQualification({ report, artifact: { ...artifact, ...a }, unit: u, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: () => {} });
  assert.throws(() => write({ contentSha256: undefined }), /content digest/);
  assert.throws(() => write({ contentSha256: "xyz" }), /content digest/);
  const swap = { kind: "npm", name: "@rightkit/release", version: "0.6.9" };
  const helper = ["package/bin/darwin/rightkit-swap", "package/bin/darwin/rightkit-swap.sign.json"];
  assert.throws(() => write({}, swap), /archive to hold package\/bin\/darwin\/rightkit-swap and package\/bin\/darwin\/rightkit-swap\.sign\.json/);
  assert.throws(() => write({ contentExcluded: [helper[0]] }, swap), /archive to hold/);
  assert.throws(() => write({ contentExcluded: [] }, swap), /archive to hold/);
  const swapWritten = []; writeQualification({ report, artifact: { ...artifact, contentExcluded: [...helper].reverse() }, unit: swap, commit, tree: "b".repeat(40), runtime: "r", executor: { kind: "broker" }, write: (f, text) => swapWritten.push(JSON.parse(text)) });
  assert.deepEqual(swapWritten[0].contentExcluded, helper);
  // A crate carries no content digest.
  assert.doesNotThrow(() => write({ contentSha256: undefined }, { kind: "cargo", name: "rightkit-demo", version: "1.0.0" }));
  void t;
});

test("a release run sends each compile and package broker job a nonce bound to commit, tree, unit and stage, fetches those jobs' signed receipts and embeds them in the qualification", async () => {
  const s = setup();
  const envs = {}; const carriers = [];
  const nonce = (stage) => qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage });
  const stageCommands = () => ({ compile: [{ kind: "cargo", argvs: [["fmt"]] }, { kind: "cargo", argvs: [["test"]] }], package: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["pack"]] }] });
  const requests = { "c-1": { kind: "cargo", requestedArgv: ["fmt"] }, "c-2": { kind: "cargo", requestedArgv: ["test"] }, "p-1": { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["pack"] }, "x-1": { kind: "cargo", requestedArgv: ["metadata"] } };
  const fetched = [];
  const fetchReceipt = (id) => { fetched.push(id); return { id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, request: requests[id] }, signature: { alg: "ES256", keyId: "k", value: "ab" } } }; };
  let ids = ["c-1", "c-2", "c-1"];
  s.owner.compileLocalUnit = async (_kind, _args, { env }) => { envs.compile = env.RIGHTKIT_ATTEST_STAGE_NONCE; carriers.push(...Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_(ATTEST_NONCE|SNAPSHOT_REQUEST|REQUEST_ID_FILE)$/.test(k))); return {}; };
  s.owner.localUnitCompileRequests = () => ids;
  const packaged = { ...unit, commit, artifactFile: s.artifactFile, artifactSha256: createHash("sha256").update("artifact").digest("hex"), contentSha256: "c".repeat(64) };
  s.owner.packageLocalUnit = async (_state, { env }) => { envs.package = env.RIGHTKIT_ATTEST_STAGE_NONCE; return { ...packaged, brokerRequests: ["p-1"] }; };
  s.owner.qualifyLocalUnit = async (_state, { env }) => { envs.qualify = env.RIGHTKIT_ATTEST_STAGE_NONCE; carriers.push(...Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_(ATTEST_NONCE|SNAPSHOT_REQUEST|REQUEST_ID_FILE)$/.test(k))); return { ...packaged, qualifiedOnly: true, evidencePath: "gate.json" }; };
  fs.writeFileSync(s.artifactFile, "artifact");
  let written;
  const releaser = async ({ qualification }) => { written = JSON.parse(fs.readFileSync(qualification, "utf8")); return { registry: "npm" }; };
  const run = (over = {}) => runUnitGate({ ...s.args, executor: { kind: "broker" }, verifyReceipts: () => {}, to: "release", fetchReceipt, stageCommands, releaser, ...over });
  assert.equal(await run(), 0);
  assert.equal(envs.compile, nonce("compile")); assert.equal(envs.package, nonce("package"));
  assert.deepEqual(carriers, [], "the stage env carries the stage nonce only in RIGHTKIT_ATTEST_STAGE_NONCE: no carrier variable (RUSTRIGHT_ATTEST_NONCE and the rest) is set for the stage's calls");
  assert.equal(written.contentSha256, "c".repeat(64), "the qualification carries the archive's content digest");
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
  // A run that does not reach qualify sets no nonce and fetches nothing: what the stages do is unchanged.
  Object.keys(envs).forEach((k) => delete envs[k]); fetched.length = 0;
  assert.equal(await runUnitGate({ ...s.args, executor: { kind: "broker" }, to: "package", fetchReceipt, stageCommands }), 0);
  assert.deepEqual([envs.compile, envs.package, envs.qualify], [undefined, undefined, undefined]);
  assert.deepEqual(fetched, []);
  // A direct host sets none either, whatever the target.
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

test("[M6] readBrokerReceipt starts the client the way every gate child does: on Windows the .cmd shim's .exe target, no shell", () => {
  const prov = { schema: "rustright.provenance.v1", payload: { jobId: "j" }, signature: { alg: "ES256", keyId: "k", value: "ab" } };
  const exe = "D:\\.rightkit-managed\\runtime-current\\rustright.exe";
  const shim = `@echo off\r\nrem ${AGENT_SHIM_MARKER}\r\nsetlocal\r\nset "RUSTRIGHT_MANAGED_AGENT_SHIM="\r\nset "RIGHTKIT_MANAGED_AGENT_SHIM="\r\n"${exe}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
  const launches = [];
  const run = (cmd, args, options) => {
    launches.push({ cmd, args, options });
    if (cmd === "where") return { status: 0, stdout: "D:\\.rightkit-managed\\agent-bin\\rustright.cmd\r\n" };
    return { status: 0, stdout: JSON.stringify({ receipt: { id: "j", provenance: prov } }) };
  };
  const env0 = process.env.RUSTRIGHT_MANAGED_AGENT_SHIM; process.env.RUSTRIGHT_MANAGED_AGENT_SHIM = "1";
  try {
    assert.deepEqual(readBrokerReceipt("j", { run, platform: "win32", readFile: () => shim }), { id: "j", provenance: prov });
  } finally { if (env0 === undefined) delete process.env.RUSTRIGHT_MANAGED_AGENT_SHIM; else process.env.RUSTRIGHT_MANAGED_AGENT_SHIM = env0; }
  assert.equal(launches[0].cmd, "where");
  assert.equal(launches[1].cmd, exe, "the shim's target is spawned, never rustright or rustright.cmd");
  assert.deepEqual(launches[1].args, ["status", "j"]);
  assert.equal(launches[1].options.shell, false);
  assert.equal(Object.keys(launches[1].options.env).some((k) => k.toUpperCase() === "RUSTRIGHT_MANAGED_AGENT_SHIM"), false);
  // A shim of any other shape is refused, never run through a shell.
  assert.throws(() => readBrokerReceipt("j", { run, platform: "win32", readFile: () => "@echo off\r\ncalc\r\n" }), /cannot report job j/);
});

test("[M5] releaseUnit runs rk-pipeline from the PRIMARY CHECKOUT, never from the managed snapshot, and only when HEAD is the qualified commit", async () => {
  const qualification = path.join(tempRoot("unit-qualification-m5-"), "qualification.json"); fs.writeFileSync(qualification, "{}");
  const snapshot = "/managed/snapshots/abc/snap", checkout = "/primary/rightkit";
  const calls = [];
  const dispatched = { unit: unit.name, sha: commit, registry: "npm" };
  const make = (head) => (cmd, args, options) => {
    calls.push({ cmd, args, cwd: options?.cwd });
    if (cmd === "git" && args[0] === "status") return { status: 0, stdout: "" }; // GB-07: a clean checkout (the coordinator must be exactly the qualified commit's bytes)
    if (cmd === "git") return { status: 0, stdout: `${head}\n` };
    return { status: 1, stdout: "", stderr: "stop here" };
  };
  const base = { unit, root: snapshot, checkout, commit, executor: { kind: "broker" }, env: {}, artifact: {}, qualification, platform: "darwin", out: () => {}, readJson: () => ({ dispatches: { k: dispatched } }) };
  await assert.rejects(releaseUnit({ ...base, run: make(commit) }), /canary.*failed/);
  const pipelineCalls = calls.filter((c) => c.cmd !== "git");
  assert.ok(pipelineCalls.length >= 1);
  for (const c of pipelineCalls) {
    assert.equal(c.args[0], path.join(checkout, "scripts", "ci", "rk-pipeline.mjs"), "the primary checkout's pipeline script");
    assert.equal(c.cwd, checkout, "from the primary checkout");
    assert.ok(!c.args[0].startsWith(snapshot) && !String(c.cwd).startsWith(snapshot), "never from inside the snapshot");
  }
  // HEAD elsewhere: the pre-flight and the dispatch would prove another commit; nothing starts.
  calls.length = 0;
  await assert.rejects(releaseUnit({ ...base, run: make("c".repeat(40)) }), /not at the qualified commit/);
  assert.deepEqual(calls.filter((c) => c.cmd !== "git"), []);
});

// Known red (AGENTS.md: "never publish a unit listed there"): the gate refuses a listed unit before any compile.
function writeKnownRed(root, table) {
  fs.mkdirSync(path.join(root, "ci"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "known-red.json"), typeof table === "string" ? table : JSON.stringify(table));
}

test("known red: a unit listed for this host is refused before check or compile, at every stage that compiles", async () => {
  for (const key of ["@rightkit/demo", "rust:@rightkit/demo", "packages/demo", "npm:packages/demo"]) {
    for (const to of ["compile", "package", "qualify", "release"]) {
      const s = setup();
      writeKnownRed(s.args.root, { macos: { [key]: "tests fail on macOS (rightkit#1)" }, windows: {} });
      const errs = [];
      assert.equal(await runUnitGate({ ...s.args, to, err: (l) => errs.push(l) }), 1, `${key} --to ${to}`);
      assert.deepEqual(s.calls, [], `${key} --to ${to}: nothing ran, not even the class A check`);
      assert.match(errs.join("\n"), /BLOCKED: npm:@rightkit\/demo is listed in ci\/known-red\.json for macos .*tests fail on macOS \(rightkit#1\)/);
    }
  }
});

test("known red: a crate is matched by its name and its crates/ directory", async () => {
  for (const key of ["rightkit-demo", "rust:rightkit-demo", "cargo:rightkit-demo", "crates/rightkit-demo"]) {
    const s = setup();
    writeKnownRed(s.args.root, { macos: { [key]: "red" } });
    assert.equal(await runUnitGate({ ...s.args, selector: "cargo:rightkit-demo", resolve: () => ({ kind: "cargo", name: "rightkit-demo", version: "1.0.0" }), err: () => {} }), 1, key);
    assert.deepEqual(s.calls, []);
  }
});

test("known red: the host table decides compile, package and qualify; a release is barred by either host's entry", async () => {
  const s = setup();
  writeKnownRed(s.args.root, { macos: {}, windows: { "@rightkit/demo": "windows only red" } });
  // Listed only for Windows: this Mac compiles, packages and qualifies it.
  assert.equal(await runUnitGate(s.args), 0);
  assert.deepEqual(s.calls, ["check:release", "compile", "package", "qualify", "close"]);
  // The same unit on a Windows host is refused.
  s.calls.length = 0;
  const errs = [];
  assert.equal(await runUnitGate({ ...s.args, platform: "win32", err: (l) => errs.push(l) }), 1);
  assert.deepEqual(s.calls, []);
  assert.match(errs.join("\n"), /for windows/);
  // A release is never run for a unit red on any host, whichever host runs it.
  assert.equal(await runUnitGate({ ...s.args, to: "release", err: (l) => errs.push(l) }), 1);
  assert.deepEqual(s.calls, []);
  // Linux has no known-red table: the host entry does not apply there, so only a release is barred.
  assert.equal(await runUnitGate({ ...s.args, platform: "linux" }), 0);
});

test("known red: --to check compiles nothing and still runs; a missing or empty table lists nothing; an unreadable one refuses", async () => {
  const s = setup();
  writeKnownRed(s.args.root, { macos: { "@rightkit/demo": "red" } });
  assert.equal(await runUnitGate({ ...s.args, to: "check" }), 0);
  assert.deepEqual(s.calls, ["check:compile"]);
  const empty = setup();
  writeKnownRed(empty.args.root, { macos: {}, windows: {} });
  assert.equal(await runUnitGate(empty.args), 0);
  const absent = setup();
  assert.equal(await runUnitGate(absent.args), 0, "no ci/known-red.json: nothing is listed");
  const broken = setup();
  writeKnownRed(broken.args.root, "{ not json");
  const errs = [];
  assert.equal(await runUnitGate({ ...broken.args, err: (l) => errs.push(l) }), 1);
  assert.deepEqual(broken.calls, []);
  assert.match(errs.join("\n"), /ci\/known-red\.json cannot be read/);
});

// ---- the local self-check and the sanctioned no-upload route (rightkit#1301) ---------------------------------------------------------
/** A broker-host run with the owner returning broker job ids for each stage and fetchReceipt serving them (shapes only: verification is stubbed or real per test). */
function brokerRun() {
  const s = setup();
  const stageCommands = () => ({ compile: [{ kind: "cargo", argvs: [["fmt"]] }], package: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["pack"]] }] });
  const requests = { "c-1": { kind: "cargo", requestedArgv: ["fmt"] }, "p-1": { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["pack"] } };
  const fetchReceipt = (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, request: requests[id] }, signature: { alg: "ES256", keyId: "k", value: "ab" } } });
  const envs = {};
  s.owner.compileLocalUnit = async (_k, _a, { env }) => { envs.compile = env; return {}; };
  s.owner.localUnitCompileRequests = () => ["c-1"];
  const packaged = { ...unit, commit, artifactFile: s.artifactFile, artifactSha256: createHash("sha256").update("artifact").digest("hex"), contentSha256: "c".repeat(64) };
  s.owner.packageLocalUnit = async (_s, { env }) => { envs.package = env; return { ...packaged, brokerRequests: ["p-1"] }; };
  s.owner.qualifyLocalUnit = async (_s, { env }) => { envs.qualify = env; return { ...packaged, qualifiedOnly: true, evidencePath: "gate.json" }; };
  const errs = [], outs = [], verified = [], released = [];
  const run = (over = {}) => runUnitGate({ ...s.args, executor: { kind: "broker" }, fetchReceipt, stageCommands, out: (l) => outs.push(l), err: (l) => errs.push(l), releaser: async () => { released.push(1); return { registry: "npm" }; }, selfGit: () => assert.fail("the stub verifier reads no git"),
    verifyReceipts: (text, o) => { verified.push({ q: JSON.parse(text), ...o }); }, ...over });
  return { s, run, envs, errs, outs, verified, released };
}

test("`--to qualify` on a broker host attests the stages, writes the qualification beside the artifact and verifies it locally; it dispatches nothing", async () => {
  const h = brokerRun();
  assert.equal(await h.run({ to: "qualify" }), 0, h.errs.join("\n"));
  assert.deepEqual(h.released, [], "no releaser: this is the no-upload route");
  assert.equal(h.envs.compile.RIGHTKIT_ATTEST_STAGE_NONCE, qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage: "compile" }), "attested without release");
  assert.equal(h.envs.package.RIGHTKIT_ATTEST_STAGE_NONCE, qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage: "package" }));
  assert.equal(h.envs.qualify.RIGHTKIT_ATTEST_STAGE_NONCE, undefined, "the qualify stage runs no broker job");
  for (const stage of ["compile", "package", "qualify"]) assert.ok(!Object.keys(h.envs[stage]).some((k) => /^(RUSTRIGHT|RIGHTKIT)_(ATTEST_NONCE|SNAPSHOT_REQUEST)$/.test(k)), `${stage}: no carrier variable`);
  assert.equal(h.verified.length, 1);
  assert.equal(h.verified[0].sha, commit, "verified against the dispatched commit");
  assert.equal(h.verified[0].q.contentSha256, "c".repeat(64));
  assert.deepEqual(h.verified[0].q.stages.compile.receipts.map((r) => r.id), ["c-1"]);
  assert.deepEqual(h.verified[0].q.stages.package.receipts.map((r) => r.id), ["p-1"]);
  const file = path.join(path.dirname(h.s.artifactFile), "qualification.json");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).contentSha256, "c".repeat(64), "the qualification is written beside the artifact");
  assert.match(h.outs.join("\n"), /qualification .*qualification\.json verified locally; nothing was dispatched/);
  // A target short of qualify attests nothing and writes nothing.
  const none = brokerRun();
  assert.equal(await none.run({ to: "package" }), 0);
  assert.equal(none.envs.compile.RIGHTKIT_ATTEST_STAGE_NONCE, undefined); assert.deepEqual(none.verified, []);
});

test("a qualification the hosted admission would refuse stops the run before the releaser (--to release) and fails the run (--to qualify); the default verifier is the real one", async () => {
  const bad = (text, o) => { void text; void o; throw new Error("publication admission refused: package receipt 1 (job p-1) does not verify against the enrolled pins at aaaaaaaaaaaa: signature: signature does not verify"); };
  const rel = brokerRun();
  assert.equal(await rel.run({ to: "release", verifyReceipts: bad }), 1);
  assert.deepEqual(rel.released, [], "nothing is dispatched");
  assert.match(rel.errs.join("\n"), /local check of the qualification's signed broker receipts failed; the hosted admission would refuse it, so nothing is dispatched: package receipt 1 \(job p-1\) does not verify/);
  assert.doesNotMatch(rel.errs.join("\n"), /publication admission refused:/);
  const q = brokerRun();
  assert.equal(await q.run({ to: "qualify", verifyReceipts: bad }), 1);
  assert.match(q.errs.join("\n"), /local check of the qualification's signed broker receipts failed/);
  assert.equal(q.s.reports.at(-1).stages.qualify.ok, false);
  // And with no stub: the real verifier runs against the commit's own files (git reads are injected), and an unsigned receipt does not pass it.
  const real = brokerRun();
  const gitAt = (...args) => {
    if (args[0] === "rev-parse") return `${tree}\n`;
    if (args[1] === `${commit}:ci/rk-pipeline.json`) return JSON.stringify({ attestation: { darwin: { publicKey: "04".padEnd(130, "0"), keyId: "k".repeat(64), host: "enrolled.local", brokerDigest: "d".repeat(64) } } });
    throw new Error(`unexpected git ${args.join(" ")}`);
  };
  const errs = [];
  assert.equal(await runUnitGate({ ...real.s.args, executor: { kind: "broker" }, to: "release", fetchReceipt: (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, request: id === "c-1" ? { kind: "cargo", requestedArgv: ["fmt"] } : { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["pack"] } }, signature: { alg: "ES256", keyId: "k", value: "ab" } } }),
    stageCommands: () => ({ compile: [{ kind: "cargo", argvs: [["fmt"]] }], package: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["pack"]] }] }), selfGit: gitAt, releaser: async () => assert.fail("not released"), err: (l) => errs.push(l) }), 1);
  assert.match(errs.join("\n"), /local check of the qualification's signed broker receipts failed.*which ci\/rk-pipeline\.json does not enroll/);
});

test("[GB-07] unit release refuses to start the coordinator from a primary checkout whose coordinator or release code is not exactly the qualified commit's bytes (modified tracked file, untracked file under scripts/ci or packages/release); a clean checkout at the commit proceeds", async () => {
  const { releaseUnit } = await import("./unit-gate.mjs");
  const qualification = path.join(tempRoot("unit-qualification-"), "qualification.json");
  fs.writeFileSync(qualification, "{}");
  const make = ({ tracked = "", scoped = "", head = commit } = {}) => { const calls = []; const run = (cmd, args, options) => {
    calls.push({ cmd: path.basename(cmd), args, cwd: options?.cwd });
    if (cmd === "git" && args[0] === "rev-parse") return { status: 0, stdout: `${head}\n`, stderr: "" };
    if (cmd === "git" && args[0] === "status") return { status: 0, stdout: args.includes("--untracked-files=no") ? tracked : scoped, stderr: "" };
    return { status: 1, stdout: "", stderr: "stop here" };
  }; return { calls, run }; };
  const base = (run) => ({ unit, root: "/snap/commit", checkout: "/repo", commit, executor: { kind: "broker" }, env: {}, artifact: { artifactSha256: "1".repeat(64) }, qualification, platform: "darwin", out: () => {}, scratch: tempRoot("unit-release-"), run });
  // Clean and at the commit: the coordinator starts from the primary checkout (cwd too).
  const clean = make();
  await assert.rejects(releaseUnit(base(clean.run)), /canary/);
  const started = clean.calls.find((c) => c.args[0]?.endsWith("rk-pipeline.mjs"));
  assert.equal(started.args[0], path.join("/repo", "scripts", "ci", "rk-pipeline.mjs")); assert.equal(started.cwd, "/repo");
  // A modified tracked file, or an untracked one beside the coordinator: nothing is dispatched, the coordinator never starts.
  for (const [name, over] of [["a modified tracked file", { tracked: " M scripts/ci/rk-pipeline.mjs\n" }], ["an untracked sibling the coordinator could import", { scoped: "?? scripts/ci/rk-evil.mjs\n" }]]) {
    const dirty = make(over);
    await assert.rejects(releaseUnit(base(dirty.run)), /holds modified or untracked files.*exactly the qualified commit/, name);
    assert.equal(dirty.calls.some((c) => c.args[0]?.endsWith("rk-pipeline.mjs")), false, `${name}: the coordinator never started`);
  }
  // An unreadable status is not a clean one.
  const unreadable = { calls: [], run: (cmd, args) => (cmd === "git" && args[0] === "rev-parse" ? { status: 0, stdout: `${commit}\n` } : cmd === "git" ? { status: 128, stdout: "" } : { status: 1 }) };
  await assert.rejects(releaseUnit(base(unreadable.run)), /cannot be read for modified coordinator files/);
});
