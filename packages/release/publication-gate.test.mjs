// The test count rule of the publication gate (GATE-CONTRACT GC-A08; Adrian, 2026-11-03, end-to-end tests only: "My decision was no unit
// tests. Only e2e."): a crate or npm unit that ran zero tests with no failure, skip or ignore passes; a failure still fails, a skip or an
// ignore beside zero tests is never a pass, and the commit, host and archive binding of a run is unchanged.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";
import { GATE_SCHEMA, provenGateSteps, ranTestsOrNone, validatePublicationEvidence } from "./publication-gate.mjs";

const commit = "8e6f0c433d00192f6947c0cb204a487507cbcce3";
const name = "rightkit-time";
const version = "0.3.2";
const artifactBytes = Buffer.from("the packed crate");
const artifactSha256 = crypto.createHash("sha256").update(artifactBytes).digest("hex");
const run = (over = {}) => ({
  host: { os: "linux", arch: "x64" }, commit, features: ["default"], artifactSha256,
  command: `cargo test -p ${name} --no-fail-fast`, operator: "right-release --local", timestamp: "2026-11-11T10:00:00.000Z",
  passed: 0, failed: 0, skipped: 0, ignored: 0, ...over,
});
const evidence = (runOver = {}, over = {}) => ({ schema: GATE_SCHEMA, kind: "cargo", name, version, commit, artifactSha256, hostPolicy: "linux-direct", runs: [run(runOver)], ...over });
const direct = (doc, expected = {}) => validatePublicationEvidence({ expected: { kind: "cargo", name, version, commit, directLinux: true, ...expected }, evidence: doc, artifactBytes });

test("a crate that ran zero tests with nothing failed, skipped or ignored satisfies the gate", () => {
  assert.deepEqual(direct(evidence()).missing, []);
  // A run that carries no skip or ignore counts at all (publish.yml's native verify evidence) reports none.
  assert.deepEqual(direct(evidence({ skipped: undefined, ignored: undefined })).missing, []);
  assert.deepEqual(direct(evidence({ passed: 7 })).missing, []);
});

test("an npm unit with no test qualifies as a crate with no test does", () => {
  const doc = evidence({}, { kind: "npm", name: "@rightkit/tokens" });
  assert.deepEqual(direct(doc, { kind: "npm", name: "@rightkit/tokens" }).missing, []);
});

test("zero tests beside a failure, a skip or an ignore is refused", () => {
  assert.match(direct(evidence({ failed: 1 })).missing.join("; "), /Linux run receipt 0 invalid: failed \(must be 0\)/);
  assert.match(direct(evidence({ skipped: 1 })).missing.join("; "), /Linux run receipt 0 invalid: passed \(must be > 0, or 0 with nothing skipped or ignored\)/);
  assert.match(direct(evidence({ ignored: 2 })).missing.join("; "), /passed \(must be > 0, or 0 with nothing skipped or ignored\)/);
  // A positive count with a failure is as refused as before.
  assert.match(direct(evidence({ passed: 3, failed: 1 })).missing.join("; "), /failed \(must be 0\)/);
});

test("a count that is no count is refused", () => {
  for (const passed of [undefined, null, -1, 0.5, "0", NaN]) assert.match(direct(evidence({ passed })).missing.join("; "), /passed \(must be > 0/, String(passed));
});

test("a zero-test run is still bound to the commit, the host and the archive hash", () => {
  assert.match(direct(evidence({ commit: "0".repeat(40) })).missing.join("; "), /commit \(must equal /);
  assert.match(direct(evidence({ artifactSha256: "1".repeat(64) })).missing.join("; "), /artifactSha256 \(must equal the evidence hash\)/);
  assert.match(direct(evidence({ host: { os: "linux" } })).missing.join("; "), /host\.arch/);
  assert.match(direct(evidence({ host: { os: "darwin", arch: "arm64" } })).missing.join("; "), /no Linux host run receipt/);
  assert.match(direct(evidence(), { commit: "2".repeat(40) }).missing.join("; "), /evidence commit .* != expected commit/);
  assert.match(direct(evidence(), { version: "9.9.9" }).missing.join("; "), /evidence version/);
  assert.match(validatePublicationEvidence({ expected: { kind: "cargo", name, version, commit, directLinux: true }, evidence: evidence(), artifactBytes: Buffer.from("other bytes") }).missing.join("; "), /artifact sha256 does not match/);
  // Without the direct-Linux request the default hosts are still owed: a Linux run proves neither macOS nor Windows.
  assert.match(validatePublicationEvidence({ expected: { kind: "cargo", name, version, commit }, evidence: evidence(), artifactBytes }).missing.join("; "), /no macOS host run receipt.*no Windows host run receipt/);
});

test("a Swift package still needs a positive count", () => {
  const doc = evidence({ host: { os: "darwin", arch: "arm64" } }, { kind: "swift", name: "RightKitSwift", hostPolicy: undefined });
  const result = validatePublicationEvidence({ expected: { kind: "swift", name: "RightKitSwift", version, commit }, evidence: doc, artifactBytes });
  assert.match(result.missing.join("; "), /macOS run receipt 0 invalid: passed/);
});

test("ranTestsOrNone: a positive count, or zero with nothing skipped or ignored", () => {
  assert.equal(ranTestsOrNone(1), true);
  assert.equal(ranTestsOrNone(0), true);
  assert.equal(ranTestsOrNone(0, { skipped: 0, ignored: 0 }), true);
  assert.equal(ranTestsOrNone(0, { skipped: 1, ignored: 0 }), false);
  assert.equal(ranTestsOrNone(0, { skipped: 0, ignored: 1 }), false);
  assert.equal(ranTestsOrNone(4, { skipped: 1, ignored: 1 }), true); // a skip beside real tests is the allowsets' business, as before
  assert.equal(ranTestsOrNone(-1), false);
  assert.equal(ranTestsOrNone(undefined), false);
});

test("a passing test step that ran no test proves the tests; a failure, a skip or an ignore beside zero does not", () => {
  const step = (over = {}) => ({ command: `cargo test -p ${name} --no-fail-fast`, ok: true, executed: 0, failed: 0, ignored: 0, skipped: 0, binaries: 1, ...over });
  const proven = (stepOver) => provenGateSteps({ ...evidence({ host: { os: "darwin", arch: "arm64" }, steps: [step(stepOver)] }), hostPolicy: "darwin-only" }).tests;
  assert.equal(proven({}), true);
  assert.equal(proven({ executed: 5 }), true);
  assert.equal(proven({ failed: 1 }), false);
  assert.equal(proven({ executed: 5, failed: 1 }), false);
  assert.equal(proven({ skipped: 1 }), false);
  assert.equal(proven({ ignored: 1 }), false);
  assert.equal(proven({ ok: false }), false);
  assert.equal(proven({ executed: undefined }), false);
});
