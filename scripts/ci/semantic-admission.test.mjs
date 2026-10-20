import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { canonicalRunProblems, consumerQualificationProblems, runtimeFeatureProofPolicy, semanticAdmission } from "./semantic-admission.mjs";
import { derivePublicationObligations } from "./publication-obligations.mjs";

const SHA = "a".repeat(40); const TREE = "b".repeat(40); const HEX = "c".repeat(64);
const hash = (data) => crypto.createHash("sha256").update(data).digest("hex");

test("runtime classification reuses owner & always-on stays capability HOLD", () => {
  for (const input of [
    { crate: "rightkit-custom", meta: { continuous: true } },
    { crate: "rightkit-custom", meta: { continuous: "always-on" } },
    { crate: "rightkit-custom", meta: { runtime_budget: true } },
    { crate: "rightkit-custom", hasFile: true },
    { crate: "rightkit-logs" },
  ]) {
    const result = runtimeFeatureProofPolicy(input);
    assert.equal(result.classification, "always-on");
    assert.equal(result.status, "HOLD");
    assert.deepEqual(result.failures, ["authenticated isolated runtime feature/workload proof missing"]);
  }
});
test("noncontinuous & held-resource declarations retain structural classification", () => {
  for (const [continuous, kind] of [[false, "false"], ["held-resource", "held-resource"]]) {
    // A budget file does not override either explicit declaration in owner policy.
    const result = runtimeFeatureProofPolicy({ crate: "rightkit-custom", meta: { continuous }, hasFile: true });
    assert.equal(result.classification, kind);
    assert.equal(result.status, "not-applicable");
    assert.deepEqual(result.failures, []);
  }
  assert.equal(runtimeFeatureProofPolicy({ crate: "rightkit-custom" }).classification, "undeclared");
});
test("listed owners cannot bypass HOLD with false/held-resource or unchecked proof claims", () => {
  for (const continuous of [false, "held-resource", undefined]) {
    const input = { crate: "rightkit-logs", meta: { continuous }, runtime: true, verified: true, producer: { isolated: true }, callback: () => true };
    const result = runtimeFeatureProofPolicy(input);
    assert.equal(result.listedOwner, true);
    assert.equal(result.status, "HOLD");
    assert.equal(result.required, true);
  }
});
function canonical() {
  const expected = { kind: "cargo", name: "rightkit-logs", version: "0.3.4", commit: SHA, artifactSha256: HEX };
  const actualResults = [{ command: "cargo test -p rightkit-logs --no-fail-fast", executed: 2, failed: 0 }, { command: "cargo test -p rightkit-logs --features x --no-fail-fast", executed: 3, failed: 0 }];
  const evidence = { schema: 1, ...expected, runs: [{ commit: SHA, artifactSha256: HEX, host: { os: "darwin", arch: "arm64" }, command: actualResults.map((r) => r.command).join(" && "), features: ["default", "--features x"], passed: 5, failed: 0, operator: "actor/job", timestamp: "2026-10-20T00:00:00Z" }] };
  return { expected, actualResults, evidence, host: "macos" };
}
test("canonical native content binds actual commands, counts, features & host", () => {
  assert.deepEqual(canonicalRunProblems(canonical()), []);
  for (const damage of [
    (f) => { f.evidence.commit = TREE; },
    (f) => { f.evidence.runs[0].passed = 6; },
    (f) => { f.evidence.runs[0].failed = 1; },
    (f) => { f.evidence.runs[0].command += " --invented"; },
    (f) => { f.evidence.runs[0].features = ["default"]; },
    (f) => { f.evidence.runs[0].host.os = "win32"; },
    (f) => { delete f.evidence.runs[0].host.arch; },
    (f) => { delete f.evidence.runs[0].operator; },
    (f) => { delete f.evidence.runs[0].timestamp; },
    (f) => { f.evidence.runs.push(f.evidence.runs[0]); },
    (f) => { f.evidence = null; },
  ]) { const f = canonical(); damage(f); assert.ok(canonicalRunProblems(f).length); }
});

function consumer() {
  const expected = { name: "rightkit-logs", version: "0.3.4", commit: SHA, manifestSha256: HEX, archiveSha256: HEX, host: "macos", runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1" };
  const recipe = { pkId: "PK11", id: "ci/consumers/rightkit-logs/src/main.rs", sourceCommit: SHA, files: { "Cargo.toml": HEX, "src/main.rs": HEX }, sha256: HEX, manifestSha256: HEX, exampleSha256: HEX, dependency: { name: expected.name, version: "=0.3.4", features: [], defaultFeatures: false } };
  const cwd = "/runner/temp/consumer";
  const metadata = { packages: [{ id: "selected", name: expected.name, version: expected.version, source: "registry+https://github.com/rust-lang/crates.io-index" }], resolve: { nodes: [{ id: "selected", features: [] }] } };
  const commands = [["metadata", "--format-version", "1", "--locked", "--offline"], ["build", "--locked", "--offline"], ["run", "--locked", "--offline"]].map((args) => ({ command: "cargo", args, cwd, status: 0, stdout: args[0] === "metadata" ? JSON.stringify(metadata) : args[0] === "run" ? "actual API output\n" : "", stderr: "" }));
  const report = { schema: 1, kind: "registry-consumer", qualification: "R2a", ...expected, arch: "arm64", recipe, verified: true, finalSource: "sealed-directory-registry", finalManifestSha256: HEX, lockSha256: HEX, resolvedFeatures: [], toolchain: { rustc: "rustc 1.90", cargo: "cargo 1.90" }, lockEvidence: [{ name: expected.name, version: expected.version, source: "registry+https://github.com/rust-lang/crates.io-index", checksum: HEX, archiveSha256: HEX }], consumerDirectory: cwd, commands, build: { locked: true, offline: true, status: 0 }, runtime: { locked: true, offline: true, status: 0 }, runtimeOutput: "actual API output\n" };
  return { expected, recipe, report };
}
test("R2a accepts bound named recipe & actual locked/offline command evidence", () => {
  assert.deepEqual(consumerQualificationProblems(consumer()), []);
});
for (const [label, damage] of [
  ["generic import", (f) => { delete f.report.qualification; }],
  ["unverified", (f) => { f.report.verified = false; }],
  ["foreign source", (f) => { f.report.commit = TREE; }],
  ["foreign run", (f) => { f.report.runId = "78"; }],
  ["foreign attempt", (f) => { f.report.runAttempt = "2"; }],
  ["foreign archive", (f) => { f.report.archiveSha256 = "d".repeat(64); }],
  ["foreign recipe", (f) => { f.report.recipe = { ...f.recipe, exampleSha256: "d".repeat(64) }; }],
  ["missing recipe", (f) => { f.recipe = undefined; }],
  ["runtime failure", (f) => { f.report.runtime.status = 1; }],
  ["online build", (f) => { f.report.build.offline = false; }],
  ["missing command", (f) => { f.report.commands.splice(1, 1); }],
  ["failed actual command", (f) => { f.report.commands[1].status = 1; }],
  ["invented output", (f) => { f.report.runtimeOutput = "invented"; }],
  ["invented features", (f) => { f.report.resolvedFeatures = ["invented"]; }],
  ["path dependency", (f) => { f.report.lockEvidence[0].source = null; }],
  ["missing filesystem fixture", (f) => { f.expected.name = "rightkit-fs"; }],
]) test(`R2a refuses ${label}`, () => { const f = consumer(); damage(f); assert.ok(consumerQualificationProblems(f).length); });

function npmFixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-semantic-fixture-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const packDir = path.join(root, "packs"); fs.mkdirSync(packDir);
  fs.mkdirSync(path.join(root, "packages/no-test"), { recursive: true });
  const json = { name: "@rightkit/no-test", version: "1.0.0" };
  fs.writeFileSync(path.join(root, "packages/no-test/package.json"), JSON.stringify(json));
  const bytes = Buffer.from("sealed archive fixture");
  const unit = { name: json.name, version: json.version, dir: "packages/no-test", file: "no-test.tgz", size: bytes.length, sha256: hash(bytes) };
  const manifest = { schema: 1, registry: "npm", commit: SHA, runId: "77", runAttempt: "1", units: [unit] };
  const rawManifest = JSON.stringify(manifest); fs.writeFileSync(path.join(packDir, "manifest.json"), rawManifest); fs.writeFileSync(path.join(packDir, unit.file), bytes);
  const candidate = { sourceSha: SHA, manifestSha256: hash(rawManifest), registry: "npm", units: [json.name], runId: "77", runAttempt: "1" };
  const hosts = {}; const receipts = {}; const qa = {};
  for (const host of ["macos", "windows"]) {
    hosts[host] = derivePublicationObligations({ registry: "npm", units: [json.name], host, packageManifests: [{ dir: unit.dir, json }], allow: { allowNoTest: [json.name] } });
    const summary = { host, sha: SHA, selection: null, steps: "lock,fmt,test,closure", results: [], rows: [] };
    const unitEvidence = { schema: 2, host, registry: "npm", sha: SHA, ok: true, units: [{ name: json.name, id: "npm:packages/no-test", status: "allowed-no-test", executed: 0, ignored: 0, skipped: 0, problems: [], steps: [] }] };
    qa[host] = { summary, unitEvidence, summarySha256: hash(JSON.stringify(summary)), unitEvidenceSha256: hash(JSON.stringify(unitEvidence)) };
    receipts[host] = { schema: 1, kind: "verify", host, registry: "npm", commit: SHA, manifestSha256: candidate.manifestSha256, runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1", sourceGate: { summarySha256: qa[host].summarySha256, unitEvidenceSha256: qa[host].unitEvidenceSha256 }, units: [{ ...unit, status: "allowed-no-test", bytesVerified: true, publicationEvidence: null, consumer: { verified: true } }] };
  }
  return { candidate, manifest, source: { source: { sha: SHA, tree: TREE }, hosts, root, sourceHashes: {} }, receipts, qa, packDir };
}
test("explicit no-test policy stays separate & cannot manufacture canonical positive runs or npm PK", (t) => {
  const input = npmFixture(t); const original = JSON.stringify(input.receipts);
  const result = semanticAdmission(input);
  assert.equal(result.ok, false); assert.equal(result.units.length, 1);
  assert.equal(result.hostVerdicts.macos.ok, true);
  assert.equal(result.units[0].policies.macos, "allowed-no-test");
  assert.ok(result.units[0].failures.some((s) => s.includes("policy conflict")));
  assert.ok(result.units[0].failures.some((s) => s.includes("missing canonical")));
  assert.ok(result.units[0].failures.some((s) => s.includes("generic import is not PK")));
  assert.equal(JSON.stringify(input.receipts), original);
  assert.equal(result.archiveHashes[input.manifest.units[0].name], input.manifest.units[0].sha256);
});
test("changed actual archive & raw QA hash fail independently", (t) => {
  const input = npmFixture(t);
  fs.writeFileSync(path.join(input.packDir, input.manifest.units[0].file), "changed bytes");
  input.qa.windows.summarySha256 = "d".repeat(64);
  const result = semanticAdmission(input);
  assert.ok(result.failures.some((s) => s.includes("sourceGate differs")));
  assert.ok(result.units[0].failures.some((s) => s.includes("actual archive")));
});
